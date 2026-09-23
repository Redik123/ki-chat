//! Protocole partagé ki-chat : messages de contrôle (JSON, une ligne par
//! message sur le flux QUIC fiable) et format des paquets voix (datagrammes
//! QUIC, binaire).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub type UserId = u64;
pub type ChannelId = u32;
pub type RoleId = u32;

/// Ensemble de permissions.
///
/// Un `u64` nu plutôt qu'un type dédié : aucune dépendance nouvelle, et
/// surtout un client d'une version antérieure ignore simplement les bits
/// qu'il ne connaît pas, au lieu d'échouer à désérialiser.
pub type Perms = u64;

/// Les permissions, une par bit.
fn default_true() -> bool {
    true
}

/// Pour ne pas écrire un drapeau à faux dans chaque ligne de journal.
fn is_false(b: &bool) -> bool {
    !*b
}

pub mod perm {
    pub const VIEW_CHANNEL: u64 = 1 << 0;
    pub const SEND_MESSAGE: u64 = 1 << 1;
    pub const CONNECT_VOICE: u64 = 1 << 2;
    pub const UPLOAD_FILE: u64 = 1 << 3;
    pub const CREATE_INVITE: u64 = 1 << 4;
    pub const MANAGE_INVITES: u64 = 1 << 5;
    pub const KICK: u64 = 1 << 6;
    pub const BAN: u64 = 1 << 7;
    pub const RESET_PASSWORD: u64 = 1 << 8;
    pub const MANAGE_CHANNELS: u64 = 1 << 9;
    pub const MANAGE_ROLES: u64 = 1 << 10;
    pub const MANAGE_SERVER: u64 = 1 << 11;
    pub const VIEW_AUDIT_LOG: u64 = 1 << 12;
    /// Couper le micro de quelqu'un, ou le rendre sourd, **côté serveur**.
    ///
    /// À ne pas confondre avec le micro qu'on coupe soi-même : celui-ci est
    /// une sanction, elle survit à la reconnexion et le client ne peut pas la
    /// contourner — c'est le relais qui la fait respecter.
    pub const MUTE_MEMBERS: u64 = 1 << 13;
    /// Déplacer quelqu'un d'un salon vocal à un autre, ou l'en sortir.
    pub const MOVE_MEMBERS: u64 = 1 << 14;
    /// Supprimer les messages **des autres**. Les siens, chacun peut.
    pub const DELETE_MESSAGES: u64 = 1 << 15;
    /// Piloter le bot musique : lecture, file d'attente, volume global.
    pub const CONTROL_MUSIC: u64 = 1 << 16;
    /// Tout permis. Placé au bit de poids fort pour que les permissions
    /// futures remplissent le bas sans jamais entrer en collision.
    pub const ADMINISTRATOR: u64 = 1 << 63;

    /// Ce que reçoit tout membre, même sans rôle attribué.
    pub const DEFAULT: u64 =
        VIEW_CHANNEL | SEND_MESSAGE | CONNECT_VOICE | UPLOAD_FILE;

    /// Ce qui ne s'accorde jamais à `@everyone`.
    ///
    /// Ces permissions n'existent que pour distinguer une autorité d'une
    /// autre : les donner à tout le monde ne promeut personne, ça met le
    /// serveur à plat. Et l'on ne pourrait pas revenir en arrière — le rôle
    /// par défaut est au rang zéro, or l'on n'édite qu'un rôle strictement
    /// sous son propre rang. La règle vit ici pour que le serveur la fasse
    /// respecter et que l'interface cesse de proposer ce qui sera refusé.
    pub const NOT_FOR_EVERYONE: u64 = ADMINISTRATOR
        | CONTROL_MUSIC
        | MANAGE_ROLES
        | MANAGE_CHANNELS
        | MANAGE_SERVER
        | MANAGE_INVITES
        | BAN
        | KICK
        | RESET_PASSWORD
        | MUTE_MEMBERS
        | MOVE_MEMBERS
        | DELETE_MESSAGES;

    /// Liste ordonnée pour l'interface : (bit, intitulé, explication).
    pub const ALL: &[(u64, &str, &str)] = &[
        (VIEW_CHANNEL, "Voir les salons", "lire la liste et l'historique"),
        (SEND_MESSAGE, "Écrire", "envoyer des messages"),
        (CONNECT_VOICE, "Rejoindre le vocal", "entrer dans un salon vocal"),
        (UPLOAD_FILE, "Partager des fichiers", "téléverser images et documents"),
        (CREATE_INVITE, "Créer des invitations", "générer des codes d'accès"),
        (MANAGE_INVITES, "Gérer les invitations", "révoquer les codes des autres"),
        (KICK, "Expulser", "déconnecter quelqu'un, qui peut revenir"),
        (BAN, "Bannir", "empêcher quelqu'un de revenir"),
        (RESET_PASSWORD, "Réinitialiser les mots de passe", ""),
        (MANAGE_CHANNELS, "Gérer les salons", "créer, renommer, supprimer, verrouiller"),
        (MANAGE_ROLES, "Gérer les rôles", "créer des rôles et les attribuer"),
        (MANAGE_SERVER, "Gérer le serveur", "nom et logo"),
        (VIEW_AUDIT_LOG, "Voir le journal", "consulter les actions d'administration"),
        (MUTE_MEMBERS, "Couper le micro", "faire taire ou rendre sourd, en vocal"),
        (MOVE_MEMBERS, "Déplacer en vocal", "changer quelqu'un de salon vocal, ou l'en sortir"),
        (DELETE_MESSAGES, "Supprimer les messages", "effacer les messages des autres"),
        (CONTROL_MUSIC, "Contrôler la musique", "piloter le bot musique : lecture, file, volume"),
        (ADMINISTRATOR, "Administrateur", "toutes les permissions, présentes et futures"),
    ];

    /// Vrai si `held` accorde `need`.
    ///
    /// `ADMINISTRATOR` court-circuite la vérification de **permission**. Il
    /// ne contourne jamais celle de **rang** : sans cette distinction, un
    /// second administrateur pourrait bannir le propriétaire.
    pub fn has(held: u64, need: u64) -> bool {
        held & ADMINISTRATOR != 0 || held & need == need
    }
}

/// Rôles créés au premier démarrage, jamais supprimables.
pub const ROLE_EVERYONE: RoleId = 1;
pub const ROLE_OWNER: RoleId = 2;

/// Messages envoyés par le client au serveur (flux de contrôle, JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Premier message obligatoire. Si le compte n'existe pas, `invite`
    /// (le code d'invitation du serveur) est requis pour le créer.
    Auth {
        username: String,
        password: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        invite: Option<String>,
    },
    /// Ouvrir un salon textuel (ce qu'on lit et où l'on écrit).
    Join { channel: ChannelId },
    /// Fermer le salon textuel courant.
    Leave,
    /// Entrer dans un salon vocal. Se connecter au serveur n'y met plus
    /// personne d'office : on y entre quand on le décide.
    ///
    /// `password` ne sert qu'aux salons verrouillés. Un client d'une version
    /// antérieure n'en envoie pas et se voit refuser l'entrée d'un salon
    /// protégé, ce qui est le comportement voulu.
    JoinVoice {
        channel: ChannelId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<String>,
    },
    /// Sortir du vocal.
    LeaveVoice,
    /// Message texte dans le salon courant, en réponse à un autre ou non.
    Chat {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<MsgRef>,
    },
    /// Poser (`on`) ou retirer sa réaction sur un message du salon courant.
    React { message: MsgRef, emoji: String, on: bool },
    /// Supprimer un message du salon courant : le sien, ou celui d'un autre
    /// avec la permission `DELETE_MESSAGES`.
    DeleteMessage { message: MsgRef },
    /// Modifier un de ses messages du salon courant : le texte remplace
    /// l'ancien chez tout le monde, et le message se dit « modifié ». Les
    /// siens seulement — un modérateur supprime, il ne réécrit pas.
    EditMessage { message: MsgRef, text: String },
    /// Demander l'historique du salon courant.
    History { limit: u32 },
    /// « J'ai lu ce salon jusqu'à `ts` inclus. » Le serveur tient ce repère
    /// par membre et par salon (voir [`ServerMsg::NonLus`]) : il suit d'un
    /// ordinateur à l'autre, et c'est lui qui sait ce qui a été écrit entre
    /// deux sessions.
    ///
    /// `ts` est borné côté serveur au dernier message du salon : un client
    /// ne peut pas « lire l'avenir ». Un client neuf ne l'envoie qu'après
    /// avoir reçu un `NonLus` — preuve que le serveur en face connaît ce
    /// message. Un serveur antérieur répondrait « message invalide ».
    Lu { channel: ChannelId, ts: u64 },
    /// Chercher un texte dans l'historique.
    ///
    /// La casse est ignorée. Le serveur ne cherche que dans les salons que
    /// le demandeur a le droit de lire — sans quoi la recherche deviendrait
    /// le moyen le plus simple de lire un salon privé.
    Search {
        query: String,
        /// `None` = tous les salons visibles. Restreindre coûte moins cher,
        /// et c'est le cas le plus fréquent : on sait dans quel salon on a
        /// vu passer la chose.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<ChannelId>,
        /// Nombre de résultats voulu. Borné par le serveur.
        #[serde(default)]
        limit: u32,
    },
    /// Remonter le fil : les messages **antérieurs** à `before_ts`.
    ///
    /// Sans ça, seuls les derniers messages sont atteignables — tout ce que
    /// contient le fichier du salon au-delà reste invisible, alors même
    /// qu'il est conservé.
    HistoryBefore {
        /// Horodatage du plus ancien message déjà affiché (ms Unix).
        before_ts: u64,
        limit: u32,
        /// Salon visé. Indicatif : le serveur fait autorité avec le salon
        /// réellement ouvert, et se contente de le renvoyer dans la réponse.
        /// Absent d'un client antérieur, d'où la valeur par défaut.
        #[serde(default)]
        channel: ChannelId,
    },
    /// Le client raconte où il en est dans VALORANT — ce qu'il a lu dans son
    /// propre client Riot, et qu'il a choisi de partager. `None` : il ne
    /// joue plus, ou ne partage plus.
    GameStatus {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        jeu: Option<JeuStatut>,
    },
    /// « Poke » un membre : un son et un clignotement chez lui, rien
    /// d'autre — pour appeler quelqu'un qui traîne dans les menus sans lui
    /// écrire. Le serveur refuse ([`ServerMsg::PokeRefuse`]) s'il est hors
    /// ligne, en vocal, en partie, s'il n'en veut pas, ou si l'on insiste
    /// trop ; il ne relaie que ce qui peut être reçu.
    ///
    /// Un client neuf ne l'envoie qu'après une preuve que le serveur en
    /// face est récent (un `NonLus` reçu) : un serveur antérieur répondrait
    /// « message invalide », en bannière.
    Poke { user_id: UserId },
    /// Ce que je fais des pokes qu'on m'adresse. État de session, comme
    /// `GameStatus` : envoyé après `Welcome` et à chaque changement. Sans
    /// rien reçu, le serveur tient chacun pour joignable.
    AccepterPokes { accepter: bool },
    /// Lier son compte Riot (« Pseudo#TAG ») : le serveur le résout et
    /// tient à jour sa fiche (rang, matchs) par HenrikDev. La réponse vient
    /// à part, `LiaisonRiot`, une fois le compte trouvé.
    LierRiot { riot_id: String },
    /// Délier son compte — ou, pour un administrateur, celui de `user_id`.
    DelierRiot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_id: Option<UserId>,
    },
    /// La fiche VALORANT d'un membre lié, telle que le serveur la garde.
    FicheValorant { user_id: UserId },
    /// Toutes les fiches des membres liés, pour la page de stats.
    StatsValorant,
    /// Une commande au bot musique (permission « Contrôler la musique »).
    Musique { commande: CommandeMusique },
    /// Le client annonce son état vocal : émission en cours, et micro coupé
    /// volontairement — pour que les autres distinguent « muet » de « parti ».
    VoiceState {
        speaking: bool,
        /// Micro coupé par la personne. Absent d'un client antérieur : faux.
        #[serde(default)]
        muted: bool,
    },
    /// Démarrer un partage d'écran dans son salon vocal. Idempotent : un
    /// second appel renvoie le stream existant.
    ///
    /// La clé est générée par le streamer et confiée au serveur pour la
    /// durée du stream : il ne la remet qu'à un spectateur vérifié (même
    /// salon vocal), jamais au salon entier. Le serveur connaît déjà la clé
    /// voix — même modèle de confiance ; les enveloppes par spectateur
    /// (niveau 2, X25519) sont prévues en S4.
    StreamStart {
        meta: StreamMeta,
        /// Clé XChaCha20-Poly1305 du stream (32 octets, hex).
        stream_key: String,
        /// Ce client sait encoder une seconde qualité, basse, à la demande
        /// du serveur (`StreamBudget.basse`) — depuis 0.1.46. Un streamer
        /// d'avant n'en produit qu'une : ses spectateurs n'ont que le palier
        /// commun.
        #[serde(default)]
        couches: bool,
    },
    /// Arrêter son partage d'écran.
    StreamStop,
    /// Le streamer annonce un changement (dimensions, débit) : le serveur le
    /// rediffuse au salon en StreamMetaChanged.
    StreamMetaUpdate { meta: StreamMeta },
    /// Regarder le stream d'un membre de son salon vocal.
    Watch {
        stream_id: u32,
        /// Ce client sait recevoir la qualité basse et passer d'une qualité
        /// à l'autre en cours de route (depuis 0.1.46) : le serveur peut l'y
        /// mettre quand sa connexion ne suit pas la haute.
        #[serde(default)]
        couches: bool,
    },
    /// Cesser de regarder.
    Unwatch { stream_id: u32 },
    /// Expulse un utilisateur du serveur (admin uniquement). Il peut se
    /// reconnecter aussitôt : pour l'en empêcher, voir `AdminBan`.
    Kick {
        user_id: UserId,
        #[serde(default)]
        reason: String,
    },
    /// Coupe le micro de quelqu'un **côté serveur**, ou le lui rend.
    ///
    /// Rien à voir avec le micro qu'on coupe soi-même (`VoiceState`) : celui-ci
    /// est décidé par un modérateur, survit à la reconnexion, et le relais
    /// cesse de transmettre la voix — un client modifié n'y peut rien.
    AdminVoiceMute { username: String, muted: bool },
    /// Rend quelqu'un sourd côté serveur, ou lui rend l'écoute.
    ///
    /// **Indépendant** de la coupure de micro. Les deux se combinent parce
    /// qu'un modérateur ne veut pas toujours les deux : faire taire quelqu'un
    /// qui hurle n'oblige pas à le priver de la conversation.
    AdminVoiceDeafen { username: String, deafened: bool },
    /// Déplace quelqu'un de salon vocal. `channel: None` l'en sort.
    AdminVoiceMove {
        username: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<ChannelId>,
    },
    /// Demande l'état admin (comptes + invitations). Admin uniquement.
    AdminListUsers,
    /// Génère un code d'invitation. Admin uniquement.
    ///
    /// Variante struct depuis la version des invitations permanentes : un
    /// client plus ancien envoie `{"type":"admin_create_invite"}`, qui
    /// désérialise vers les valeurs par défaut ci-dessous — c'est-à-dire
    /// l'ancien comportement, un code à usage unique et sans expiration.
    AdminCreateInvite {
        /// `None` = illimité, autrement dit un lien permanent.
        #[serde(default = "default_invite_uses")]
        uses: Option<u32>,
        /// Étiquette libre, pour s'y retrouver (« tournoi du samedi »).
        #[serde(default)]
        label: String,
        /// Durée de validité en secondes. 0 = pas d'expiration.
        #[serde(default)]
        ttl_secs: u64,
    },
    /// Révoque un code d'invitation. Il reste au journal, mais ne sert plus.
    AdminRevokeInvite { code: String },
    /// Redéfinit le mot de passe d'un compte. Admin uniquement.
    AdminResetPassword { username: String, new_password: String },
    /// Bloque ou débloque un compte. Admin uniquement.
    ///
    /// Conservé pour les clients antérieurs à `AdminBan` : le serveur le
    /// traite comme un bannissement définitif et sans motif.
    AdminSetBanned { username: String, banned: bool },
    /// Bannit un compte, avec motif et durée. Admin uniquement.
    AdminBan {
        username: String,
        #[serde(default)]
        reason: String,
        /// Durée en secondes. 0 = définitif.
        #[serde(default)]
        duration_secs: u64,
    },
    /// Lève un bannissement. Admin uniquement.
    AdminUnban { username: String },
    /// Demande le journal d'audit. Admin uniquement.
    AdminAuditLog {
        #[serde(default)]
        limit: u32,
    },
    /// Demande la liste des rôles.
    AdminListRoles,
    AdminCreateRole {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<u32>,
        #[serde(default)]
        rank: u16,
        #[serde(default)]
        perms: Perms,
    },
    /// Remplacement complet du rôle : pas d'ambiguïté sur ce qui est mis à
    /// jour et ce qui est laissé tel quel.
    AdminEditRole { role: RoleInfo },
    AdminDeleteRole { id: RoleId },
    /// Remplace la liste des rôles d'un compte.
    AdminSetUserRoles { username: String, roles: Vec<RoleId> },
    AdminCreateChannel {
        name: String,
        #[serde(default)]
        kind: ChannelKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_roles: Option<Vec<RoleId>>,
    },
    /// Remplacement complet ; l'identifiant est porté par la valeur.
    AdminEditChannel { channel: ChannelInfo },
    AdminDeleteChannel { channel: ChannelId },
    /// Nouvel ordre d'affichage. Doit être une permutation exacte des
    /// salons existants, sinon le serveur refuse — une liste tronquée
    /// ferait disparaître des salons.
    AdminReorderChannels { order: Vec<ChannelId> },
    /// Pose ou retire le mot de passe éphémère d'un salon vocal.
    /// `password: None` retire le verrou.
    AdminSetVoicePassword {
        channel: ChannelId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<String>,
        /// Durée de vie en secondes. Bornée par le serveur.
        #[serde(default)]
        ttl_secs: u32,
    },
    /// Redéfinit l'identité du serveur (nom, logo). Admin uniquement.
    ///
    /// C'est le serveur qui possède ces données : un membre ordinaire ne
    /// peut pas les changer, et donc pas se faire passer pour un autre
    /// serveur en changeant le logo dans son coin.
    AdminSetServerInfo {
        /// `None` = ne pas toucher au nom.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default)]
        icon: IconChange,
    },
    /// Choisit le salon du fil de jeu VALORANT (`None` : fil éteint).
    /// Permission « gérer le serveur ».
    AdminSetFilValorant {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<ChannelId>,
    },
    /// Les membres peuvent-ils ajouter des morceaux au bot musique ?
    /// Permission « gérer le serveur ».
    AdminSetMusique { membres_ajoutent: bool },
    /// L'adresse web publique du serveur (`https://ts.baws.fun:8080`) : la
    /// base des liens des portes web et des QR codes de l'atelier. Le
    /// serveur ne sait pas sous quel nom on le joint — derrière Docker ou
    /// une box, rien ne le lui dit. Vide : retour à l'automatique
    /// (`KI_PUBLIC_URL`, sinon l'adresse de connexion du client). Le
    /// serveur la normalise ([`normaliser_adresse_web`]) et refuse ce qui
    /// ne se lit pas. Permission « gérer le serveur ».
    AdminSetAdresseWeb { adresse: String },
    /// Change son propre mot de passe (l'ancien est vérifié).
    ChangePassword { old_password: String, new_password: String },
    /// Définit ou retire sa propre photo de profil. Chacun ne règle que la
    /// sienne — le serveur la range dans le compte et la diffuse.
    SetAvatar {
        #[serde(default)]
        avatar: IconChange,
    },
    /// Réclame les photos de profil qu'on n'a pas encore en cache.
    ///
    /// Les vignettes ne voyagent pas dans la liste des membres : celle-ci ne
    /// porte qu'une empreinte, et le client ne demande que ce qui lui manque.
    RequestAvatars { user_ids: Vec<UserId> },
    /// Ouvrir une **porte web** : un salon textuel temporaire, joignable
    /// depuis un navigateur par `https://<serveur>/s/<slug>` sans compte ni
    /// installation — pour faire entrer des inconnus le temps d'une soirée.
    /// Permission « Créer des invitations » : ouvrir une porte, c'est déjà
    /// inviter. Réponse : [`ServerMsg::PorteOuverte`], ou `Error`.
    ///
    /// Un client neuf ne l'envoie qu'après la preuve que le serveur en face
    /// sert les portes (`portes: true` dans son [`ServerMsg::Welcome`]) :
    /// un serveur antérieur répondrait « message invalide ».
    PorteOuvrir {
        /// Le nom de la porte dans le lien : 3 à 24 caractères parmi
        /// `[a-z0-9-]` (voir [`slug_valide`]), choisi par l'hôte pour être
        /// criable en vocal (« salon1 »). Unique parmi les portes ouvertes.
        slug: String,
        /// Le nom du salon temporaire, nettoyé comme un nom de salon.
        /// Vide : le serveur reprend le slug.
        #[serde(default)]
        nom_salon: String,
        /// Durée de vie demandée en secondes, bornée par le serveur à
        /// [`PORTE_TTL_MAX_SECS`]. 0 = la borne. La porte ferme de toute
        /// façon [`PORTE_VIDE_SECS`] après le départ du dernier invité.
        #[serde(default)]
        ttl_secs: u64,
    },
    /// Accepter ou refuser quelqu'un qui frappe à une porte
    /// ([`ServerMsg::PorteDemande`]). Répondent l'hôte de la porte et tout
    /// connecté qui détient « Expulser » — la première réponse l'emporte,
    /// les autres reçoivent un `Info`.
    PorteRepondre {
        demande_id: u64,
        accepter: bool,
        /// Dit à l'invité s'il est refusé (« pas ce soir »). Libre, borné.
        #[serde(default)]
        motif: String,
    },
    /// Mettre un invité web à la porte. Hôte ou « Expulser ». Il peut
    /// frapper à nouveau ; c'est au serveur de tenir son adresse à l'écart
    /// s'il insiste.
    PorteExpulser { invite_id: UserId },
    /// Fermer une porte : les invités sont congédiés
    /// ([`ServerMsg::PorteFermee`]) et le salon temporaire **effacé**, pas
    /// archivé — ce que des inconnus ont écrit n'a pas à rester. Hôte ou
    /// « Expulser ».
    PorteFermer { slug: String },
    /// Faire entrer un invité web dans un salon vocal, ou l'en sortir
    /// (`channel: None`). L'invité n'a pas de compte, donc pas de
    /// « Rejoindre le vocal » : c'est l'hôte qui décide pour lui, comme
    /// `AdminVoiceMove`. Hôte ou « Déplacer en vocal ».
    PorteVocal {
        invite_id: UserId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<ChannelId>,
    },
    /// « Lui offrir ki-chat » : le serveur crée une invitation à usage
    /// unique, valable sept jours, au nom de l'auteur, la pousse à l'invité
    /// par sa porte ([`ServerMsg::PorteInvitation`]) et la poste en message
    /// système dans le salon, lien de téléchargement compris. Permission
    /// « Créer des invitations » — la même que pour un code ordinaire.
    PorteOffrir { invite_id: UserId },
    /// Keepalive.
    Ping,
}

