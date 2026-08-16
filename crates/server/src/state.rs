//! État partagé du serveur : utilisateurs connectés, salons, registre voix.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use ki_protocol::{ChannelId, ChannelInfo, Member, ServerMsg, UserId};
use rand::Rng;
use tokio::sync::mpsc::UnboundedSender;

use crate::accounts::Accounts;
use crate::history::History;
use crate::meta::ServerMeta;
use crate::throttle::Throttle;

pub struct ConnectedUser {
    pub username: String,
    /// Salon textuel ouvert : ce que la personne lit en ce moment.
    pub channel: Option<ChannelId>,
    /// Salon vocal occupé. Distinct du précédent : on lit un salon sans
    /// forcément y parler, et l'inverse.
    pub voice: Option<ChannelId>,
    pub speaking: bool,
    /// Rôles du compte, et ce qui s'en déduit. **Recalculés** à chaque
    /// changement de rôle (`AppState::refresh_member`) : les garder à jour
    /// ici évite de reprendre le magasin de rôles sur le chemin chaud.
    pub roles: Vec<ki_protocol::RoleId>,
    pub perms: ki_protocol::Perms,
    pub rank: u16,
    pub color: Option<u32>,
    /// Conservé pour les clients antérieurs aux rôles : dérivé de
    /// `perms`, jamais source de vérité.
    pub admin: bool,
    /// Jeton de session (authentifie les uploads HTTP de fichiers).
    pub voice_token: u64,
    /// Canal vers la tâche d'écriture du flux de contrôle de ce client.
    pub tx: UnboundedSender<ServerMsg>,
    /// Connexion QUIC du client (datagrammes voix).
    pub conn: quinn::Connection,
    /// Anti-spam du chat.
    pub chat_budget: TokenBucket,
}

/// Seau à jetons : autorise une rafale courte, puis un débit soutenu.
///
/// Le chat n'avait aucune limite. Un client modifié pouvait donc émettre des
/// milliers de messages par seconde, et saturer d'un coup la mémoire
/// glissante, le fichier d'historique et la bande passante de tout le monde.
/// La rafale reste généreuse : coller cinq lignes d'affilée est un usage
/// normal, en écrire cinquante ne l'est pas.
pub struct TokenBucket {
    tokens: f32,
    last: Instant,
}

impl TokenBucket {
    /// Jetons regagnés par seconde.
    const RATE: f32 = 5.0;
    /// Réserve maximale, donc taille de la rafale tolérée.
    const BURST: f32 = 10.0;

