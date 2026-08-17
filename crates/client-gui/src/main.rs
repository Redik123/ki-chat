//! ki-chat : client graphique (egui) — chat texte + vocal, orienté gaming.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod appicon;
mod icons;
mod images;
mod net;
mod photos;
mod ptt;
mod secret;
mod servers;
mod theme;
mod ui;
mod update;

use std::collections::HashMap;

use chrono::{Datelike, TimeZone};
use device_query::{DeviceQuery, DeviceState};
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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

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
    let outcome = eframe::run_native(
        "ki-chat",
        options,
        Box::new(|cc| Ok(Box::new(KiApp::new(cc)))),
    );
    // Une mise à jour installée ne prend effet qu'au prochain lancement : on
    // le déclenche ici, la fenêtre fermée — donc après que les réglages ont
    // été enregistrés et les périphériques audio rendus.
    update::relaunch_if_requested();
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
}

impl AdminTab {
    const ALL: [AdminTab; 6] = [
        AdminTab::Server,
        AdminTab::Channels,
        AdminTab::Roles,
        AdminTab::Members,
        AdminTab::Invites,
        AdminTab::Audit,
    ];

    fn label(self) -> &'static str {
        match self {
            AdminTab::Server => "Serveur",
            AdminTab::Channels => "Salons",
            AdminTab::Roles => "Rôles",
            AdminTab::Members => "Membres",
            AdminTab::Invites => "Invitations",
            AdminTab::Audit => "Journal",
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
    preview_icon: Option<(String, egui::TextureHandle)>,
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
    /// Effets sonores chargés au démarrage : nom court -> PCM 48 kHz mono.
    sounds: HashMap<String, Vec<f32>>,
    sfx_on: bool,
    sfx_volume: f32,
    /// Occupants du salon vocal à l'image précédente : c'est leur écart qui
    /// révèle une arrivée ou un départ. Les messages `UserJoined`/`UserLeft`
    /// portent sur le serveur entier, pas sur le salon vocal.
    prev_voice_peers: std::collections::HashSet<UserId>,
    /// Sons étouffés jusque-là : entrer dans un salon peuplé déclencherait
    /// sinon six sons d'arrivée d'un coup.
    sfx_quiet_until: std::time::Instant,
    /// Reste-t-il du passé à remonter dans ce salon ?
    history_more: bool,
    /// Une page est déjà demandée : on n'en réclame pas une seconde à
    /// chaque image tant que celle-ci n'est pas arrivée.
    history_pending: bool,
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
    last_ptt_down: Option<std::time::Instant>,
    loopback: bool,
    /// Calibration des seuils en cours : (départ, crête ambiante mesurée).
    calibrating: Option<(std::time::Instant, f32)>,
    // Labo vidéo (S1a du partage d'écran) : boucle locale de test.
    labo: Option<ki_video::LocalLoop>,
    labo_frame: std::sync::Arc<std::sync::Mutex<Option<ki_video::RgbaFrame>>>,
    labo_stats: std::sync::Arc<ki_video::StageStats>,
    labo_texture: Option<egui::TextureHandle>,
    device: DeviceState,
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
}

impl KiApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
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

