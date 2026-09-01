//! ki-chat : client graphique (egui) — chat texte + vocal, orienté gaming.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod appicon;
mod icons;
mod images;
mod markup;
mod net;
mod perf;
mod photos;
mod ptt;
mod secours;
mod secret;
mod servers;
mod sfxgen;
mod theme;
mod ui;
mod update;
mod veille;

/// Sous `--features mesures`, toutes les allocations du processus passent par
/// un compteur. C'est ce qui rend vérifiable la cible « ~0 allocation par
/// image » de P3 : aujourd'hui le rendu recopie l'état complet (membres,
/// rôles, salons, journal) vingt fois par seconde, et l'on ne peut pas
/// corriger ce qu'on ne compte pas.
#[cfg(feature = "mesures")]
#[global_allocator]
static COMPTEUR: perf::alloc::Compteur = perf::alloc::Compteur;

use std::collections::HashMap;

use chrono::{Datelike, TimeZone};
use eframe::egui::{self, Color32, RichText, Sense, Vec2};
use icons::Icon;
use ki_protocol::{
    AccountInfo, AuditRecord, ChannelId, ChannelInfo, ChannelKind, ChatRecord, ClientMsg,
    IconChange, InviteInfo, Member, ServerInfo, ServerMsg, UserId,
};
use ptt::PttKey;
use theme::{color_for, ACCENT, DANGER, INFO, SPEAK, TEXT, TEXT_DIM, TEXT_FAINT, WARN};
use ui::Tone;

#[derive(Clone, Copy, PartialEq, Eq)]
enum MicMode {
    Open,
    Ptt,
    /// Activation vocale : émet quand le niveau dépasse le seuil réglé.
    Vad,
}

impl MicMode {
    fn label(self) -> &'static str {
        match self {
            MicMode::Open => "Micro ouvert",
            MicMode::Ptt => "Push-to-talk",
            MicMode::Vad => "Activation vocale",
        }
    }

    fn id(self) -> &'static str {
        match self {
            MicMode::Open => "open",
            MicMode::Ptt => "ptt",
            MicMode::Vad => "vad",
        }
    }
}

/// Débits Opus proposés (bits/s).
const BITRATES: [i32; 6] = [24_000, 32_000, 48_000, 64_000, 96_000, 128_000];

/// Deux messages du même auteur espacés de moins de ça sont regroupés.
const GROUP_WINDOW_MS: u64 = 5 * 60 * 1000;

const SIDEBAR_WIDTH: f32 = 248.0;
const ROSTER_WIDTH: f32 = 210.0;

/// Au-dessus de ce niveau crête, on considère que la personne parle, même si
/// son `VoiceState` ne nous est pas parvenu. Le niveau vient du tampon de
/// gigue et décroît tout seul : l'indicateur s'éteint donc sans message.
const SPEAK_LEVEL: f32 = 0.02;

fn main() -> eframe::Result {
    // Traces sur stderr ET dans un fichier, plus un panic hook : compilée
    // `panic = "abort"` sans console, l'application mourait sans témoin.
    let journal = secours::installer();
    match &journal {
        Some(chemin) => {
            tracing::info!("ki-chat {} démarre — journal : {}", update::current(), chemin.display());
        }
        None => tracing::warn!(
            "ki-chat {} démarre — journal sur disque indisponible, stderr seul",
            update::current()
        ),
    }
    let relances = secours::essais_actuels();
    if relances > 0 {
        tracing::info!("instance relancée automatiquement (tentative {relances})");
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 720.0])
            .with_min_inner_size([900.0, 560.0])
            .with_title("ki-chat")
            .with_icon(std::sync::Arc::new(theme::app_icon())),
        // eframe mémorisait la géométrie et la remettait au lancement — y
        // compris une origine sur un écran secondaire débranché depuis,
        // auquel cas la fenêtre revenait minuscule en haut à gauche.
        //
        // Attention : ce drapeau ne gouverne que la **sauvegarde**. La
        // relecture au démarrage n'est pas conditionnée par lui. C'est donc
        // `save()` qui fait le vrai travail, en vidant la clé « window » ;
        // sans valeur à relire, eframe laisse la position au système. Le
        // drapeau est ici pour qu'eframe ne la réécrive pas derrière nous.
        //
        // Seul l'état « maximisée » est repris, par nos soins (`update`).
        persist_window: false,
        ..Default::default()
    };
    let depart = std::time::Instant::now();
    let outcome = eframe::run_native(
        "ki-chat",
        options,
        Box::new(|cc| Ok(Box::new(KiApp::new(cc)))),
    );
    match &outcome {
        // Une mise à jour installée ne prend effet qu'au prochain lancement :
        // on le déclenche ici, la fenêtre fermée — donc après que les
        // réglages ont été enregistrés et les périphériques audio rendus.
        Ok(()) => update::relaunch_if_requested(),
        // eframe tient toute erreur de rendu pour fatale : un seul
        // `SwapBuffers` raté — pilote réinitialisé sous un jeu, veille,
        // bascule de GPU d'un portable — et sa boucle se termine, fenêtre
        // fermée, en rendant l'erreur ici. Le contexte graphique suivant
        // n'aura rien : on relance, plutôt que de laisser un hoquet d'une
        // image coûter la soirée. Budget borné : une erreur qui revient dès
        // le démarrage n'est pas un hoquet, on rend la main.
        Err(e) => {
            tracing::error!("boucle graphique terminée en erreur : {e}");
            match secours::decision_relance(relances, depart.elapsed()) {
                Some(essais) => secours::relancer(essais),
                None => tracing::error!("l'erreur revient dès le démarrage — relances épuisées"),
            }
        }
    }
    outcome
}

/// Case où le thread du sélecteur de fichier dépose la vignette encodée,
/// ou le message d'erreur si l'image est illisible.
type PickedImage = std::sync::Arc<std::sync::Mutex<Option<Result<String, String>>>>;

/// Onglets de la fenêtre d'administration. Tout tenait auparavant dans une
/// seule colonne déroulante ; avec les bannissements et le journal, elle ne
/// se lisait plus.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AdminTab {
    Server,
    Channels,
    Roles,
    Members,
    Invites,
    Audit,
    Diagnostics,
}

impl AdminTab {
    const ALL: [AdminTab; 7] = [
        AdminTab::Server,
        AdminTab::Channels,
        AdminTab::Roles,
        AdminTab::Members,
        AdminTab::Invites,
        AdminTab::Audit,
        AdminTab::Diagnostics,
    ];

    fn label(self) -> &'static str {
        match self {
            AdminTab::Server => "Serveur",
            AdminTab::Channels => "Salons",
            AdminTab::Roles => "Rôles",
            AdminTab::Members => "Membres",
            AdminTab::Invites => "Invitations",
            AdminTab::Audit => "Journal",
            AdminTab::Diagnostics => "Diagnostics",
        }
    }

    /// Permission qui donne accès à cet onglet. Un onglet hors de portée
    /// est **caché**, pas grisé : un onglet vide n'apprend rien.
    fn needs(self) -> ki_protocol::Perms {
        use ki_protocol::perm::*;
        match self {
            AdminTab::Server => MANAGE_SERVER,
            AdminTab::Channels => MANAGE_CHANNELS,
            AdminTab::Roles => MANAGE_ROLES,
            AdminTab::Members => KICK,
            AdminTab::Invites => CREATE_INVITE,
            AdminTab::Audit => VIEW_AUDIT_LOG,
            // Les journaux techniques des joueurs : réservé au super-admin,
            // pas à quiconque sait expulser quelqu'un.
            AdminTab::Diagnostics => ADMINISTRATOR,
        }
    }
}

/// Rôle en cours d'édition dans l'onglet Rôles.
struct RoleDraft {
    /// `None` = création.
    id: Option<ki_protocol::RoleId>,
    name: String,
    color: egui::Color32,
    colored: bool,
    rank: u16,
    perms: ki_protocol::Perms,
}

/// Salon en cours de création dans l'onglet Salons.
struct ChannelDraft {
    name: String,
    kind: ChannelKind,
    /// Vide = visible par tout le monde.
    allowed_roles: Vec<ki_protocol::RoleId>,
    restricted: bool,
}

/// Une connexion perdue que l'on tente de reprendre toute seule.
///
/// Sans ça, le moindre hoquet — le serveur qui redémarre, la box qui
/// renumérote, un jeu qui sature le lien montant — renvoyait tout le monde à
/// l'écran de connexion, en pleine partie, à retrouver son salon vocal à la
/// main. Trente fois, chaque fois.
struct Reprise {
    /// Tentatives déjà faites. C'est elle qui espace les suivantes.
    essais: u32,
    /// Heure de la prochaine tentative.
    quand: std::time::Instant,
    /// Ce qu'on lisait, et où l'on parlait. Rendus après le `Welcome` : se
    /// reconnecter pour se retrouver ailleurs, en pleine partie, ne vaut
    /// guère mieux que de ne pas se reconnecter.
    salon: Option<ChannelId>,
    vocal: Option<ChannelId>,
}

impl Reprise {
    /// Au-delà, on cesse et l'on rend la main. Avec le plafond ci-dessous,
    /// cela fait une vingtaine de minutes : de quoi couvrir un redémarrage de
    /// serveur, une mise à jour Windows, une box qui reboote. Au-delà, ce
    /// n'est plus un hoquet, et marteler n'y changera rien.
    const MAX: u32 = 40;
    /// Plafond de l'attente entre deux essais.
    const PLAFOND: std::time::Duration = std::time::Duration::from_secs(30);

    /// Attente avant la `n`-ième tentative : doublement, plafonné, puis tiré
    /// **au hasard entre la moitié et la totalité**.
    ///
    /// Le tirage n'est pas un raffinement. Trente clients qui perdent le
    /// serveur au même instant le retrouvent au même instant : sans
    /// dispersion, ils frappent tous ensemble, et chaque salve fait hacher
    /// trente Argon2id d'un coup — volontairement lent — sur une machine qui
    /// vient à peine de redémarrer. Ils échoueraient ensemble et
    /// recommenceraient ensemble, indéfiniment. C'est le troupeau classique,
    /// et il se soigne par le hasard, pas par un délai plus long.
    fn attente(essais: u32, alea: u64) -> std::time::Duration {
        let doublements = essais.saturating_sub(1).min(5);
        let base = std::time::Duration::from_secs(1 << doublements).min(Self::PLAFOND);
        let plein = base.as_millis() as u64;
        let moitie = plein / 2;
        std::time::Duration::from_millis(moitie + alea % (plein - moitie + 1))
    }
}

/// Une source de hasard qui n'a pas besoin d'en être une.
///
/// Disperser des reconnexions ne demande pas de cryptographie : il suffit que
/// deux machines ne tirent pas le même nombre. Les nanosecondes de l'horloge
/// suffisent, et évitent une dépendance de plus dans un client qui en compte
/// déjà assez.
fn alea() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::from(d.subsec_nanos()))
}

/// Salon en cours de modification, dans le panneau d'administration.
///
/// Séparé du brouillon de création : les deux formulaires peuvent être
/// ouverts en même temps, et confondre les deux ferait qu'ouvrir une
/// modification effacerait la création commencée.
struct ChannelEdit {
    id: ki_protocol::ChannelId,
    draft: ChannelDraft,
    /// Le type ne se change pas après coup — on ne convertit pas un salon
    /// textuel plein d'historique en salon vocal. Conservé pour le
    /// renvoyer tel quel au serveur.
    position: u32,
}

/// Verrou vocal en cours de pose.
struct VerrouDraft {
    channel: ki_protocol::ChannelId,
    mot_de_passe: String,
    ttl_secs: u32,
}

/// Durées proposées pour un verrou vocal. Le serveur borne entre une minute
/// et un jour ; on ne propose que ce qui a un sens : le temps d'une partie.
const VOICE_LOCK_DURATIONS: &[(&str, u32)] =
    &[("15 minutes", 900), ("1 heure", 3600), ("4 heures", 14_400), ("1 jour", 86_400)];

/// Durées proposées pour un bannissement. Un menu court vaut mieux qu'un
/// champ libre : personne ne bannit « 137 minutes ».
const BAN_DURATIONS: &[(&str, u64)] = &[
    ("1 heure", 3600),
    ("1 jour", 86_400),
    ("7 jours", 604_800),
    ("30 jours", 2_592_000),
    ("Définitif", 0),
];

/// Saisie du mot de passe d'un salon vocal verrouillé.
struct VoicePrompt {
    channel: ChannelId,
    password: String,
    /// Vrai après un refus : on distingue « il en faut un » de « ce n'est
    /// pas le bon », sinon on ne sait pas si l'on s'est trompé.
    wrong: bool,
}

/// Bannissement en cours de saisie.
struct BanDraft {
    username: String,
    reason: String,
    /// Durée en secondes ; 0 = définitif.
    duration_secs: u64,
}

/// Formulaire d'ajout ou de modification d'un serveur.
struct ServerDraft {
    /// `None` = création.
    id: Option<u64>,
    /// Alias local, pas le nom officiel du serveur.
    name: String,
    address: String,
}

impl ServerDraft {
    fn new() -> Self {
        Self { id: None, name: String::new(), address: String::new() }
    }

    fn edit(server: &servers::Server) -> Self {
        Self {
            id: Some(server.id),
            name: server.name.clone(),
            address: server.address.clone(),
        }
    }
}

struct KiApp {
    /// Sonde les releases GitHub au démarrage et applique la mise à jour si
    /// l'utilisateur l'accepte.
    updater: update::Updater,
    // Carnet de serveurs
    book: Vec<servers::Server>,
    /// Serveur sélectionné dans le lanceur.
    selected: Option<u64>,
    probes: servers::Probes,
    /// Formulaire ouvert par le bouton « + » ou le crayon.
    draft: Option<ServerDraft>,
    /// Textures des logos, montées à la demande. `None` = pas de logo (ou
    /// vignette illisible) : on retient l'échec pour ne pas réessayer.
    icon_textures: HashMap<u64, Option<egui::TextureHandle>>,
    /// Logo de serveur rapporté par le sélecteur de fichier, sur son thread.
    picked_icon: PickedImage,
    /// Photo de profil rapportée par le sélecteur. Case distincte du logo :
    /// les deux fenêtres peuvent être ouvertes en même temps.
    picked_avatar: PickedImage,
    /// Aperçu de vignette, indexé par la vignette elle-même : sans ce cache
    /// on téléverserait une texture à chaque image rendue.
    /// Aperçus de vignettes, **un par emplacement**.
    ///
    /// C'était un seul créneau partagé par « Mon compte » et Admin ▸ Serveur.
    /// Les deux panneaux ouverts en même temps se le disputaient : chaque
    /// image, la clé changeait, donc décodage PNG et téléversement GPU deux
    /// fois par image, indéfiniment.
    preview_icons: HashMap<Apercu, (String, egui::TextureHandle)>,
    /// Aperçus des images partagées dans le fil.
    previews: images::Previews,
    /// Fenêtre maximisée : la seule géométrie qu'on mémorise. Cf. `main`.
    maximized: bool,
    /// Vrai tant que `maximized` n'a pas été réappliqué au démarrage.
    restore_maximized: bool,
    /// Photos de profil montées : user_id -> (empreinte, texture).
    avatars: HashMap<UserId, (String, egui::TextureHandle)>,
    /// Vignettes (octets PNG) prêtes à monter, venues du réseau ou du
    /// cache disque. Le montage demande le contexte egui, d'où l'attente.
    incoming_avatars: HashMap<UserId, (String, Vec<u8>)>,
    /// Photo choisie dans « Mon compte », pas encore envoyée.
    account_avatar: IconChange,

    // Connexion
    /// Adresse du serveur sélectionné — le reste de l'appli s'en sert
    /// (base HTTP des fichiers, code d'invitation à recopier).
    url: String,
    username: String,
    password: String,
    remember_password: bool,
    invite: String,
    /// Le champ « code d'invitation » n'est affiché qu'à la demande.
    show_invite: bool,
    conn: Option<net::NetHandle>,
    connecting: bool,
    /// Début de la tentative en cours, pour la borner (voir
    /// `DELAI_CONNEXION`). `None` = pas de tentative en vol.
    connect_started: Option<std::time::Instant>,
    welcomed: bool,
    error: Option<String>,

    // État serveur
    my_id: Option<UserId>,
    /// Permissions effectives, annoncées par le serveur. L'interface s'en
    /// sert pour ne montrer que ce qui aboutira.
    my_perms: ki_protocol::Perms,
    /// Mon rang : on n'agit que sur strictement plus bas que soi.
    my_rank: u16,
    voice_token: u64,
    channels: Vec<ChannelInfo>,
    /// Tous les rôles du serveur, pour les couleurs et les badges.
    roles: Vec<ki_protocol::RoleInfo>,
    /// Salon textuel ouvert.
    current: Option<ChannelId>,
    /// Salon vocal où l'on se trouve. `None` = connecté au serveur sans
    /// être en vocal, ce qui est l'état à l'arrivée.
    voice_channel: Option<ChannelId>,
    /// Tout le monde sur le serveur, pas seulement le salon courant.
    members: Vec<Member>,
    messages: Vec<ChatRecord>,
    /// Effets sonores : nom court -> PCM 48 kHz mono. Les défauts synthétisés
    /// sont toujours là ; un .wav déposé dans « sons » les remplace.
    sounds: HashMap<String, Vec<f32>>,
    /// Événements dont le son vient d'un fichier (pour l'afficher).
    custom_sfx: std::collections::HashSet<String>,
    /// Événements coupés individuellement dans les réglages.
    sfx_muted: std::collections::HashSet<String>,
    sfx_on: bool,
    sfx_volume: f32,
    /// La fenêtre a-t-elle le focus ? (sons de message + notification)
    window_focused: bool,
    /// Demande de clignotement de la barre des tâches à honorer.
    wants_attention: bool,
    /// Occupants du salon vocal à l'image précédente : c'est leur écart qui
    /// révèle une arrivée ou un départ. Les messages `UserJoined`/`UserLeft`
    /// portent sur le serveur entier, pas sur le salon vocal.
    prev_voice_peers: std::collections::HashSet<UserId>,
    /// Salon vocal de l'image précédente : quand il change, les occupants du
    /// nouveau salon sont adoptés EN SILENCE (entrer dans un salon peuplé ne
    /// déclenche pas six sons d'arrivée) — sans fenêtre temporelle, qui
    /// mangeait les sons dès qu'on changeait de salon rapidement.
    prev_voice_channel: Option<ChannelId>,
    /// Reste-t-il du passé à remonter dans ce salon ?
    history_more: bool,
    /// Une page est déjà demandée : on n'en réclame pas une seconde à
    /// chaque image tant que celle-ci n'est pas arrivée.
    history_pending: bool,
    /// Empreinte du certificat du serveur courant, pour épingler le HTTPS
    /// du partage de fichiers sur la même identité que le QUIC.
    server_fingerprint: String,
    /// Adresse d'un serveur dont l'identité vient de changer. Tant qu'elle est
    /// posée, l'écran de connexion propose d'accepter la nouvelle — c'est la
    /// seule façon de se reconnecter à un serveur réinstallé.
    identity_alarm: Option<String>,
    /// Ce qu'on veut du vocal : un salon, ou en sortir.
    ///
    /// L'affichage suit cette intention le temps que le serveur la traite,
    /// puis c'est la liste des membres qui fait foi. Sans cette fenêtre, une
    /// liste produite avant notre clic nous remettait dans le salon qu'on
    /// venait de quitter ; sans sa **péremption**, un refus jamais notifié
    /// figeait l'affichage pour le reste de la session.
    voice_intent: Option<Option<ChannelId>>,
    /// Fin de la fenêtre pendant laquelle l'intention prime.
    voice_intent_until: std::time::Instant,
    /// Hauteur du fil au rendu précédent, en points.
    ///
    /// Sert à recaler la vue quand une page s'ajoute **au-dessus** : sans ça
    /// le contenu grandit vers le haut, la vue reste collée au sommet, et l'on
    /// perd la ligne qu'on était en train de lire.
    chat_height: f32,
    /// Hauteur du fil juste avant qu'une page ne s'y ajoute en tête. Le
    /// prochain rendu s'en sert pour rattraper le décalage, puis l'efface.
    history_anchor: Option<f32>,

    // UI
    input: String,
    /// Redonner le focus à la zone de saisie au prochain rendu.
    focus_input: bool,
    show_settings: bool,
    /// Journal audio déplié dans les réglages.
    show_audio_journal: bool,
    /// Docteur audio déplié dans les réglages.
    show_docteur: bool,
    /// Diagnostic partagé : le journal technique part vers le serveur
    /// (opt-in). Voir `flush_diag` pour ce qui transite — et ne transite pas.
    diag_share: bool,
    /// Horodatage (epoch ms) de la dernière ligne de journal déjà envoyée.
    diag_last_sent_ts: u64,
    /// Dernier envoi périodique, pour la cadence de dix minutes.
    diag_last_flush: Option<std::time::Instant>,
    /// Onglet admin « Diagnostics » : le dernier lot récupéré du serveur,
    /// rempli par un thread de récupération.
    diag_admin: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// Dernier diagnostic établi. Coûteux — il énumère les processus et
    /// interroge le registre — donc calculé au clic, pas à chaque image.
    docteur: Option<ki_voice::docteur::Diagnostic>,
    /// Relevé de performance déplié dans les réglages.
    show_perf: bool,
    /// Coût de l'interface, mesuré en continu (P1).
    perf: perf::Perf,
    /// Couleur de pseudo par auteur, reconstruite à chaque `Members`.
    ///
    /// Le fil la cherchait par un `find` **linéaire** dans la liste des
    /// membres, pour chaque message et à chaque image : cinq cents messages
    /// et trente membres, c'était quinze mille comparaisons par image, trois
    /// cent mille par seconde, pour un résultat qui ne change qu'à la
    /// réception d'un roster.
    author_colors: HashMap<UserId, egui::Color32>,
    /// Hauteur mesurée de chaque message à l'image précédente, pour pouvoir
    /// sauter ceux qui sont hors de l'écran sans changer la taille du fil.
    /// Clé : (auteur, horodatage). Vidée dès que la largeur change, une
    /// hauteur dépendant du retour à la ligne.
    msg_heights: HashMap<(UserId, u64), f32>,
    /// Largeur pour laquelle `msg_heights` a été mesurée.
    msg_heights_width: f32,
    input_devices: Vec<String>,
    output_devices: Vec<String>,
    upload_status: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    info: Option<String>,

    /// Identité du serveur courant, telle qu'il l'a annoncée.
    server_info: ServerInfo,

    // Panneau admin
    show_admin: bool,
    /// Nom du serveur en cours d'édition dans le panneau admin.
    admin_name: String,
    /// Logo choisi dans le panneau admin, pas encore envoyé.
    admin_icon: IconChange,
    admin_users: Vec<AccountInfo>,
    admin_invites: Vec<InviteInfo>,
    last_invite: Option<String>,
    reset_target: Option<String>,
    reset_password: String,
    /// Journal d'audit, du plus récent au plus ancien.
    audit: Vec<AuditRecord>,
    /// Onglet ouvert dans la fenêtre d'administration.
    admin_tab: AdminTab,
    /// Réglages du prochain code d'invitation à créer.
    invite_uses: Option<u32>,
    invite_label: String,
    /// Bannissement en cours de saisie : la cible, le motif, la durée.
    ban_draft: Option<BanDraft>,
    /// Salon vocal verrouillé dont on attend le mot de passe.
    voice_prompt: Option<VoicePrompt>,
    /// Rôle en cours de création ou d'édition.
    role_draft: Option<RoleDraft>,
    /// Salon en cours de création.
    channel_draft: Option<ChannelDraft>,
    /// Salon en cours de renommage, et verrou vocal en cours de pose. Un
    /// seul de chaque à la fois : deux formulaires ouverts sur la même
    /// liste, on ne sait plus lequel on remplit.
    channel_edit: Option<ChannelEdit>,
    verrou_draft: Option<VerrouDraft>,

    /// Reprise automatique en cours. `None` = rien à reprendre.
    reprise: Option<Reprise>,
    /// Le moteur voix et ses attaches. **Hors** de la connexion, exprès :
    /// une coupure ne doit pas refermer le micro (voir `net::VoiceLink`).
    link: net::VoiceLink,

    // Recherche dans l'historique
    show_search: bool,
    search_query: String,
    /// Chercher dans le salon courant seulement, ou dans tous ceux qu'on peut
    /// lire. Restreint par défaut : on sait presque toujours où l'on a vu
    /// passer la chose, et le serveur relit moins de fichiers.
    search_ici: bool,
    search_hits: Vec<ki_protocol::SearchHit>,
    search_more: bool,
    /// La requête effectivement envoyée, en attente de réponse.
    ///
    /// Sert à jeter une réponse périmée : le serveur relit des fichiers, deux
    /// réponses peuvent revenir dans le désordre, et la plus lente écraserait
    /// la bonne.
    search_envoyee: Option<String>,
    search_focus: bool,
    /// Salon dans lequel on a sauté au milieu du passé.
    ///
    /// Le fil n'affiche alors pas la fin de la conversation, et rien ne le
    /// dirait sans ce drapeau : on croirait le salon vide depuis.
    retour_present: Option<ChannelId>,
    /// Compte dont on modifie les rôles, et la sélection en cours.
    roles_target: Option<String>,
    roles_draft: Vec<ki_protocol::RoleId>,

    // Mon compte
    show_account: bool,
    old_password: String,
    new_password: String,

    // Volumes par utilisateur, par compte local : mon pseudo -> (user_id -> gain).
    // Chacun a ses propres réglages, persistés entre les sessions.
    all_volumes: HashMap<String, HashMap<u64, f32>>,

    // Vocal
    mode: MicMode,
    ptt_key: PttKey,
    muted: bool,
    /// Micro armé (mute/PTT/mode) — commande envoyée au moteur.
    armed: bool,
    /// Émission réellement en cours (lue depuis le moteur, après VAD).
    transmitting: bool,
    pref_input: Option<String>,
    pref_output: Option<String>,
    /// Moteur audio Windows natif (WASAPI direct) — cpal si décoché.
    native_audio: bool,
    /// Mode brut du micro : demande à Windows d'ignorer les effets tiers
    /// (Sonar, Nahimic…). Moteur natif seulement.
    raw_mic: bool,
    /// Micro en catégorie « communications » dès l'ouverture (sinon le moteur
    /// n'y bascule que s'il détecte un micro affamé).
    comms_mic: bool,
    noise_mode: u8,
    input_gain: f32,
    output_gain: f32,
    vad_threshold: f32,
    vad_hangover_ms: u32,
    /// Débit choisi (0 = Auto : piloté par les rapports qualité du serveur).
    bitrate: i32,
    /// Débit courant en mode Auto.
    auto_bitrate: i32,
    /// Rapports "propres" consécutifs (pour remonter le débit).
    good_reports: u8,
    /// Dernières pertes montantes signalées par le serveur (%).
    upstream_loss: Option<f32>,
    /// Protection contre les pertes DRED : 0 = off, 1 = auto, 2 = toujours.
    dred_mode: u8,
    /// DRED actuellement engagé (mode auto).
    dred_active: bool,
    agc: bool,
    agc_target: f32,
    gate_threshold: f32,
    jitter_frames: usize,
    ptt_release_ms: u32,
    loopback: bool,
    /// Calibration des seuils en cours : (départ, crête ambiante mesurée).
    calibrating: Option<(std::time::Instant, f32)>,
    // Labo vidéo (S1a du partage d'écran) : boucle locale de test.
    labo: Option<ki_video::LocalLoop>,
    labo_frame: std::sync::Arc<std::sync::Mutex<Option<ki_video::RgbaFrame>>>,
    labo_stats: std::sync::Arc<ki_video::StageStats>,
    labo_texture: Option<egui::TextureHandle>,
    /// Surveillance de la touche push-to-talk, sur son propre fil.
    ///
    /// `Option` parce qu'elle a besoin du contexte egui, qui n'existe qu'une
    /// fois la fenêtre ouverte : elle démarre à la première image.
    ptt: Option<ptt::Watcher>,
    /// Interdiction de veille système pendant le vocal.
    veille: veille::Garde,
}

/// Photo de l'état vocal prise une fois par frame, pour ne pas verrouiller
/// le moteur audio à répétition pendant le rendu.
#[derive(Default)]
struct VoiceSnapshot {
    engine_up: bool,
    stats: ki_voice::VoiceStats,
    ping: Option<u32>,
    levels: HashMap<u64, f32>,
    /// Périphérique audio perdu, en cours de réouverture : (micro, sortie).
    device_trouble: (bool, bool),
    /// Périphériques de repli en service : (micro, sortie).
    device_fallback: (bool, bool),
    /// Le moteur propose de basculer le micro en catégorie « communications »
    /// (micro affamé) et attend la réponse de l'utilisateur.
    comms_proposal: bool,
}

