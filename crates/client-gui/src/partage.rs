//! Le partage d'écran côté interface : l'état d'une diffusion en cours, ses
//! réglages, et le fil décodeur d'un spectateur — jalons S1b et S2 de
//! PLAN-STREAM.md.
//!
//! Deux moitiés, volontairement dissymétriques :
//! - **diffuser** : la boucle streamer (crate vidéo) capture et encode ; la
//!   couche réseau (net.rs) chiffre et émet. Ici ne vivent que l'assemblage,
//!   les réglages et l'aperçu local.
//! - **regarder** : les trames arrivent brutes du réseau (chiffrées, une par
//!   flux QUIC, dans le désordre) ; le fil de ce module déchiffre, remet en
//!   ordre par numéro de séquence, décode, et dépose l'image pour l'UI.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use eframe::egui::{self, RichText};
use ki_protocol::StreamMeta;
use ki_video::{
    CaptureSource, EncoderChoice, FrameEmit, FrameSink, MonitorInfo, StageStats, StreamConfig,
    StreamerLoop, ViewerDecoder, WindowInfo,
};

use crate::theme::{TEXT, TEXT_DIM, WARN};
use crate::ui;

// ---------------------------------------------------------------------
// Réglages
// ---------------------------------------------------------------------

/// Les réglages de diffusion, persistés dans le stockage eframe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reglages {
    pub source: CaptureSource,
    /// Hauteur plafond de l'image émise ; 0 = celle de la source.
    pub max_height: u32,
    pub fps: u32,
    pub kbps: u32,
    pub cursor: bool,
    pub preview: bool,
    pub encodeur: EncoderChoice,
    /// Le son du jeu (tout le système sauf ki-chat) dans le stream.
    pub son: bool,
}

impl Default for Reglages {
    fn default() -> Self {
        Self {
            source: CaptureSource::Monitor(0),
            max_height: 0,
            fps: 30,
            kbps: 6000,
            cursor: true,
            preview: true,
            encodeur: EncoderChoice::Auto,
            son: true,
        }
    }
}

/// Les encodeurs proposés, avec le mot qui les présente.
const ENCODEURS: [(EncoderChoice, &str, &str); 3] = [
    (EncoderChoice::Auto, "auto", "Auto — NVENC si la carte le permet"),
    (EncoderChoice::Nvenc, "nvenc", "NVENC (carte NVIDIA)"),
    (EncoderChoice::Logiciel, "logiciel", "Logiciel (processeur)"),
];

/// Les hauteurs proposées (0 = celle de la source).
const HAUTEURS: [(u32, &str); 4] = [(0, "Native"), (1080, "1080p"), (720, "720p"), (480, "480p")];
const CADENCES: [u32; 3] = [15, 30, 60];

impl Reglages {
    pub fn load(get: impl Fn(&str, &str) -> String) -> Self {
        let d = Self::default();
        let source = match get("stream_source", "").split_once(':') {
            Some(("fenetre", titre)) if !titre.is_empty() => {
                CaptureSource::Window(titre.to_string())
            }
            Some(("ecran", n)) => CaptureSource::Monitor(n.parse().unwrap_or(0)),
            _ => CaptureSource::Monitor(0),
        };
        let nombre = |cle: &str, defaut: u32| get(cle, "").parse().unwrap_or(defaut);
        Self {
            source,
            max_height: nombre("stream_max_height", d.max_height),
            fps: nombre("stream_fps", d.fps).clamp(1, 120),
            kbps: nombre("stream_kbps", d.kbps).clamp(500, 50_000),
            cursor: get("stream_cursor", "on") != "off",
            preview: get("stream_preview", "on") != "off",
            son: get("stream_audio", "on") != "off",
            encodeur: {
                let cle = get("stream_encoder", "auto");
                ENCODEURS
                    .iter()
                    .find(|(_, id, _)| *id == cle)
                    .map(|(e, _, _)| *e)
                    .unwrap_or_default()
            },
        }
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        let source = match &self.source {
            CaptureSource::Monitor(n) => format!("ecran:{n}"),
            CaptureSource::Window(t) => format!("fenetre:{t}"),
        };
        storage.set_string("stream_source", source);
        storage.set_string("stream_max_height", self.max_height.to_string());
        storage.set_string("stream_fps", self.fps.to_string());
        storage.set_string("stream_kbps", self.kbps.to_string());
        storage.set_string("stream_cursor", if self.cursor { "on" } else { "off" }.into());
        storage.set_string("stream_preview", if self.preview { "on" } else { "off" }.into());
        storage.set_string("stream_audio", if self.son { "on" } else { "off" }.into());
        let encodeur = ENCODEURS
            .iter()
            .find(|(e, _, _)| *e == self.encodeur)
            .map(|(_, id, _)| *id)
            .unwrap_or("auto");
        storage.set_string("stream_encoder", encodeur.into());
    }

    pub fn config(&self) -> StreamConfig {
        StreamConfig {
            source: self.source.clone(),
            max_height: self.max_height,
            fps: self.fps,
            bitrate_bps: self.kbps.saturating_mul(1000),
            cursor: self.cursor,
            preview: self.preview,
            encoder: self.encodeur,
            gop_s: 2,
            profil: ki_video::Profil::Diffusion,
        }
    }

    /// Ce qu'on annonce au salon — les dimensions viendront des trames.
    pub fn meta(&self) -> StreamMeta {
        StreamMeta {
            width: 0,
            height: 0,
            fps: self.fps.min(255) as u8,
            kbps: self.kbps,
            machine: empreinte_machine(),
        }
    }
}

/// L'empreinte de cette machine et de ce compte Windows, telle qu'elle
/// voyage dans les métadonnées d'un stream : deux ki-chat lancés ici (un
/// qui diffuse, un qui regarde — le cas du test) se reconnaissent, et le
/// spectateur coupe le son du jeu chez lui au lieu de le renvoyer en boucle
/// dans la capture du streamer. FNV-1a, comme les vignettes.
pub fn empreinte_machine() -> u64 {
    let nom = nom_machine();
    let compte = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in nom.bytes().chain([0u8]).chain(compte.bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h.max(1)
}

/// Le nom de la machine, tel que le système le donne.
#[cfg(windows)]
fn nom_machine() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_default()
}