/// Messages envoyés par le serveur au client (flux de contrôle, JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    /// Réponse à Auth : identité attribuée + jeton voix pour l'UDP.
    Welcome {
        user_id: UserId,
        voice_token: u64,
        udp_port: u16,
        /// Clé de chiffrement voix de la session (32 octets, hex).
        /// Distribuée sur le flux de contrôle, lui-même dans le tunnel
        /// TLS 1.3 de QUIC.
        voice_key: String,
        /// Vrai si ce compte a toutes les permissions. Conservé : le client
        /// en ligne de commande et les versions antérieures s'en servent.
        #[serde(default)]
        is_admin: bool,
        /// Permissions effectives du destinataire, pour que l'interface
        /// n'affiche que les boutons qui aboutiront.
        #[serde(default)]
        perms: Perms,
        #[serde(default)]
        rank: u16,
        /// Tous les rôles du serveur : les couleurs et les badges en
        /// dépendent, pas seulement l'administration.
        #[serde(default)]
        roles: Vec<RoleInfo>,
        /// **Filtrée** pour ce destinataire : un salon restreint n'apparaît
        /// pas dans la liste de qui n'y a pas accès.
        channels: Vec<ChannelInfo>,
        /// Identité du serveur (nom, logo), telle que ses admins l'ont réglée.
        #[serde(default)]
        server: ServerInfo,
        /// Ce serveur sert les **portes web** (depuis 0.1.44). C'est la seule
        /// preuve sur laquelle le client s'appuie avant d'envoyer un
        /// `Porte*` : un serveur antérieur ne pose pas le champ — il reste
        /// faux — et répondrait « message invalide ». Un `NonLus` reçu ne
        /// prouve rien ici : la 0.1.43 l'envoie déjà.
        #[serde(default)]
        portes: bool,
    },
    /// L'identité du serveur vient de changer : poussée à tout le monde.
    ServerInfo { server: ServerInfo },
    /// Photo de profil d'un membre : réponse à `RequestAvatars`, ou envoi
    /// spontané quand quelqu'un change la sienne. `data` à `None` = plus de
    /// photo, on revient au monogramme.
    Avatar {
        user_id: UserId,
        /// Empreinte du contenu, à comparer avec celle de `Member`.
        hash: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<String>,
    },
    /// Un utilisateur a rejoint le salon.
    UserJoined { user_id: UserId, username: String },
    /// Un utilisateur a quitté le salon.
    UserLeft { user_id: UserId },
    /// Message texte relayé — aux **lecteurs** du salon, ceux qui l'ont
    /// ouvert. Les autres reçoivent un [`ServerMsg::Nouveau`].
    Chat {
        user_id: UserId,
        username: String,
        text: String,
        /// Millisecondes depuis l'époque Unix.
        ts: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<ReplyRef>,
        /// Le salon d'où il vient. `0` = serveur antérieur, qui ne le
        /// disait pas : le client s'en remet alors au salon qu'il lit. Un
        /// client antérieur ignore le champ.
        #[serde(default)]
        channel: ChannelId,
    },
    /// Un message vient d'être écrit dans un salon que le destinataire
    /// **peut voir mais ne lit pas** : de quoi poser une pastille, sonner
    /// s'il est nommé, sans lui envoyer une conversation qu'il n'affiche
    /// pas. Un client antérieur jette ce message sans bruit — c'est ce qui
    /// interdit d'envoyer un `Chat` aux non-lecteurs : il l'afficherait
    /// dans le mauvais fil.
    ///
    /// Le texte voyage entier (borné comme un `Chat`) : c'est le client qui
    /// reconnaît une mention, avec la même règle qu'à l'affichage.
    Nouveau {
        channel: ChannelId,
        user_id: UserId,
        username: String,
        text: String,
        ts: u64,
    },
    /// Où en est le destinataire dans chaque salon visible, à la connexion :
    /// envoyé après `Members`. Un salon sans rien de neuf y figure aussi,
    /// avec `non_lus` à 0 — la liste dit du même coup « ce serveur tient
    /// les lus », ce qui autorise le client à envoyer des [`ClientMsg::Lu`].
    NonLus { salons: Vec<NonLuSalon> },
    /// Quelqu'un te poke : `username` te veut. Même forme que `UserJoined`.
    /// Un client antérieur jette ce message sans bruit.
    Poke { user_id: UserId, username: String },
    /// Ton poke n'est pas parti : `user_id` est la cible visée — de quoi
    /// griser son bouton un moment —, `message` dit pourquoi, en toutes
    /// lettres et en français (« Nono est en vocal », « trop de pokes —
    /// attends un peu »). Un `Error` aurait fait l'affaire pour l'affichage,
    /// pas pour rattacher le refus à la cible.
    PokeRefuse { user_id: UserId, message: String },
    /// Quelqu'un a posé ou retiré une réaction sur un message du salon.
    Reaction {
        channel: ChannelId,
        message: MsgRef,
        emoji: String,
        by: UserId,
        on: bool,
    },
    /// Un message du salon a été supprimé : il disparaît chez tout le monde.
    MessageDeleted { channel: ChannelId, message: MsgRef },
    /// Un message du salon a été modifié par son auteur : voici son texte.
    MessageEdited { channel: ChannelId, message: MsgRef, text: String },
    /// Réponse à `LierRiot` / `DelierRiot` : réussi ou non, et pourquoi.
    LiaisonRiot {
        ok: bool,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        riot_id: Option<String>,
    },
    /// La fiche d'un membre (`None` : pas lié, ou rien encore).
    FicheValorant {
        user_id: UserId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fiche: Option<FicheValorant>,
    },
    /// L'état du bot musique, à la connexion et à chaque changement.
    MusiqueEtat { etat: EtatMusique },
    /// Les résultats d'une recherche du bot, au demandeur.
    MusiqueResultats { texte: String, pistes: Vec<Piste> },
    /// Toutes les fiches du groupe, pour la page de stats — et les
    /// prochains matchs d'esport, si le serveur les a.
    ///
    /// Chaque fiche est un **résumé** (voir [`FicheValorant::resume`]) qui
    /// porte son [`BilanMembre`] ; la fiche complète ne part que par
    /// [`ServerMsg::FicheValorant`], à la demande. Le tout tient sous
    /// [`STATS_MAX_BYTES`].
    StatsValorant {
        fiches: Vec<FicheMembre>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        esports: Vec<MatchEsport>,
        /// Parties commencées sur trente jours, tous membres et modes,
        /// par `[jour UTC 0 = lundi … 6][heure 0..24]` : 168 cases, à lire
        /// `jour * 24 + heure`. Vide si rien — ou si le serveur est d'avant.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        activite: Vec<u16>,
    },
    /// Historique demandé.
    History { messages: Vec<ChatRecord> },
    /// Résultats d'une recherche, du plus ancien au plus récent.
    SearchResults {
        /// La requête, rappelée telle qu'elle a été reçue.
        ///
        /// On tape vite et le serveur relit des fichiers : deux réponses
        /// peuvent revenir dans le désordre. Sans ce rappel, une réponse
        /// périmée écraserait la bonne — le classique de toute recherche
        /// au fil de la frappe.
        query: String,
        hits: Vec<SearchHit>,
        /// Vrai s'il y avait plus de résultats que la limite : ceux rendus
        /// sont alors les plus récents.
        #[serde(default)]
        more: bool,
    },
    /// Page d'historique plus ancienne, à **ajouter au-dessus** de ce qui est
    /// déjà affiché — au contraire de `History`, qui remplace tout.
    HistoryPage {
        messages: Vec<ChatRecord>,
        /// Faux quand on a atteint le début du salon : le client cesse alors
        /// de redemander à chaque défilement.
        #[serde(default)]
        more: bool,
        /// Salon d'où vient cette page.
        ///
        /// La réponse est produite hors de l'ordre du flux — le serveur relit
        /// le fichier du salon sur son pool bloquant — si bien qu'elle peut
        /// arriver après un changement de salon. Sans ce champ, le client
        /// collait les messages d'une conversation en tête d'une autre.
        /// `0` = serveur antérieur, le client ne peut alors que faire confiance.
        #[serde(default)]
        channel: ChannelId,
    },
    /// État vocal d'un membre du salon.
    VoiceState {
        user_id: UserId,
        speaking: bool,
        /// Micro coupé volontairement. Absent d'un serveur antérieur : faux.
        #[serde(default)]
        muted: bool,
    },
    /// Un membre diffuse son écran. Annoncé au salon — SANS la clé : elle ne
    /// se remet qu'à qui demande à regarder, après vérification.
    StreamStarted {
        stream_id: u32,
        user_id: UserId,
        meta: StreamMeta,
    },
    /// La diffusion s'arrête (volontairement, ou par départ/déconnexion).
    StreamStopped { stream_id: u32 },
    /// Réponse à StreamStart : l'identifiant attribué (le streamer le grave
    /// dans chaque en-tête de trame).
    StreamGranted { stream_id: u32 },
    /// Réponse à Watch, au seul demandeur : la clé de déchiffrement du
    /// stream (tenue par le serveur pour la durée du stream, remise après
    /// vérification que le demandeur partage le salon vocal du streamer).
    WatchAccepted {
        stream_id: u32,
        /// Clé XChaCha20-Poly1305 du stream (32 octets, hex).
        stream_key: String,
        meta: StreamMeta,
    },
    /// Regard refusé (pas dans le salon vocal du streamer, stream éteint…).
    WatchDenied { stream_id: u32, reason: String },
    /// Au streamer : un spectateur (nouveau, ou qui a perdu pied) a besoin
    /// d'une trame clé. Cadence bornée par le serveur (≤ 1 / 500 ms par
    /// qualité).
    KeyframeNeeded {
        stream_id: u32,
        /// Sur la qualité basse (sinon la haute, la seule d'avant 0.1.46).
        #[serde(default)]
        basse: bool,
    },
    /// Les caractéristiques d'un stream ont changé (dimensions, débit).
    StreamMetaChanged { stream_id: u32, meta: StreamMeta },
    /// Au streamer : le palier de débit que ses spectateurs avalent, à
    /// appliquer à l'encodeur — il descend dès qu'un lien sature, remonte
    /// d'un cran toutes les cinq secondes sans saturation, et revient au
    /// réglage quand tout passe. Un client d'avant l'ignore.
    StreamBudget {
        stream_id: u32,
        /// Le débit de la qualité haute, en kbit/s.
        kbps: u32,
        /// La qualité basse à encoder en plus, en kbit/s — des spectateurs
        /// dont la connexion ne suit pas la haute la regardent. `None` :
        /// personne, elle s'arrête. Seulement vers un streamer qui a dit
        /// `couches` à `StreamStart`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        basse: Option<u32>,
        /// Le palier vient de la connexion **du streamer** (ses trames
        /// arrivent en retard, ou pas du tout), pas d'un spectateur.
        #[serde(default)]
        montant: bool,
    },
    /// Liste complète des membres. Envoyée à la connexion, et chaque fois
    /// qu'un changement touche potentiellement tout le monde (rôles remaniés,
    /// salon supprimé).
    Members { members: Vec<Member> },
    /// **Un seul** membre a changé : il vient de se connecter, de se
    /// déconnecter, d'entrer ou de sortir d'un vocal. Le client l'insère ou
    /// le remplace dans sa liste, sur la foi de `user_id`.
    ///
    /// C'est la raison d'être de ce message. La liste entière partait à
    /// chaque bascule, et elle porte **tous les comptes non bannis** — pas
    /// seulement les connectés. Un serveur de trente habitués qui a vu passer
    /// deux cents personnes en un an rediffusait donc deux cents membres,
    /// trente fois, à chaque entrée en vocal. Mesuré à vingt clients qui se
    /// connectent : 504 rosters, près d'un mégaoctet de contrôle.
    ///
    /// Un client antérieur ignore ce message — tous les `match` du protocole
    /// sont exhaustifs et tolèrent l'inconnu — et il verra simplement la
    /// présence se rafraîchir un peu moins souvent, aux `Members` complets.
    MemberUpdate { member: Member },
    /// Erreur (auth refusée, salon inconnu, ...).
    Error { message: String },
    /// Le destinataire vient d'être expulsé par un admin.
    Kicked {
        #[serde(default)]
        reason: String,
    },
    /// État admin : tous les comptes + les invitations actives.
    AdminInfo {
        users: Vec<AccountInfo>,
        invites: Vec<InviteInfo>,
    },
    /// Journal d'audit, du plus récent au plus ancien.
    AuditLog { records: Vec<AuditRecord> },
    /// Définition de tous les rôles, poussée à chaque changement.
    Roles { roles: Vec<RoleInfo> },
    /// Ce que le destinataire a désormais le droit de faire.
    ///
    /// Poussé dès que ses rôles changent. Sans ce message, `perms` et `rank`
    /// ne voyageaient que dans `Welcome` : promouvoir quelqu'un ne changeait
    /// rien chez lui jusqu'à ce qu'il relance l'application, et le
    /// rétrograder lui laissait des boutons qui échouaient tous.
    Perms {
        #[serde(default)]
        perms: Perms,
        #[serde(default)]
        rank: u16,
        /// Vrai si ce compte a toutes les permissions. Comme dans `Welcome`,
        /// pour les clients qui s'en servent encore.
        #[serde(default)]
        is_admin: bool,
    },
    /// La liste des salons a changé. **Calculée par destinataire** : elle
    /// diffère d'une personne à l'autre selon ce qu'elle a le droit de voir.
    ChannelsUpdated { channels: Vec<ChannelInfo> },
    /// Entrée refusée dans un salon vocal verrouillé.
    VoiceLocked {
        channel: ChannelId,
        /// Vrai si un mot de passe a été fourni mais qu'il est faux, faux
        /// s'il n'y en avait pas — de quoi distinguer « il en faut un » de
        /// « ce n'est pas le bon ».
        #[serde(default)]
        wrong: bool,
    },
    /// Un code d'invitation vient d'être créé (réponse à AdminCreateInvite).
    InviteCreated { code: String },
    /// Message d'information (succès d'une action admin, ...).
    Info { message: String },
    /// Rapport qualité réseau : pertes mesurées par le serveur sur le flux
    /// montant du destinataire (en %). Sert au débit adaptatif.
    NetQuality { loss_pct: f32 },
    /// Réponse à `PorteOuvrir`, à l'hôte seul : la porte est ouverte, voici
    /// le lien à partager. Le salon temporaire arrive à part, par
    /// `ChannelsUpdated`, avec son `expire_le`.
    PorteOuverte {
        slug: String,
        /// Le lien complet (`https://ts.baws.fun:8080/salon1`), construit
        /// par le serveur depuis son adresse web publique — réglée dans
        /// Admin → Serveur, ou `KI_PUBLIC_URL` —, jamais depuis l'en-tête
        /// `Host` d'une requête, qu'un visiteur choisit. Sans adresse
        /// connue, le chemin seul (`/salon1`) : le client le complète avec
        /// l'adresse par laquelle il joint le serveur.
        url: String,
        /// Le salon temporaire créé pour cette porte.
        salon: ChannelId,
        /// Fermeture au plus tard (ms Unix).
        expire_le: u64,
    },
    /// Quelqu'un frappe à une porte : de quoi afficher « Kevin veut
    /// rejoindre par le web » avec Accepter / Refuser. Envoyé à l'hôte et
    /// à tout connecté détenant « Expulser ». Se répond par
    /// [`ClientMsg::PorteRepondre`]. Un client antérieur jette ce message
    /// sans bruit — il ne peut pas répondre, c'est la seule conséquence.
    PorteDemande {
        slug: String,
        demande_id: u64,
        /// Le nom qu'il s'est donné, déjà nettoyé comme un pseudo par le
        /// serveur — le client passe quand même par `safe_display`.
        nom: String,
        /// Son adresse, tronquée (« 82.65.x.x ») : assez pour reconnaître
        /// un insistant, pas assez pour le pister.
        #[serde(default)]
        ip_masquee: String,
    },
    /// L'état complet d'une porte, poussé à l'hôte et aux détenteurs
    /// d'« Expulser » à chaque changement — une entrée, un départ, une
    /// demande qui arrive ou qui expire. Un état et non des événements :
    /// trente demandes en rafale font une seule liste, pas trente
    /// bannières.
    PorteEtat {
        slug: String,
        salon: ChannelId,
        #[serde(default)]
        invites: Vec<InviteWeb>,
        #[serde(default)]
        demandes: Vec<DemandeWeb>,
        /// Fermeture au plus tard (ms Unix).
        #[serde(default)]
        expire_le: u64,
        /// Le lien de la porte, comme dans [`ServerMsg::PorteOuverte`] —
        /// pour qui la gère sans l'avoir ouverte, pour l'hôte revenu d'une
        /// reconnexion, et à nouveau quand l'adresse publique change. Vide
        /// chez un serveur d'avant 0.1.45.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        url: String,
    },
    /// À l'invité web, par sa porte : « Voilà ki-chat ». Une invitation à
    /// usage unique, valable sept jours, créée par le serveur au nom de
    /// l'hôte quand il clique « L'inviter dans ki-chat » — et postée en
    /// message système dans le salon, pour que tout le monde la voie.
    /// Un client ki-chat ne le reçoit jamais.
    PorteInvitation {
        code: String,
        /// L'adresse à saisir (« ts.baws.fun:9988 »).
        serveur: String,
        /// Le lien de téléchargement ([`PORTE_TELECHARGEMENT`]).
        telechargement: String,
    },
    /// La porte est fermée : par l'hôte, par expiration, ou faute
    /// d'invité. À l'hôte, aux détenteurs d'« Expulser » et aux invités
    /// (pour qui c'est la fin de la page). Le salon disparaît par
    /// `ChannelsUpdated`, comme d'habitude.
    PorteFermee {
        slug: String,
        /// En toutes lettres et en français (« fermée par redik »,
        /// « expirée », « plus personne depuis dix minutes »).
        #[serde(default)]
        motif: String,
    },
    /// Réponse au Ping.
    Pong,
}

// ---------------------------------------------------------------------
// Les portes web
// ---------------------------------------------------------------------
//
// Une porte, c'est un lien `https://<serveur>/s/<slug>` qui mène à un salon
// textuel temporaire. Qui frappe donne un nom, un membre l'accepte, et il
// écrit dans le salon depuis son navigateur, sans compte. À la fermeture, le
// salon est effacé. Les invités ne sont pas des comptes : ils vivent dans
// une plage d'identifiants réservée, portent « (web) » dans leur nom, et ne
// reçoivent jamais rien d'un autre salon.

/// Premier identifiant de la plage réservée aux invités web : `1 << 62`.
///
/// Les comptes partent de 1 et s'incrémentent ; 0 est le serveur lui-même,
/// [`MUSIQUE_ID`] le bot. Rien ne s'approche de 4,6 × 10¹⁸ comptes : aucune
/// collision possible. Un invité reçoit un identifiant unique dans cette
/// plage pour la durée de sa session ; le même nom qui revient plus tard
/// en reçoit un autre.
pub const INVITE_ID_BASE: UserId = 1 << 62;
/// Dernier identifiant de la plage des invités, exclus : `1 << 63`. Le bot
/// musique, tout en haut, reste en dehors.
pub const INVITE_ID_FIN: UserId = 1 << 63;
/// Le pas entre deux identifiants d'invités : `1 << 10`. La page web lit
/// ces identifiants en JavaScript — dans le JSON des messages, et dans les
/// trames voix — où un nombre est un double à 53 bits de mantisse : entre
/// 2⁶² et 2⁶³, seuls les multiples de 2¹⁰ s'écrivent exactement, les
/// autres s'arrondissent au plus proche et deux invités se confondraient.
/// Le serveur n'attribue donc que `INVITE_ID_BASE + k × INVITE_ID_PAS` ;
/// un client ki-chat, en `u64`, n'en a que faire.
pub const INVITE_ID_PAS: UserId = 1 << 10;

/// Un identifiant de la plage des invités web ?
pub fn est_invite(id: UserId) -> bool {
    (INVITE_ID_BASE..INVITE_ID_FIN).contains(&id)
}

/// Un compte, au sens d'une personne membre : ni le serveur (0), ni le bot
/// musique, ni un invité web. C'est le filtre des mentions et des sons —
/// ce qui vient d'ailleurs que d'un membre ne nomme personne.
pub fn est_compte(id: UserId) -> bool {
    id != 0 && id != MUSIQUE_ID && !est_invite(id)
}

/// Ce que le serveur colle au nom d'un invité (« Kevin (web) ») pour que
/// les membres voient d'un coup d'œil que ce n'est pas un compte. La
/// contrepartie : aucun pseudo de compte ne peut finir ainsi.
pub const INVITE_SUFFIXE: &str = " (web)";

/// Longueur d'un slug de porte, en caractères.
pub const PORTE_SLUG_MIN: usize = 3;
pub const PORTE_SLUG_MAX: usize = 24;

/// Un slug de porte acceptable : de [`PORTE_SLUG_MIN`] à [`PORTE_SLUG_MAX`]
/// caractères parmi `a-z`, `0-9` et `-`. Minuscules seulement — un lien se
/// dicte à voix haute, et « Salon1 » et « salon1 » ne doivent pas être deux
/// portes. La même règle chez le client (pour ne pas envoyer ce qui sera
/// refusé) et chez le serveur (qui ne croit pas le client).
pub fn slug_valide(slug: &str) -> bool {
    (PORTE_SLUG_MIN..=PORTE_SLUG_MAX).contains(&slug.len())
        && slug.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Durée de vie maximale d'une porte : six heures, une soirée entière —
/// une partie avec un invité a déjà duré cinq heures et demie. Une porte
/// n'est pas pour autant un salon permanent ouvert sur Internet.
pub const PORTE_TTL_MAX_SECS: u64 = 6 * 60 * 60;
/// Une porte sans invité ferme au bout de dix minutes.
pub const PORTE_VIDE_SECS: u64 = 10 * 60;
/// Portes ouvertes en même temps, au plus.
pub const PORTES_MAX: usize = 5;
/// Invités par porte, au plus.
pub const PORTE_INVITES_MAX: usize = 20;
/// Demandes en attente par porte, au plus — et une seule par adresse.
pub const PORTE_DEMANDES_MAX: usize = 5;
/// Longueur maximale du motif d'un refus, en caractères.
pub const MAX_PORTE_MOTIF: usize = 200;
/// Où télécharger ki-chat, tel que la page web et l'invitation le donnent.
pub const PORTE_TELECHARGEMENT: &str = "https://github.com/Redik123/ki-chat/releases/latest";

/// Longueur maximale de l'adresse web publique, en octets.
pub const MAX_ADRESSE_WEB: usize = 200;

/// L'adresse web publique telle qu'un admin la tape, rendue canonique :
/// `ts.baws.fun:8080` devient `https://ts.baws.fun:8080` — sans schéma, un
/// navigateur tenterait `http://`, et la page, servie en TLS, ne
/// s'ouvrirait pas. Schéma et hôte en minuscules, pas de barre finale ; un
/// chemin est permis (un serveur derrière `https://exemple.fr/ki`). Vide
/// reste vide : retour à l'automatique.
///
/// Refusé : un autre schéma que `http(s)`, un hôte vide ou illisible, un
/// port hors de `1..=65535`, un identifiant (`moi@`), une requête, un
/// fragment, une espace. La même règle chez le client (qui montre l'erreur
/// avant l'envoi) et chez le serveur (qui ne croit pas le client).
pub fn normaliser_adresse_web(brut: &str) -> Result<String, String> {
    let brut = brut.trim();
    if brut.is_empty() {
        return Ok(String::new());
    }
    if brut.len() > MAX_ADRESSE_WEB {
        return Err(format!("adresse trop longue : {MAX_ADRESSE_WEB} caractères au plus"));
    }
    let (schema, reste) = match brut.split_once("://") {
        Some((schema, reste)) => (schema.to_ascii_lowercase(), reste),
        None => ("https".to_string(), brut),
    };
    if schema != "https" && schema != "http" {
        return Err(format!("« {schema}:// » : l'adresse commence par https://"));
    }
    let reste = reste.trim_end_matches('/');
    let (autorite, chemin) = reste.split_at(reste.find('/').unwrap_or(reste.len()));
    let (hote, port) = match autorite.strip_prefix('[') {
        // IPv6 : entre crochets, le port après.
        Some(v6) => {
            let Some((ip, apres)) = v6.split_once(']') else {
                return Err("adresse IPv6 sans crochet fermant".into());
            };
            if ip.is_empty() || !ip.chars().all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.') {
                return Err(format!("« {ip} » n'est pas une adresse IPv6"));
            }
            let port = match apres {
                "" => None,
                p => Some(p.strip_prefix(':').ok_or("après l'adresse IPv6, seul un port (« :8080 »)")?),
            };
            (format!("[{}]", ip.to_ascii_lowercase()), port)
        }
        None => {
            let (hote, port) = match autorite.rsplit_once(':') {
                Some((hote, port)) => (hote, Some(port)),
                None => (autorite, None),
            };
            if hote.is_empty() {
                return Err("il manque l'hôte : ex. ts.baws.fun:8080".into());
            }
            if !hote.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
                return Err(format!(
                    "« {hote} » : un hôte ne contient que des lettres, des chiffres, des points et des tirets"
                ));
            }
            (hote.to_ascii_lowercase(), port)
        }
    };
    let port = match port {
        None => String::new(),
        Some(p) => match p.parse::<u16>() {
            Ok(n) if n > 0 && p.chars().all(|c| c.is_ascii_digit()) => format!(":{n}"),
            _ => return Err(format!("« {p} » n'est pas un port (1 à 65535)")),
        },
    };
    if !chemin.chars().all(|c| c.is_ascii_alphanumeric() || "/-._~%".contains(c)) {
        return Err("après l'hôte, ni espace, ni « ? », ni « # »".into());
    }
    Ok(format!("{schema}://{hote}{port}{chemin}"))
}

/// Un invité web présent dans un salon temporaire, vu de l'hôte.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteWeb {
    /// Son identifiant de session, dans la plage [`INVITE_ID_BASE`].
    pub invite_id: UserId,
    /// Son nom **avec** le suffixe « (web) », tel qu'il signe ses messages.
    pub nom: String,
    /// Entré à (ms Unix).
    #[serde(default)]
    pub depuis: u64,
    /// Le salon vocal où l'hôte l'a mis, s'il y est ([`ClientMsg::PorteVocal`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vocal: Option<ChannelId>,
}

/// Quelqu'un qui attend derrière une porte.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DemandeWeb {
    pub demande_id: u64,
    pub nom: String,
    /// A frappé à (ms Unix).
    #[serde(default)]
    pub depuis: u64,
}

#[cfg(test)]
mod portes_tests {
    use super::*;

    /// La plage des invités ne touche ni les comptes, ni le serveur, ni le
    /// bot ; et `est_compte` est le filtre exact des mentions.
    #[test]
    fn la_plage_des_invites_est_a_part() {
        assert!(!est_invite(0));
        assert!(!est_invite(1));
        assert!(!est_invite(INVITE_ID_BASE - 1));
        assert!(est_invite(INVITE_ID_BASE));
        assert!(est_invite(INVITE_ID_BASE + 12_345));
        assert!(est_invite(INVITE_ID_FIN - 1));
        assert!(!est_invite(INVITE_ID_FIN));
        assert!(!est_invite(MUSIQUE_ID));
        assert!(!est_invite(u64::MAX));

        assert!(est_compte(1));
        assert!(est_compte(INVITE_ID_BASE - 1));
        assert!(!est_compte(0));
        assert!(!est_compte(MUSIQUE_ID));
        assert!(!est_compte(INVITE_ID_BASE));
    }

    /// Un identifiant d'invité survit au passage par un double JavaScript :
    /// c'est ce que garantit le pas, et ce qu'un pas de 1 ne garantit pas.
    #[test]
    fn un_identifiant_d_invite_tient_dans_un_double() {
        for k in [0u64, 1, 2, 3, 19, 20_000] {
            let id = INVITE_ID_BASE + k * INVITE_ID_PAS;
            assert!(est_invite(id));
            assert_eq!(id as f64 as u64, id, "k = {k}");
        }
        assert_ne!((INVITE_ID_BASE + 1) as f64 as u64, INVITE_ID_BASE + 1, "sans le pas, l'arrondi mange l'unité");
    }