impl KiApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // (le littéral Self plus bas est affecté à `app` pour charger les
        // sons juste après la construction)
        theme::install(&cc.egui_ctx);
        // Les photos s'accumulent au fil des changements : on borne le
        // dossier une fois par démarrage.
        photos::prune();

        let get = |key: &str, default: &str| -> String {
            cc.storage
                .and_then(|s| s.get_string(key))
                .unwrap_or_else(|| default.to_string())
        };
        // Le carnet reprend l'unique adresse des versions précédentes ; on
        // rouvre sur le serveur utilisé en dernier.
        let book = servers::load(cc.storage);
        let selected = book
            .iter()
            .max_by_key(|s| s.last_used)
            .or_else(|| book.first())
            .map(|s| s.id);
        let active = selected.and_then(|id| book.iter().find(|s| s.id == id));

        // La vérification part maintenant et répond quand elle répond : le
        // lanceur s'affiche sans l'attendre.
        let refused = Some(get("update_skipped", "")).filter(|v| !v.is_empty());

        let mut app = Self {
            updater: update::Updater::start(refused, cc.egui_ctx.clone()),
            url: active.map(|s| s.address.clone()).unwrap_or_default(),
            username: active
                .map(|s| s.username.clone())
                .unwrap_or_else(|| get("username", "")),
            password: active.and_then(|s| s.password()).unwrap_or_default(),
            remember_password: active.is_some_and(|s| s.secret.is_some()),
            book,
            selected,
            probes: servers::Probes::default(),
            draft: None,
            icon_textures: HashMap::new(),
            picked_icon: Default::default(),
            picked_avatar: Default::default(),
            preview_icons: HashMap::new(),
            previews: images::Previews::default(),
            maximized: get("window_maximized", "") == "on",
            restore_maximized: get("window_maximized", "") == "on",
            avatars: HashMap::new(),
            incoming_avatars: HashMap::new(),
            account_avatar: IconChange::Keep,
            server_info: ServerInfo::default(),
            admin_name: String::new(),
            admin_icon: IconChange::Keep,
            invite: String::new(),
            show_invite: false,
            conn: None,
            connecting: false,
            connect_started: None,
            welcomed: false,
            error: None,
            my_id: None,
            my_perms: 0,
            my_rank: 0,
            roles: Vec::new(),
            voice_token: 0,
            channels: Vec::new(),
            current: None,
            voice_channel: None,
            members: Vec::new(),
            messages: Vec::new(),
            sounds: HashMap::new(),   // rempli par reload_sounds() ci-dessous
            custom_sfx: std::collections::HashSet::new(),
            sfx_muted: get("sfx_muted", "")
                .split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            sfx_on: get("sfx_on", "on") == "on",
            sfx_volume: get("sfx_volume", "0.6").parse().unwrap_or(0.6),
            window_focused: true,
            wants_attention: false,
            prev_voice_peers: std::collections::HashSet::new(),
            prev_voice_channel: None,
            history_more: false,
            history_pending: false,
            server_fingerprint: String::new(),
            identity_alarm: None,
            voice_intent: None,
            voice_intent_until: std::time::Instant::now(),
            chat_height: 0.0,
            history_anchor: None,
            input: String::new(),
            focus_input: false,
            show_settings: false,
            show_audio_journal: false,
            show_docteur: false,
            docteur: None,
            diag_share: get("diag_share", "off") == "on",
            diag_last_sent_ts: 0,
            diag_last_flush: None,
            diag_admin: Default::default(),
            show_perf: false,
            perf: perf::Perf::default(),
            author_colors: HashMap::new(),
            msg_heights: HashMap::new(),
            msg_heights_width: 0.0,
            input_devices: Vec::new(),
            output_devices: Vec::new(),
            upload_status: Default::default(),
            info: None,
            show_admin: false,
            admin_users: Vec::new(),
            admin_invites: Vec::new(),
            last_invite: None,
            reset_target: None,
            reset_password: String::new(),
            audit: Vec::new(),
            admin_tab: AdminTab::Server,
            invite_uses: Some(1),
            invite_label: String::new(),
            ban_draft: None,
            voice_prompt: None,
            role_draft: None,
            channel_draft: None,
            channel_edit: None,
            verrou_draft: None,
            reprise: None,
            link: net::VoiceLink::default(),
            show_search: false,
            search_query: String::new(),
            search_ici: true,
            search_hits: Vec::new(),
            search_more: false,
            search_envoyee: None,
            search_focus: false,
            retour_present: None,
            roles_target: None,
            roles_draft: Vec::new(),
            show_account: false,
            old_password: String::new(),
            new_password: String::new(),
            all_volumes: cc
                .storage
                .and_then(|s| s.get_string("user_volumes"))
                .and_then(|json| serde_json::from_str(&json).ok())
                .unwrap_or_default(),
            mode: match get("mic_mode", "ptt").as_str() {
                "open" => MicMode::Open,
                "vad" => MicMode::Vad,
                _ => MicMode::Ptt,
            },
            ptt_key: PttKey::from_id(&get("ptt_key", "lalt")).unwrap_or(PttKey::LAlt),
            muted: false,
            armed: false,
            transmitting: false,
            pref_input: Some(get("input_device", "")).filter(|s| !s.is_empty()),
            pref_output: Some(get("output_device", "")).filter(|s| !s.is_empty()),
            native_audio: get("native_audio", "on") != "off",
            raw_mic: get("raw_mic", "off") == "on",
            comms_mic: get("comms_mic", "off") == "on",
            noise_mode: get("noise_mode", "1")
                .parse()
                .unwrap_or(ki_voice::NOISE_RNNOISE),
            input_gain: get("input_gain", "1.0").parse().unwrap_or(1.0),
            output_gain: get("output_gain", "1.0").parse().unwrap_or(1.0),
            vad_threshold: get("vad_threshold", "0.02").parse().unwrap_or(0.02),
            vad_hangover_ms: get("vad_hangover_ms", "400").parse().unwrap_or(400),
            bitrate: get("bitrate", "0").parse().unwrap_or(0),
            auto_bitrate: 64_000,
            good_reports: 0,
            upstream_loss: None,
            dred_mode: get("dred_mode", "1").parse().unwrap_or(1),
            dred_active: false,
            agc: get("agc", "on") != "off",
            agc_target: get("agc_target", "0.30").parse().unwrap_or(0.30),
            gate_threshold: get("gate_threshold", "0").parse().unwrap_or(0.0),
            jitter_frames: get("jitter_frames", "0").parse().unwrap_or(0),
            ptt_release_ms: get("ptt_release_ms", "100").parse().unwrap_or(100),
            loopback: false,
            calibrating: None,
            labo: None,
            labo_frame: Default::default(),
            labo_stats: Default::default(),
            labo_texture: None,
            ptt: None,
            veille: veille::Garde::default(),
        };
        // Instance relancée par le dispositif de secours : le dire. Sans ce
        // bandeau, la fenêtre qui se ferme puis revient passe pour un caprice,
        // et personne ne sait qu'un journal attend d'être lu.
        if secours::essais_actuels() > 0 {
            app.error = Some(match secours::chemin_journal() {
                Some(chemin) => format!(
                    "ki-chat s'est relancé : la fenêtre précédente s'est fermée sur une \
                     erreur graphique (pilote réinitialisé par un jeu, veille…). \
                     Détails : {}",
                    chemin.display()
                ),
                None => "ki-chat s'est relancé : la fenêtre précédente s'est fermée sur \
                         une erreur graphique (pilote réinitialisé par un jeu, veille…)"
                    .into(),
            });
        }
        app.reload_sounds();
        app
    }

    /// (Re)charge les effets sonores : les défauts synthétisés d'abord, puis
    /// les .wav du dossier « sons » qui remplacent ceux du même nom.
    fn reload_sounds(&mut self) {
        let disk = load_sounds();
        self.custom_sfx = disk.keys().cloned().collect();
        let mut sounds = sfxgen::defaults();
        sounds.extend(disk);
        tracing::info!(
            "effets sonores : {} chargés dont {} personnalisés",
            sounds.len(),
            self.custom_sfx.len()
        );
        self.sounds = sounds;
    }

    /// Démarre la boucle locale du labo vidéo (S1a) : capture de l'écran
    /// principal, aller-retour H.264 complet, image déposée pour l'UI.
    fn start_labo(&mut self, ctx: egui::Context) {
        let stats = std::sync::Arc::new(ki_video::StageStats::default());
        let frame_slot = self.labo_frame.clone();
        let sink: ki_video::FrameSink = std::sync::Arc::new(move |frame| {
            *frame_slot.lock().unwrap() = Some(frame);
            // Chaque trame capturée réveille la fenêtre. C'était le seul
            // moyen de dépasser les 20 images par seconde du repeint
            // périodique ; depuis que celui-ci est conditionnel, c'est
            // devenu le seul moyen d'en avoir tout court — et c'est la bonne
            // façon : la vidéo peint quand elle a quelque chose à montrer,
            // pas à une cadence devinée d'avance.
            ctx.request_repaint();
        });
        match ki_video::LocalLoop::start(stats.clone(), sink) {
            Ok(handle) => {
                self.labo_stats = stats;
                self.labo_texture = None;
                self.labo = Some(handle);
            }
            Err(e) => self.error = Some(format!("labo vidéo : {e:#}")),
        }
    }

    fn stop_labo(&mut self) {
        if let Some(labo) = self.labo.take() {
            labo.stop();
        }
        *self.labo_frame.lock().unwrap() = None;
        self.labo_texture = None;
    }

    /// Fenêtre du labo : l'image décodée + les compteurs par étage.
    fn labo_window(&mut self, ctx: &egui::Context) {
        // Nouvelle image ? On met la texture à jour (allocation unique,
        // `set` ensuite — le pattern egui pour la vidéo).
        if let Some(frame) = self.labo_frame.lock().unwrap().take() {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [frame.width, frame.height],
                &frame.rgba,
            );
            match &mut self.labo_texture {
                Some(tex) if tex.size() == [frame.width, frame.height] => {
                    tex.set(image, egui::TextureOptions::LINEAR);
                }
                _ => {
                    self.labo_texture = Some(ctx.load_texture(
                        "labo-video",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
            self.labo_stats.painted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        let mut open = true;
        egui::Window::new("🧪 Labo vidéo — aller-retour H.264 local")
            .open(&mut open)
            .default_width(820.0)
            .show(ctx, |ui| {
                if let Some(tex) = &self.labo_texture {
                    let [w, h] = tex.size();
                    let avail = ui.available_width().max(320.0);
                    let scale = (avail / w as f32).min(1.0);
                    ui.image((tex.id(), egui::vec2(w as f32 * scale, h as f32 * scale)));
                } else {
                    ui.label("démarrage de la capture…");
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new(self.labo_stats.summary()).weak().size(11.5).monospace(),
                );
            });
        if !open {
            self.stop_labo();
        }
    }

    /// Seuil VAD effectif : actif seulement en mode activation vocale.
    fn effective_vad(&self) -> f32 {
        if self.mode == MicMode::Vad {
            self.vad_threshold
        } else {
            0.0
        }
    }

    /// Débit effectif : le choix manuel, ou le débit auto-piloté.
    fn effective_bitrate(&self) -> i32 {
        if self.bitrate == 0 {
            self.auto_bitrate
        } else {
            self.bitrate
        }
    }

    /// Débit adaptatif : réagit aux rapports de pertes montantes du serveur.
    /// Baisse d'un cran dès 5 % de pertes, remonte d'un cran après trois
    /// fenêtres propres consécutives (15 s), plafonné à 64 kbps.
    fn on_net_quality(&mut self, loss_pct: f32) {
        self.upstream_loss = Some(loss_pct);
        // Protection DRED automatique : engagée dès 2 % de pertes, relâchée
        // après trois fenêtres propres (le débit adaptatif gère le reste).
        if self.dred_mode == 1 {
            let was = self.dred_active;
            if loss_pct > 2.0 {
                self.dred_active = true;
            } else if loss_pct < 0.5 && self.good_reports >= 2 {
                self.dred_active = false;
            }
            if was != self.dred_active {
                self.apply_audio_settings();
            }
        }
        if self.bitrate != 0 {
            return; // débit manuel : on affiche l'info, on ne pilote pas
        }
        let idx = BITRATES
            .iter()
            .position(|&b| b == self.auto_bitrate)
            .unwrap_or(3);
        if loss_pct > 5.0 {
            if idx > 0 {
                self.auto_bitrate = BITRATES[idx - 1];
                self.good_reports = 0;
                self.apply_audio_settings();
            }
        } else if loss_pct < 1.0 {
            self.good_reports = self.good_reports.saturating_add(1);
            if self.good_reports >= 3 && self.auto_bitrate < 64_000 {
                self.auto_bitrate = BITRATES[idx + 1].min(64_000);
                self.good_reports = 0;
                self.apply_audio_settings();
            }
        } else {
            self.good_reports = 0;
        }
    }

    /// Pousse tous les réglages audio actuels vers le moteur, à chaud.
    fn apply_audio_settings(&self) {
        if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
            engine.set_noise_mode(self.noise_mode);
            engine.set_input_gain(self.input_gain);
            engine.set_output_gain(self.output_gain);
            engine.set_vad_threshold(self.effective_vad());
            engine.set_vad_hangover_ms(self.vad_hangover_ms);
            engine.set_bitrate(self.effective_bitrate());
            engine.set_agc(self.agc);
            engine.set_agc_target(self.agc_target);
            engine.set_gate_threshold(self.gate_threshold);
            engine.set_jitter_frames(self.jitter_frames);
            engine.set_dred(match self.dred_mode {
                0 => 0,
                2 => ki_voice::DRED_DEFAULT,
                _ => {
                    if self.dred_active {
                        ki_voice::DRED_DEFAULT
                    } else {
                        0
                    }
                }
            });
            engine.set_loopback(self.loopback);
        }
    }

    fn voice_prefs(&self) -> net::VoicePrefs {
        net::VoicePrefs {
            input_device: self.pref_input.clone(),
            output_device: self.pref_output.clone(),
            native_audio: self.native_audio,
            raw_mic: self.raw_mic,
            comms_mic: self.comms_mic,
            noise_mode: self.noise_mode,
            volumes: self
                .all_volumes
                .get(self.username.trim())
                .cloned()
                .unwrap_or_default(),
            input_gain: self.input_gain,
            output_gain: self.output_gain,
            vad_threshold: self.effective_vad(),
            vad_hangover_ms: self.vad_hangover_ms,
            bitrate: self.effective_bitrate(),
            agc: self.agc,
            agc_target: self.agc_target,
            gate_threshold: self.gate_threshold,
            jitter_frames: self.jitter_frames,
            dred: match self.dred_mode {
                0 => 0,
                2 => ki_voice::DRED_DEFAULT,
                _ if self.dred_active => ki_voice::DRED_DEFAULT,
                _ => 0,
            },
        }
    }

    /// Photo de l'état du moteur audio pour la frame en cours.
    fn voice_snapshot(&self) -> VoiceSnapshot {
        // Le ping n'existe que connecté ; le moteur, lui, tourne même
        // pendant une coupure — et c'est voulu : le vumètre du micro
        // continue de bouger, les réglages audio restent utilisables.
        let ping = self.conn.as_ref().and_then(|c| c.rtt_ms());
        match self.link.engine.lock().unwrap().as_ref() {
            Some(engine) => VoiceSnapshot {
                engine_up: true,
                stats: engine.stats(),
                ping,
                levels: engine.user_levels().into_iter().collect(),
                device_trouble: engine.device_trouble(),
                device_fallback: engine.device_fallback(),
                comms_proposal: engine.comms_proposal(),
            },
            None => VoiceSnapshot {
                ping,
                ..Default::default()
            },
        }
    }

    /// Mon pseudo **tel que le serveur le connaît**.
    ///
    /// Et non celui tapé dans le formulaire de connexion : le serveur le
    /// rogne, et une mention se compare à ce qu'il diffuse, pas à ce qu'on a
    /// saisi.
    fn my_pseudo(&self) -> Option<&str> {
        let id = self.my_id?;
        self.members
            .iter()
            .find(|m| m.user_id == id)
            .map(|m| m.username.as_str())
    }

    /// Volume mémorisé pour un utilisateur (1.0 = 100 %).
    fn volume_of(&self, user_id: UserId) -> f32 {
        self.all_volumes
            .get(self.username.trim())
            .and_then(|v| v.get(&user_id))
            .copied()
            .unwrap_or(1.0)
    }

    /// Mémorise et applique à chaud le volume d'un utilisateur.
    fn set_volume(&mut self, user_id: UserId, gain: f32) {
        let my = self.username.trim().to_string();
        let volumes = self.all_volumes.entry(my).or_default();
        if (gain - 1.0).abs() < 0.001 {
            volumes.remove(&user_id);
        } else {
            volumes.insert(user_id, gain);
        }
        if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
            engine.set_user_volume(user_id, gain);
        }
    }

    /// Base HTTP du serveur (partage de fichiers) : même hôte que le QUIC,
    /// port HTTP conventionnel 8080.
    fn http_base(&self) -> String {
        let trimmed = self.url.trim();
        let host = trimmed.rsplit_once(':').map(|(h, _)| h).unwrap_or(trimmed);
        // HTTPS : le partage de fichiers portait le jeton de session et le
        // contenu des fichiers en clair, à côté d'un tunnel QUIC chiffré.
        format!("https://{host}:8080")
    }

    /// Client HTTP épinglé sur l'empreinte du serveur courant.
    ///
    /// Le certificat étant celui du QUIC, la même empreinte fait foi : une
    /// seule identité à vérifier, et rien qui parte vers un imposteur.
    fn http_agent(&self) -> ureq::Agent {
        let expected =
            (!self.server_fingerprint.is_empty()).then_some(self.server_fingerprint.as_str());
        ureq::AgentBuilder::new()
            .tls_config(ki_client_quic::pinned_tls_config(expected))
            .build()
    }

    fn send(&self, msg: ClientMsg) {
        if let Some(conn) = &self.conn {
            conn.send(msg);
        }
    }

    /// Ouvre un salon textuel : on change ce qu'on lit, rien d'autre.
    fn join(&mut self, channel: ChannelId) {
        self.current = Some(channel);
        self.messages.clear();
        // On repart du principe qu'il y a un passé à remonter : le serveur
        // dira le contraire dès la première page s'il n'y en a pas.
        self.history_more = true;
        self.history_pending = false;
        // Entrer normalement dans un salon, c'est en voir la fin : le pied de
        // fil « tu regardes un message retrouvé » n'a plus lieu d'être.
        self.retour_present = None;
        // L'ancre de défilement se rapporte à la hauteur du salon qu'on
        // quitte : la garder ferait sauter la vue du nouveau.
        self.history_anchor = None;
        self.focus_input = true;
        self.send(ClientMsg::Join { channel });
        self.send(ClientMsg::History { limit: 100 });
    }

    /// Ai-je cette permission ?
    fn can(&self, need: ki_protocol::Perms) -> bool {
        ki_protocol::perm::has(self.my_perms, need)
    }

    /// Puis-je agir sur cette personne ? Le rang tranche, jamais la
    /// permission : c'est ce qui empêche un second administrateur de bannir
    /// le propriétaire.
    fn outranks(&self, member_rank: u16) -> bool {
        member_rank < self.my_rank
    }

    /// Ai-je de quoi ouvrir le panneau d'administration ?
    ///
    /// **Une seule** de ces permissions suffit — d'où le `any` et non un
    /// masque combiné : `perm::has` exige la totalité des bits qu'on lui
    /// passe, ce qui n'aurait ouvert le panneau qu'aux tout-puissants.
    fn any_admin_power(&self) -> bool {
        use ki_protocol::perm::*;
        [
            MANAGE_SERVER,
            MANAGE_CHANNELS,
            MANAGE_ROLES,
            KICK,
            BAN,
            CREATE_INVITE,
            VIEW_AUDIT_LOG,
        ]
        .iter()
        .any(|p| self.can(*p))
    }

    /// Couleur d'un membre : celle de son rôle, sinon son pseudo.
    fn color_of(&self, member: &Member) -> egui::Color32 {
        theme::member_color(member.color, &member.username)
    }

    /// Nom du rôle le mieux classé d'un membre, pour l'afficher en badge.
    fn top_role_name(&self, member: &Member) -> Option<&str> {
        member
            .roles
            .iter()
            .filter_map(|id| self.roles.iter().find(|r| r.id == *id))
            .max_by_key(|r| r.rank)
            .map(|r| r.name.as_str())
    }

    /// Joue un effet, s'il est présent et si les sons ne sont pas coupés.
    ///
    /// Silencieux quand le son n'existe pas : chacun dépose les fichiers
    /// qu'il veut, et il ne manque rien à celui qui n'en met aucun.
    fn play_sfx(&self, name: &str) {
        if !self.sfx_on || self.sfx_muted.contains(name) {
            return;
        }
        let Some(pcm) = self.sounds.get(name) else { return };
        if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
            engine.play_effect(pcm, self.sfx_volume);
        }
    }

    /// Compare les occupants de mon salon vocal à ceux de l'image précédente
    /// pour jouer les sons d'arrivée et de départ.
    ///
    /// C'est un écart qu'on mesure, et non `UserJoined`/`UserLeft` : ces
    /// deux-là portent sur le serveur entier, alors que seul mon propre
    /// salon vocal m'intéresse ici.
    /// Ce qui suit tout changement de la liste des membres, qu'il vienne
    /// d'un roster complet ou d'une seule fiche.
    ///
    /// **Le serveur fait foi sur notre propre présence en vocal**, sauf
    /// pendant la brève fenêtre qui suit un clic : la liste peut avoir été
    /// produite avant qu'il ne traite notre demande, et la suivre nous
    /// remettrait dans le salon qu'on vient de quitter. Dès qu'elle
    /// correspond à ce qu'on voulait, la fenêtre se referme ; si elle expire
    /// sans jamais correspondre — refus, message perdu — c'est le serveur qui
    /// reprend la main, ce qui évite tout affichage figé.
    fn after_roster_change(&mut self) {
        if let Some(me) = self.members.iter().find(|m| Some(m.user_id) == self.my_id) {
            let mine = me.voice;
            let waiting = self.voice_intent.is_some()
                && std::time::Instant::now() < self.voice_intent_until;
            match self.voice_intent {
                Some(want) if mine == want => {
                    self.voice_intent = None;
                    self.voice_channel = mine;
                }
                _ if waiting => {}
                _ => {
                    self.voice_intent = None;
                    self.voice_channel = mine;
                }
            }
        }
        self.update_voice_peers();
    }

    fn update_voice_peers(&mut self) {
        let Some(mine) = self.voice_channel else {
            self.prev_voice_peers.clear();
            self.prev_voice_channel = None;
            return;
        };
        let now: std::collections::HashSet<UserId> = self
            .members
            .iter()
            .filter(|m| m.voice == Some(mine) && Some(m.user_id) != self.my_id)
            .map(|m| m.user_id)
            .collect();
        // Je viens d'arriver dans CE salon : ses occupants sont adoptés en
        // silence — seuls les mouvements ULTÉRIEURS feront du bruit.
        if self.prev_voice_channel != Some(mine) {
            self.prev_voice_channel = Some(mine);
            self.prev_voice_peers = now;
            return;
        }
        if now.difference(&self.prev_voice_peers).next().is_some() {
            self.play_sfx(sfx::PEER_JOIN);
        }
        if self.prev_voice_peers.difference(&now).next().is_some() {
            self.play_sfx(sfx::PEER_LEAVE);
        }
        self.prev_voice_peers = now;
    }

    /// Demande la page d'historique précédant le plus ancien message affiché.
    fn load_older_history(&mut self) {
        if self.history_pending || !self.history_more {
            return;
        }
        let Some(oldest) = self.messages.first().map(|m| m.ts) else { return };
        let Some(channel) = self.current else { return };
        self.history_pending = true;
        self.send(ClientMsg::HistoryBefore { before_ts: oldest, limit: 100, channel });
    }

    /// Entre dans un salon vocal, ou en change.
    fn join_voice(&mut self, channel: ChannelId) {
        if self.voice_channel == Some(channel) {
            return;
        }
        // Le serveur refuserait : autant le dire tout de suite, plutôt que
        // d'allumer le salon et le micro pour rien.
        if !self.can(ki_protocol::perm::CONNECT_VOICE) {
            self.error = Some("tu n'as pas le droit de rejoindre le vocal".into());
            return;
        }
        // L'entrée est affichée sans attendre — le retour immédiat compte —
        // mais elle reste **en attente de confirmation** : le serveur peut
        // refuser, et la liste des membres qu'il diffuse fait foi. Sans cette
        // réconciliation, un refus laissait l'interface montrer le salon et
        // armer le micro dans le vide, indéfiniment.
        self.voice_channel = Some(channel);
        self.set_voice_intent(Some(channel));
        self.play_sfx(sfx::SELF_JOIN);
        // Le prochain passage d'update_voice_peers adoptera les occupants du
        // nouveau salon en silence (voir prev_voice_channel).
        self.prev_voice_channel = None;
        self.prev_voice_peers.clear();
        self.send(ClientMsg::JoinVoice { channel, password: None });
    }

    /// Annonce ce qu'on veut du vocal, et laisse à l'affichage le temps que le
    /// serveur le confirme. Passé ce délai, c'est lui qui décide de nouveau.
    fn set_voice_intent(&mut self, want: Option<ChannelId>) {
        self.voice_intent = Some(want);
        self.voice_intent_until =
            std::time::Instant::now() + std::time::Duration::from_secs(3);
    }

    /// Quitte le vocal, sans quitter le serveur.
    fn leave_voice(&mut self) {
        self.set_voice_intent(None);
        if self.voice_channel.take().is_none() {
            return;
        }
        self.armed = false;
        self.transmitting = false;
        self.play_sfx(sfx::SELF_LEAVE);
        self.prev_voice_peers.clear();
        self.send(ClientMsg::LeaveVoice);
    }

    /// Nom du salon vocal occupé.
    fn voice_channel_name(&self) -> Option<&str> {
        let id = self.voice_channel?;
        self.channels.iter().find(|c| c.id == id).map(|c| c.name.as_str())
    }

    /// Referme la connexion et **efface tout ce qui appartenait au serveur
    /// quitté**.
    ///
    /// L'exhaustivité n'est pas du zèle : les identifiants d'utilisateur sont
    /// attribués **par serveur**, si bien que la moindre table indexée par
    /// `user_id` qui survivait montrait, sur le serveur suivant, la photo et
    /// le nom de quelqu'un d'autre. Le reste était du même ordre : une fenêtre
    /// « Bannir untel » ressurgissait ailleurs, le panneau d'administration
    /// affichait les comptes d'un serveur auquel on n'était plus connecté, et
    /// l'empreinte retenue servait à vérifier une identité qui n'était plus la
    /// bonne.
    ///
    /// Toute donnée venue du réseau se remet donc à zéro ici. Ce qui reste est
    /// exactement ce qui appartient à la **machine** et non au serveur : les
    /// réglages audio, le carnet de serveurs, les volumes par personne.
    /// Le lien a lâché tout seul : on arme la reprise plutôt que de rendre
    /// l'écran de connexion.
    ///
    /// Distinct de [`Self::disconnect`], qui reste la sortie **définitive** —
    /// on s'est déconnecté soi-même, on a été expulsé, le mot de passe est
    /// faux, ou l'identité du serveur a changé. Confondre les deux ferait
    /// marteler un serveur qui vient de nous refuser, ou reprendre une
    /// connexion que l'on venait de fermer exprès.
    fn connexion_perdue(&mut self, error: Option<String>) {
        // On ne reprend qu'une connexion qui a **déjà fonctionné**. Un
        // premier essai qui échoue, c'est une adresse mal tapée ou un serveur
        // qu'on n'a jamais eu : réessayer en boucle n'y changerait rien et
        // masquerait l'erreur derrière un décompte.
        if !self.welcomed && self.reprise.is_none() {
            self.disconnect(error);
            return;
        }
        // À partir d'ici, on garde le son : `fermer_session` et non
        // `disconnect`.
        let essais = self.reprise.as_ref().map_or(0, |r| r.essais) + 1;
        // Relevés **avant** le nettoyage, qui les efface. Et repris de la
        // reprise en cours s'il y en a une : à la deuxième tentative, l'état
        // courant est déjà vide.
        let (salon, vocal) = match &self.reprise {
            Some(r) => (r.salon, r.vocal),
            None => (self.current, self.voice_channel),
        };
        self.fermer_session(error);
        if essais > Reprise::MAX {
            // On renonce : plus aucune raison de garder le micro ouvert, et
            // le tenir priverait un jeu du périphérique sans contrepartie.
            self.link.arreter();
            self.error = Some(
                "connexion perdue — le serveur n'a pas répondu, reconnecte-toi quand il \
                 sera revenu"
                    .into(),
            );
            return;
        }
        self.reprise = Some(Reprise {
            essais,
            quand: std::time::Instant::now() + Reprise::attente(essais, alea()),
            salon,
            vocal,
        });
    }

    /// Relance la connexion quand son tour est venu.
    fn tick_reprise(&mut self, ctx: &egui::Context) {
        let Some(r) = &self.reprise else { return };
        if self.connecting || std::time::Instant::now() < r.quand {
            return;
        }
        self.connect(ctx);
    }

    /// Temps restant avant la prochaine tentative, pour l'afficher.
    fn reprise_dans(&self) -> Option<std::time::Duration> {
        let r = self.reprise.as_ref()?;
        Some(r.quand.saturating_duration_since(std::time::Instant::now()))
    }

    /// Sortie **définitive** : on ferme la session **et** on rend les
    /// périphériques audio.
    ///
    /// C'est ici, et seulement ici, que le son s'arrête. Une coupure subie
    /// passe par [`Self::connexion_perdue`], qui ne ferme que la session et
    /// laisse le moteur debout, périphériques ouverts. Sans cette séparation,
    /// tout hoquet du réseau refermait le micro — et un jeu pouvait s'en
    /// emparer dans l'intervalle, en mode exclusif, pour ne plus le rendre.
    fn disconnect(&mut self, error: Option<String>) {
        self.fermer_session(error);
        self.link.arreter();
    }

    /// Défait la session sans toucher au son.
    fn fermer_session(&mut self, error: Option<String>) {
        // Toute sortie désarme la reprise : sans ça, cliquer « Se
        // déconnecter » pendant une coupure nous y ramènerait tout seul.
        // `connexion_perdue` la réarme après coup, elle seule.
        self.reprise = None;
        if let Some(mut conn) = self.conn.take() {
            conn.quit();
        }
        self.connecting = false;
        self.connect_started = None;
        self.welcomed = false;

        // --- Identité et droits ---
        self.my_id = None;
        self.my_perms = 0;
        self.my_rank = 0;
        self.roles.clear();
        self.voice_token = 0;
        self.server_fingerprint.clear();

        // --- Salons, présence, conversation ---
        self.channels.clear();
        self.current = None;
        self.voice_channel = None;
        self.voice_intent = None;
        self.members.clear();
        self.messages.clear();
        self.history_more = false;
        self.history_pending = false;
        self.history_anchor = None;

        // --- Vignettes : indexées par user_id, donc par serveur ---
        self.avatars.clear();
        self.incoming_avatars.clear();
        self.account_avatar = IconChange::Keep;
        self.preview_icons.clear();
        self.previews = images::Previews::default();

        // --- Identité du serveur et panneau d'administration ---
        self.server_info = ServerInfo::default();
        self.admin_name.clear();
        self.admin_icon = IconChange::Keep;
        self.admin_users.clear();
        self.admin_invites.clear();
        self.last_invite = None;
        self.audit.clear();
        self.roles_draft.clear();

        // --- Fenêtres et brouillons en cours ---
        self.ban_draft = None;
        self.voice_prompt = None;
        self.channel_edit = None;
        self.verrou_draft = None;
        // Les résultats se rapportent à un serveur et à ses salons : les
        // garder ferait cliquer sur des numéros de salon d'ailleurs.
        self.show_search = false;
        self.search_query.clear();
        self.search_hits.clear();
        self.search_envoyee = None;
        self.search_more = false;
        self.retour_present = None;
        *self.upload_status.lock().unwrap() = None;

        self.armed = false;
        self.transmitting = false;
        self.loopback = false;
        self.show_settings = false;
        self.show_admin = false;
        self.show_account = false;
        self.error = error;
    }

    /// Faut-il une image de plus, et dans combien de temps ?
    ///
    /// `None` = rien ne bouge, l'application peut dormir jusqu'au prochain
    /// événement. C'est le cas par défaut, et c'est nouveau : le repeint était
    /// inconditionnel, à vingt images par seconde, même fenêtre réduite
    /// pendant une partie.
    ///
    /// Tout ce qui vient de l'extérieur réveille déjà la fenêtre de lui-même
    /// (réseau, images téléchargées, sondes de serveurs, touche
    /// push-to-talk). Ne restent donc ici que les choses qui **s'animent
    /// toutes seules** : un vumètre suit un niveau que personne ne nous
    /// signale, un décompte descend, une barre de progression avance.
    fn repaint_delay(&self, voice: &VoiceSnapshot) -> Option<std::time::Duration> {
        use std::time::Duration;
        /// Rythme des animations. Vingt par seconde suffisent à un vumètre :
        /// l'œil ne suit pas plus vite, et le moteur audio ne produit de
        /// toute façon qu'un niveau toutes les vingt millisecondes.
        const ANIME: Duration = Duration::from_millis(50);

        if !self.welcomed {
            // Écran de connexion.
            if self.connecting {
                // Le décompte du bouton d'annulation descend seconde après
                // seconde.
                return Some(Duration::from_millis(250));
            }
            // Une sonde en cours fait tourner son indicateur.
            if self
                .book
                .iter()
                .any(|s| matches!(self.probes.reach(s.id), servers::Reach::Probing))
            {
                return Some(ANIME);
            }
            // Et sinon, une image par seconde — pas pour l'affichage, pour la
            // **sonde périodique**.
            //
            // Elle est déclenchée depuis le rendu (`probes.sweep`), toutes les
            // vingt secondes. Sans image, pas de déclenchement ; sans
            // déclenchement, pas de résultat ; sans résultat, pas de réveil —
            // et l'état des serveurs se figeait pour de bon sur ce qu'il était
            // à l'ouverture. Une image par seconde, c'est un vingtième de ce
            // que coûtait l'écran de connexion avant, et ça suffit largement à
            // armer une horloge de vingt secondes.
            return Some(Duration::from_secs(1));
        }

        // En vocal : vumètres par locuteur, indicateur d'émission, ping.
        if self.voice_channel.is_some() {
            return Some(ANIME);
        }
        // Quelqu'un parle dans un salon qu'on regarde sans y être : son
        // vumètre s'anime quand même dans la liste des membres.
        if voice.levels.values().any(|l| *l > 0.0) {
            return Some(ANIME);
        }
        // Réglages ouverts : vumètre du micro en direct, statistiques réseau,
        // et la calibration qui mesure cinq secondes durant.
        if self.show_settings {
            return Some(ANIME);
        }
        // Un envoi de fichier en cours affiche sa progression.
        if self.upload_status.lock().unwrap().is_some() {
            return Some(ANIME);
        }
        // Une mise à jour en téléchargement, aussi.
        if matches!(self.updater.status(), update::Status::Downloading { .. }) {
            return Some(ANIME);
        }
        // Un périphérique audio perdu se rouvre en tâche de fond : la
        // bannière doit disparaître d'elle-même quand il revient.
        if voice.device_trouble.0 || voice.device_trouble.1 {
            return Some(Duration::from_millis(500));
        }
        None
    }

    /// Au-delà, « Connexion… » cesse d'attendre.
    ///
    /// Rien ne bornait cette attente : `connecting` n'était levé que par
    /// `Welcome`, `ConnectFailed` ou `Disconnected`, et le keep-alive de cinq
    /// secondes tenait l'expiration d'inactivité de QUIC en échec. Un serveur
    /// qui accepte la connexion puis ne répond plus — surchargé, ou arrêté
    /// entre la poignée de main et l'authentification — laissait donc l'écran
    /// figé pour toujours.
    ///
    /// Vingt secondes : bien au-delà d'un Argon2id sur un VPS chargé, bien en
    /// deçà de la patience de qui que ce soit.
    const DELAI_CONNEXION: std::time::Duration = std::time::Duration::from_secs(20);

    /// Abandonne une connexion qui n'aboutit pas.
    fn check_connect_timeout(&mut self) {
        let Some(depuis) = self.connect_started else { return };
        if self.connecting && depuis.elapsed() >= Self::DELAI_CONNEXION {
            self.connexion_perdue(Some(
                "le serveur n'a pas répondu — vérifie l'adresse, ou réessaie".into(),
            ));
        }
    }

    fn connect(&mut self, ctx: &egui::Context) {
        self.error = None;
        self.connecting = true;
        self.connect_started = Some(std::time::Instant::now());
        let invite = self.invite.trim();
        self.conn = Some(net::connect(
            self.url.trim().to_string(),
            net::Credentials {
                username: self.username.trim().to_string(),
                password: self.password.clone(),
                invite: (!invite.is_empty()).then(|| invite.to_string()),
                // L'empreinte retenue pour ce serveur, s'il est au carnet :
                // c'est elle qui fera refuser un imposteur.
                fingerprint: self
                    .book
                    .iter()
                    .find(|s| s.address == self.url.trim())
                    .map(|s| s.cert_fingerprint.clone())
                    .unwrap_or_default(),
            },
            self.voice_prefs(),
            self.link.clone(),
            ctx.clone(),
        ));
    }

    // -----------------------------------------------------------------
    // Carnet de serveurs
    // -----------------------------------------------------------------

    fn active_server(&self) -> Option<&servers::Server> {
        let id = self.selected?;
        self.book.iter().find(|s| s.id == id)
    }

    /// Nom du serveur courant, pour l'en-tête de la colonne de gauche.
    fn server_label(&self) -> String {
        match self.active_server() {
            Some(server) => server.label().to_string(),
            None => self.url.trim().to_string(),
        }
    }

    /// Bascule sur un serveur : ses identifiants remplissent le formulaire.
    fn select_server(&mut self, id: u64) {
        let Some(server) = self.book.iter().find(|s| s.id == id) else { return };
        self.selected = Some(id);
        self.url = server.address.clone();
        self.username = server.username.clone();
        self.password = server.password().unwrap_or_default();
        self.remember_password = server.secret.is_some();
        self.error = None;
    }

    /// Monte en textures les vignettes reçues depuis le dernier rendu.
    /// Une vignette illisible est simplement abandonnée : le monogramme
    /// prend le relais.
    fn mount_avatars(&mut self, ctx: &egui::Context) {
        for (user_id, (hash, png)) in std::mem::take(&mut self.incoming_avatars) {
            let Some(image) = servers::decode_png(&png) else { continue };
            let texture =
                ctx.load_texture(format!("avatar-{user_id}"), image, egui::TextureOptions::LINEAR);
            self.avatars.insert(user_id, (hash, texture));
        }
    }

    /// Photo de profil d'un membre, si elle est déjà arrivée.
    fn avatar_of(&self, user_id: UserId) -> Option<&egui::TextureHandle> {
        self.avatars.get(&user_id).map(|(_, texture)| texture)
    }

    /// Réclame les photos qui manquent, ou dont l'empreinte a changé. Rien
    /// n'est redemandé tant que l'image en cache est la bonne.
    fn fetch_missing_avatars(&mut self, members: &[Member]) {
        let mut to_download = Vec::new();
        for member in members {
            let Some(hash) = &member.avatar else {
                // Photo retirée côté serveur : on l'oublie ici aussi.
                self.avatars.remove(&member.user_id);
                continue;
            };
            let in_memory = self.avatars.get(&member.user_id).is_some_and(|(k, _)| k == hash);
            if in_memory {
                continue;
            }
            // Le cache disque est adressé par empreinte : si le fichier est
            // là, c'est forcément la bonne version.
            match photos::load(hash) {
                Some(png) => {
                    self.incoming_avatars.insert(member.user_id, (hash.clone(), png));
                }
                None => to_download.push(member.user_id),
            }
        }
        if !to_download.is_empty() {
            self.send(ClientMsg::RequestAvatars { user_ids: to_download });
        }
    }

    /// Texture du logo d'un serveur, montée à la première demande.
    fn server_icon(
        &mut self,
        ctx: &egui::Context,
        server: &servers::Server,
    ) -> Option<egui::TextureHandle> {
        if let Some(cached) = self.icon_textures.get(&server.id) {
            return cached.clone();
        }
        let texture = server.icon.as_deref().and_then(servers::decode_icon).map(|image| {
            ctx.load_texture(
                format!("server-icon-{}", server.id),
                image,
                egui::TextureOptions::LINEAR,
            )
        });
        self.icon_textures.insert(server.id, texture.clone());
        texture
    }

    /// Ouvre le sélecteur de fichier dans un thread : le dialogue natif ne
    /// doit pas figer le rendu.
    ///
    /// Le logo du serveur et la photo de profil ont chacun leur case : avec
    /// une case commune, la fenêtre qui la relevait en premier volait le
    /// fichier choisi par l'autre.
    fn pick_image(&self, ctx: &egui::Context, title: &str, slot: &PickedImage) {
        let slot = slot.clone();
        let title = title.to_string();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let picked = rfd::FileDialog::new()
                .set_title(title)
                .add_filter("Images", &["png", "jpg", "jpeg"])
                .pick_file();
            let Some(path) = picked else { return };
            let outcome = servers::encode_icon(&path).map_err(|e| format!("{e:#}"));
            *slot.lock().unwrap() = Some(outcome);
            ctx.request_repaint();
        });
    }

    fn delete_server(&mut self, id: u64) {
        self.book.retain(|s| s.id != id);
        self.probes.forget(id);
        self.icon_textures.remove(&id);
        if self.selected != Some(id) {
            return;
        }
        self.selected = None;
        match self.book.first().map(|s| s.id) {
            Some(next) => self.select_server(next),
            None => {
                self.url.clear();
                self.username.clear();
                self.password.clear();
            }
        }
    }

    /// Enregistre le formulaire d'édition dans le carnet et sélectionne le
    /// serveur concerné. Renvoie son identifiant.
    fn commit_draft(&mut self, draft: &ServerDraft, ctx: &egui::Context) -> u64 {
        let name = draft.name.trim().to_string();
        let address = draft.address.trim().to_string();
        let id = match draft.id {
            Some(id) => {
                if let Some(server) = self.book.iter_mut().find(|s| s.id == id) {
                    server.name = name;
                    server.address = address;
                }
                id
            }
            None => {
                let id = servers::next_id(&self.book);
                self.book.push(servers::Server {
                    id,
                    name,
                    address,
                    username: String::new(),
                    secret: None,
                    legacy_password: None,
                    last_used: 0,
                    server_name: String::new(),
                    cert_fingerprint: String::new(),
                    icon: None,
                });
                id
            }
        };
        self.select_server(id);
        if let Some(server) = self.book.iter().find(|s| s.id == id) {
            self.probes.probe(server, ctx);
        }
        id
    }

    /// Recopie dans le carnet l'identité annoncée par le serveur, pour que
    /// le lanceur puisse l'afficher même hors ligne. Le client ne fait que
    /// mémoriser : ces deux champs ne se règlent que côté serveur.
    fn cache_server_identity(&mut self) {
        let Some(id) = self.selected else { return };
        let (name, icon) = (self.server_info.name.clone(), self.server_info.icon.clone());
        let changed = match self.book.iter_mut().find(|s| s.id == id) {
            Some(server) => {
                let changed = server.icon != icon;
                server.server_name = name;
                server.icon = icon;
                changed
            }
            None => false,
        };
        if changed {
            self.icon_textures.remove(&id);
        }
    }

    /// Après une connexion réussie : mémorise pseudo, mot de passe (si
    /// demandé) et date, pour que le lanceur rouvre au bon endroit.
    fn remember_connection(&mut self) {
        let Some(id) = self.selected else { return };
        let username = self.username.trim().to_string();
        let password = self.remember_password.then(|| self.password.clone());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if let Some(server) = self.book.iter_mut().find(|s| s.id == id) {
            server.username = username;
            server.set_password(password.as_deref());
            server.last_used = now;
        }
    }

    // -----------------------------------------------------------------
    // Événements réseau
    // -----------------------------------------------------------------

    fn poll_events(&mut self) {
        let Some(conn) = &self.conn else { return };
        let events: Vec<net::Event> = conn.events.try_iter().collect();
        for event in events {
            match event {
                net::Event::ConnectFailed(e) => {
                    // Identité changée : c'est le seul échec où l'utilisateur
                    // a une décision à prendre, et il lui faut de quoi la
                    // prendre. On le dit en clair et l'on propose la sortie,
                    // au lieu de laisser une erreur de bibliothèque et un
                    // carnet qu'aucun écran ne permet de corriger.
                    let changed = e.contains("ApplicationVerificationFailure")
                        || e.contains("invalid peer certificate");
                    if changed {
                        self.identity_alarm = Some(self.url.trim().to_string());
                        self.disconnect(Some(
                            "l'identité du serveur a changé depuis la dernière connexion"
                                .into(),
                        ));
                    } else {
                        self.connexion_perdue(Some(e));
                    }
                }
                net::Event::Disconnected => {
                    let had_error = self.error.take();
                    self.connexion_perdue(
                        had_error.or_else(|| Some("déconnecté du serveur".into())),
                    );
                }
                net::Event::Msg(msg) => self.handle_server_msg(msg),
                net::Event::Fingerprint(fp) => {
                    // Première connexion à ce serveur : on retient son
                    // identité. Les suivantes la compareront, et refuseront
                    // quiconque se présenterait à sa place. Une empreinte
                    // déjà connue ne change pas ici — la connexion aurait
                    // échoué avant si elle avait différé.
                    let address = self.url.trim().to_string();
                    self.server_fingerprint = fp.clone();
                    if let Some(s) = self.book.iter_mut().find(|s| s.address == address) {
                        if s.cert_fingerprint.is_empty() {
                            tracing::info!("empreinte de {address} retenue : {fp}");
                            s.cert_fingerprint = fp;
                        }
                    }
                }
            }
        }
    }

    fn handle_server_msg(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::Welcome {
                user_id,
                voice_token,
                is_admin,
                perms,
                rank,
                roles,
                channels,
                server,
                ..
            } => {
                self.welcomed = true;
                self.connecting = false;
                self.connect_started = None;
                self.error = None;
                self.remember_connection();
                self.my_id = Some(user_id);
                // `is_admin` reste la réponse d'un serveur antérieur aux
                // rôles : sans permissions annoncées, on lui accorde tout
                // plutôt que rien, sans quoi son propre admin perdrait son
                // panneau en mettant le client à jour.
                self.my_perms = if perms == 0 && is_admin {
                    ki_protocol::perm::ADMINISTRATOR
                } else if perms == 0 {
                    ki_protocol::perm::DEFAULT
                } else {
                    perms
                };
                self.my_rank = rank;
                self.roles = roles;
                self.voice_token = voice_token;
                self.channels = channels;
                // L'identité du serveur arrive **dès** le Welcome. L'ignorer
                // ici laissait nom et logo invisibles jusqu'à ce qu'un admin
                // les ré-enregistre, ce qui n'arrivait jamais.
                self.server_info = safe_server_info(server);
                self.cache_server_identity();
                if !self.show_admin {
                    self.admin_name = self.server_info.name.clone();
                }
                // Reprise réussie : on rend la place qu'on occupait, salon lu
                // **et** salon vocal. Se reconnecter tout seul pour se
                // réveiller ailleurs, muet, en pleine partie, ne vaudrait
                // guère mieux que de ne pas se reconnecter.
                //
                // Le vocal est la seule exception au « on n'entre dans aucun
                // vocal, ça se décide » d'en dessous : ici, c'était décidé —
                // c'est le réseau qui en a décidé autrement.
                if let Some(r) = self.reprise.take() {
                    let salon = r.salon.filter(|c| self.channels.iter().any(|k| k.id == *c));
                    let vocal = r.vocal.filter(|c| self.channels.iter().any(|k| k.id == *c));
                    if let Some(c) = salon {
                        self.join(c);
                    }
                    if let Some(c) = vocal {
                        // Un salon devenu verrouillé pendant la coupure
                        // refusera : le client demande alors le mot de passe,
                        // comme pour une entrée ordinaire.
                        self.join_voice(c);
                    }
                    self.info = Some("connexion rétablie".into());
                    if salon.is_some() {
                        return;
                    }
                }
                // On ouvre le premier salon textuel pour avoir de quoi lire,
                // mais on n'entre dans aucun vocal : ça se décide.
                let first_text = self
                    .channels
                    .iter()
                    .find(|c| c.kind == ChannelKind::Text)
                    .map(|c| c.id);
                if let Some(first) = first_text {
                    self.join(first);
                }
            }
            ServerMsg::Chat {
                user_id,
                username,
                text,
                ts,
            } => {
                // Être **nommé** n'est pas un message de plus : ça appelle une
                // réponse. On prévient donc même fenêtre au premier plan, là où
                // un message ordinaire ne le fait pas.
                let pour_moi = Some(user_id) != self.my_id
                    && self.my_pseudo().is_some_and(|moi| {
                        let membres: Vec<&str> =
                            self.members.iter().map(|m| m.username.as_str()).collect();
                        markup::me_mentionne(&text, &membres, moi)
                    });
                // Jamais de son pour ses propres messages ; et pour ceux des
                // autres, seulement quand la fenêtre n'a pas le focus — en
                // pleine conversation, un bip par message serait insupportable.
                // La barre des tâches clignote en prime : le « quoi de neuf »
                // se voit même en jeu.
                if Some(user_id) != self.my_id && (pour_moi || !self.window_focused) {
                    self.play_sfx(sfx::MESSAGE);
                    self.wants_attention = true;
                }
                self.messages.push(ChatRecord {
                    user_id,
                    username,
                    text,
                    ts,
                });
                if self.messages.len() > 500 {
                    self.messages.remove(0);
                }
            }
            ServerMsg::History { messages } => {
                self.messages = messages.into_iter().map(clean_record).collect();
            }
            ServerMsg::SearchResults { query, hits, more } => {
                // Une réponse à une requête qu'on ne pose plus est jetée : le
                // serveur relit des fichiers, et la réponse à « val » peut
                // très bien arriver après celle à « valorant ».
                if self.search_envoyee.as_deref() != Some(query.as_str()) {
                    return;
                }
                self.search_envoyee = None;
                self.search_hits = hits;
                self.search_more = more;
            }
            ServerMsg::HistoryPage { messages, more, channel } => {
                // Une page d'un autre salon est jetée : le serveur relit le
                // fichier hors de l'ordre du flux, si bien qu'elle peut
                // arriver après un changement de salon. L'appliquer collerait
                // une conversation en tête d'une autre, et écraserait au
                // passage le « reste-t-il du passé » du salon courant.
                // `0` = serveur antérieur, qui ne renseigne pas ce champ.
                if channel != 0 && self.current != Some(channel) {
                    return;
                }
                self.history_pending = false;
                self.history_more = more;
                if messages.is_empty() {
                    return;
                }
                // En tête, et surtout sans doublon : une page peut recouvrir
                // ce qui est déjà affiché si des messages sont arrivés entre
                // la demande et la réponse.
                let known: std::collections::HashSet<(UserId, u64)> =
                    self.messages.iter().map(|m| (m.user_id, m.ts)).collect();
                let mut older: Vec<ChatRecord> = messages
                    .into_iter()
                    .map(clean_record)
                    .filter(|m| !known.contains(&(m.user_id, m.ts)))
                    .collect();
                if older.is_empty() {
                    return;
                }
                // La hauteur d'avant est retenue : le contenu va grandir vers
                // le haut, et le prochain rendu rattrapera le décalage pour
                // que la ligne en cours de lecture ne bouge pas d'un pixel.
                self.history_anchor = Some(self.chat_height);
                older.append(&mut self.messages);
                self.messages = older;
            }
            ServerMsg::Members { members } => {
                self.fetch_missing_avatars(&members);
                self.members = members
                    .into_iter()
                    .map(|mut member| {
                        member.username = safe_name(&member.username);
                        member
                    })
                    .collect();
                // La couleur de chaque auteur est résolue **ici**, une fois
                // par roster, et plus dans le fil à chaque message et à
                // chaque image. Changer un rôle recolore donc toujours les
                // anciens messages : le serveur rediffuse la liste.
                self.author_colors = self
                    .members
                    .iter()
                    .map(|m| (m.user_id, theme::member_color(m.color, &m.username)))
                    .collect();
                self.after_roster_change();
            }
            ServerMsg::MemberUpdate { member } => {
                // Une seule fiche a changé : on la fusionne au lieu de
                // remplacer la liste. Le serveur envoyait le roster entier —
                // tous les comptes non bannis, pas seulement les connectés —
                // à chaque entrée et sortie de vocal.
                let mut member = member;
                member.username = safe_name(&member.username);
                self.fetch_missing_avatars(std::slice::from_ref(&member));
                self.author_colors.insert(
                    member.user_id,
                    theme::member_color(member.color, &member.username),
                );
                match self.members.iter_mut().find(|m| m.user_id == member.user_id) {
                    Some(place) => *place = member,
                    None => {
                        // Nouveau venu : inséré à sa place, la liste étant
                        // triée par pseudo comme le serveur la produit. La
                        // retrier entièrement ferait le travail qu'on vient
                        // d'éviter.
                        let cle = member.username.to_lowercase();
                        let pos = self
                            .members
                            .partition_point(|m| m.username.to_lowercase() < cle);
                        self.members.insert(pos, member);
                    }
                }
                self.after_roster_change();
            }
            ServerMsg::UserJoined { user_id, .. } => {
                // La liste complète suit immédiatement dans un `Members` :
                // ici on ne fait que réclamer la photo du nouveau venu.
                if !self.avatars.contains_key(&user_id) {
                    self.send(ClientMsg::RequestAvatars { user_ids: vec![user_id] });
                }
            }
            ServerMsg::UserLeft { user_id } => {
                self.members.retain(|m| m.user_id != user_id);
            }
            ServerMsg::VoiceState { user_id, speaking, muted } => {
                if let Some(m) = self.members.iter_mut().find(|m| m.user_id == user_id) {
                    m.speaking = speaking;
                    m.muted = muted;
                }
            }
            ServerMsg::Error { message } => {
                let message = ki_protocol::safe_display(&message, 300);
                // Avant le Welcome, une erreur = échec de connexion (jeton...).
                if !self.welcomed {
                    self.disconnect(Some(message));
                } else {
                    // Rien n'est défait ici. Une erreur n'est pas rattachable
                    // à la demande qui l'a provoquée — « tu écris trop vite »
                    // arrive dans la même fenêtre qu'une entrée en vocal — et
                    // s'en servir pour sortir du salon éjectait sur une erreur
                    // sans rapport. Si le serveur a refusé l'entrée, il ne
                    // nous listera pas dedans : la fenêtre d'intention expire,
                    // et sa liste des membres nous remet d'aplomb.
                    self.error = Some(message);
                }
            }
            ServerMsg::Kicked { reason } => {
                // Le motif vient d'un autre membre : il passe par le même
                // nettoyage que n'importe quel texte reçu avant affichage.
                let reason = ki_protocol::safe_display(&reason, 300);
                self.disconnect(Some(if reason.is_empty() {
                    "tu as été expulsé par un admin".into()
                } else {
                    format!("expulsé par un admin : {reason}")
                }));
            }
            ServerMsg::AdminInfo { users, invites } => {
                self.admin_users = users;
                self.admin_invites = invites;
            }
            ServerMsg::AuditLog { records } => {
                self.audit = records;
            }
            ServerMsg::Perms { perms, rank, .. } => {
                // Pas de repli sur DEFAULT ici, contrairement au Welcome : ce
                // message n'existe que sur un serveur qui connaît les rôles,
                // et `perms == 0` y est un état légitime — celui de qui vient
                // de tout se faire retirer. S'accorder DEFAULT reviendrait à
                // afficher des actions que le serveur refuse.
                self.my_perms = perms;
                self.my_rank = rank;
                // Un panneau ouvert sur des actions qu'on vient de perdre
                // n'aboutirait plus : on le referme plutôt que de le laisser
                // proposer des boutons qui échouent.
                if !self.any_admin_power() {
                    self.close_admin();
                }
            }
            ServerMsg::Roles { roles } => {
                self.roles = roles;
            }
            ServerMsg::ChannelsUpdated { channels } => {
                self.channels = channels;
                // Le salon lu a pu disparaître, ou m'être retiré : on se
                // rabat sur le premier salon textuel encore visible plutôt
                // que de laisser une vue morte.
                let still_there =
                    self.current.is_some_and(|c| self.channels.iter().any(|ch| ch.id == c));
                if !still_there {
                    let first = self
                        .channels
                        .iter()
                        .find(|c| c.kind == ChannelKind::Text)
                        .map(|c| c.id);
                    match first {
                        Some(id) => self.join(id),
                        None => {
                            self.current = None;
                            self.messages.clear();
                        }
                    }
                }
                // Idem pour le vocal : on ne reste pas dans un salon qu'on
                // ne voit plus, sinon on parle à des gens qu'on ne voit pas.
                if self.voice_channel.is_some_and(|c| !self.channels.iter().any(|ch| ch.id == c))
                {
                    self.voice_channel = None;
                    self.armed = false;
                    self.transmitting = false;
                    self.info = Some("le salon vocal a été fermé".into());
                }
            }
            ServerMsg::VoiceLocked { channel, wrong } => {
                self.voice_channel = None;
                self.voice_intent = None;
                self.voice_prompt = Some(VoicePrompt {
                    channel,
                    password: String::new(),
                    wrong,
                });
            }
            ServerMsg::InviteCreated { code } => {
                self.last_invite = Some(code);
            }
            ServerMsg::Info { message } => {
                self.info = Some(ki_protocol::safe_display(&message, 300));
            }
            ServerMsg::ServerInfo { server } => {
                // Un admin vient de changer le nom ou le logo : tout le
                // monde le reçoit et le carnet se met à jour.
                self.server_info = safe_server_info(server);
                self.cache_server_identity();
                if !self.show_admin {
                    self.admin_name = self.server_info.name.clone();
                }
            }
            ServerMsg::Avatar { user_id, hash, data } => match data {
                // Le montage en texture demande le contexte egui : la
                // vignette attend ici, `mount_avatars` la reprendra au
                // prochain rendu. Elle part aussi au cache disque, pour ne
                // plus jamais la retélécharger tant qu'elle ne change pas.
                Some(data) => {
                    // On ne fait pas confiance au serveur non plus : la
                    // vignette est contrôlée avant d'atterrir sur le disque.
                    match servers::decode_base64(&data)
                        .filter(|png| ki_protocol::check_png(png).is_ok())
                    {
                        Some(png) => {
                            photos::store(&hash, &png);
                            self.incoming_avatars.insert(user_id, (hash, png));
                        }
                        None => tracing::warn!("photo de {user_id} refusée : vignette invalide"),
                    }
                }
                None => {
                    self.avatars.remove(&user_id);
                    self.incoming_avatars.remove(&user_id);
                }
            },
            ServerMsg::NetQuality { loss_pct } => {
                self.on_net_quality(loss_pct);
            }
            ServerMsg::Pong => {}
        }
    }

    /// Envoie au serveur les diagnostics accumulés depuis le dernier envoi :
    /// méta (version, système), nouvelles lignes du journal audio, et — au
    /// premier envoi de la session ou sur demande — le rapport du docteur.
    ///
    /// C'est TOUT ce qui transite, et rien d'autre : pas un message, pas une
    /// trame audio. Le lot part en JSONL sur le canal HTTPS épinglé du
    /// serveur (même authentification que le partage de fichiers), dans un
    /// thread — l'interface ne l'attend jamais.
    fn flush_diag(&mut self, manual: bool) {
        if self.conn.is_none() {
            return;
        }
        let journal = ki_voice::journal_snapshot();
        let fresh: Vec<&(u64, String)> =
            journal.iter().filter(|(t, _)| *t > self.diag_last_sent_ts).collect();
        if fresh.is_empty() && !manual {
            return;
        }
        let now_ms = chrono::Local::now().timestamp_millis().max(0) as u64;
        let mut lot = String::new();
        lot.push_str(&format!(
            "{{\"type\":\"meta\",\"t\":{now_ms},\"version\":\"{}\",\"os\":\"{} {}\"}}\n",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
        ));
        for (t, msg) in &fresh {
            lot.push_str(&format!(
                "{{\"type\":\"journal\",\"t\":{t},\"msg\":{}}}\n",
                serde_json::Value::String((*msg).clone())
            ));
        }
        // Le docteur énumère les processus : pas de quoi le faire chaque
        // minute. Au premier envoi et à la demande, c'est là qu'il compte.
        if manual || self.diag_last_sent_ts == 0 {
            if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
                lot.push_str(&format!(
                    "{{\"type\":\"docteur\",\"t\":{now_ms},\"rapport\":{}}}\n",
                    serde_json::Value::String(engine.docteur().rapport())
                ));
            }
        }
        if let Some((t, _)) = journal.last() {
            self.diag_last_sent_ts = *t;
        }
        let base = self.http_base();
        let agent = self.http_agent();
        let token_hex = format!("{:x}", self.voice_token);
        std::thread::spawn(move || {
            if let Err(e) = agent
                .post(&format!("{base}/diag"))
                .set("x-ki-token", &token_hex)
                .send_string(&lot)
            {
                // Raté silencieux : le diagnostic est un luxe, pas une
                // fonction — la prochaine minute réessaiera avec le cumul.
                tracing::debug!("envoi du diagnostic raté : {e}");
            }
        });
    }

    /// La cadence du diagnostic partagé : dix minutes, et seulement s'il y a
    /// du neuf. Appelée à chaque image ; le garde-fou coûte deux lectures.
    fn maybe_flush_diag(&mut self) {
        if !self.diag_share || self.conn.is_none() {
            return;
        }
        let due = self
            .diag_last_flush
            .map(|t| t.elapsed() > std::time::Duration::from_secs(600))
            .unwrap_or(true);
        if !due {
            return;
        }
        self.diag_last_flush = Some(std::time::Instant::now());
        self.flush_diag(false);
    }

    /// Sélection d'un fichier puis upload, dans un thread (le dialogue
    /// natif et l'envoi ne doivent pas bloquer l'UI).
    fn start_upload(&self) {
        let Some(conn) = &self.conn else { return };
        let sender = conn.sender();
        let base = self.http_base();
        let agent = self.http_agent();
        let token_hex = format!("{:x}", self.voice_token);
        let status = self.upload_status.clone();
        std::thread::spawn(move || {
            let Some(path) = rfd::FileDialog::new().pick_file() else { return };
            let name: String = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "fichier".into())
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            *status.lock().unwrap() = Some(format!("envoi de {name}…"));
            let result = (|| -> Result<String, String> {
                let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                if bytes.len() > 25 * 1024 * 1024 {
                    return Err("fichier trop gros (25 Mo max)".into());
                }
                let resp = agent.post(&format!("{base}/upload?name={name}"))
                    .set("x-ki-token", &token_hex)
                    .send_bytes(&bytes)
                    .map_err(|e| e.to_string())?;
                let json: serde_json::Value = resp.into_json().map_err(|e| e.to_string())?;
                let file_path = json["url"].as_str().ok_or("réponse invalide")?.to_string();
                Ok(format!("{base}{file_path}"))
            })();
            match result {
                Ok(url) => {
                    let _ = sender.send(net::Cmd::Send(ClientMsg::Chat { text: url }));
                    *status.lock().unwrap() = None;
                }
                Err(e) => *status.lock().unwrap() = Some(format!("échec de l'envoi : {e}")),
            }
        });
    }

    /// Lance la calibration : mesure le niveau ambiant (autres voix, bruit)
    /// pendant 5 s, micro « nu » (porte et AGC suspendus le temps de mesurer).
    fn start_calibration(&mut self) {
        {
            if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
                engine.set_gate_threshold(0.0);
                engine.set_agc(false);
            }
        }
        self.calibrating = Some((std::time::Instant::now(), 0.0));
    }

    /// Fait avancer la calibration ; à la fin, règle les seuils juste
    /// au-dessus du niveau ambiant mesuré et restaure la chaîne.
    fn tick_calibration(&mut self, mic_peak: f32) {
        const CALIB_SECS: f32 = 5.0;
        let Some((start, peak)) = &mut self.calibrating else { return };
        *peak = peak.max(mic_peak);
        if start.elapsed().as_secs_f32() < CALIB_SECS {
            return;
        }
        let ambient = *peak;
        self.calibrating = None;
        self.gate_threshold = (ambient * 1.4).clamp(0.004, 0.10);
        if self.mode == MicMode::Vad {
            self.vad_threshold = (ambient * 1.8).clamp(0.01, 0.25);
        }
        self.apply_audio_settings();
        self.info = Some(format!(
            "calibré : ambiance {:.1} % → porte de bruit {:.1} %{}",
            ambient * 100.0,
            self.gate_threshold * 100.0,
            if self.mode == MicMode::Vad {
                format!(", seuil d'activation {:.1} %", self.vad_threshold * 100.0)
            } else {
                String::new()
            }
        ));
    }

    // -----------------------------------------------------------------
    // Vocal : décision d'émission (mode ouvert / PTT global)
    // -----------------------------------------------------------------

    fn update_voice(&mut self) {
        let engine_guard = self.link.engine.lock().unwrap();
        let Some(engine) = engine_guard.as_ref() else { return };

        // « Armé » : le micro a le droit d'émettre. En activation vocale,
        // c'est ensuite le moteur qui décide selon le seuil.
        // La touche est lue par le fil dédié, à cent hertz, et le maintien
        // après relâchement y est calculé aussi. Ici on ne fait plus que lire
        // le verdict : plus aucune raison de repeindre pour surveiller un
        // clavier.
        let ptt_active =
            self.mode == MicMode::Ptt && self.ptt.as_ref().is_some_and(|w| w.active());
        // Hors d'un salon vocal, le micro reste fermé quoi qu'il arrive.
        let armed = !self.muted
            && self.voice_channel.is_some()
            && (matches!(self.mode, MicMode::Open | MicMode::Vad) || ptt_active);
        // Le moteur est réglé **à chaque image**, et non sur la seule
        // transition. `self.armed` était écrit hors d'ici — en quittant le
        // vocal, sur une erreur, à la disparition d'un salon — si bien que
        // l'état local et celui du moteur pouvaient diverger sans jamais se
        // retrouver : le micro restait ouvert alors que l'interface le
        // montrait fermé, et l'on continuait d'être entendu. L'écriture est
        // un simple stockage atomique, la refaire à chaque image ne coûte rien.
        self.armed = armed;
        engine.set_transmit(armed);

        // Émission réelle (après VAD) : indicateur TX + diffusion aux autres.
        let sending = engine.is_sending();
        drop(engine_guard);
        if sending != self.transmitting {
            self.transmitting = sending;
            self.send(ClientMsg::VoiceState { speaking: sending, muted: self.muted });
        }
    }

    // -----------------------------------------------------------------
    // Écran de connexion
    // -----------------------------------------------------------------

    fn login_screen(&mut self, ctx: &egui::Context) {
        // Teste les serveurs en tâche de fond, au plus une fois toutes les
        // 20 s : l'état et le ping sont connus avant même de se connecter.
        self.probes.sweep(&self.book, ctx, false);

        let editing = self.draft.is_some();
        let ready = self.selected.is_some()
            && !self.username.trim().is_empty()
            && !self.password.is_empty()
            && !self.connecting;
        let enter = !editing && ctx.input(|i| i.key_pressed(egui::Key::Enter));
        let mut go = false;

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::BG_DEEP))
            .show(ctx, |ui| {
                const CARD: f32 = 420.0;
                ui.vertical_centered(|ui| {
                    // Bloc centré, très légèrement remonté (centre optique).
                    let free = ui.available_height();
                    ui.add_space(((free - 620.0) * 0.40).max(16.0));
                    brand(ui);
                    ui.add_space(22.0);

                    // Le parent centre ses enfants ; à l'intérieur de la carte
                    // on repasse en alignement à gauche pour les étiquettes.
                    ui.allocate_ui_with_layout(
                        Vec2::new(CARD, 0.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_width(CARD);
                            ui::card(ui, |ui| {
                                if editing {
                                    self.server_editor(ui, ctx);
                                } else {
                                    self.server_picker(ui, ctx);
                                    ui.add_space(18.0);
                                    go = self.credentials(ui, ready);
                                }
                            });

                            // Une coupure en cours de reprise n'est pas une
                            // erreur à congédier : c'est un état, et il dit
                            // ce qu'il attend. Le rouge est réservé à ce sur
                            // quoi il faut agir.
                            if let Some(dans) = self.reprise_dans() {
                                let essais =
                                    self.reprise.as_ref().map_or(0, |r| r.essais);
                                ui.add_space(12.0);
                                let quoi = if self.connecting {
                                    format!("Connexion perdue — tentative {essais}…")
                                } else {
                                    format!(
                                        "Connexion perdue — nouvelle tentative dans {} s \
                                         (essai {essais})",
                                        dans.as_secs() + 1
                                    )
                                };
                                ui::banner(ui, Tone::Warn, &quoi, false);
                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    if ui::button(ui, Icon::Refresh, "Réessayer maintenant")
                                        .clicked()
                                    {
                                        if let Some(r) = &mut self.reprise {
                                            r.quand = std::time::Instant::now();
                                        }
                                    }
                                    // Renoncer rend la main sans rien perdre :
                                    // le formulaire est déjà rempli.
                                    if ui::button(ui, Icon::Close, "Arrêter d'essayer")
                                        .clicked()
                                    {
                                        // Renoncer rend aussi les
                                        // périphériques : les tenir pour une
                                        // connexion qu'on ne cherche plus
                                        // n'apporte rien à personne.
                                        self.reprise = None;
                                        self.link.arreter();
                                    }
                                });
                            } else if let Some(err) = self.error.clone() {
                                ui.add_space(12.0);
                                if ui::banner(ui, Tone::Danger, &err, true) {
                                    self.error = None;
                                }
                            }
                            // Identité changée : soit le serveur a été
                            // réinstallé, soit quelqu'un se glisse au milieu.
                            // Le client ne peut pas trancher — l'utilisateur,
                            // lui, sait si l'hébergeur a refait son serveur.
                            if let Some(address) = self.identity_alarm.clone() {
                                ui.add_space(8.0);
                                ui.label(
                                    RichText::new(
                                        "Si le serveur vient d'être réinstallé, c'est \
                                         normal. Sinon, quelqu'un s'interpose : demande \
                                         son empreinte à l'hébergeur avant d'accepter.",
                                    )
                                    .color(TEXT_DIM)
                                    .size(11.5),
                                );
                                ui.add_space(6.0);
                                if ui::button(
                                    ui,
                                    Icon::Key,
                                    "Accepter la nouvelle identité du serveur",
                                )
                                .clicked()
                                {
                                    if let Some(s) = self
                                        .book
                                        .iter_mut()
                                        .find(|s| s.address == address)
                                    {
                                        s.cert_fingerprint.clear();
                                    }
                                    self.identity_alarm = None;
                                    self.error = None;
                                }
                            }
                        },
                    );

                    ui.add_space(16.0);
                    ui.label(
                        RichText::new(concat!("ki-chat ", env!("CARGO_PKG_VERSION")))
                            .color(theme::BORDER)
                            .size(11.0),
                    );
                });
            });

        if go || (enter && ready) {
            self.connect(ctx);
        }
    }

    /// Liste des serveurs connus, avec leur état mesuré.
    fn server_picker(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui::section_label(ui, "Serveurs");
            if !self.book.is_empty() {
                ui.label(
                    RichText::new(self.book.len().to_string())
                        .color(theme::BORDER_STRONG)
                        .size(11.0)
                        .strong(),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui::icon_button_ex(ui, Icon::Plus, 26.0, "Ajouter un serveur", Some(ACCENT))
                    .clicked()
                {
                    self.draft = Some(ServerDraft::new());
                }
                if !self.book.is_empty()
                    && ui::icon_button_ex(ui, Icon::Refresh, 26.0, "Retester les serveurs", None)
                        .clicked()
                {
                    self.probes.sweep(&self.book, ctx, true);
                }
            });
        });
        ui.add_space(2.0);

        if self.book.is_empty() {
            let mut add = false;
            ui.add_space(8.0);
            ui.vertical_centered(|ui| {
                ui::glyph(ui, Icon::Server, 30.0, theme::BORDER);
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Aucun serveur enregistré").color(TEXT_DIM).size(13.5),
                );
                ui.add_space(2.0);
                ui::hint(ui, "Demande son adresse à l'admin du serveur.");
                ui.add_space(12.0);
                add = ui::primary_button(ui, Some(Icon::Plus), "Ajouter un serveur", None)
                    .clicked();
            });
            if add {
                self.draft = Some(ServerDraft::new());
            }
            ui.add_space(6.0);
            return;
        }

        let book = self.book.clone();
        for server in &book {
            let selected = self.selected == Some(server.id);
            let reach = self.probes.reach(server.id);
            let icon = self.server_icon(ctx, server);
            match server_row(ui, server, selected, reach, icon.as_ref()) {
                RowAction::Select => self.select_server(server.id),
                RowAction::Edit => self.draft = Some(ServerDraft::edit(server)),
                RowAction::None => {}
            }
            // Pas d'espace ajouté ici : la mise en page verticale intercale
            // déjà `item_spacing.y` entre deux lignes.
        }
    }

    /// Formulaire d'ajout / modification d'un serveur.
    fn server_editor(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(mut draft) = self.draft.take() else { return };
        let creating = draft.id.is_none();

        ui::group_title(
            ui,
            if creating { Icon::Plus } else { Icon::Pencil },
            if creating { "Nouveau serveur" } else { "Modifier le serveur" },
        );

        ui::field_label(ui, "Adresse");
        ui.add(ui::text_field(&mut draft.address, "hôte ou hôte:port", false));
        ui.add_space(4.0);
        ui::hint(ui, "Port 9987 par défaut.");

        ui.add_space(12.0);
        ui::field_label(ui, "Alias local (facultatif)");
        ui.add(ui::text_field(&mut draft.name, "ex. Chez Kévin", false));
        ui.add_space(4.0);
        // Le nom et le logo appartiennent au serveur ; l'alias n'est qu'un
        // pense-bête local, et le logo n'est pas modifiable ici du tout.
        ui::hint(
            ui,
            "Ne change l'affichage que sur cet ordinateur. Le nom et le logo \
             officiels sont ceux définis par les admins du serveur.",
        );

        let valid = !draft.address.trim().is_empty();
        let submit = valid && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));

        ui.add_space(14.0);
        let mut close = false;
        ui.horizontal(|ui| {
            let save = ui
                .add_enabled_ui(valid, |ui| {
                    ui::primary_button(ui, Some(Icon::Check), "Enregistrer", None)
                })
                .inner
                .clicked();
            if save || submit {
                self.commit_draft(&draft, ctx);
                close = true;
            }
            if ui::button(ui, Icon::Close, "Annuler").clicked() || cancel {
                close = true;
            }
            if let Some(id) = draft.id {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui::tinted_button(ui, Some(Icon::Trash), "Supprimer", Tone::Danger).clicked()
                    {
                        self.delete_server(id);
                        close = true;
                    }
                });
            }
        });

        if !close {
            self.draft = Some(draft);
        }
    }

    /// Identifiants du serveur sélectionné. Renvoie `true` si l'utilisateur
    /// a demandé la connexion.
    fn credentials(&mut self, ui: &mut egui::Ui, ready: bool) -> bool {
        // « Enregistré » posé dans le champ : on voit d'un coup d'œil que les
        // identifiants viennent du coffre et pas d'une saisie oubliée.
        let stored = self.active_server().is_some_and(|s| s.secret.is_some());
        ui::field_label(ui, "Pseudo");
        let field = ui.add(ui::text_field(&mut self.username, "ton pseudo", false));
        if stored {
            let galley = ui.fonts(|f| {
                f.layout_no_wrap(
                    "ENREGISTRÉ".into(),
                    egui::FontId::proportional(9.5),
                    theme::alpha(ACCENT, 190),
                )
            });
            let rect = field.rect;
            ui.painter().galley(
                egui::pos2(
                    rect.right() - 11.0 - galley.size().x,
                    rect.center().y - galley.size().y / 2.0,
                ),
                galley,
                ACCENT,
            );
        }
        ui.add_space(10.0);

        ui::field_label(ui, "Mot de passe");
        ui.add(ui::text_field(&mut self.password, "ton mot de passe", true));
        ui.add_space(10.0);

        // Sans coffre natif, on ne propose pas de mémoriser : pas de repli
        // en clair, la case est simplement inerte.
        let vault = secret::available();
        if !vault {
            self.remember_password = false;
        }
        let mut open_invite = false;
        ui.horizontal(|ui| {
            ui.add_enabled_ui(vault, |ui| {
                ui.checkbox(&mut self.remember_password, "Retenir le mot de passe")
                    .on_hover_text(if vault {
                        "Chiffré par Windows avec une clé dérivée de ta session : \
                         illisible sur une autre machine ou sous un autre compte, \
                         même si le fichier de configuration est copié."
                    } else {
                        "Indisponible : aucun coffre à secrets sur cette plateforme."
                    });
            });
            // Le code d'invitation ne sert qu'une fois dans la vie d'un
            // compte : il tient sur la même ligne, en retrait.
            if !self.show_invite {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    open_invite = ui
                        .add(
                            egui::Label::new(
                                RichText::new("Code d'invitation").color(INFO).size(12.0),
                            )
                            .sense(Sense::click()),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text("Nécessaire uniquement à la première connexion")
                        .clicked();
                });
            }
        });
        if open_invite {
            self.show_invite = true;
        }
        if self.show_invite {
            ui.add_space(10.0);
            ui::field_label(ui, "Code d'invitation");
            ui.add(ui::text_field(&mut self.invite, "fourni par un admin", false));
            ui::hint(ui, "Nécessaire uniquement à la première connexion.");
        }

        ui.add_space(16.0);

        // Pendant une tentative, le bouton devient une **annulation**.
        //
        // Attendre était la seule option offerte, et l'attente n'était pas
        // bornée : quand le serveur accepte la connexion puis ne répond plus,
        // l'écran restait figé sur « Connexion… » sans rien à cliquer. Le
        // délai de `DELAI_CONNEXION` finit par trancher, mais vingt secondes
        // sans pouvoir revenir en arrière, c'est vingt secondes de trop quand
        // on vient de taper la mauvaise adresse.
        if self.connecting {
            let reste = self
                .connect_started
                .map(|d| Self::DELAI_CONNEXION.saturating_sub(d.elapsed()).as_secs() + 1)
                .unwrap_or(0);
            let annule = ui
                .add_sized(
                    [ui.available_width(), 38.0],
                    egui::Button::new(
                        RichText::new(format!("Connexion…  ✕  ({reste} s)")).size(14.0),
                    ),
                )
                .on_hover_text("Annuler la connexion")
                .clicked();
            if annule {
                self.disconnect(None);
            }
            // Jamais « se connecter » : la tentative est déjà en cours.
            return false;
        }

        // Le bouton nomme sa destination : avec plusieurs serveurs, ça évite
        // de se connecter au mauvais sans s'en rendre compte.
        let label = match self.active_server() {
            Some(server) => format!("Se connecter à {}", ellipsize(server.label(), 26)),
            None => "Se connecter".to_string(),
        };
        ui.add_enabled_ui(ready, |ui| {
            ui::primary_button(ui, None, &label, Some(ui.available_width()))
        })
        .inner
        .clicked()
    }

    // -----------------------------------------------------------------
    // Écran principal
    // -----------------------------------------------------------------

    fn main_screen(&mut self, ctx: &egui::Context, voice: &VoiceSnapshot) {
        self.mount_avatars(ctx);
        self.previews.set_origin(self.http_base());
        self.previews.set_agent(self.http_agent());
        self.previews.mount(ctx);

        self.voice_bar(ctx, voice);
        self.sidebar(ctx, voice);
        self.roster_panel(ctx, voice);
        self.chat_panel(ctx);
        self.comms_popup(ctx, voice);

        if self.show_settings {
            self.settings_window(ctx, voice);
        }
        if self.show_admin {
            self.admin_window(ctx);
        }
        if self.show_search {
            self.search_window(ctx);
        }
        if self.show_account {
            self.account_window(ctx);
        }
        if self.ban_draft.is_some() {
            self.ban_window(ctx);
        }
        if self.voice_prompt.is_some() {
            self.voice_password_window(ctx);
        }
        if self.labo.is_some() {
            self.labo_window(ctx);
        }
    }

    /// Demande le mot de passe d'un salon vocal verrouillé.
    fn voice_password_window(&mut self, ctx: &egui::Context) {
        let Some(prompt) = self.voice_prompt.as_mut() else { return };
        let channel = prompt.channel;
        let name = self
            .channels
            .iter()
            .find(|c| c.id == channel)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        let wrong = prompt.wrong;
        let mut open = true;
        let mut submit = false;
        let mut cancel = false;

        egui::Window::new(format!("Salon verrouillé — {name}"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(300.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                if wrong {
                    ui::banner(ui, Tone::Danger, "Mot de passe incorrect", false);
                    ui.add_space(6.0);
                }
                ui::field_label(ui, "Mot de passe du salon");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut prompt.password)
                        .password(true)
                        .margin(egui::Margin::symmetric(10, 7))
                        .desired_width(f32::INFINITY),
                );
                response.request_focus();
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui::primary_button(ui, Some(Icon::Check), "Entrer", None).clicked() {
                        submit = true;
                    }
                    if ui::button(ui, Icon::Close, "Annuler").clicked() {
                        cancel = true;
                    }
                });
            });

        if submit {
            if let Some(prompt) = self.voice_prompt.take() {
                // Même intention que par le clic ordinaire : sans elle, un
                // refus laissait l'affichage dans un salon où l'on n'est pas.
                self.voice_channel = Some(channel);
                self.set_voice_intent(Some(channel));
                self.send(ClientMsg::JoinVoice {
                    channel,
                    password: Some(prompt.password),
                });
            }
        } else if cancel || !open {
            self.voice_prompt = None;
        }
    }

    /// Saisie d'un bannissement : motif et durée.
    ///
    /// Une fenêtre plutôt qu'un menu : bannir se justifie, et le motif est
    /// relu par la personne bannie comme par les autres admins dans le
    /// journal. Lui demander de le taper à la volée dans un menu contextuel
    /// donnerait des motifs vides.
    fn ban_window(&mut self, ctx: &egui::Context) {
        let Some(draft) = self.ban_draft.as_mut() else { return };
        let username = draft.username.clone();
        let mut open = true;
        let mut confirm = false;
        let mut cancel = false;

        egui::Window::new(format!("Bannir {username}"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(320.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui::field_label(ui, "Motif (visible par la personne bannie)");
                ui.add(ui::text_field(&mut draft.reason, "ex. spam répété", false));
                ui.add_space(8.0);

                ui::field_label(ui, "Durée");
                ui.horizontal_wrapped(|ui| {
                    for (label, secs) in BAN_DURATIONS {
                        if ui
                            .selectable_label(draft.duration_secs == *secs, *label)
                            .clicked()
                        {
                            draft.duration_secs = *secs;
                        }
                    }
                });
                ui.add_space(10.0);
                ui::hairline(ui);
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    if ui::tinted_button(ui, Some(Icon::Ban), "Bannir", Tone::Danger).clicked() {
                        confirm = true;
                    }
                    if ui::button(ui, Icon::Close, "Annuler").clicked() {
                        cancel = true;
                    }
                });
            });

        if confirm {
            if let Some(draft) = self.ban_draft.take() {
                self.send(ClientMsg::AdminBan {
                    username: draft.username,
                    reason: draft.reason,
                    duration_secs: draft.duration_secs,
                });
            }
        } else if cancel || !open {
            self.ban_draft = None;
        }
    }

    /// Barre du bas : tout le contrôle vocal et la télémétrie réseau.
    fn voice_bar(&mut self, ctx: &egui::Context, voice: &VoiceSnapshot) {
        egui::TopBottomPanel::bottom("voice_bar")
            .frame(
                egui::Frame::NONE
                    .fill(theme::BG_SIDE)
                    .inner_margin(egui::Margin::symmetric(12, 9)),
            )
            .show(ctx, |ui| {
                // Périphérique disparu : le dire franchement. Sans ça, on
                // parle dans le vide sans comprendre pourquoi — le cas le
                // plus courant étant un casque sans fil qui sort de veille.
                let (mic_lost, out_lost) = voice.device_trouble;
                if mic_lost || out_lost {
                    let what = match (mic_lost, out_lost) {
                        (true, true) => "Micro et sortie audio perdus",
                        (true, false) => "Micro perdu",
                        _ => "Sortie audio perdue",
                    };
                    // Le cas qui dure : un jeu (Valorant…) a pris le
                    // périphérique en mode exclusif — Windows refusera nos
                    // réouvertures tant qu'il le tient. La reconnexion seule
                    // n'y peut rien : c'est une case à décocher côté Windows,
                    // autant le dire ici plutôt que laisser mariner.
                    ui::banner(
                        ui,
                        Tone::Warn,
                        &format!(
                            "{what} — reconnexion automatique en cours… Si ça arrive \
                             quand un jeu se lance et que ça dure : Paramètres son \
                             Windows → ton micro → Propriétés → Avancé → décoche \
                             « Autoriser les applications à prendre le contrôle \
                             exclusif »."
                        ),
                        false,
                    );
                    ui.add_space(6.0);
                }
                // Le repli est une autre histoire : le son passe, simplement
                // pas par le périphérique réglé. Annoncer une perte serait
                // faux, et promettre une reconnexion, trompeur — c'est un
                // avis, pas une alerte, et il dit quoi faire.
                let (mic_back, out_back) = voice.device_fallback;
                if (mic_back || out_back) && !mic_lost && !out_lost {
                    let what = match (mic_back, out_back) {
                        (true, true) => "Le micro et la sortie audio réglés",
                        (true, false) => "Le micro réglé",
                        _ => "La sortie audio réglée",
                    };
                    ui::banner(
                        ui,
                        Tone::Info,
                        &format!(
                            "{what} n'a pas été trouvé — on utilise le périphérique par                              défaut, et on le reprend dès son retour."
                        ),
                        false,
                    );
                    ui.add_space(6.0);
                }
                ui.horizontal(|ui| {
                    // --- Micro : gros bouton d'état ---
                    let in_voice = self.voice_channel.is_some();
                    let (icon, tint, state, state_color) = if !in_voice {
                        (Icon::MicOff, None, "Hors vocal", TEXT_FAINT)
                    } else if self.muted {
                        (Icon::MicOff, Some(DANGER), "Micro coupé", DANGER)
                    } else if self.transmitting {
                        (Icon::Mic, Some(SPEAK), "En émission", SPEAK)
                    } else {
                        (Icon::Mic, None, "Micro prêt", TEXT_DIM)
                    };
                    let tip = if !in_voice {
                        "Rejoins un salon vocal pour parler"
                    } else if self.muted {
                        "Réactiver le micro"
                    } else {
                        "Couper le micro"
                    };
                    let clicked = ui::icon_button_ex(ui, icon, 38.0, tip, tint).clicked();
                    if clicked && in_voice {
                        self.muted = !self.muted;
                        self.play_sfx(if self.muted { sfx::MUTE } else { sfx::UNMUTE });
                        // Annoncer la bascule tout de suite : les autres voient
                        // l'icône « muet » au lieu de se demander si l'on est
                        // parti. Sans ça, seule la prochaine transition de
                        // parole aurait porté l'information.
                        self.send(ClientMsg::VoiceState {
                            speaking: self.transmitting,
                            muted: self.muted,
                        });
                    }
                    ui.add_space(2.0);
                    ui.vertical(|ui| {
                        ui.add_space(1.0);
                        ui.label(RichText::new(state).color(state_color).size(13.0).strong());
                        // Sous-titre : le salon vocal occupé, ou la touche.
                        let detail = match (self.voice_channel_name(), self.mode) {
                            (None, _) => "aucun salon vocal".to_string(),
                            (Some(name), MicMode::Ptt) => {
                                format!("{name} · touche {}", self.ptt_key.label())
                            }
                            (Some(name), _) => name.to_string(),
                        };
                        ui.label(RichText::new(detail).color(TEXT_FAINT).size(11.0));
                    });

                    // Sortir du vocal sans quitter le serveur.
                    if in_voice
                        && ui::icon_button_ex(
                            ui,
                            Icon::Logout,
                            30.0,
                            "Quitter le vocal",
                            Some(DANGER),
                        )
                            .clicked()
                    {
                        self.leave_voice();
                    }

                    ui.add_space(14.0);

                    // --- Mode d'émission ---
                    let mode_before = self.mode;
                    egui::ComboBox::from_id_salt("mic_mode")
                        .width(150.0)
                        .selected_text(RichText::new(self.mode.label()).color(TEXT))
                        .show_ui(ui, |ui| {
                            for mode in [MicMode::Open, MicMode::Ptt, MicMode::Vad] {
                                ui.selectable_value(&mut self.mode, mode, mode.label());
                            }
                        });
                    if self.mode != mode_before {
                        self.apply_audio_settings();
                    }
                    if self.mode == MicMode::Ptt {
                        egui::ComboBox::from_id_salt("ptt_key")
                            .width(110.0)
                            .selected_text(RichText::new(self.ptt_key.label()).color(TEXT))
                            .show_ui(ui, |ui| {
                                for key in PttKey::ALL {
                                    ui.selectable_value(&mut self.ptt_key, key, key.label());
                                }
                            });
                    }

                    ui.add_space(6.0);
                    if ui::button(ui, Icon::Loupe, "Chercher").clicked() {
                        self.ouvrir_recherche();
                    }
                    if ui::button(ui, Icon::Sliders, "Audio").clicked() {
                        self.show_settings = !self.show_settings;
                        if self.show_settings {
                            let (inputs, outputs) = ki_voice::list_devices();
                            self.input_devices = inputs;
                            self.output_devices = outputs;
                        }
                    }
                    if self.any_admin_power() && ui::button(ui, Icon::Crown, "Admin").clicked() {
                        self.show_admin = !self.show_admin;
                        if self.show_admin {
                            self.admin_name = self.server_info.name.clone();
                            self.admin_icon = IconChange::Keep;
                            self.send(ClientMsg::AdminListUsers);
                        }
                    }

                    // Une sanction vocale s'annonce là où l'on cherchera :
                    // à côté du micro, pas au fond d'une liste. Sans ça on
                    // croit son matériel en panne et l'on part démonter ses
                    // réglages audio — ou son casque.
                    if let Some(moi) = self.my_id.and_then(|id| {
                        self.members.iter().find(|m| m.user_id == id)
                    }) {
                        let sanction = match (moi.force_muted, moi.force_deafened) {
                            (true, true) => Some("micro coupé et sourd (modérateur)"),
                            (true, false) => Some("micro coupé par un modérateur"),
                            (false, true) => Some("rendu sourd par un modérateur"),
                            (false, false) => None,
                        };
                        if let Some(texte) = sanction {
                            ui.add_space(6.0);
                            ui::glyph(ui, Icon::MicOff, 13.0, WARN);
                            ui.label(RichText::new(texte).color(WARN).size(12.0)).on_hover_text(
                                "ce n'est pas ton matériel : quelqu'un l'a décidé côté serveur",
                            );
                        }
                    }

                    // --- Télémétrie, alignée à droite ---
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !voice.engine_up {
                            ui.label(RichText::new("vocal indisponible").color(WARN).size(12.0))
                                .on_hover_text(
                                    "aucun périphérique audio n'a pu être ouvert — \
                                 vérifie les réglages audio",
                                );
                            return;
                        }
                        let s = &voice.stats;
                        let level = (s.mic_peak * 3.0).min(1.0);
                        let color = if self.transmitting {
                            SPEAK
                        } else {
                            theme::BG_ACTIVE
                        };
                        ui::meter(ui, level, Vec2::new(96.0, 7.0), color)
                            .on_hover_text("niveau du micro");

                        ui.add_space(4.0);
                        ui::stat_row(
                            ui,
                            &[
                                (Icon::ArrowUp, compact(s.packets_sent), TEXT_FAINT),
                                (Icon::ArrowDown, compact(s.packets_received), TEXT_FAINT),
                                (
                                    Icon::Close,
                                    compact(s.packets_lost),
                                    if s.packets_lost > 0 { WARN } else { TEXT_FAINT },
                                ),
                            ],
                            11.0,
                        )
                        .on_hover_text("paquets voix envoyés · reçus · perdus");

                        ui.add_space(4.0);
                        match voice.ping {
                            Some(ping) => {
                                let (lit, color) = if ping < 30 {
                                    (4, SPEAK)
                                } else if ping < 80 {
                                    (3, ACCENT)
                                } else if ping < 150 {
                                    (2, WARN)
                                } else {
                                    (1, DANGER)
                                };
                                ui::signal_badge(ui, lit, &format!("{ping} ms"), color)
                                    .on_hover_text("ping (RTT QUIC)");
                            }
                            None => {
                                ui::signal_badge(ui, 0, "— ms", TEXT_FAINT);
                            }
                        }
                    });
                });
            });
    }

    /// Colonne de gauche : marque, salons, membres, et mon compte en pied.
    fn sidebar(&mut self, ctx: &egui::Context, voice: &VoiceSnapshot) {
        // Résolus avant le panneau : la fermeture emprunte déjà `self`.
        let header_name = self.server_label();
        let header_icon = self
            .active_server()
            .cloned()
            .and_then(|server| self.server_icon(ctx, &server));

        egui::SidePanel::left("sidebar")
            .resizable(false)
            .exact_width(SIDEBAR_WIDTH)
            .frame(egui::Frame::NONE.fill(theme::BG_SIDE))
            .show(ctx, |ui| {
                // --- En-tête : marque + serveur ---
                egui::TopBottomPanel::top("brand")
                    .frame(
                        egui::Frame::NONE
                            .fill(theme::BG_SIDE)
                            .inner_margin(egui::Margin::symmetric(12, 11)),
                    )
                    .show_inside(ui, |ui| {
                        // Le logo et le nom du serveur priment : avec
                        // plusieurs serveurs enregistrés, c'est ce qui situe.
                        ui.horizontal(|ui| {
                            let (rect, _) =
                                ui.allocate_exact_size(Vec2::splat(30.0), Sense::hover());
                            match &header_icon {
                                Some(icon) => ui::paint_server_badge(
                                    ui.painter(),
                                    rect,
                                    &header_name,
                                    self.url.trim(),
                                    Some(icon),
                                ),
                                None => icons::logo(ui.painter(), rect, ACCENT, theme::BG_SIDE),
                            }
                            ui.vertical(|ui| {
                                ui.label(
                                    RichText::new(self.server_label())
                                        .color(TEXT)
                                        .size(15.0)
                                        .strong(),
                                );
                                ui.label(
                                    RichText::new(self.url.trim()).color(TEXT_FAINT).size(11.0),
                                );
                            });
                        });
                    });

                // --- Pied : mon compte ---
                egui::TopBottomPanel::bottom("me")
                    .frame(
                        egui::Frame::NONE
                            .fill(theme::BG_SIDE)
                            .inner_margin(egui::Margin::symmetric(10, 9)),
                    )
                    .show_inside(ui, |ui| {
                        ui.horizontal(|ui| {
                            let me = self.username.clone();
                            let mine = self.my_id.and_then(|id| self.avatars.get(&id));
                            let clicked = ui::avatar(
                                ui,
                                &me,
                                30.0,
                                self.transmitting,
                                mine.map(|(_, t)| t),
                                theme::BG_SIDE,
                            )
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked();
                            let named = ui
                                .vertical(|ui| {
                                    ui.label(
                                        RichText::new(&me)
                                            .color(color_for(&me))
                                            .size(13.0)
                                            .strong(),
                                    );
                                    let (dot, text) = if self.muted {
                                        (DANGER, "micro coupé")
                                    } else if self.transmitting {
                                        (SPEAK, "en émission")
                                    } else {
                                        (theme::BORDER, "connecté")
                                    };
                                    ui.horizontal(|ui| {
                                        ui::status_dot(ui, dot, text, 8.0);
                                    });
                                })
                                .response
                                .interact(Sense::click())
                                .on_hover_text("gérer mon compte")
                                .clicked();
                            if clicked || named {
                                self.show_account = true;
                            }

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui::icon_button(ui, Icon::Logout, "Se déconnecter").clicked()
                                    {
                                        self.disconnect(None);
                                    }
                                    if ui::icon_button(ui, Icon::Gear, "Réglages audio").clicked()
                                    {
                                        self.show_settings = !self.show_settings;
                                        if self.show_settings {
                                            let (i, o) = ki_voice::list_devices();
                                            self.input_devices = i;
                                            self.output_devices = o;
                                        }
                                    }
                                },
                            );
                        });
                    });

                // --- Salons et membres ---
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::NONE
                            .fill(theme::BG_SIDE)
                            .inner_margin(egui::Margin::symmetric(8, 4)),
                    )
                    .show_inside(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink(false)
                            .show(ui, |ui| {
                                // `take` et non `clone`, comme pour le roster :
                                // la liste est empruntée le temps du rendu et
                                // remise à la sortie. Voir plus bas.
                                let channels = std::mem::take(&mut self.channels);

                                // --- Salons textuels : on les ouvre ---
                                ui::section_label(ui, "Salons textuels");
                                for ch in channels.iter().filter(|c| c.kind == ChannelKind::Text) {
                                    let selected = self.current == Some(ch.id);
                                    let kind = ChannelKind::Text;
                                    let row = channel_row(ui, &ch.name, selected, kind);
                                    if row.clicked() && !selected {
                                        self.join(ch.id);
                                    }
                                }

                                // --- Salons vocaux : on y entre ---
                                ui.add_space(10.0);
                                ui::section_label(ui, "Salons vocaux");
                                for ch in channels.iter().filter(|c| c.kind == ChannelKind::Voice) {
                                    let here = self.voice_channel == Some(ch.id);
                                    let row = channel_row(ui, &ch.name, here, ChannelKind::Voice);
                                    if row.clicked() {
                                        if here {
                                            self.leave_voice();
                                        } else {
                                            self.join_voice(ch.id);
                                        }
                                    }
                                    // Qui est dans ce salon vocal, comme sur
                                    // Discord : la présence se lit d'un coup
                                    // d'œil, sans y entrer.
                                    self.voice_occupants(ui, ch.id, voice);
                                }
                                self.channels = channels;
                            });
                    });
            });
    }

    /// Occupants d'un salon vocal, listés sous son intitulé.
    fn voice_occupants(&mut self, ui: &mut egui::Ui, channel: ChannelId, voice: &VoiceSnapshot) {
        let members: Vec<Member> = self
            .members
            .iter()
            .filter(|m| m.voice == Some(channel))
            .cloned()
            .collect();
        if members.is_empty() {
            return;
        }

        for m in &members {
            let is_me = Some(m.user_id) == self.my_id;
            let level = if is_me {
                voice.stats.mic_peak
            } else {
                voice.levels.get(&m.user_id).copied().unwrap_or(0.0)
            };
            // Pour soi, l'état d'émission local fait foi. Pour les autres, le
            // drapeau du serveur **ou** le niveau reçu : le second sert de
            // filet si un `VoiceState` se perd, et retombe de lui-même.
            let speaking =
                if is_me { self.transmitting } else { m.speaking || level > SPEAK_LEVEL };
            let volume = self.volume_of(m.user_id);
            let photo = self.avatar_of(m.user_id);
            // Cette liste ne contient que des occupants du vocal : l'icône
            // « muet » s'y montre sans autre condition. Pour soi, l'état
            // local fait foi — l'écho serveur peut être en retard d'un aller.
            let muted = if is_me { self.muted } else { m.muted };
            let response = member_row(
                ui,
                MemberRow { member: m, speaking, muted, is_me, level, volume, photo },
            );
            self.member_menu(response, m, is_me);
        }
        ui.add_space(6.0);
    }

    /// Clic sur soi-même : son compte. Clic droit sur un autre : volume et
    /// modération. Le même comportement dans les deux listes.
    fn member_menu(&mut self, response: egui::Response, m: &Member, is_me: bool) {
        if is_me {
            // Sa propre sanction se dit ici aussi. Être réduit au silence
            // sans savoir pourquoi est la pire version de cette
            // fonctionnalité : on croit son micro cassé et l'on va fouiller
            // les réglages audio.
            let quoi = match (m.force_muted, m.force_deafened) {
                (true, true) => "un modérateur t'a coupé le micro et rendu sourd",
                (true, false) => "un modérateur t'a coupé le micro",
                (false, true) => "un modérateur t'a rendu sourd",
                (false, false) => "gérer mon compte",
            };
            if response.on_hover_text(quoi).clicked() {
                self.show_account = true;
            }
            return;
        }
        // Une sanction se dit en toutes lettres au survol : l'ambre du
        // badge attire l'œil, elle n'explique pas.
        let response = match (m.force_muted, m.force_deafened) {
            (true, true) => response.on_hover_text("micro coupé et rendu sourd par un modérateur"),
            (true, false) => response.on_hover_text("micro coupé par un modérateur"),
            (false, true) => response.on_hover_text("rendu sourd par un modérateur"),
            (false, false) => response,
        };
        response.context_menu(|ui| {
            ui.set_width(228.0);
            ui.label(RichText::new(&m.username).color(self.color_of(m)).strong());
            if let Some(role) = self.top_role_name(m) {
                ui.label(RichText::new(role).color(TEXT_FAINT).size(11.0));
            }
            if !m.online {
                ui.label(RichText::new("hors ligne").color(TEXT_FAINT).size(11.0));
            }
            // Le volume ne concerne que quelqu'un qu'on peut entendre.
            if m.online {
                ui.add_space(4.0);
                let mut pct = self.volume_of(m.user_id) * 100.0;
                if ui
                    .add(
                        egui::Slider::new(&mut pct, 0.0..=200.0)
                            .suffix(" %")
                            .integer()
                            .text("volume"),
                    )
                    .changed()
                {
                    self.set_volume(m.user_id, pct / 100.0);
                }
                if ui::button(ui, Icon::Refresh, "Remettre à 100 %").clicked() {
                    self.set_volume(m.user_id, 1.0);
                }
            }

            // Attribution de rôles : cocher/décocher, borné par son propre
            // rang — on ne donne pas un rôle au-dessus de soi.
            let can_roles = self.can(ki_protocol::perm::MANAGE_ROLES)
                && self.outranks(m.rank)
                && !self.roles.is_empty();
            if can_roles {
                ui.add_space(4.0);
                ui::hairline(ui);
                ui.add_space(4.0);
                ui.menu_button("Rôles", |ui| {
                    ui.set_width(200.0);
                    let mut roles_sorted = self.roles.clone();
                    roles_sorted.sort_by_key(|r| std::cmp::Reverse(r.rank));
                    for role in &roles_sorted {
                        let assignable = self.outranks(role.rank);
                        let mut has = m.roles.contains(&role.id);
                        let label = RichText::new(&role.name)
                            .color(theme::member_color(role.color, &role.name));
                        if ui
                            .add_enabled(assignable, egui::Checkbox::new(&mut has, label))
                            .changed()
                        {
                            let mut new_roles = m.roles.clone();
                            if has {
                                new_roles.push(role.id);
                            } else {
                                new_roles.retain(|id| *id != role.id);
                            }
                            self.send(ClientMsg::AdminSetUserRoles {
                                username: m.username.clone(),
                                roles: new_roles,
                            });
                        }
                    }
                });
            }

            // Modération vocale. Sanctionner et déplacer sont deux pouvoirs
            // distincts : faire taire celui qui hurle n'est pas ranger les
            // gens par salon, et tout le monde n'a pas à pouvoir les deux.
            let peut_sanctionner =
                self.outranks(m.rank) && self.can(ki_protocol::perm::MUTE_MEMBERS);
            let peut_deplacer = self.outranks(m.rank)
                && self.can(ki_protocol::perm::MOVE_MEMBERS)
                && m.online;
            if peut_sanctionner || peut_deplacer {
                ui.add_space(4.0);
                ui::hairline(ui);
                ui.add_space(4.0);
            }
            if peut_sanctionner {
                // L'intitulé dit ce que le clic va faire, pas l'état courant :
                // « Couper le micro » sur quelqu'un déjà coupé le rendrait
                // muet une seconde fois, ce qui ne veut rien dire.
                let (mute_txt, mute_ton) = if m.force_muted {
                    ("Rendre le micro", Tone::Accent)
                } else {
                    ("Couper le micro", Tone::Danger)
                };
                if ui::tinted_button(ui, Some(Icon::MicOff), mute_txt, mute_ton).clicked() {
                    self.send(ClientMsg::AdminVoiceMute {
                        username: m.username.clone(),
                        muted: !m.force_muted,
                    });
                    ui.close();
                }
                let (deaf_txt, deaf_ton) = if m.force_deafened {
                    ("Lui rendre l'écoute", Tone::Accent)
                } else {
                    ("Rendre sourd", Tone::Danger)
                };
                if ui::tinted_button(ui, Some(Icon::HeadphonesOff), deaf_txt, deaf_ton).clicked() {
                    self.send(ClientMsg::AdminVoiceDeafen {
                        username: m.username.clone(),
                        deafened: !m.force_deafened,
                    });
                    ui.close();
                }
            }
            if peut_deplacer {
                let vocaux: Vec<ki_protocol::ChannelInfo> = self
                    .channels
                    .iter()
                    .filter(|c| c.kind == ChannelKind::Voice && Some(c.id) != m.voice)
                    .cloned()
                    .collect();
                ui.menu_button("Déplacer en vocal", |ui| {
                    ui.set_width(200.0);
                    for c in &vocaux {
                        if ui::button(ui, Icon::Volume, &c.name).clicked() {
                            self.send(ClientMsg::AdminVoiceMove {
                                username: m.username.clone(),
                                channel: Some(c.id),
                            });
                            ui.close();
                        }
                    }
                    // Sortir quelqu'un du vocal n'est proposé que s'il y est :
                    // ailleurs, le bouton ne ferait rien de visible.
                    if m.voice.is_some()
                        && ui::tinted_button(ui, Some(Icon::Logout), "Sortir du vocal", Tone::Danger)
                            .clicked()
                    {
                        self.send(ClientMsg::AdminVoiceMove {
                            username: m.username.clone(),
                            channel: None,
                        });
                        ui.close();
                    }
                });
            }

            // Chaque action n'apparaît que si elle aboutirait : la permission
            // ET le rang. Un bouton grisé n'invite qu'à un clic qui échoue,
            // là où la hiérarchie se lit déjà dans les badges de rôle.
            let can_moderate = self.outranks(m.rank);
            let show_kick = can_moderate && self.can(ki_protocol::perm::KICK) && m.online;
            let show_ban = can_moderate && self.can(ki_protocol::perm::BAN);
            if show_kick || show_ban {
                ui.add_space(4.0);
                ui::hairline(ui);
                ui.add_space(4.0);
            }
            // Expulser met dehors ; la personne peut revenir aussitôt.
            if show_kick
                && ui::tinted_button(ui, Some(Icon::Logout), "Expulser", Tone::Danger).clicked()
            {
                self.send(ClientMsg::Kick {
                    user_id: m.user_id,
                    reason: String::new(),
                });
                ui.close();
            }
            // Bannir l'empêche de revenir : motif et durée se saisissent
            // dans une petite fenêtre, plutôt qu'au fond d'un menu.
            if show_ban
                && ui::tinted_button(ui, Some(Icon::Ban), "Bannir…", Tone::Danger).clicked()
            {
                self.ban_draft = Some(BanDraft {
                    username: m.username.clone(),
                    reason: String::new(),
                    duration_secs: 86_400,
                });
                ui.close();
            }
        });
    }

    /// Colonne de droite : tout le monde sur le serveur, comme sur Discord.
    /// On y voit qui est là même sans partager de salon vocal.
    fn roster_panel(&mut self, ctx: &egui::Context, voice: &VoiceSnapshot) {
        egui::SidePanel::right("roster")
            .resizable(false)
            .exact_width(ROSTER_WIDTH)
            .frame(
                egui::Frame::NONE
                    .fill(theme::BG_SIDE)
                    .inner_margin(egui::Margin::symmetric(8, 10)),
            )
            .show(ctx, |ui| {
                // `take` et non `clone` : la liste est empruntée le temps du
                // rendu, puis remise. Copiée, elle coûtait — pseudo, rôles et
                // empreinte d'avatar par membre — une reconstruction complète
                // à chaque image, pour lire des champs qu'on ne modifie pas.
                // C'est la technique déjà employée par le fil de discussion ;
                // la remise a lieu plus bas, avant de sortir de la fermeture.
                let members = std::mem::take(&mut self.members);
                let (online, offline): (Vec<&Member>, Vec<&Member>) =
                    members.iter().partition(|m| m.online);

                ui.horizontal(|ui| {
                    ui::section_label(ui, "En ligne");
                    ui.label(
                        RichText::new(online.len().to_string())
                            .color(theme::BORDER_STRONG)
                            .size(11.0)
                            .strong(),
                    );
                });
                ui.add_space(2.0);

                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    for m in &online {
                        let is_me = Some(m.user_id) == self.my_id;
                        // Le vumètre n'a de sens que si l'on partage le salon
                        // vocal de la personne : sinon on ne l'entend pas.
                        let audible = m.voice.is_some() && m.voice == self.voice_channel;
                        let level = if !audible {
                            0.0
                        } else if is_me {
                            voice.stats.mic_peak
                        } else {
                            voice.levels.get(&m.user_id).copied().unwrap_or(0.0)
                        };
                        let speaking =
                            if is_me { self.transmitting } else { m.speaking || level > SPEAK_LEVEL };
                        let volume = self.volume_of(m.user_id);
                        let photo = self.avatar_of(m.user_id);
                        // « Muet » n'a de sens qu'en vocal : hors salon, un
                        // micro coupé résiduel n'apprend rien à personne.
                        let muted =
                            m.voice.is_some() && if is_me { self.muted } else { m.muted };
                        let response = member_row(
                            ui,
                            MemberRow {
                                member: m,
                                speaking: speaking && audible,
                                muted,
                                is_me,
                                level,
                                volume,
                                photo,
                            },
                        );
                        self.member_menu(response, m, is_me);
                    }

                    // Toute la communauté, pas seulement les présents : les
                    // hors-ligne en dessous, éteints mais bien là — et le
                    // clic droit (rôles, bannir…) marche aussi sur eux.
                    if !offline.is_empty() {
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui::section_label(ui, "Hors ligne");
                            ui.label(
                                RichText::new(offline.len().to_string())
                                    .color(theme::BORDER_STRONG)
                                    .size(11.0)
                                    .strong(),
                            );
                        });
                        ui.add_space(2.0);
                        for m in &offline {
                            let photo = self.avatar_of(m.user_id);
                            let response = member_row(ui, MemberRow::offline(m, photo));
                            self.member_menu(response, m, false);
                        }
                    }
                });
                drop((online, offline));
                self.members = members;
            });
    }

    /// Zone centrale : en-tête du salon, conversation, saisie.
    fn chat_panel(&mut self, ctx: &egui::Context) {
        let channel_name = self
            .current
            .and_then(|id| self.channels.iter().find(|c| c.id == id))
            .map(|c| c.name.clone())
            .unwrap_or_default();

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::BG_BASE))
            .show(ctx, |ui| {
                // --- En-tête ---
                egui::TopBottomPanel::top("chat_head")
                    .frame(
                        egui::Frame::NONE
                            .fill(theme::BG_BASE)
                            .inner_margin(egui::Margin::symmetric(16, 12)),
                    )
                    .show_inside(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui::glyph(ui, Icon::Hash, 17.0, TEXT_FAINT);
                            ui.label(RichText::new(&channel_name).color(TEXT).size(16.5).strong());
                            // Plus de compteur ici : un salon textuel n'a pas
                            // de membres, et la colonne de droite dit déjà qui
                            // est connecté au serveur.
                        });

                        let status = self.upload_status.lock().unwrap().clone();
                        if let Some(status) = status {
                            ui.add_space(8.0);
                            ui::banner(ui, Tone::Info, &status, false);
                        }
                        if let Some(err) = self.error.clone() {
                            ui.add_space(8.0);
                            if ui::banner(ui, Tone::Warn, &err, true) {
                                self.error = None;
                            }
                        }
                    });

                // --- Saisie ---
                egui::TopBottomPanel::bottom("chat_input")
                    .frame(
                        egui::Frame::NONE
                            .fill(theme::BG_BASE)
                            .inner_margin(egui::Margin {
                                left: 16,
                                right: 16,
                                top: 6,
                                bottom: 14,
                            }),
                    )
                    .show_inside(ui, |ui| self.chat_input(ui, &channel_name));

                // --- Conversation ---
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(theme::BG_BASE))
                    .show_inside(ui, |ui| self.chat_log(ui, &channel_name));
            });
    }

    fn chat_input(&mut self, ui: &mut egui::Ui, channel_name: &str) {
        let mut submit = false;
        // Sans le droit d'écrire, la zone de saisie n'est pas grisée : elle
        // disparaît, remplacée par la raison. Un champ où l'on peut taper mais
        // dont rien ne part est plus déroutant qu'une absence de champ.
        if !self.can(ki_protocol::perm::SEND_MESSAGE) {
            egui::Frame::NONE
                .fill(theme::BG_RAISED)
                .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
                .corner_radius(egui::CornerRadius::same(12))
                .inner_margin(egui::Margin::symmetric(10, 10))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("Tu n'as pas le droit d'écrire dans ce serveur.")
                            .color(TEXT_DIM)
                            .size(12.0),
                    );
                });
            return;
        }
        let can_upload = self.can(ki_protocol::perm::UPLOAD_FILE);
        egui::Frame::NONE
            .fill(theme::BG_RAISED)
            .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
            .corner_radius(egui::CornerRadius::same(12))
            .inner_margin(egui::Margin::symmetric(6, 5))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if can_upload
                        && ui::icon_button(ui, Icon::Paperclip, "Envoyer un fichier (25 Mo max)")
                            .clicked()
                    {
                        self.start_upload();
                    }

                    let filled = !self.input.trim().is_empty();
                    let send_width = 34.0 + ui.spacing().item_spacing.x;
                    // Zone **multiligne**. Le protocole a toujours accepté les
                    // sauts de ligne ; c'était l'interface qui les interdisait,
                    // si bien que Maj+Entrée envoyait le message au lieu d'aller
                    // à la ligne — et qu'on ne pouvait pas coller un extrait de
                    // code sans qu'il parte en dix messages.
                    //
                    // La hauteur suit le contenu, dans des bornes : une ligne au
                    // repos, jusqu'à six ensuite. Sans plafond, coller cent
                    // lignes mangerait la conversation.
                    let lignes = self.input.lines().count().clamp(1, 6);
                    let response = ui.add_sized(
                        Vec2::new(ui.available_width() - send_width, 18.0 * lignes as f32 + 8.0),
                        egui::TextEdit::multiline(&mut self.input)
                            .char_limit(ki_protocol::MAX_CHAT_TEXT)
                            .desired_rows(lignes)
                            .frame(false)
                            .margin(egui::Margin::symmetric(4, 4))
                            .hint_text(format!("Message dans #{channel_name}")),
                    );
                    // Entrée envoie, Maj+Entrée va à la ligne. La zone
                    // multiligne consomme Entrée pour son propre compte : on
                    // intercepte donc AVANT elle, et l'on retire le saut de
                    // ligne qu'elle vient d'insérer.
                    if response.has_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift)
                    {
                        // egui a déjà écrit le saut de ligne dans le tampon au
                        // moment où l'on regarde : on l'enlève, sinon chaque
                        // message partirait avec une ligne vide en trop.
                        if self.input.ends_with('\n') {
                            self.input.pop();
                        }
                        submit = true;
                        self.focus_input = true;
                    }
                    if std::mem::take(&mut self.focus_input) {
                        response.request_focus();
                    }

                    let tint = if filled { Some(ACCENT) } else { None };
                    if ui::icon_button_ex(ui, Icon::Send, 32.0, "Envoyer (Entrée) — Maj+Entrée pour aller à la ligne", tint).clicked()
                    {
                        submit = true;
                        self.focus_input = true;
                    }
                });
            });

        if submit {
            let text = self.input.trim().to_string();
            if !text.is_empty() {
                self.send(ClientMsg::Chat { text });
                self.input.clear();
            }
        }
    }

    fn chat_log(&mut self, ui: &mut egui::Ui, channel_name: &str) {
        let mut want_older = false;
        let mut revenir = false;
        // Compté, et pas déduit de `messages.len()` : le jour où le fil sera
        // virtualisé (P3.3), les deux cesseront de coïncider — et c'est
        // précisément l'écart qui prouvera que ça marche.
        let mut rendus = 0usize;
        self.perf.debut_fil();
        let out = egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink(false)
            .show(ui, |ui| {
                if self.messages.is_empty() {
                    empty_state(ui, channel_name);
                    return;
                }

                // En-tête du fil : c'est là que se remonte le passé. Un
                // bouton explicite en plus du chargement au défilement — le
                // défilement seul laisse croire qu'on est au début alors
                // qu'il reste des mois de conversation.
                if self.history_pending {
                    ui.vertical_centered(|ui| {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("Chargement des messages plus anciens…")
                                .color(TEXT_FAINT)
                                .size(11.5),
                        );
                        ui.add_space(6.0);
                    });
                } else if self.history_more {
                    ui.vertical_centered(|ui| {
                        ui.add_space(6.0);
                        if ui
                            .button(
                                RichText::new("Charger les messages plus anciens")
                                    .color(TEXT_DIM)
                                    .size(11.5),
                            )
                            .clicked()
                        {
                            want_older = true;
                        }
                        ui.add_space(6.0);
                    });
                } else {
                    day_separator(ui, &format!("Début de #{channel_name}"));
                }

                // Une hauteur dépend du retour à la ligne, donc de la largeur :
                // la moindre variation périme toutes les mesures.
                let largeur = ui.available_width();
                if (self.msg_heights_width - largeur).abs() > 0.5 {
                    self.msg_heights.clear();
                    self.msg_heights_width = largeur;
                }
                // La zone réellement visible. Une marge d'un écran de part et
                // d'autre : on préfère peindre un peu trop que de laisser un
                // blanc apparaître au défilement rapide.
                let vue = ui.clip_rect();
                let marge = vue.height().max(200.0);

                // Les pseudos connus, empruntés le temps du rendu comme les
                // messages : c'est ce qui permet de reconnaître une mention
                // sans allouer une chaîne par membre et par image.
                let membres_source = std::mem::take(&mut self.members);
                let membres: Vec<&str> =
                    membres_source.iter().map(|m| m.username.as_str()).collect();
                let moi = self
                    .my_id
                    .and_then(|id| membres_source.iter().find(|m| m.user_id == id))
                    .map(|m| m.username.clone());

                let mut last_day = i32::MIN;
                let mut previous: Option<(UserId, u64)> = None;
                let messages = std::mem::take(&mut self.messages);
                for msg in &messages {
                    let day = day_key(msg.ts);
                    let jour_change = day != last_day;
                    if jour_change {
                        last_day = day;
                    }
                    let grouped = !jour_change
                        && previous.is_some_and(|(user, ts)| {
                            user == msg.user_id && msg.ts.saturating_sub(ts) < GROUP_WINDOW_MS
                        });
                    previous = Some((msg.user_id, msg.ts));

                    // Hors écran, et la hauteur est connue de l'image
                    // précédente : on réserve la place sans rien construire.
                    //
                    // C'est tout l'objet de la manœuvre. Le fil parcourait
                    // TOUS les messages en mémoire à chaque image, visibles ou
                    // non — jusqu'à cinq cents blocs de widgets construits,
                    // mis en page et poussés vers le GPU vingt fois par
                    // seconde, pour en montrer une vingtaine. La place étant
                    // réservée à l'identique, la barre de défilement et le
                    // rattrapage de pagination ne voient aucune différence.
                    let cle = (msg.user_id, msg.ts);
                    let haut = ui.cursor().top();
                    if let Some(hauteur) = self.msg_heights.get(&cle).copied() {
                        let bas = haut + hauteur;
                        if bas < vue.top() - marge || haut > vue.bottom() + marge {
                            ui.allocate_space(Vec2::new(largeur, hauteur));
                            continue;
                        }
                    }

                    rendus += 1;
                    let photo = self.avatars.get(&msg.user_id).map(|(_, t)| t.clone());
                    // L'auteur peut avoir quitté le serveur : on retombe
                    // alors sur son pseudo, plutôt que de perdre la couleur.
                    let color = self
                        .author_colors
                        .get(&msg.user_id)
                        .copied()
                        .unwrap_or_else(|| color_for(&msg.username));

                    // `scope` et non une mesure du curseur : c'est ce qui rend
                    // les deux chemins interchangeables. Le curseur, lui,
                    // avance de la hauteur PLUS l'espacement entre widgets ;
                    // on aurait donc réservé un espacement de trop par message
                    // sauté, et le fil se serait allongé à mesure qu'on le
                    // remonte. Un `scope` et un `allocate_space` sont deux
                    // widgets qui occupent exactement la hauteur demandée.
                    let previews = &mut self.previews;
                    let bloc = ui.scope(|ui| {
                        if jour_change {
                            day_separator(ui, &day_label(msg.ts));
                        }
                        message_block(
                            ui,
                            MessageRow {
                                msg,
                                with_header: !grouped,
                                photo: photo.as_ref(),
                                color,
                                membres: &membres,
                                moi: moi.as_deref(),
                            },
                            previews,
                        );
                    });
                    // Mesurée séparateur compris : c'est le bloc entier qu'on
                    // sautera la prochaine fois.
                    self.msg_heights.insert(cle, bloc.response.rect.height());
                }
                self.messages = messages;
                drop(membres);
                self.members = membres_source;
                // Le cache ne garde que ce qui est encore affiché : sans ça,
                // remonter un fil de plusieurs mois y laisserait une entrée
                // par message lu, pour toujours.
                if self.msg_heights.len() > self.messages.len() * 2 {
                    let vivants: std::collections::HashSet<(UserId, u64)> =
                        self.messages.iter().map(|m| (m.user_id, m.ts)).collect();
                    self.msg_heights.retain(|k, _| vivants.contains(k));
                }

                // On a sauté au milieu du passé : ce qui a suivi n'est pas
                // affiché, et rien ne le dirait. Sans ce pied de fil, on croit
                // le salon mort depuis ce jour-là.
                if self.retour_present == self.current {
                    ui.add_space(6.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("tu regardes un message retrouvé")
                                .color(TEXT_FAINT)
                                .size(11.5),
                        );
                        if ui
                            .button(
                                RichText::new("Revenir au présent").color(TEXT_DIM).size(11.5),
                            )
                            .clicked()
                        {
                            revenir = true;
                        }
                    });
                }
                ui.add_space(10.0);
            });
        self.perf.fin_fil(rendus, self.messages.len());
        // Une page vient de s'ajouter au-dessus : on décale la vue d'autant
        // que le contenu a grandi. Sans ce rattrapage, la vue reste au sommet
        // du nouveau bloc — l'écran saute, on perd sa ligne, et la condition
        // « on est tout en haut » reste vraie, ce qui enchaînait les
        // chargements jusqu'à épuiser le salon.
        self.chat_height = out.content_size.y;
        let mut offset_y = out.state.offset.y;
        if let Some(before) = self.history_anchor.take() {
            let grown = out.content_size.y - before;
            if grown > 0.0 {
                let mut state = out.state;
                // Borné dès cette image, et pas seulement à la suivante par
                // egui : un fil qui tenait entièrement dans la fenêtre est
                // collé en bas, et ajouter `grown` par-dessus l'envoyait
                // au-delà de la fin — un éclair de vide, puis un retour en bas,
                // soit l'inverse de ce qu'on cherche.
                let max = (out.content_size.y - out.inner_rect.height()).max(0.0);
                state.offset.y = (state.offset.y + grown).clamp(0.0, max);
                offset_y = state.offset.y;
                state.store(ui.ctx(), out.id);
                ui.ctx().request_repaint();
            }
        }

        // Arrivé tout en haut, on charge sans attendre le clic : c'est le
        // geste naturel pour remonter une conversation. Mais seulement si l'on
        // défile **réellement** : rester en haut ne doit pas suffire, sinon la
        // demande repart à chaque image. Le bouton explicite couvre le cas où
        // l'on est déjà en haut sans rien toucher.
        let scrolling = ui.input(|i| {
            i.raw_scroll_delta.y.abs() > 0.0 || i.smooth_scroll_delta.y.abs() > 0.0
        });
        // `offset_y` et non `out.state.offset.y` : sur l'image du recalage,
        // ce dernier vaut encore la valeur d'avant correction, c'est-à-dire
        // ~0 — et l'on redemanderait aussitôt une page de plus.
        if offset_y <= 24.0 && !self.messages.is_empty() && scrolling {
            want_older = true;
        }
        if want_older {
            self.load_older_history();
        }
        if revenir {
            if let Some(channel) = self.current {
                self.join(channel);
            }
        }
    }

    // -----------------------------------------------------------------
    // Fenêtres
    // -----------------------------------------------------------------

    /// Ouvre la recherche, curseur dans le champ.
    fn ouvrir_recherche(&mut self) {
        self.show_search = !self.show_search;
        if self.show_search {
            self.search_focus = true;
        }
    }

    /// Envoie la requête en cours, si elle a de quoi chercher.
    fn lancer_recherche(&mut self) {
        let query = self.search_query.trim().to_string();
        if query.is_empty() {
            self.search_hits.clear();
            self.search_envoyee = None;
            return;
        }
        let channel = self.search_ici.then_some(self.current).flatten();
        self.search_envoyee = Some(query.clone());
        self.send(ClientMsg::Search {
            query,
            channel,
            limit: ki_protocol::MAX_SEARCH_HITS as u32,
        });
    }

    /// Ouvre un salon **au niveau d'un message précis** plutôt qu'à sa fin.
    ///
    /// Ce n'est pas `join` : celui-ci demande les derniers messages, alors
    /// qu'ici on veut la page qui **se termine** par le message trouvé. Le
    /// résultat est donc en bas de l'écran, sans qu'il faille recalculer un
    /// défilement — et sans afficher ce qui a suivi, d'où le retour au
    /// présent proposé en bas du fil.
    fn sauter_a(&mut self, channel: ChannelId, ts: u64) {
        self.current = Some(channel);
        self.messages.clear();
        self.msg_heights.clear();
        self.history_more = true;
        self.history_pending = true;
        self.history_anchor = None;
        self.retour_present = Some(channel);
        self.send(ClientMsg::Join { channel });
        // `before_ts` est exclusif : +1 pour que le message cherché soit dans
        // la page, et en dernier.
        self.send(ClientMsg::HistoryBefore { before_ts: ts + 1, limit: 100, channel });
    }

    /// Fenêtre de recherche : la requête, la portée, et les résultats.
    fn search_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let mut lancer = false;
        let mut saut: Option<(ChannelId, u64)> = None;
        let roomy = (ctx.screen_rect().height() - 160.0).clamp(280.0, 720.0);
        egui::Window::new("Rechercher")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(440.0)
            .default_height(roomy)
            .min_width(320.0)
            .show(ctx, |ui| {
                let champ = ui.add(ui::text_field(&mut self.search_query, "chercher…", false));
                if std::mem::take(&mut self.search_focus) {
                    champ.request_focus();
                }
                // À la frappe, pas de recherche : chaque requête fait relire
                // des journaux entiers au serveur. On cherche quand on a fini
                // de taper, et on le dit.
                if champ.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    lancer = true;
                }
                ui.horizontal(|ui| {
                    let ici = ui.selectable_label(self.search_ici, "Ce salon");
                    let partout = ui.selectable_label(!self.search_ici, "Partout");
                    // Changer de portée relance : sinon la liste affichée ne
                    // correspond plus au bouton allumé, et l'on croit que la
                    // recherche n'a rien trouvé ailleurs.
                    if ici.clicked() && !self.search_ici {
                        self.search_ici = true;
                        lancer = true;
                    }
                    if partout.clicked() && self.search_ici {
                        self.search_ici = false;
                        lancer = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui::button(ui, Icon::Loupe, "Chercher").clicked() {
                            lancer = true;
                        }
                    });
                });
                ui::hint(ui, "Entrée pour chercher — la casse et les accents majuscules sont ignorés");

                ui.add_space(8.0);
                ui::hairline(ui);
                ui.add_space(8.0);

                if self.search_envoyee.is_some() && self.search_hits.is_empty() {
                    ui.label(RichText::new("Recherche en cours…").color(TEXT_FAINT).size(12.0));
                    return;
                }
                if self.search_hits.is_empty() {
                    let quoi = if self.search_query.trim().is_empty() {
                        "Tape ce que tu cherches, puis Entrée."
                    } else {
                        "Aucun message ne contient ça."
                    };
                    ui.label(RichText::new(quoi).color(TEXT_FAINT).size(12.0));
                    return;
                }
                if self.search_more {
                    ui::hint(ui, "il y en avait davantage : voici les plus récents");
                }

                // Les plus récents en premier : c'est l'ordre dans lequel on
                // cherche, alors que le serveur rend l'ordre du fil.
                let hits: Vec<ki_protocol::SearchHit> =
                    self.search_hits.iter().rev().cloned().collect();
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    for (rang, hit) in hits.iter().enumerate() {
                        let nom = self
                            .channels
                            .iter()
                            .find(|c| c.id == hit.channel)
                            .map(|c| c.name.clone())
                            .unwrap_or_else(|| format!("salon {}", hit.channel));
                        // Le rang, et non salon+horodatage : deux messages
                        // peuvent partager les deux à la milliseconde près,
                        // et deux widgets de même identité, egui n'en peint
                        // qu'un.
                        let bloc = ui.push_id(("hit", rang), |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!("#{nom}")).color(ACCENT).size(11.0),
                                );
                                ui.label(
                                    RichText::new(&hit.record.username).color(TEXT_DIM).size(11.0),
                                );
                                ui.label(
                                    RichText::new(day_label(hit.record.ts))
                                        .color(TEXT_FAINT)
                                        .size(11.0),
                                );
                            });
                            // Une seule ligne : un résultat sert à reconnaître
                            // le message, pas à le relire. Le fil s'en charge.
                            let apercu: String = hit
                                .record
                                .text
                                .lines()
                                .next()
                                .unwrap_or_default()
                                .chars()
                                .take(160)
                                .collect();
                            ui.label(RichText::new(apercu).color(TEXT).size(13.0));
                        });
                        if bloc
                            .response
                            .interact(egui::Sense::click())
                            .on_hover_text("aller à ce message")
                            .clicked()
                        {
                            saut = Some((hit.channel, hit.record.ts));
                        }
                        ui.add_space(4.0);
                        ui::hairline(ui);
                        ui.add_space(4.0);
                    }
                });
            });
        if lancer {
            self.lancer_recherche();
        }
        if let Some((channel, ts)) = saut {
            self.sauter_a(channel, ts);
        }
        if !open {
            self.show_search = false;
        }
    }

    /// Fenêtre de réglages audio : périphériques, micro, sortie, qualité.
    fn settings_window(&mut self, ctx: &egui::Context, voice: &VoiceSnapshot) {
        let mut restart = false;
        let mut apply = false;
        let mut open = true;
        let engine_up = voice.engine_up;
        let stats = &voice.stats;
        let mic_peak = stats.mic_peak;
        self.tick_calibration(mic_peak);

        // Hauteur d'ouverture : large sur un grand écran, sans jamais
        // déborder d'un petit. L'utilisateur peut ensuite redimensionner,
        // et egui retient la taille d'une session à l'autre.
        let roomy = (ctx.screen_rect().height() - 120.0).clamp(320.0, 900.0);
        egui::Window::new("Réglages audio")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(420.0)
            .default_height(roomy)
            .min_width(340.0)
            .min_height(260.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // --- Périphériques ---
                        ui::group_title(ui, Icon::Headphones, "Périphériques");
                        // Le remède logiciel au « il faut débrancher/rebrancher
                        // le casque » : certains pilotes (Razer…) laissent un
                        // flux zombie après le passage d'un jeu.
                        if ui::button(ui, Icon::Refresh, "Réinitialiser l'audio").clicked() {
                            {
                                if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
                                    engine.reset_audio_devices();
                                    self.info = Some(
                                        "micro et sortie rouverts — comme un \
                                         débranchement/rebranchement du casque"
                                            .into(),
                                    );
                                }
                            }
                        }
                        ui::hint(
                            ui,
                            "si le son part en vrille quand un jeu se lance ou se ferme, \
                             ce bouton rouvre tout sans toucher au câble",
                        );
                        ui.add_space(6.0);
                        let device_combo = |ui: &mut egui::Ui,
                                            id: &str,
                                            label: &str,
                                            devices: &[String],
                                            sel: &mut Option<String>|
                         -> bool {
                            let mut changed = false;
                            ui::field_label(ui, label);
                            egui::ComboBox::from_id_salt(id)
                                .width(ui.available_width() - 8.0)
                                .selected_text(
                                    RichText::new(
                                        sel.clone().unwrap_or_else(|| "(défaut système)".into()),
                                    )
                                    .color(TEXT),
                                )
                                .show_ui(ui, |ui| {
                                    if ui
                                        .selectable_label(sel.is_none(), "(défaut système)")
                                        .clicked()
                                    {
                                        changed |= sel.is_some();
                                        *sel = None;
                                    }
                                    for d in devices {
                                        let active = sel.as_deref() == Some(d.as_str());
                                        if ui.selectable_label(active, d).clicked() && !active {
                                            *sel = Some(d.clone());
                                            changed = true;
                                        }
                                    }
                                });
                            ui.add_space(8.0);
                            changed
                        };
                        restart |= device_combo(
                            ui,
                            "input_dev",
                            "Micro",
                            &self.input_devices.clone(),
                            &mut self.pref_input,
                        );
                        restart |= device_combo(
                            ui,
                            "output_dev",
                            "Sortie",
                            &self.output_devices.clone(),
                            &mut self.pref_output,
                        );
                        if ui::button(ui, Icon::Refresh, "Actualiser la liste").clicked() {
                            let (inputs, outputs) = ki_voice::list_devices();
                            self.input_devices = inputs;
                            self.output_devices = outputs;
                        }
                        if cfg!(windows) {
                            ui.add_space(8.0);
                            if ui
                                .checkbox(
                                    &mut self.native_audio,
                                    "Moteur audio natif (recommandé)",
                                )
                                .on_hover_text(
                                    "parle à Windows comme Discord : suit le périphérique \
                                     de communication, survit aux jeux qui changent le \
                                     format audio. Décoche si le son se comporte moins \
                                     bien qu'avant.",
                                )
                                .changed()
                            {
                                restart = true;
                            }
                            if self.native_audio
                                && ui
                                    .checkbox(
                                        &mut self.raw_mic,
                                        "Micro brut (ignorer les effets du casque)",
                                    )
                                    .on_hover_text(
                                        "court-circuite les traitements tiers (Sonar, \
                                         Nahimic, Synapse…) sur le micro. À essayer si le \
                                         micro bugue quand un jeu se lance.",
                                    )
                                    .changed()
                            {
                                restart = true;
                            }
                            if self.native_audio
                                && ui
                                    .checkbox(
                                        &mut self.comms_mic,
                                        "Partager le micro avec la voix du jeu",
                                    )
                                    .on_hover_text(
                                        "ouvre le micro dans la voie « communications » de \
                                         Windows, celle des voix intégrées des jeux — \
                                         nécessaire quand elles affament le micro (le \
                                         moteur le fait tout seul au besoin, cette case le \
                                         rend permanent). Revers : Windows peut baisser le \
                                         volume des autres sons pendant le vocal → Panneau \
                                         son → Communication → « Ne rien faire ».",
                                    )
                                    .changed()
                            {
                                restart = true;
                            }
                        }

                        // --- Journal audio ---
                        // Ce que l'audio a vécu (ouvertures, pertes, replis,
                        // réouvertures) : les bugs « au lancement d'un jeu »
                        // varient d'un casque à l'autre, et ce journal
                        // remplace la divination — on se le fait copier-coller.
                        ui.add_space(8.0);
                        let label = if self.show_audio_journal {
                            "Masquer le journal audio"
                        } else {
                            "Journal audio"
                        };
                        if ui::button(ui, Icon::Info, label).clicked() {
                            self.show_audio_journal = !self.show_audio_journal;
                        }
                        if self.show_audio_journal {
                            let events = ki_voice::journal_snapshot();
                            if events.is_empty() {
                                ui::hint(ui, "rien à signaler pour l'instant");
                            } else {
                                if ui::button(ui, Icon::Copy, "Copier le journal").clicked() {
                                    let text: String = events
                                        .iter()
                                        .map(|(ts, m)| {
                                            format!("{} {m}\n", format_time_secs(*ts))
                                        })
                                        .collect();
                                    ctx.copy_text(text);
                                    self.info = Some("journal audio copié".into());
                                }
                                ui.add_space(4.0);
                                // Les plus récents d'abord : c'est eux qu'on
                                // vient voir quand ça vient de buguer.
                                for (ts, msg) in events.iter().rev().take(12) {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(
                                            RichText::new(format_time_secs(*ts))
                                                .color(TEXT_DIM)
                                                .monospace()
                                                .size(11.5),
                                        );
                                        ui.label(
                                            RichText::new(msg.as_str()).color(TEXT).size(11.5),
                                        );
                                    });
                                }
                                if events.len() > 12 {
                                    ui::hint(
                                        ui,
                                        "le bouton copie l'intégralité du journal",
                                    );
                                }
                            }
                        }

                        // --- Diagnostic partagé ---
                        // Le journal ci-dessus se copie à la main ; ici, il
                        // part tout seul vers le serveur — pour que l'admin
                        // (et l'assistant qui débogue avec lui) lise ce qui
                        // s'est passé sans rien demander à personne. Opt-in,
                        // et technique seulement : jamais les messages, jamais
                        // l'audio.
                        ui.add_space(8.0);
                        ui.checkbox(
                            &mut self.diag_share,
                            "Partager mes diagnostics avec l'admin du serveur",
                        )
                        .on_hover_text(
                            "envoie toutes les 10 minutes le journal technique \
                             (périphériques, pertes, réouvertures), la version et le \
                             rapport du docteur au serveur du groupe. Jamais tes \
                             messages, jamais ta voix. Décochable à tout moment.",
                        );
                        if self.diag_share
                            && ui::button(ui, Icon::Send, "Envoyer le diagnostic maintenant")
                                .clicked()
                        {
                            self.flush_diag(true);
                            self.info = Some("diagnostic envoyé au serveur".into());
                        }

                        // --- Docteur audio ---
                        // Le journal ci-dessus raconte ce qui s'est passé ;
                        // celui-ci dit ce qu'il faut FAIRE. Windows n'offre
                        // aucune API de « priorité micro » : quand un autre
                        // logiciel tient la voie de capture, on ne peut pas la
                        // lui reprendre — seulement le nommer, et dire le
                        // réglage qui rend la main.
                        ui.add_space(8.0);
                        let label = if self.show_docteur {
                            "Masquer le docteur audio"
                        } else {
                            "Docteur audio — pourquoi mon micro bugue ?"
                        };
                        if ui::button(ui, Icon::Info, label).clicked() {
                            self.show_docteur = !self.show_docteur;
                            if self.show_docteur {
                                // Établi au clic : l'énumération des processus
                                // et la lecture du registre n'ont rien à faire
                                // dans une boucle de rendu.
                                self.docteur =
                                    self.link.engine.lock().unwrap().as_ref().map(|e| e.docteur());
                            }
                        }
                        if self.show_docteur {
                            match &self.docteur {
                                None => ui::hint(
                                    ui,
                                    "le moteur audio n'est pas démarré — rejoins un \
                                     salon vocal, puis reviens",
                                ),
                                Some(d) => {
                                    if ui::button(ui, Icon::Copy, "Copier le diagnostic")
                                        .clicked()
                                    {
                                        ctx.copy_text(d.rapport());
                                        self.info = Some("diagnostic copié".into());
                                    }
                                    ui.add_space(4.0);
                                    // Les conseils, numérotés, dans l'ordre où
                                    // ils valent la peine d'être essayés.
                                    for (i, conseil) in d.conseils().iter().enumerate() {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label(
                                                RichText::new(format!("{}.", i + 1))
                                                    .color(ACCENT)
                                                    .strong()
                                                    .size(11.5),
                                            );
                                            ui.label(
                                                RichText::new(conseil.as_str())
                                                    .color(TEXT)
                                                    .size(11.5),
                                            );
                                        });
                                        ui.add_space(3.0);
                                    }
                                    ui::hint(
                                        ui,
                                        "ki-chat ne touche à aucun réglage système : il \
                                         te dit quoi regarder, tu décides.",
                                    );
                                }
                            }
                        }

                        // --- Relevé de performance ---
                        // Même usage que le journal au-dessus : on se le fait
                        // copier-coller. « Ça rame quand je joue » ne se
                        // reproduit pas sur la machine de développement, et
                        // une moyenne noierait justement l'image lente qu'on
                        // vient chercher — d'où des quantiles.
                        ui.add_space(8.0);
                        let label = if self.show_perf {
                            "Masquer le relevé de performance"
                        } else {
                            "Relevé de performance"
                        };
                        if ui::button(ui, Icon::Info, label).clicked() {
                            self.show_perf = !self.show_perf;
                        }
                        if self.show_perf {
                            let lignes = self.perf.lignes();
                            if ui::button(ui, Icon::Copy, "Copier le relevé").clicked() {
                                let mut texte = format!(
                                    "ki-chat {} — relevé de performance\n",
                                    env!("CARGO_PKG_VERSION")
                                );
                                for (quoi, valeur) in &lignes {
                                    texte.push_str(&format!("{quoi} : {valeur}\n"));
                                }
                                texte.push_str(&format!(
                                    "Trames incomplètes (audio) : {}\n",
                                    voice.stats.underruns
                                ));
                                // Le diagnostic voyage avec : celui qui reçoit
                                // ce relevé n'a alors plus rien à demander.
                                if let Some(d) = &self.docteur {
                                    texte.push('\n');
                                    texte.push_str(&d.rapport());
                                }
                                ctx.copy_text(texte);
                                self.info = Some("relevé copié".into());
                            }
                            ui.add_space(4.0);
                            for (quoi, valeur) in &lignes {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(
                                        RichText::new(quoi.as_str()).color(TEXT_DIM).size(11.5),
                                    );
                                    ui.label(
                                        RichText::new(valeur.as_str())
                                            .color(TEXT)
                                            .monospace()
                                            .size(11.5),
                                    );
                                });
                            }
                            // Les trous audio vivent dans le moteur, pas dans
                            // l'interface, mais c'est le même relevé pour qui
                            // le lit. Zéro est la seule bonne valeur.
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new("Trames incomplètes (audio)")
                                        .color(TEXT_DIM)
                                        .size(11.5),
                                );
                                ui.label(
                                    RichText::new(voice.stats.underruns.to_string())
                                        .color(if voice.stats.underruns == 0 {
                                            SPEAK
                                        } else {
                                            DANGER
                                        })
                                        .monospace()
                                        .size(11.5),
                                );
                            });
                        }

                        // --- Micro ---
                        ui.add_space(12.0);
                        ui::hairline(ui);
                        ui.add_space(10.0);
                        ui::group_title(ui, Icon::Mic, "Micro");

                        // Vumètre en direct, avec repère du seuil d'activation.
                        let above = self.mode == MicMode::Vad && mic_peak >= self.vad_threshold;
                        let meter_color = if !engine_up {
                            theme::BG_ACTIVE
                        } else if above || (self.mode != MicMode::Vad && mic_peak > 0.01) {
                            SPEAK
                        } else {
                            TEXT_DIM
                        };
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("niveau").color(TEXT_DIM).size(12.5));
                            let threshold = (self.mode == MicMode::Vad)
                                .then(|| (self.vad_threshold * 3.0).min(1.0));
                            ui::meter_with_threshold(
                                ui,
                                (mic_peak * 3.0).min(1.0),
                                threshold,
                                Vec2::new(ui.available_width().min(230.0), 9.0),
                                meter_color,
                            );
                            if !engine_up {
                                ui.label(RichText::new("vocal inactif").color(WARN).size(11.0));
                            }
                        });
                        ui.add_space(8.0);

                        // Calibration automatique des seuils sur le niveau ambiant.
                        match self.calibrating {
                            None => {
                                if engine_up
                                && ui::button(ui, Icon::Target, "Calibrer les seuils (5 s)")
                                    .on_hover_text(
                                        "reste silencieux — laisse le bruit ambiant ou l'autre \
                                         voix de la pièce parler : je règle la porte de bruit \
                                         juste au-dessus de ce niveau",
                                    )
                                    .clicked()
                            {
                                self.start_calibration();
                            }
                            }
                            Some((start, peak)) => {
                                let progress = (start.elapsed().as_secs_f32() / 5.0).min(1.0);
                                ui.horizontal(|ui| {
                                    ui::meter(ui, progress, Vec2::new(180.0, 9.0), ACCENT);
                                    ui.label(
                                        RichText::new(format!(
                                            "chut… ambiance {:.1} %",
                                            peak * 100.0
                                        ))
                                        .color(TEXT_DIM)
                                        .size(12.0),
                                    );
                                    if ui::icon_button(ui, Icon::Close, "Annuler").clicked() {
                                        self.calibrating = None;
                                        self.apply_audio_settings();
                                    }
                                });
                            }
                        }
                        ui.add_space(8.0);

                        if ui
                            .checkbox(&mut self.agc, "Gain automatique (AGC)")
                            .on_hover_text(
                                "normalise ta voix tout seul : fini les réglages manuels",
                            )
                            .changed()
                        {
                            apply = true;
                        }
                        if self.agc {
                            let mut target_pct = self.agc_target * 100.0;
                            if ui
                                .add(
                                    egui::Slider::new(&mut target_pct, 15.0..=50.0)
                                        .text("niveau cible")
                                        .suffix(" %")
                                        .integer(),
                                )
                                .changed()
                            {
                                self.agc_target = target_pct / 100.0;
                                apply = true;
                            }
                        }
                        let mut gain_pct = self.input_gain * 100.0;
                        if ui
                            .add(
                                egui::Slider::new(&mut gain_pct, 0.0..=200.0)
                                    .text(if self.agc {
                                        "pré-ampli"
                                    } else {
                                        "gain d'entrée"
                                    })
                                    .suffix(" %")
                                    .integer(),
                            )
                            .changed()
                        {
                            self.input_gain = gain_pct / 100.0;
                            apply = true;
                        }

                        if self.mode == MicMode::Vad {
                            let mut thr_pct = self.vad_threshold * 100.0;
                            if ui
                                .add(
                                    egui::Slider::new(&mut thr_pct, 0.5..=25.0)
                                        .text("seuil d'activation")
                                        .suffix(" %"),
                                )
                                .changed()
                            {
                                self.vad_threshold = thr_pct / 100.0;
                                apply = true;
                            }
                            let mut hang = self.vad_hangover_ms as f32;
                            if ui
                                .add(
                                    egui::Slider::new(&mut hang, 100.0..=1000.0)
                                        .text("maintien après la voix")
                                        .suffix(" ms")
                                        .step_by(50.0),
                                )
                                .changed()
                            {
                                self.vad_hangover_ms = hang as u32;
                                apply = true;
                            }
                            ui::hint(
                            ui,
                            "parle : la jauge doit dépasser le repère orange quand ta voix passe",
                        );
                        }
                        if self.mode == MicMode::Ptt {
                            let mut rel = self.ptt_release_ms as f32;
                            if ui
                                .add(
                                    egui::Slider::new(&mut rel, 0.0..=500.0)
                                        .text("relâchement du push-to-talk")
                                        .suffix(" ms")
                                        .step_by(25.0),
                                )
                                .changed()
                            {
                                self.ptt_release_ms = rel as u32;
                            }
                        }

                        // --- Suppression de bruit ---
                        ui.add_space(12.0);
                        ui::hairline(ui);
                        ui.add_space(10.0);
                        ui::group_title(ui, Icon::Volume, "Suppression de bruit");
                        let noise_label = |m: u8| match m {
                            ki_voice::NOISE_OFF => "Désactivée",
                            ki_voice::NOISE_DEEP => "DeepFilterNet3 (studio, +30 ms)",
                            _ => "RNNoise (léger)",
                        };
                        egui::ComboBox::from_id_salt("noise_mode")
                            .width(270.0)
                            .selected_text(RichText::new(noise_label(self.noise_mode)).color(TEXT))
                            .show_ui(ui, |ui| {
                                for mode in [
                                    ki_voice::NOISE_OFF,
                                    ki_voice::NOISE_RNNOISE,
                                    ki_voice::NOISE_DEEP,
                                ] {
                                    if ui
                                        .selectable_label(
                                            self.noise_mode == mode,
                                            noise_label(mode),
                                        )
                                        .clicked()
                                    {
                                        self.noise_mode = mode;
                                        apply = true;
                                    }
                                }
                            });
                        if self.noise_mode == ki_voice::NOISE_DEEP {
                            ui::hint(
                                ui,
                                "réseau de neurones DeepFilterNet3 : supprime clavier, ventilo, \
                             fond sonore — qualité Krisp/Discord, 100 % local",
                            );
                        }
                        ui.add_space(6.0);
                        let mut gate_pct = self.gate_threshold * 100.0;
                        if ui
                            .add(
                                egui::Slider::new(&mut gate_pct, 0.0..=10.0)
                                    .text("porte de bruit")
                                    .suffix(" %"),
                            )
                            .on_hover_text("0 % = désactivée ; coupe tout résidu sous ce niveau")
                            .changed()
                        {
                            self.gate_threshold = gate_pct / 100.0;
                            apply = true;
                        }
                        if ui
                            .checkbox(&mut self.loopback, "S'écouter — aller-retour codec complet")
                            .on_hover_text(
                                "tu entends EXACTEMENT ce que les autres entendent : \
                             filtres + encodage/décodage Opus au débit courant",
                            )
                            .changed()
                        {
                            apply = true;
                        }

                        // --- Sortie ---
                        ui.add_space(12.0);
                        ui::hairline(ui);
                        ui.add_space(10.0);
                        ui::group_title(ui, Icon::Headphones, "Sortie");
                        let mut out_pct = self.output_gain * 100.0;
                        if ui
                            .add(
                                egui::Slider::new(&mut out_pct, 0.0..=200.0)
                                    .text("volume")
                                    .suffix(" %")
                                    .integer(),
                            )
                            .changed()
                        {
                            self.output_gain = out_pct / 100.0;
                            apply = true;
                        }
                        ui.add_space(6.0);
                        if ui::button(ui, Icon::Play, "Jouer un son de test").clicked() {
                            if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
                                engine.play_test_tone();
                            }
                        }

                        // --- Effets sonores ---
                        ui.add_space(12.0);
                        ui::hairline(ui);
                        ui.add_space(10.0);
                        ui::group_title(ui, Icon::Play, "Sons & notifications");
                        // L'état du système, en clair : si un jour « pas de
                        // son », cette ligne dit immédiatement où chercher.
                        let engine_ok = self.link.engine.lock().unwrap().is_some();
                        ui::hint(
                            ui,
                            &format!(
                                "{} sons chargés ({} perso) · sortie audio {}",
                                self.sounds.len(),
                                self.custom_sfx.len(),
                                if engine_ok { "active" } else { "INACTIVE — connecte-toi" },
                            ),
                        );
                        ui.checkbox(&mut self.sfx_on, "Jouer les sons");
                        if self.sfx_on {
                            let mut pct = self.sfx_volume * 100.0;
                            if ui
                                .add(
                                    egui::Slider::new(&mut pct, 0.0..=100.0)
                                        .suffix(" %")
                                        .integer()
                                        .text("volume des sons"),
                                )
                                .changed()
                            {
                                self.sfx_volume = pct / 100.0;
                            }
                            ui.add_space(6.0);

                            // Un réglage par événement : à couper, à écouter,
                            // et l'étiquette dit si le son est personnalisé.
                            for (name, label) in [
                                (sfx::MESSAGE, "Message reçu (fenêtre à l'arrière-plan)"),
                                (sfx::PEER_JOIN, "Quelqu'un arrive dans mon vocal"),
                                (sfx::PEER_LEAVE, "Quelqu'un quitte mon vocal"),
                                (sfx::SELF_JOIN, "Je rejoins un vocal"),
                                (sfx::SELF_LEAVE, "Je quitte le vocal"),
                                (sfx::MUTE, "Micro coupé"),
                                (sfx::UNMUTE, "Micro réactivé"),
                            ] {
                                ui.horizontal(|ui| {
                                    let mut enabled = !self.sfx_muted.contains(name);
                                    if ui.checkbox(&mut enabled, label).changed() {
                                        if enabled {
                                            self.sfx_muted.remove(name);
                                        } else {
                                            self.sfx_muted.insert(name.to_string());
                                        }
                                    }
                                    if self.custom_sfx.contains(name) {
                                        ui.label(
                                            RichText::new("perso")
                                                .color(ACCENT)
                                                .size(10.5),
                                        );
                                    }
                                    if ui.small_button("▶").on_hover_text("écouter").clicked()
                                    {
                                        // La préécoute ignore la coupure de
                                        // l'événement, pas le volume.
                                        if let Some(pcm) = self.sounds.get(name) {
                                            if let Some(engine) =
                                                self.link.engine.lock().unwrap().as_ref()
                                            {
                                                engine.play_effect(pcm, self.sfx_volume);
                                            }
                                        }
                                    }
                                });
                            }
                            if self.conn.is_none() {
                                ui::hint(ui, "connecte-toi pour la préécoute ▶");
                            }
                        }

                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui::button(ui, Icon::Paperclip, "Dossier des sons").clicked() {
                                if let Some(dir) = sound_dirs().into_iter().nth(1) {
                                    let _ = std::fs::create_dir_all(&dir);
                                    let _ = std::process::Command::new("explorer")
                                        .arg(&dir)
                                        .spawn();
                                }
                            }
                            if ui::button(ui, Icon::Refresh, "Recharger").clicked() {
                                self.reload_sounds();
                            }
                        });
                        ui::hint(
                            ui,
                            "les sons par défaut sont intégrés ; dépose des .wav (48 kHz \
                             conseillé) nommés message, arrivee, depart, rejoint-vocal, \
                             quitte-vocal, micro-coupe, micro-actif pour les remplacer",
                        );

                        // --- Réseau & qualité ---
                        ui.add_space(12.0);
                        ui::hairline(ui);
                        ui.add_space(10.0);
                        ui::group_title(ui, Icon::Info, "Réseau & qualité");
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("débit voix").color(TEXT_DIM).size(12.5));
                            let selected = if self.bitrate == 0 {
                                format!("Auto ({} kbps)", self.auto_bitrate / 1000)
                            } else {
                                format!("{} kbps", self.bitrate / 1000)
                            };
                            egui::ComboBox::from_id_salt("bitrate")
                                .width(150.0)
                                .selected_text(RichText::new(selected).color(TEXT))
                                .show_ui(ui, |ui| {
                                    if ui.selectable_label(self.bitrate == 0, "Auto").clicked() {
                                        self.bitrate = 0;
                                        apply = true;
                                    }
                                    for br in BITRATES {
                                        if ui
                                            .selectable_label(
                                                self.bitrate == br,
                                                format!("{} kbps", br / 1000),
                                            )
                                            .clicked()
                                        {
                                            self.bitrate = br;
                                            apply = true;
                                        }
                                    }
                                });
                        });
                        ui::hint(ui, "Auto : s'adapte aux pertes mesurées par le serveur.");
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("protection pertes (DRED)")
                                    .color(TEXT_DIM)
                                    .size(12.5),
                            );
                            let dred_label = |m: u8| match m {
                                0 => "Désactivée".to_string(),
                                2 => "Toujours".to_string(),
                                _ => "Auto (recommandé)".to_string(),
                            };
                            egui::ComboBox::from_id_salt("dred_mode")
                                .width(150.0)
                                .selected_text(
                                    RichText::new(dred_label(self.dred_mode)).color(TEXT),
                                )
                                .show_ui(ui, |ui| {
                                    for mode in [1u8, 0, 2] {
                                        if ui
                                            .selectable_label(
                                                self.dred_mode == mode,
                                                dred_label(mode),
                                            )
                                            .clicked()
                                        {
                                            self.dred_mode = mode;
                                            apply = true;
                                        }
                                    }
                                });
                        });
                        ui::hint(
                            ui,
                            match self.dred_mode {
                                0 => "redondance neuronale coupée",
                                2 => "≈1 s de voix re-transmise en continu dans chaque paquet",
                                _ if self.dred_active => {
                                    "ENGAGÉE — pertes détectées, la voix est protégée"
                                }
                                _ => "en veille : s'engage automatiquement dès 2 % de pertes",
                            },
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("tampon de gigue").color(TEXT_DIM).size(12.5));
                            let jitter_label = |frames: usize| match frames {
                                0 => "Auto (adaptatif)".to_string(),
                                f => format!("{} ms", f * 20),
                            };
                            egui::ComboBox::from_id_salt("jitter")
                                .width(150.0)
                                .selected_text(
                                    RichText::new(jitter_label(self.jitter_frames)).color(TEXT),
                                )
                                .show_ui(ui, |ui| {
                                    for frames in [0usize, 2, 3, 4, 6, 8] {
                                        if ui
                                            .selectable_label(
                                                self.jitter_frames == frames,
                                                jitter_label(frames),
                                            )
                                            .clicked()
                                        {
                                            self.jitter_frames = frames;
                                            apply = true;
                                        }
                                    }
                                });
                        });
                        ui::hint(ui, "Auto recommandé ; fixe si la voix est hachée.");

                        if engine_up {
                            ui.add_space(10.0);
                            let ping_txt = match voice.ping {
                                Some(p) => format!("{p} ms"),
                                None => "—".into(),
                            };
                            let upstream = match self.upstream_loss {
                                Some(l) => format!(" · pertes montantes {l:.1} %"),
                                None => String::new(),
                            };
                            ui::hint(
                                ui,
                                &format!(
                                    "ping {ping_txt} · gigue max {:.1} ms · perdus {} \
                                 (récupérés FEC : {}) · rejetés {}{upstream}",
                                    stats.worst_jitter_ms,
                                    stats.packets_lost,
                                    stats.packets_recovered,
                                    stats.packets_rejected,
                                ),
                            );
                        }

                        // --- Labo vidéo (S1a du partage d'écran) ---
                        ui.add_space(10.0);
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("🧪 labo vidéo").color(TEXT_DIM).size(12.5),
                            );
                            let running = self.labo.is_some();
                            let label = if running { "Arrêter le test" } else { "Se voir (test local)" };
                            if ui.button(label).clicked() {
                                if running {
                                    self.stop_labo();
                                } else {
                                    self.start_labo(ctx.clone());
                                }
                            }
                        });
                        ui::hint(
                            ui,
                            "capture ton écran principal et l'affiche après un aller-retour \
                             H.264 complet — zéro réseau, c'est le banc d'essai du stream",
                        );

                        if let Some(info) = self.info.clone() {
                            ui.add_space(10.0);
                            if ui::banner(ui, Tone::Accent, &info, true) {
                                self.info = None;
                            }
                        }
                    });
            });

        if apply {
            self.apply_audio_settings();
        }
        if restart {
            {
                self.link.restart_voice(self.voice_prefs());
            }
        }
        if !open {
            self.close_settings();
        }
    }

    fn close_settings(&mut self) {
        self.show_settings = false;
        self.info = None;
        if self.calibrating.take().is_some() || self.loopback {
            self.loopback = false;
            self.apply_audio_settings();
        }
    }

    /// Fenêtre de mise à jour : proposée, jamais imposée.
    ///
    /// Elle se pose par-dessus l'écran courant, quel qu'il soit — la sonde
    /// GitHub répond quand elle répond, et rien ne garantit qu'on soit encore
    /// sur le lanceur à ce moment-là.
    fn update_window(&mut self, ctx: &egui::Context) {
        let status = self.updater.status();
        if matches!(status, update::Status::Idle) {
            return;
        }

        // Titre figé : le changer d'un état à l'autre ferait repartir la
        // fenêtre au centre à chaque transition.
        egui::Window::new("Mise à jour")
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| match status {
                update::Status::Idle => {}
                update::Status::Available(release) => {
                    ui.label(
                        RichText::new(format!("Version {} disponible", release.version))
                            .size(16.0)
                            .strong(),
                    );
                    ui::hint(ui, &format!("Version installée : {}", update::current()));

                    if !release.notes.is_empty() {
                        ui.add_space(8.0);
                        egui::ScrollArea::vertical()
                            .max_height(160.0)
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(release.notes.as_str())
                                        .color(TEXT_DIM)
                                        .size(13.0),
                                );
                            });
                    }

                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui::primary_button(ui, Some(Icon::ArrowDown), "Mettre à jour", None)
                            .clicked()
                        {
                            self.updater.accept(&release, ctx.clone());
                        }
                        // Libellé franc : le refus est mémorisé, on ne
                        // redemandera qu'à la version suivante. « Plus tard »
                        // promettrait une relance qui n'aura pas lieu.
                        if ui::button(ui, Icon::Close, "Ignorer cette version").clicked() {
                            self.updater.skip(&release.version);
                        }
                    });
                    ui::hint(ui, "L'application redémarrera d'elle-même une fois à jour.");
                }
                update::Status::Downloading { done, total } => {
                    ui.label(RichText::new("Téléchargement…").size(16.0).strong());
                    ui.add_space(8.0);
                    let ratio = if total > 0 {
                        done as f32 / total as f32
                    } else {
                        0.0
                    };
                    ui::meter(ui, ratio, Vec2::new(ui.available_width(), 8.0), theme::ACCENT);
                    ui::hint(
                        ui,
                        &format!("{:.1} Mo sur {:.1} Mo", megabytes(done), megabytes(total)),
                    );
                }
                update::Status::Ready => {
                    ui.label(RichText::new("Mise à jour installée").size(16.0).strong());
                    ui::hint(ui, "Redémarrage…");
                    // L'utilisateur a déjà dit oui : on ne lui redemande pas
                    // s'il veut bien relancer. La fenêtre se ferme, eframe
                    // enregistre, `main` relance.
                    update::request_restart();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                update::Status::Failed(message) => {
                    ui.label(RichText::new("Mise à jour impossible").size(16.0).strong());
                    ui.label(RichText::new(message.as_str()).color(DANGER).size(13.0));
                    ui.add_space(8.0);
                    ui.hyperlink_to(
                        "Télécharger depuis GitHub",
                        update::Updater::releases_page(),
                    );
                    ui.add_space(8.0);
                    if ui::button(ui, Icon::Close, "Fermer").clicked() {
                        self.updater.dismiss();
                    }
                }
            });
    }

    /// Fenêtre « Mon compte » : changement de son propre mot de passe.
    fn account_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let mut to_send: Option<ClientMsg> = None;
        egui::Window::new("Mon compte")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(300.0)
            .show(ctx, |ui| {
                // Photo rapportée par le thread du sélecteur de fichier.
                if let Some(outcome) = self.picked_avatar.lock().unwrap().take() {
                    match outcome {
                        Ok(data) => self.account_avatar = IconChange::Set { data },
                        Err(e) => self.error = Some(format!("image illisible : {e}")),
                    }
                }

                // Aperçu : la photo en attente d'envoi, sinon celle en place.
                let pending = match &self.account_avatar {
                    IconChange::Set { data } => Some(data.clone()),
                    IconChange::Clear => None,
                    IconChange::Keep => None,
                };
                let staged = self.preview_texture(ctx, Apercu::Avatar, pending.as_deref());
                let mine = self
                    .my_id
                    .and_then(|id| self.avatars.get(&id))
                    .map(|(_, texture)| texture.clone());
                let shown = match (&self.account_avatar, &staged) {
                    (IconChange::Set { .. }, Some(texture)) => Some(texture.clone()),
                    (IconChange::Clear, _) => None,
                    _ => mine.clone(),
                };

                ui.horizontal(|ui| {
                    let me = self.username.clone();
                    ui::avatar(ui, &me, 56.0, false, shown.as_ref(), theme::BG_RAISED);
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&me).color(color_for(&me)).size(16.0).strong());
                        // Le rôle vaut mieux qu'un « administrateur/membre »
                        // binaire : c'est lui que les autres voient.
                        let role = self
                            .members
                            .iter()
                            .find(|m| Some(m.user_id) == self.my_id)
                            .and_then(|m| self.top_role_name(m))
                            .unwrap_or("membre")
                            .to_string();
                        ui.label(RichText::new(role).color(TEXT_FAINT).size(12.0));
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui::button(ui, Icon::Pencil, "Photo").clicked() {
                                self.pick_image(ctx, "Photo de profil", &self.picked_avatar);
                            }
                            if shown.is_some()
                                && ui::button(ui, Icon::Trash, "Retirer").clicked()
                            {
                                self.account_avatar = IconChange::Clear;
                            }
                        });
                    });
                });

                let dirty = !matches!(self.account_avatar, IconChange::Keep);
                if dirty {
                    ui.add_space(8.0);
                    let clicked =
                        ui::primary_button(ui, Some(Icon::Check), "Enregistrer la photo", None)
                            .clicked();
                    if clicked {
                        to_send = Some(ClientMsg::SetAvatar {
                            avatar: std::mem::take(&mut self.account_avatar),
                        });
                        self.preview_icons.remove(&Apercu::Avatar);
                    }
                } else {
                    ui.add_space(4.0);
                    ui::hint(ui, "PNG ou JPG, réduit en vignette 64×64.");
                }

                ui.add_space(12.0);
                ui::hairline(ui);
                ui.add_space(10.0);

                ui::group_title(ui, Icon::Key, "Changer mon mot de passe");
                ui::field_label(ui, "Mot de passe actuel");
                ui.add(ui::text_field(&mut self.old_password, "", true));
                ui.add_space(8.0);
                ui::field_label(ui, "Nouveau mot de passe");
                ui.add(ui::text_field(
                    &mut self.new_password,
                    "6 caractères minimum",
                    true,
                ));
                if !self.new_password.is_empty() && self.new_password.len() < 6 {
                    ui.add_space(4.0);
                    ui::hint(ui, "6 caractères minimum");
                }
                ui.add_space(12.0);
                let ready = !self.old_password.is_empty() && self.new_password.len() >= 6;
                let clicked = ui
                    .add_enabled_ui(ready, |ui| {
                        ui::primary_button(
                            ui,
                            Some(Icon::Check),
                            "Changer le mot de passe",
                            Some(ui.available_width()),
                        )
                    })
                    .inner
                    .clicked();
                if clicked {
                    to_send = Some(ClientMsg::ChangePassword {
                        old_password: self.old_password.clone(),
                        new_password: self.new_password.clone(),
                    });
                    self.old_password.clear();
                    self.new_password.clear();
                }
                if let Some(info) = self.info.clone() {
                    ui.add_space(10.0);
                    if ui::banner(ui, Tone::Accent, &info, true) {
                        self.info = None;
                    }
                }
            });
        if let Some(msg) = to_send {
            self.send(msg);
        }
        if !open {
            self.close_account();
        }
    }

    fn close_account(&mut self) {
        self.show_account = false;
        self.old_password.clear();
        self.new_password.clear();
        self.info = None;
        self.account_avatar = IconChange::Keep;
        self.preview_icons.remove(&Apercu::Avatar);
    }

    /// Fenêtre d'administration : invitations, comptes, blocages, mots de passe.
    /// Texture d'aperçu d'une vignette, reconstruite seulement si elle change.
    ///
    /// `emplacement` distingue les deux aperçus possibles. Sans lui, ouvrir
    /// « Mon compte » et Admin ▸ Serveur ensemble faisait alterner la clé à
    /// chaque image : un décodage PNG et un téléversement GPU par panneau et
    /// par image, pour deux vignettes qui ne changeaient pas.
    fn preview_texture(
        &mut self,
        ctx: &egui::Context,
        emplacement: Apercu,
        data: Option<&str>,
    ) -> Option<egui::TextureHandle> {
        let Some(data) = data else {
            self.preview_icons.remove(&emplacement);
            return None;
        };
        let a_jour = self
            .preview_icons
            .get(&emplacement)
            .is_some_and(|(key, _)| key == data);
        if !a_jour {
            match servers::decode_icon(data) {
                Some(image) => {
                    let texture = ctx.load_texture(
                        format!("icon-preview-{emplacement:?}"),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.preview_icons.insert(emplacement, (data.to_string(), texture));
                }
                None => {
                    self.preview_icons.remove(&emplacement);
                }
            }
        }
        self.preview_icons.get(&emplacement).map(|(_, texture)| texture.clone())
    }

    /// Identité du serveur : nom et logo, que seuls les admins règlent.
    /// C'est ici — et nulle part côté client — que le logo se change.
    fn server_identity_section(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        to_send: &mut Vec<ClientMsg>,
    ) {
        // Vignette rapportée par le thread du sélecteur de fichier.
        if let Some(outcome) = self.picked_icon.lock().unwrap().take() {
            match outcome {
                Ok(data) => self.admin_icon = IconChange::Set { data },
                Err(e) => self.error = Some(format!("image illisible : {e}")),
            }
        }

        ui::group_title(ui, Icon::Server, "Identité du serveur");

        // Ce que verra tout le monde une fois enregistré.
        let pending = match &self.admin_icon {
            IconChange::Set { data } => Some(data.clone()),
            IconChange::Clear => None,
            IconChange::Keep => self.server_info.icon.clone(),
        };
        let preview = self.preview_texture(ctx, Apercu::LogoServeur, pending.as_deref());
        let has_icon = pending.is_some();

        ui.horizontal_top(|ui| {
            let (badge, _) = ui.allocate_exact_size(Vec2::splat(62.0), Sense::hover());
            ui::paint_server_badge(
                ui.painter(),
                badge,
                &self.admin_name,
                self.url.trim(),
                preview.as_ref(),
            );
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui::field_label(ui, "Nom du serveur");
                ui.add(ui::text_field(&mut self.admin_name, "ex. Chez Kévin", false));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui::button(ui, Icon::Pencil, "Choisir un logo").clicked() {
                        self.pick_image(ctx, "Logo du serveur", &self.picked_icon);
                    }
                    if has_icon && ui::button(ui, Icon::Trash, "Retirer").clicked() {
                        self.admin_icon = IconChange::Clear;
                    }
                });
            });
        });
        ui.add_space(6.0);
        ui::hint(
            ui,
            "PNG ou JPG, réduit en vignette 64×64. Nom et logo sont poussés à \
             tous les membres : eux ne peuvent pas les changer.",
        );

        let dirty = !matches!(self.admin_icon, IconChange::Keep)
            || self.admin_name.trim() != self.server_info.name;
        let too_long = self.admin_name.trim().chars().count() > ki_protocol::MAX_SERVER_NAME;
        if too_long {
            ui.add_space(4.0);
            ui::hint(
                ui,
                &format!("{} caractères maximum", ki_protocol::MAX_SERVER_NAME),
            );
        }

        ui.add_space(10.0);
        let clicked = ui
            .add_enabled_ui(dirty && !too_long, |ui| {
                ui::primary_button(ui, Some(Icon::Check), "Appliquer au serveur", None)
            })
            .inner
            .clicked();
        if clicked {
            to_send.push(ClientMsg::AdminSetServerInfo {
                name: Some(self.admin_name.trim().to_string()),
                icon: std::mem::take(&mut self.admin_icon),
            });
        }
    }

    /// La demande de bascule du micro en catégorie « communications ».
    ///
    /// Le moteur a détecté un micro affamé (il s'ouvre, rien n'en sort) et
    /// propose la seule parade logicielle : partager la voie de traitement
    /// avec la voix du jeu. Mais cette bascule peut faire baisser le volume
    /// des autres sons (l'atténuation Windows) — alors on demande, on
    /// n'impose pas. Refuser vaut pour la session : à l'utilisateur de
    /// régler son casque, le docteur audio lui dit comment.
    fn comms_popup(&mut self, ctx: &egui::Context, voice: &VoiceSnapshot) {
        if !voice.comms_proposal {
            return;
        }
        let mut reponse: Option<bool> = None;
        egui::Window::new("Ton micro ne livre rien")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -40.0])
            .show(ctx, |ui| {
                ui.set_max_width(380.0);
                ui.label(
                    "Ton micro s'ouvre mais ne capte rien : un autre logiciel — la \
                     voix intégrée d'un jeu, le pilote du casque — tient probablement \
                     la voie de capture.",
                );
                ui.add_space(6.0);
                ui.label(
                    "ki-chat peut demander la même voie que lui (catégorie \
                     « communications » de Windows). Revers possible : Windows peut \
                     alors baisser le volume de tes autres sons — réglable dans \
                     Panneau son → Communications → « Ne rien faire ».",
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui::button(ui, Icon::Check, "Basculer (cette session)").clicked() {
                        reponse = Some(true);
                    }
                    if ui::button(ui, Icon::Close, "Non — je règle mon casque").clicked() {
                        reponse = Some(false);
                    }
                });
                ui.add_space(4.0);
                ui::hint(
                    ui,
                    "pour le rendre permanent : ⚙ Audio → « Partager le micro avec \
                     la voix du jeu ». Pour comprendre : le docteur audio, au même \
                     endroit.",
                );
            });
        if let Some(accept) = reponse {
            if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
                engine.resolve_comms(accept);
            }
            self.info = Some(
                if accept {
                    "micro basculé en catégorie communications pour cette session"
                } else {
                    "compris — le micro reste en catégorie standard"
                }
                .into(),
            );
        }
    }

    /// Onglet Diagnostics : les journaux techniques que les joueurs
    /// volontaires partagent (⚙ Audio → « Partager mes diagnostics »). Tout
    /// se récupère en un clic, s'affiche, et se copie d'un bloc — pour être
    /// collé à qui débogue.
    fn admin_diag_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui::group_title(ui, Icon::Info, "Diagnostics des joueurs");
        ui::hint(
            ui,
            "n'existe que pour ceux qui ont coché « Partager mes diagnostics » dans \
             leurs réglages audio — technique seulement, jamais les messages ni la voix",
        );
        ui.add_space(6.0);
        if ui::button(ui, Icon::Refresh, "Récupérer tous les diagnostics").clicked() {
            let base = self.http_base();
            let agent = self.http_agent();
            let token_hex = format!("{:x}", self.voice_token);
            let slot = self.diag_admin.clone();
            *slot.lock().unwrap() = Some("récupération en cours…".into());
            std::thread::spawn(move || {
                let resultat = (|| -> Result<String, String> {
                    let liste = agent
                        .get(&format!("{base}/diag"))
                        .set("x-ki-token", &token_hex)
                        .call()
                        .map_err(|e| e.to_string())?
                        .into_string()
                        .map_err(|e| e.to_string())?;
                    if liste.trim().is_empty() {
                        return Ok("aucun diagnostic reçu pour l'instant — personne n'a \
                                   coché l'option, ou le serveur vient d'être redéployé"
                            .into());
                    }
                    // La fin de chaque archive suffit : c'est le passé récent
                    // qu'on débogue, et l'affichage comme le presse-papiers
                    // n'ont pas à charrier des mégaoctets d'historique.
                    let mut tout = String::new();
                    for ligne in liste.lines() {
                        let Some(fichier) = ligne.split('\t').next() else { continue };
                        tout.push_str(&format!("\n===== {ligne} =====\n"));
                        match agent
                            .get(&format!("{base}/diag/{fichier}?tail=65536"))
                            .set("x-ki-token", &token_hex)
                            .call()
                        {
                            Ok(r) => tout.push_str(&r.into_string().unwrap_or_default()),
                            Err(e) => tout.push_str(&format!("(illisible : {e})\n")),
                        }
                    }
                    Ok(tout)
                })();
                *slot.lock().unwrap() = Some(match resultat {
                    Ok(t) => t,
                    Err(e) => format!("échec de la récupération : {e}"),
                });
            });
        }
        let contenu = self.diag_admin.lock().unwrap().clone();
        if let Some(texte) = contenu {
            ui.add_space(6.0);
            if ui::button(ui, Icon::Copy, "Tout copier").clicked() {
                ctx.copy_text(texte.clone());
                self.info = Some("diagnostics copiés — prêts à coller".into());
            }
            ui.add_space(4.0);
            // L'affichage est borné aux derniers caractères : des mégaoctets
            // de journal mettraient l'interface à genoux. La copie, elle,
            // emporte tout ce qui a été récupéré.
            let debut = texte
                .char_indices()
                .rev()
                .nth(20_000)
                .map(|(i, _)| i)
                .unwrap_or(0);
            egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                ui.label(
                    RichText::new(&texte[debut..]).monospace().size(11.0).color(TEXT),
                );
            });
        }
    }

    fn admin_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let mut to_send: Vec<ClientMsg> = Vec::new();
        let roomy = (ctx.screen_rect().height() - 120.0).clamp(320.0, 900.0);
        egui::Window::new("Administration")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(460.0)
            .default_height(roomy)
            .min_width(360.0)
            .min_height(260.0)
            .show(ctx, |ui| {
                // Onglets : tout tenait auparavant dans une seule colonne
                // déroulante, qui ne se lisait plus une fois les
                // bannissements et le journal ajoutés.
                ui.horizontal_wrapped(|ui| {
                    let tabs: Vec<AdminTab> =
                        AdminTab::ALL.into_iter().filter(|t| self.can(t.needs())).collect();
                    for tab in tabs {
                        if ui.selectable_label(self.admin_tab == tab, tab.label()).clicked() {
                            self.admin_tab = tab;
                            // Le journal ne se charge qu'à l'ouverture de
                            // son onglet : inutile de le pousser à chaque
                            // ouverture du panneau.
                            if tab == AdminTab::Audit {
                                to_send.push(ClientMsg::AdminAuditLog { limit: 200 });
                            }
                        }
                    }
                });
                ui.add_space(8.0);
                ui::hairline(ui);
                ui.add_space(10.0);

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // L'onglet ouvert peut être devenu hors de portée si
                        // les rôles viennent de changer sous nos pieds.
                        if !self.can(self.admin_tab.needs()) {
                            self.admin_tab = AdminTab::ALL
                                .into_iter()
                                .find(|t| self.can(t.needs()))
                                .unwrap_or(AdminTab::Server);
                        }
                        match self.admin_tab {
                            AdminTab::Server => self.server_identity_section(ui, ctx, &mut to_send),
                            AdminTab::Channels => self.admin_channels_tab(ui, &mut to_send),
                            AdminTab::Roles => self.admin_roles_tab(ui, &mut to_send),
                            AdminTab::Members => self.admin_members_tab(ui, &mut to_send),
                            AdminTab::Invites => self.admin_invites_tab(ui, ctx, &mut to_send),
                            AdminTab::Audit => self.admin_audit_tab(ui),
                            AdminTab::Diagnostics => self.admin_diag_tab(ui, ctx),
                        }
                        if let Some(info) = self.info.clone() {
                            ui.add_space(10.0);
                            if ui::banner(ui, Tone::Accent, &info, true) {
                                self.info = None;
                            }
                        }
                    });
            });
        for msg in to_send {
            self.send(msg);
        }
        if !open {
            self.close_admin();
        }
    }

    /// Onglet « Salons » : créer, renommer, restreindre, supprimer.
    fn admin_channels_tab(&mut self, ui: &mut egui::Ui, to_send: &mut Vec<ClientMsg>) {
        ui::group_title(ui, Icon::Plus, "Nouveau salon");
        let draft = self.channel_draft.get_or_insert_with(|| ChannelDraft {
            name: String::new(),
            kind: ChannelKind::Text,
            allowed_roles: Vec::new(),
            restricted: false,
        });
        ui.add(ui::text_field(&mut draft.name, "nom du salon", false));
        ui.horizontal(|ui| {
            ui.selectable_value(&mut draft.kind, ChannelKind::Text, "Textuel");
            ui.selectable_value(&mut draft.kind, ChannelKind::Voice, "Vocal");
        });
        let roles = self.roles.clone();
        restriction_par_roles(ui, &roles, draft);
        let ready = !draft.name.trim().is_empty()
            && (!draft.restricted || !draft.allowed_roles.is_empty());
        let create = ui
            .add_enabled_ui(ready, |ui| ui::button(ui, Icon::Plus, "Créer le salon"))
            .inner
            .clicked();
        if create {
            to_send.push(ClientMsg::AdminCreateChannel {
                name: draft.name.trim().to_string(),
                kind: draft.kind,
                allowed_roles: draft.restricted.then(|| draft.allowed_roles.clone()),
            });
            self.channel_draft = None;
        }

        ui.add_space(12.0);
        ui::hairline(ui);
        ui.add_space(10.0);
        ui::group_title(ui, Icon::Hash, "Salons existants");
        let channels = self.channels.clone();
        // L'ordre affiché **est** l'ordre du serveur : la liste arrive déjà
        // triée par position, et qui gère les salons les voit tous. C'est ce
        // qui permet d'envoyer une permutation exacte — le serveur refuse
        // toute liste incomplète, et une vue filtrée en serait une.
        let ids: Vec<ki_protocol::ChannelId> = channels.iter().map(|c| c.id).collect();
        for (i, ch) in channels.iter().enumerate() {
            ui.horizontal(|ui| {
                ui::glyph(
                    ui,
                    if ch.kind == ChannelKind::Voice { Icon::Volume } else { Icon::Hash },
                    13.0,
                    TEXT_DIM,
                );
                ui.label(RichText::new(&ch.name).color(TEXT).size(13.0));
                if ch.allowed_roles.is_some() {
                    ui.label(RichText::new("privé").color(ACCENT).size(11.0));
                }
                if ch.locked {
                    ui.label(RichText::new("verrouillé").color(WARN).size(11.0));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui::icon_button_ex(ui, Icon::Trash, 24.0, "Supprimer", None).clicked() {
                        to_send.push(ClientMsg::AdminDeleteChannel { channel: ch.id });
                    }
                    if ui::icon_button_ex(ui, Icon::Pencil, 24.0, "Renommer", None).clicked() {
                        // Un formulaire déjà ouvert sur ce salon se referme :
                        // le crayon fait aller-retour, sans bouton « annuler »
                        // à chercher.
                        self.channel_edit = match self.channel_edit.take() {
                            Some(e) if e.id == ch.id => None,
                            _ => Some(ChannelEdit {
                                id: ch.id,
                                position: ch.position,
                                draft: ChannelDraft {
                                    name: ch.name.clone(),
                                    kind: ch.kind,
                                    restricted: ch.allowed_roles.is_some(),
                                    allowed_roles: ch.allowed_roles.clone().unwrap_or_default(),
                                },
                            }),
                        };
                    }
                    // Le verrou n'a de sens que sur un salon vocal : c'est le
                    // seul endroit où l'on entre, et donc le seul où l'on
                    // puisse être arrêté à la porte.
                    if ch.kind == ChannelKind::Voice {
                        let quoi = if ch.locked { "Changer le verrou" } else { "Verrouiller" };
                        let teinte = ch.locked.then_some(WARN);
                        if ui::icon_button_ex(ui, Icon::Key, 24.0, quoi, teinte).clicked() {
                            self.verrou_draft = match self.verrou_draft.take() {
                                Some(v) if v.channel == ch.id => None,
                                _ => Some(VerrouDraft {
                                    channel: ch.id,
                                    mot_de_passe: String::new(),
                                    ttl_secs: VOICE_LOCK_DURATIONS[1].1,
                                }),
                            };
                        }
                    }
                    // Monter et descendre plutôt qu'un glisser-déposer : sur
                    // une dizaine de salons c'est aussi rapide, et ça ne
                    // dépend pas de la précision de la souris.
                    let bouger = |haut: bool| -> ClientMsg {
                        let mut order = ids.clone();
                        order.swap(i, if haut { i - 1 } else { i + 1 });
                        ClientMsg::AdminReorderChannels { order }
                    };
                    if ui
                        .add_enabled_ui(i + 1 < ids.len(), |ui| {
                            ui::icon_button_ex(ui, Icon::ArrowDown, 24.0, "Descendre", None)
                        })
                        .inner
                        .clicked()
                    {
                        to_send.push(bouger(false));
                    }
                    if ui
                        .add_enabled_ui(i > 0, |ui| {
                            ui::icon_button_ex(ui, Icon::ArrowUp, 24.0, "Monter", None)
                        })
                        .inner
                        .clicked()
                    {
                        to_send.push(bouger(true));
                    }
                });
            });

            // Les deux formulaires s'ouvrent sous leur salon, et pas dans une
            // fenêtre à part : on voit ce qu'on modifie.
            if self.channel_edit.as_ref().is_some_and(|e| e.id == ch.id) {
                let mut valider = false;
                let mut annuler = false;
                ui.indent(("edition", ch.id), |ui| {
                    let edit = self.channel_edit.as_mut().expect("vérifié à l'instant");
                    ui.add(ui::text_field(&mut edit.draft.name, "nom du salon", false));
                    restriction_par_roles(ui, &roles, &mut edit.draft);
                    let ok = !edit.draft.name.trim().is_empty()
                        && (!edit.draft.restricted || !edit.draft.allowed_roles.is_empty());
                    ui.horizontal(|ui| {
                        valider = ui
                            .add_enabled_ui(ok, |ui| ui::button(ui, Icon::Check, "Enregistrer"))
                            .inner
                            .clicked();
                        annuler = ui::button(ui, Icon::Close, "Annuler").clicked();
                    });
                });
                if valider {
                    let edit = self.channel_edit.take().expect("vérifié à l'instant");
                    to_send.push(ClientMsg::AdminEditChannel {
                        channel: ki_protocol::ChannelInfo {
                            id: edit.id,
                            name: edit.draft.name.trim().to_string(),
                            // La nature ne se change pas — le serveur le
                            // refuse — mais le message remplace tout le
                            // salon : il faut la lui redire à l'identique.
                            kind: edit.draft.kind,
                            // Idem pour la position : la taire reviendrait à
                            // demander le rang 0, et renommer un salon le
                            // ferait remonter en tête de liste.
                            position: edit.position,
                            locked: false,
                            allowed_roles: edit
                                .draft
                                .restricted
                                .then(|| edit.draft.allowed_roles.clone()),
                        },
                    });
                } else if annuler {
                    self.channel_edit = None;
                }
            }

            if self.verrou_draft.as_ref().is_some_and(|v| v.channel == ch.id) {
                let mut poser = false;
                let mut retirer = false;
                ui.indent(("verrou", ch.id), |ui| {
                    let v = self.verrou_draft.as_mut().expect("vérifié à l'instant");
                    ui.add(ui::text_field(&mut v.mot_de_passe, "mot de passe", true));
                    ui.horizontal_wrapped(|ui| {
                        for (label, secs) in VOICE_LOCK_DURATIONS {
                            ui.selectable_value(&mut v.ttl_secs, *secs, *label);
                        }
                    });
                    let ok = !v.mot_de_passe.is_empty();
                    ui.horizontal(|ui| {
                        poser = ui
                            .add_enabled_ui(ok, |ui| ui::button(ui, Icon::Key, "Poser le verrou"))
                            .inner
                            .clicked();
                        // Retirer n'est proposé que s'il y a quelque chose à
                        // retirer, sinon le bouton ne fait rien de visible.
                        retirer = ch.locked
                            && ui::button(ui, Icon::Close, "Retirer le verrou").clicked();
                    });
                    ui::hint(
                        ui,
                        "qui gère les salons entre sans le mot de passe : \
                         poser un verrou ne s'enferme pas dehors",
                    );
                });
                if poser || retirer {
                    let v = self.verrou_draft.take().expect("vérifié à l'instant");
                    to_send.push(ClientMsg::AdminSetVoicePassword {
                        channel: v.channel,
                        password: poser.then_some(v.mot_de_passe),
                        ttl_secs: v.ttl_secs,
                    });
                }
            }
        }
        ui::hint(
            ui,
            "supprimer un salon n'efface pas son historique : le fichier est archivé",
        );
    }

    /// Onglet « Rôles » : couleur, rang, permissions.
    fn admin_roles_tab(&mut self, ui: &mut egui::Ui, to_send: &mut Vec<ClientMsg>) {
        let roles = self.roles.clone();
        ui::group_title(ui, Icon::Crown, "Rôles");
        for role in &roles {
            ui.horizontal(|ui| {
                let color = theme::member_color(role.color, &role.name);
                ui::status_dot(ui, color, "", 10.0);
                ui.label(RichText::new(&role.name).color(color).size(13.0).strong());
                ui.label(RichText::new(format!("rang {}", role.rank)).color(TEXT_FAINT).size(11.0));
                if role.system {
                    ui.label(RichText::new("système").color(TEXT_FAINT).size(11.0));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // On ne touche qu'à ce qui est strictement sous son
                    // propre rang : sinon on se donnerait des pouvoirs.
                    if role.rank < self.my_rank
                        && !role.system
                        && ui::icon_button_ex(ui, Icon::Trash, 24.0, "Supprimer", None).clicked()
                    {
                        to_send.push(ClientMsg::AdminDeleteRole { id: role.id });
                    }
                    // `@everyone` se modifie aussi, et c'est indispensable :
                    // les permissions étant une union, c'est le seul endroit
                    // d'où l'on peut retirer un droit à tout le monde. Son nom
                    // et son rang restent figés, le serveur les refuse — seul
                    // l'administrateur y touche, pour la même raison.
                    let everyone = role.id == ki_protocol::ROLE_EVERYONE
                        && self.can(ki_protocol::perm::ADMINISTRATOR);
                    if ((role.rank < self.my_rank && !role.system) || everyone)
                        && ui::icon_button_ex(ui, Icon::Pencil, 24.0, "Modifier", None).clicked()
                    {
                        self.role_draft = Some(RoleDraft {
                            id: Some(role.id),
                            name: role.name.clone(),
                            color: theme::member_color(role.color, &role.name),
                            colored: role.color.is_some(),
                            rank: role.rank,
                            perms: role.perms,
                        });
                    }
                });
            });
        }

        ui.add_space(10.0);
        if self.role_draft.is_none() && ui::button(ui, Icon::Plus, "Nouveau rôle").clicked() {
            self.role_draft = Some(RoleDraft {
                id: None,
                name: String::new(),
                color: ACCENT,
                colored: true,
                // Par défaut juste en dessous de soi : un rôle créé au même
                // rang serait immédiatement hors de portée de son auteur.
                rank: self.my_rank.saturating_sub(1),
                perms: ki_protocol::perm::DEFAULT,
            });
        }

        let Some(mut draft) = self.role_draft.take() else { return };
        let mut keep = true;
        ui.add_space(8.0);
        ui::hairline(ui);
        ui.add_space(8.0);
        // `@everyone` : ni nom ni rang ne se changent, le serveur les refuse.
        // On règle uniquement ce que reçoit tout le monde.
        let everyone = draft.id == Some(ki_protocol::ROLE_EVERYONE);
        ui::field_label(ui, "Nom");
        ui.add_enabled_ui(!everyone, |ui| {
            ui.add(ui::text_field(&mut draft.name, "ex. Modérateur", false));
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.checkbox(&mut draft.colored, "Couleur de pseudo");
            if draft.colored {
                ui.color_edit_button_srgba(&mut draft.color);
            }
        });
        ui.add_space(6.0);
        if everyone {
            ui::hint(ui, "ce que reçoit tout le monde, y compris les comptes sans rôle");
        } else {
            let ceiling = self.my_rank.saturating_sub(1);
            ui.add(egui::Slider::new(&mut draft.rank, 0..=ceiling.max(1)).text("rang"));
            ui::hint(ui, "un rang supérieur l'emporte : on n'agit que sur plus bas que soi");
        }
        ui.add_space(6.0);
        ui::field_label(ui, "Permissions");
        for (bit, label, why) in ki_protocol::perm::ALL {
            // On n'accorde pas ce qu'on n'a pas soi-même : sans cette borne,
            // gérer les rôles suffirait à devenir administrateur.
            if !self.can(*bit) {
                continue;
            }
            // Et certaines n'ont pas de sens accordées à tout le monde : les
            // poser sur `@everyone` mettrait le serveur à plat, sans retour
            // possible. Le serveur les refuse, autant ne pas les proposer.
            if everyone && ki_protocol::perm::NOT_FOR_EVERYONE & bit != 0 {
                continue;
            }
            let mut on = draft.perms & bit != 0;
            let response = ui.checkbox(&mut on, *label);
            if !why.is_empty() {
                response.on_hover_text(*why);
            }
            if on {
                draft.perms |= bit;
            } else {
                draft.perms &= !bit;
            }
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let ready = !draft.name.trim().is_empty();
            let save = ui
                .add_enabled_ui(ready, |ui| {
                    ui::primary_button(ui, Some(Icon::Check), "Enregistrer", None)
                })
                .inner
                .clicked();
            if save {
                let color = draft.colored.then(|| {
                    let c = draft.color;
                    ((c.r() as u32) << 16) | ((c.g() as u32) << 8) | c.b() as u32
                });
                to_send.push(match draft.id {
                    Some(id) => ClientMsg::AdminEditRole {
                        role: ki_protocol::RoleInfo {
                            id,
                            name: draft.name.trim().to_string(),
                            color,
                            rank: draft.rank,
                            perms: draft.perms,
                            system: false,
                        },
                    },
                    None => ClientMsg::AdminCreateRole {
                        name: draft.name.trim().to_string(),
                        color,
                        rank: draft.rank,
                        perms: draft.perms,
                    },
                });
                keep = false;
            }
            if ui::button(ui, Icon::Close, "Annuler").clicked() {
                keep = false;
            }
        });
        // Remis en place seulement si l'édition continue : enregistrer ou
        // annuler referme le formulaire.
        if keep {
            self.role_draft = Some(draft);
        }
    }

    /// Onglet « Invitations » : créer, copier, révoquer.
    fn admin_invites_tab(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        to_send: &mut Vec<ClientMsg>,
    ) {
        ui::group_title(ui, Icon::Plus, "Nouveau code");
        ui::field_label(ui, "Étiquette (pour s'y retrouver plus tard)");
        ui.add(ui::text_field(&mut self.invite_label, "ex. tournoi du samedi", false));
        ui.add_space(8.0);

        ui::field_label(ui, "Nombre d'utilisations");
        ui.horizontal_wrapped(|ui| {
            for (label, uses) in
                [("1", Some(1u32)), ("5", Some(5)), ("25", Some(25)), ("Illimité", None)]
            {
                if ui.selectable_label(self.invite_uses == uses, label).clicked() {
                    self.invite_uses = uses;
                }
            }
        });
        if self.invite_uses.is_none() {
            ui::hint(ui, "lien permanent — chaque compte créé sera consigné au journal");
        }
        ui.add_space(8.0);
        if ui::button(ui, Icon::Plus, "Générer le code").clicked() {
            to_send.push(ClientMsg::AdminCreateInvite {
                uses: self.invite_uses,
                label: self.invite_label.trim().to_string(),
                ttl_secs: 0,
            });
            self.invite_label.clear();
        }

        if let Some(code) = self.last_invite.clone() {
            ui.add_space(8.0);
            egui::Frame::NONE
                .fill(theme::alpha(ACCENT, 24))
                .stroke(egui::Stroke::new(1.0_f32, theme::alpha(ACCENT, 80)))
                .corner_radius(egui::CornerRadius::same(9))
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(&code).color(ACCENT).strong().monospace().size(15.0),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui::icon_button(ui, Icon::Copy, "Copier « serveur + code »")
                                .clicked()
                            {
                                ctx.copy_text(format!(
                                    "serveur : {}  |  code d'invitation : {code}",
                                    self.url
                                ));
                            }
                            if ui::icon_button(ui, Icon::Key, "Copier le code").clicked() {
                                ctx.copy_text(code.clone());
                            }
                        });
                    });
                });
        }

        let invites = self.admin_invites.clone();
        if !invites.is_empty() {
            ui.add_space(12.0);
            ui::hairline(ui);
            ui.add_space(10.0);
            ui::group_title(ui, Icon::Key, "Codes émis");
            for invite in &invites {
                ui.horizontal(|ui| {
                    let color = if invite.revoked { TEXT_FAINT } else { TEXT_DIM };
                    let mut code = RichText::new(&invite.code).color(color).monospace().size(13.0);
                    if invite.revoked {
                        code = code.strikethrough();
                    }
                    ui.label(code);
                    // Un lien permanent se signale : c'est l'information
                    // qui décide s'il faut le révoquer.
                    match invite.uses_left {
                        None if !invite.revoked => {
                            ui.label(RichText::new("permanent").color(ACCENT).size(11.0));
                        }
                        Some(left) if !invite.revoked => {
                            ui.label(
                                RichText::new(format!("{left} restant(s)"))
                                    .color(TEXT_FAINT)
                                    .size(11.0),
                            );
                        }
                        _ => {
                            ui.label(RichText::new("révoqué").color(DANGER).size(11.0));
                        }
                    }
                    if invite.uses > 0 {
                        ui.label(
                            RichText::new(format!("· {} compte(s) créé(s)", invite.uses))
                                .color(TEXT_FAINT)
                                .size(11.0),
                        );
                    }
                    if !invite.label.is_empty() {
                        ui.label(
                            RichText::new(format!("· {}", invite.label))
                                .color(TEXT_FAINT)
                                .size(11.0),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !invite.revoked
                            && ui::icon_button_ex(ui, Icon::Ban, 24.0, "Révoquer", None).clicked()
                        {
                            to_send
                                .push(ClientMsg::AdminRevokeInvite { code: invite.code.clone() });
                        }
                        if ui::icon_button_ex(ui, Icon::Copy, 24.0, "Copier", None).clicked() {
                            ctx.copy_text(invite.code.clone());
                        }
                    });
                });
            }
        }
    }

    /// Onglet « Membres » : bannir, débannir, réinitialiser un mot de passe.
    fn admin_members_tab(&mut self, ui: &mut egui::Ui, to_send: &mut Vec<ClientMsg>) {
        ui::group_title(ui, Icon::User, "Comptes");

        let users = self.admin_users.clone();
        let my_name = users
            .iter()
            .find(|u| Some(u.user_id) == self.my_id)
            .map(|u| u.username.clone());
        for account in &users {
            ui.horizontal(|ui| {
                let photo = self.avatars.get(&account.user_id);
                ui::avatar(
                    ui,
                    &account.username,
                    26.0,
                    false,
                    photo.map(|(_, t)| t),
                    theme::BG_RAISED,
                );
                let dot = if account.online { SPEAK } else { theme::BORDER };
                ui::status_dot(ui, dot, "", 8.0);

                // La couleur du compte vient de son rôle le mieux classé,
                // comme partout ailleurs dans l'application.
                let account_color = theme::member_color(
                    self.roles
                        .iter()
                        .filter(|r| account.roles.contains(&r.id))
                        .max_by_key(|r| r.rank)
                        .and_then(|r| r.color),
                    &account.username,
                );
                let mut name =
                    RichText::new(&account.username).color(account_color).size(13.5);
                if account.banned {
                    name = name.strikethrough();
                }
                ui.label(name);
                if account.admin {
                    ui::glyph(ui, Icon::Crown, 13.0, ACCENT);
                }
                for role in self.roles.iter().filter(|r| account.roles.contains(&r.id)) {
                    ui.label(
                        RichText::new(&role.name)
                            .color(theme::member_color(role.color, &role.name))
                            .size(10.5),
                    );
                }
                if account.banned {
                    ui.label(RichText::new(ban_summary(account)).color(DANGER).size(11.0));
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Mêmes bornes que le serveur : la permission, et le
                    // rang strictement supérieur. Le bouton n'apparaît que
                    // s'il aboutirait.
                    if self.can(ki_protocol::perm::MANAGE_ROLES)
                        && self.outranks(account.rank)
                        && ui::icon_button_ex(ui, Icon::Crown, 26.0, "Rôles…", None).clicked()
                    {
                        self.roles_target = Some(account.username.clone());
                        self.roles_draft = account.roles.clone();
                    }
                    // Mêmes bornes que le serveur, ici aussi : la permission
                    // **et** le rang, et jamais sur soi-même. Sans elles, ces
                    // boutons s'affichaient à qui n'avait que « Expulser »,
                    // ouvraient une fenêtre, et le clic final se faisait
                    // refuser — ou, pour le bannissement de soi, aboutissait.
                    let myself = my_name.as_ref() == Some(&account.username);
                    let can_ban = self.can(ki_protocol::perm::BAN)
                        && self.outranks(account.rank)
                        && !myself;
                    if can_ban {
                        if account.banned {
                            if ui::icon_button_ex(ui, Icon::Check, 26.0, "Annuler le ban", None)
                                .clicked()
                            {
                                to_send.push(ClientMsg::AdminUnban {
                                    username: account.username.clone(),
                                });
                            }
                        } else if ui::icon_button_ex(ui, Icon::Ban, 26.0, "Bannir…", None)
                            .clicked()
                        {
                            self.ban_draft = Some(BanDraft {
                                username: account.username.clone(),
                                reason: String::new(),
                                duration_secs: 86_400,
                            });
                        }
                    }
                    // On règle toujours son propre mot de passe ; celui d'un
                    // autre demande la permission et le rang.
                    let can_reset = self.can(ki_protocol::perm::RESET_PASSWORD)
                        && (myself || self.outranks(account.rank));
                    if can_reset
                        && ui::icon_button_ex(
                            ui,
                            Icon::Key,
                            26.0,
                            "Réinitialiser le mot de passe",
                            None,
                        )
                        .clicked()
                    {
                        self.reset_target = Some(account.username.clone());
                        self.reset_password.clear();
                    }
                });
            });
            // Motif et auteur sous la ligne : c'est ce qu'on cherche quand
            // on revient sur un bannissement des semaines plus tard.
            if account.banned && !(account.ban_reason.is_empty() && account.ban_by.is_empty()) {
                ui.horizontal(|ui| {
                    ui.add_space(34.0);
                    let mut detail = String::new();
                    if !account.ban_reason.is_empty() {
                        detail.push_str(&safe_name(&account.ban_reason));
                    }
                    if !account.ban_by.is_empty() {
                        if !detail.is_empty() {
                            detail.push_str(" — ");
                        }
                        detail.push_str(&format!("par {}", safe_name(&account.ban_by)));
                    }
                    ui.label(RichText::new(detail).color(TEXT_FAINT).size(11.0));
                });
            }
        }

        // --- Attribution des rôles ---
        if let Some(target) = self.roles_target.clone() {
            ui.add_space(10.0);
            ui::hairline(ui);
            ui.add_space(8.0);
            ui::field_label(ui, &format!("Rôles de {target}"));
            let roles = self.roles.clone();
            let mine = self.my_rank;
            let mut assignable = 0;
            for role in &roles {
                // `@everyone` est implicite : il n'est jamais stocké sur un
                // compte, et le proposer laisserait croire qu'on peut le
                // retirer à quelqu'un.
                if role.id == ki_protocol::ROLE_EVERYONE {
                    continue;
                }
                // On n'attribue qu'un rôle strictement sous son propre rang :
                // sinon on se donnerait, par personne interposée, un pouvoir
                // qu'on n'a pas.
                if role.rank >= mine {
                    continue;
                }
                assignable += 1;
                let mut on = self.roles_draft.contains(&role.id);
                let label = RichText::new(&role.name)
                    .color(theme::member_color(role.color, &role.name));
                if ui.checkbox(&mut on, label).changed() {
                    if on {
                        self.roles_draft.push(role.id);
                    } else {
                        self.roles_draft.retain(|r| *r != role.id);
                    }
                }
            }
            if assignable == 0 {
                ui::hint(ui, "aucun rôle sous ton rang n'est attribuable");
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui::primary_button(ui, Some(Icon::Check), "Appliquer", None).clicked() {
                    to_send.push(ClientMsg::AdminSetUserRoles {
                        username: target.clone(),
                        roles: self.roles_draft.clone(),
                    });
                    self.roles_target = None;
                    self.roles_draft.clear();
                }
                if ui::button(ui, Icon::Close, "Annuler").clicked() {
                    self.roles_target = None;
                    self.roles_draft.clear();
                }
            });
        }

        // --- Formulaire de réinitialisation ---
        if let Some(target) = self.reset_target.clone() {
            ui.add_space(10.0);
            ui::hairline(ui);
            ui.add_space(8.0);
            ui::field_label(ui, &format!("Nouveau mot de passe pour {target}"));
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.reset_password)
                        .password(true)
                        .margin(egui::Margin::symmetric(10, 7))
                        .background_color(theme::BG_DEEP)
                        .desired_width(170.0),
                );
                let ok = self.reset_password.len() >= 6;
                let clicked = ui
                    .add_enabled_ui(ok, |ui| {
                        ui::primary_button(ui, Some(Icon::Check), "Appliquer", None)
                    })
                    .inner
                    .clicked();
                if clicked {
                    to_send.push(ClientMsg::AdminResetPassword {
                        username: target.clone(),
                        new_password: self.reset_password.clone(),
                    });
                    self.reset_target = None;
                    self.reset_password.clear();
                }
                if ui::button(ui, Icon::Close, "Annuler").clicked() {
                    self.reset_target = None;
                    self.reset_password.clear();
                }
            });
            if self.reset_password.len() < 6 && !self.reset_password.is_empty() {
                ui::hint(ui, "6 caractères minimum");
            }
        }
    }

    /// Onglet « Journal » : les actions d'administration, du plus récent au
    /// plus ancien. C'est ce qui rend un lien d'invitation permanent
    /// acceptable — on sait toujours qui est entré par où.
    fn admin_audit_tab(&mut self, ui: &mut egui::Ui) {
        if self.audit.is_empty() {
            ui::hint(ui, "aucune action consignée pour l'instant");
            return;
        }
        let records = self.audit.clone();
        for record in &records {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("{} {}", day_label(record.ts), format_time(record.ts)))
                        .color(TEXT_FAINT)
                        .monospace()
                        .size(11.0),
                );
                ui.label(
                    RichText::new(audit_label(&record.action))
                        .color(audit_tone(&record.action))
                        .size(12.5)
                        .strong(),
                );
                let actor = if record.actor.is_empty() {
                    "le serveur".to_string()
                } else {
                    safe_name(&record.actor)
                };
                ui.label(RichText::new(format!("par {actor}")).color(TEXT_DIM).size(12.0));
                if !record.target.is_empty() {
                    ui.label(
                        RichText::new(format!("→ {}", safe_name(&record.target)))
                            .color(TEXT_DIM)
                            .size(12.0),
                    );
                }
                if !record.detail.is_empty() {
                    ui.label(
                        RichText::new(ki_protocol::safe_display(&record.detail, 200))
                            .color(TEXT_FAINT)
                            .size(11.5),
                    );
                }
            });
            ui.add_space(2.0);
        }
    }

    fn close_admin(&mut self) {
        self.show_admin = false;
        self.info = None;
        self.last_invite = None;
        self.admin_icon = IconChange::Keep;
        self.preview_icons.remove(&Apercu::LogoServeur);
    }
}