/// Unix n'exporte pas le nom d'hôte dans l'environnement d'une application
/// graphique : on le demande à la libc.
#[cfg(unix)]
fn nom_machine() -> String {
    let mut tampon = [0u8; 256];
    // SAFETY : le tampon est le nôtre, sa longueur est passée avec lui, et
    // gethostname n'écrit jamais au-delà.
    let rc = unsafe { libc::gethostname(tampon.as_mut_ptr().cast(), tampon.len()) };
    if rc != 0 {
        return String::new();
    }
    let fin = tampon.iter().position(|&b| b == 0).unwrap_or(tampon.len());
    String::from_utf8_lossy(&tampon[..fin]).into_owned()
}

/// Écrans et fenêtres capturables, relevés à l'ouverture du sélecteur et
/// rafraîchis à la demande — énumérer les fenêtres à chaque image serait
/// du gaspillage pour une liste qui bouge une fois par minute.
#[derive(Default)]
pub struct Sources {
    pub ecrans: Vec<MonitorInfo>,
    pub fenetres: Vec<WindowInfo>,
    releve: Option<Instant>,
}

impl Sources {
    pub fn rafraichir(&mut self) {
        self.ecrans = ki_video::list_monitors();
        self.fenetres = ki_video::list_windows();
        self.releve = Some(Instant::now());
    }

    /// Jamais relevées, ou depuis trop longtemps pour un panneau qui
    /// vient de s'ouvrir.
    pub fn perimees(&self) -> bool {
        self.releve.is_none_or(|t| t.elapsed() > Duration::from_secs(20))
    }
}

/// Le libellé d'une source, tel que le sélecteur le montre.
fn libelle(source: &CaptureSource, sources: &Sources) -> String {
    match source {
        CaptureSource::Monitor(0) => "Écran principal".to_string(),
        CaptureSource::Monitor(n) => sources
            .ecrans
            .iter()
            .find(|e| e.index == *n)
            .map(|e| format!("Écran {n} — {}", e.name))
            .unwrap_or_else(|| format!("Écran {n}")),
        CaptureSource::Window(t) => format!("Fenêtre : {t}"),
    }
}

/// Les réglages de diffusion, dans le sélecteur comme dans ⚙. Rend `true`
/// si quelque chose a changé.
pub fn reglages_ui(ui: &mut egui::Ui, r: &mut Reglages, sources: &mut Sources) -> bool {
    let mut change = false;

    ui::field_label(ui, "Source");
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("stream_source")
            .width(280.0)
            .selected_text(RichText::new(libelle(&r.source, sources)).color(TEXT))
            .show_ui(ui, |ui| {
                let principal = CaptureSource::Monitor(0);
                if ui.selectable_label(r.source == principal, "Écran principal").clicked() {
                    r.source = principal;
                    change = true;
                }
                for e in &sources.ecrans {
                    let s = CaptureSource::Monitor(e.index);
                    let mention = if e.primary { " (principal)" } else { "" };
                    let txt = format!("Écran {} — {} {}x{}{mention}", e.index, e.name, e.width, e.height);
                    if ui.selectable_label(r.source == s, txt).clicked() {
                        r.source = s;
                        change = true;
                    }
                }
                if !sources.fenetres.is_empty() {
                    ui.separator();
                    ui.label(RichText::new("Fenêtres").color(TEXT_DIM).size(11.5));
                }
                for f in &sources.fenetres {
                    let s = CaptureSource::Window(f.title.clone());
                    let txt = if f.process.is_empty() {
                        f.title.clone()
                    } else {
                        format!("{} — {}", f.title, f.process)
                    };
                    if ui.selectable_label(r.source == s, txt).clicked() {
                        r.source = s;
                        change = true;
                    }
                }
            });
        if ui::icon_button(ui, crate::icons::Icon::Refresh, "relever les écrans et fenêtres")
            .clicked()
        {
            sources.rafraichir();
        }
    });
    if matches!(r.source, CaptureSource::Window(_)) {
        ui::hint(ui, "une fenêtre réduite dans la barre des tâches ne se capture plus");
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("résolution").color(TEXT_DIM).size(12.5));
        let actuel = HAUTEURS
            .iter()
            .find(|(h, _)| *h == r.max_height)
            .map(|(_, l)| l.to_string())
            .unwrap_or_else(|| format!("{}p", r.max_height));
        egui::ComboBox::from_id_salt("stream_res")
            .width(96.0)
            .selected_text(RichText::new(actuel).color(TEXT))
            .show_ui(ui, |ui| {
                for (h, l) in HAUTEURS {
                    if ui.selectable_label(r.max_height == h, l).clicked() {
                        r.max_height = h;
                        change = true;
                    }
                }
            });
        ui.add_space(8.0);
        ui.label(RichText::new("images/s").color(TEXT_DIM).size(12.5));
        egui::ComboBox::from_id_salt("stream_fps")
            .width(64.0)
            .selected_text(RichText::new(r.fps.to_string()).color(TEXT))
            .show_ui(ui, |ui| {
                for c in CADENCES {
                    if ui.selectable_label(r.fps == c, c.to_string()).clicked() {
                        r.fps = c;
                        change = true;
                    }
                }
            });
    });

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("encodeur").color(TEXT_DIM).size(12.5));
        let actuel = ENCODEURS
            .iter()
            .find(|(e, _, _)| *e == r.encodeur)
            .map(|(_, _, l)| *l)
            .unwrap_or("Auto");
        egui::ComboBox::from_id_salt("stream_encoder")
            .width(240.0)
            .selected_text(RichText::new(actuel).color(TEXT))
            .show_ui(ui, |ui| {
                for (e, _, l) in ENCODEURS {
                    if ui.selectable_label(r.encodeur == e, l).clicked() {
                        r.encodeur = e;
                        change = true;
                    }
                }
            });
    });
    ui::hint(
        ui,
        "NVENC encode sur la carte graphique : le processeur reste au jeu. Sans carte \
         NVIDIA, l'encodeur logiciel prend le relais tout seul.",
    );
    // L'état réel de NVENC sur cette machine, tel que l'inventaire l'a
    // relevé au démarrage : disponible sur telle carte, ou pourquoi pas —
    // un pilote trop ancien se voit ici avant de se subir en diffusant.
    if let Some(inventaire) = ki_video::inventaire_pret() {
        let etat = inventaire.split("; ").nth(1).unwrap_or(inventaire);
        let vieux = etat.contains("trop ancien");
        ui.label(
            RichText::new(etat)
                .color(if vieux { WARN } else { TEXT_DIM })
                .size(11.5),
        );
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("débit").color(TEXT_DIM).size(12.5));
        let mut mbit = r.kbps as f32 / 1000.0;
        if ui
            .add(
                egui::Slider::new(&mut mbit, 1.0..=20.0)
                    .step_by(0.5)
                    .fixed_decimals(1)
                    .suffix(" Mbit/s"),
            )
            .changed()
        {
            r.kbps = (mbit * 1000.0).round() as u32;
            change = true;
        }
    });
    ui::hint(
        ui,
        "720p · 30 i/s · 4 Mbit/s passe partout ; 1080p · 60 i/s demande 10 Mbit/s et un \
         CPU disponible — le débit sort de ta connexion une fois, le serveur le \
         redistribue",
    );

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.checkbox(&mut r.cursor, "Curseur de la souris").changed() {
            change = true;
        }
        ui.add_space(10.0);
        if ui
            .checkbox(&mut r.preview, "Fenêtre d'aperçu")
            .on_hover_text("voir ce que les autres reçoivent — coûte un décodage par image")
            .changed()
        {
            change = true;
        }
    });
    ui.add_space(6.0);
    if ui
        .checkbox(&mut r.son, "Son du jeu dans le stream")
        .on_hover_text(
            "tout ce que joue ton PC, sauf ki-chat lui-même : les spectateurs entendent le \
             jeu (et ta musique), jamais leurs propres voix en retour. Windows 10 2004 ou \
             plus récent.",
        )
        .changed()
    {
        change = true;
    }
    change
}

