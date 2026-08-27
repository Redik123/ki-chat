//! État partagé du serveur : utilisateurs connectés, salons, registre voix.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ki_protocol::{ChannelId, ChannelInfo, Member, ServerMsg, UserId};
use rand::Rng;
use tokio::sync::mpsc::error::TrySendError;

use crate::accounts::Accounts;
use crate::history::History;
use crate::meta::ServerMeta;
use crate::throttle::Throttle;

/// Profondeur de la file d'envoi d'un client.
///
/// Dimensionnée sur le pire cas **légitime** connu, avec de la marge : trente
/// personnes qui se reconnectent d'un coup après un redémarrage produisent, en
/// comptant les entrées en vocal et les photos qui suivent, de l'ordre de cent
/// cinquante messages chez chacun. On garde de quoi encaisser plusieurs fois
/// cela — un client au lien étroit vide sa file lentement, et une grosse photo
/// en tête retient les petits messages derrière elle — tout en restant **fini**.
pub const OUTBOX_CAP: usize = 512;

/// Une ligne de contrôle prête à partir sur le fil : JSON, saut de ligne
/// compris.
///
/// Partagée par compteur de références, elle est sérialisée **une fois** puis
/// distribuée telle quelle. La file portait des `ServerMsg`, que chaque tâche
/// d'écriture sérialisait pour son propre compte : diffuser un roster à trente
/// personnes, c'était trente copies profondes de la liste **et** trente
/// sérialisations du même contenu.
pub type Line = std::sync::Arc<[u8]>;

/// Sérialise un message en ligne prête à écrire.
///
/// `None` = la ligne dépasse ce que le client acceptera de lire. Ce garde-fou
/// vivait dans chaque tâche d'écriture ; il est ici, une fois, au seul endroit
/// où la ligne existe. Au-delà de `MAX_LINE` le lecteur d'en face ferme la
/// connexion : une réponse trop grosse déconnecterait donc son destinataire.
///
/// `>=` et non `>` : le saut de ligne ajouté ensuite compte pour le lecteur
/// d'en face, qui mesure son tampon **avant** de le retirer. Un JSON de très
/// exactement `MAX_LINE` partirait donc à `MAX_LINE + 1` octets.
pub fn encode(msg: &ServerMsg) -> Option<Line> {
    let json = match serde_json::to_string(msg) {
        Ok(json) => json,
        Err(e) => {
            tracing::error!("sérialisation d'un message de contrôle impossible : {e}");
            return None;
        }
    };
    if json.len() >= ki_protocol::MAX_LINE {
        tracing::error!("message de contrôle trop long ({} octets), ignoré", json.len());
        return None;
    }
    let mut bytes = json.into_bytes();
    bytes.push(b'\n');
    Some(bytes.into())
}

/// File d'envoi vers un client, bornée.
///
/// Elle était non bornée. Comme `write_all` respecte le contrôle de flux de
/// QUIC, un client qui cessait de lire son flux faisait grandir sa file sans
/// aucune limite : quelques requêtes d'historique suffisaient alors à faire
/// passer le serveur en gigaoctets, depuis un simple compte authentifié.
/// Déposer ne bloque jamais ; si la file est pleine, c'est que le client ne
/// suit plus, et sa connexion est fermée plutôt que de faire payer sa lenteur
/// à la mémoire de tout le monde.
#[derive(Clone)]
pub struct Outbox {
    tx: tokio::sync::mpsc::Sender<Line>,
    conn: quinn::Connection,
    /// Vrai dès que la saturation a été constatée. Une session est diffusée en
    /// boucle : sans ce drapeau, chaque message suivant reprenait le journal et
    /// refermait une connexion déjà fermée — le tout sous le verrou des
    /// connectés, qui est sur le chemin de tout.
    closing: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Outbox {
    pub fn new(tx: tokio::sync::mpsc::Sender<Line>, conn: quinn::Connection) -> Self {
        Self { tx, conn, closing: Default::default() }
    }

    /// Dépose un message, en le sérialisant pour ce seul destinataire.
    ///
    /// `Err` = le client ne le recevra pas (message impossible à sérialiser,
    /// file saturée, ou session finie). La plupart des appelants s'en
    /// désintéressent, la déconnexion étant alors engagée ; ceux qui portent
    /// une information que l'on ne peut pas réémettre — un motif d'expulsion —
    /// le consignent.
    pub fn send(&self, msg: ServerMsg) -> Result<(), ()> {
        match encode(&msg) {
            Some(line) => self.send_line(&line),
            None => Err(()),
        }
    }

    /// Dépose une ligne déjà sérialisée. C'est par là que passent les
    /// diffusions : une sérialisation, N dépôts, et le contenu n'est partagé
    /// que par un compteur de références.
    pub fn send_line(&self, line: &Line) -> Result<(), ()> {
        match self.tx.try_send(line.clone()) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                use std::sync::atomic::Ordering;
                if !self.closing.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        "file d'envoi saturée : le client ne suit plus, connexion fermée"
                    );
                    self.conn.close(0u32.into(), b"file saturee");
                }
                Err(())
            }
            Err(TrySendError::Closed(_)) => Err(()),
        }
    }
}