    /// Un slug se dicte à voix haute : minuscules, chiffres, tirets, ni
    /// trop court ni trop long.
    #[test]
    fn un_slug_de_porte_se_dicte_a_voix_haute() {
        assert!(slug_valide("salon1"));
        assert!(slug_valide("abc"));
        assert!(slug_valide("soiree-du-samedi-2026"));
        assert!(slug_valide(&"a".repeat(PORTE_SLUG_MAX)));
        assert!(!slug_valide("ab"));
        assert!(!slug_valide(&"a".repeat(PORTE_SLUG_MAX + 1)));
        assert!(!slug_valide("Salon1"));
        assert!(!slug_valide("salon 1"));
        assert!(!slug_valide("salon_1"));
        assert!(!slug_valide("salon/1"));
        assert!(!slug_valide("été"));
        assert!(!slug_valide(""));
    }

    /// Chaque message de porte fait l'aller-retour tel quel, sous le nom
    /// `snake_case` que la page web lit aussi.
    #[test]
    fn les_messages_de_porte_font_l_aller_retour() {
        let allers: Vec<ClientMsg> = vec![
            ClientMsg::PorteOuvrir { slug: "salon1".into(), nom_salon: "Soirée".into(), ttl_secs: 3600 },
            ClientMsg::PorteRepondre { demande_id: 7, accepter: false, motif: "pas ce soir".into() },
            ClientMsg::PorteExpulser { invite_id: INVITE_ID_BASE + 1 },
            ClientMsg::PorteFermer { slug: "salon1".into() },
            ClientMsg::PorteVocal { invite_id: INVITE_ID_BASE + 1, channel: Some(4) },
            ClientMsg::PorteVocal { invite_id: INVITE_ID_BASE + 1, channel: None },
            ClientMsg::PorteOffrir { invite_id: INVITE_ID_BASE + 1 },
        ];
        let types = [
            "porte_ouvrir",
            "porte_repondre",
            "porte_expulser",
            "porte_fermer",
            "porte_vocal",
            "porte_vocal",
            "porte_offrir",
        ];
        for (msg, attendu) in allers.iter().zip(types) {
            let json = serde_json::to_string(msg).unwrap();
            assert!(json.contains(&format!("\"type\":\"{attendu}\"")), "{json}");
            let relu: ClientMsg = serde_json::from_str(&json).unwrap();
            assert_eq!(serde_json::to_string(&relu).unwrap(), json);
        }
        // `PorteVocal` sans salon : le champ ne voyage pas.
        let sortie = serde_json::to_string(&allers[5]).unwrap();
        assert!(!sortie.contains("channel"), "{sortie}");

        let retours: Vec<ServerMsg> = vec![
            ServerMsg::PorteOuverte {
                slug: "salon1".into(),
                url: "https://ts.baws.fun/s/salon1".into(),
                salon: 9,
                expire_le: 1_800_000_000_000,
            },
            ServerMsg::PorteDemande {
                slug: "salon1".into(),
                demande_id: 7,
                nom: "Kevin".into(),
                ip_masquee: "82.65.x.x".into(),
            },
            ServerMsg::PorteEtat {
                slug: "salon1".into(),
                salon: 9,
                invites: vec![InviteWeb {
                    invite_id: INVITE_ID_BASE + 1,
                    nom: "Kevin (web)".into(),
                    depuis: 1_700_000_000_000,
                    vocal: Some(4),
                }],
                demandes: vec![DemandeWeb { demande_id: 8, nom: "Léa".into(), depuis: 1_700_000_001_000 }],
                expire_le: 1_800_000_000_000,
                url: "https://ts.baws.fun:8080/salon1".into(),
            },
            ServerMsg::PorteInvitation {
                code: "ki-abcdefghij".into(),
                serveur: "ts.baws.fun:9988".into(),
                telechargement: PORTE_TELECHARGEMENT.into(),
            },
            ServerMsg::PorteFermee { slug: "salon1".into(), motif: "expirée".into() },
        ];
        let types = ["porte_ouverte", "porte_demande", "porte_etat", "porte_invitation", "porte_fermee"];
        for (msg, attendu) in retours.iter().zip(types) {
            let json = serde_json::to_string(msg).unwrap();
            assert!(json.contains(&format!("\"type\":\"{attendu}\"")), "{json}");
            let relu: ServerMsg = serde_json::from_str(&json).unwrap();
            assert_eq!(serde_json::to_string(&relu).unwrap(), json);
        }

        // L'état d'une porte se relit champ à champ.
        let json = serde_json::to_string(&retours[2]).unwrap();
        let ServerMsg::PorteEtat { invites, demandes, expire_le, .. } = serde_json::from_str(&json).unwrap() else {
            panic!("ce n'est pas un PorteEtat");
        };
        assert_eq!(invites[0].vocal, Some(4));
        assert_eq!(invites[0].nom, "Kevin (web)");
        assert_eq!(demandes[0].demande_id, 8);
        assert_eq!(expire_le, 1_800_000_000_000);
    }

    /// Les champs optionnels ont leur défaut : un client d'avant les portes
    /// envoie `porte_ouvrir` avec le seul slug, un serveur d'avant n'envoie
    /// ni `expire_le` sur un salon ni `invite` sur un membre — et un salon
    /// ordinaire d'aujourd'hui ne dit pas qu'il n'expire pas.
    #[test]
    fn les_anciennes_formes_se_relisent() {
        let msg: ClientMsg = serde_json::from_str(r#"{"type":"porte_ouvrir","slug":"salon1"}"#).unwrap();
        let ClientMsg::PorteOuvrir { slug, nom_salon, ttl_secs } = msg else { panic!("ce n'est pas un PorteOuvrir") };
        assert_eq!(slug, "salon1");
        assert!(nom_salon.is_empty());
        assert_eq!(ttl_secs, 0);
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"porte_repondre","demande_id":3,"accepter":true}"#).unwrap();
        let ClientMsg::PorteRepondre { motif, accepter, .. } = msg else { panic!("ce n'est pas un PorteRepondre") };
        assert!(accepter && motif.is_empty());
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"porte_vocal","invite_id":4611686018427387905}"#).unwrap();
        let ClientMsg::PorteVocal { invite_id, channel } = msg else { panic!("ce n'est pas un PorteVocal") };
        assert!(est_invite(invite_id) && channel.is_none());

        let msg: ServerMsg = serde_json::from_str(
            r#"{"type":"porte_etat","slug":"salon1","salon":9}"#,
        )
        .unwrap();
        let ServerMsg::PorteEtat { invites, demandes, expire_le, .. } = msg else { panic!("ce n'est pas un PorteEtat") };
        assert!(invites.is_empty() && demandes.is_empty() && expire_le == 0);
        let msg: ServerMsg = serde_json::from_str(r#"{"type":"porte_fermee","slug":"salon1"}"#).unwrap();
        let ServerMsg::PorteFermee { motif, .. } = msg else { panic!("ce n'est pas un PorteFermee") };
        assert!(motif.is_empty());

        // Un salon d'un serveur antérieur, sans `expire_le`.
        let ancien = r#"{"id":3,"name":"général","kind":"text","position":0,"locked":false}"#;
        let salon: ChannelInfo = serde_json::from_str(ancien).unwrap();
        assert_eq!(salon.expire_le, None);
        let json = serde_json::to_string(&salon).unwrap();
        assert!(!json.contains("expire_le"), "{json}");
        let temporaire = ChannelInfo { expire_le: Some(1_800_000_000_000), ..salon };
        let json = serde_json::to_string(&temporaire).unwrap();
        assert!(json.contains("\"expire_le\":1800000000000"), "{json}");
        assert_eq!(serde_json::from_str::<ChannelInfo>(&json).unwrap().expire_le, Some(1_800_000_000_000));

        // Un membre d'un serveur antérieur, sans `invite`.
        let ancien = r#"{"user_id":1,"username":"alice","speaking":false}"#;
        let m: Member = serde_json::from_str(ancien).unwrap();
        assert!(!m.invite);
        let json = serde_json::to_string(&m).unwrap();
        assert!(!json.contains("invite"), "un membre ordinaire ne dit pas qu'il n'est pas invité : {json}");
        let invite = Member { user_id: INVITE_ID_BASE, username: "Kevin (web)".into(), invite: true, ..m };
        let json = serde_json::to_string(&invite).unwrap();
        assert!(json.contains("\"invite\":true"), "{json}");
        let relu: Member = serde_json::from_str(&json).unwrap();
        assert!(relu.invite && est_invite(relu.user_id));
        assert!(relu.username.ends_with(INVITE_SUFFIXE));
    }
}

/// Identité publique d'un serveur, définie par ses admins et distribuée
/// aux clients authentifiés.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    /// Nom affiché. Vide = jamais défini, le client retombe sur l'adresse.
    #[serde(default)]
    pub name: String,
    /// Logo : vignette PNG carrée encodée en base64. `None` = pas de logo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Le salon du fil de jeu VALORANT : le serveur y annonce les parties
    /// finies des membres liés. `None` = fil éteint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fil_valorant: Option<ChannelId>,
    /// Les membres sans « Contrôler la musique » peuvent chercher et
    /// ajouter des morceaux en fin de file — pas piloter.
    #[serde(default)]
    pub musique_membres_ajoutent: bool,
    /// L'adresse web publique réglée par un admin
    /// ([`ClientMsg::AdminSetAdresseWeb`]), déjà normalisée : schéma, hôte
    /// et port, sans barre finale. Vide : jamais réglée.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub adresse_web: String,
}

/// Ce qu'un admin veut faire du logo du serveur.
///
/// Un `Option<Option<String>>` dirait la même chose mais se lirait mal, en
/// Rust comme en JSON.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum IconChange {
    /// Laisser le logo tel quel.
    #[default]
    Keep,
    /// Retirer le logo.
    Clear,
    /// Remplacer le logo.
    Set { data: String },
}

/// Taille maximale d'un logo de serveur, en octets de base64 (~96 Kio).
/// Une vignette 64×64 en fait typiquement 3 à 8.
pub const MAX_SERVER_ICON: usize = 96 * 1024;
/// Même plafond pour une photo de profil.
pub const MAX_AVATAR: usize = MAX_SERVER_ICON;
/// Longueur maximale du nom d'un serveur, en caractères.
pub const MAX_SERVER_NAME: usize = 40;

/// Côté maximal admis pour une vignette (logo de serveur, photo de profil).
/// L'application en produit des 64×64 ; la marge couvre les écrans denses.
pub const MAX_THUMBNAIL_PX: u32 = 256;

// ---------------------------------------------------------------------
// Bornes des entrées
// ---------------------------------------------------------------------
//
// Tout ce qui traverse le réseau vient d'un pair qu'on ne contrôle pas :
// notre application se comporte bien, mais rien n'oblige l'autre bout à
// être notre application. Chaque champ a donc une borne, et elle est
// définie ici pour que le client et le serveur appliquent la même.

/// Longueur maximale d'une ligne du flux de contrôle, en octets.
///
/// C'est la borne la plus fondamentale : elle est **sous** le JSON. Un
/// lecteur de lignes ordinaire fait grandir son tampon jusqu'au prochain
/// saut de ligne — un pair qui n'en envoie jamais épuise la mémoire d'en
/// face sans avoir à s'authentifier. Dimensionnée sur le plus gros message
/// légitime : une vignette en base64 dans son enveloppe JSON.
pub const MAX_LINE: usize = 160 * 1024;

/// Le budget du message de la page du groupe ([`ServerMsg::StatsValorant`]),
/// sous [`MAX_LINE`] avec la marge que `history.rs` s'accorde déjà : une
/// ligne qui dépasse ferme la connexion des deux côtés, et trente fiches
/// pleines la rempliraient sans mal. Le serveur allège tout le monde d'un
/// cran tant que sa ligne dépasse ce budget.
pub const STATS_MAX_BYTES: usize = MAX_LINE - 8 * 1024;

/// Longueur maximale d'un message de chat, en caractères.
pub const MAX_CHAT_TEXT: usize = 4000;
/// Longueur maximale d'un pseudo, en caractères.
pub const MAX_USERNAME: usize = 32;
/// Longueur maximale d'un mot de passe, en octets. Argon2 travaille à coût
/// fixe, mais rien ne justifie d'accepter un mot de passe démesuré.
pub const MAX_PASSWORD: usize = 256;
/// Longueur maximale d'un code d'invitation, en octets.
pub const MAX_INVITE: usize = 64;
/// Longueur maximale d'une requête de recherche, en caractères.
pub const MAX_SEARCH_QUERY: usize = 128;
/// Résultats de recherche rendus au plus. Au-delà, on ne lit plus une liste :
/// on affine sa requête.
pub const MAX_SEARCH_HITS: usize = 100;
/// Sauts de ligne consécutifs tolérés dans un message.
const MAX_BLANK_LINES: usize = 3;

/// Caractère à retirer d'un texte reçu.
///
/// Deux familles : les caractères de contrôle (hors saut de ligne et
/// tabulation), et les **commandes bidirectionnelles** Unicode. Ces
/// dernières inversent le sens d'affichage du texte qui suit : elles
/// permettent de faire lire à l'écran tout autre chose que ce qui est
/// réellement écrit, donc de maquiller un lien ou d'imiter le message de
/// quelqu'un d'autre.
fn is_dangerous(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t')
        || matches!(c,
            '\u{200e}' | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}')
}

/// Valide et nettoie un message de chat avant de l'accepter.
///
/// Renvoie le texte nettoyé, ou la raison du refus.
pub fn clean_chat(text: &str) -> Result<String, String> {
    let filtered: String = text.chars().filter(|c| !is_dangerous(*c)).collect();
    let trimmed = filtered.trim();
    if trimmed.is_empty() {
        return Err("message vide".into());
    }
    if trimmed.chars().count() > MAX_CHAT_TEXT {
        return Err(format!("message trop long ({MAX_CHAT_TEXT} caractères maximum)"));
    }
    Ok(collapse_blank_lines(trimmed))
}

/// Ramène les enfilades de lignes vides à `MAX_BLANK_LINES` : sans ça, un
/// message de trois caractères peut occuper tout l'écran de chacun.
fn collapse_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = 0usize;
    for c in text.chars() {
        if c == '\n' {
            run += 1;
            if run > MAX_BLANK_LINES {
                continue;
            }
        } else {
            run = 0;
        }
        out.push(c);
    }
    out
}

/// Version sûre à afficher d'un texte reçu : caractères dangereux retirés,
/// longueur bornée.
///
/// Le pendant de [`clean_chat`] côté réception. Le serveur valide déjà ce
/// qu'il relaie, mais le client n'a pas à lui faire confiance pour autant :
/// il peut être plus vieux, modifié, ou hostile. Ici on ne rejette rien —
/// on affiche au mieux, tronqué si besoin.
pub fn safe_display(text: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max_chars * 4));
    for (count, c) in text.chars().filter(|c| !is_dangerous(*c)).enumerate() {
        if count == max_chars {
            out.push('…');
            break;
        }
        out.push(c);
    }
    collapse_blank_lines(&out)
}

/// Vérifie qu'une vignette reçue est bien un petit PNG, **sans la décoder**.
///
/// Le serveur ne peut pas croire le client sur parole : notre application
/// réencode les images en PNG 64×64, mais rien n'empêche quelqu'un d'écrire
/// son propre client et d'envoyer autre chose.
///
/// Le danger n'est pas qu'une image « contienne un virus » — elle n'est
/// jamais exécutée, seulement décodée puis affichée. C'est la **bombe de
/// décompression** : un PNG de quelques kilo-octets peut déclarer
/// 30000×30000 pixels, soit ~3,6 Go réclamés au décodeur de *chaque* client
/// qui l'affiche. Un seul envoi ferait ainsi tomber tout le salon. On lit
/// donc l'en-tête IHDR et on refuse tout ce qui n'est pas une petite image,
/// avant que le moindre décodeur ne soit sollicité.
pub fn check_thumbnail(data: &str) -> Result<(), String> {
    use base64::Engine as _;

    if data.len() > MAX_SERVER_ICON {
        return Err("vignette trop lourde".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| "vignette illisible".to_string())?;
    check_png(&bytes)
}

/// Blocs PNG autorisés : ceux qui portent des pixels, et rien d'autre.
///
/// Un PNG est une suite de blocs typés. Ceux-ci décrivent l'image ;
/// tous les autres (`tEXt`, `zTXt`, `iTXt`, `eXIf`…) ne servent qu'à
/// transporter des métadonnées — et donc, pour qui le veut, n'importe quels
/// octets. On les refuse.
const PIXEL_CHUNKS: [&[u8; 4]; 4] = [b"IHDR", b"PLTE", b"IDAT", b"tRNS"];

/// Contrôle **toute la structure** d'un PNG, sans le décoder : signature,
/// enchaînement des blocs, dimensions, et fin de fichier exacte.
///
/// Ne vérifier que l'en-tête ne suffirait pas. Une vignette peut être
/// parfaitement valide en 64×64 *et* traîner derrière elle des blocs de
/// métadonnées ou des données collées après `IEND` — c'est le principe des
/// fichiers « polyglottes ». Comme le serveur redistribue ce blob à tous les
/// membres, qui l'écrivent sur leur disque, laisser passer ces octets
/// transformerait l'application en canal de distribution de fichiers.
///
/// On exige donc : uniquement les blocs porteurs de pixels, et pas un octet
/// après la fin. Ce qui reste possible — cacher de l'information dans les
/// pixels eux-mêmes — est inévitable pour toute image, et sans danger : ces
/// octets ne sont jamais interprétés, seulement affichés.
pub fn check_png(bytes: &[u8]) -> Result<(), String> {
    const MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.len() < 8 || bytes[..8] != MAGIC {
        return Err("ce n'est pas une image PNG".into());
    }

    let mut pos = 8;
    let mut first = true;
    let mut closed = false;
    while pos < bytes.len() {
        // Un bloc : longueur (4) + type (4) + données + CRC (4).
        if pos + 8 > bytes.len() {
            return Err("bloc PNG tronqué".into());
        }
        let len = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        let kind: &[u8] = &bytes[pos + 4..pos + 8];
        let Some(end) = pos.checked_add(12).and_then(|p| p.checked_add(len)) else {
            return Err("bloc PNG démesuré".into());
        };
        if end > bytes.len() {
            return Err("bloc PNG tronqué".into());
        }

        if first {
            if kind != b"IHDR" || len != 13 {
                return Err("en-tête PNG malformé".into());
            }
            check_dimensions(&bytes[pos + 8..pos + 16])?;
            first = false;
        } else if kind == b"IEND" {
            // Rien ne doit suivre la fin de l'image.
            if end != bytes.len() {
                return Err("données ajoutées après la fin de l'image".into());
            }
            closed = true;
        } else if !PIXEL_CHUNKS.iter().any(|allowed| kind == *allowed) {
            let name = String::from_utf8_lossy(kind).to_string();
            return Err(format!("bloc « {name} » interdit dans une vignette"));
        }

        pos = end;
    }

    if !closed {
        return Err("image PNG incomplète".into());
    }
    Ok(())
}

/// Largeur et hauteur d'un IHDR, en tête de ses 13 octets de données.
fn check_dimensions(ihdr: &[u8]) -> Result<(), String> {
    let field = |at: usize| u32::from_be_bytes([ihdr[at], ihdr[at + 1], ihdr[at + 2], ihdr[at + 3]]);
    let (width, height) = (field(0), field(4));
    if width == 0 || height == 0 {
        return Err("image vide".into());
    }
    if width > MAX_THUMBNAIL_PX || height > MAX_THUMBNAIL_PX {
        return Err(format!(
            "image {width}×{height} : {MAX_THUMBNAIL_PX} pixels de côté au maximum"
        ));
    }
    Ok(())
}

/// Empreinte courte d'une vignette, pour savoir si le cache d'un client est
/// à jour.
///
/// FNV-1a : ce n'est pas une empreinte cryptographique et ça n'a pas à
/// l'être — elle ne sert qu'à comparer deux versions d'une même image. Une
/// collision afficherait une photo périmée, rien de plus.
pub fn avatar_hash(data: Option<&str>) -> Option<String> {
    let data = data?;
    let hash = data.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |acc, b| {
        (acc ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
    });
    Some(format!("{hash:016x}"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountInfo {
    pub username: String,
    pub user_id: UserId,
    pub admin: bool,
    pub banned: bool,
    pub online: bool,
    /// Motif du bannissement en cours, vide s'il n'y en a pas.
    #[serde(default)]
    pub ban_reason: String,
    /// Fin du bannissement (ms Unix). `None` avec `banned` = définitif.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ban_until: Option<u64>,
    /// Qui a banni.
    #[serde(default)]
    pub ban_by: String,
    #[serde(default)]
    pub roles: Vec<RoleId>,
    /// Rang le plus élevé : sert à masquer les actions qui seraient
    /// refusées, plutôt que de les griser.
    #[serde(default)]
    pub rank: u16,
}

/// Nombre d'usages par défaut d'une invitation : un seul, comme avant les
/// invitations permanentes. C'est ce que reçoit un client qui n'envoie pas
/// le champ.
fn default_invite_uses() -> Option<u32> {
    Some(1)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteInfo {
    pub code: String,
    /// `None` = illimité. Un serveur antérieur envoyait un entier nu, que
    /// serde lit toujours comme `Some(n)`.
    #[serde(default = "default_invite_uses")]
    pub uses_left: Option<u32>,
    /// Nombre de comptes réellement créés avec ce code.
    #[serde(default)]
    pub uses: u32,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub created_at: u64,
    /// Expiration (ms Unix). `None` = jamais.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub revoked: bool,
}

/// Une entrée du journal d'audit.
///
/// `action` est une chaîne et non une énumération pour deux raisons : un
/// client plus ancien doit pouvoir afficher une action qu'il ne connaît pas
/// plutôt que d'échouer à désérialiser, et `data/audit.jsonl` reste lisible
/// et « greppable » à la main.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    /// Millisecondes depuis l'époque Unix.
    pub ts: u64,
    /// Verbe stable, jamais traduit : « invite.create », « member.ban »…
    pub action: String,
    /// Auteur de l'action. Vide = le serveur lui-même (expiration d'un ban).
    #[serde(default)]
    pub actor: String,
    /// Compte visé, s'il y en a un.
    #[serde(default)]
    pub target: String,
    /// Détail libre, dépendant de l'action : code d'invitation, motif de
    /// bannissement, ancienne et nouvelle valeur.
    #[serde(default)]
    pub detail: String,
}

/// Nature d'un salon. Un salon textuel se lit et s'écrit ; un salon vocal
/// s'occupe, et n'a pas d'historique.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    #[default]
    Text,
    Voice,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelInfo {
    pub id: ChannelId,
    pub name: String,
    /// Par défaut textuel : un serveur d'une version antérieure n'envoie
    /// pas ce champ, et ses salons se comportaient comme du texte.
    #[serde(default)]
    pub kind: ChannelKind,
    /// Ordre d'affichage dans la barre latérale.
    #[serde(default)]
    pub position: u32,
    /// Salon vocal protégé par un mot de passe éphémère. Le mot de passe
    /// lui-même ne quitte jamais le serveur : ce drapeau suffit au client
    /// pour savoir qu'il doit le demander.
    #[serde(default)]
    pub locked: bool,
    /// `None` = visible par tout le monde. Sinon, réservé à ces rôles.
    /// N'est renseigné que pour qui peut gérer les salons — les autres n'ont
    /// pas à connaître la composition des restrictions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_roles: Option<Vec<RoleId>>,
    /// Salon **temporaire** — celui d'une porte web : effacé à cette date
    /// (ms Unix) au plus tard, ou avant si la porte ferme. `None` = salon
    /// ordinaire, et c'est ce que lit un client antérieur, qui voit alors
    /// un salon textuel comme les autres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expire_le: Option<u64>,
}

/// Un rôle : une couleur de pseudo, un rang, un jeu de permissions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleInfo {
    pub id: RoleId,
    pub name: String,
    /// Couleur du pseudo, 0xRRGGBB. `None` = couleur par défaut du thème.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
    /// Autorité. On n'agit que sur strictement plus bas que soi, et l'on
    /// n'attribue qu'un rôle de rang strictement inférieur au sien.
    #[serde(default)]
    pub rank: u16,
    #[serde(default)]
    pub perms: Perms,
    /// Rôle du serveur : ni supprimable, ni renommable.
    #[serde(default)]
    pub system: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Member {
    pub user_id: UserId,
    pub username: String,
    pub speaking: bool,
    /// Micro coupé volontairement — l'icône « muet » chez les autres, pour
    /// distinguer qui s'est tu de qui est parti. Absent d'un serveur
    /// antérieur : faux.
    #[serde(default)]
    pub muted: bool,
    /// Identifiant du stream que ce membre diffuse, s'il partage son écran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streaming: Option<u32>,
    /// Micro coupé **par un modérateur**. Distinct de `muted`, et il faut que
    /// ça se voie : l'un se défait d'un clic par l'intéressé, l'autre non.
    #[serde(default)]
    pub force_muted: bool,
    /// Rendu sourd par un modérateur.
    #[serde(default)]
    pub force_deafened: bool,
    #[serde(default)]
    pub admin: bool,
    /// Empreinte de la photo de profil, ou `None` s'il n'y en a pas. La
    /// vignette elle-même se demande à part (`RequestAvatars`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// Salon vocal occupé, ou `None` si la personne est connectée au serveur
    /// sans être en vocal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<ChannelId>,
    /// Où en est ce membre dans VALORANT, s'il partage son activité —
    /// voir [`JeuStatut`]. Absent d'un serveur antérieur, ou s'il ne
    /// partage pas, ou s'il ne joue pas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jeu: Option<JeuStatut>,
    /// Son Riot ID (« Pseudo#TAG ») s'il a lié son compte, et son rang
    /// compétitif d'après la dernière fiche (0 = non classé, 27 = Radiant).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub riot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rang_valorant: Option<u8>,
    #[serde(default)]
    pub roles: Vec<RoleId>,
    /// Vrai si la personne est connectée au serveur. Le roster liste AUSSI
    /// les comptes hors ligne (non bannis) : c'est ce champ qui les sépare.
    /// Défaut `true` : un vieux serveur n'envoie que des connectés.
    #[serde(default = "default_true")]
    pub online: bool,
    /// Couleur du pseudo, résolue par le serveur depuis le rôle le mieux
    /// classé qui en porte une. `None` = le client retombe sur son hachage
    /// de pseudo habituel, comme avant les rôles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
    /// Rang le plus élevé. Le client s'en sert pour masquer les actions de
    /// modération qui seraient refusées de toute façon.
    #[serde(default)]
    pub rank: u16,
    /// Un invité web : pas un compte, pas de rôle, pas de photo, présent le
    /// temps d'une porte. Un client antérieur ignore le champ et le liste
    /// en ligne comme un membre — son nom finit par « (web) », ça suffit.
    /// Redondant avec [`est_invite`] sur `user_id`, à dessein : le drapeau
    /// se lit sans connaître la plage.
    #[serde(default, skip_serializing_if = "is_false")]
    pub invite: bool,
}

/// Un résultat de recherche : le message, et le salon d'où il vient.
///
/// Le salon est indispensable : une recherche traverse plusieurs salons, et
/// un message sans son salon ne se retrouve plus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub channel: ChannelId,
    pub record: ChatRecord,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatRecord {
    pub user_id: UserId,
    pub username: String,
    pub text: String,
    pub ts: u64,
    /// Réponse à un autre message : de qui, et un extrait, pour l'afficher
    /// sans avoir à retrouver l'original.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<ReplyRef>,
    /// Les réactions, par emoji. Absentes d'un journal antérieur.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<Reaction>,
    /// Modifié par son auteur après coup. Absent d'un journal antérieur.
    #[serde(default, skip_serializing_if = "is_false")]
    pub edited: bool,
}

/// Ce qu'un membre n'a pas encore lu dans un salon, tel que le serveur le
/// compte à la connexion.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NonLuSalon {
    pub channel: ChannelId,
    /// Horodatage du dernier message **lu** (0 : jamais rien lu). Tout
    /// message d'horodatage supérieur est non lu — c'est là que le client
    /// pose son « nouveaux messages ».
    #[serde(default)]
    pub dernier_ts: u64,
    /// Messages non lus, comptés sur ce que le serveur garde en mémoire
    /// (mille par salon) : au-delà, le compte s'arrête là.
    #[serde(default)]
    pub non_lus: u32,
    /// L'un d'eux nomme le destinataire (`@pseudo`). Approximation du
    /// serveur, qui ne partage pas le découpeur du client : frontière de
    /// mot, casse ASCII ignorée, blocs et portions de code exclus.
    #[serde(default)]
    pub mention: bool,
}

/// La clé d'un message : son auteur et son horodatage. Le serveur rend
/// l'horodatage unique par salon, ce qui rend la paire unique.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MsgRef {
    pub user_id: UserId,
    pub ts: u64,
}

/// Le message auquel on répond, tel qu'on le rappelle sous la réponse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyRef {
    pub user_id: UserId,
    pub ts: u64,
    pub username: String,
    /// Le début du message d'origine, borné à [`MAX_EXCERPT`] caractères.
    pub excerpt: String,
}

/// Une réaction : un emoji et ceux qui l'ont posé.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reaction {
    pub emoji: String,
    pub users: Vec<UserId>,
}