// ---------------------------------------------------------------------
// Diffuser
// ---------------------------------------------------------------------

/// Une diffusion en cours, vue de l'interface.
///
/// Elle garde de quoi relancer la capture avec d'autres réglages sans
/// toucher au stream : le même émetteur (la séquence continue, les nonces
/// ne se répètent jamais), le même drapeau « trame clé exigée », la même
/// fenêtre d'aperçu.
pub struct GoLive {
    pub boucle: StreamerLoop,
    pub stats: Arc<StageStats>,
    pub stream_id: u32,
    /// L'aperçu local — exactement ce que les spectateurs reçoivent, déjà
    /// converti en image egui sur le fil vidéo : le fil d'interface n'a plus
    /// que la texture à pousser.
    pub apercu: Arc<Mutex<Option<egui::ColorImage>>>,
    pub emit: FrameEmit,
    pub sink: FrameSink,
    pub force_idr: Arc<AtomicBool>,
    /// Cadence et débit annoncés au salon, lus par la couche réseau quand
    /// les dimensions changent.
    pub cadence: Arc<Mutex<StreamMeta>>,
    /// Les réglages en vigueur.
    pub reglages: Reglages,
    /// La clé du stream, pour (re)démarrer le son du jeu en cours de route.
    pub key: [u8; 32],
    /// Le son du jeu, tant qu'il est diffusé — indépendant de la vidéo.
    pub audio: Option<ki_voice::jeu::GameAudio>,
    /// L'instant zéro des horodatages, commun à l'image et au son, et qui
    /// survit aux changements de réglages : sans ça, chaque relance de la
    /// capture remettait la vidéo à zéro pendant que le son continuait.
    pub origine: std::time::Instant,
    /// Les deux qualités : la basse que le serveur demande pour les
    /// connexions lentes, la haute suspendue quand personne ne la regarde.
    /// Elles survivent aux relances de la capture.
    pub qualites: Arc<ki_video::Qualites>,
}

impl GoLive {
    pub fn arreter(self) {
        self.boucle.stop();
    }

    /// Relance la capture avec d'autres réglages, le stream restant le même.
    /// L'ancienne boucle s'arrête d'abord : deux encodeurs qui se relaient
    /// sur la même séquence donneraient un salmigondis au décodeur. Le son
    /// du jeu, lui, continue sans interruption.
    pub fn reconfigurer(self, reglages: &Reglages) -> anyhow::Result<Self> {
        let Self {
            boucle,
            stats,
            stream_id,
            apercu,
            emit,
            sink,
            force_idr,
            cadence,
            key,
            audio,
            origine,
            qualites,
            ..
        } = self;
        boucle.stop();
        *apercu.lock().unwrap() = None;
        let boucle = StreamerLoop::start(
            stats.clone(),
            sink.clone(),
            emit.clone(),
            reglages.config(),
            force_idr.clone(),
            origine,
            Some(qualites.clone()),
        )?;
        Ok(Self {
            boucle,
            stats,
            stream_id,
            apercu,
            emit,
            sink,
            force_idr,
            cadence,
            reglages: reglages.clone(),
            key,
            audio,
            origine,
            qualites,
        })
    }
}

/// Une cadence instantanée déduite de compteurs cumulés : images/s et
/// kbit/s sur la dernière seconde, pas la moyenne depuis le début.
pub struct Cadence {
    depuis: Instant,
    trames: u64,
    octets: u64,
    pub fps: f32,
    pub kbps: f32,
}

impl Default for Cadence {
    fn default() -> Self {
        Self::new()
    }
}

impl Cadence {
    pub fn new() -> Self {
        Self { depuis: Instant::now(), trames: 0, octets: 0, fps: 0.0, kbps: 0.0 }
    }

    pub fn relever(&mut self, trames: u64, octets: u64) {
        let dt = self.depuis.elapsed().as_secs_f32();
        if dt < 1.0 {
            return;
        }
        // Un compteur qui recule, c'est une boucle relancée : on repart.
        if trames >= self.trames && octets >= self.octets {
            self.fps = (trames - self.trames) as f32 / dt;
            self.kbps = (octets - self.octets) as f32 * 8.0 / 1000.0 / dt;
        }
        self.trames = trames;
        self.octets = octets;
        self.depuis = Instant::now();
    }
}

/// Une hauteur plafonnée : `reglee` à 0 veut dire « celle de la source »,
/// que le plafond remplace alors.
pub fn plafonner_hauteur(reglee: u32, plafond: u32) -> u32 {
    if reglee == 0 {
        plafond
    } else {
        reglee.min(plafond)
    }
}