/// Noms des sons reconnus. Chaque nom correspond au nom d'un fichier `.wav`
/// (sans l'extension) déposé dans le dossier des sons.
mod sfx {
    pub const SELF_JOIN: &str = "rejoint-vocal";
    pub const SELF_LEAVE: &str = "quitte-vocal";
    pub const PEER_JOIN: &str = "arrivee";
    pub const PEER_LEAVE: &str = "depart";
    pub const MESSAGE: &str = "message";
    pub const MUTE: &str = "micro-coupe";
    pub const UNMUTE: &str = "micro-actif";
}

/// Où l'on cherche les sons, dans l'ordre : un dossier `sons/` à côté de
/// l'exécutable, puis celui des réglages.
///
/// Rien n'est embarqué dans le binaire : les fichiers audio du dépôt sont
/// des œuvres tierces, et le dépôt est public. Chacun dépose donc les siens.
fn sound_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.join("sons"));
        }
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        dirs.push(std::path::PathBuf::from(appdata).join("ki-chat").join("sons"));
    }
    dirs
}

fn load_sounds() -> HashMap<String, Vec<f32>> {
    let mut found = HashMap::new();
    for dir in sound_dirs() {
        for (name, pcm) in ki_voice::effects::load_dir(&dir) {
            // Le premier dossier gagne : celui à côté de l'exécutable
            // l'emporte sur celui des réglages.
            found.entry(name).or_insert(pcm);
        }
    }
    if !found.is_empty() {
        tracing::info!("{} effet(s) sonore(s) chargé(s)", found.len());
    }
    found
}