/// Longueur de l'extrait rappelé sous une réponse.
pub const MAX_EXCERPT: usize = 120;

/// Les réactions proposées d'un clic. Un client peut en envoyer d'autres
/// (n'importe quel emoji), le serveur ne vérifie que la forme.
pub const REACTIONS: &[&str] = &["👍", "👎", "❤️", "😂", "😮", "😢", "🔥", "🎉", "👀", "✅"];

/// Emojis de réaction admis au plus par message : au-delà, ce n'est plus une
/// réaction, c'est du bruit.
pub const MAX_REACTIONS: usize = 20;

/// Un emoji de réaction acceptable : court, sans caractère de contrôle ni
/// blanc, un seul « caractère » à l'écran (les emojis composés comptent
/// plusieurs points de code : ❤️ en fait deux).
pub fn clean_emoji(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() || s.len() > 16 || s.chars().count() > 4 {
        return None;
    }
    if s.chars().any(|c| c.is_control() || c.is_whitespace() || c.is_ascii()) {
        return None;
    }
    Some(s.to_string())
}

/// Le début d'un texte, pour le rappeler sous une réponse : une seule
/// ligne, coupée proprement à [`MAX_EXCERPT`] caractères.
pub fn excerpt_of(text: &str) -> String {
    let premiere = text.lines().next().unwrap_or("").trim();
    let mut out: String = premiere.chars().take(MAX_EXCERPT).collect();
    if premiere.chars().count() > MAX_EXCERPT || text.lines().count() > 1 {
        out.push('…');
    }
    out
}

/// Où en est un joueur dans VALORANT : l'état de sa session, sa file, sa
/// carte, le score de son équipe, sa party. C'est ce que son propre client
/// Riot raconte à ses amis ; ki-chat le relaie aux membres du serveur, avec
/// son accord — rien sur les adversaires, jamais.
#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JeuStatut {
    pub etat: JeuEtat,
    /// La file : « competitive », « unrated », « swiftplay », « deathmatch »…
    /// Vide en partie personnalisée.
    #[serde(default)]
    pub file: String,
    /// La carte, en nom d'affichage (« Ascent »). Vide hors partie.
    #[serde(default)]
    pub carte: String,
    #[serde(default)]
    pub score_allie: u8,
    #[serde(default)]
    pub score_adverse: u8,
    #[serde(default)]
    pub party_taille: u8,
    #[serde(default)]
    pub party_max: u8,
    /// Party ouverte : n'importe quel ami peut la rejoindre.
    #[serde(default)]
    pub party_ouverte: bool,
    /// Rang compétitif tel que le client l'annonce : 0 = non classé, puis
    /// Fer 1 (3) … Radiant (27). Icône côté client, valorant-api.com.
    #[serde(default)]
    pub rang: u8,
    #[serde(default)]
    pub niveau: u32,
    /// Partie personnalisée (pas de file).
    #[serde(default)]
    pub custom: bool,
    /// Un autre jeu que VALORANT — « Rocket League », reconnu à sa
    /// fenêtre : « joue à … », et rien d'autre, on ne lit rien dedans.
    /// Vide = VALORANT, dont les champs ci-dessus racontent la partie ;
    /// c'est ce qu'un client d'avant envoie, et ce qu'il lit.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub nom: String,
}

/// La fiche VALORANT d'un membre, telle que le serveur la garde d'après
/// HenrikDev : rang courant et pic, derniers mouvements de RR, derniers
/// matchs résumés — la ligne du membre seulement, jamais les neuf autres.
///
/// Depuis 0.1.40 la fiche **s'accumule** : le serveur fusionne l'ancienne
/// et la neuve à chaque rafraîchissement (soixante matchs, cent points de
/// RR), et les agrégats — bilan, forme, série, agents, cartes, duos — se
/// calculent ici, une seule fois pour le serveur et le client. Une fiche
/// d'avant se relit telle quelle : tout ce qui est nouveau a son défaut.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FicheValorant {
    pub riot_id: String,
    pub region: String,
    pub plateforme: String,
    #[serde(default)]
    pub niveau: u32,
    #[serde(default)]
    pub rang: RangValorant,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pic: Option<RangValorant>,
    #[serde(default)]
    pub historique_rr: Vec<PointRR>,
    #[serde(default)]
    pub matchs: Vec<MatchResume>,
    /// Dernière mise à jour, en millisecondes Unix.
    #[serde(default)]
    pub maj: u64,
    /// Les actes joués d'après v3/mmr, du plus ancien au plus récent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub saisons: Vec<StatsSaison>,
}

/// Un acte tel que v3/mmr le résume (`seasonal[]`) : combien de parties,
/// combien gagnées, et où il a fini.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StatsSaison {
    /// « e9a2 » (season.short).
    #[serde(default)]
    pub saison: String,
    /// wins
    #[serde(default)]
    pub victoires: u16,
    /// games
    #[serde(default)]
    pub parties: u16,
    /// end_tier.id
    #[serde(default)]
    pub tier_fin: u8,
    /// end_rr
    #[serde(default)]
    pub rr_fin: u16,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RangValorant {
    /// 0 = non classé … 27 = Radiant.
    pub tier: u8,
    pub rr: u16,
    /// Variation au dernier match classé.
    #[serde(default)]
    pub delta: i32,
    #[serde(default)]
    pub elo: u32,
    /// La saison (« E9A2 »), pour le pic.
    #[serde(default)]
    pub saison: String,
    /// Parties de placement encore à jouer (games_needed_for_rating).
    /// Renseigné pour `rang`, laissé à 0 pour `pic`.
    #[serde(default)]
    pub placements_restants: u8,
    /// Boucliers contre la descente (rank_protection_shields).
    #[serde(default)]
    pub boucliers: u8,
    /// Place au classement régional (Immortel et plus) ; 0 = pas classé.
    #[serde(default)]
    pub classement: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PointRR {
    /// Le match qui a produit ce mouvement — pour le retrouver dans les
    /// derniers matchs et l'annoncer avec ses RR.
    #[serde(default)]
    pub match_id: String,
    pub date: u64,
    pub tier: u8,
    pub rr: u16,
    pub delta: i32,
    #[serde(default)]
    pub carte: String,
    /// L'acte du point (« e9a2 »), pour tracer une frontière sur la courbe.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub saison: String,
    /// Descente évitée grâce à un bouclier (was_derank_protected).
    #[serde(default)]
    pub protege: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MatchResume {
    pub id: String,
    pub date: u64,
    pub carte: String,
    pub mode: String,
    pub agent: String,
    pub kills: u16,
    pub deaths: u16,
    pub assists: u16,
    pub score: u32,
    /// Pourcentage de tirs à la tête.
    #[serde(default)]
    pub tete_pct: u8,
    /// Manches gagnées / perdues par son équipe.
    pub manches: (u8, u8),
    /// `None` : match nul ou inconnu.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gagne: Option<bool>,
    #[serde(default)]
    pub tier: u8,
    #[serde(default)]
    pub duree_s: u32,
    /// L'acte (« e9a2 »), pour couper les séries et les courbes.
    /// metadata.season.short — vide si absent.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub saison: String,
    /// Dégâts infligés et reçus sur tout le match (stats.damage.dealt /
    /// received). 0 pour un match résumé avant 0.1.40.
    #[serde(default)]
    pub degats: u32,
    #[serde(default)]
    pub degats_recus: u32,
    /// Tirs à la tête et tirs au total (tête + corps + jambes), pour un
    /// pourcentage agrégé exact ; `tete_pct` reste pour les anciens clients.
    #[serde(default)]
    pub tetes: u16,
    #[serde(default)]
    pub tirs: u16,
    /// Combien de joueurs dans sa party, lui compris (1 à 5 ; 0 = inconnu).
    /// Un effectif, jamais une identité.
    #[serde(default)]
    pub party: u8,
    /// Les autres membres du groupe dans son camp, et en face — des
    /// `UserId`, jamais un puuid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub avec: Vec<UserId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contre: Vec<UserId>,
    /// Ce que les manches racontent — `None` quand on ne les a pas eues
    /// (match d'avant 0.1.40, combat à mort, JSON sans `rounds`/`kills`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manches_detail: Option<DetailManches>,
}

/// La ligne du membre manche par manche : rien des neuf autres.
/// Tous les compteurs sont des `u8` : un match compte au plus 30 manches.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DetailManches {
    /// Manches jouées d'après `rounds[]` (recoupe `manches.0 + manches.1`).
    #[serde(default)]
    pub manches: u8,
    /// Manches où il a tué, assisté, survécu, ou été échangé (KAST).
    #[serde(default)]
    pub kast: u8,
    #[serde(default)]
    pub premiers_sangs: u8,
    #[serde(default)]
    pub premieres_morts: u8,
    /// Manches à 3, 4, 5 kills ou plus.
    #[serde(default)]
    pub triples: u8,
    #[serde(default)]
    pub quadruples: u8,
    #[serde(default)]
    pub aces: u8,
    /// Situations 1 contre X (X ≥ 1) tentées, gagnées, et le plus gros X
    /// gagné.
    #[serde(default)]
    pub clutchs_tentes: u8,
    #[serde(default)]
    pub clutchs: u8,
    #[serde(default)]
    pub meilleur_clutch: u8,
    #[serde(default)]
    pub poses: u8,
    #[serde(default)]
    pub desamorcages: u8,
    /// Une lettre par manche dans l'ordre : `V` gagnée, `D` perdue.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub deroule: String,
}

/// Des sommes sur un ensemble de matchs : elles s'additionnent, se
/// filtrent et se relisent sans perte ; les taux se font au dernier
/// moment, par les méthodes — qui rendent `None` plutôt que de diviser
/// par zéro, et le client affiche « — ».
///
/// Un bilan se construit par [`Bilan::ajouter`], une ligne à la fois ;
/// [`FicheValorant::bilan`] le fait sur une fenêtre et un mode. Il ne
/// juge pas ce qu'on lui donne : c'est à l'appelant d'écarter les matchs
/// sans manches ([`FicheValorant::a_des_manches`]) s'il ne veut pas qu'un
/// combat à mort pèse sur les moyennes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Bilan {
    #[serde(default)]
    pub matchs: u16,
    #[serde(default)]
    pub victoires: u16,
    /// Les nuls sont ce qui reste : `matchs − victoires − defaites`.
    #[serde(default)]
    pub defaites: u16,
    /// Σ (manches.0 + manches.1).
    #[serde(default)]
    pub manches: u16,
    #[serde(default)]
    pub kills: u32,
    #[serde(default)]
    pub deaths: u32,
    #[serde(default)]
    pub assists: u32,
    #[serde(default)]
    pub score: u32,
    #[serde(default)]
    pub tetes: u32,
    #[serde(default)]
    pub tirs: u32,
    /// Matchs et manches des seuls matchs qui ont `degats > 0` (ADR).
    #[serde(default)]
    pub matchs_degats: u16,
    #[serde(default)]
    pub manches_degats: u16,
    #[serde(default)]
    pub degats: u32,
    /// Matchs et manches des seuls matchs qui ont `manches_detail`
    /// (KAST, premiers sangs, clutchs…).
    #[serde(default)]
    pub matchs_detailles: u16,
    #[serde(default)]
    pub manches_detaillees: u16,
    #[serde(default)]
    pub kast: u16,
    #[serde(default)]
    pub premiers_sangs: u16,
    #[serde(default)]
    pub premieres_morts: u16,
    #[serde(default)]
    pub triples: u16,
    #[serde(default)]
    pub quadruples: u16,
    #[serde(default)]
    pub aces: u16,
    #[serde(default)]
    pub clutchs: u16,
    #[serde(default)]
    pub clutchs_tentes: u16,
    #[serde(default)]
    pub meilleur_clutch: u8,
    /// Σ delta des points de RR de la fenêtre.
    #[serde(default)]
    pub rr: i32,
    #[serde(default)]
    pub duree_s: u32,
}

impl Bilan {
    /// Une ligne de plus. Tout se somme en saturant : les nombres viennent
    /// du réseau et un compteur qui déborde ne doit jamais faire tomber
    /// qui l'additionne.
    pub fn ajouter(&mut self, m: &MatchResume) {
        self.matchs = self.matchs.saturating_add(1);
        match m.gagne {
            Some(true) => self.victoires = self.victoires.saturating_add(1),
            Some(false) => self.defaites = self.defaites.saturating_add(1),
            None => {}
        }
        let manches = u16::from(m.manches.0) + u16::from(m.manches.1);
        self.manches = self.manches.saturating_add(manches);
        self.kills = self.kills.saturating_add(u32::from(m.kills));
        self.deaths = self.deaths.saturating_add(u32::from(m.deaths));
        self.assists = self.assists.saturating_add(u32::from(m.assists));
        self.score = self.score.saturating_add(m.score);
        self.tetes = self.tetes.saturating_add(u32::from(m.tetes));
        self.tirs = self.tirs.saturating_add(u32::from(m.tirs));
        if m.degats > 0 {
            self.matchs_degats = self.matchs_degats.saturating_add(1);
            self.manches_degats = self.manches_degats.saturating_add(manches);
            self.degats = self.degats.saturating_add(m.degats);
        }
        if let Some(d) = &m.manches_detail {
            self.matchs_detailles = self.matchs_detailles.saturating_add(1);
            self.manches_detaillees = self.manches_detaillees.saturating_add(u16::from(d.manches));
            self.kast = self.kast.saturating_add(u16::from(d.kast));
            self.premiers_sangs = self.premiers_sangs.saturating_add(u16::from(d.premiers_sangs));
            self.premieres_morts = self.premieres_morts.saturating_add(u16::from(d.premieres_morts));
            self.triples = self.triples.saturating_add(u16::from(d.triples));
            self.quadruples = self.quadruples.saturating_add(u16::from(d.quadruples));
            self.aces = self.aces.saturating_add(u16::from(d.aces));
            self.clutchs = self.clutchs.saturating_add(u16::from(d.clutchs));
            self.clutchs_tentes = self.clutchs_tentes.saturating_add(u16::from(d.clutchs_tentes));
            self.meilleur_clutch = self.meilleur_clutch.max(d.meilleur_clutch);
        }
        self.duree_s = self.duree_s.saturating_add(m.duree_s);
    }

    /// Un taux `numerateur / denominateur`, ou rien si le dénominateur
    /// est nul : c'est la seule division de ce bloc.
    fn taux(numerateur: f32, denominateur: u32) -> Option<f32> {
        if denominateur == 0 {
            None
        } else {
            Some(numerateur / denominateur as f32)
        }
    }

    /// kills / max(deaths, 1) — `None` sans match.
    pub fn kd(&self) -> Option<f32> {
        if self.matchs == 0 {
            None
        } else {
            Some(self.kills as f32 / self.deaths.max(1) as f32)
        }
    }

    /// (kills + assists) / max(deaths, 1) — `None` sans match.
    pub fn kda(&self) -> Option<f32> {
        if self.matchs == 0 {
            None
        } else {
            Some((self.kills as f32 + self.assists as f32) / self.deaths.max(1) as f32)
        }
    }

    /// Score moyen par manche — `None` sans manche.
    pub fn acs(&self) -> Option<f32> {
        Self::taux(self.score as f32, u32::from(self.manches))
    }

    /// Dégâts moyens par manche, sur les seuls matchs qui les ont.
    pub fn adr(&self) -> Option<f32> {
        Self::taux(self.degats as f32, u32::from(self.manches_degats))
    }

    /// Part des manches avec kill, assist, survie ou échange, en pour
    /// cent, sur les seuls matchs détaillés.
    pub fn kast_pct(&self) -> Option<f32> {
        Self::taux(f32::from(self.kast) * 100.0, u32::from(self.manches_detaillees))
    }

    /// Part des tirs à la tête, en pour cent — `None` sans tir.
    pub fn tete_pct(&self) -> Option<f32> {
        Self::taux(self.tetes as f32 * 100.0, self.tirs)
    }

    /// Victoires sur victoires + défaites, en pour cent ; les nuls ne
    /// comptent pas. `None` sans match décidé.
    pub fn victoires_pct(&self) -> Option<f32> {
        let decides = u32::from(self.victoires) + u32::from(self.defaites);
        Self::taux(f32::from(self.victoires) * 100.0, decides)
    }

    /// Premiers sangs par match détaillé.
    pub fn fk_par_match(&self) -> Option<f32> {
        Self::taux(f32::from(self.premiers_sangs), u32::from(self.matchs_detailles))
    }

    /// Assez de matchs pour qu'un taux veuille dire quelque chose.
    pub fn assez(&self, n: u16) -> bool {
        self.matchs >= n
    }
}

/// Où le MMR caché se situe par rapport au rang affiché — deviné, jamais
/// lu : Riot ne l'expose pas (voir [`FicheValorant::mmr_estime`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PositionMmr {
    /// Il gagne nettement plus de RR qu'il n'en perd : le jeu le pousse
    /// à monter.
    AuDessus,
    /// Gains et pertes se valent à [`SEUIL_MMR`] près.
    #[default]
    AuNiveau,
    /// Il perd nettement plus qu'il ne gagne : le jeu le retient.
    EnDessous,
}

/// L'écart, en RR par match, entre le gain moyen et la perte moyenne à
/// partir duquel on dit le MMR « au-dessus » (ou « en dessous ») du rang
/// plutôt qu'« au niveau ». Cinq RR : un match qui rapporte +22 et coûte
/// −13 est clairement poussé ; +18 / −17 est à l'équilibre.
pub const SEUIL_MMR: f32 = 5.0;
/// Combien de points de RR récents entrent dans l'estimation : assez pour
/// lisser un match, pas assez pour traîner un vieux MMR.
pub const MMR_POINTS: usize = 20;
/// Victoires et défaites qu'il faut au minimum, chacune, pour qu'une
/// moyenne veuille dire quelque chose.
pub const MMR_MIN_PAR_CAMP: u16 = 3;

/// Le MMR caché tel qu'on le devine aux variations de RR du membre —
/// comme le font les trackers : aucune requête, aucune donnée d'autrui.
/// Des `f32`, mais jamais dans un message : ce n'est pas transmis, c'est
/// recalculé par qui en a besoin (et les moyennes sont finies par
/// construction).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EstimationMmr {
    #[serde(default)]
    pub position: PositionMmr,
    /// Moyenne des `delta > 0`.
    #[serde(default)]
    pub gain_moyen: f32,
    /// Moyenne des `|delta|` pour `delta < 0` — en valeur absolue.
    #[serde(default)]
    pub perte_moyenne: f32,
    #[serde(default)]
    pub victoires: u16,
    #[serde(default)]
    pub defaites: u16,
    /// Les classés retenus : `victoires + defaites`.
    #[serde(default)]
    pub points: u16,
}

/// Le nom français du mode classé, tel que le serveur le traduit.
const MODE_CLASSE: &str = "Compétitif";
/// Les modes sans manches : un combat à mort n'a ni camp ni score par
/// manche, il n'entre dans aucune moyenne.
const MODES_SANS_MANCHES: [&str; 2] = ["Combat à mort", "Combat à mort par équipe"];

impl FicheValorant {
    /// Les matchs à manches : tout sauf « Combat à mort » et « Combat à
    /// mort par équipe » — et tout match dont `manches == (0, 0)`, qui
    /// n'a rien à dire sur une manche non plus.
    pub fn a_des_manches(m: &MatchResume) -> bool {
        !MODES_SANS_MANCHES.contains(&m.mode.as_str()) && m.manches != (0, 0)
    }

    /// Un match classé : le mode « Compétitif », rien d'autre.
    fn est_classe(m: &MatchResume) -> bool {
        m.mode == MODE_CLASSE
    }

    /// Les matchs de la fenêtre `[depuis, jusqu_a[` (ms Unix), dans
    /// l'ordre de la fiche ; `classe` = « Compétitif » seul. Les combats
    /// à mort y sont : c'est aux agrégats de les écarter.
    pub fn matchs_dans(
        &self,
        depuis: u64,
        jusqu_a: u64,
        classe: bool,
    ) -> impl Iterator<Item = &MatchResume> {
        self.matchs
            .iter()
            .filter(move |m| m.date >= depuis && m.date < jusqu_a && (!classe || Self::est_classe(m)))
    }

    /// Les matchs à manches de la fenêtre, ceux qui pèsent sur un bilan.
    fn matchs_comptes(
        &self,
        depuis: u64,
        jusqu_a: u64,
        classe: bool,
    ) -> impl Iterator<Item = &MatchResume> {
        self.matchs_dans(depuis, jusqu_a, classe).filter(|m| Self::a_des_manches(m))
    }

    /// Le bilan de la fenêtre : les matchs à manches du mode demandé, et
    /// `rr` = Σ delta des points de RR de la fenêtre.
    pub fn bilan(&self, depuis: u64, jusqu_a: u64, classe: bool) -> Bilan {
        let mut b = Bilan::default();
        for m in self.matchs_comptes(depuis, jusqu_a, classe) {
            b.ajouter(m);
        }
        b.rr = self
            .historique_rr
            .iter()
            .filter(|p| p.date >= depuis && p.date < jusqu_a)
            .fold(0i32, |acc, p| acc.saturating_add(p.delta));
        b
    }

    /// Les matchs classés du plus récent au plus ancien, quel que soit
    /// l'ordre de la fiche.
    fn classes_recents(&self) -> Vec<&MatchResume> {
        let mut v: Vec<&MatchResume> =
            self.matchs.iter().filter(|m| Self::est_classe(m) && Self::a_des_manches(m)).collect();
        v.sort_by_key(|m| std::cmp::Reverse(m.date));
        v
    }

    /// Les `n` derniers classés du plus récent au plus ancien : 1 victoire,
    /// -1 défaite, 0 nul.
    pub fn forme(&self, n: usize) -> Vec<i8> {
        self.classes_recents()
            .into_iter()
            .take(n)
            .map(|m| match m.gagne {
                Some(true) => 1,
                Some(false) => -1,
                None => 0,
            })
            .collect()
    }

    /// +3 = trois victoires d'affilée, -2 = deux défaites ; les nuls sont
    /// ignorés ; 0 sans classé.
    pub fn serie(&self) -> i8 {
        let mut serie: i8 = 0;
        for r in self.forme(usize::MAX) {
            if r == 0 {
                continue;
            }
            if serie == 0 || (serie > 0) == (r > 0) {
                serie = serie.saturating_add(r);
            } else {
                break;
            }
        }
        serie
    }