    /// Consomme un jeton. `false` = trop rapide, le message est refusé.
    pub fn take(&mut self) -> bool {
        let now = Instant::now();
        let gained = now.duration_since(self.last).as_secs_f32() * Self::RATE;
        self.tokens = (self.tokens + gained).min(Self::BURST);
        self.last = now;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

impl Default for TokenBucket {
    fn default() -> Self {
        Self { tokens: Self::BURST, last: Instant::now() }
    }
}

/// Mot de passe éphémère d'un salon vocal.
pub struct VoiceLock {
    /// Comparé en clair, à temps constant. Le hacher n'apporterait rien —
    /// le serveur détient le secret de toute façon — et un Argon2 par
    /// tentative d'entrée serait un déni de service bien plus concret que
    /// la fuite d'un mot de passe qui vit vingt minutes.
    pub password: String,
    pub expires_at: std::time::Instant,
}

/// Comparaison à temps constant : une comparaison naïve s'arrête au premier
/// octet différent, ce qui laisse deviner le mot de passe caractère par
/// caractère en mesurant le temps de réponse.
pub fn secret_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Table de routage voix, consultée sur le chemin chaud (chaque datagramme).
/// Reconstruite uniquement aux événements rares (connexion/join/leave),
/// lue sous un simple verrou partagé : aucune contention avec le contrôle.
#[derive(Default)]
pub struct RouteTable {
    /// user_id -> salon actuel.
    pub channel_of: HashMap<UserId, ChannelId>,
    /// salon -> destinataires (user_id, connexion QUIC) — précalculé.
    /// `quinn::Connection` est un handle clonable ; `send_datagram` ne
    /// bloque jamais.
    pub peers: HashMap<ChannelId, Vec<(UserId, quinn::Connection)>>,
}

pub struct AppState {
    /// Code d'invitation (création de comptes).
    pub token: String,
    /// Clé de chiffrement voix, régénérée à chaque démarrage du serveur.
    /// Le serveur ne s'en sert jamais pour déchiffrer : il la distribue
    /// seulement aux clients authentifiés via Welcome.
    pub voice_key: [u8; 32],
    pub channels: crate::channels::Channels,
    pub roles: crate::roles::Roles,
    pub accounts: Accounts,
    /// Verrous des salons vocaux. **En mémoire seulement** : c'est le sens
    /// d'« éphémère », et ça supprime la question de leur persistance — un
    /// redémarrage du serveur les lève tous.
    pub voice_locks: Mutex<HashMap<ChannelId, VoiceLock>>,
    /// Identité publique du serveur (nom, logo), réglée par les admins.
    pub meta: ServerMeta,
    /// Limiteur des tentatives d'authentification.
    pub throttle: Throttle,
    /// Bornes du partage de fichiers (plafond global, durée de vie).
    pub files_quota: crate::files::Quota,
    pub data_dir: String,
    pub users: Mutex<HashMap<UserId, ConnectedUser>>,
    pub voice_routes: std::sync::RwLock<RouteTable>,
    pub history: History,
    /// Journal des actions d'administration.
    pub audit: crate::audit::Audit,
}

impl AppState {
    pub fn new(
        token: String,
        data_dir: &str,
        files_quota: crate::files::Quota,
    ) -> anyhow::Result<Self> {
        // Les salons ne sont plus câblés : ils vivent dans channels.json,
        // qui reprend les six d'origine au premier démarrage.
        let channels = crate::channels::Channels::open(data_dir)?;
        let roles = crate::roles::Roles::open(data_dir)?;
        // Seuls les salons textuels ont un historique à tenir.
        let text_channels: Vec<ChannelInfo> = channels
            .list()
            .into_iter()
            .filter(|c| c.kind == ki_protocol::ChannelKind::Text)
            .collect();
        let history = History::open(data_dir, &text_channels)?;
        let accounts = Accounts::open(data_dir)?;
        let meta = ServerMeta::open(data_dir)?;
        let audit = crate::audit::Audit::open(data_dir)?;
        Ok(Self {
            token,
            voice_key: rand::rng().random(),
            channels,
            roles,
            voice_locks: Mutex::new(HashMap::new()),
            accounts,
            meta,
            throttle: Throttle::default(),
            files_quota,
            data_dir: data_dir.to_string(),
            users: Mutex::new(HashMap::new()),
            voice_routes: std::sync::RwLock::new(RouteTable::default()),
            history,
            audit,
        })
    }

    /// Reconstruit la table de routage voix depuis l'état des connexions.
    /// À appeler après tout événement qui la change : connexion, join/leave,
    /// déconnexion. Coût négligeable (rare + ~30 users).
    pub fn rebuild_voice_routes(&self) {
        let users = self.users.lock().unwrap();
        let mut routes = self.voice_routes.write().unwrap();
        // Le routage de la voix suit le salon **vocal**, pas le salon lu.
        routes.channel_of = users
            .iter()
            .filter_map(|(id, u)| u.voice.map(|c| (*id, c)))
            .collect();
        routes.peers.clear();
        for (id, u) in users.iter() {
            let Some(channel) = u.voice else { continue };
            routes.peers.entry(channel).or_default().push((*id, u.conn.clone()));
        }
    }

    /// Vrai si le salon existe **et** est de la nature attendue : on ne
    /// parle pas dans un salon textuel, on n'écrit pas dans un vocal.
    pub fn channel_is(&self, id: ChannelId, kind: ki_protocol::ChannelKind) -> bool {
        self.channels.is(id, kind)
    }

    /// Rôles d'un connecté, `@everyone` compris.
    ///
    /// Le rôle par défaut n'est jamais stocké sur un compte — le retirer à
    /// quelqu'un n'aurait pas de sens — mais il doit compter dans les
    /// restrictions de salon, sans quoi un salon réservé à `@everyone`
    /// deviendrait invisible pour tout le monde.
    fn effective_roles(&self, user_id: UserId) -> Vec<ki_protocol::RoleId> {
        let users = self.users.lock().unwrap();
        let mut roles = users.get(&user_id).map(|u| u.roles.clone()).unwrap_or_default();
        roles.push(ki_protocol::ROLE_EVERYONE);
        roles
    }

    fn manages_channels(&self, user_id: UserId) -> bool {
        let users = self.users.lock().unwrap();
        users
            .get(&user_id)
            .is_some_and(|u| ki_protocol::perm::has(u.perms, ki_protocol::perm::MANAGE_CHANNELS))
    }

    /// Ce salon est-il visible par cette personne ?
    pub fn can_view(&self, user_id: UserId, channel: ChannelId) -> bool {
        let Some(info) = self.channels.get(channel) else { return false };
        crate::channels::can_view(
            &info,
            &self.effective_roles(user_id),
            self.manages_channels(user_id),
        )
    }

    /// Les salons **tels que cette personne les voit**, verrous compris.
    pub fn visible_channels(&self, user_id: UserId) -> Vec<ChannelInfo> {
        let mut list = self
            .channels
            .visible_to(&self.effective_roles(user_id), self.manages_channels(user_id));
        let locks = self.voice_locks.lock().unwrap();
        let now = std::time::Instant::now();
        for channel in &mut list {
            channel.locked = locks.get(&channel.id).is_some_and(|l| l.expires_at > now);
        }
        list
    }

    /// Pousse à **chacun** sa propre liste de salons.
    ///
    /// Surtout pas un `broadcast_all` d'une liste unique : ce serait révéler
    /// à tout le monde le nom des salons privés. C'est le piège numéro un de
    /// cette fonctionnalité.
    pub fn push_channels(&self) {
        let ids: Vec<UserId> = { self.users.lock().unwrap().keys().copied().collect() };
        for id in ids {
            let channels = self.visible_channels(id);
            let users = self.users.lock().unwrap();
            if let Some(u) = users.get(&id) {
                let _ = u.tx.send(ServerMsg::ChannelsUpdated { channels });
            }
        }
    }

    /// Recalcule les permissions d'un connecté depuis ses rôles.
    pub fn refresh_member(&self, user_id: UserId) {
        let roles = {
            let users = self.users.lock().unwrap();
            let Some(u) = users.get(&user_id) else { return };
            self.accounts.roles_of(&u.username)
        };
        let perms = self.roles.perms_of(&roles);
        let rank = self.roles.rank_of(&roles);
        let color = self.roles.color_of(&roles);
        let mut users = self.users.lock().unwrap();
        if let Some(u) = users.get_mut(&user_id) {
            u.admin = ki_protocol::perm::has(perms, ki_protocol::perm::ADMINISTRATOR);
            u.roles = roles;
            u.perms = perms;
            u.rank = rank;
            u.color = color;
        }
    }

    /// Remet tout le monde dans un état cohérent avec ce qu'il voit
    /// réellement.
    ///
    /// À appeler après **toute** mutation de salon ou de rôle. Sans elle,
    /// quelqu'un resterait assis dans un salon qu'il ne voit plus — donc à
    /// écrire et à parler à des gens qui ne le voient pas.
    pub fn reconcile_memberships(&self) {
        let ids: Vec<UserId> = { self.users.lock().unwrap().keys().copied().collect() };
        let mut voice_changed = false;
        for id in ids {
            let (channel, voice) = {
                let users = self.users.lock().unwrap();
                let Some(u) = users.get(&id) else { continue };
                (u.channel, u.voice)
            };
            if let Some(c) = channel {
                if !self.can_view(id, c) {
                    let mut users = self.users.lock().unwrap();
                    if let Some(u) = users.get_mut(&id) {
                        u.channel = None;
                        let _ = u.tx.send(ServerMsg::Info {
                            message: "le salon que tu lisais ne t'est plus accessible".into(),
                        });
                    }
                }
            }
            if let Some(c) = voice {
                if !self.can_view(id, c) {
                    let mut users = self.users.lock().unwrap();
                    if let Some(u) = users.get_mut(&id) {
                        u.voice = None;
                        u.speaking = false;
                    }
                    voice_changed = true;
                }
            }
        }
        if voice_changed {
            self.rebuild_voice_routes();
        }
        self.push_channels();
        self.broadcast_all(&ServerMsg::Members { members: self.roster() });
    }

    /// Vérifie le verrou d'un salon vocal. `Ok(())` = entrée autorisée.
    /// `Err(wrong)` = refus, `wrong` disant si un mot de passe erroné a été
    /// fourni ou s'il en manquait un.
    pub fn check_voice_lock(
        &self,
        user_id: UserId,
        channel: ChannelId,
        password: Option<&str>,
    ) -> Result<(), bool> {
        // Qui gère les salons entre toujours : c'est lui qui pose le verrou,
        // et il doit pouvoir vérifier ce qu'il vient de faire.
        if self.manages_channels(user_id) {
            return Ok(());
        }
        let mut locks = self.voice_locks.lock().unwrap();
        let Some(lock) = locks.get(&channel) else { return Ok(()) };
        if lock.expires_at <= std::time::Instant::now() {
            // Expiré : on le retire au passage plutôt que de faire tourner
            // une tâche de fond pour ça.
            locks.remove(&channel);
            return Ok(());
        }
        match password {
            Some(given) if secret_eq(given, &lock.password) => Ok(()),
            Some(_) => Err(true),
            None => Err(false),
        }
    }

    /// Envoie un message à tous les membres d'un salon, sauf `except`.
    pub fn broadcast(&self, channel: ChannelId, except: Option<UserId>, msg: &ServerMsg) {
        // La visibilité est revérifiée ici, et pas seulement au `Join` :
        // `u.channel` a été posé par un `Join` autrefois valide, et un
        // changement de rôle entre-temps a pu le périmer.
        let targets: Vec<UserId> = {
            let users = self.users.lock().unwrap();
            users
                .iter()
                .filter(|(id, u)| u.channel == Some(channel) && Some(**id) != except)
                .map(|(id, _)| *id)
                .collect()
        };
        for id in targets {
            if !self.can_view(id, channel) {
                continue;
            }
            let users = self.users.lock().unwrap();
            if let Some(u) = users.get(&id) {
                let _ = u.tx.send(msg.clone());
            }
        }
    }

    /// Envoie un message à tous les clients connectés, salon ou pas.
    pub fn broadcast_all(&self, msg: &ServerMsg) {
        let users = self.users.lock().unwrap();
        for u in users.values() {
            let _ = u.tx.send(msg.clone());
        }
    }

    /// Comme `broadcast_all`, mais sans renvoyer à l'émetteur : celui-ci
    /// connaît déjà son propre état, sans aller-retour réseau.
    pub fn broadcast_all_except(&self, except: UserId, msg: &ServerMsg) {
        let users = self.users.lock().unwrap();
        for (id, u) in users.iter() {
            if *id != except {
                let _ = u.tx.send(msg.clone());
            }
        }
    }

    /// Tout le monde sur le serveur, avec son salon vocal éventuel et
    /// l'empreinte de sa photo (la vignette se demande à part).
    ///
    /// La liste est celle du **serveur**, plus celle d'un salon : on veut
    /// voir qui est là, y compris ceux qui ne sont dans aucun vocal.
    pub fn roster(&self) -> Vec<Member> {
        let avatars = self.accounts.avatar_hashes();
        let users = self.users.lock().unwrap();
        let mut members: Vec<Member> = users
            .iter()
            .map(|(id, u)| Member {
                user_id: *id,
                username: u.username.clone(),
                speaking: u.speaking,
                admin: u.admin,
                avatar: avatars.get(id).cloned(),
                voice: u.voice,
                roles: u.roles.clone(),
                color: u.color,
                rank: u.rank,
            })
            .collect();
        members.sort_by(|a, b| a.username.to_lowercase().cmp(&b.username.to_lowercase()));
        members
    }

    /// Identité liée à un jeton de session (authentification des uploads HTTP).
    pub fn user_by_voice_token(&self, token: u64) -> Option<(UserId, String)> {
        let users = self.users.lock().unwrap();
        users
            .iter()
            .find(|(_, u)| u.voice_token == token)
            .map(|(id, u)| (*id, u.username.clone()))
    }

    /// Nettoyage complet à la déconnexion d'un client (ferme aussi sa
    /// connexion QUIC — utile pour kick/ban).
    pub fn disconnect(&self, user_id: UserId) {
        self.remove_session(user_id, None);
    }

    /// Comme `disconnect`, mais seulement si la session enregistrée est bien
    /// **celle-ci**, identifiée par son jeton.
    ///
    /// Sans cette garde, la tâche d'une connexion morte évincerait en
    /// s'achevant la session qui vient de la remplacer : on se reconnecte,
    /// et l'ancienne connexion nous éjecte une seconde plus tard.
    pub fn disconnect_session(&self, user_id: UserId, voice_token: u64) {
        self.remove_session(user_id, Some(voice_token));
    }

    fn remove_session(&self, user_id: UserId, only_if_token: Option<u64>) {
        {
            let mut users = self.users.lock().unwrap();
            match users.get(&user_id) {
                Some(u) if only_if_token.is_none_or(|t| u.voice_token == t) => {
                    let u = users.remove(&user_id).expect("présent à l'instant");
                    u.conn.close(0u32.into(), b"bye");
                }
                // Personne, ou une session plus récente : rien à faire.
                _ => return,
            }
        }
        self.rebuild_voice_routes();
        // La liste du serveur change pour tout le monde.
        self.broadcast_all(&ServerMsg::UserLeft { user_id });
        self.broadcast_all(&ServerMsg::Members { members: self.roster() });
    }
}

pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La comparaison du mot de passe de salon doit rendre le bon verdict —
    /// et surtout ne pas s'arrêter au premier octet différent, sans quoi le
    /// temps de réponse laisserait deviner le secret caractère par caractère.
    #[test]
    fn voice_secrets_compare_without_leaking_their_length() {
        assert!(secret_eq("hunter2", "hunter2"));
        assert!(!secret_eq("hunter2", "hunter3"));
        // Longueurs différentes : refusé, jamais un préfixe accepté.
        assert!(!secret_eq("hunter", "hunter2"));
        assert!(!secret_eq("hunter2", "hunter"));
        // Le vide ne passe pas pour un joker.
        assert!(!secret_eq("", "hunter2"));
        assert!(secret_eq("", ""));
        // Différence sur le dernier octet : le pliage doit la voir.
        assert!(!secret_eq("aaaaaaab", "aaaaaaaa"));
        // ...et sur le premier, tout autant.
        assert!(!secret_eq("baaaaaaa", "aaaaaaaa"));
    }
}
