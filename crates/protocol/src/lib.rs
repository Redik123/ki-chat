//! Protocole partagé ki-chat : messages de contrôle (JSON, une ligne par
//! message sur le flux QUIC fiable) et format des paquets voix (datagrammes
//! QUIC, binaire).

use serde::{Deserialize, Serialize};

pub type UserId = u64;
pub type ChannelId = u32;

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
    JoinVoice { channel: ChannelId },
    /// Sortir du vocal.
    LeaveVoice,
    /// Message texte dans le salon courant.
    Chat { text: String },
    /// Demander l'historique du salon courant.
    History { limit: u32 },
    /// Remonter le fil : les messages **antérieurs** à `before_ts`.
    ///
    /// Sans ça, seuls les derniers messages sont atteignables — tout ce que
    /// contient le fichier du salon au-delà reste invisible, alors même
    /// qu'il est conservé.
    HistoryBefore {
        /// Horodatage du plus ancien message déjà affiché (ms Unix).
        before_ts: u64,
        limit: u32,
    },
    /// Le client annonce qu'il entre/sort du vocal du salon courant.
    VoiceState { speaking: bool },
    /// Expulse un utilisateur du serveur (admin uniquement). Il peut se
    /// reconnecter aussitôt : pour l'en empêcher, voir `AdminBan`.
    Kick {
        user_id: UserId,
        #[serde(default)]
        reason: String,
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
        /// Vrai si ce compte a le rôle admin.
        #[serde(default)]
        is_admin: bool,
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
    },
    /// Historique demandé.
    History { messages: Vec<ChatRecord> },
    /// Page d'historique plus ancienne, à **ajouter au-dessus** de ce qui est
    /// déjà affiché — au contraire de `History`, qui remplace tout.
    HistoryPage {
        messages: Vec<ChatRecord>,
        /// Faux quand on a atteint le début du salon : le client cesse alors
        /// de redemander à chaque défilement.
        #[serde(default)]
        more: bool,
    },
    /// État vocal d'un membre du salon.
    VoiceState { user_id: UserId, speaking: bool },
    /// Liste des membres présents dans le salon rejoint.
    Members { members: Vec<Member> },
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
    let mut count = 0usize;
    for c in text.chars().filter(|c| !is_dangerous(*c)) {
        if count == max_chars {
            out.push('…');
            break;
        }
        out.push(c);
        count += 1;
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Member {
    pub user_id: UserId,
    pub username: String,
    pub speaking: bool,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRecord {
    pub user_id: UserId,
    pub username: String,
    pub text: String,
    pub ts: u64,
}

/// --- Protocole voix (UDP), version 2 ---
///
/// Chaque paquet voix a un en-tête binaire fixe suivi de la trame Opus
/// chiffrée (XChaCha20-Poly1305). Petit-boutiste (little-endian) partout.
///
/// Client -> serveur :
///   [0..2]  magic  "KV"
///   [2]     version (2)
///   [3..11] voice_token (u64) — remis par le serveur dans Welcome
///   [11..19] compteur (u64) — strictement croissant, sert de nonce
///   [19..]  trame Opus chiffrée (+16 octets de tag Poly1305)
///
/// Serveur -> clients (mode SFU : le serveur relaie sans déchiffrer) :
///   identique, mais [3..11] = user_id de l'émetteur.
///
/// Le nonce XChaCha20 (24 octets) est dérivé de (user_id, compteur) : il est
/// donc unique par clé tant que la clé change à chaque démarrage du serveur.
/// Un paquet sans charge (en-tête seul) est un keepalive : il enregistre
/// l'adresse UDP du client auprès du serveur et n'est jamais relayé.
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

/// --- Petits utilitaires hex (clé voix dans Welcome) ---

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
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

    /// La pagination se distingue du chargement initial : `History` remplace
    /// le fil affiché, `HistoryPage` s'ajoute au-dessus. Confondre les deux
    /// effacerait la conversation à chaque remontée.
    #[test]
    fn history_page_carries_whether_more_remains() {
        let page = ServerMsg::HistoryPage { messages: Vec::new(), more: true };
        let json = serde_json::to_string(&page).unwrap();
        assert!(json.contains("\"type\":\"history_page\""));

        // `more` absent (serveur d'une version antérieure) vaut « plus rien
        // à charger » : le client cesse de demander au lieu de boucler.
        let msg: ServerMsg =
            serde_json::from_str(r#"{"type":"history_page","messages":[]}"#).unwrap();
        let ServerMsg::HistoryPage { more, .. } = msg else { panic!("pas un HistoryPage") };
        assert!(!more);
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