    /// Le MMR caché, deviné : Riot ne le montre pas, mais il se lit dans
    /// les RR — gagner plus qu'on ne perd, c'est un MMR au-dessus du
    /// rang, le jeu pousse à monter ; l'inverse, en dessous. Sur les
    /// [`MMR_POINTS`] classés les plus récents de l'acte en cours (celui
    /// du point le plus récent ; tout si l'acte est inconnu), sans les
    /// `delta == 0` ni les descentes protégées par un bouclier, qui
    /// faussent la perte. `None` sans rang, pendant les placements (leurs
    /// deltas sont énormes), ou sans [`MMR_MIN_PAR_CAMP`] victoires et
    /// autant de défaites — les effectifs garantissent qu'on ne divise
    /// jamais par zéro.
    pub fn mmr_estime(&self) -> Option<EstimationMmr> {
        if self.rang.tier < 3 || self.rang.placements_restants > 0 {
            return None;
        }
        let mut points: Vec<&PointRR> = self.historique_rr.iter().collect();
        points.sort_by_key(|p| std::cmp::Reverse(p.date));
        let acte = points.first().map(|p| p.saison.as_str()).unwrap_or_default();
        let (mut gains, mut pertes) = (0f32, 0f32);
        let (mut victoires, mut defaites) = (0u16, 0u16);
        let retenus = points
            .iter()
            .filter(|p| (acte.is_empty() || p.saison == acte) && p.delta != 0 && !p.protege)
            .take(MMR_POINTS);
        for p in retenus {
            if p.delta > 0 {
                gains += p.delta as f32;
                victoires = victoires.saturating_add(1);
            } else {
                pertes += p.delta.unsigned_abs() as f32;
                defaites = defaites.saturating_add(1);
            }
        }
        if victoires < MMR_MIN_PAR_CAMP || defaites < MMR_MIN_PAR_CAMP {
            return None;
        }
        let gain_moyen = gains / f32::from(victoires);
        let perte_moyenne = pertes / f32::from(defaites);
        let diff = gain_moyen - perte_moyenne;
        let position = if diff >= SEUIL_MMR {
            PositionMmr::AuDessus
        } else if diff <= -SEUIL_MMR {
            PositionMmr::EnDessous
        } else {
            PositionMmr::AuNiveau
        };
        Some(EstimationMmr {
            position,
            gain_moyen,
            perte_moyenne,
            victoires,
            defaites,
            points: victoires.saturating_add(defaites),
        })
    }

    /// (nom, bilan) par la clé donnée, sur la fenêtre, trié par matchs
    /// décroissants — et par nom à égalité, pour que deux appels rendent
    /// le même ordre.
    fn ventiler(
        &self,
        depuis: u64,
        jusqu_a: u64,
        classe: bool,
        cle: fn(&MatchResume) -> &str,
    ) -> Vec<(String, Bilan)> {
        let mut par: BTreeMap<&str, Bilan> = BTreeMap::new();
        for m in self.matchs_comptes(depuis, jusqu_a, classe) {
            par.entry(cle(m)).or_default().ajouter(m);
        }
        let mut v: Vec<(String, Bilan)> = par.into_iter().map(|(k, b)| (k.to_string(), b)).collect();
        v.sort_by_key(|(_, b)| std::cmp::Reverse(b.matchs));
        v
    }

    /// (agent, bilan) sur la fenêtre, trié par matchs décroissants.
    pub fn par_agent(&self, depuis: u64, jusqu_a: u64, classe: bool) -> Vec<(String, Bilan)> {
        self.ventiler(depuis, jusqu_a, classe, |m| m.agent.as_str())
    }

    /// (carte, bilan) sur la fenêtre, trié par matchs décroissants.
    pub fn par_carte(&self, depuis: u64, jusqu_a: u64, classe: bool) -> Vec<(String, Bilan)> {
        self.ventiler(depuis, jusqu_a, classe, |m| m.carte.as_str())
    }

    /// (membre, parties ensemble, victoires ensemble) d'après `avec`, sur
    /// la fenêtre et tous les modes à manches, trié par parties puis
    /// victoires décroissantes.
    pub fn duos(&self, depuis: u64, jusqu_a: u64) -> Vec<(UserId, u16, u16)> {
        let mut par: BTreeMap<UserId, (u16, u16)> = BTreeMap::new();
        for m in self.matchs_comptes(depuis, jusqu_a, false) {
            let gagne = u16::from(m.gagne == Some(true));
            // Un même membre deux fois dans `avec` — ça vient du réseau —
            // n'est qu'une partie ensemble.
            let mut vus = std::collections::BTreeSet::new();
            for id in m.avec.iter().filter(|id| vus.insert(**id)) {
                let e = par.entry(*id).or_default();
                e.0 = e.0.saturating_add(1);
                e.1 = e.1.saturating_add(gagne);
            }
        }
        let mut v: Vec<(UserId, u16, u16)> = par.into_iter().map(|(id, (p, g))| (id, p, g)).collect();
        v.sort_by_key(|&(_, p, g)| std::cmp::Reverse((p, g)));
        v
    }

    /// La fiche allégée pour la page du groupe : les `n_matchs` matchs les
    /// plus récents sans `manches_detail`, les `n_points` points les plus
    /// récents, `saisons` vidées — le reste tel quel. Les `id` restent :
    /// ils servent au regroupement « ensemble » et au lien ΔRR.
    pub fn resume(&self, n_matchs: usize, n_points: usize) -> FicheValorant {
        let mut matchs: Vec<&MatchResume> = self.matchs.iter().collect();
        matchs.sort_by_key(|m| std::cmp::Reverse(m.date));
        let matchs = matchs
            .into_iter()
            .take(n_matchs)
            .map(|m| MatchResume { manches_detail: None, ..m.clone() })
            .collect();
        let mut points: Vec<&PointRR> = self.historique_rr.iter().collect();
        points.sort_by_key(|p| std::cmp::Reverse(p.date));
        let historique_rr = points.into_iter().take(n_points).cloned().collect();
        FicheValorant {
            riot_id: self.riot_id.clone(),
            region: self.region.clone(),
            plateforme: self.plateforme.clone(),
            niveau: self.niveau,
            rang: self.rang.clone(),
            pic: self.pic.clone(),
            historique_rr,
            matchs,
            maj: self.maj,
            saisons: Vec::new(),
        }
    }
}

/// La fiche d'un membre avec son identité, pour la page de stats du
/// groupe.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FicheMembre {
    pub user_id: UserId,
    pub username: String,
    pub fiche: FicheValorant,
    /// Le bilan que le serveur calcule à l'envoi ; `None` d'un serveur
    /// d'avant, et le client recalcule sur ce qu'il a.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bilan: Option<BilanMembre>,
}

/// Ce que la page du groupe reçoit d'un membre sans porter ses soixante
/// matchs : calculé à l'envoi, jamais stocké. Classé seulement.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BilanMembre {
    #[serde(default)]
    pub sept_jours: Bilan,
    #[serde(default)]
    pub trente_jours: Bilan,
    #[serde(default)]
    pub serie: i8,
    /// Les dix derniers classés, du plus récent : 1, -1, 0.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forme: Vec<i8>,
    /// (agent, parties, victoires) — les trois plus joués sur 30 j.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<(String, u16, u16)>,
    /// (carte, parties, victoires) — cinq au plus sur 30 j.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cartes: Vec<(String, u16, u16)>,
    /// (membre, parties ensemble, victoires ensemble) sur 30 j, cinq au
    /// plus.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub duos: Vec<(UserId, u16, u16)>,
}

/// Un match d'esport à venir ou en cours, d'après HenrikDev — pour la
/// page Stats.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MatchEsport {
    pub date: u64,
    pub ligue: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub tournoi: String,
    /// Les deux équipes, par leur code (« FNC », « TH »).
    pub equipes: Vec<String>,
    /// « unstarted » ou « inProgress ».
    #[serde(default)]
    pub etat: String,
    /// « BO3 », « BO5 » — vide si inconnu.
    #[serde(default)]
    pub format: String,
}

/// Le nom français d'un rang compétitif (0 = non classé, 3 = Fer 1 …
/// 27 = Radiant ; 1 et 2 n'existent pas).
pub fn nom_de_rang(tier: u8) -> String {
    let paliers = ["Fer", "Bronze", "Argent", "Or", "Platine", "Diamant", "Ascendant", "Immortel"];
    match tier {
        0..=2 => "Non classé".to_string(),
        27.. => "Radiant".to_string(),
        t => {
            let i = (t - 3) as usize;
            format!("{} {}", paliers[i / 3], i % 3 + 1)
        }
    }
}

/// Le Riot ID « Pseudo#TAG » découpé et vérifié : un pseudo de 3 à 16
/// caractères, un tag de 3 à 5 lettres ou chiffres.
pub fn parser_riot_id(s: &str) -> Option<(String, String)> {
    let (nom, tag) = s.trim().rsplit_once('#')?;
    let (nom, tag) = (nom.trim(), tag.trim());
    let nom_ok = (3..=16).contains(&nom.chars().count())
        && !nom.chars().any(|c| c.is_control() || c == '#' || c == '/');
    let tag_ok = (3..=5).contains(&tag.chars().count()) && tag.chars().all(|c| c.is_ascii_alphanumeric());
    (nom_ok && tag_ok).then(|| (nom.to_string(), tag.to_uppercase()))
}

/// L'état de session VALORANT, tel que la présence le nomme.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JeuEtat {
    /// Dans les menus (ou en file d'attente).
    #[default]
    Menus,
    /// Sélection des agents.
    PreGame,
    /// En partie.
    EnJeu,
}

/// Longueur admise pour la file et la carte : ce sont des identifiants
/// courts, tout ce qui dépasse est suspect.
pub const MAX_JEU_TEXTE: usize = 32;

impl JeuStatut {
    /// Le statut tel que le serveur le garde : textes bornés et assainis,
    /// nombres plafonnés — il vient d'un client, comme tout le reste.
    pub fn nettoyer(&self) -> Self {
        Self {
            etat: self.etat,
            file: safe_display(&self.file, MAX_JEU_TEXTE),
            carte: safe_display(&self.carte, MAX_JEU_TEXTE),
            score_allie: self.score_allie.min(99),
            score_adverse: self.score_adverse.min(99),
            party_taille: self.party_taille.min(10),
            party_max: self.party_max.min(10),
            party_ouverte: self.party_ouverte,
            rang: self.rang.min(27),
            niveau: self.niveau.min(9999),
            custom: self.custom,
            nom: safe_display(&self.nom, MAX_JEU_TEXTE),
        }
    }

    /// Un autre jeu que VALORANT, reconnu à sa fenêtre : « joue à … »,
    /// sans rien d'autre — on ne lit rien dans le jeu.
    pub fn autre_jeu(nom: &str) -> Self {
        Self { etat: JeuEtat::EnJeu, nom: nom.to_string(), ..Self::default() }
    }

    /// VALORANT, dont la présence raconte la partie ; sinon c'est un autre
    /// jeu, dont on ne sait que le nom.
    pub fn est_valorant(&self) -> bool {
        self.nom.is_empty()
    }

    /// Le nom français de la file. Les files console portent un préfixe
    /// (`console_competitive`) : même nom, avec la mention.
    pub fn libelle_file(&self) -> String {
        let (file, console) = match self.file.strip_prefix("console_") {
            Some(reste) => (reste, true),
            None => (self.file.as_str(), false),
        };
        let nom = match file {
            "competitive" => "compétitive",
            "unrated" => "non classée",
            "swiftplay" => "swiftplay",
            "spikerush" => "spike rush",
            "deathmatch" => "deathmatch",
            "ggteam" => "escalade",
            "hurm" => "team deathmatch",
            "premier" => "Premier",
            "newmap" => "nouvelle carte",
            "" if self.custom => "personnalisée",
            "" => "",
            autre => autre,
        };
        if console && !nom.is_empty() {
            format!("{nom} (console)")
        } else {
            nom.to_string()
        }
    }

    /// Une ligne pour la liste des membres : « compétitive · Ascent · 7-5 »,
    /// « sélection des agents », « au menu »…
    pub fn ligne(&self) -> String {
        if !self.nom.is_empty() {
            return format!("joue à {}", self.nom);
        }
        let file = self.libelle_file();
        let party = if self.party_taille > 1 {
            format!(" · party {}/{}", self.party_taille, self.party_max.max(self.party_taille))
        } else {
            String::new()
        };
        let file = file.as_str();
        // Une party ouverte et pas pleine, au menu — pas en file, où elle
        // est verrouillée : elle cherche du monde.
        let cherche = if matches!(self.etat, JeuEtat::Menus)
            && file.is_empty()
            && self.party_ouverte
            && self.party_taille < self.party_max
        {
            " · cherche des joueurs"
        } else {
            ""
        };
        match self.etat {
            JeuEtat::Menus if file.is_empty() => format!("Valorant · au menu{party}{cherche}"),
            JeuEtat::Menus => format!("Valorant · en file {file}{party}{cherche}"),
            JeuEtat::PreGame => {
                let ou = if self.carte.is_empty() { String::new() } else { format!(" · {}", self.carte) };
                format!("{file} · sélection des agents{ou}{party}")
            }
            JeuEtat::EnJeu => {
                let ou = if self.carte.is_empty() { String::new() } else { format!(" · {}", self.carte) };
                let score = if self.custom && self.score_allie == 0 && self.score_adverse == 0 {
                    String::new()
                } else {
                    format!(" · {}-{}", self.score_allie, self.score_adverse)
                };
                format!("{file}{ou}{score}{party}")
            }
        }
    }
}

// --- Le tableau de bord de l'administration (GET /admin/tableau) ---

/// L'état du serveur en un écran, pour l'onglet « Tableau de bord » de
/// l'administration. Tout est facultatif à la lecture : un serveur d'avant
/// ne dit pas tout, et un champ de plus ne casse pas un client d'avant.
/// Rien ici ne contient de message ni de voix.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TableauAdmin {
    pub version: String,
    /// Secondes depuis le démarrage du serveur.
    pub depuis_s: u64,
    /// Les comptes non bannis, connectés ou non.
    pub comptes: u32,
    pub en_ligne: Vec<TableauMembre>,
    pub salons_texte: u32,
    /// Les salons vocaux et qui s'y trouve.
    pub vocal: Vec<TableauSalonVocal>,
    pub diffusions: Vec<TableauDiffusion>,
    pub fichiers: TableauStock,
    pub clips: TableauStock,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disque_libre_octets: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memoire_octets: Option<u64>,
    pub musique: TableauMusique,
    /// Les compteurs du service VALORANT, en une ligne.
    pub valorant: String,
    /// Une ligne par version de ki-chat dans les archives de diagnostic.
    pub diagnostics: Vec<TableauDiag>,
    /// La fabrique des vidéos (conversions et exports de clips) : depuis
    /// 0.1.43, absent d'un serveur d'avant.
    pub fabrique: TableauFabrique,
    /// Les portes web ouvertes : depuis 0.1.44, absent d'un serveur d'avant.
    pub portes: Vec<TableauPorte>,
}

/// Une porte web ouverte, vue du tableau de bord.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TableauPorte {
    pub slug: String,
    /// Le nom du salon temporaire.
    pub salon: String,
    /// Le pseudo de l'hôte.
    pub hote: String,
    pub invites: u32,
    pub demandes: u32,
    /// Fermeture au plus tard (ms Unix).
    pub expire_le: u64,
}

/// La fabrique des vidéos : un ffmpeg à la fois, une file devant.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TableauFabrique {
    /// Les tâches qui attendent, celle en cours non comprise.
    pub en_file: u32,
    /// Ce qui se fait : « conversion » ou « export », et de quel dossier.
    pub en_cours: Option<String>,
    /// Depuis combien de secondes.
    pub depuis_s: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TableauMembre {
    pub user_id: UserId,
    pub pseudo: String,
    /// Le nom de son salon vocal, s'il y est.
    pub vocal: Option<String>,
    pub diffuse: bool,
    /// Sa ligne de jeu, telle que la liste des membres la montre.
    pub jeu: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TableauSalonVocal {
    pub nom: String,
    pub occupants: Vec<String>,
    pub verrouille: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TableauDiffusion {
    pub streamer: String,
    pub spectateurs: u32,
    pub largeur: u16,
    pub hauteur: u16,
    pub fps: u8,
    pub kbps: u32,
    /// Le palier de débit demandé au streamer, s'il bride le réglage.
    pub palier: Option<u32>,
}

/// Un stock de fichiers face à ses bornes (0 = pas de borne).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TableauStock {
    pub nombre: u32,
    pub octets: u64,
    pub plafond_octets: u64,
    pub ttl_jours: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TableauMusique {
    pub disponible: bool,
    pub salon: Option<ChannelId>,
    /// Le titre en cours, s'il y en a un.
    pub en_cours: Option<String>,
    pub lecture: bool,
    /// Pistes en file, celle en cours non comprise.
    pub file: u32,
    pub pistes_jouees: u32,
    pub echecs: u32,
    /// Délai moyen entre la demande et le premier son, en millisecondes.
    pub premier_son_ms: u64,
    /// Le chemin du yt-dlp en service ; vide s'il manque.
    pub yt_dlp: String,
}

/// Les compteurs d'une version de ki-chat dans les archives de diagnostic
/// (voir le serveur, `diag.rs`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TableauDiag {
    pub version: String,
    pub joueurs: u64,
    pub sessions: u64,
    pub reouvertures: u64,
    pub famines: u64,
    pub erreurs: u64,
    pub crashs: u64,
    pub taille_ko: u64,
}

// --- Le bot musique (voir PLAN-MUSIQUE.md) ---

/// L'identifiant du membre virtuel « Musique », hors de la plage des
/// comptes : c'est lui que porte l'en-tête voix des trames du bot, et que
/// chacun règle ou coupe comme un membre.
pub const MUSIQUE_ID: UserId = u64::MAX - 1;
pub const MUSIQUE_NOM: &str = "Musique";

/// Ce qu'on demande au bot. Tout demande « Contrôler la musique ».
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum CommandeMusique {
    /// Le bot vient dans mon salon vocal.
    Rejoindre,
    /// Une adresse YouTube ou SoundCloud en fin de file — ou tout de suite.
    Ajouter {
        url: String,
        #[serde(default)]
        maintenant: bool,
    },
    Retirer { index: usize },
    /// Une piste de la file change de place.
    Deplacer { de: usize, vers: usize },
    /// Une piste déjà résolue — un résultat de recherche — en file, ou
    /// tout de suite.
    AjouterPiste {
        piste: Piste,
        #[serde(default)]
        maintenant: bool,
    },
    /// Chercher sur YouTube (« youtube ») ou SoundCloud (« soundcloud »).
    Chercher {
        texte: String,
        #[serde(default)]
        source: String,
    },
    /// La file (et la piste en cours) devient une playlist du groupe.
    PlaylistEnregistrer { nom: String },
    /// Une playlist en file — à la place de la file, ou à sa suite.
    PlaylistCharger {
        nom: String,
        #[serde(default)]
        remplacer: bool,
    },
    PlaylistSupprimer { nom: String },
    /// Une piste de plus dans une playlist — créée s'il le faut. L'étoile.
    PlaylistAjouterPiste { nom: String, piste: Piste },
    Lecture,
    Pause,
    Suivant,
    /// Avancer ou reculer dans la piste en cours.
    Position { secondes: u32 },
    Vider,
    Volume { pour_cent: u8 },
    Arreter,
}

/// Longueur maximale d'une recherche de musique, en caractères.
pub const MAX_RECHERCHE_MUSIQUE: usize = 80;
/// La file d'attente ne dépasse pas ça, playlists comprises.
pub const MAX_FILE_MUSIQUE: usize = 300;
/// La playlist de l'étoile.
pub const PLAYLIST_FAVORIS: &str = "Favoris";
/// Les playlists du groupe : combien, de quelle taille, quel nom.
pub const MAX_PLAYLISTS: usize = 50;
pub const MAX_PISTES_PLAYLIST: usize = 200;
pub const MAX_NOM_PLAYLIST: usize = 40;

/// Une playlist du groupe, en résumé — le contenu ne voyage qu'à la
/// demande, en file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResumePlaylist {
    pub nom: String,
    pub pistes: u32,
    pub duree_s: u32,
}

/// Ce que le bot a fait depuis le démarrage du serveur — pour sa fiche.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompteursMusique {
    #[serde(default)]
    pub pistes_jouees: u32,
    #[serde(default)]
    pub echecs: u32,
    /// Délai moyen entre la demande et le premier son, en millisecondes.
    #[serde(default)]
    pub premier_son_ms: u32,
}

/// Une piste, telle que le serveur l'a résolue.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Piste {
    /// « youtube », « soundcloud ».
    pub source: String,
    pub url: String,
    pub titre: String,
    #[serde(default)]
    pub artiste: String,
    #[serde(default)]
    pub duree_s: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vignette: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ajoute_par: Option<String>,
}

/// L'état du bot, poussé à tout le monde à chaque changement.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EtatMusique {
    /// Le serveur a les outils (yt-dlp, ffmpeg) ; sinon le bot n'existe pas.
    #[serde(default)]
    pub disponible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salon: Option<ChannelId>,
    #[serde(default)]
    pub lecture: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub en_cours: Option<Piste>,
    #[serde(default)]
    pub position_ms: u64,
    #[serde(default)]
    pub file: Vec<Piste>,
    /// Volume global du bot, 0–100.
    #[serde(default)]
    pub volume: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub erreur: Option<String>,
    #[serde(default)]
    pub compteurs: CompteursMusique,
    #[serde(default)]
    pub playlists: Vec<ResumePlaylist>,
}

/// Une adresse que le bot accepte : YouTube ou SoundCloud, en HTTPS,
/// courte et sans rien d'exotique — c'est un argument de ligne de commande.
pub fn url_musique_valide(url: &str) -> bool {
    let url = url.trim();
    if url.len() > 300 || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let Some(reste) = url.strip_prefix("https://") else { return false };
    let hote = reste.split(['/', '?', '#']).next().unwrap_or("");
    const HOTES: [&str; 8] = [
        "www.youtube.com",
        "youtube.com",
        "m.youtube.com",
        "music.youtube.com",
        "youtu.be",
        "soundcloud.com",
        "m.soundcloud.com",
        "on.soundcloud.com",
    ];
    HOTES.contains(&hote)
}

/// --- Protocole voix (datagrammes), version 2 ---
///
/// Chaque paquet voix a un en-tête binaire fixe suivi de la trame Opus
/// chiffrée (XChaCha20-Poly1305). Petit-boutiste (little-endian) partout.
/// Le transport est aujourd'hui le datagramme QUIC (donc dans le tunnel TLS
/// de la connexion) — l'en-tête ne suppose rien de plus qu'un datagramme.
///
/// Dans les deux sens :
///   [0..2]  magic  "KV"
///   [2]     version (2)
///   [3..11] user_id de l'émetteur (u64)
///   [11..19] compteur (u64) — strictement croissant, sert de nonce
///   [19..]  trame Opus chiffrée (+16 octets de tag Poly1305)
///
/// Le serveur relaie sans déchiffrer (mode SFU) et fait autorité sur
/// l'identité : le user_id annoncé est celui de la connexion QUIC porteuse,
/// pas une déclaration du client.
///
/// Le nonce XChaCha20 (24 octets) est dérivé de (user_id, compteur) : il est
/// donc unique par clé tant que la clé change à chaque démarrage du serveur
/// et que les compteurs repartent d'un tirage aléatoire à chaque moteur.
pub const VOICE_MAGIC: [u8; 2] = *b"KV";
pub const VOICE_VERSION: u8 = 2;
pub const VOICE_HEADER_LEN: usize = 19;
/// Taille max d'un paquet : 20 ms d'Opus à 128 kbps + tag tient très large.
pub const VOICE_MAX_PACKET: usize = 1400;

pub struct VoicePacket<'a> {
    pub id: u64,
    pub counter: u64,
    pub payload: &'a [u8],
}

/// Analyse un paquet voix entrant. Retourne None si le paquet est invalide.
pub fn parse_voice_packet(buf: &[u8]) -> Option<VoicePacket<'_>> {
    if buf.len() < VOICE_HEADER_LEN || buf[0..2] != VOICE_MAGIC || buf[2] != VOICE_VERSION {
        return None;
    }
    let id = u64::from_le_bytes(buf[3..11].try_into().ok()?);
    let counter = u64::from_le_bytes(buf[11..19].try_into().ok()?);
    Some(VoicePacket {
        id,
        counter,
        payload: &buf[VOICE_HEADER_LEN..],
    })
}

/// Écrit un en-tête voix dans `buf` (qui doit faire au moins VOICE_HEADER_LEN).
pub fn write_voice_header(buf: &mut [u8], id: u64, counter: u64) {
    buf[0..2].copy_from_slice(&VOICE_MAGIC);
    buf[2] = VOICE_VERSION;
    buf[3..11].copy_from_slice(&id.to_le_bytes());
    buf[11..19].copy_from_slice(&counter.to_le_bytes());
}

