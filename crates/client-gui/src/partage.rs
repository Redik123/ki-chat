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
    CaptureSource, FrameEmit, FrameSink, MonitorInfo, RgbaFrame, StageStats, StreamConfig,
    StreamerLoop, ViewerDecoder, WindowInfo,
};

use crate::theme::{TEXT, TEXT_DIM};
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
        }
    }
}

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
    }

    pub fn config(&self) -> StreamConfig {
        StreamConfig {
            source: self.source.clone(),
            max_height: self.max_height,
            fps: self.fps,
            bitrate_bps: self.kbps.saturating_mul(1000),
            cursor: self.cursor,
            preview: self.preview,
        }
    }

    /// Ce qu'on annonce au salon — les dimensions viendront des trames.
    pub fn meta(&self) -> StreamMeta {
        StreamMeta { width: 0, height: 0, fps: self.fps.min(255) as u8, kbps: self.kbps }
    }
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
    /// L'aperçu local — exactement ce que les spectateurs reçoivent.
    pub apercu: Arc<Mutex<Option<RgbaFrame>>>,
    pub emit: FrameEmit,
    pub sink: FrameSink,
    pub force_idr: Arc<AtomicBool>,
    /// Cadence et débit annoncés au salon, lus par la couche réseau quand
    /// les dimensions changent.
    pub cadence: Arc<Mutex<StreamMeta>>,
    /// Les réglages en vigueur.
    pub reglages: Reglages,
}

impl GoLive {
    pub fn arreter(self) {
        self.boucle.stop();
    }