/// Assainit un message reçu avant affichage : pseudo et texte viennent
/// d'autrui et passent par les mêmes règles que partout ailleurs.
fn clean_record(mut record: ChatRecord) -> ChatRecord {
    record.username = safe_name(&record.username);
    record.text = ki_protocol::safe_display(&record.text, ki_protocol::MAX_CHAT_TEXT);
    record
}

/// Étiquette courte d'un bannissement : « banni » ou le temps restant.
fn ban_summary(account: &AccountInfo) -> String {
    let Some(until) = account.ban_until else { return "banni".into() };
    let now = chrono::Local::now().timestamp_millis().max(0) as u64;
    let minutes = until.saturating_sub(now) / 60_000;
    match minutes {
        0 => "banni — expire".into(),
        1..=90 => format!("banni — {minutes} min"),
        91..=2879 => format!("banni — {} h", minutes / 60),
        _ => format!("banni — {} j", minutes / 1440),
    }
}

/// Traduit un verbe du journal d'audit. Les verbes ne sont jamais traduits
/// côté serveur — le fichier doit rester lisible et « greppable » — donc la
/// traduction se fait ici. Un verbe inconnu (serveur plus récent que le
/// client) s'affiche tel quel plutôt que de disparaître.
fn audit_label(action: &str) -> String {
    match action {
        "invite.create" => "invitation créée".into(),
        "invite.use" => "compte créé".into(),
        "invite.revoke" => "invitation révoquée".into(),
        "member.kick" => "expulsion".into(),
        "member.ban" => "bannissement".into(),
        "member.unban" => "ban annulé".into(),
        "member.password_reset" => "mot de passe réinitialisé".into(),
        "server.info" => "identité du serveur".into(),
        other => other.to_string(),
    }
}