/// --- Protocole média (partage d'écran), version 1 — voir PLAN-STREAM.md ---
///
/// Chaque trame vidéo voyage dans SON flux QUIC unidirectionnel : fiabilité
/// par trame, sans blocage de tête de ligne entre trames, et le relais peut
/// jeter une trame entière d'un `stop_sending`. L'en-tête est en clair — le
/// serveur route et filtre sans déchiffrer — et sert d'AAD au chiffrement :
/// le réécrire invalide le tag.
///
///   [0..2]   magic  "KF"
///   [2]      version (1)
///   [3]      drapeaux — bit 0 : trame clé (IDR) ; bit 1 : qualité basse
///   [4..8]   stream_id (u32) — attribué par le serveur à StreamStart
///   [8..16]  seq (u64) — strictement croissant, jamais réinitialisé (nonce)
///   [16..24] pts_us (u64) — horodatage de capture, base de la sync A/V
///   [24..28] group_id (u32) — index de GOP (porte ouverte MoQ, cf. plan)
///   [28..30] largeur (u16) · [30..32] hauteur (u16)
///
/// La charge est chiffrée XChaCha20-Poly1305 avec la clé DU STREAM (générée
/// par le streamer, remise à chaque spectateur via WatchAccepted — jamais
/// diffusée au salon). Le nonce porte un octet de domaine : la même clé
/// couvrira la vidéo (1) et l'audio du jeu (2) sans jamais croiser leurs
/// nonces ; la voix (domaine 0 implicite) a sa propre clé de session.
pub const MEDIA_MAGIC_VIDEO: [u8; 2] = *b"KF";
pub const MEDIA_VERSION: u8 = 1;
pub const MEDIA_HEADER_LEN: usize = 32;
/// Une trame vidéo (IDR comprise) ne dépasse jamais ça : au-delà, l'entrée
/// est hostile ou l'encodeur déréglé — dans les deux cas, on coupe.
pub const MEDIA_MAX_FRAME: usize = 4 * 1024 * 1024;
/// Drapeau : la trame est une trame clé (IDR) — un spectateur peut décoder
/// à partir d'elle sans rien avoir vu avant.
pub const MEDIA_FLAG_IDR: u8 = 1 << 0;
/// Drapeau : la trame est de la qualité **basse** — la seconde image, plus
/// petite, que le streamer encode pour les connexions qui ne suivent pas la
/// haute (depuis 0.1.46). Sa séquence est la sienne, son domaine de nonce
/// aussi ([`MEDIA_DOMAIN_VIDEO_BASSE`]).
pub const MEDIA_FLAG_BASSE: u8 = 1 << 1;
/// Domaines de nonce sous une clé de stream. Deux qualités, deux
/// séquences qui partent chacune de zéro : sans domaine distinct, elles
/// réutiliseraient les mêmes nonces sous la même clé.
pub const MEDIA_DOMAIN_VIDEO: u8 = 1;
pub const MEDIA_DOMAIN_GAME_AUDIO: u8 = 2;
pub const MEDIA_DOMAIN_VIDEO_BASSE: u8 = 3;

/// En-tête d'une trame média, tel qu'il circule en clair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaHeader {
    pub idr: bool,
    /// Qualité basse ([`MEDIA_FLAG_BASSE`]).
    pub basse: bool,
    pub stream_id: u32,
    pub seq: u64,
    pub pts_us: u64,
    pub group_id: u32,
    pub width: u16,
    pub height: u16,
}

/// Écrit l'en-tête média dans `buf` (au moins MEDIA_HEADER_LEN octets).
pub fn write_media_header(buf: &mut [u8], h: &MediaHeader) {
    buf[0..2].copy_from_slice(&MEDIA_MAGIC_VIDEO);
    buf[2] = MEDIA_VERSION;
    buf[3] = if h.idr { MEDIA_FLAG_IDR } else { 0 } | if h.basse { MEDIA_FLAG_BASSE } else { 0 };
    buf[4..8].copy_from_slice(&h.stream_id.to_le_bytes());
    buf[8..16].copy_from_slice(&h.seq.to_le_bytes());
    buf[16..24].copy_from_slice(&h.pts_us.to_le_bytes());
    buf[24..28].copy_from_slice(&h.group_id.to_le_bytes());
    buf[28..30].copy_from_slice(&h.width.to_le_bytes());
    buf[30..32].copy_from_slice(&h.height.to_le_bytes());
}

/// Analyse un en-tête média. None si magie, version ou taille ne collent pas.
pub fn parse_media_header(buf: &[u8]) -> Option<MediaHeader> {
    if buf.len() < MEDIA_HEADER_LEN || buf[0..2] != MEDIA_MAGIC_VIDEO || buf[2] != MEDIA_VERSION {
        return None;
    }
    Some(MediaHeader {
        idr: buf[3] & MEDIA_FLAG_IDR != 0,
        basse: buf[3] & MEDIA_FLAG_BASSE != 0,
        stream_id: u32::from_le_bytes(buf[4..8].try_into().ok()?),
        seq: u64::from_le_bytes(buf[8..16].try_into().ok()?),
        pts_us: u64::from_le_bytes(buf[16..24].try_into().ok()?),
        group_id: u32::from_le_bytes(buf[24..28].try_into().ok()?),
        width: u16::from_le_bytes(buf[28..30].try_into().ok()?),
        height: u16::from_le_bytes(buf[30..32].try_into().ok()?),
    })
}

/// --- Son du jeu, version 1 : un paquet Opus par datagramme ---
///
/// Le son du jeu voyage en datagrammes QUIC, jamais dans les flux vidéo :
/// un paquet perdu ne vaut pas d'être retransmis, et rien ne doit faire
/// attendre le son derrière une trame clé de 200 Ko. Même clé de stream que
/// la vidéo, domaine de nonce 2, en-tête en clair qui sert d'AAD :
///
///   [0..2]   magic "KA"
///   [2]      version (1)
///   [3]      drapeaux (réservé, 0)
///   [4..8]   stream_id (u32)
///   [8..16]  seq (u64) — jamais réinitialisé (nonce)
///   [16..24] pts_us (u64) — horodatage de capture, base de la sync A/V
pub const MEDIA_MAGIC_AUDIO: [u8; 2] = *b"KA";
pub const AUDIO_HEADER_LEN: usize = 24;
/// Un paquet Opus stéréo de 20 ms à 96 kbit/s fait ~240 octets ; au-delà de
/// ça, l'entrée est hostile.
pub const AUDIO_MAX_PACKET: usize = 1200;

/// En-tête d'un paquet de son du jeu, tel qu'il circule en clair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioHeader {
    pub stream_id: u32,
    pub seq: u64,
    pub pts_us: u64,
}

/// Écrit l'en-tête audio dans `buf` (au moins AUDIO_HEADER_LEN octets).
pub fn write_audio_header(buf: &mut [u8], h: &AudioHeader) {
    buf[0..2].copy_from_slice(&MEDIA_MAGIC_AUDIO);
    buf[2] = MEDIA_VERSION;
    buf[3] = 0;
    buf[4..8].copy_from_slice(&h.stream_id.to_le_bytes());
    buf[8..16].copy_from_slice(&h.seq.to_le_bytes());
    buf[16..24].copy_from_slice(&h.pts_us.to_le_bytes());
}

/// Analyse un en-tête audio. None si magie, version ou taille ne collent pas.
pub fn parse_audio_header(buf: &[u8]) -> Option<AudioHeader> {
    if buf.len() < AUDIO_HEADER_LEN || buf[0..2] != MEDIA_MAGIC_AUDIO || buf[2] != MEDIA_VERSION {
        return None;
    }
    Some(AudioHeader {
        stream_id: u32::from_le_bytes(buf[4..8].try_into().ok()?),
        seq: u64::from_le_bytes(buf[8..16].try_into().ok()?),
        pts_us: u64::from_le_bytes(buf[16..24].try_into().ok()?),
    })
}

/// Un datagramme est-il du son de jeu (et non de la voix) ? Les deux
/// partagent la connexion ; la magie les sépare avant tout autre examen.
pub fn is_audio_datagram(buf: &[u8]) -> bool {
    buf.len() >= 2 && buf[0..2] == MEDIA_MAGIC_AUDIO
}

#[cfg(test)]
mod audio_tests {
    use super::*;

    #[test]
    fn l_en_tete_audio_fait_l_aller_retour_et_ne_se_confond_pas_avec_la_voix() {
        let h = AudioHeader { stream_id: 7, seq: 123_456, pts_us: 9_876_543 };
        let mut buf = [0u8; AUDIO_HEADER_LEN];
        write_audio_header(&mut buf, &h);
        assert_eq!(parse_audio_header(&buf), Some(h));
        assert!(is_audio_datagram(&buf));
        assert!(parse_voice_packet(&buf).is_none());
        assert!(parse_audio_header(&buf[..AUDIO_HEADER_LEN - 1]).is_none());
        // Une trame vidéo n'est pas du son.
        let mut video = [0u8; MEDIA_HEADER_LEN];
        write_media_header(
            &mut video,
            &MediaHeader { idr: true, basse: false, stream_id: 7, seq: 1, pts_us: 0, group_id: 0, width: 1, height: 1 },
        );
        assert!(!is_audio_datagram(&video));
        // Et les nonces vidéo et audio d'une même séquence diffèrent.
        assert_ne!(
            nonce_for_media(MEDIA_DOMAIN_VIDEO, 7, 1),
            nonce_for_media(MEDIA_DOMAIN_GAME_AUDIO, 7, 1)
        );
    }
}

/// Nonce XChaCha20 (24 octets) d'une trame média : octet de domaine,
/// identifiant de stream, séquence. Unique par clé de stream tant que `seq`
/// ne se répète pas — et il ne se réinitialise jamais, par contrat.
pub fn nonce_for_media(domain: u8, stream_id: u32, seq: u64) -> [u8; 24] {
    let mut n = [0u8; 24];
    n[0] = domain;
    n[1..5].copy_from_slice(&stream_id.to_le_bytes());
    n[8..16].copy_from_slice(&seq.to_le_bytes());
    n
}

/// Ce qu'un stream diffuse, annoncé au salon et mis à jour au vol.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamMeta {
    pub width: u16,
    pub height: u16,
    #[serde(default)]
    pub fps: u8,
    /// Débit d'encodage courant, en kbps.
    #[serde(default)]
    pub kbps: u32,
    /// Empreinte de la machine du streamer (nom de machine + compte
    /// Windows). Un spectateur sur la **même** machine coupe le son du jeu
    /// chez lui : la boucle du streamer ne s'exclut qu'elle-même, et
    /// recapturerait ce second ki-chat sans fin. 0 = inconnue.
    #[serde(default)]
    pub machine: u64,
}

#[cfg(test)]
mod media_tests {
    use super::*;

    #[test]
    fn en_tete_media_aller_retour() {
        let h = MediaHeader {
            idr: true,
            basse: false,
            stream_id: 7,
            seq: 123_456_789_012,
            pts_us: 42_000_000,
            group_id: 9,
            width: 1920,
            height: 1080,
        };
        let mut buf = [0u8; MEDIA_HEADER_LEN];
        write_media_header(&mut buf, &h);
        assert_eq!(parse_media_header(&buf), Some(h));
        // La qualité basse voyage dans son drapeau, sans toucher au reste.
        for (idr, basse) in [(false, true), (true, true), (false, false)] {
            let b = MediaHeader { idr, basse, ..h };
            let mut buf = [0u8; MEDIA_HEADER_LEN];
            write_media_header(&mut buf, &b);
            assert_eq!(parse_media_header(&buf), Some(b));
        }
        // Et ses nonces ne croisent jamais ceux de la haute.
        assert_ne!(
            nonce_for_media(MEDIA_DOMAIN_VIDEO, 7, 1),
            nonce_for_media(MEDIA_DOMAIN_VIDEO_BASSE, 7, 1)
        );

        // Magie ou version faussées : rejet net.
        let mut faux = buf;
        faux[0] = b'X';
        assert!(parse_media_header(&faux).is_none());
        let mut faux = buf;
        faux[2] = 99;
        assert!(parse_media_header(&faux).is_none());
        assert!(parse_media_header(&buf[..MEDIA_HEADER_LEN - 1]).is_none());
    }

    /// La même clé de stream couvre vidéo et audio du jeu : leurs nonces ne
    /// doivent JAMAIS se croiser, ni entre domaines, ni entre streams, ni
    /// entre séquences.
    #[test]
    fn les_nonces_media_ne_se_croisent_pas() {
        let a = nonce_for_media(MEDIA_DOMAIN_VIDEO, 1, 5);
        assert_ne!(a, nonce_for_media(MEDIA_DOMAIN_GAME_AUDIO, 1, 5));
        assert_ne!(a, nonce_for_media(MEDIA_DOMAIN_VIDEO, 2, 5));
        assert_ne!(a, nonce_for_media(MEDIA_DOMAIN_VIDEO, 1, 6));
    }

    /// Un client d'avant le partage d'écran lit un Member sans le champ
    /// `streaming` ; un serveur d'avant n'envoie pas le champ. Personne ne
    /// casse — la discipline serde(default) de toute la maison.
    #[test]
    fn member_sans_streaming_se_lit() {
        let ancien = r#"{"user_id":1,"username":"alice","speaking":false}"#;
        let m: Member = serde_json::from_str(ancien).unwrap();
        assert_eq!(m.streaming, None);
    }
}