        Self {
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
            preview_icon: None,
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
            sounds: load_sounds(),
            sfx_on: get("sfx_on", "on") == "on",
            sfx_volume: get("sfx_volume", "0.6").parse().unwrap_or(0.6),
            prev_voice_peers: std::collections::HashSet::new(),
            sfx_quiet_until: std::time::Instant::now(),
            history_more: false,
            history_pending: false,
            chat_height: 0.0,
            history_anchor: None,
            input: String::new(),
            focus_input: false,
            show_settings: false,
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
            last_ptt_down: None,
            loopback: false,
            calibrating: None,
            labo: None,
            labo_frame: Default::default(),
            labo_stats: Default::default(),
            labo_texture: None,
            device: DeviceState::new(),
        }
    }

    /// Démarre la boucle locale du labo vidéo (S1a) : capture de l'écran
    /// principal, aller-retour H.264 complet, image déposée pour l'UI.
    fn start_labo(&mut self, ctx: egui::Context) {
        let stats = std::sync::Arc::new(ki_video::StageStats::default());
        let frame_slot = self.labo_frame.clone();
        let sink: ki_video::FrameSink = std::sync::Arc::new(move |frame| {
            *frame_slot.lock().unwrap() = Some(frame);
            // Seul moyen de peindre à 30 fps : le repeint périodique de
            // l'app est plafonné à 20 fps par request_repaint_after(50 ms).
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
        let Some(conn) = &self.conn else { return };
        if let Some(engine) = conn.engine.lock().unwrap().as_ref() {
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
        let Some(conn) = &self.conn else { return VoiceSnapshot::default() };
        let ping = conn.rtt_ms();
        match conn.engine.lock().unwrap().as_ref() {
            Some(engine) => VoiceSnapshot {
                engine_up: true,
                stats: engine.stats(),
                ping,
                levels: engine.user_levels().into_iter().collect(),
                device_trouble: engine.device_trouble(),
            },
            None => VoiceSnapshot {
                ping,
                ..Default::default()
            },
        }
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
        if let Some(conn) = &self.conn {
            if let Some(engine) = conn.engine.lock().unwrap().as_ref() {
                engine.set_user_volume(user_id, gain);
            }
        }
    }

    /// Base HTTP du serveur (partage de fichiers) : même hôte que le QUIC,
    /// port HTTP conventionnel 8080.
    fn http_base(&self) -> String {
        let trimmed = self.url.trim();
        let host = trimmed.rsplit_once(':').map(|(h, _)| h).unwrap_or(trimmed);
        format!("http://{host}:8080")
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
        if !self.sfx_on || std::time::Instant::now() < self.sfx_quiet_until {
            return;
        }
        let Some(pcm) = self.sounds.get(name) else { return };
        let Some(conn) = &self.conn else { return };
        if let Some(engine) = conn.engine.lock().unwrap().as_ref() {
            engine.play_effect(pcm, self.sfx_volume);
        }
    }

    /// Compare les occupants de mon salon vocal à ceux de l'image précédente
    /// pour jouer les sons d'arrivée et de départ.
    ///
    /// C'est un écart qu'on mesure, et non `UserJoined`/`UserLeft` : ces
    /// deux-là portent sur le serveur entier, alors que seul mon propre
    /// salon vocal m'intéresse ici.
    fn update_voice_peers(&mut self) {
        let Some(mine) = self.voice_channel else {
            self.prev_voice_peers.clear();
            return;
        };
        let now: std::collections::HashSet<UserId> = self
            .members
            .iter()
            .filter(|m| m.voice == Some(mine) && Some(m.user_id) != self.my_id)
            .map(|m| m.user_id)
            .collect();
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
        self.voice_channel = Some(channel);
        self.play_sfx(sfx::SELF_JOIN);
        // Le roster arrive juste après : sans ce répit, entrer dans un salon
        // déjà peuplé jouerait un son d'arrivée par occupant.
        self.sfx_quiet_until = std::time::Instant::now() + std::time::Duration::from_millis(1500);
        self.prev_voice_peers.clear();
        self.send(ClientMsg::JoinVoice { channel, password: None });
    }

    /// Quitte le vocal, sans quitter le serveur.
    fn leave_voice(&mut self) {
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

    fn disconnect(&mut self, error: Option<String>) {
        if let Some(mut conn) = self.conn.take() {
            conn.quit();
        }
        self.connecting = false;
        self.welcomed = false;
        self.my_id = None;
        self.my_perms = 0;
        self.my_rank = 0;
        self.roles.clear();
        self.voice_token = 0;
        self.channels.clear();
        self.current = None;
        self.voice_channel = None;
        self.members.clear();
        self.messages.clear();
        self.armed = false;
        self.transmitting = false;
        self.loopback = false;
        self.show_settings = false;
        self.show_admin = false;
        self.show_account = false;
        self.error = error;
    }

    fn connect(&mut self, ctx: &egui::Context) {
        self.error = None;
        self.connecting = true;
        let invite = self.invite.trim();
        self.conn = Some(net::connect(
            self.url.trim().to_string(),
            net::Credentials {
                username: self.username.trim().to_string(),
                password: self.password.clone(),
                invite: (!invite.is_empty()).then(|| invite.to_string()),
            },
            self.voice_prefs(),
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
                net::Event::ConnectFailed(e) => self.disconnect(Some(e)),
                net::Event::Disconnected => {
                    let had_error = self.error.take();
                    self.disconnect(had_error.or_else(|| Some("déconnecté du serveur".into())));
                }
                net::Event::Msg(msg) => self.handle_server_msg(msg),
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
                self.error = None;
                // Le roster et l'historique arrivent dans la foulée : sans
                // ce répit, se connecter déclencherait une salve de sons.
                self.sfx_quiet_until =
                    std::time::Instant::now() + std::time::Duration::from_secs(2);
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
                // Jamais de son pour ses propres messages : on sait qu'on
                // vient d'écrire.
                if Some(user_id) != self.my_id {
                    self.play_sfx(sfx::MESSAGE);
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
                self.update_voice_peers();
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
            ServerMsg::VoiceState { user_id, speaking } => {
                if let Some(m) = self.members.iter_mut().find(|m| m.user_id == user_id) {
                    m.speaking = speaking;
                }
            }
            ServerMsg::Error { message } => {
                let message = ki_protocol::safe_display(&message, 300);
                // Avant le Welcome, une erreur = échec de connexion (jeton...).
                if !self.welcomed {
                    self.disconnect(Some(message));
                } else {
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

    /// Sélection d'un fichier puis upload, dans un thread (le dialogue
    /// natif et l'envoi ne doivent pas bloquer l'UI).
    fn start_upload(&self) {
        let Some(conn) = &self.conn else { return };
        let sender = conn.sender();
        let base = self.http_base();
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
                let resp = ureq::post(&format!("{base}/upload?name={name}"))
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
        if let Some(conn) = &self.conn {
            if let Some(engine) = conn.engine.lock().unwrap().as_ref() {
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
        let Some(conn) = &self.conn else { return };
        let engine_guard = conn.engine.lock().unwrap();
        let Some(engine) = engine_guard.as_ref() else { return };

        // « Armé » : le micro a le droit d'émettre. En activation vocale,
        // c'est ensuite le moteur qui décide selon le seuil.
        let key_down =
            self.mode == MicMode::Ptt && self.device.get_keys().contains(&self.ptt_key.keycode());
        if key_down {
            self.last_ptt_down = Some(std::time::Instant::now());
        }
        // Relâchement : on continue d'émettre un court instant après la
        // touche, pour ne pas couper la dernière syllabe.
        let ptt_active = self.mode == MicMode::Ptt
            && (key_down
                || self
                    .last_ptt_down
                    .is_some_and(|t| t.elapsed().as_millis() < self.ptt_release_ms as u128));
        // Hors d'un salon vocal, le micro reste fermé quoi qu'il arrive.
        let armed = !self.muted
            && self.voice_channel.is_some()
            && (matches!(self.mode, MicMode::Open | MicMode::Vad) || ptt_active);
        if armed != self.armed {
            self.armed = armed;
            engine.set_transmit(armed);
        }

        // Émission réelle (après VAD) : indicateur TX + diffusion aux autres.
        let sending = engine.is_sending();
        drop(engine_guard);
        if sending != self.transmitting {
            self.transmitting = sending;
            self.send(ClientMsg::VoiceState { speaking: sending });
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

                            if let Some(err) = self.error.clone() {
                                ui.add_space(12.0);
                                if ui::banner(ui, Tone::Danger, &err, true) {
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
        // Le bouton nomme sa destination : avec plusieurs serveurs, ça évite
        // de se connecter au mauvais sans s'en rendre compte.
        let label = match (self.connecting, self.active_server()) {
            (true, _) => "Connexion…".to_string(),
            (false, Some(server)) => format!("Se connecter à {}", ellipsize(server.label(), 26)),
            (false, None) => "Se connecter".to_string(),
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

    fn main_screen(&mut self, ctx: &egui::Context) {
        self.mount_avatars(ctx);
        self.previews.set_origin(self.http_base());
        self.previews.mount(ctx);
        let voice = self.voice_snapshot();

        self.voice_bar(ctx, &voice);
        self.sidebar(ctx, &voice);
        self.roster_panel(ctx, &voice);
        self.chat_panel(ctx);

        if self.show_settings {
            self.settings_window(ctx, &voice);
        }
        if self.show_admin {
            self.admin_window(ctx);
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
                self.voice_channel = Some(channel);
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
                    ui::banner(
                        ui,
                        Tone::Warn,
                        &format!("{what} — reconnexion automatique en cours…"),
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
                                let channels = self.channels.clone();

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
            let response = member_row(ui, m, speaking, is_me, level, volume, photo);
            self.member_menu(response, m, is_me);
        }
        ui.add_space(6.0);
    }

    /// Clic sur soi-même : son compte. Clic droit sur un autre : volume et
    /// modération. Le même comportement dans les deux listes.
    fn member_menu(&mut self, response: egui::Response, m: &Member, is_me: bool) {
        if is_me {
            if response.on_hover_text("gérer mon compte").clicked() {
                self.show_account = true;
            }
            return;
        }
        response.context_menu(|ui| {
            ui.set_width(228.0);
            ui.label(RichText::new(&m.username).color(self.color_of(m)).strong());
            if let Some(role) = self.top_role_name(m) {
                ui.label(RichText::new(role).color(TEXT_FAINT).size(11.0));
            }
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
            // Chaque action n'apparaît que si elle aboutirait : la permission
            // ET le rang. Un bouton grisé n'invite qu'à un clic qui échoue,
            // là où la hiérarchie se lit déjà dans les badges de rôle.
            let can_moderate = self.outranks(m.rank);
            let show_kick = can_moderate && self.can(ki_protocol::perm::KICK);
            let show_ban = can_moderate && self.can(ki_protocol::perm::BAN);
            if show_kick || show_ban {
                ui.add_space(4.0);
                ui::hairline(ui);
                ui.add_space(4.0);
            }
            // Expulser met dehors ; la personne peut revenir aussitôt.
            if show_kick {
                if ui::tinted_button(ui, Some(Icon::Logout), "Expulser", Tone::Danger).clicked() {
                    self.send(ClientMsg::Kick {
                        user_id: m.user_id,
                        reason: String::new(),
                    });
                    ui.close();
                }
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
                let members = self.members.clone();
                ui.horizontal(|ui| {
                    ui::section_label(ui, "En ligne");
                    ui.label(
                        RichText::new(members.len().to_string())
                            .color(theme::BORDER_STRONG)
                            .size(11.0)
                            .strong(),
                    );
                });
                ui.add_space(2.0);

                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    for m in &members {
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
                        let response =
                            member_row(ui, m, speaking && audible, is_me, level, volume, photo);
                        self.member_menu(response, m, is_me);
                    }
                });
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
                    let response = ui.add_sized(
                        Vec2::new(ui.available_width() - send_width, 26.0),
                        egui::TextEdit::singleline(&mut self.input)
                            .char_limit(ki_protocol::MAX_CHAT_TEXT)
                            .frame(false)
                            .margin(egui::Margin::symmetric(4, 4))
                            .hint_text(format!("Message dans #{channel_name}")),
                    );
                    if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submit = true;
                        self.focus_input = true;
                    }
                    if std::mem::take(&mut self.focus_input) {
                        response.request_focus();
                    }

                    let tint = if filled { Some(ACCENT) } else { None };
                    if ui::icon_button_ex(ui, Icon::Send, 32.0, "Envoyer (Entrée)", tint).clicked()
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

                let mut last_day = i32::MIN;
                let mut previous: Option<(UserId, u64)> = None;
                let messages = std::mem::take(&mut self.messages);
                for msg in &messages {
                    let day = day_key(msg.ts);
                    if day != last_day {
                        day_separator(ui, &day_label(msg.ts));
                        last_day = day;
                        previous = None;
                    }
                    let grouped = previous.is_some_and(|(user, ts)| {
                        user == msg.user_id && msg.ts.saturating_sub(ts) < GROUP_WINDOW_MS
                    });
                    let photo = self.avatars.get(&msg.user_id).map(|(_, t)| t.clone());
                    // L'auteur peut avoir quitté le serveur : on retombe
                    // alors sur son pseudo, plutôt que de perdre la couleur.
                    let color = self
                        .members
                        .iter()
                        .find(|m| m.user_id == msg.user_id)
                        .map(|m| theme::member_color(m.color, &m.username))
                        .unwrap_or_else(|| color_for(&msg.username));
                    message_block(
                        ui,
                        msg,
                        !grouped,
                        photo.as_ref(),
                        &mut self.previews,
                        color,
                    );
                    previous = Some((msg.user_id, msg.ts));
                }
                self.messages = messages;
                ui.add_space(10.0);
            });
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
    }

    // -----------------------------------------------------------------
    // Fenêtres
    // -----------------------------------------------------------------

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
                            if let Some(conn) = &self.conn {
                                if let Some(engine) = conn.engine.lock().unwrap().as_ref() {
                                    engine.play_test_tone();
                                }
                            }
                        }

                        // --- Effets sonores ---
                        ui.add_space(12.0);
                        ui::hairline(ui);
                        ui.add_space(10.0);
                        ui::group_title(ui, Icon::Play, "Effets sonores");
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
                        }
                        if self.sounds.is_empty() {
                            ui::hint(
                                ui,
                                "aucun son chargé — dépose des .wav dans le dossier « sons »",
                            );
                        } else {
                            let mut names: Vec<&str> =
                                self.sounds.keys().map(String::as_str).collect();
                            names.sort_unstable();
                            ui::hint(ui, &format!("chargés : {}", names.join(", ")));
                        }
                        if ui::button(ui, Icon::Refresh, "Recharger les sons").clicked() {
                            self.sounds = load_sounds();
                        }

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
            if let Some(conn) = &self.conn {
                conn.restart_voice(self.voice_prefs());
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
                let staged = self.preview_texture(ctx, pending.as_deref());
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
                        self.preview_icon = None;
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
        self.preview_icon = None;
    }

    /// Fenêtre d'administration : invitations, comptes, blocages, mots de passe.
    /// Texture d'aperçu d'une vignette, reconstruite seulement si elle change.
    fn preview_texture(
        &mut self,
        ctx: &egui::Context,
        data: Option<&str>,
    ) -> Option<egui::TextureHandle> {
        let Some(data) = data else {
            self.preview_icon = None;
            return None;
        };
        if !self.preview_icon.as_ref().is_some_and(|(key, _)| key == data) {
            self.preview_icon = servers::decode_icon(data).map(|image| {
                let texture =
                    ctx.load_texture("icon-preview", image, egui::TextureOptions::LINEAR);
                (data.to_string(), texture)
            });
        }
        self.preview_icon.as_ref().map(|(_, texture)| texture.clone())
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
        let preview = self.preview_texture(ctx, pending.as_deref());
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
        ui.checkbox(&mut draft.restricted, "Réservé à certains rôles");
        if draft.restricted {
            let roles = self.roles.clone();
            ui.horizontal_wrapped(|ui| {
                for role in &roles {
                    // `@everyone` n'a pas de sens dans une restriction : le
                    // cocher reviendrait à ne rien restreindre du tout.
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
        for ch in &channels {
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
                });
            });
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
        self.preview_icon = None;
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

/// Ligne de membre : avatar, pseudo, badges, vumètre pendant qu'il parle.
fn member_row(
    ui: &mut egui::Ui,
    member: &Member,
    speaking: bool,
    is_me: bool,
    level: f32,
    volume: f32,
    photo: Option<&egui::TextureHandle>,
) -> egui::Response {
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
    // c'est le hachage du pseudo, comme avant les rôles.
    let color = theme::member_color(member.color, &member.username);
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

    // Côté droit : volume personnalisé, puis vumètre pendant la parole.
    let mut right = rect.right() - 10.0;
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
fn message_block(
    ui: &mut egui::Ui,
    msg: &ChatRecord,
    with_header: bool,
    photo: Option<&egui::TextureHandle>,
    previews: &mut images::Previews,
    // Couleur de l'auteur, résolue depuis son rôle par l'appelant. Elle
    // n'est pas figée dans l'historique : changer un rôle doit recolorer les
    // anciens messages, pas seulement les nouveaux.
    color: egui::Color32,
) {
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
                message_body(ui, &msg.text);
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
                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
            }
        }
    }
}

fn message_body(ui: &mut egui::Ui, text: &str) {
    let parts = split_links(text);
    if parts.len() == 1 && !parts[0].0 {
        ui.label(RichText::new(text).color(TEXT).size(14.0));
        return;
    }
    ui.scope(|ui| {
        // Espacement nul : les espaces font partie des fragments de texte,
        // le retour à la ligne tombe donc au bon endroit.
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.horizontal_wrapped(|ui| {
            for (is_link, chunk) in parts {
                if is_link {
                    ui.hyperlink_to(RichText::new(shorten(chunk)).size(14.0), chunk)
                        .on_hover_text(chunk);
                } else {
                    ui.label(RichText::new(chunk).color(TEXT).size(14.0));
                }
            }
        });
    });
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
        // Géométrie : on ne restaure que « maximisée », et on suit l'état
        // courant pour le réenregistrer. Cf. `main` pour le pourquoi.
        if self.restore_maximized {
            self.restore_maximized = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
        }
        let was_maximized = self.maximized;
        self.maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(was_maximized));

        self.poll_events();
        self.update_voice();

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
            self.main_screen(ctx);
        } else {
            self.login_screen(ctx);
        }
        self.update_window(ctx);

        // Repeint périodique : nécessaire pour le PTT global (poll clavier)
        // et les indicateurs temps réel, même fenêtre non focalisée.
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
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
}