// ---------------------------------------------------------------------
// L'encodeur qui se règle tout seul
// ---------------------------------------------------------------------

/// Les crans que la diffusion descend quand l'encodeur ne suit pas :
/// d'abord la cadence à 30 i/s, puis la hauteur, 1080p puis 720p — jamais
/// plus bas, un stream à 480p ne se regarde plus. `hauteur` est celle que
/// l'on émet (la source, ou le plafond réglé), `fps` le réglage. Le premier
/// cran est le réglage lui-même.
pub fn paliers_encodeur(hauteur: u32, fps: u32) -> Vec<(u32, u32)> {
    let mut crans = vec![(hauteur, fps)];
    let mut f = fps;
    if f > 30 {
        f = 30;
        crans.push((hauteur, f));
    }
    let mut h = hauteur;
    for candidat in [1080, 720] {
        if candidat < h {
            h = candidat;
            crans.push((h, f));
        }
    }
    crans
}

/// Ce que le régulateur décide à une seconde donnée.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cran {
    Descente,
    Remontee,
}

/// Le régulateur de l'encodeur — le pendant, côté streamer, du palier de
/// débit que le serveur demande pour un spectateur qui ne suit pas.
///
/// Une fois par seconde, il regarde si le pipeline tient la cadence
/// demandée : le temps par trame (conversion, encodage, et le décodage de
/// l'aperçu s'il est là) face au budget d'une trame, et les trames que la
/// capture jette faute d'encodeur libre. Cinq secondes de saturation, et
/// la diffusion descend d'un cran ; quinze secondes de repos ensuite, le
/// temps que les moyennes se refassent. Deux minutes de calme, et il tente
/// de remonter d'un cran — deux fois par diffusion au plus, et une
/// remontée qui sature à nouveau verrouille le cran pour la session : on
/// ne fait pas osciller les spectateurs. Un changement de réglages par le
/// streamer repart de zéro.
pub struct Regulateur {
    /// Les crans, du réglage au plus bas — connus à la première trame, la
    /// hauteur émise venant de la source.
    crans: Option<Vec<(u32, u32)>>,
    cran: usize,
    saturees: u32,
    calmes: u32,
    repos: u32,
    remontees: u32,
    remontee_en_cours: bool,
    remontee_depuis: u32,
    verrou: bool,
    derniere: Instant,
    /// Le compteur de trames sautées à la dernière seconde.
    sautees: u64,
}

impl Default for Regulateur {
    fn default() -> Self {
        Self::new()
    }
}

impl Regulateur {
    pub fn new() -> Self {
        Self {
            crans: None,
            cran: 0,
            saturees: 0,
            calmes: 0,
            repos: 0,
            remontees: 0,
            remontee_en_cours: false,
            remontee_depuis: 0,
            verrou: false,
            derniere: Instant::now(),
            sautees: 0,
        }
    }

    /// Le cran en vigueur s'il n'est pas le réglage : (hauteur, cadence).
    pub fn cran_actuel(&self) -> Option<(u32, u32)> {
        let crans = self.crans.as_ref()?;
        (self.cran > 0).then(|| crans[self.cran])
    }

    /// À appeler à chaque image pendant une diffusion : relève une fois
    /// par seconde, et rend un mot pour le streamer quand le cran change.
    /// `fps` et `preview` sont les réglages en vigueur.
    pub fn tick(&mut self, stats: &StageStats, fps: u32, preview: bool) -> Option<String> {
        if self.derniere.elapsed() < Duration::from_secs(1) {
            return None;
        }
        self.derniere = Instant::now();
        let (_, hauteur) = stats.dims();
        if hauteur == 0 {
            return None;
        }
        let n = self
            .crans
            .get_or_insert_with(|| paliers_encodeur(hauteur, fps))
            .len();
        let sautees = stats.skipped.load(Ordering::Relaxed);
        let delta = sautees.saturating_sub(self.sautees);
        self.sautees = sautees;
        if n < 2 {
            return None;
        }
        // La basse, quand elle tourne, passe par le même fil : son coût
        // compte, au prorata de sa cadence (30 i/s au plus).
        let basse = if stats.basse_dims().1 > 0 {
            stats.basse_ms.get() * (30.0 / fps.max(1) as f32).min(1.0)
        } else {
            0.0
        };
        let charge = stats.convert_ms.get()
            + stats.encode_ms.get()
            + if preview { stats.decode_ms.get() } else { 0.0 }
            + basse;
        let budget = 1000.0 / fps.max(1) as f32;
        let sature = charge > 0.9 * budget || delta as f32 > 0.15 * fps as f32;
        let decision = self.decider(sature)?;
        let (h, f) = self.crans.as_ref()?[self.cran];
        Some(match decision {
            Cran::Descente => format!(
                "l'encodeur ne suivait pas ({charge:.1} ms par trame pour {budget:.1}, \
                 {delta} trames sautées/s) : la diffusion passe en {h}p{f}"
            ),
            Cran::Remontee => format!("l'encodeur a de la marge : la diffusion remonte en {h}p{f}"),
        })
    }

    /// La machine à décider, une observation par seconde — sans horloge,
    /// pour se tester.
    pub fn decider(&mut self, sature: bool) -> Option<Cran> {
        let n = self.crans.as_ref().map(Vec::len)?;
        if sature {
            self.saturees += 1;
            self.calmes = 0;
        } else {
            self.calmes += 1;
            self.saturees = 0;
        }
        // Une remontée tient si une minute passe sans saturer.
        if self.remontee_en_cours {
            self.remontee_depuis += 1;
            if self.remontee_depuis >= 60 && !sature {
                self.remontee_en_cours = false;
            }
        }
        // Le repos : le temps que les moyennes se refassent au nouveau
        // cran, on ne compte rien — ni saturation, ni calme.
        if self.repos > 0 {
            self.repos -= 1;
            self.saturees = 0;
            self.calmes = 0;
            return None;
        }
        if self.saturees >= 5 && self.cran + 1 < n {
            self.cran += 1;
            self.repos = 15;
            self.saturees = 0;
            self.calmes = 0;
            if self.remontee_en_cours {
                self.remontee_en_cours = false;
                self.verrou = true;
            }
            return Some(Cran::Descente);
        }
        if self.calmes >= 120 && self.cran > 0 && !self.verrou && self.remontees < 2 {
            self.cran -= 1;
            self.repos = 15;
            self.calmes = 0;
            self.remontees += 1;
            self.remontee_en_cours = true;
            self.remontee_depuis = 0;
            return Some(Cran::Remontee);
        }
        None
    }
}

