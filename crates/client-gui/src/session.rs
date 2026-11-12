//! La session : tout ce qui n'existe que le temps d'une connexion à un
//! serveur — le lien lui-même, qui l'on est chez lui, et ce qu'il nous a
//! donné à voir.
//!
//! Sortie de `KiApp` d'un bloc : ce qui appartient à un serveur se lit à son
//! préfixe (`self.session.members`), et ne se confond plus avec ce qui
//! appartient à l'appli (réglages, fenêtres, carnet des serveurs).

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use ki_protocol::{ChannelId, ChannelInfo, ChatRecord, Member, Perms, RoleInfo, UserId};

use crate::net::NetHandle;
use crate::NonLu;

/// La session en cours : la connexion, qui l'on est chez ce serveur, ce qu'il
/// nous a donné à voir — salons, rôles, membres, messages.
pub(crate) struct Session {
    /// Le lien avec le serveur ; `None` hors connexion.
    pub(crate) conn: Option<NetHandle>,
    /// Une connexion est en cours d'établissement.
    pub(crate) connecting: bool,
    /// Début de la tentative en cours, pour la borner (voir
    /// `DELAI_CONNEXION`). `None` = pas de tentative en vol.
    pub(crate) connect_started: Option<Instant>,
    /// Le serveur nous a accueillis (`Welcome` reçu) : l'écran principal
    /// remplace celui de connexion.
    pub(crate) welcomed: bool,
    /// Notre numéro de compte chez ce serveur.
    pub(crate) my_id: Option<UserId>,
    /// Permissions effectives, annoncées par le serveur. L'interface s'en
    /// sert pour ne montrer que ce qui aboutira.
    pub(crate) my_perms: Perms,
    /// Mon rang : on n'agit que sur strictement plus bas que soi.
    pub(crate) my_rank: u16,
    /// Le jeton de la session, donné à l'accueil : il signe la voix et
    /// les requêtes HTTP de cette connexion.
    pub(crate) voice_token: u64,
    /// Les salons que l'on peut voir, textuels et vocaux.
    pub(crate) channels: Vec<ChannelInfo>,
    /// Tous les rôles du serveur, pour les couleurs et les badges.
    pub(crate) roles: Vec<RoleInfo>,
    /// Tout le monde sur le serveur, pas seulement le salon courant.
    pub(crate) members: Vec<Member>,
    /// Les messages chargés du salon ouvert, du plus ancien au plus récent.
    pub(crate) messages: Vec<ChatRecord>,
    /// Salon textuel ouvert.
    pub(crate) current: Option<ChannelId>,
    /// Salon vocal où l'on se trouve. `None` = connecté au serveur sans
    /// être en vocal, ce qui est l'état à l'arrivée.
    pub(crate) voice_channel: Option<ChannelId>,
    /// Ce qui n'a pas été lu, par salon. Le salon courant y figure aussi
    /// quand on ne le regarde pas — fenêtre à l'arrière-plan, ou fil
    /// remonté dans le passé.
    pub(crate) non_lus: HashMap<ChannelId, NonLu>,
    /// Le serveur tient les repères de lecture : vrai dès le premier
    /// `NonLus` reçu. Avant ça — ou face à un serveur antérieur — on
    /// n'envoie jamais de `Lu`, qu'il prendrait pour un message invalide.
    pub(crate) serveur_gere_lus: bool,
    /// Reste-t-il du passé à remonter dans ce salon ?
    pub(crate) history_more: bool,
    /// Une page est déjà demandée : on n'en réclame pas une seconde à
    /// chaque image tant que celle-ci n'est pas arrivée.
    pub(crate) history_pending: bool,
    /// Ce qu'on veut du vocal : un salon, ou en sortir.
    ///
    /// L'affichage suit cette intention le temps que le serveur la traite,
    /// puis c'est la liste des membres qui fait foi. Sans cette fenêtre, une
    /// liste produite avant notre clic nous remettait dans le salon qu'on
    /// venait de quitter ; sans sa **péremption**, un refus jamais notifié
    /// figeait l'affichage pour le reste de la session.
    pub(crate) voice_intent: Option<Option<ChannelId>>,
    /// Fin de la fenêtre pendant laquelle l'intention prime.
    pub(crate) voice_intent_until: Instant,
    /// Occupants du salon vocal à l'image précédente : c'est leur écart qui
    /// révèle une arrivée ou un départ. Les messages `UserJoined`/`UserLeft`
    /// portent sur le serveur entier, pas sur le salon vocal.
    pub(crate) prev_voice_peers: HashSet<UserId>,
    /// Salon vocal de l'image précédente : quand il change, les occupants du
    /// nouveau salon sont adoptés EN SILENCE (entrer dans un salon peuplé ne
    /// déclenche pas six sons d'arrivée) — sans fenêtre temporelle, qui
    /// mangeait les sons dès qu'on changeait de salon rapidement.
    pub(crate) prev_voice_channel: Option<ChannelId>,
}

impl Session {
    /// Hors connexion : personne, nulle part, rien de chargé.
    pub(crate) fn nouvelle() -> Self {
        Self {
            conn: None,
            connecting: false,
            connect_started: None,
            welcomed: false,
            my_id: None,
            my_perms: 0,
            my_rank: 0,
            voice_token: 0,
            channels: Vec::new(),
            roles: Vec::new(),
            members: Vec::new(),
            messages: Vec::new(),
            current: None,
            voice_channel: None,
            non_lus: HashMap::new(),
            serveur_gere_lus: false,
            history_more: false,
            history_pending: false,
            voice_intent: None,
            voice_intent_until: Instant::now(),
            prev_voice_peers: HashSet::new(),
            prev_voice_channel: None,
        }
    }
}