/// Couleur d'un verbe : rouge pour ce qui sanctionne, accent pour ce qui
/// ouvre un accès. Le journal se parcourt à l'œil avant de se lire.
fn audit_tone(action: &str) -> egui::Color32 {
    match action {
        "member.kick" | "member.ban" => DANGER,
        "invite.create" | "invite.use" => ACCENT,
        _ => TEXT_DIM,
    }
}

// ---------------------------------------------------------------------
// Widgets propres à l'écran principal
// ---------------------------------------------------------------------

/// Logo + nom + accroche, centrés au pixel près, posés sur un halo.
fn brand(ui: &mut egui::Ui) {
    let galley = ui.fonts(|f| {
        f.layout_no_wrap("ki-chat".into(), egui::FontId::proportional(38.0), ACCENT)
    });
    let mark = 56.0;
    let total = mark + 16.0 + galley.size().x;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), mark), Sense::hover());
    let left = rect.center().x - total / 2.0;

    // Halo derrière la marque. Dessiné avant tout le reste, il reste sous
    // la carte qui sera posée par-dessus.
    ui::glow(
        ui.painter(),
        rect.center(),
        300.0,
        theme::alpha(ACCENT, 30),
    );

    icons::logo(
        ui.painter(),
        egui::Rect::from_min_size(
            egui::pos2(left, rect.center().y - mark / 2.0),
            Vec2::splat(mark),
        ),
        ACCENT,
        theme::BG_DEEP,
    );
    let baseline = rect.center().y - galley.size().y / 2.0;
    ui.painter()
        .galley(egui::pos2(left + mark + 15.0, baseline), galley, ACCENT);

    ui.add_space(6.0);
    ui.label(
        RichText::new("chat & vocal basse latence — 100 % Rust")
            .color(TEXT_FAINT)
            .size(13.0),
    );
}