// --- Petits utilitaires hex (clé voix dans Welcome) ---

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    // `from_str_radix` accepte un signe devant le nombre : « +a » se lisait
    // comme 0x0a. Trouvé par le fuzzing (crates/protocol/fuzz). Un
    // hexadécimal, c'est des chiffres hexadécimaux et rien d'autre.
    if !s.len().is_multiple_of(2) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// L'hexadécimal, strictement : ce que `hex_encode` écrit, en
    /// majuscules ou non — et pas le « +a » que `from_str_radix` tolère.
    #[test]
    fn hex_decode_ne_prend_que_des_chiffres_hexadecimaux() {
        assert_eq!(hex_decode("0aFF"), Some(vec![0x0a, 0xff]));
        assert_eq!(hex_decode(""), Some(vec![]));
        assert_eq!(hex_decode("+a"), None);
        assert_eq!(hex_decode("-1"), None);
        assert_eq!(hex_decode(" a"), None);
        assert_eq!(hex_decode("abc"), None);
        assert_eq!(hex_decode("zz"), None);
        assert_eq!(hex_decode("é"), None);
        let octets: Vec<u8> = (0..=255).collect();
        assert_eq!(hex_decode(&hex_encode(&octets)).as_deref(), Some(octets.as_slice()));
    }

    /// Un journal d'avant les réactions se relit tel quel, et un message
    /// d'aujourd'hui fait l'aller-retour avec sa réponse et ses réactions.
    #[test]
    fn un_message_ancien_se_relit_et_un_nouveau_fait_l_aller_retour() {
        let ancien = r#"{"user_id":1,"username":"kevin","text":"yo","ts":42}"#;
        let rec: ChatRecord = serde_json::from_str(ancien).unwrap();
        assert!(rec.reply_to.is_none() && rec.reactions.is_empty());

        let nouveau = ChatRecord {
            user_id: 2,
            username: "léa".into(),
            text: "oui".into(),
            ts: 43,
            reply_to: Some(ReplyRef { user_id: 1, ts: 42, username: "kevin".into(), excerpt: "yo".into() }),
            reactions: vec![Reaction { emoji: "👍".into(), users: vec![1, 3] }],
            edited: false,
        };
        let ligne = serde_json::to_string(&nouveau).unwrap();
        let relu: ChatRecord = serde_json::from_str(&ligne).unwrap();
        assert_eq!(relu.reply_to, nouveau.reply_to);
        assert_eq!(relu.reactions, nouveau.reactions);
        // Et rien de tout ça n'alourdit un message ordinaire.
        let simple = serde_json::to_string(&ChatRecord { ts: 1, ..Default::default() }).unwrap();
        assert!(!simple.contains("reply_to") && !simple.contains("reactions"));
        assert!(!simple.contains("edited"), "un message intact ne dit pas qu'il ne l'est pas");
        let modifie = ChatRecord { ts: 1, edited: true, ..Default::default() };
        let json = serde_json::to_string(&modifie).unwrap();
        assert!(json.contains("\"edited\":true"));
        assert!(serde_json::from_str::<ChatRecord>(&json).unwrap().edited);
    }

    /// Une réaction, c'est un emoji : pas une phrase, pas une lettre, pas
    /// un blanc. Et l'extrait d'une réponse tient sur une ligne bornée.
    #[test]
    fn l_emoji_de_reaction_est_borne_et_l_extrait_aussi() {
        for ok in REACTIONS {
            assert!(clean_emoji(ok).is_some(), "{ok}");
        }
        assert_eq!(clean_emoji(" 🎉 ").as_deref(), Some("🎉"));
        assert!(clean_emoji("").is_none());
        assert!(clean_emoji("a").is_none());
        assert!(clean_emoji("👍👍👍👍👍").is_none());
        assert!(clean_emoji("\u{7}").is_none());

        assert_eq!(excerpt_of("salut\nça va"), "salut…");
        let long = "x".repeat(MAX_EXCERPT + 5);
        let e = excerpt_of(&long);
        assert_eq!(e.chars().count(), MAX_EXCERPT + 1);
        assert!(e.ends_with('…'));
        assert_eq!(excerpt_of("court"), "court");
    }

    /// Le statut de jeu fait l'aller-retour, se nettoie, et se raconte en
    /// une ligne lisible ; un membre d'un serveur antérieur n'en a pas.
    #[test]
    fn le_statut_de_jeu_se_raconte_en_une_ligne() {
        let s = JeuStatut {
            etat: JeuEtat::EnJeu,
            file: "competitive".into(),
            carte: "Ascent".into(),
            score_allie: 7,
            score_adverse: 5,
            party_taille: 3,
            party_max: 5,
            party_ouverte: true,
            rang: 15,
            niveau: 120,
            custom: false,
            nom: String::new(),
        };
        assert_eq!(s.ligne(), "compétitive · Ascent · 7-5 · party 3/5");
        let relu: JeuStatut = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(relu, s);

        let menu = JeuStatut { etat: JeuEtat::Menus, file: String::new(), party_taille: 1, ..s.clone() };
        // Sa party est ouverte et il est seul : il cherche du monde.
        assert_eq!(menu.ligne(), "Valorant · au menu · cherche des joueurs");
        let file = JeuStatut { etat: JeuEtat::Menus, ..s.clone() };
        assert_eq!(file.ligne(), "Valorant · en file compétitive · party 3/5");
        let choix = JeuStatut { etat: JeuEtat::PreGame, party_taille: 1, ..s.clone() };
        assert_eq!(choix.ligne(), "compétitive · sélection des agents · Ascent");

        let sale = JeuStatut {
            file: "x".repeat(200),
            carte: "Asc\u{7}ent".into(),
            score_allie: 250,
            rang: 99,
            ..s.clone()
        };
        let propre = sale.nettoyer();
        // Tronqué à la borne, plus le signe qui dit qu'il l'a été.
        assert!(propre.file.chars().count() <= MAX_JEU_TEXTE + 1 && propre.file.ends_with('…'));
        assert_eq!(propre.carte, "Ascent");
        assert_eq!((propre.score_allie, propre.rang), (99, 27));

        let ancien = r#"{"user_id":1,"username":"k","speaking":false}"#;
        let m: Member = serde_json::from_str(ancien).unwrap();
        assert!(m.jeu.is_none() && m.riot_id.is_none() && m.rang_valorant.is_none());
    }

    /// Le bot n'accepte que YouTube et SoundCloud, en HTTPS, sans espace.
    #[test]
    fn un_autre_jeu_se_dit_en_une_ligne() {
        let j = JeuStatut::autre_jeu("Rocket League");
        assert_eq!(j.ligne(), "joue à Rocket League");
        assert!(!j.est_valorant());
        assert_eq!(j.etat, JeuEtat::EnJeu);
        // Un client d'avant ne connaît pas le nom : il ne l'envoie pas, et
        // ce qu'il lit reste VALORANT.
        let relu: JeuStatut =
            serde_json::from_str("{\"etat\":\"en_jeu\",\"file\":\"competitive\"}").unwrap();
        assert!(relu.est_valorant());
        assert!(!serde_json::to_string(&relu).unwrap().contains("nom"));
        assert!(serde_json::to_string(&j).unwrap().contains("\"nom\":\"Rocket League\""));
        // Le nom est assaini et borné comme le reste : il vient d'un client.
        let sale = JeuStatut { nom: "Jeu\u{0}".repeat(40), ..JeuStatut::default() }.nettoyer();
        assert!(!sale.nom.contains('\u{0}'));
        assert!(sale.nom.chars().count() <= MAX_JEU_TEXTE + 1);
    }

    #[test]
    fn le_tableau_de_bord_fait_l_aller_retour_et_se_lit_vide() {
        let t = TableauAdmin {
            version: "0.1.39".into(),
            depuis_s: 3600,
            en_ligne: vec![TableauMembre { pseudo: "kevin".into(), diffuse: true, ..Default::default() }],
            fichiers: TableauStock { nombre: 3, octets: 10, plafond_octets: 100, ttl_jours: 30 },
            disque_libre_octets: Some(1 << 30),
            ..Default::default()
        };
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<TableauAdmin>(&json).unwrap(), t);
        // Un serveur qui ne dit rien : tout à zéro, rien ne casse.
        let vide: TableauAdmin = serde_json::from_str("{}").unwrap();
        assert_eq!(vide, TableauAdmin::default());
        assert!(!json.contains("memoire_octets"), "absent : pas écrit");
        // La fabrique (0.1.43) : lue si elle est là, à zéro sinon.
        assert_eq!(vide.fabrique, TableauFabrique::default());
        let t = TableauAdmin {
            fabrique: TableauFabrique { en_file: 2, en_cours: Some("export de abcd".into()), depuis_s: 41 },
            ..Default::default()
        };
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("\"fabrique\""));
        assert_eq!(serde_json::from_str::<TableauAdmin>(&json).unwrap(), t);
    }

    #[test]
    fn les_adresses_du_bot_sont_filtrees() {
        assert!(url_musique_valide("https://www.youtube.com/watch?v=abc"));
        assert!(url_musique_valide("https://youtu.be/abc"));
        assert!(url_musique_valide("https://soundcloud.com/artiste/titre"));
        assert!(!url_musique_valide("http://www.youtube.com/watch?v=abc"));
        assert!(!url_musique_valide("https://evil.com/?youtube.com"));
        assert!(!url_musique_valide("https://youtube.com.evil.com/x"));
        assert!(!url_musique_valide("https://www.youtube.com/watch?v=abc --exec rm"));
        let cmd: ClientMsg = serde_json::from_str(r#"{"type":"musique","commande":{"op":"ajouter","url":"https://youtu.be/x"}}"#).unwrap();
        assert!(matches!(cmd, ClientMsg::Musique { commande: CommandeMusique::Ajouter { maintenant: false, .. } }));
        let cmd: ClientMsg = serde_json::from_str(r#"{"type":"musique","commande":{"op":"suivant"}}"#).unwrap();
        assert!(matches!(cmd, ClientMsg::Musique { commande: CommandeMusique::Suivant }));
    }

    /// Une party ouverte et pas pleine, au menu, cherche des joueurs — en
    /// partie, non.
    #[test]
    fn la_party_ouverte_cherche_des_joueurs() {
        let mut j = JeuStatut {
            etat: JeuEtat::Menus,
            party_ouverte: true,
            party_taille: 3,
            party_max: 5,
            ..Default::default()
        };
        assert_eq!(j.ligne(), "Valorant · au menu · party 3/5 · cherche des joueurs");
        j.party_taille = 5;
        assert_eq!(j.ligne(), "Valorant · au menu · party 5/5");
        j.party_taille = 3;
        j.etat = JeuEtat::EnJeu;
        j.file = "competitive".into();
        j.carte = "Ascent".into();
        assert!(!j.ligne().contains("cherche"));
    }

    /// Les rangs ont leur nom français, et le Riot ID se découpe en
    /// pseudo et tag — ou se refuse.
    #[test]
    fn les_rangs_et_les_riot_id_se_lisent() {
        assert_eq!(nom_de_rang(0), "Non classé");
        assert_eq!(nom_de_rang(3), "Fer 1");
        assert_eq!(nom_de_rang(14), "Or 3");
        assert_eq!(nom_de_rang(24), "Immortel 1");
        assert_eq!(nom_de_rang(27), "Radiant");
        assert_eq!(parser_riot_id(" Redik#6162 "), Some(("Redik".into(), "6162".into())));
        assert_eq!(parser_riot_id("Jean Michel#eu w"), None);
        assert_eq!(parser_riot_id("Jean Michel#euw"), Some(("Jean Michel".into(), "EUW".into())));
        assert!(parser_riot_id("sansTag").is_none());
        assert!(parser_riot_id("ab#123").is_none());
        let f = FicheValorant { riot_id: "Redik#6162".into(), ..Default::default() };
        let relu: FicheValorant = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(relu, f);
        // Un acte et un bilan de membre font l'aller-retour aussi.
        let acte = StatsSaison { saison: "e9a2".into(), victoires: 14, parties: 25, tier_fin: 16, rr_fin: 57 };
        let relu: StatsSaison = serde_json::from_str(&serde_json::to_string(&acte).unwrap()).unwrap();
        assert_eq!(relu, acte);
        let bilan = BilanMembre {
            sept_jours: Bilan { matchs: 3, victoires: 2, defaites: 1, rr: 41, ..Default::default() },
            trente_jours: Bilan { matchs: 12, victoires: 7, defaites: 5, ..Default::default() },
            serie: -2,
            forme: vec![-1, -1, 1, 0],
            agents: vec![("Jett".into(), 8, 5), ("Reyna".into(), 4, 2)],
            cartes: vec![("Ascent".into(), 6, 4)],
            duos: vec![(7, 5, 4)],
        };
        let relu: BilanMembre = serde_json::from_str(&serde_json::to_string(&bilan).unwrap()).unwrap();
        assert_eq!(relu, bilan);
    }

    /// Un match classé, gagné, de treize manches à neuf, sans détail :
    /// le socle des tests d'agrégats.
    fn match_de_test(id: &str, date: u64, mode: &str, gagne: Option<bool>) -> MatchResume {
        MatchResume {
            id: id.into(),
            date,
            carte: "Ascent".into(),
            mode: mode.into(),
            agent: "Jett".into(),
            kills: 20,
            deaths: 10,
            assists: 4,
            score: 5_500,
            tete_pct: 25,
            manches: if gagne == Some(false) { (9, 13) } else { (13, 9) },
            gagne,
            tier: 15,
            duree_s: 2_400,
            tetes: 25,
            tirs: 100,
            ..Default::default()
        }
    }

    /// Une fiche écrite par un serveur 0.1.39 — pas un champ de plus que
    /// ce qu'il connaissait — se relit avec les défauts ; une fiche où
    /// tout est rempli fait l'aller-retour à l'identique ; et rien de neuf
    /// n'alourdit un match ou une fiche de membre qui n'en a pas.
    #[test]
    fn une_fiche_d_avant_se_relit_et_une_fiche_enrichie_fait_l_aller_retour() {
        let ancienne = r#"{"riot_id":"Redik#6162","region":"eu","plateforme":"pc","niveau":212,
            "rang":{"tier":15,"rr":40,"delta":18,"elo":1240,"saison":""},
            "historique_rr":[{"match_id":"m1","date":1700000000000,"tier":15,"rr":40,"delta":18,"carte":"Ascent"}],
            "matchs":[{"id":"m1","date":1700000000000,"carte":"Ascent","mode":"Compétitif","agent":"Jett",
                "kills":20,"deaths":10,"assists":3,"score":4000,"tete_pct":20,"manches":[13,9],"gagne":true,
                "tier":15,"duree_s":2400}],
            "maj":1700000001000}"#;
        let f: FicheValorant = serde_json::from_str(ancienne).unwrap();
        assert_eq!(f.riot_id, "Redik#6162");
        assert_eq!(f.matchs.len(), 1);
        assert_eq!(f.historique_rr.len(), 1);
        assert!(f.matchs[0].manches_detail.is_none());
        assert!(f.matchs[0].avec.is_empty() && f.matchs[0].contre.is_empty());
        assert_eq!(f.matchs[0].party, 0);
        assert_eq!(f.matchs[0].degats, 0);
        assert!(f.matchs[0].saison.is_empty());
        assert!(f.saisons.is_empty());
        assert_eq!(f.rang.boucliers, 0);
        assert_eq!(f.rang.placements_restants, 0);
        assert_eq!(f.rang.classement, 0);
        assert!(!f.historique_rr[0].protege);
        assert!(f.historique_rr[0].saison.is_empty());
        // Et elle vaut ce qu'elle valait : un match gagné, 18 RR.
        assert_eq!(f.bilan(0, u64::MAX, true).victoires, 1);
        assert_eq!(f.bilan(0, u64::MAX, true).rr, 18);

        let detail = DetailManches {
            manches: 22,
            kast: 17,
            premiers_sangs: 3,
            premieres_morts: 2,
            triples: 2,
            quadruples: 1,
            aces: 0,
            clutchs_tentes: 2,
            clutchs: 1,
            meilleur_clutch: 2,
            poses: 3,
            desamorcages: 1,
            deroule: "VVDVVDDVVVVDVDDVVDVVVD".into(),
        };
        let pleine = FicheValorant {
            riot_id: "Redik#6162".into(),
            region: "eu".into(),
            plateforme: "pc".into(),
            niveau: 212,
            rang: RangValorant {
                tier: 16,
                rr: 57,
                delta: 18,
                elo: 1357,
                saison: "e9a2".into(),
                placements_restants: 0,
                boucliers: 2,
                classement: 0,
            },
            pic: Some(RangValorant { tier: 18, rr: 12, saison: "e9a1".into(), ..Default::default() }),
            historique_rr: vec![PointRR {
                match_id: "m1".into(),
                date: 1_700_000_000_000,
                tier: 16,
                rr: 57,
                delta: 18,
                carte: "Ascent".into(),
                saison: "e9a2".into(),
                protege: true,
            }],
            matchs: vec![MatchResume {
                saison: "e9a2".into(),
                degats: 4_212,
                degats_recus: 3_980,
                party: 3,
                avec: vec![2, 3],
                contre: vec![4],
                manches_detail: Some(detail),
                ..match_de_test("m1", 1_700_000_000_000, "Compétitif", Some(true))
            }],
            maj: 1_700_000_001_000,
            saisons: vec![StatsSaison {
                saison: "e9a2".into(),
                victoires: 14,
                parties: 25,
                tier_fin: 16,
                rr_fin: 57,
            }],
        };
        let json = serde_json::to_string(&pleine).unwrap();
        let relu: FicheValorant = serde_json::from_str(&json).unwrap();
        assert_eq!(relu, pleine);

        let nu = serde_json::to_string(&MatchResume::default()).unwrap();
        for champ in ["manches_detail", "avec", "contre", "saison"] {
            assert!(!nu.contains(champ), "un match sans {champ} ne l'écrit pas : {nu}");
        }
        let membre = serde_json::to_string(&FicheMembre::default()).unwrap();
        assert!(!membre.contains("bilan"), "une fiche de membre sans bilan ne l'écrit pas : {membre}");
        let stats = serde_json::to_string(&ServerMsg::StatsValorant {
            fiches: vec![],
            esports: vec![],
            activite: vec![],
        })
        .unwrap();
        assert!(!stats.contains("activite"), "pas d'activité, pas de champ : {stats}");
    }

    /// Le bilan ne prend que la fenêtre et le mode demandés ; les nuls ne
    /// sont ni victoire ni défaite ; un combat à mort n'entre jamais ; et
    /// sans dégâts connus, l'ADR est « — », pas 0.
    #[test]
    fn le_bilan_ne_compte_que_la_fenetre_et_le_mode() {
        let jour = 86_400_000u64;
        let maintenant = 1_800_000_000_000u64;
        let (depuis, jusqu_a) = (maintenant - 7 * jour, maintenant);
        let f = FicheValorant {
            matchs: vec![
                match_de_test("classe", maintenant - jour, "Compétitif", Some(true)),
                match_de_test("nul", maintenant - 2 * jour, "Non classé", None),
                match_de_test("dm", maintenant - 3 * jour, "Combat à mort", Some(true)),
                match_de_test("vieux", maintenant - 8 * jour, "Compétitif", Some(false)),
            ],
            historique_rr: vec![
                PointRR { match_id: "classe".into(), date: maintenant - jour, delta: 18, ..Default::default() },
                PointRR { match_id: "vieux".into(), date: maintenant - 8 * jour, delta: -12, ..Default::default() },
            ],
            ..Default::default()
        };
        let classe = f.bilan(depuis, jusqu_a, true);
        assert_eq!((classe.matchs, classe.victoires, classe.defaites), (1, 1, 0));
        assert_eq!(classe.rr, 18, "seul le point de la fenêtre compte");
        assert_eq!(classe.manches, 22);
        assert_eq!(classe.kills, 20);
        assert_eq!(classe.kd(), Some(2.0));
        assert_eq!(classe.acs(), Some(250.0));
        assert_eq!(classe.tete_pct(), Some(25.0));
        assert_eq!(classe.victoires_pct(), Some(100.0));
        assert_eq!(classe.adr(), None, "pas de dégâts connus : pas d'ADR");
        assert_eq!(classe.kast_pct(), None, "pas de détail : pas de KAST");
        assert_eq!(classe.fk_par_match(), None);
        assert_eq!(classe.duree_s, 2_400);

        let tous = f.bilan(depuis, jusqu_a, false);
        assert_eq!(tous.matchs, 2, "le nul entre, le combat à mort jamais");
        assert_eq!((tous.victoires, tous.defaites), (1, 0));
        assert_eq!(tous.victoires_pct(), Some(100.0), "un nul ne pèse pas sur le taux");
        assert_eq!(tous.rr, 18);

        // Toute la fiche : le vieux match aussi, et son -12.
        let tout = f.bilan(0, u64::MAX, true);
        assert_eq!((tout.matchs, tout.victoires, tout.defaites), (2, 1, 1));
        assert_eq!(tout.rr, 6);
        assert_eq!(tout.victoires_pct(), Some(50.0));
        assert!(tout.assez(2) && !tout.assez(3));

        // Un match avec dégâts et détail nourrit l'ADR et le KAST, sur ses
        // seules manches.
        let mut b = tout.clone();
        b.ajouter(&MatchResume {
            degats: 3_300,
            manches_detail: Some(DetailManches {
                manches: 22,
                kast: 11,
                premiers_sangs: 4,
                meilleur_clutch: 3,
                ..Default::default()
            }),
            ..match_de_test("riche", maintenant, "Compétitif", Some(true))
        });
        assert_eq!(b.matchs, 3);
        assert_eq!((b.matchs_degats, b.manches_degats), (1, 22));
        assert_eq!(b.adr(), Some(150.0));
        assert_eq!(b.kast_pct(), Some(50.0));
        assert_eq!(b.fk_par_match(), Some(4.0));
        assert_eq!(b.meilleur_clutch, 3);
        // `matchs_dans` rend la fenêtre entière, combat à mort compris :
        // c'est la table, pas la moyenne.
        let ids: Vec<&str> = f.matchs_dans(depuis, jusqu_a, false).map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["classe", "nul", "dm"]);
        assert!(!FicheValorant::a_des_manches(&f.matchs[2]));
        assert!(!FicheValorant::a_des_manches(&MatchResume::default()), "sans manche du tout non plus");
    }

    /// Un bilan vide n'a pas de taux : toutes les méthodes rendent `None`,
    /// et aucune ne divise par zéro — même avec des kills sans mort.
    #[test]
    fn les_taux_ne_divisent_jamais_par_zero() {
        let vide = Bilan::default();
        assert_eq!(vide.kd(), None);
        assert_eq!(vide.kda(), None);
        assert_eq!(vide.acs(), None);
        assert_eq!(vide.adr(), None);
        assert_eq!(vide.kast_pct(), None);
        assert_eq!(vide.tete_pct(), None);
        assert_eq!(vide.victoires_pct(), None);
        assert_eq!(vide.fk_par_match(), None);
        assert!(!vide.assez(1) && vide.assez(0));
        // Un match sans mort : le K/D se calcule sur une mort, pas sur zéro.
        let mut b = Bilan::default();
        b.ajouter(&MatchResume { kills: 10, deaths: 0, assists: 2, ..match_de_test("m", 1, "Compétitif", None) });
        assert_eq!(b.kd(), Some(10.0));
        assert_eq!(b.kda(), Some(12.0));
        // Des compteurs déjà au plafond n'explosent pas non plus.
        let mut plein = Bilan { matchs: u16::MAX, kills: u32::MAX, meilleur_clutch: 5, ..Default::default() };
        plein.ajouter(&match_de_test("m", 1, "Compétitif", Some(true)));
        assert_eq!((plein.matchs, plein.kills, plein.meilleur_clutch), (u16::MAX, u32::MAX, 5));
    }

    /// La forme se lit du plus récent au plus ancien, quel que soit
    /// l'ordre de la fiche ; la série compte les résultats d'affilée en
    /// sautant les nuls ; sans classé, rien.
    #[test]
    fn la_forme_et_la_serie_se_lisent_du_plus_recent() {
        // Rangés du plus ancien au plus récent : V, D, V, V — soit, du
        // plus récent, V V D V.
        let mut f = FicheValorant {
            matchs: vec![
                match_de_test("a", 1_000, "Compétitif", Some(true)),
                match_de_test("b", 2_000, "Compétitif", Some(false)),
                match_de_test("c", 3_000, "Compétitif", Some(true)),
                match_de_test("d", 4_000, "Compétitif", Some(true)),
                // Une partie rapide gagnée entre-temps ne compte pas.
                match_de_test("e", 3_500, "Partie rapide", Some(true)),
            ],
            ..Default::default()
        };
        assert_eq!(f.forme(10), [1, 1, -1, 1]);
        assert_eq!(f.forme(2), [1, 1]);
        assert_eq!(f.serie(), 2);
        // Un nul tout frais s'ignore : la série tient.
        f.matchs.push(match_de_test("nul", 5_000, "Compétitif", None));
        assert_eq!(f.forme(10), [0, 1, 1, -1, 1]);
        assert_eq!(f.serie(), 2);
        // Deux défaites par-dessus : la série s'inverse.
        f.matchs.push(match_de_test("f", 6_000, "Compétitif", Some(false)));
        f.matchs.push(match_de_test("g", 7_000, "Compétitif", Some(false)));
        assert_eq!(f.serie(), -2);
        // Sans aucun classé : rien à lire.
        let sans = FicheValorant {
            matchs: vec![match_de_test("x", 1, "Non classé", Some(true))],
            ..Default::default()
        };
        assert!(sans.forme(10).is_empty());
        assert_eq!(sans.serie(), 0);
        assert_eq!(FicheValorant::default().serie(), 0);
    }

    /// Le MMR caché se devine aux variations de RR : gagner nettement
    /// plus qu'on ne perd, c'est au-dessus du rang ; l'inverse, en
    /// dessous ; sinon au niveau. Sans trois victoires et trois défaites,
    /// sans rang ou en placements, rien ; un point protégé, un delta nul
    /// ou un acte précédent ne comptent pas ; au-delà de vingt, seuls les
    /// plus récents pèsent.
    #[test]
    fn le_mmr_cache_se_devine_aux_variations_de_rr() {
        // Le point `i` est d'autant plus ancien que `i` est grand.
        let point = |i: u64, delta: i32, saison: &str| PointRR {
            match_id: format!("m{i}"),
            date: 10_000 - i,
            delta,
            saison: saison.into(),
            ..Default::default()
        };
        let fiche = |deltas: &[i32]| FicheValorant {
            rang: RangValorant { tier: 16, rr: 57, ..Default::default() },
            historique_rr: deltas.iter().enumerate().map(|(i, d)| point(i as u64, *d, "e9a2")).collect(),
            ..Default::default()
        };
        // (1) Il gagne bien plus qu'il ne perd : le jeu le pousse.
        let e = fiche(&[22, -12, 24, -14, 21, -13]).mmr_estime().expect("assez de classés");
        assert_eq!(e.position, PositionMmr::AuDessus);
        assert!((e.gain_moyen - 22.333).abs() < 0.01, "gain {}", e.gain_moyen);
        assert!((e.perte_moyenne - 13.0).abs() < 0.01, "perte {}", e.perte_moyenne);
        assert_eq!((e.victoires, e.defaites, e.points), (3, 3, 6));
        // (2) L'inverse : le jeu le retient.
        let e = fiche(&[12, -21, 11, -22, 13, -20]).mmr_estime().unwrap();
        assert_eq!(e.position, PositionMmr::EnDessous);
        // (3) +18 / −17 : à l'équilibre.
        let e = fiche(&[18, -17, 18, -17, 18, -17]).mmr_estime().unwrap();
        assert_eq!(e.position, PositionMmr::AuNiveau);
        // (4) Deux victoires seulement : pas assez.
        assert_eq!(fiche(&[22, -12, -14, 24, -13]).mmr_estime(), None);
        // (5) Une descente protégée et un delta nul ne pèsent pas.
        let mut f = fiche(&[22, -12, 24, -14, 21, -13]);
        f.historique_rr.push(PointRR { protege: true, ..point(50, -60, "e9a2") });
        f.historique_rr.push(point(51, 0, "e9a2"));
        let e = f.mmr_estime().unwrap();
        assert!((e.perte_moyenne - 13.0).abs() < 0.01, "la descente protégée ne pèse pas");
        assert_eq!(e.points, 6);
        // (6) Un point de l'acte précédent est écarté quand le plus
        // récent a une saison — mais tout compte si elle est inconnue.
        let mut f = fiche(&[22, -12, 24, -14, 21, -13]);
        f.historique_rr.push(point(60, 80, "e9a1"));
        let e = f.mmr_estime().unwrap();
        assert!((e.gain_moyen - 22.333).abs() < 0.01, "l'acte d'avant ne compte pas");
        for p in &mut f.historique_rr {
            p.saison.clear();
        }
        let e = f.mmr_estime().unwrap();
        assert_eq!(e.victoires, 4, "sans acte connu, tout compte");
        // (7) Sans rang, ou en placements : rien à deviner.
        let mut f = fiche(&[22, -12, 24, -14, 21, -13]);
        f.rang.tier = 0;
        assert_eq!(f.mmr_estime(), None);
        f.rang.tier = 16;
        f.rang.placements_restants = 2;
        assert_eq!(f.mmr_estime(), None);
        // (8) Plus de vingt points : seuls les vingt plus récents comptent.
        let mut deltas: Vec<i32> = (0..20).map(|i| if i % 2 == 0 { 20 } else { -10 }).collect();
        deltas.extend(std::iter::repeat_n(-50, 10));
        let e = fiche(&deltas).mmr_estime().unwrap();
        assert_eq!((e.victoires, e.defaites, e.points), (10, 10, 20));
        assert!((e.gain_moyen - 20.0).abs() < 0.01);
        assert!((e.perte_moyenne - 10.0).abs() < 0.01, "les −50 anciens ne pèsent pas");
        assert_eq!(e.position, PositionMmr::AuDessus);
        // Une estimation se sérialise et se relit telle quelle.
        let relu: EstimationMmr = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(relu, e);
    }

    /// Agents, cartes et duos se ventilent sur la fenêtre, triés par
    /// parties ; les victoires ensemble sont celles des matchs où l'on
    /// était du même côté.
    #[test]
    fn les_agents_les_cartes_et_les_duos_se_ventilent() {
        let f = FicheValorant {
            matchs: vec![
                MatchResume { avec: vec![2, 3], ..match_de_test("a", 1_000, "Compétitif", Some(true)) },
                MatchResume {
                    agent: "Reyna".into(),
                    carte: "Bind".into(),
                    avec: vec![2],
                    ..match_de_test("b", 2_000, "Compétitif", Some(false))
                },
                // Le 2 en double : une seule partie ensemble quand même.
                MatchResume { avec: vec![2, 2], contre: vec![3], ..match_de_test("c", 3_000, "Compétitif", Some(true)) },
                // Hors fenêtre : ne pèse nulle part.
                MatchResume { agent: "Sage".into(), avec: vec![9], ..match_de_test("z", 9_000, "Compétitif", Some(true)) },
                // Un combat à mort ne ventile rien, même « avec » quelqu'un.
                MatchResume { avec: vec![5], ..match_de_test("dm", 2_500, "Combat à mort", Some(true)) },
            ],
            ..Default::default()
        };
        let agents = f.par_agent(0, 5_000, true);
        let noms: Vec<(&str, u16, u16)> =
            agents.iter().map(|(n, b)| (n.as_str(), b.matchs, b.victoires)).collect();
        assert_eq!(noms, [("Jett", 2, 2), ("Reyna", 1, 0)]);
        let cartes = f.par_carte(0, 5_000, true);
        let noms: Vec<(&str, u16, u16)> =
            cartes.iter().map(|(n, b)| (n.as_str(), b.matchs, b.victoires)).collect();
        assert_eq!(noms, [("Ascent", 2, 2), ("Bind", 1, 0)]);
        assert_eq!(cartes[1].1.victoires_pct(), Some(0.0));
        assert_eq!(f.duos(0, 5_000), [(2, 3, 2), (3, 1, 1)]);
        assert!(f.duos(6_000, 8_000).is_empty());
        // À égalité de parties, l'ordre est celui des noms : stable d'un
        // envoi à l'autre.
        let egaux = FicheValorant {
            matchs: vec![
                MatchResume { agent: "Sova".into(), ..match_de_test("a", 1, "Compétitif", Some(true)) },
                MatchResume { agent: "Brimstone".into(), ..match_de_test("b", 2, "Compétitif", Some(true)) },
            ],
            ..Default::default()
        };
        let noms: Vec<String> = egaux.par_agent(0, 10, true).into_iter().map(|(n, _)| n).collect();
        assert_eq!(noms, ["Brimstone", "Sova"]);
    }

    /// Le résumé garde les matchs et les points les plus récents, sans le
    /// détail des manches ni les actes — et tout le reste.
    #[test]
    fn le_resume_allege_la_fiche() {
        let detail = DetailManches { manches: 22, kast: 15, deroule: "VD".into(), ..Default::default() };
        let mut f = FicheValorant {
            riot_id: "Redik#6162".into(),
            niveau: 212,
            rang: RangValorant { tier: 16, rr: 57, boucliers: 1, ..Default::default() },
            pic: Some(RangValorant { tier: 18, ..Default::default() }),
            saisons: vec![StatsSaison { saison: "e9a2".into(), ..Default::default() }],
            ..Default::default()
        };
        // Du plus ancien au plus récent, pour vérifier qu'on trie.
        for i in 0..60u64 {
            f.matchs.push(MatchResume {
                manches_detail: Some(detail.clone()),
                avec: vec![2],
                ..match_de_test(&format!("m{i}"), 1_000 + i, "Compétitif", Some(true))
            });
        }
        for i in 0..100u64 {
            f.historique_rr.push(PointRR { match_id: format!("m{i}"), date: 1_000 + i, ..Default::default() });
        }
        let r = f.resume(5, 10);
        assert_eq!(r.matchs.len(), 5);
        assert_eq!(r.historique_rr.len(), 10);
        let ids: Vec<&str> = r.matchs.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["m59", "m58", "m57", "m56", "m55"]);
        assert!(r.matchs.iter().all(|m| m.manches_detail.is_none()));
        assert!(r.matchs.iter().all(|m| m.avec == [2]), "les co-membres restent");
        assert_eq!(r.historique_rr[0].match_id, "m99");
        assert_eq!(r.historique_rr[9].match_id, "m90");
        assert!(r.saisons.is_empty());
        assert_eq!(r.riot_id, "Redik#6162");
        assert_eq!(r.rang, f.rang);
        assert_eq!(r.pic, f.pic);
        assert_eq!(r.niveau, 212);
        // Une fiche plus courte que la demande rend ce qu'elle a.
        let petite = FicheValorant { matchs: f.matchs[..2].to_vec(), ..Default::default() };
        assert_eq!(petite.resume(5, 10).matchs.len(), 2);
        assert!(FicheValorant::default().resume(5, 10).matchs.is_empty());
    }

    /// Supprimer les messages des autres est une autorité : jamais pour
    /// `@everyone`, et connue de la liste des permissions.
    #[test]
    fn supprimer_les_messages_est_une_autorite() {
        let refuse_a_tous = std::hint::black_box(perm::NOT_FOR_EVERYONE);
        assert!(refuse_a_tous & perm::DELETE_MESSAGES != 0);
        assert!(perm::ALL.iter().any(|(bit, _, _)| *bit == perm::DELETE_MESSAGES));
        assert!(perm::has(perm::ADMINISTRATOR, perm::DELETE_MESSAGES));
        assert!(!perm::has(perm::DEFAULT, perm::DELETE_MESSAGES));
    }

    #[test]
    fn voice_roundtrip() {
        let mut buf = [0u8; 32];
        write_voice_header(&mut buf, 42, 7_000_000_000);
        buf[VOICE_HEADER_LEN..VOICE_HEADER_LEN + 3].copy_from_slice(b"abc");
        let pkt = parse_voice_packet(&buf[..VOICE_HEADER_LEN + 3]).unwrap();
        assert_eq!(pkt.id, 42);
        assert_eq!(pkt.counter, 7_000_000_000);
        assert_eq!(pkt.payload, b"abc");
    }

    #[test]
    fn rejects_bad_magic() {
        let buf = [0u8; 32];
        assert!(parse_voice_packet(&buf).is_none());
    }

    /// Un serveur d'une version antérieure n'envoie pas encore son
    /// identité : le client doit quand même pouvoir lire son Welcome.
    #[test]
    fn welcome_without_server_info_still_parses() {
        let json = r#"{"type":"welcome","user_id":1,"voice_token":2,"udp_port":0,
                       "voice_key":"ab","is_admin":true,"channels":[]}"#;
        let msg: ServerMsg = serde_json::from_str(json).unwrap();
        let ServerMsg::Welcome { server, is_admin, portes, .. } = msg else {
            panic!("ce n'est pas un Welcome");
        };
        assert!(is_admin);
        assert_eq!(server, ServerInfo::default());
        assert!(!portes, "un serveur d'avant les portes ne les sert pas");
    }

    /// Ce protocole n'a pas de champ de version : la compatibilité repose
    /// entièrement sur `#[serde(default)]`. C'est donc *le* test à ne pas
    /// laisser tomber — un client resté sur une version antérieure doit
    /// continuer à se faire comprendre.
    #[test]
    fn messages_from_an_older_client_still_parse() {
        // `Kick` sans motif.
        let msg: ClientMsg = serde_json::from_str(r#"{"type":"kick","user_id":7}"#).unwrap();
        let ClientMsg::Kick { user_id, reason } = msg else { panic!("ce n'est pas un Kick") };
        assert_eq!(user_id, 7);
        assert!(reason.is_empty());

        // `AdminCreateInvite` était une variante sans champ : sa valeur par
        // défaut doit rester l'ancien comportement, un code à usage unique
        // et sans expiration — surtout pas un lien permanent par accident.
        let msg: ClientMsg = serde_json::from_str(r#"{"type":"admin_create_invite"}"#).unwrap();
        let ClientMsg::AdminCreateInvite { uses, label, ttl_secs } = msg else {
            panic!("ce n'est pas un AdminCreateInvite")
        };
        assert_eq!(uses, Some(1));
        assert!(label.is_empty());
        assert_eq!(ttl_secs, 0);
    }

    /// Le cas qui décide d'un déploiement échelonné : un client **resté en
    /// arrière** doit continuer à comprendre un serveur à jour. Tout le
    /// monde ne met pas à jour le même jour.
    ///
    /// Les formes d'avant sont reconstituées ici, puisque le code courant ne
    /// les porte plus : `Kicked` était une variante sans champ, et
    /// `InviteInfo.uses_left` un entier nu.
    #[test]
    fn an_older_client_still_understands_a_newer_server() {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum OldServerMsg {
            Kicked,
            AdminInfo { invites: Vec<OldInviteInfo> },
        }
        #[derive(Deserialize)]
        struct OldInviteInfo {
            code: String,
            uses_left: u32,
        }

        // Le serveur envoie désormais un motif : l'ancienne variante sans
        // champ doit l'ignorer, pas échouer.
        let json = serde_json::to_string(&ServerMsg::Kicked { reason: "spam".into() }).unwrap();
        assert!(matches!(
            serde_json::from_str::<OldServerMsg>(&json).unwrap(),
            OldServerMsg::Kicked
        ));

        // Une invitation à usages limités reste lisible par l'ancien entier.
        let bounded = ServerMsg::AdminInfo {
            users: Vec::new(),
            invites: vec![InviteInfo {
                code: "ki-abc".into(),
                uses_left: Some(3),
                uses: 1,
                label: "tournoi".into(),
                created_by: "chef".into(),
                created_at: 42,
                expires_at: None,
                revoked: false,
            }],
        };
        let json = serde_json::to_string(&bounded).unwrap();
        let OldServerMsg::AdminInfo { invites } = serde_json::from_str(&json).unwrap() else {
            panic!("ce n'est pas un AdminInfo")
        };
        assert_eq!(invites[0].code, "ki-abc");
        assert_eq!(invites[0].uses_left, 3);

        // En revanche, un lien **permanent** sérialise `uses_left: null`, que
        // l'ancien `u32` ne sait pas lire : son panneau d'administration
        // n'affichera pas la liste. Limite connue et bornée — le reste de la
        // session, chat et vocal compris, n'en dépend pas.
        let permanent = ServerMsg::AdminInfo {
            users: Vec::new(),
            invites: vec![InviteInfo {
                code: "ki-perm".into(),
                uses_left: None,
                uses: 0,
                label: String::new(),
                created_by: String::new(),
                created_at: 0,
                expires_at: None,
                revoked: false,
            }],
        };
        let json = serde_json::to_string(&permanent).unwrap();
        assert!(serde_json::from_str::<OldServerMsg>(&json).is_err());
    }

    /// `ADMINISTRATOR` accorde toute permission, mais le rang reste une
    /// affaire distincte : c'est ce qui empêche un second administrateur de
    /// bannir le propriétaire.
    #[test]
    fn administrator_grants_every_permission() {
        assert!(perm::has(perm::ADMINISTRATOR, perm::BAN));
        assert!(perm::has(perm::ADMINISTRATOR, perm::MANAGE_ROLES | perm::KICK));
        // Une permission future, inconnue d'aujourd'hui, est couverte aussi.
        assert!(perm::has(perm::ADMINISTRATOR, 1 << 42));

        assert!(!perm::has(perm::DEFAULT, perm::BAN));
        assert!(perm::has(perm::DEFAULT, perm::SEND_MESSAGE));
        // Exiger deux permissions demande bien de les avoir toutes les deux.
        assert!(!perm::has(perm::KICK, perm::KICK | perm::BAN));
        assert!(perm::has(perm::KICK | perm::BAN, perm::KICK | perm::BAN));
    }

    /// Aucun bit ne doit être attribué deux fois : une collision donnerait
    /// silencieusement une permission qu'on n'a pas accordée.
    #[test]
    fn permission_bits_do_not_collide() {
        let mut seen = 0u64;
        for (bit, name, _) in perm::ALL {
            assert_eq!(bit.count_ones(), 1, "{name} n'est pas un bit unique");
            assert_eq!(seen & bit, 0, "{name} réutilise un bit déjà pris");
            seen |= bit;
        }
    }

    /// Les rôles et les salons restreints n'existent pas pour un serveur
    /// d'une version antérieure : le client doit rester utilisable.
    #[test]
    fn members_and_channels_without_roles_still_parse() {
        let member: Member = serde_json::from_str(
            r#"{"user_id":1,"username":"alice","speaking":false}"#,
        )
        .unwrap();
        assert!(member.roles.is_empty());
        assert_eq!(member.color, None);
        assert_eq!(member.rank, 0);

        let channel: ChannelInfo =
            serde_json::from_str(r#"{"id":1,"name":"général"}"#).unwrap();
        assert_eq!(channel.kind, ChannelKind::Text);
        assert_eq!(channel.position, 0);
        assert!(!channel.locked);
        assert_eq!(channel.allowed_roles, None);

        // `JoinVoice` sans mot de passe : la forme qu'envoient les clients
        // d'avant les salons verrouillés.
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"join_voice","channel":101}"#).unwrap();
        let ClientMsg::JoinVoice { password, .. } = msg else { panic!("pas un JoinVoice") };
        assert_eq!(password, None);
    }

    /// La pagination se distingue du chargement initial : `History` remplace
    /// le fil affiché, `HistoryPage` s'ajoute au-dessus. Confondre les deux
    /// effacerait la conversation à chaque remontée.
    #[test]
    fn history_page_carries_whether_more_remains() {
        let page = ServerMsg::HistoryPage { messages: Vec::new(), more: true, channel: 7 };
        let json = serde_json::to_string(&page).unwrap();
        assert!(json.contains("\"type\":\"history_page\""));

        // `more` absent (serveur d'une version antérieure) vaut « plus rien
        // à charger » : le client cesse de demander au lieu de boucler. Et
        // `channel` absent vaut 0, que le client traite comme « je ne peux pas
        // vérifier » plutôt que comme le salon numéro zéro — qui n'existe pas.
        let msg: ServerMsg =
            serde_json::from_str(r#"{"type":"history_page","messages":[]}"#).unwrap();
        let ServerMsg::HistoryPage { more, channel, .. } = msg else {
            panic!("pas un HistoryPage")
        };
        assert!(!more);
        assert_eq!(channel, 0);

        // Symétriquement, un client antérieur n'envoie pas le salon dans sa
        // demande : le serveur fait de toute façon autorité avec le salon
        // réellement ouvert, ce champ n'est qu'un écho.
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"history_before","before_ts":42,"limit":50}"#)
                .unwrap();
        let ClientMsg::HistoryBefore { channel, limit, .. } = msg else {
            panic!("pas un HistoryBefore")
        };
        assert_eq!(channel, 0);
        assert_eq!(limit, 50);
    }

    /// Les non-lus : un `Chat` d'avant se relit sans salon, un `Chat`
    /// d'aujourd'hui le porte, et les trois messages nouveaux font
    /// l'aller-retour — y compris un `NonLus` réduit à l'essentiel.
    #[test]
    fn les_non_lus_font_l_aller_retour_et_un_chat_d_avant_se_relit() {
        // Un serveur antérieur ne dit pas le salon : 0, « fais confiance ».
        let ancien = r#"{"type":"chat","user_id":1,"username":"kevin","text":"yo","ts":42}"#;
        let ServerMsg::Chat { channel, ts, .. } = serde_json::from_str(ancien).unwrap() else {
            panic!("pas un Chat")
        };
        assert_eq!((channel, ts), (0, 42));

        let chat = ServerMsg::Chat {
            user_id: 1,
            username: "kevin".into(),
            text: "yo".into(),
            ts: 42,
            reply_to: None,
            channel: 7,
        };
        let json = serde_json::to_string(&chat).unwrap();
        assert!(json.contains("\"channel\":7"));

        let nouveau = ServerMsg::Nouveau {
            channel: 7,
            user_id: 1,
            username: "kevin".into(),
            text: "@léa tu viens ?".into(),
            ts: 43,
        };
        let json = serde_json::to_string(&nouveau).unwrap();
        assert!(json.contains("\"type\":\"nouveau\""));
        let ServerMsg::Nouveau { channel, text, .. } = serde_json::from_str(&json).unwrap() else {
            panic!("pas un Nouveau")
        };
        assert_eq!(channel, 7);
        assert_eq!(text, "@léa tu viens ?");

        let salons = vec![
            NonLuSalon { channel: 7, dernier_ts: 40, non_lus: 3, mention: true },
            NonLuSalon { channel: 8, dernier_ts: 0, non_lus: 0, mention: false },
        ];
        let json = serde_json::to_string(&ServerMsg::NonLus { salons: salons.clone() }).unwrap();
        let ServerMsg::NonLus { salons: relus } = serde_json::from_str(&json).unwrap() else {
            panic!("pas un NonLus")
        };
        assert_eq!(relus, salons);
        // Les champs d'un salon ont tous une valeur par défaut : un serveur
        // qui en ajoutera d'autres restera lisible, et l'inverse aussi.
        let minimal: NonLuSalon = serde_json::from_str(r#"{"channel":9}"#).unwrap();
        assert_eq!(minimal, NonLuSalon { channel: 9, ..Default::default() });

        let json = serde_json::to_string(&ClientMsg::Lu { channel: 7, ts: 43 }).unwrap();
        assert!(json.contains("\"type\":\"lu\""));
        let ClientMsg::Lu { channel, ts } = serde_json::from_str(&json).unwrap() else {
            panic!("pas un Lu")
        };
        assert_eq!((channel, ts), (7, 43));
    }

    /// Le poke : les deux demandes et les deux réponses font l'aller-retour
    /// sous les noms `snake_case` attendus, et un refus garde sa cible.
    #[test]
    fn le_poke_fait_l_aller_retour_avec_sa_cible() {
        let json = serde_json::to_string(&ClientMsg::Poke { user_id: 12 }).unwrap();
        assert_eq!(json, r#"{"type":"poke","user_id":12}"#);
        let ClientMsg::Poke { user_id } = serde_json::from_str(&json).unwrap() else {
            panic!("pas un Poke")
        };
        assert_eq!(user_id, 12);

        let json = serde_json::to_string(&ClientMsg::AccepterPokes { accepter: false }).unwrap();
        assert_eq!(json, r#"{"type":"accepter_pokes","accepter":false}"#);
        let ClientMsg::AccepterPokes { accepter } = serde_json::from_str(&json).unwrap() else {
            panic!("pas un AccepterPokes")
        };
        assert!(!accepter);

        let json = serde_json::to_string(&ServerMsg::Poke { user_id: 3, username: "nono".into() }).unwrap();
        assert!(json.contains("\"type\":\"poke\""));
        let ServerMsg::Poke { user_id, username } = serde_json::from_str(&json).unwrap() else {
            panic!("pas un Poke")
        };
        assert_eq!((user_id, username.as_str()), (3, "nono"));

        let refus = ServerMsg::PokeRefuse { user_id: 12, message: "Nono est en vocal".into() };
        let json = serde_json::to_string(&refus).unwrap();
        assert!(json.contains("\"type\":\"poke_refuse\""));
        let ServerMsg::PokeRefuse { user_id, message } = serde_json::from_str(&json).unwrap() else {
            panic!("pas un PokeRefuse")
        };
        assert_eq!((user_id, message.as_str()), (12, "Nono est en vocal"));
    }

    /// Et dans l'autre sens : un serveur antérieur ne connaît ni le motif
    /// d'expulsion, ni le détail des bannissements et des invitations.
    #[test]
    fn messages_from_an_older_server_still_parse() {
        let msg: ServerMsg = serde_json::from_str(r#"{"type":"kicked"}"#).unwrap();
        let ServerMsg::Kicked { reason } = msg else { panic!("ce n'est pas un Kicked") };
        assert!(reason.is_empty());

        // `uses_left` était un entier nu ; il devient `Option<u32>`, où
        // `None` signifie « illimité ». Un entier doit donc rester borné.
        let json = r#"{"type":"admin_info","users":[
              {"username":"alice","user_id":1,"admin":true,"banned":false,"online":true}],
            "invites":[{"code":"ki-abc","uses_left":3}]}"#;
        let msg: ServerMsg = serde_json::from_str(json).unwrap();
        let ServerMsg::AdminInfo { users, invites } = msg else {
            panic!("ce n'est pas un AdminInfo")
        };
        assert_eq!(users[0].ban_reason, "");
        assert_eq!(users[0].ban_until, None);
        assert_eq!(invites[0].uses_left, Some(3));
        assert_eq!(invites[0].uses, 0);
        assert!(!invites[0].revoked);
    }

    #[test]
    fn icon_change_defaults_to_leaving_the_logo_alone() {
        // Un AdminSetServerInfo qui ne parle que du nom ne doit pas
        // effacer le logo par accident.
        let json = r#"{"type":"admin_set_server_info","name":"Chez Kévin"}"#;
        let msg: ClientMsg = serde_json::from_str(json).unwrap();
        let ClientMsg::AdminSetServerInfo { name, icon } = msg else {
            panic!("mauvais message");
        };
        assert_eq!(name.as_deref(), Some("Chez Kévin"));
        assert!(matches!(icon, IconChange::Keep));
    }

    /// L'adresse web publique : ce qu'un admin tape devient une adresse que
    /// le navigateur ouvre — avec son `https://` —, et ce qui ne se lit pas
    /// est refusé, des deux côtés.
    #[test]
    fn une_adresse_web_se_normalise() {
        let n = normaliser_adresse_web;
        assert_eq!(n("ts.baws.fun:8080").unwrap(), "https://ts.baws.fun:8080");
        assert_eq!(n("  https://ts.baws.fun:8080/  ").unwrap(), "https://ts.baws.fun:8080");
        assert_eq!(n("HTTPS://TS.Baws.Fun").unwrap(), "https://ts.baws.fun");
        assert_eq!(n("http://192.168.2.36:8080").unwrap(), "http://192.168.2.36:8080");
        assert_eq!(n("https://exemple.fr/ki/").unwrap(), "https://exemple.fr/ki");
        assert_eq!(n("[::1]:8080").unwrap(), "https://[::1]:8080");
        assert_eq!(n("").unwrap(), "");
        assert_eq!(n("   ").unwrap(), "");
        for faux in [
            "ftp://ts.baws.fun",
            "https://",
            "ts.baws.fun:",
            "ts.baws.fun:0",
            "ts.baws.fun:99999",
            "ts.baws.fun:+80",
            "ts baws.fun",
            "https://moi@ts.baws.fun",
            "https://ts.baws.fun/?x=1",
            "https://ts.baws.fun#haut",
            "https://[::1",
        ] {
            assert!(n(faux).is_err(), "{faux} devrait être refusée");
        }
        assert!(n(&"a".repeat(MAX_ADRESSE_WEB + 1)).is_err());
    }

    /// Les champs de 0.1.45 sont facultatifs : l'identité d'un serveur
    /// d'avant se lit sans adresse, l'état de sa porte sans lien — et le
    /// message d'admin a son nom.
    #[test]
    fn l_adresse_web_et_le_lien_sont_facultatifs() {
        let info: ServerInfo = serde_json::from_str(r#"{"name":"BAWS"}"#).unwrap();
        assert!(info.adresse_web.is_empty());
        let json = serde_json::to_string(&ServerInfo::default()).unwrap();
        assert!(!json.contains("adresse_web"), "vide, le champ ne voyage pas : {json}");
        let etat: ServerMsg =
            serde_json::from_str(r#"{"type":"porte_etat","slug":"valo","salon":4,"expire_le":5}"#).unwrap();
        let ServerMsg::PorteEtat { url, .. } = etat else { panic!("{etat:?}") };
        assert!(url.is_empty());
        let msg = ClientMsg::AdminSetAdresseWeb { adresse: "https://ts.baws.fun:8080".into() };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"admin_set_adresse_web""#), "{json}");
        let relu: ClientMsg = serde_json::from_str(&json).unwrap();
        let ClientMsg::AdminSetAdresseWeb { adresse } = relu else { panic!("{relu:?}") };
        assert_eq!(adresse, "https://ts.baws.fun:8080");
    }

    /// Les champs des deux qualités (0.1.46) sont facultatifs : les
    /// messages d'un pair d'avant se lisent comme avant.
    #[test]
    fn les_deux_qualites_sont_facultatives() {
        let w: ClientMsg = serde_json::from_str(r#"{"type":"watch","stream_id":3}"#).unwrap();
        assert!(matches!(w, ClientMsg::Watch { stream_id: 3, couches: false }));
        let k: ServerMsg = serde_json::from_str(r#"{"type":"keyframe_needed","stream_id":3}"#).unwrap();
        assert!(matches!(k, ServerMsg::KeyframeNeeded { stream_id: 3, basse: false }));
        let b: ServerMsg = serde_json::from_str(r#"{"type":"stream_budget","stream_id":3,"kbps":2500}"#).unwrap();
        let ServerMsg::StreamBudget { kbps, basse, montant, .. } = b else { panic!("{b:?}") };
        assert_eq!((kbps, basse, montant), (2500, None, false));
        let b = ServerMsg::StreamBudget { stream_id: 3, kbps: 6000, basse: Some(1500), montant: false };
        let relu: ServerMsg = serde_json::from_str(&serde_json::to_string(&b).unwrap()).unwrap();
        assert!(matches!(relu, ServerMsg::StreamBudget { basse: Some(1500), .. }));
    }

    #[test]
    fn icon_change_roundtrip() {
        for change in [
            IconChange::Keep,
            IconChange::Clear,
            IconChange::Set { data: "AAAA".into() },
        ] {
            let json = serde_json::to_string(&change).unwrap();
            let back: IconChange = serde_json::from_str(&json).unwrap();
            assert_eq!(
                std::mem::discriminant(&change),
                std::mem::discriminant(&back)
            );
        }
    }

    /// Assemble un bloc PNG : longueur, type, données, CRC.
    /// Le CRC n'est pas vérifié par `check_png` — un attaquant saurait le
    /// calculer, il n'apporterait aucune sécurité.
    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&[0, 0, 0, 0]);
        out
    }

    fn ihdr(width: u32, height: u32) -> Vec<u8> {
        let mut data = width.to_be_bytes().to_vec();
        data.extend_from_slice(&height.to_be_bytes());
        data.extend_from_slice(&[8, 6, 0, 0, 0]); // profondeur, couleur, ...
        chunk(b"IHDR", &data)
    }

    /// PNG structurellement complet, sans pixels réels : `check_png` ne
    /// décode rien, seule la charpente compte.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        out.extend(ihdr(width, height));
        out.extend(chunk(b"IDAT", &[0x78, 0x9c, 0x00]));
        out.extend(chunk(b"IEND", &[]));
        out
    }

    #[test]
    fn a_decompression_bomb_is_refused_before_decoding() {
        // Quelques dizaines d'octets qui réclameraient ~3,6 Go au décodeur
        // de chaque client du salon.
        let refusal = check_png(&png(30_000, 30_000)).unwrap_err();
        assert!(refusal.contains("30000"), "message peu clair : {refusal}");

        assert!(check_png(&png(64, 64)).is_ok());
        // La limite exacte passe, un pixel de plus non.
        assert!(check_png(&png(MAX_THUMBNAIL_PX, MAX_THUMBNAIL_PX)).is_ok());
        assert!(check_png(&png(MAX_THUMBNAIL_PX + 1, 64)).is_err());
        assert!(check_png(&png(0, 64)).is_err());
    }

    /// Le cœur de la question : une vignette valide ne doit pas pouvoir
    /// servir de véhicule à des octets arbitraires.
    #[test]
    fn a_thumbnail_cannot_smuggle_arbitrary_bytes() {
        let payload = b"MZ charge utile arbitraire".repeat(200);

        // 1. Dans un bloc de métadonnées.
        let mut with_text = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        with_text.extend(ihdr(64, 64));
        with_text.extend(chunk(b"tEXt", &payload));
        with_text.extend(chunk(b"IDAT", &[0x78, 0x9c, 0x00]));
        with_text.extend(chunk(b"IEND", &[]));
        let refusal = check_png(&with_text).unwrap_err();
        assert!(refusal.contains("tEXt"), "message peu clair : {refusal}");

        // 2. Collée après la fin de l'image (fichier « polyglotte »).
        let mut appended = png(64, 64);
        appended.extend_from_slice(&payload);
        assert!(check_png(&appended).is_err());

        // 3. Sans marqueur de fin, pour que le reste passe inaperçu.
        let mut unterminated = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        unterminated.extend(ihdr(64, 64));
        unterminated.extend(chunk(b"IDAT", &payload));
        assert!(check_png(&unterminated).is_err());
    }

    #[test]
    fn only_real_png_structures_are_accepted() {
        assert!(check_png(b"").is_err());
        assert!(check_png(&[b'M', b'Z', 0x90, 0x00, 0x03]).is_err()); // .exe
        assert!(check_png(&[0xff, 0xd8, 0xff, 0xe0]).is_err()); // JPEG
        // Signature correcte, mais premier bloc qui n'est pas IHDR.
        let mut wrong_first = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        wrong_first.extend(chunk(b"IDAT", &[0]));
        assert!(check_png(&wrong_first).is_err());
        // Bloc annonçant plus de données qu'il n'en reste.
        let mut truncated = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        truncated.extend(ihdr(64, 64));
        truncated.extend_from_slice(&999_u32.to_be_bytes());
        truncated.extend_from_slice(b"IDAT");
        assert!(check_png(&truncated).is_err());
        // Longueur démesurée : pas de débordement de calcul.
        let mut huge = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        huge.extend(ihdr(64, 64));
        huge.extend_from_slice(&u32::MAX.to_be_bytes());
        huge.extend_from_slice(b"IDAT");
        assert!(check_png(&huge).is_err());
    }

    #[test]
    fn thumbnails_are_checked_through_their_base64() {
        use base64::Engine as _;
        let encode = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);

        assert!(check_thumbnail(&encode(&png(64, 64))).is_ok());
        assert!(check_thumbnail(&encode(&png(30_000, 30_000))).is_err());
        assert!(check_thumbnail("pas du base64 !").is_err());
        // Trop lourd : refusé sans même être décodé.
        assert!(check_thumbnail(&"A".repeat(MAX_SERVER_ICON + 1)).is_err());
    }

    #[test]
    fn chat_text_is_bounded() {
        // Sans borne, ce message serait relayé à tout le salon puis gardé
        // en mémoire dans l'historique.
        assert!(clean_chat(&"a".repeat(MAX_CHAT_TEXT + 1)).is_err());
        assert!(clean_chat(&"a".repeat(MAX_CHAT_TEXT)).is_ok());

        // Un message vide, ou réduit à des blancs, n'a rien à faire là.
        assert!(clean_chat("").is_err());
        assert!(clean_chat("   \n\t ").is_err());

        // Le compte est en caractères, pas en octets : un message accentué
        // ne doit pas être refusé pour sa taille encodée.
        let accented = "é".repeat(MAX_CHAT_TEXT);
        assert!(accented.len() > MAX_CHAT_TEXT, "prémisse du test");
        assert!(clean_chat(&accented).is_ok());
    }

    #[test]
    fn chat_text_loses_its_dangerous_characters() {
        // Commande bidirectionnelle : elle inverse l'affichage du texte qui
        // suit, de quoi maquiller un lien ou imiter quelqu'un.
        let spoof = clean_chat("regarde \u{202e}gnp.exe").unwrap();
        assert!(!spoof.contains('\u{202e}'), "commande bidi conservée : {spoof:?}");
        assert!(spoof.contains("gnp.exe"));

        // Caractères de contrôle retirés, sauts de ligne et tabulations
        // gardés — ils font partie d'un message normal.
        let cleaned = clean_chat("salut\u{0}\u{7}\u{1b}[31m rouge\nligne\tsuite").unwrap();
        assert_eq!(cleaned, "salut[31m rouge\nligne\tsuite");

        // Les enfilades de lignes vides sont ramenées à quelques-unes :
        // trois caractères ne doivent pas occuper tout l'écran de chacun.
        let flood = clean_chat(&format!("haut{}bas", "\n".repeat(400))).unwrap();
        assert_eq!(flood.matches('\n').count(), MAX_BLANK_LINES);
    }

    #[test]
    fn displayed_text_is_repaired_rather_than_refused() {
        // À la réception on n'a pas le luxe de refuser : on affiche au mieux.
        let long = safe_display(&"a".repeat(500), 100);
        assert_eq!(long.chars().count(), 101); // 100 + le caractère de coupe
        assert!(long.ends_with('…'));

        assert_eq!(safe_display("bonjour\u{202e}", 100), "bonjour");
        assert_eq!(safe_display("", 100), "");
        // Coupe sur les caractères, jamais au milieu d'un caractère encodé.
        assert_eq!(safe_display("ééééé", 3), "ééé…");
    }

    #[test]
    fn avatar_hash_tracks_content() {
        // Pas de photo, pas d'empreinte.
        assert!(avatar_hash(None).is_none());
        // Même contenu, même empreinte : le cache du client tient.
        assert_eq!(avatar_hash(Some("AAAA")), avatar_hash(Some("AAAA")));
        // Contenu différent, empreinte différente : le client redemande.
        assert_ne!(avatar_hash(Some("AAAA")), avatar_hash(Some("AAAB")));
        // Longueur fixe, lisible dans un JSON.
        let hash = avatar_hash(Some("vignette")).unwrap();
        assert_eq!(hash.len(), 16);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn members_of_older_servers_have_no_avatar() {
        let json = r#"{"user_id":1,"username":"redik","speaking":false}"#;
        let member: Member = serde_json::from_str(json).unwrap();
        assert!(member.avatar.is_none());
        assert!(!member.admin);
    }

    #[test]
    fn set_avatar_without_op_leaves_the_photo_alone() {
        let msg: ClientMsg = serde_json::from_str(r#"{"type":"set_avatar"}"#).unwrap();
        let ClientMsg::SetAvatar { avatar } = msg else { panic!("mauvais message") };
        assert!(matches!(avatar, IconChange::Keep));
    }

    #[test]
    fn hex_roundtrip() {
        let bytes = [0u8, 1, 0xab, 0xff, 42];
        let hex = hex_encode(&bytes);
        assert_eq!(hex, "0001abff2a");
        assert_eq!(hex_decode(&hex).unwrap(), bytes);
        assert!(hex_decode("xyz").is_none());
        assert!(hex_decode("abc").is_none());
    }
}