// ---------------------------------------------------------------------
// Regarder
// ---------------------------------------------------------------------

/// Un stream que l'on regarde.
pub struct Regard {
    pub stream_id: u32,
    /// Qui diffuse (pour le titre de la fenêtre).
    pub streamer: String,
    /// La dernière image décodée, déjà au format egui, prête à peindre.
    pub image: Arc<Mutex<Option<egui::ColorImage>>>,
    /// Images décodées depuis le début, pour la cadence affichée.
    pub images: Arc<AtomicU64>,
    /// On lit la qualité basse : le serveur nous y a mis, notre connexion
    /// ne suivait pas la haute.
    pub basse: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    /// Le fil du son du jeu, s'il a pu démarrer.
    audio_worker: Option<std::thread::JoinHandle<()>>,
}

impl Regard {
    /// Démarre le fil décodeur. `rx` reçoit les trames brutes que la couche
    /// réseau aiguille (`set_video_feed`), `audio_rx` les datagrammes de son
    /// du jeu (`set_game_audio_feed`), joués par le moteur vocal `engine`.
    pub fn demarrer(
        stream_id: u32,
        streamer: String,
        key: [u8; 32],
        rx: std_mpsc::Receiver<Vec<u8>>,
        audio_rx: std_mpsc::Receiver<bytes::Bytes>,
        engine: Arc<Mutex<Option<ki_voice::VoiceEngine>>>,
        ctx: egui::Context,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let image = Arc::new(Mutex::new(None));
        let images = Arc::new(AtomicU64::new(0));
        let basse = Arc::new(AtomicBool::new(false));
        // L'horloge du son : le fil du son y note où en est la lecture, le
        // fil de l'image retient chaque image jusqu'à cet instant-là.
        let horloge: Horloge = Arc::new(Mutex::new(None));
        let worker = {
            let (stop, image, images, horloge, basse) =
                (stop.clone(), image.clone(), images.clone(), horloge.clone(), basse.clone());
            std::thread::Builder::new()
                .name("video-regard".into())
                .spawn(move || fil_decodeur(stream_id, key, rx, image, images, stop, ctx, horloge, basse))
                .ok()
        };
        let audio_worker = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("audio-regard".into())
                .spawn(move || fil_audio(stream_id, key, audio_rx, engine, stop, horloge))
                .ok()
        };
        Self { stream_id, streamer, image, images, basse, stop, worker, audio_worker }
    }

    pub fn arreter(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
        if let Some(w) = self.audio_worker.take() {
            let _ = w.join();
        }
    }
}

/// Le fil du son du jeu chez le spectateur : déchiffre chaque datagramme
/// (même clé que la vidéo, domaine de nonce 2), décode, et verse dans la
/// sortie du moteur vocal — qui mixe, règle le volume et annule l'écho
/// comme pour tout le reste.
/// Où en est la lecture du son du jeu : l'horodatage (µs, base du streamer)
/// de ce qui sort de la carte son, et l'instant du relevé — entre deux
/// relevés, l'horloge avance d'elle-même.
type Horloge = Arc<Mutex<Option<(u64, std::time::Instant)>>>;

/// L'horodatage du son en cours de lecture, extrapolé depuis le dernier
/// relevé ; rien si le son s'est tu depuis plus d'une seconde et demie —
/// l'image ne doit alors attendre personne.
fn lire_horloge(horloge: &Horloge) -> Option<u64> {
    let releve = *horloge.lock().unwrap();
    releve.and_then(|(pts, quand)| {
        let age = quand.elapsed();
        (age < Duration::from_millis(1500)).then(|| pts + age.as_micros() as u64)
    })
}