/// Ce qu'un clic sur une ligne de serveur a déclenché.
enum RowAction {
    None,
    Select,
    Edit,
}

/// Ligne du carnet de serveurs : nom, adresse, et état mesuré à l'avance.
fn server_row(
    ui: &mut egui::Ui,
    server: &servers::Server,
    selected: bool,
    reach: servers::Reach,
    icon: Option<&egui::TextureHandle>,
) -> RowAction {
    const HEIGHT: f32 = 52.0;
    const EDIT: f32 = 24.0;
    const BADGE: f32 = 32.0;

    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), HEIGHT), Sense::click());

    // Le crayon est posé avec `interact` et peint à la main, pas via un
    // sous-Ui : `scope_builder` recale le curseur du parent sur le rectangle
    // de l'enfant, et comme le crayon est plus court que la ligne, cela
    // rognait l'espacement de la ligne suivante — l'écart changeait donc
    // selon la ligne sélectionnée. `interact` ne touche pas à la mise en page.
    let edit_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - EDIT - 8.0, rect.center().y - EDIT / 2.0),
        Vec2::splat(EDIT),
    );
    let edit = ui
        .interact(edit_rect, response.id.with("edit"), Sense::click())
        .on_hover_text("Modifier ou supprimer");

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let bg = if selected {
            theme::mix(theme::BG_DEEP, ACCENT, 0.12)
        } else if response.hovered() {
            theme::mix(theme::BG_DEEP, theme::BG_HOVER, 0.75)
        } else {
            theme::BG_DEEP
        };
        painter.rect(
            rect,
            egui::CornerRadius::same(10),
            bg,
            egui::Stroke::new(1.0_f32, if selected { ACCENT } else { theme::BORDER_SOFT }),
            egui::StrokeKind::Inside,
        );
        if selected {
            let bar = egui::Rect::from_min_size(
                egui::pos2(rect.left() + 4.0, rect.center().y - 11.0),
                Vec2::new(3.0, 22.0),
            );
            painter.rect_filled(bar, egui::CornerRadius::same(2), ACCENT);
        }

        let badge = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 11.0, rect.center().y - BADGE / 2.0),
            Vec2::splat(BADGE),
        );
        ui::paint_server_badge(painter, badge, &server.name, server.address.trim(), icon);

        // Sans nom donné, l'adresse tient lieu de titre : inutile de la
        // répéter en dessous.
        let text_left = badge.right() + 11.0;
        let named = server.has_label();
        let title_color = if selected { ACCENT } else { TEXT };
        if named {
            painter.text(
                egui::pos2(text_left, rect.center().y - 8.0),
                egui::Align2::LEFT_CENTER,
                server.label(),
                egui::FontId::proportional(13.5),
                title_color,
            );
            let mut address = server.address.trim().to_string();
            if let Some(tag) = address_tag(&address) {
                address.push_str(" · ");
                address.push_str(tag);
            }
            painter.text(
                egui::pos2(text_left, rect.center().y + 9.0),
                egui::Align2::LEFT_CENTER,
                address,
                egui::FontId::proportional(11.0),
                TEXT_FAINT,
            );
        } else {
            painter.text(
                egui::pos2(text_left, rect.center().y),
                egui::Align2::LEFT_CENTER,
                server.address.trim(),
                egui::FontId::proportional(13.5),
                title_color,
            );
        }

        // Qualité : des barres de réseau se lisent d'un coup d'œil, bien
        // mieux qu'un rond de couleur, et la valeur reste lisible à côté.
        // Chasse fixe pour les valeurs : les millisecondes s'alignent d'une
        // ligne à l'autre. Les états en toutes lettres restent en romain.
        let (text, color, bars, numeric) = match reach {
            servers::Reach::Online { ping_ms } => {
                let (bars, color) = quality(ping_ms);
                (format!("{ping_ms} ms"), color, Some(bars), true)
            }
            servers::Reach::Offline => ("injoignable".to_string(), DANGER, Some(0), false),
            servers::Reach::Probing => ("test…".to_string(), TEXT_FAINT, None, false),
            servers::Reach::Unknown => ("non testé".to_string(), TEXT_FAINT, None, false),
        };
        let font = if numeric {
            egui::FontId::monospace(11.5)
        } else {
            egui::FontId::proportional(12.0)
        };
        let galley = ui.fonts(|f| f.layout_no_wrap(text, font, color));
        let value_left = rect.right() - EDIT - 14.0 - galley.size().x;
        let baseline = rect.center().y - galley.size().y / 2.0;
        painter.galley(egui::pos2(value_left, baseline), galley, color);

        let marker = egui::Rect::from_center_size(
            egui::pos2(value_left - 15.0, rect.center().y),
            Vec2::splat(14.0),
        );
        match bars {
            Some(lit) => icons::signal(painter, marker, lit, color, theme::BORDER),
            None => ui::spinner(painter, marker.center(), 5.0, ui.input(|i| i.time), color),
        }

        // Le crayon ne se montre qu'au survol ou sur la ligne courante : la
        // liste reste calme. Sa place est réservée en permanence, pour que le
        // ping ne se décale pas quand la souris passe.
        if response.hovered() || edit.hovered() || selected {
            if edit.hovered() {
                let bg = if edit.is_pointer_button_down_on() {
                    theme::BG_ACTIVE
                } else {
                    theme::BG_HOVER
                };
                painter.rect_filled(edit_rect, egui::CornerRadius::same(7), bg);
            }
            let fg = if edit.hovered() { TEXT } else { TEXT_FAINT };
            icons::draw(painter, edit_rect.shrink(EDIT * 0.24), Icon::Pencil, fg);
        }
    }

    // Le crayon est au-dessus : il prend le clic avant la ligne.
    if edit.clicked() {
        RowAction::Edit
    } else if response.clicked() {
        RowAction::Select
    } else {
        RowAction::None
    }
}

