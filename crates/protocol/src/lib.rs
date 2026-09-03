//! Protocole partagé ki-chat : messages de contrôle (JSON, une ligne par
//! message sur le flux QUIC fiable) et format des paquets voix (datagrammes
//! QUIC, binaire).

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
    /// Demander l'historique du salon courant.
    History { limit: u32 },
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
    },
    /// Arrêter son partage d'écran.
    StreamStop,
    /// Le streamer annonce un changement (dimensions, débit) : le serveur le
    /// rediffuse au salon en StreamMetaChanged.
    StreamMetaUpdate { meta: StreamMeta },
    /// Regarder le stream d'un membre de son salon vocal.
    Watch { stream_id: u32 },
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
    /// Message texte relayé.
    Chat {
        user_id: UserId,
        username: String,
        text: String,
        /// Millisecondes depuis l'époque Unix.
        ts: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<ReplyRef>,
    },
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
    /// d'une trame clé. Cadence bornée par le serveur (≤ 1 / 500 ms).
    KeyframeNeeded { stream_id: u32 },
    /// Les caractéristiques d'un stream ont changé (dimensions, débit).
    StreamMetaChanged { stream_id: u32, meta: StreamMeta },
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
    /// Réponse au Ping.
    Pong,
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
///   [3]      drapeaux — bit 0 : trame clé (IDR)
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

/// Domaines de nonce sous une clé de stream.
pub const MEDIA_DOMAIN_VIDEO: u8 = 1;
pub const MEDIA_DOMAIN_GAME_AUDIO: u8 = 2;

/// En-tête d'une trame média, tel qu'il circule en clair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaHeader {
    pub idr: bool,
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
    buf[3] = if h.idr { MEDIA_FLAG_IDR } else { 0 };
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
            &MediaHeader { idr: true, stream_id: 7, seq: 1, pts_us: 0, group_id: 0, width: 1, height: 1 },
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
}

#[cfg(test)]
mod media_tests {
    use super::*;

    #[test]
    fn en_tete_media_aller_retour() {
        let h = MediaHeader {
            idr: true,
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
    if !s.len().is_multiple_of(2) {
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
        };
        let ligne = serde_json::to_string(&nouveau).unwrap();
        let relu: ChatRecord = serde_json::from_str(&ligne).unwrap();
        assert_eq!(relu.reply_to, nouveau.reply_to);
        assert_eq!(relu.reactions, nouveau.reactions);
        // Et rien de tout ça n'alourdit un message ordinaire.
        let simple = serde_json::to_string(&ChatRecord { ts: 1, ..Default::default() }).unwrap();
        assert!(!simple.contains("reply_to") && !simple.contains("reactions"));
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
        let ServerMsg::Welcome { server, is_admin, .. } = msg else {
            panic!("ce n'est pas un Welcome");
        };
        assert!(is_admin);
        assert_eq!(server, ServerInfo::default());
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
