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
    pub channels: Vec<ChannelInfo>,
    pub accounts: Accounts,
    /// Identité publique du serveur (nom, logo), réglée par les admins.
    pub meta: ServerMeta,
    /// Limiteur des tentatives d'authentification.
    pub throttle: Throttle,
    pub data_dir: String,
    pub users: Mutex<HashMap<UserId, ConnectedUser>>,
    pub voice_routes: std::sync::RwLock<RouteTable>,
    pub history: History,
    /// Journal des actions d'administration.
    pub audit: crate::audit::Audit,
}

impl AppState {
    pub fn new(token: String, data_dir: &str) -> anyhow::Result<Self> {
        // Salons par défaut, orientés gaming. Configurables plus tard.
        // Les identifiants 1 à 3 sont conservés : l'historique déjà écrit
        // sur disque porte ces numéros.
        use ki_protocol::ChannelKind::{Text, Voice};
        let channels = vec![
            ChannelInfo { id: 1, name: "général".into(), kind: Text },
            ChannelInfo { id: 2, name: "gaming".into(), kind: Text },
            ChannelInfo { id: 3, name: "afk".into(), kind: Text },
            ChannelInfo { id: 101, name: "Général".into(), kind: Voice },
            ChannelInfo { id: 102, name: "Gaming".into(), kind: Voice },
            ChannelInfo { id: 103, name: "AFK".into(), kind: Voice },
        ];
        // Seuls les salons textuels ont un historique à tenir.
        let text_channels: Vec<ChannelInfo> =
            channels.iter().filter(|c| c.kind == Text).cloned().collect();
        let history = History::open(data_dir, &text_channels)?;
        let accounts = Accounts::open(data_dir)?;
        let meta = ServerMeta::open(data_dir)?;
        let audit = crate::audit::Audit::open(data_dir)?;
        Ok(Self {
            token,
            voice_key: rand::rng().random(),
            channels,
            accounts,
            meta,
            throttle: Throttle::default(),
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
        self.channels.iter().any(|c| c.id == id && c.kind == kind)
    }

    /// Envoie un message à tous les membres d'un salon, sauf `except`.
    pub fn broadcast(&self, channel: ChannelId, except: Option<UserId>, msg: &ServerMsg) {
        let users = self.users.lock().unwrap();
        for (id, u) in users.iter() {
            if u.channel == Some(channel) && Some(*id) != except {
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