/// Nombre de barres allumées et couleur, pour un ping donné.
fn quality(ping_ms: u32) -> (u8, Color32) {
    match ping_ms {
        0..=29 => (4, SPEAK),
        30..=79 => (3, ACCENT),
        80..=149 => (2, WARN),
        _ => (1, TEXT_DIM),
    }
}

/// Étiquette courte qui situe une adresse, ou `None` si elle est publique.
fn address_tag(address: &str) -> Option<&'static str> {
    let host = address.trim().rsplit_once(':').map_or(address.trim(), |(h, _)| h);
    if matches!(host, "localhost" | "127.0.0.1" | "::1") {
        return Some("local");
    }
    let mut parts = host.split('.').map(str::parse::<u8>);
    let (Some(Ok(a)), Some(Ok(b)), Some(Ok(_)), Some(Ok(_)), None) =
        (parts.next(), parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    // Plages privées RFC 1918.
    let private = a == 10 || (a == 192 && b == 168) || (a == 172 && (16..=31).contains(&b));
    private.then_some("réseau local")
}

/// Ligne de salon : pastille pleine largeur, filet d'accent si sélectionnée.
fn channel_row(
    ui: &mut egui::Ui,
    name: &str,
    selected: bool,
    kind: ChannelKind,
) -> egui::Response {
    let height = 34.0;
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let bg = if selected {
            theme::alpha(ACCENT, 26)
        } else if response.hovered() {
            theme::BG_HOVER
        } else {
            Color32::TRANSPARENT
        };
        if bg != Color32::TRANSPARENT {
            painter.rect_filled(rect, egui::CornerRadius::same(9), bg);
        }
        if selected {
            let bar = egui::Rect::from_min_size(
                egui::pos2(rect.left() + 2.0, rect.center().y - 8.0),
                Vec2::new(3.0, 16.0),
            );
            painter.rect_filled(bar, egui::CornerRadius::same(2), ACCENT);
        }
        let fg = if selected {
            ACCENT
        } else if response.hovered() {
            TEXT
        } else {
            TEXT_DIM
        };
        let icon = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 12.0, rect.center().y - 8.0),
            Vec2::splat(16.0),
        );
        // Le pictogramme dit la nature du salon : on lit un « # », on parle
        // dans un haut-parleur.
        let symbol = match kind {
            ChannelKind::Text => Icon::Hash,
            ChannelKind::Voice => Icon::Volume,
        };
        icons::draw(painter, icon, symbol, fg);
        painter.text(
            egui::pos2(rect.left() + 34.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            name,
            egui::FontId::proportional(14.0),
            fg,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Les deux vignettes dont on peut afficher un aperçu, et qui peuvent l'être
/// **en même temps** : la photo de profil dans « Mon compte », le logo du
/// serveur dans Admin ▸ Serveur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Apercu {
    Avatar,
    LogoServeur,
}

/// Ce qu'une ligne de membre montre en plus du compte lui-même : l'état
/// « ici et maintenant », qui ne vient pas du roster mais du moteur audio et
/// des réglages locaux. Groupé, parce que huit paramètres positionnels
/// donnaient des appels du genre `(m, false, false, false, 0.0, 1.0)`, où
/// rien ne dit lequel est quoi.
struct MemberRow<'a> {
    member: &'a Member,
    speaking: bool,
    muted: bool,
    is_me: bool,
    /// Niveau instantané, pour le vumètre (0..1).
    level: f32,
    /// Volume personnalisé appliqué à cette personne (1.0 = 100 %).
    volume: f32,
    photo: Option<&'a egui::TextureHandle>,
}