fn fil_audio(
    stream_id: u32,
    key: [u8; 32],
    rx: std_mpsc::Receiver<bytes::Bytes>,
    engine: Arc<Mutex<Option<ki_voice::VoiceEngine>>>,
    stop: Arc<AtomicBool>,
    horloge: Horloge,
) {
    let cipher = XChaCha20Poly1305::new(&key.into());
    let mut lecteur = match ki_voice::jeu::Lecteur::new() {
        Ok(l) => l,
        Err(e) => {
            ki_video::journal(format!("son du stream indisponible : {e:#}"));
            return;
        }
    };
    let mut premier = true;
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(dat) => {
                let Some(h) = ki_protocol::parse_audio_header(&dat) else { continue };
                if h.stream_id != stream_id {
                    continue;
                }
                let (aad, sealed) = dat.split_at(ki_protocol::AUDIO_HEADER_LEN);
                let nonce = ki_protocol::nonce_for_media(
                    ki_protocol::MEDIA_DOMAIN_GAME_AUDIO,
                    stream_id,
                    h.seq,
                );
                let Ok(opus) =
                    cipher.decrypt(XNonce::from_slice(&nonce), Payload { msg: sealed, aad })
                else {
                    continue;
                };
                if premier {
                    premier = false;
                    ki_video::journal("visionnage : le son du jeu arrive".to_string());
                }
                if let Some(e) = engine.lock().unwrap().as_ref() {
                    lecteur.jouer(h.seq, &opus, e);
                    // Ce qui sort de la carte son en ce moment : cette trame
                    // finit dans 20 ms, moins tout ce qui attend devant elle.
                    let avance_us = e.aux_pending() as u64 * 1_000_000 / 48_000;
                    let en_lecture = (h.pts_us + 20_000).saturating_sub(avance_us);
                    *horloge.lock().unwrap() = Some((en_lecture, std::time::Instant::now()));
                }
            }
            Err(std_mpsc::RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
            }
            Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Au-delà de tant de trames en attente derrière un trou, on cesse
/// d'espérer : saut à la prochaine trame clé disponible, ou table rase.
const ATTENTE_MAX: usize = 30;

/// Les trames d'une qualité en attente de leur tour, par séquence :
/// (trame clé ?, horodatage, octets).
type Attente = BTreeMap<u64, (bool, u64, Vec<u8>)>;

/// La trame clé de l'autre qualité où passer, s'il y en a une : la
/// première en attente plus récente que ce qu'on a déjà lu. Une plus
/// ancienne, croisée en route (la petite double la grosse), ne ramène pas
/// en arrière.
fn cle_de_bascule(autre: &Attente, lu_pts: Option<u64>) -> Option<u64> {
    autre
        .iter()
        .find(|(_, (idr, pts, _))| *idr && lu_pts.is_none_or(|l| *pts > l))
        .map(|(s, _)| *s)
}

/// Le fil d'un spectateur : déchiffre, remet en ordre, décode.
///
/// Les trames arrivent dans le désordre — un flux QUIC chacune, les petites
/// doublent les grosses. La lecture ne démarre qu'à une trame clé, puis
/// avance strictement en séquence ; un trou qui s'éternise se règle en
/// sautant à la trame clé suivante (le serveur en a déjà demandé une si un
/// envoi nous a été sacrifié).
/// Une image décodée peut attendre le son jusqu'à cette avance ; au-delà,
/// on l'affiche quand même — mieux vaut un léger décalage qu'une image
/// figée si l'horloge du son déraille.
const TOLERANCE_US: u64 = 15_000;
/// Images décodées en attente d'affichage au plus (≈ 400 ms à 30 i/s).
const FILE_AFFICHAGE_MAX: usize = 12;

#[allow(clippy::too_many_arguments)]
fn fil_decodeur(
    stream_id: u32,
    key: [u8; 32],
    rx: std_mpsc::Receiver<Vec<u8>>,
    image: Arc<Mutex<Option<egui::ColorImage>>>,
    images: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    ctx: egui::Context,
    horloge: Horloge,
    basse: Arc<AtomicBool>,
) {
    let cipher = XChaCha20Poly1305::new(&key.into());
    let Ok(mut decodeur) = ViewerDecoder::new() else {
        ki_video::journal("visionnage impossible : décodeur H.264 du spectateur indisponible");
        return;
    };
    let depart = std::time::Instant::now();
    let mut premiere = true;
    // Les trames déchiffrées en attente de leur tour, par qualité (haute,
    // basse) puis par séquence : (trame clé ?, horodatage, octets). Deux
    // qualités, deux séquences — elles ne se mélangent jamais. Celles de
    // l'autre qualité attendent aussi : après une bascule, les petites
    // trames qui suivent la trame clé arrivent souvent avant elle.
    let mut attentes: [Attente; 2] = [BTreeMap::new(), BTreeMap::new()];
    let mut prochaine: Option<u64> = None;
    // La qualité qu'on lit (0 : haute, 1 : basse), et l'horodatage de la
    // dernière image décodée.
    let mut couche = 0usize;
    let mut lu_pts: Option<u64> = None;
    // Les images décodées qui attendent leur instant sur l'horloge du son.
    let mut a_afficher: std::collections::VecDeque<(u64, egui::ColorImage)> =
        std::collections::VecDeque::new();

    loop {
        // Une image en attente : on se réveille souvent pour la poser à
        // l'heure ; sinon, au rythme des trames.
        let delai = if a_afficher.is_empty() { 200 } else { 5 };
        match rx.recv_timeout(Duration::from_millis(delai)) {
            Ok(bytes) => {
                if let Some(h) = ki_protocol::parse_media_header(&bytes) {
                    if h.stream_id == stream_id {
                        let domaine = if h.basse {
                            ki_protocol::MEDIA_DOMAIN_VIDEO_BASSE
                        } else {
                            ki_protocol::MEDIA_DOMAIN_VIDEO
                        };
                        let nonce = ki_protocol::nonce_for_media(domaine, stream_id, h.seq);
                        // L'en-tête est l'AAD : altéré en route, le tag le
                        // trahit.
                        let (aad, sealed) = bytes.split_at(ki_protocol::MEDIA_HEADER_LEN);
                        if let Ok(clair) = cipher
                            .decrypt(XNonce::from_slice(&nonce), Payload { msg: sealed, aad })
                        {
                            attentes[usize::from(h.basse)].insert(h.seq, (h.idr, h.pts_us, clair));
                        }
                    }
                }
            }
            Err(std_mpsc::RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
            }
            Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
        }

        // Le serveur nous a changé de qualité : une trame clé de l'autre,
        // plus récente que ce qu'on a déjà lu, et l'on y passe — la lecture
        // repart d'elle.
        let autre = 1 - couche;
        if let Some(s) = cle_de_bascule(&attentes[autre], lu_pts) {
            couche = autre;
            attentes[1 - autre].clear();
            attentes[autre].retain(|k, _| *k >= s);
            prochaine = Some(s);
            basse.store(autre == 1, Ordering::Relaxed);
        } else if attentes[autre].len() > 2 * ATTENTE_MAX {
            attentes[autre].clear();
        }
        let attente = &mut attentes[couche];

        // Point de départ : la première trame clé vue. Tout ce qui la
        // précède est indécodable — jeté sans regret.
        if prochaine.is_none() {
            if let Some(s) = attente
                .iter()
                .find(|(_, (idr, _, _))| *idr)
                .map(|(s, _)| *s)
            {
                attente.retain(|k, _| *k >= s);
                prochaine = Some(s);
            } else if attente.len() > 2 * ATTENTE_MAX {
                attente.clear();
            }
        }

        if let Some(mut next) = prochaine {
            loop {
                // Tout ce qui est contigu part au décodeur, dans l'ordre.
                while let Some((_, pts, clair)) = attente.remove(&next) {
                    lu_pts = Some(pts);
                    if let Some(frame) = decodeur.decode(&clair) {
                        // La conversion RGBA -> image egui (8 Mo en 1080p)
                        // se paie ici, pas sur le fil d'interface.
                        let prete = egui::ColorImage::from_rgba_unmultiplied(
                            [frame.width, frame.height],
                            &frame.rgba,
                        );
                        a_afficher.push_back((pts, prete));
                    }
                    next = next.wrapping_add(1);
                }

                // Une trame clé en attente au-delà d'un trou : on y saute
                // tout de suite. Elle remet le décodeur à neuf, et les trames
                // manquantes d'avant ne serviraient à rien — le serveur les
                // annule d'ailleurs à chaque trame clé quand le lien ne suit
                // pas. Attendre (c'était trente trames) ne faisait que geler
                // l'image une seconde.
                match attente
                    .iter()
                    .find(|(k, (idr, _, _))| **k > next && *idr)
                    .map(|(s, _)| *s)
                {
                    Some(s) => {
                        attente.retain(|k, _| *k >= s);
                        next = s;
                    }
                    None => break,
                }
            }

            // Un trou qui s'éternise sans trame clé en réserve : table
            // rase, la prochaine trame clé relancera la lecture.
            if attente.len() > ATTENTE_MAX {
                attente.clear();
                prochaine = None;
            } else {
                prochaine = Some(next);
            }
        }

        // Présentation : chaque image attend que le son en soit au même
        // point ; sans son (ou son tari), tout de suite. Une file qui
        // déborde s'affiche quand même — l'horloge ne doit jamais figer
        // l'image.
        let maintenant = lire_horloge(&horloge);
        while let Some((pts, _)) = a_afficher.front() {
            let due = match maintenant {
                Some(h) => *pts <= h + TOLERANCE_US,
                None => true,
            } || a_afficher.len() > FILE_AFFICHAGE_MAX;
            if !due {
                break;
            }
            let (_, prete) = a_afficher.pop_front().expect("file non vide");
            if premiere {
                premiere = false;
                ki_video::journal(format!(
                    "visionnage : première image {}x{} après {} ms",
                    prete.size[0],
                    prete.size[1],
                    depart.elapsed().as_millis()
                ));
            }
            *image.lock().unwrap() = Some(prete);
            images.fetch_add(1, Ordering::Relaxed);
            // Seul moyen de peindre au rythme du stream : la boucle de
            // repeint de l'application est plafonnée à 20 fps sinon.
            ctx.request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_crans_de_l_encodeur_descendent_la_cadence_puis_la_hauteur() {
        assert_eq!(paliers_encodeur(1080, 60), vec![(1080, 60), (1080, 30), (720, 30)]);
        assert_eq!(
            paliers_encodeur(1440, 60),
            vec![(1440, 60), (1440, 30), (1080, 30), (720, 30)]
        );
        assert_eq!(paliers_encodeur(1080, 30), vec![(1080, 30), (720, 30)]);
        assert_eq!(paliers_encodeur(720, 60), vec![(720, 60), (720, 30)]);
        // Rien à descendre : 720p30 et moins restent tels quels.
        assert_eq!(paliers_encodeur(720, 30), vec![(720, 30)]);
        assert_eq!(paliers_encodeur(480, 30), vec![(480, 30)]);
    }

    #[test]
    fn le_regulateur_descend_apres_cinq_secondes_et_verrouille_une_remontee_ratee() {
        let mut r = Regulateur::new();
        r.crans = Some(paliers_encodeur(1080, 60));
        assert_eq!(r.cran_actuel(), None);
        // Quatre secondes ne suffisent pas, la cinquième descend.
        for _ in 0..4 {
            assert_eq!(r.decider(true), None);
        }
        assert_eq!(r.decider(true), Some(Cran::Descente));
        assert_eq!(r.cran_actuel(), Some((1080, 30)));
        // Quinze secondes de repos, saturées ou non : rien ne bouge.
        for _ in 0..15 {
            assert_eq!(r.decider(true), None);
        }
        for _ in 0..4 {
            assert_eq!(r.decider(true), None);
        }
        assert_eq!(r.decider(true), Some(Cran::Descente));
        assert_eq!(r.cran_actuel(), Some((720, 30)));
        // Tout en bas : saturer encore ne descend plus.
        for _ in 0..40 {
            assert_eq!(r.decider(true), None);
        }
        // Deux minutes de calme (après le repos), et une remontée.
        let mut remontee = None;
        for i in 0..200 {
            if let Some(c) = r.decider(false) {
                remontee = Some((i, c));
                break;
            }
        }
        assert_eq!(remontee.map(|(_, c)| c), Some(Cran::Remontee));
        // Le repos a été consommé par les quarante secondes du bas : la
        // cent-vingtième seconde de calme est la bonne.
        assert_eq!(remontee.map(|(i, _)| i), Some(119));
        assert_eq!(r.cran_actuel(), Some((1080, 30)));
        // La remontée sature aussitôt : on redescend, et plus jamais de
        // remontée — les spectateurs ne font pas le yo-yo.
        let redescente: Vec<Cran> = (0..30).filter_map(|_| r.decider(true)).collect();
        assert_eq!(redescente, vec![Cran::Descente]);
        assert_eq!(r.cran_actuel(), Some((720, 30)));
        assert!(r.verrou);
        assert!((0..400).filter_map(|_| r.decider(false)).next().is_none());
    }

    #[test]
    fn une_remontee_qui_tient_une_minute_ne_verrouille_pas_la_suivante() {
        let mut r = Regulateur::new();
        r.crans = Some(paliers_encodeur(1080, 60));
        // Deux crans plus bas, vite fait.
        let descentes: Vec<Cran> = (0..60).filter_map(|_| r.decider(true)).collect();
        assert_eq!(descentes, vec![Cran::Descente, Cran::Descente]);
        // Une remontée qui tient : une minute de calme la confirme, la
        // saturation d'après est une descente ordinaire, sans verrou.
        let remontee: Vec<Cran> = (0..200).filter_map(|_| r.decider(false)).collect();
        assert_eq!(remontee, vec![Cran::Remontee]);
        assert!(!r.remontee_en_cours);
        let descente: Vec<Cran> = (0..30).filter_map(|_| r.decider(true)).collect();
        assert_eq!(descente, vec![Cran::Descente]);
        assert!(!r.verrou);
        // Une seconde remontée reste permise — la troisième, non.
        let remontee: Vec<Cran> = (0..200).filter_map(|_| r.decider(false)).collect();
        assert_eq!(remontee, vec![Cran::Remontee]);
        let _ = (0..30).filter_map(|_| r.decider(true)).count();
        assert!((0..400).filter_map(|_| r.decider(false)).next().is_none());
    }

    /// La résolution et la cadence suivent le débit, sans jamais dépasser le
    /// réglage : 1080p60 à 8 Mbit/s, 720p30 à 1,5, 360p30 tout en bas.
    #[test]
    fn la_resolution_suit_le_debit() {
        assert_eq!(ki_video::qualite_pour_debit(8000), None);
        assert_eq!(ki_video::qualite_pour_debit(4000), Some((1080, 30)));
        assert_eq!(ki_video::qualite_pour_debit(2500), Some((720, 30)));
        assert_eq!(ki_video::qualite_pour_debit(1500), Some((720, 30)));
        assert_eq!(ki_video::qualite_pour_debit(1000), Some((540, 30)));
        assert_eq!(ki_video::qualite_pour_debit(700), Some((480, 30)));
        assert_eq!(ki_video::qualite_pour_debit(450), Some((360, 30)));
        assert_eq!(plafonner_hauteur(0, 720), 720, "« native » prend le plafond");
        assert_eq!(plafonner_hauteur(480, 720), 480, "un réglage plus bas reste");
        assert_eq!(plafonner_hauteur(1080, 720), 720);
        // La basse ne dépasse jamais la haute, ni 30 i/s.
        assert_eq!(ki_video::qualite_basse(2500, 1080, 60), (720, 30));
        assert_eq!(ki_video::qualite_basse(2500, 480, 60), (480, 30));
        assert_eq!(ki_video::qualite_basse(1000, 1080, 15), (540, 15));
    }

    /// Une bascule de qualité se fait à la trame clé de l'autre, si elle est
    /// plus récente que ce qu'on a lu — jamais sur une trame ordinaire, ni
    /// sur une trame clé périmée croisée en route.
    #[test]
    fn la_bascule_de_qualite_attend_une_trame_cle_recente() {
        let mut autre: Attente = BTreeMap::new();
        autre.insert(5, (false, 90, vec![]));
        assert_eq!(cle_de_bascule(&autre, Some(80)), None, "pas sans trame clé");
        autre.insert(6, (true, 100, vec![]));
        assert_eq!(cle_de_bascule(&autre, Some(80)), Some(6));
        assert_eq!(cle_de_bascule(&autre, None), Some(6), "au départ, n'importe laquelle");
        assert_eq!(cle_de_bascule(&autre, Some(150)), None, "périmée : on ne revient pas en arrière");
    }

    /// Le chiffrement d'une trame telle que l'émetteur la fabrique doit se
    /// déchiffrer telle que le spectateur la lit — en-tête en AAD compris :
    /// un octet d'en-tête réécrit par le chemin invalide le tag.
    #[test]
    fn une_trame_chiffree_fait_l_aller_retour() {
        let key = [9u8; 32];
        let cipher = XChaCha20Poly1305::new(&key.into());
        let h = ki_protocol::MediaHeader {
            idr: true,
            basse: false,
            stream_id: 3,
            seq: 41,
            pts_us: 1_000_000,
            group_id: 2,
            width: 1280,
            height: 720,
        };
        let mut head = [0u8; ki_protocol::MEDIA_HEADER_LEN];
        ki_protocol::write_media_header(&mut head, &h);
        let nonce = ki_protocol::nonce_for_media(ki_protocol::MEDIA_DOMAIN_VIDEO, 3, 41);
        let sealed = cipher
            .encrypt(XNonce::from_slice(&nonce), Payload { msg: b"nal factice", aad: &head })
            .unwrap();

        let clair = cipher
            .decrypt(XNonce::from_slice(&nonce), Payload { msg: &sealed, aad: &head })
            .unwrap();
        assert_eq!(clair, b"nal factice");

        // En-tête trafiqué (le relais réécrirait la séquence ?) : rejet.
        let mut faux = head;
        faux[8] ^= 1;
        assert!(cipher
            .decrypt(XNonce::from_slice(&nonce), Payload { msg: &sealed, aad: &faux })
            .is_err());
    }

    /// Les réglages survivent à l'aller-retour par le stockage, la source
    /// « fenêtre » avec son titre — deux-points compris.
    #[test]
    fn les_reglages_font_l_aller_retour_par_le_stockage() {
        let r = Reglages {
            source: CaptureSource::Window("Valorant : partie classée".into()),
            max_height: 720,
            fps: 60,
            kbps: 8000,
            cursor: false,
            preview: false,
            encodeur: EncoderChoice::Nvenc,
            son: false,
        };

        let mut cle_valeur = std::collections::HashMap::new();
        struct Memoire<'a>(&'a mut std::collections::HashMap<String, String>);
        impl eframe::Storage for Memoire<'_> {
            fn get_string(&self, key: &str) -> Option<String> {
                self.0.get(key).cloned()
            }
            fn set_string(&mut self, key: &str, value: String) {
                self.0.insert(key.to_string(), value);
            }
            fn flush(&mut self) {}
        }
        r.save(&mut Memoire(&mut cle_valeur));
        let relu = Reglages::load(|k, d| cle_valeur.get(k).cloned().unwrap_or_else(|| d.to_string()));
        assert_eq!(relu, r);

        // Un stockage vide donne les défauts.
        let defaut = Reglages::load(|_, d| d.to_string());
        assert_eq!(defaut, Reglages::default());
        assert_eq!(defaut.meta().fps, 30);
        assert_eq!(defaut.config().bitrate_bps, 6_000_000);
    }

    /// La cadence se mesure sur la dernière seconde, et une boucle
    /// relancée (compteurs repartis de zéro) ne donne pas de valeur absurde.
    #[test]
    fn la_cadence_se_deduit_des_compteurs() {
        let mut c = Cadence::new();
        c.relever(30, 100_000);
        assert_eq!(c.fps, 0.0, "pas avant une seconde");
        // Les compteurs partent de zéro à la création : 60 trames et
        // 300 ko deux secondes plus tard, c'est 30 i/s à 1 200 kbit/s.
        c.depuis = Instant::now() - Duration::from_secs(2);
        c.relever(60, 300_000);
        assert!((c.fps - 30.0).abs() < 1.0, "{}", c.fps);
        assert!((c.kbps - 1200.0).abs() < 50.0, "{}", c.kbps);
        c.depuis = Instant::now() - Duration::from_secs(1);
        c.relever(5, 1000);
        assert!((c.fps - 30.0).abs() < 1.0, "un compteur qui recule garde la dernière mesure");
        c.depuis = Instant::now() - Duration::from_secs(1);
        c.relever(65, 151_000);
        assert!((c.fps - 60.0).abs() < 2.0, "et la mesure suivante repart de la nouvelle base");
    }
}