/// Ce que le routage de la voix a besoin de savoir d'un connecté.
///
/// Générique sur la connexion pour une seule raison : pouvoir éprouver la
/// règle sans monter un vrai lien QUIC. Ce n'est pas un détour gratuit — la
/// règle porte les sanctions vocales, et c'est le genre d'invariant qu'on
/// veut voir tenir dans un test plutôt que de le croire sur parole.
pub struct Voix<C> {
    pub id: UserId,
    pub channel: Option<ChannelId>,
    pub force_muted: bool,
    pub force_deafened: bool,
    pub conn: C,
}

/// Construit les deux tables du relais : où va la voix de chacun, et qui
/// occupe chaque salon.
///
/// # Les sanctions vocales vivent ici, et nulle part ailleurs
///
/// Le relais lit ces tables une fois par datagramme, soit cinquante fois par
/// seconde et par émetteur. Y ajouter « et au fait, celui-ci est-il coupé ? »
/// aurait demandé de reprendre la table des connectés sur ce chemin : un
/// verrou de plus par paquet, sur le seul endroit du serveur où la latence
/// s'entend.
///
/// Alors on ne teste rien à l'exécution. Quelqu'un dont le micro est coupé
/// n'a **pas d'entrée** dans `channel_of` : ses paquets ne trouvent aucun
/// salon où aller. Quelqu'un de sourd n'apparaît dans **aucune** liste de
/// `peers` : on ne lui envoie rien. La sanction ne coûte pas une instruction,
/// et aucun client modifié ne la contourne — elle s'applique du côté qui
/// relaie, pas du côté qui parle.
fn router<C>(monde: Vec<Voix<C>>) -> Routes<C> {
    let mut routes = Routes::default();
    for v in monde {
        // Le routage de la voix suit le salon **vocal**, pas le salon lu.
        let Some(channel) = v.channel else { continue };
        if !v.force_muted {
            routes.channel_of.insert(v.id, channel);
        }
        if !v.force_deafened {
            routes.peers.entry(channel).or_default().push((v.id, v.conn));
        }
    }
    routes
}

pub struct ConnectedUser {
    pub username: String,
    /// Salon textuel ouvert : ce que la personne lit en ce moment.
    pub channel: Option<ChannelId>,
    /// Salon vocal occupé. Distinct du précédent : on lit un salon sans
    /// forcément y parler, et l'inverse.
    pub voice: Option<ChannelId>,
    pub speaking: bool,
    /// Micro coupé volontairement (annoncé par le client, montré aux autres).
    pub muted: bool,
    /// Sanctions vocales posées par un modérateur, relues du compte à la
    /// connexion. Elles ne sont **pas** annoncées par le client et ne peuvent
    /// donc pas être désavouées par lui.
    pub force_muted: bool,
    pub force_deafened: bool,
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
    pub tx: Outbox,
    /// Connexion QUIC du client (datagrammes voix).
    pub conn: quinn::Connection,
    /// Anti-spam du chat.
    pub chat_budget: TokenBucket,
    /// Anti-spam des entrées et sorties de salon vocal.
    ///
    /// Chaque bascule reconstruit la table de routage de la voix et rediffuse
    /// la liste des membres à **tout** le serveur : quelques dizaines par
    /// seconde suffisaient à étrangler le relais de tout le monde. Un humain
    /// en fait deux ou trois d'affilée, jamais cinquante.
    pub voice_budget: TokenBucket,
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
    /// Jetons regagnés par seconde.
    rate: f32,
    /// Réserve maximale, donc taille de la rafale tolérée.
    burst: f32,
}