    /// Relance la capture avec d'autres réglages, le stream restant le même.
    /// L'ancienne boucle s'arrête d'abord : deux encodeurs qui se relaient
    /// sur la même séquence donneraient un salmigondis au décodeur.
    pub fn reconfigurer(self, reglages: &Reglages) -> anyhow::Result<Self> {
        let Self { boucle, stats, stream_id, apercu, emit, sink, force_idr, cadence, .. } = self;
        boucle.stop();
        *apercu.lock().unwrap() = None;
        let boucle = StreamerLoop::start(
            stats.clone(),
            sink.clone(),
            emit.clone(),
            reglages.config(),
            force_idr.clone(),
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

// ---------------------------------------------------------------------
// Regarder
// ---------------------------------------------------------------------

/// Un stream que l'on regarde.
pub struct Regard {
    pub stream_id: u32,
    /// Qui diffuse (pour le titre de la fenêtre).
    pub streamer: String,
    /// La dernière image décodée, prête à peindre.
    pub image: Arc<Mutex<Option<RgbaFrame>>>,
    /// Images décodées depuis le début, pour la cadence affichée.
    pub images: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Regard {
    /// Démarre le fil décodeur. `rx` reçoit les trames brutes que la couche
    /// réseau aiguille (`set_video_feed`).
    pub fn demarrer(
        stream_id: u32,
        streamer: String,
        key: [u8; 32],
        rx: std_mpsc::Receiver<Vec<u8>>,
        ctx: egui::Context,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let image = Arc::new(Mutex::new(None));
        let images = Arc::new(AtomicU64::new(0));
        let worker = {
            let (stop, image, images) = (stop.clone(), image.clone(), images.clone());
            std::thread::Builder::new()
                .name("video-regard".into())
                .spawn(move || fil_decodeur(stream_id, key, rx, image, images, stop, ctx))
                .ok()
        };
        Self { stream_id, streamer, image, images, stop, worker }
    }

    pub fn arreter(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// Au-delà de tant de trames en attente derrière un trou, on cesse
/// d'espérer : saut à la prochaine trame clé disponible, ou table rase.
const ATTENTE_MAX: usize = 30;

/// Le fil d'un spectateur : déchiffre, remet en ordre, décode.
///
/// Les trames arrivent dans le désordre — un flux QUIC chacune, les petites
/// doublent les grosses. La lecture ne démarre qu'à une trame clé, puis
/// avance strictement en séquence ; un trou qui s'éternise se règle en
/// sautant à la trame clé suivante (le serveur en a déjà demandé une si un
/// envoi nous a été sacrifié).
fn fil_decodeur(
    stream_id: u32,
    key: [u8; 32],
    rx: std_mpsc::Receiver<Vec<u8>>,
    image: Arc<Mutex<Option<RgbaFrame>>>,
    images: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    ctx: egui::Context,
) {
    let cipher = XChaCha20Poly1305::new(&key.into());
    let Ok(mut decodeur) = ViewerDecoder::new() else {
        tracing::error!("décodeur du spectateur indisponible");
        return;
    };
    // Les trames déchiffrées en attente de leur tour, par séquence.
    let mut attente: BTreeMap<u64, (bool, Vec<u8>)> = BTreeMap::new();
    let mut prochaine: Option<u64> = None;

    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(bytes) => {
                let Some(h) = ki_protocol::parse_media_header(&bytes) else { continue };
                if h.stream_id != stream_id {
                    continue;
                }
                let nonce = ki_protocol::nonce_for_media(
                    ki_protocol::MEDIA_DOMAIN_VIDEO,
                    stream_id,
                    h.seq,
                );
                // L'en-tête est l'AAD : altéré en route, le tag le trahit.
                let (aad, sealed) = bytes.split_at(ki_protocol::MEDIA_HEADER_LEN);
                let Ok(clair) =
                    cipher.decrypt(XNonce::from_slice(&nonce), Payload { msg: sealed, aad })
                else {
                    continue;
                };
                attente.insert(h.seq, (h.idr, clair));
            }
            Err(std_mpsc::RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                continue;
            }
            Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
        }

        // Point de départ : la première trame clé vue. Tout ce qui la
        // précède est indécodable — jeté sans regret.
        if prochaine.is_none() {
            if let Some(s) = attente
                .iter()
                .find(|(_, (idr, _))| *idr)
                .map(|(s, _)| *s)
            {
                attente.retain(|k, _| *k >= s);
                prochaine = Some(s);
            } else {
                if attente.len() > 2 * ATTENTE_MAX {
                    attente.clear();
                }
                continue;
            }
        }
        let Some(mut next) = prochaine else { continue };

        // Tout ce qui est contigu part au décodeur, dans l'ordre.
        while let Some((_, clair)) = attente.remove(&next) {
            if let Some(frame) = decodeur.decode(&clair) {
                *image.lock().unwrap() = Some(frame);
                images.fetch_add(1, Ordering::Relaxed);
                // Seul moyen de peindre au rythme du stream : la boucle de
                // repeint de l'application est plafonnée à 20 fps sinon.
                ctx.request_repaint();
            }
            next = next.wrapping_add(1);
        }

        // Un trou qui s'éternise : on saute à la prochaine trame clé plutôt
        // que d'attendre une trame qui ne viendra peut-être jamais.
        if attente.len() > ATTENTE_MAX {
            if let Some(s) = attente
                .iter()
                .find(|(k, (idr, _))| **k > next && *idr)
                .map(|(s, _)| *s)
            {
                attente.retain(|k, _| *k >= s);
                next = s;
            } else {
                // Rien de décodable en réserve : table rase, la prochaine
                // trame clé relancera la lecture.
                attente.clear();
                prochaine = None;
                continue;
            }
        }
        prochaine = Some(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Le chiffrement d'une trame telle que l'émetteur la fabrique doit se
    /// déchiffrer telle que le spectateur la lit — en-tête en AAD compris :
    /// un octet d'en-tête réécrit par le chemin invalide le tag.
    #[test]
    fn une_trame_chiffree_fait_l_aller_retour() {
        let key = [9u8; 32];
        let cipher = XChaCha20Poly1305::new(&key.into());
        let h = ki_protocol::MediaHeader {
            idr: true,
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