impl<'a> MemberRow<'a> {
    /// Quelqu'un qui n'est pas là : rien à dire de son micro ni de son
    /// niveau, seul le compte et sa photo subsistent.
    fn offline(member: &'a Member, photo: Option<&'a egui::TextureHandle>) -> Self {
        Self {
            member,
            speaking: false,
            muted: false,
            is_me: false,
            level: 0.0,
            volume: 1.0,
            photo,
        }
    }
}

/// Ligne de membre : avatar, pseudo, badges, vumètre pendant qu'il parle.
fn member_row(ui: &mut egui::Ui, row: MemberRow<'_>) -> egui::Response {
    let MemberRow { member, speaking, muted, is_me, level, volume, photo } = row;
    let height = 38.0;
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect, egui::CornerRadius::same(9), theme::BG_HOVER);
    }

    let avatar_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 14.0, rect.center().y - 13.0),
        Vec2::splat(26.0),
    );
    ui::paint_avatar(painter, avatar_rect, &member.username, speaking, photo, theme::BG_SIDE);

    // La couleur vient du rôle quand le serveur en donne une ; sinon
    // c'est le hachage du pseudo, comme avant les rôles. Hors ligne, la
    // couleur s'éteint : présent dans la liste, absent de la pièce.
    let color = if member.online {
        theme::member_color(member.color, &member.username)
    } else {
        theme::member_color(member.color, &member.username).gamma_multiply(0.45)
    };
    let font = egui::FontId::proportional(13.5);
    let galley = ui.fonts(|f| f.layout_no_wrap(member.username.clone(), font, color));
    let name_width = galley.size().x;
    let name_left = avatar_rect.right() + 9.0;
    painter.galley(
        egui::pos2(name_left, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );

    if member.admin {
        let badge = egui::Rect::from_min_size(
            egui::pos2(name_left + name_width + 5.0, rect.center().y - 6.5),
            Vec2::splat(13.0),
        );
        icons::draw(painter, badge, Icon::Crown, ACCENT);
    }

    // Côté droit : micro coupé, volume personnalisé, vumètre pendant la
    // parole. L'icône « muet » d'abord, la plus à droite : c'est elle qui
    // répond à « il est parti ou il s'est mute ? » d'un coup d'œil.
    let mut right = rect.right() - 10.0;

    // Les sanctions d'abord, en ambre. Même forme que l'état volontaire,
    // couleur différente : c'est ce qui sépare « il s'est tu » de « on l'a
    // fait taire », et les deux peuvent tenir en même temps sur la même
    // personne. Un rouge de plus n'aurait rien distingué du tout.
    for (actif, icone) in [
        (member.force_deafened, Icon::HeadphonesOff),
        (member.force_muted, Icon::MicOff),
    ] {
        if actif {
            let badge = egui::Rect::from_min_size(
                egui::pos2(right - 14.0, rect.center().y - 7.0),
                Vec2::splat(14.0),
            );
            icons::draw(painter, badge, icone, WARN);
            right -= 20.0;
        }
    }

    if muted {
        let badge = egui::Rect::from_min_size(
            egui::pos2(right - 14.0, rect.center().y - 7.0),
            Vec2::splat(14.0),
        );
        icons::draw(painter, badge, Icon::MicOff, DANGER);
        right -= 20.0;
    }
    if speaking {
        let meter = egui::Rect::from_min_size(
            egui::pos2(right - 34.0, rect.center().y - 3.0),
            Vec2::new(34.0, 6.0),
        );
        ui::paint_meter(painter, meter, (level * 3.0).min(1.0), SPEAK);
        right -= 40.0;
    }
    if !is_me && (volume - 1.0).abs() > 0.001 {
        painter.text(
            egui::pos2(right, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            format!("{:.0} %", volume * 100.0),
            egui::FontId::proportional(10.5),
            TEXT_FAINT,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Séparateur de journée dans la conversation.
fn day_separator(ui: &mut egui::Ui, label: &str) {
    ui.add_space(14.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 16.0), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let galley = ui.fonts(|f| {
        f.layout_no_wrap(
            label.to_owned(),
            egui::FontId::proportional(11.0),
            TEXT_FAINT,
        )
    });
    let painter = ui.painter();
    let half = galley.size().x / 2.0;
    let y = rect.center().y;
    let stroke = egui::Stroke::new(1.0_f32, theme::BORDER_SOFT);
    painter.line_segment(
        [
            egui::pos2(rect.left() + 16.0, y),
            egui::pos2(rect.center().x - half - 10.0, y),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(rect.center().x + half + 10.0, y),
            egui::pos2(rect.right() - 16.0, y),
        ],
        stroke,
    );
    painter.galley(
        egui::pos2(rect.center().x - half, y - galley.size().y / 2.0),
        galley,
        TEXT_FAINT,
    );
    ui.add_space(6.0);
}

/// Un message : en-tête (avatar + pseudo + heure) si c'est le premier du
/// groupe, puis le corps. Les messages consécutifs du même auteur se
/// collent, comme sur Discord.
/// Ce qu'il faut pour peindre un message, en plus du message lui-même.
///
/// Groupé plutôt qu'énuméré : à sept paramètres positionnels, on ne sait plus
/// lequel est lequel — et il en faut deux de plus depuis les mentions.
struct MessageRow<'a> {
    msg: &'a ChatRecord,
    with_header: bool,
    photo: Option<&'a egui::TextureHandle>,
    /// Couleur de l'auteur, résolue depuis son rôle par l'appelant. Elle
    /// n'est pas figée dans l'historique : changer un rôle doit recolorer les
    /// anciens messages, pas seulement les nouveaux.
    color: egui::Color32,
    /// Pseudos connus, pour reconnaître les mentions. Sans eux, `@` n'importe
    /// quoi passerait pour une mention.
    membres: &'a [&'a str],
    /// Le pseudo de celui qui lit, pour distinguer sa propre mention.
    moi: Option<&'a str>,
}

fn message_block(
    ui: &mut egui::Ui,
    row: MessageRow<'_>,
    previews: &mut images::Previews,
) {
    let MessageRow { msg, with_header, photo, color, membres, moi } = row;
    const GUTTER: f32 = 36.0;
    let bg_slot = ui.painter().add(egui::Shape::Noop);

    let inner = ui.vertical(|ui| {
        ui.add_space(if with_header { 7.0 } else { 1.0 });
        ui.horizontal_top(|ui| {
            ui.add_space(14.0);
            if with_header {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(GUTTER), Sense::hover());
                ui::paint_avatar(ui.painter(), rect, &msg.username, false, photo, theme::BG_BASE);
            } else {
                ui.allocate_exact_size(Vec2::new(GUTTER, 1.0), Sense::hover());
            }
            ui.add_space(11.0);
            ui.vertical(|ui| {
                if with_header {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 7.0;
                        ui.label(
                            RichText::new(&msg.username)
                                .color(color)
                                .size(14.0)
                                .strong(),
                        );
                        ui.label(
                            RichText::new(format_time(msg.ts))
                                .color(TEXT_FAINT)
                                .size(11.0),
                        );
                    });
                    ui.add_space(1.0);
                }
                message_body(ui, &msg.text, membres, moi, (msg.user_id, msg.ts));
                // Les images partagées s'affichent sous le message.
                for (is_link, chunk) in split_links(&msg.text) {
                    if is_link && images::looks_like_image(chunk) {
                        image_preview(ui, chunk, previews);
                    }
                }
            });
        });
        ui.add_space(2.0);
    });

    let row = egui::Rect::from_min_max(
        egui::pos2(ui.max_rect().left() + 6.0, inner.response.rect.top()),
        egui::pos2(ui.max_rect().right() - 6.0, inner.response.rect.bottom()),
    );
    if ui.rect_contains_pointer(row) {
        ui.painter().set(
            bg_slot,
            egui::epaint::RectShape::filled(row, egui::CornerRadius::same(8), theme::BG_GHOST),
        );
    }
}

/// Corps d'un message : texte simple, ou texte + liens cliquables.
/// Vignette d'une image partagée, cliquable pour l'ouvrir en grand.
fn image_preview(ui: &mut egui::Ui, url: &str, previews: &mut images::Previews) {
    const MAX_W: f32 = 420.0;
    const MAX_H: f32 = 320.0;

    let Some(state) = previews.get(ui.ctx(), url) else { return };
    ui.add_space(6.0);
    match state {
        images::Preview::Loading => {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(180.0, 24.0), Sense::hover());
            let center = egui::pos2(rect.left() + 9.0, rect.center().y);
            ui::spinner(ui.painter(), center, 6.0, ui.input(|i| i.time), TEXT_FAINT);
            ui.painter().text(
                egui::pos2(rect.left() + 24.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                "chargement de l'image…",
                egui::FontId::proportional(12.0),
                TEXT_FAINT,
            );
        }
        images::Preview::Failed => {
            ui::hint(ui, "image illisible");
        }
        images::Preview::Ready(texture) => {
            // On respecte les proportions, sans jamais dépasser le cadre.
            let source = texture.size_vec2();
            let scale = (MAX_W / source.x).min(MAX_H / source.y).min(1.0);
            let size = source * scale;
            let (rect, response) = ui.allocate_exact_size(size, Sense::click());
            if ui.is_rect_visible(rect) {
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                ui.painter().image(texture.id(), rect, uv, Color32::WHITE);
                if response.hovered() {
                    ui.painter().rect_stroke(
                        rect,
                        egui::CornerRadius::same(6),
                        egui::Stroke::new(1.0_f32, theme::alpha(ACCENT, 160)),
                        egui::StrokeKind::Inside,
                    );
                }
            }
            let response = response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text("Ouvrir l'image");
            if response.clicked() {
                // Le port du partage n'écoute plus qu'en TLS : ouvrir un
                // ancien lien en clair donnerait « connexion réinitialisée »
                // dans le navigateur, sans que rien n'explique pourquoi.
                ui.ctx().open_url(egui::OpenUrl::new_tab(
                    previews.pinned_url(url).unwrap_or_else(|| url.to_string()),
                ));
            }
        }
    }
}

/// Le sélecteur « réservé à certains rôles » d'un salon.
///
/// Partagé par la création et la modification : deux copies divergeraient, et
/// c'est exactement le genre d'écart qui laisse un salon visible de tous alors
/// que le formulaire affichait le contraire.
fn restriction_par_roles(ui: &mut egui::Ui, roles: &[ki_protocol::RoleInfo], draft: &mut ChannelDraft) {
    ui.checkbox(&mut draft.restricted, "Réservé à certains rôles");
    if !draft.restricted {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for role in roles {
            // `@everyone` n'a pas de sens dans une restriction : le cocher
            // reviendrait à ne rien restreindre du tout.
            if role.id == ki_protocol::ROLE_EVERYONE {
                continue;
            }
            let mut on = draft.allowed_roles.contains(&role.id);
            if ui.checkbox(&mut on, &role.name).changed() {
                if on {
                    draft.allowed_roles.push(role.id);
                } else {
                    draft.allowed_roles.retain(|r| *r != role.id);
                }
            }
        }
    });
    if draft.allowed_roles.is_empty() {
        ui::hint(ui, "aucun rôle coché : le salon resterait invisible pour tous");
    }
}

/// Peint le corps d'un message : liens, mentions, gras, italique, code.
///
/// Le découpage vit dans `markup`, qui se teste sans ouvrir de fenêtre ; ici,
/// on ne fait que peindre ce qu'il rend.
///
/// `membres` sert à reconnaître les mentions — `@` suivi d'un pseudo **connu**
/// — et `moi` à distinguer la sienne. Sans la liste, une adresse électronique
/// écrite dans un message deviendrait un surlignage.
fn message_body(
    ui: &mut egui::Ui,
    text: &str,
    membres: &[&str],
    moi: Option<&str>,
    // De quoi identifier le message d'une image a l'autre. Un bloc de code
    // defile horizontalement, et egui range ce defilement sous une cle : si
    // elle change a chaque image, le bloc revient au debut des qu'on scrute
    // autre chose. L'auteur et l'horodatage, eux, ne bougent pas.
    cle: (UserId, u64),
) {
    let blocs = markup::decouper(text, membres, moi);

    // Le cas de loin le plus fréquent : une ligne, aucun balisage. On évite
    // alors toute la machinerie de mise en page horizontale, qui coûte un
    // widget par fragment.
    if let [markup::Bloc::Ligne(frags)] = blocs.as_slice() {
        if let [markup::Fragment::Texte(t)] = frags.as_slice() {
            ui.label(RichText::new(*t).color(TEXT).size(14.0));
            return;
        }
    }

    ui.scope(|ui| {
        // Espacement nul : les espaces font partie des fragments de texte,
        // le retour à la ligne tombe donc au bon endroit.
        ui.spacing_mut().item_spacing.x = 0.0;
        for (i, bloc) in blocs.iter().enumerate() {
            match bloc {
                markup::Bloc::Code(code) => bloc_de_code(ui, code, (cle, i)),
                markup::Bloc::Ligne(frags) => {
                    ui.horizontal_wrapped(|ui| {
                        for frag in frags {
                            peindre_fragment(ui, frag);
                        }
                    });
                }
            }
        }
    });
}

fn peindre_fragment(ui: &mut egui::Ui, frag: &markup::Fragment<'_>) {
    use markup::Fragment as F;
    match frag {
        F::Texte(t) => {
            ui.label(RichText::new(*t).color(TEXT).size(14.0));
        }
        F::Lien(url) => {
            ui.hyperlink_to(RichText::new(shorten(url)).size(14.0), *url)
                .on_hover_text(*url);
        }
        F::Gras(t) => {
            ui.label(RichText::new(*t).color(TEXT).size(14.0).strong());
        }
        F::Italique(t) => {
            ui.label(RichText::new(*t).color(TEXT).size(14.0).italics());
        }
        F::Code(t) => {
            // Fond discret : ce qui distingue du code d'une phrase, c'est
            // la chasse fixe et un léger relief, pas une couleur criarde.
            ui.label(
                RichText::new(*t)
                    .monospace()
                    .size(13.0)
                    .color(TEXT)
                    .background_color(theme::BG_RAISED),
            );
        }
        F::Mention { pseudo, moi } => {
            // Sa propre mention attire l'œil ; celle de quelqu'un d'autre
            // se contente d'être reconnaissable. Sans cette distinction, un
            // salon actif devient un sapin de Noël et l'on cesse de voir
            // celles qui nous concernent.
            let (couleur, fond) = if *moi {
                (theme::BG_DEEP, ACCENT)
            } else {
                (ACCENT, theme::BG_RAISED)
            };
            ui.label(
                RichText::new(format!("@{pseudo}"))
                    .size(14.0)
                    .color(couleur)
                    .background_color(fond),
            );
        }
    }
}

/// Un bloc de code : chasse fixe, fond propre, et **pas** de retour à la
/// ligne automatique — un extrait de code coupé au milieu ne se lit plus. Il
/// défile horizontalement dans son cadre.
fn bloc_de_code(ui: &mut egui::Ui, code: &str, cle: ((UserId, u64), usize)) {
    ui.add_space(3.0);
    egui::Frame::NONE
        .fill(theme::BG_RAISED)
        .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            egui::ScrollArea::horizontal()
                .id_salt(("bloc-code", cle))
                .max_height(260.0)
                .show(ui, |ui| {
                    ui.label(RichText::new(code).monospace().size(13.0).color(TEXT));
                });
        });
    ui.add_space(3.0);
}

/// Salon vide : on n'affiche pas une page blanche.
fn empty_state(ui: &mut egui::Ui, channel: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.32).max(40.0));
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(48.0), Sense::hover());
        icons::draw(ui.painter(), rect, Icon::Chat, theme::BORDER);
        ui.add_space(10.0);
        ui.label(
            RichText::new(format!("Personne n'a encore parlé dans #{channel}"))
                .color(TEXT_DIM)
                .size(14.0),
        );
        ui.add_space(3.0);
        ui.label(
            RichText::new("Lance la conversation.")
                .color(TEXT_FAINT)
                .size(12.5),
        );
    });
}

// ---------------------------------------------------------------------
// Utilitaires
// ---------------------------------------------------------------------

fn format_time(ts_millis: u64) -> String {
    chrono::Local
        .timestamp_millis_opt(ts_millis as i64)
        .single()
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_default()
}

/// Heure avec les secondes — le journal audio se joue à la seconde près.
fn format_time_secs(ts_millis: u64) -> String {
    chrono::Local
        .timestamp_millis_opt(ts_millis as i64)
        .single()
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_default()
}

/// Numéro de jour absolu — sert à détecter le changement de date.
fn day_key(ts_millis: u64) -> i32 {
    chrono::Local
        .timestamp_millis_opt(ts_millis as i64)
        .single()
        .map(|t| t.date_naive().num_days_from_ce())
        .unwrap_or(i32::MIN)
}

/// « Aujourd'hui », « Hier », ou la date — sans dépendre de la locale.
fn day_label(ts_millis: u64) -> String {
    let stamp = chrono::Local.timestamp_millis_opt(ts_millis as i64).single();
    let Some(dt) = stamp else { return String::new() };
    let today = chrono::Local::now().date_naive();
    let date = dt.date_naive();
    if date == today {
        "Aujourd'hui".into()
    } else if Some(date) == today.pred_opt() {
        "Hier".into()
    } else {
        date.format("%d/%m/%Y").to_string()
    }
}

/// Découpe un message en fragments `(est_un_lien, texte)`. Les espaces
/// restent dans les fragments de texte pour que le retour à la ligne
/// automatique se fasse au bon endroit.
fn split_links(text: &str) -> Vec<(bool, &str)> {
    let mut parts = Vec::new();
    let (mut cursor, mut start) = (0usize, 0usize);
    while cursor < text.len() {
        let is_url = text.is_char_boundary(cursor)
            && (text[cursor..].starts_with("http://") || text[cursor..].starts_with("https://"));
        if is_url {
            if cursor > start {
                parts.push((false, &text[start..cursor]));
            }
            let end = text[cursor..]
                .find(char::is_whitespace)
                .map_or(text.len(), |offset| cursor + offset);
            parts.push((true, &text[cursor..end]));
            cursor = end;
            start = end;
        } else {
            cursor += 1;
        }
    }
    if start < text.len() || parts.is_empty() {
        parts.push((false, &text[start..]));
    }
    parts
}

/// Pseudo affichable : sans caractères dangereux et de longueur bornée.
fn safe_name(username: &str) -> String {
    ki_protocol::safe_display(username, ki_protocol::MAX_USERNAME)
}

/// Identité de serveur affichable. Le nom est borné côté serveur, mais rien
/// n'oblige le serveur d'en face à être le nôtre.
fn safe_server_info(mut server: ServerInfo) -> ServerInfo {
    server.name = ki_protocol::safe_display(&server.name, ki_protocol::MAX_SERVER_NAME);
    server
}

/// Tronque un libellé trop long, en comptant en caractères et non en octets.
fn ellipsize(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// Raccourcit une URL trop longue pour ne pas noyer le message.
fn shorten(url: &str) -> String {
    let len = url.chars().count();
    if len <= 70 {
        return url.to_string();
    }
    let head: String = url.chars().take(46).collect();
    let tail: String = url.chars().skip(len - 16).collect();
    format!("{head}…{tail}")
}

/// Compte abrégé : 1234 → « 1.2k ».
fn compact(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

/// Octets en méga-octets, pour l'affichage.
fn megabytes(bytes: u64) -> f32 {
    bytes as f32 / (1024.0 * 1024.0)
}

impl eframe::App for KiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // La mesure encadre TOUT le corps de `update`, sinon elle mentirait
        // par omission — c'est le coût complet d'une image qu'on cherche, pas
        // celui de la partie qu'on a pensé à instrumenter.
        self.perf.debut_image();

        // Géométrie : on ne restaure que « maximisée », et on suit l'état
        // courant pour le réenregistrer. Cf. `main` pour le pourquoi.
        if self.restore_maximized {
            self.restore_maximized = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
        }
        let was_maximized = self.maximized;
        self.maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(was_maximized));

        // Focus : conditionne le son des messages et la notification.
        self.window_focused = ctx.input(|i| i.focused);

        // Diagnostic partagé : si l'option est cochée, le journal technique
        // part vers le serveur à son rythme (une minute, et que du neuf).
        self.maybe_flush_diag();

        // La surveillance du clavier démarre à la première image : elle a
        // besoin du contexte pour réveiller la fenêtre, et lui seul sait
        // quand il existe.
        let ptt = self.ptt.get_or_insert_with(|| ptt::Watcher::start(ctx.clone()));
        // Deux écritures atomiques, à chaque image : le fil suit les réglages
        // à chaud sans qu'on ait à le redémarrer. Hors push-to-talk il ne lit
        // même pas le clavier.
        ptt.watch((self.mode == MicMode::Ptt).then_some(self.ptt_key));
        ptt.set_release_ms(self.ptt_release_ms);

        // En vocal, la machine ne doit pas s'endormir : parler ne compte pas
        // comme de l'activité pour Windows, et un portable dont on ne touche
        // pas le clavier se mettait en veille en pleine conversation.
        self.veille.actualiser(self.voice_channel.is_some());

        self.poll_events();
        self.check_connect_timeout();
        // La reprise se déclenche depuis le rendu, comme la sonde des
        // serveurs : c'est sans risque ici, l'écran de connexion se repeint
        // au moins une fois par seconde de toute façon (voir
        // `repaint_delay`). Ce serait faux ailleurs — une horloge que
        // personne ne fait tourner ne sonne jamais.
        self.tick_reprise(ctx);
        self.update_voice();
        // Un seul instantané par image, pris ici : l'écran principal l'affiche,
        // et c'est lui qui dit s'il faut une image de plus.
        let voice = self.voice_snapshot();

        // Un message est arrivé pendant que la fenêtre était à l'arrière-plan :
        // la barre des tâches clignote (l'équivalent sobre d'une notification).
        if self.wants_attention {
            self.wants_attention = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Informational,
            ));
        }

        if self.welcomed {
            // Échap ferme la fenêtre la plus « en avant ».
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                if self.show_settings {
                    self.close_settings();
                } else if self.show_admin {
                    self.close_admin();
                } else if self.show_account {
                    self.close_account();
                }
            }
            self.main_screen(ctx, &voice);
        } else {
            self.login_screen(ctx);
        }
        self.update_window(ctx);

        // Repeint périodique **seulement s'il y a quelque chose qui bouge**.
        //
        // Il était inconditionnel : vingt images par seconde à l'écran de
        // connexion, fenêtre réduite, application en arrière-plan pendant une
        // partie. Vingt reconstructions complètes de l'arbre de widgets, vingt
        // mises en page, vingt téléversements de maillage vers le GPU, par
        // seconde, pour un écran qui ne changeait pas.
        //
        // Ce qui a rendu la chose possible : la touche push-to-talk est
        // désormais lue sur son propre fil, qui réveille la fenêtre aux seuls
        // changements. Tout le reste de ce qui arrive de l'extérieur —
        // messages, événements réseau, images téléchargées, sondes de
        // serveurs — appelle déjà `request_repaint()` en arrivant.
        if let Some(delai) = self.repaint_delay(&voice) {
            ctx.request_repaint_after(delai);
        }

        self.perf.fin_image();
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        servers::save(storage, &self.book);
        storage.set_string(
            "window_maximized",
            if self.maximized { "on" } else { "off" }.into(),
        );
        // Géométrie héritée des versions qui laissaient eframe la persister.
        // La vider est ce qui corrige réellement la fenêtre minuscule : au
        // démarrage suivant, eframe ne trouve plus rien à désérialiser et
        // laisse la position au système. On ne touche à rien d'autre — le
        // carnet de serveurs vit dans le même fichier.
        storage.set_string("window", String::new());
        storage.set_string("sfx_on", if self.sfx_on { "on" } else { "off" }.into());
        storage.set_string("sfx_volume", format!("{}", self.sfx_volume));
        storage.set_string(
            "sfx_muted",
            self.sfx_muted.iter().cloned().collect::<Vec<_>>().join(","),
        );
        storage.set_string("update_skipped", self.updater.skipped().to_string());
        storage.set_string("url", self.url.clone());
        storage.set_string("username", self.username.clone());
        storage.set_string("mic_mode", self.mode.id().into());
        storage.set_string("input_gain", format!("{}", self.input_gain));
        storage.set_string("output_gain", format!("{}", self.output_gain));
        storage.set_string("vad_threshold", format!("{}", self.vad_threshold));
        storage.set_string("vad_hangover_ms", format!("{}", self.vad_hangover_ms));
        storage.set_string("bitrate", format!("{}", self.bitrate));
        storage.set_string("agc", if self.agc { "on" } else { "off" }.into());
        storage.set_string("agc_target", format!("{}", self.agc_target));
        storage.set_string("gate_threshold", format!("{}", self.gate_threshold));
        storage.set_string("jitter_frames", format!("{}", self.jitter_frames));
        storage.set_string("ptt_release_ms", format!("{}", self.ptt_release_ms));
        storage.set_string("noise_mode", format!("{}", self.noise_mode));
        storage.set_string("dred_mode", format!("{}", self.dred_mode));
        storage.set_string("ptt_key", self.ptt_key.id().into());
        storage.set_string("input_device", self.pref_input.clone().unwrap_or_default());
        storage.set_string(
            "output_device",
            self.pref_output.clone().unwrap_or_default(),
        );
        storage.set_string("native_audio", if self.native_audio { "on" } else { "off" }.into());
        storage.set_string("raw_mic", if self.raw_mic { "on" } else { "off" }.into());
        storage.set_string("comms_mic", if self.comms_mic { "on" } else { "off" }.into());
        storage.set_string("diag_share", if self.diag_share { "on" } else { "off" }.into());
        if let Ok(json) = serde_json::to_string(&self.all_volumes) {
            storage.set_string("user_volumes", json);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(mut conn) = self.conn.take() {
            conn.quit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_addresses_are_tagged() {
        assert_eq!(address_tag("127.0.0.1"), Some("local"));
        assert_eq!(address_tag("127.0.0.1:9987"), Some("local"));
        assert_eq!(address_tag("localhost"), Some("local"));
        assert_eq!(address_tag("192.168.1.30"), Some("réseau local"));
        assert_eq!(address_tag("10.0.0.7:9987"), Some("réseau local"));
        assert_eq!(address_tag("172.20.5.1"), Some("réseau local"));
        // Hors plages privées, et noms publics : pas d'étiquette.
        assert_eq!(address_tag("172.32.5.1"), None);
        assert_eq!(address_tag("8.8.8.8"), None);
        assert_eq!(address_tag("ts.baws.fun"), None);
        assert_eq!(address_tag("kora.chat:7000"), None);
    }

    #[test]
    fn quality_degrades_with_latency() {
        assert_eq!(quality(1).0, 4);
        assert_eq!(quality(29).0, 4);
        assert_eq!(quality(41).0, 3);
        assert_eq!(quality(88).0, 2);
        assert_eq!(quality(210).0, 1);
    }

    #[test]
    fn long_labels_are_cut_on_character_boundaries() {
        assert_eq!(ellipsize("BAWS", 26), "BAWS");
        // Accents compris : on compte des caractères, pas des octets.
        let long = "Sérveur très très long de Kévin";
        let cut = ellipsize(long, 12);
        assert_eq!(cut.chars().count(), 12);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn urls_are_shortened_without_breaking_utf8() {
        let url = format!("http://exemple.fr/{}", "é".repeat(80));
        let short = shorten(&url);
        assert!(short.chars().count() < url.chars().count());
        assert!(short.contains('…'));
    }

    #[test]
    fn links_are_separated_from_the_surrounding_text() {
        let parts = split_links("regarde http://a.fr/x puis dis-moi");
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], (false, "regarde "));
        assert_eq!(parts[1], (true, "http://a.fr/x"));
        assert_eq!(parts[2], (false, " puis dis-moi"));
        // Un message sans lien reste d'un seul bloc.
        assert_eq!(split_links("salut"), vec![(false, "salut")]);
    }

    /// L'attente entre deux tentatives double, plafonne, et surtout **se
    /// disperse**. La dispersion est ce qui empêche trente clients de frapper
    /// le serveur en même temps après un redémarrage : c'est la seule
    /// propriété de cette fonction qui vaille d'être testée.
    #[test]
    fn la_reprise_double_plafonne_et_se_disperse() {
        use std::time::Duration;

        // Chaque essai vaut entre la moitié et la totalité de son palier.
        for (essai, plein) in [(1u32, 1u64), (2, 2), (3, 4), (4, 8), (5, 16), (6, 30)] {
            let plein = Duration::from_secs(plein);
            for alea in [0u64, 1, 12_345, 999_999_999] {
                let d = Reprise::attente(essai, alea);
                assert!(d <= plein, "essai {essai} : {d:?} dépasse {plein:?}");
                assert!(d >= plein / 2, "essai {essai} : {d:?} sous la moitié de {plein:?}");
            }
        }

        // Plafonné : au-delà, on n'attend pas plus longtemps. Marteler moins
        // souvent ne sert plus à rien, et l'attente ne doit pas devenir telle
        // qu'on rate le retour du serveur.
        for essai in [7u32, 20, Reprise::MAX] {
            assert!(Reprise::attente(essai, 0) <= Reprise::PLAFOND);
        }

        // Et deux clients qui tirent deux nombres différents n'attendent pas
        // la même chose. Sans ça, tout le reste ne sert à rien : ils
        // repartiraient ensemble, échoueraient ensemble, et recommenceraient.
        let a = Reprise::attente(4, 0);
        let b = Reprise::attente(4, 3_999);
        assert_ne!(a, b, "deux tirages doivent donner deux attentes");
    }
}