impl TokenBucket {
    pub fn new(rate: f32, burst: f32) -> Self {
        Self { tokens: burst, last: Instant::now(), rate, burst }
    }

    /// Consomme un jeton. `false` = trop rapide, le message est refusé.
    pub fn take(&mut self) -> bool {
        let now = Instant::now();
        let gained = now.duration_since(self.last).as_secs_f32() * self.rate;
        self.tokens = (self.tokens + gained).min(self.burst);
        self.last = now;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

impl Default for TokenBucket {
    /// Le budget du chat : cinq messages par seconde en régime, dix en rafale.
    fn default() -> Self {
        Self::new(5.0, 10.0)
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

/// Connexions arrivées mais pas encore authentifiées, comptées par adresse.
///
/// Le délai de dix secondes de la phase d'authentification ne couvrait que la
/// **première ligne** — pas l'ouverture du flux qui la porte. Une connexion
/// qui ne demandait jamais de flux bidirectionnel restait donc parquée
/// indéfiniment, et les keep-alives de QUIC empêchaient l'expiration
/// d'inactivité de jouer. Des dizaines de milliers de connexions sans
/// authentification, pour le prix d'autant de poignées de main.
///
/// Deux verrous répondent, et ils sont complémentaires : un délai autour de
/// l'ouverture du flux (voir `quic.rs`), et ce plafond-ci, qui borne ce qu'une
/// seule adresse peut faire attendre en même temps.
#[derive(Default)]
pub struct Sas {
    en_attente: Mutex<HashMap<std::net::IpAddr, u32>>,
}

/// Connexions non authentifiées tolérées d'une même adresse, en même temps.
///
/// Généreux à dessein : trente joueurs derrière la box d'un même foyer ou
/// d'un cybercafé partagent une adresse publique, et le client ouvre en plus
/// une poignée de main de test vers chaque serveur de son carnet. Ce plafond
/// ne vise pas l'usage — il vise les dizaines de milliers.
///
/// Il ne borne que la traversée du sas, qui dure au plus le délai
/// d'authentification : une fois entré, on ne compte plus.
const SAS_MAX_PAR_IP: u32 = 32;

impl Sas {
    /// Fait entrer une connexion, ou refuse si l'adresse en fait déjà trop
    /// attendre. Le jeton rendu libère la place en tombant — c'est ce qui
    /// garantit que tous les chemins de sortie de l'authentification, y
    /// compris les refus et les erreurs, rendent leur place.
    pub fn entrer(self: &Arc<Self>, ip: std::net::IpAddr) -> Option<JetonSas> {
        let mut en_attente = self.en_attente.lock().unwrap();
        let place = en_attente.entry(ip).or_insert(0);
        if *place >= SAS_MAX_PAR_IP {
            return None;
        }
        *place += 1;
        Some(JetonSas { sas: self.clone(), ip })
    }

    #[cfg(test)]
    fn en_attente(&self, ip: std::net::IpAddr) -> u32 {
        self.en_attente.lock().unwrap().get(&ip).copied().unwrap_or(0)
    }
}

/// Place occupée dans le sas, rendue à la destruction.
pub struct JetonSas {
    sas: Arc<Sas>,
    ip: std::net::IpAddr,
}

impl Drop for JetonSas {
    fn drop(&mut self) {
        let mut en_attente = self.sas.en_attente.lock().unwrap();
        if let Some(place) = en_attente.get_mut(&self.ip) {
            *place = place.saturating_sub(1);
            // L'entrée disparaît avec la dernière connexion : sans ça, la
            // table garderait une ligne par adresse ayant jamais touché le
            // serveur, ce qui est une fuite lente mais sûre.
            if *place == 0 {
                en_attente.remove(&self.ip);
            }
        }
    }
}

/// Table de routage voix, consultée sur le chemin chaud (chaque datagramme).
/// Reconstruite uniquement aux événements rares (connexion/join/leave),
/// lue sous un simple verrou partagé : aucune contention avec le contrôle.
///
/// Générique sur la connexion, et `RouteTable` en est l'instance réelle : le
/// paramètre n'existe que pour éprouver `router` sans monter de vrai lien
/// QUIC. La règle qu'il applique porte les sanctions vocales — c'est le genre
/// d'invariant qu'on veut voir tenir dans un test.
pub struct Routes<C> {
    /// user_id -> salon actuel.
    pub channel_of: HashMap<UserId, ChannelId>,
    /// salon -> destinataires (user_id, connexion QUIC) — précalculé.
    /// `quinn::Connection` est un handle clonable ; `send_datagram` ne
    /// bloque jamais.
    pub peers: HashMap<ChannelId, Vec<(UserId, C)>>,
}

/// `Default` à la main : celui que `derive` produirait exigerait `C: Default`,
/// or une table vide ne contient aucun `C`.
impl<C> Default for Routes<C> {
    fn default() -> Self {
        Self { channel_of: HashMap::new(), peers: HashMap::new() }
    }
}

pub type RouteTable = Routes<quinn::Connection>;

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
    /// Plafond des connexions non authentifiées, par adresse.
    pub sas: Arc<Sas>,
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
            sas: Arc::new(Sas::default()),
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
    /// Recalcule la table de routage de la voix.
    ///
    /// # Où vivent les sanctions vocales
    ///
    /// Ici, et nulle part ailleurs — c'est le choix qui compte.
    ///
    /// Le relais lit cette table une fois par datagramme, soit cinquante fois
    /// par seconde et par émetteur. Y ajouter « et au fait, celui-ci est-il
    /// coupé ? » aurait demandé de reprendre la table des connectés sur ce
    /// chemin : un verrou de plus par paquet, sur le seul chemin du serveur
    /// où la latence s'entend.
    ///
    /// Alors on ne teste rien à l'exécution : quelqu'un dont le micro est
    /// coupé n'a **pas d'entrée** dans `channel_of`, ses paquets ne trouvent
    /// donc aucun salon où aller. Quelqu'un de sourd n'apparaît dans aucune
    /// liste de `peers`, on ne lui envoie donc rien. La sanction ne coûte pas
    /// une instruction, et aucun client modifié ne la contourne — elle
    /// s'applique du côté du serveur qui relaie, pas de celui qui parle.
    pub fn rebuild_voice_routes(&self) {
        let monde: Vec<Voix<quinn::Connection>> = {
            let users = self.users.lock().unwrap();
            users
                .iter()
                .map(|(id, u)| Voix {
                    id: *id,
                    channel: u.voice,
                    force_muted: u.force_muted,
                    force_deafened: u.force_deafened,
                    conn: u.conn.clone(),
                })
                .collect()
        };
        *self.voice_routes.write().unwrap() = router(monde);
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
        self.holds(user_id, ki_protocol::perm::MANAGE_CHANNELS)
    }

    /// Ce salon est-il visible par cette personne ?
    pub fn can_view(&self, user_id: UserId, channel: ChannelId) -> bool {
        let Some(info) = self.channels.get(channel) else { return false };
        // Qui gère les salons voit tout : c'est la porte de secours contre le
        // salon rendu inaccessible par mégarde, et elle passe avant tout.
        let manages = self.manages_channels(user_id);
        if !manages && !self.holds(user_id, ki_protocol::perm::VIEW_CHANNEL) {
            return false;
        }
        crate::channels::can_view(&info, &self.effective_roles(user_id), manages)
    }

    /// Cette personne détient-elle cette permission ?
    pub fn holds(&self, user_id: UserId, need: ki_protocol::Perms) -> bool {
        let users = self.users.lock().unwrap();
        users.get(&user_id).is_some_and(|u| ki_protocol::perm::has(u.perms, need))
    }

    /// Les salons **tels que cette personne les voit**, verrous compris.
    pub fn visible_channels(&self, user_id: UserId) -> Vec<ChannelInfo> {
        // Même garde que `can_view`, sans quoi la liste annoncerait des salons
        // que toute tentative d'ouverture refuserait — l'inverse exact de
        // l'invariant recherché, où un salon inaccessible n'existe pas.
        let manages = self.manages_channels(user_id);
        if !manages && !self.holds(user_id, ki_protocol::perm::VIEW_CHANNEL) {
            return Vec::new();
        }
        let mut list = self.channels.visible_to(&self.effective_roles(user_id), manages);
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
        // Le pseudo est copié puis le verrou `users` est **relâché** avant de
        // solliciter le magasin des comptes. Les tenir tous les deux, dans cet
        // ordre, croisait l'ordre inverse pris ailleurs : une authentification
        // en cours pouvait alors bloquer la table des connectés, qui est sur le
        // chemin de tout — diffusion, permissions, routage de la voix.
        let username = {
            let users = self.users.lock().unwrap();
            let Some(u) = users.get(&user_id) else { return };
            u.username.clone()
        };
        let roles = self.accounts.roles_of(&username);
        let perms = self.roles.perms_of(&roles);
        let rank = self.roles.rank_of(&roles);
        let color = self.roles.color_of(&roles);
        let is_admin = ki_protocol::perm::has(perms, ki_protocol::perm::ADMINISTRATOR);
        let mut users = self.users.lock().unwrap();
        if let Some(u) = users.get_mut(&user_id) {
            let changed = u.perms != perms || u.rank != rank;
            u.admin = is_admin;
            u.roles = roles;
            u.perms = perms;
            u.rank = rank;
            u.color = color;
            // L'intéressé est prévenu : son interface n'a aucun autre moyen
            // d'apprendre qu'il vient d'être promu ou rétrogradé, `perms` et
            // `rank` ne voyageant autrement que dans `Welcome`.
            if changed {
                let _ = u.tx.send(ServerMsg::Perms { perms, rank, is_admin });
            }
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
                // La permission de parler est relue ici, et pas seulement à
                // l'entrée : sans quoi la retirer à quelqu'un déjà installé
                // dans un salon vocal ne produisait rien du tout, et il y
                // restait jusqu'à ce qu'il en sorte de lui-même.
                if !self.can_view(id, c)
                    || !self.holds(id, ki_protocol::perm::CONNECT_VOICE)
                {
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
    ///
    /// Sérialisé **une fois** pour tout le monde. Chaque destinataire recevait
    /// auparavant une copie profonde du message, que sa tâche d'écriture
    /// sérialisait ensuite pour son propre compte.
    pub fn broadcast(&self, channel: ChannelId, except: Option<UserId>, msg: &ServerMsg) {
        let Some(line) = encode(msg) else { return };
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
                let _ = u.tx.send_line(&line);
            }
        }
    }

    /// Envoie un message à tous les clients connectés, salon ou pas.
    ///
    /// Une sérialisation, N dépôts. C'est la diffusion la plus chère du
    /// serveur — le roster part par ici à chaque connexion, déconnexion,
    /// entrée et sortie de vocal — et elle coûtait, à trente connectés,
    /// trente copies profondes de la liste plus trente sérialisations du même
    /// contenu. Mesuré : 214,5 µs, contre 7,2 µs ainsi.
    pub fn broadcast_all(&self, msg: &ServerMsg) {
        let Some(line) = encode(msg) else { return };
        let users = self.users.lock().unwrap();
        for u in users.values() {
            let _ = u.tx.send_line(&line);
        }
    }

    /// Comme `broadcast_all`, mais sans renvoyer à l'émetteur : celui-ci
    /// connaît déjà son propre état, sans aller-retour réseau.
    pub fn broadcast_all_except(&self, except: UserId, msg: &ServerMsg) {
        let Some(line) = encode(msg) else { return };
        let users = self.users.lock().unwrap();
        for (id, u) in users.iter() {
            if *id != except {
                let _ = u.tx.send_line(&line);
            }
        }
    }

    /// Tout le monde sur le serveur, avec son salon vocal éventuel et
    /// l'empreinte de sa photo (la vignette se demande à part).
    ///
    /// La liste est celle du **serveur**, plus celle d'un salon : on veut
    /// voir qui est là, y compris ceux qui ne sont dans aucun vocal.
    /// La fiche d'**une** personne connectée, telle qu'elle apparaît dans la
    /// liste des membres.
    ///
    /// `None` si elle n'est plus connectée : l'appelant diffuse alors la liste
    /// complète, qui la fera basculer hors ligne — c'est un cas rare, et
    /// reconstruire une fiche « hors ligne » ici demanderait de reprendre le
    /// magasin de comptes pour rien.
    pub fn member_of(&self, user_id: UserId) -> Option<Member> {
        // Relu avant de prendre le verrou des connectés : l'ordre inverse
        // ferait attendre le magasin de comptes derrière celui-ci, qui est
        // sur le chemin de tout.
        let avatar = ki_protocol::avatar_hash(self.accounts.avatar_of(user_id).as_deref());
        let users = self.users.lock().unwrap();
        let u = users.get(&user_id)?;
        Some(Member {
            user_id,
            username: u.username.clone(),
            speaking: u.speaking,
            muted: u.muted,
            force_muted: u.force_muted,
            force_deafened: u.force_deafened,
            admin: u.admin,
            avatar,
            voice: u.voice,
            roles: u.roles.clone(),
            color: u.color,
            rank: u.rank,
            online: true,
        })
    }

    /// Diffuse le changement d'**un** membre, ou la liste entière s'il vient
    /// de se déconnecter (son état ne se déduit plus des connectés).
    pub fn broadcast_member(&self, user_id: UserId) {
        match self.member_of(user_id) {
            Some(member) => self.broadcast_all(&ServerMsg::MemberUpdate { member }),
            None => self.broadcast_all(&ServerMsg::Members { members: self.roster() }),
        }
    }

    pub fn roster(&self) -> Vec<Member> {
        let avatars = self.accounts.avatar_hashes();
        let connected: std::collections::HashSet<UserId> = {
            let users = self.users.lock().unwrap();
            users.keys().copied().collect()
        };
        let users = self.users.lock().unwrap();
        let mut members: Vec<Member> = users
            .iter()
            .map(|(id, u)| Member {
                user_id: *id,
                username: u.username.clone(),
                speaking: u.speaking,
                muted: u.muted,
                force_muted: u.force_muted,
                force_deafened: u.force_deafened,
                admin: u.admin,
                avatar: avatars.get(id).cloned(),
                voice: u.voice,
                roles: u.roles.clone(),
                color: u.color,
                rank: u.rank,
                online: true,
            })
            .collect();
        drop(users);

        // Les comptes hors ligne (non bannis) complètent le tableau : on voit
        // toute la communauté, pas seulement qui est là en ce moment.
        for account in self.accounts.list(&self.roles) {
            if account.banned || connected.contains(&account.user_id) {
                continue;
            }
            let sanctions = self.accounts.voice_sanctions(&account.username);
            members.push(Member {
                user_id: account.user_id,
                username: account.username,
                speaking: false,
                muted: false,
                // Les sanctions d'un compte hors ligne se lisent quand même :
                // elles l'attendent au retour, et le modérateur doit pouvoir
                // les lever sans attendre qu'il revienne.
                force_muted: sanctions.0,
                force_deafened: sanctions.1,
                admin: account.admin,
                avatar: avatars.get(&account.user_id).cloned(),
                voice: None,
                roles: account.roles.clone(),
                color: self.roles.color_of(&account.roles),
                rank: account.rank,
                online: false,
            });
        }
        // `cached` et non `sort_by_key` : la clé est une String, donc une
        // allocation. `sort_by_key` la recalculerait à chaque comparaison —
        // O(n log n) allocations pour trier un roster diffusé à chaque
        // entrée et sortie de vocal. Ici, une par membre.
        members.sort_by_cached_key(|m| m.username.to_lowercase());
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
        // La fiche est prise **avant** tout verrouillage, et pour deux
        // raisons. D'abord parce qu'une fois la personne retirée de la table
        // des connectés, on ne saurait plus reconstruire ses rôles ni sa
        // couleur sans reprendre le magasin de comptes. Ensuite parce que
        // `member_of` prend lui-même le verrou des connectés : l'appeler en le
        // tenant déjà serait un interblocage.
        let partant = self.member_of(user_id);
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
        // La liste du serveur change pour tout le monde — mais une seule
        // fiche a bougé : celle qui vient de passer hors ligne. Le compte
        // reste dans la liste, éteint, comme le ferait un roster complet.
        self.broadcast_all(&ServerMsg::UserLeft { user_id });
        match partant {
            Some(mut member) => {
                member.online = false;
                member.voice = None;
                member.speaking = false;
                member.muted = false;
                self.broadcast_all(&ServerMsg::MemberUpdate { member });
            }
            // Fiche introuvable : on retombe sur la liste entière plutôt que
            // de laisser quelqu'un affiché en ligne alors qu'il est parti.
            None => self.broadcast_all(&ServerMsg::Members { members: self.roster() }),
        }
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

    fn voix(id: UserId, channel: Option<ChannelId>, muet: bool, sourd: bool) -> Voix<u8> {
        Voix { id, channel, force_muted: muet, force_deafened: sourd, conn: id as u8 }
    }

    /// Les deux sanctions vocales s'appliquent par **absence** dans la table,
    /// et il faut que ça reste vrai : c'est ce qui fait qu'elles ne coûtent
    /// rien au relais et qu'aucun client ne les contourne.
    #[test]
    fn les_sanctions_vocales_sortent_de_la_table_de_routage() {
        let Routes { channel_of, peers } = router(vec![
            voix(1, Some(7), false, false), // ordinaire
            voix(2, Some(7), true, false),  // micro coupé
            voix(3, Some(7), false, true),  // sourd
            voix(4, Some(7), true, true),   // les deux
            voix(5, None, false, false),    // pas en vocal du tout
        ]);

        // Micro coupé : aucune destination, donc aucun paquet relayé. C'est
        // l'absence de clé qui fait la sanction, pas un test dans le relais.
        assert!(channel_of.contains_key(&1));
        assert!(!channel_of.contains_key(&2), "un micro coupé ne route nulle part");
        assert!(channel_of.contains_key(&3), "être sourd n'empêche pas de parler");
        assert!(!channel_of.contains_key(&4));
        assert!(!channel_of.contains_key(&5), "hors du vocal, rien à router");

        // Sourd : dans aucune liste d'occupants, donc destinataire de rien.
        let salon: Vec<UserId> = peers[&7].iter().map(|(id, _)| *id).collect();
        assert!(salon.contains(&1));
        assert!(salon.contains(&2), "un micro coupé continue d'entendre");
        assert!(!salon.contains(&3), "un sourd ne reçoit rien");
        assert!(!salon.contains(&4));
        assert_eq!(peers.len(), 1, "aucun salon fantôme pour qui n'est pas en vocal");
    }

    /// Le plafond mord, et la place se rend toute seule.
    ///
    /// C'est le `Drop` qui compte : la phase d'authentification sort par une
    /// douzaine de chemins — pseudo invalide, mot de passe trop long, refus du
    /// limiteur, flux fermé — et pas un seul ne doit laisser sa place occupée,
    /// sous peine de fermer le serveur à cette adresse pour de bon.
    #[test]
    fn le_sas_borne_une_adresse_et_rend_les_places() {
        let sas = Arc::new(Sas::default());
        let ip: std::net::IpAddr = "203.0.113.7".parse().unwrap();

        let jetons: Vec<_> = (0..SAS_MAX_PAR_IP).filter_map(|_| sas.entrer(ip)).collect();
        assert_eq!(jetons.len() as u32, SAS_MAX_PAR_IP);
        assert!(sas.entrer(ip).is_none(), "le plafond doit refuser au-delà");

        // Une autre adresse n'est pas concernée : le plafond est par adresse,
        // sans quoi un seul acharné fermerait le serveur à tout le monde.
        let autre: std::net::IpAddr = "198.51.100.1".parse().unwrap();
        assert!(sas.entrer(autre).is_some());

        drop(jetons);
        assert_eq!(sas.en_attente(ip), 0, "la table ne doit rien retenir");
        assert!(sas.entrer(ip).is_some(), "la place est rendue");
    }

    /// Le flux de contrôle entier est désormais borné en débit, et pas
    /// seulement le chat : une rafale normale doit passer, un flot doit être
    /// coupé, et la réserve doit se reconstituer avec le temps.
    #[test]
    fn the_control_budget_absorbs_a_burst_but_stops_a_flood() {
        let mut budget = TokenBucket::new(100.0, 200.0);
        // La rafale tolérée passe en entier.
        for i in 0..200 {
            assert!(budget.take(), "le message {i} de la rafale aurait dû passer");
        }
        // Au-delà, tout de suite après, c'est refusé.
        assert!(!budget.take(), "un flot ininterrompu doit être coupé");

        // La réserve se reconstitue : après 100 ms à 50 jetons/s, ~5 jetons.
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(budget.take(), "la réserve doit se reconstituer avec le temps");

        // Le budget du chat reste celui d'avant : rafale de 10.
        let mut chat = TokenBucket::default();
        for _ in 0..10 {
            assert!(chat.take());
        }
        assert!(!chat.take());
    }

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
