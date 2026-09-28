//! Pipeline vidéo du partage d'écran — voir PLAN-STREAM.md.
//!
//! Deux boucles sur les mêmes briques : celle du labo (capture -> I420 ->
//! H.264 -> décodage -> image RGBA, zéro réseau) et la boucle streamer — la
//! même, qui sait en plus réduire l'image avant l'encodeur et livre chaque
//! trame encodée à la couche réseau. Le tout instrumenté étage par étage
//! (`StageStats`). Le transport, lui, vit ailleurs : ce crate ne connaît
//! que les pixels et le codec.

pub mod capture;
#[cfg(windows)]
mod clip_gpu;
#[cfg(windows)]
mod diffusion_gpu;
#[cfg(windows)]
mod nvenc;
#[cfg(windows)]
mod nvenc_ffi;
pub mod scale;
pub mod stats;

pub use capture::{list_monitors, list_windows, CaptureSource, MonitorInfo, WindowInfo};
#[cfg(windows)]
pub use clip_gpu::ClipGpu;
pub use nvenc::{avertissement_pilote, inventaire, inventaire_lancer, inventaire_pret, sonde};
pub use stats::StageStats;

/// Les réglages de la chaîne des clips sur la carte (`ClipGpu`).
#[derive(Clone, Debug)]
pub struct ConfigClip {
    pub source: CaptureSource,
    /// Hauteur plafond de l'image enregistrée (0 : celle de l'écran).
    pub hauteur_max: u32,
    pub fps: u32,
    pub debit_bps: u32,
    pub curseur: bool,
    /// Longueur du groupe d'images, en secondes : le clip se coupe à la
    /// trame clé.
    pub gop_s: u32,
}

/// Hors de Windows, pas de chaîne sur la carte : l'enregistreur prend le
/// chemin du processeur (qui, lui-même, dit qu'il ne sait pas capturer).
#[cfg(not(windows))]
pub struct ClipGpu;

#[cfg(not(windows))]
impl ClipGpu {
    pub fn demarrer(
        _config: ConfigClip,
        _stats: std::sync::Arc<StageStats>,
        _emit: FrameEmit,
        _origine: std::time::Instant,
    ) -> anyhow::Result<Self> {
        anyhow::bail!("chaîne des clips sur la carte : Windows seulement")
    }

    pub fn source_fermee(&self) -> bool {
        false
    }

    pub fn en_panne(&self) -> bool {
        true
    }

    pub fn arreter(self) {}
}

/// NVENC est l'encodeur des cartes NVIDIA **sous Windows** (Direct3D 11 en
/// dessous). Ailleurs, l'inventaire matériel se réduit à ce que l'on sait :
/// rien — et le pipeline prend l'encodeur logiciel sans poser de question.
#[cfg(not(windows))]
mod nvenc {
    /// L'inventaire, en une ligne : la même forme que sous Windows, pour que
    /// les rapports se lisent pareil.
    pub fn inventaire() -> String {
        format!("cartes graphiques : non relevées sur {} ; NVENC indisponible (Windows seulement)", std::env::consts::OS)
    }

    /// Rien à relever sur un fil à part : l'inventaire est immédiat.
    pub fn inventaire_lancer() {}

    /// Pas de NVENC à sonder ici.
    pub fn sonde() -> String {
        format!("NVENC indisponible sur {} (Windows seulement)", std::env::consts::OS)
    }

    pub fn inventaire_pret() -> Option<&'static str> {
        static INVENTAIRE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        Some(INVENTAIRE.get_or_init(inventaire).as_str())
    }

    /// Pas de pilote NVIDIA à surveiller ailleurs que sous Windows.
    pub fn avertissement_pilote() -> Option<String> {
        None
    }
}

/// Le journal partagé de l'application, branché par l'interface : ce que la
/// vidéo a d'important à dire (encodeur retenu, capture bridée par le
/// Windows de la machine, source perdue) y part en plus des traces, pour
/// voyager avec les diagnostics.
static JOURNAL: std::sync::OnceLock<Box<dyn Fn(String) + Send + Sync>> =
    std::sync::OnceLock::new();

/// Branche le journal de l'application. Une seule fois ; les appels
/// suivants sont ignorés.
pub fn set_journal(sink: impl Fn(String) + Send + Sync + 'static) {
    let _ = JOURNAL.set(Box::new(sink));
}

/// Une ligne pour le journal partagé — et pour les traces.
pub fn journal(msg: impl Into<String>) {
    let msg = msg.into();
    tracing::info!("{msg}");
    if let Some(j) = JOURNAL.get() {
        j(msg);
    }
}

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use openh264::decoder::Decoder;
use openh264::encoder::{
    BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, UsageType,
};
use openh264::formats::{BgraSliceU8, YUVBuffer, YUVSource};
use openh264::OpenH264API;

/// Image décodée prête pour l'affichage (RGBA 8 bits serré).
pub struct RgbaFrame {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// Réceptacle d'images côté UI : la boucle y dépose chaque frame décodée.
pub type FrameSink = Arc<dyn Fn(RgbaFrame) + Send + Sync>;

/// Une trame encodée, telle que l'encodeur la rend.
pub struct Paquet {
    pub data: Vec<u8>,
    pub idr: bool,
}

/// Un encodeur H.264, logiciel ou matériel : des images I420 entrent, des
/// trames Annex B sortent. `None` : l'encodeur a sauté la trame.
pub trait VideoEncoder {
    fn nom(&self) -> &'static str;
    fn encode(&mut self, src: &dyn YUVSource, force_idr: bool)
        -> anyhow::Result<Option<Paquet>>;
}

/// Quel encodeur : le matériel s'il existe, ou imposé, ou le logiciel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EncoderChoice {
    #[default]
    Auto,
    Nvenc,
    Logiciel,
}

/// L'encodeur logiciel (openh264), derrière le trait commun.
struct Logiciel(Encoder);

/// Pont entre `&dyn YUVSource` et l'API générique d'openh264.
struct Dyn<'a>(&'a dyn YUVSource);

impl YUVSource for Dyn<'_> {
    fn dimensions(&self) -> (usize, usize) {
        self.0.dimensions()
    }

    fn strides(&self) -> (usize, usize, usize) {
        self.0.strides()
    }

    fn y(&self) -> &[u8] {
        self.0.y()
    }

    fn u(&self) -> &[u8] {
        self.0.u()
    }

    fn v(&self) -> &[u8] {
        self.0.v()
    }
}

impl VideoEncoder for Logiciel {
    fn nom(&self) -> &'static str {
        "logiciel"
    }

    fn encode(
        &mut self,
        src: &dyn YUVSource,
        force_idr: bool,
    ) -> anyhow::Result<Option<Paquet>> {
        if force_idr {
            self.0.force_intra_frame();
        }
        let bs = self.0.encode(&Dyn(src)).map_err(|e| anyhow::anyhow!("openh264 : {e}"))?;
        Ok(match bs.frame_type() {
            FrameType::Skip => None,
            t => Some(Paquet { data: bs.to_vec(), idr: t == FrameType::IDR }),
        })
    }
}

/// Ce que l'on encode : une diffusion, où chaque trame part sur le réseau
/// et où le débit se tient à la trame près ; ou un clip, où seule l'image
/// compte et où le débit peut varier avec elle.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Profil {
    #[default]
    Diffusion,
    Clip,
}

/// Crée l'encodeur demandé. En « Auto », NVENC si la machine l'offre, le
/// logiciel sinon — et l'on dit lequel, au journal comme aux stats.
///
/// `tentative` compte les échecs NVENC précédents sur ce flux : à zéro on
/// entre par la texture (le chemin standard), ensuite par le tampon
/// historique — deux pilotes différents ne refusent pas la même chose.
#[allow(clippy::too_many_arguments)]
pub fn creer_encodeur(
    choix: EncoderChoice,
    width: u32,
    height: u32,
    bitrate_bps: u32,
    fps: u32,
    gop_s: u32,
    profil: Profil,
    stats: &StageStats,
    tentative: u32,
) -> anyhow::Result<Box<dyn VideoEncoder>> {
    // NVENC n'existe que sous Windows ; ailleurs, l'exiger est une erreur
    // franche, et « Auto » veut simplement dire « logiciel ».
    #[cfg(not(windows))]
    let _ = (tentative, profil);
    #[cfg(not(windows))]
    if choix == EncoderChoice::Nvenc {
        anyhow::bail!("NVENC exigé par les réglages, mais indisponible sur {}", std::env::consts::OS);
    }
    #[cfg(windows)]
    if choix != EncoderChoice::Logiciel {
        let entree = if tentative == 0 { nvenc::Entree::Texture } else { nvenc::Entree::Tampon };
        // Le profil « clip » demande plus à la carte (deux passes en pleine
        // résolution, AQ temporelle) : si elle refuse, celui de la diffusion
        // vaut toujours mieux que le logiciel.
        let ouverture = nvenc::Nvenc::avec_entree_gop(width, height, bitrate_bps, fps, gop_s, entree, profil)
            .or_else(|e| {
                if profil == Profil::Clip {
                    journal(format!("NVENC : profil clip refusé ({e:#}) — profil diffusion à la place"));
                    nvenc::Nvenc::avec_entree_gop(width, height, bitrate_bps, fps, gop_s, entree, Profil::Diffusion)
                } else {
                    Err(e)
                }
            });
        match ouverture {
            Ok(e) => {
                let (maj, min) = e.version_pilote();
                let chemin = match e.entree() {
                    nvenc::Entree::Texture => "texture",
                    nvenc::Entree::Tampon => "tampon",
                };
                journal(format!(
                    "encodeur : NVENC sur {} (API {maj}.{min}), {width}x{height} à {fps} i/s, \
                     {} kbit/s, entrée par {chemin}",
                    e.carte,
                    bitrate_bps / 1000
                ));
                stats.materiel.store(true, Ordering::Relaxed);
                return Ok(Box::new(e));
            }
            Err(e) if choix == EncoderChoice::Nvenc => {
                return Err(e.context("NVENC exigé par les réglages"));
            }
            Err(e) => {
                journal(format!("NVENC indisponible ({e:#}) : encodeur logiciel"));
                // Un pilote trop vieux se dit à la personne qui diffuse, pas
                // seulement au journal : c'est elle qui peut y remédier.
                let raison = format!("{e:#}");
                if raison.contains("trop ancien") {
                    stats.poser_avis(format!("{raison} — en attendant, encodage logiciel"));
                }
            }
        }
    }
    stats.materiel.store(false, Ordering::Relaxed);
    journal(format!(
        "encodeur : logiciel (openh264), {width}x{height} à {fps} i/s, {} kbit/s",
        bitrate_bps / 1000
    ));
    Ok(Box::new(Logiciel(screen_encoder(width, height, bitrate_bps, fps, gop_s)?)))
}

/// Boucle locale S1a : capture écran -> I420 -> H.264 -> décodage -> sink.
///
/// Threads : la capture vit sur le thread de windows-capture (rappel léger),
/// tout le travail (conversion, encodage, décodage) sur UN thread pipeline
/// dédié — jamais sur le thread réseau ni sur l'UI.
pub struct LocalLoop {
    /// `None` une fois arrêtées : l'arrêt passe aussi par `Drop`.
    control: Option<capture::Control>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl LocalLoop {
    pub fn start(stats: Arc<StageStats>, sink: FrameSink) -> anyhow::Result<Self> {
        stats.mark_started();
        // Trames capturées -> pipeline (borné à 1 : on saute plutôt que
        // d'accumuler du retard) ; tampons recyclés dans l'autre sens.
        let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<capture::CapturedFrame>(1);
        let (recycle_tx, recycle_rx) = std::sync::mpsc::channel::<Vec<u8>>();

        let control = capture::start_capture(
            &CaptureSource::Monitor(0),
            true,
            30,
            capture::CaptureFlags {
                stats: stats.clone(),
                tx: frame_tx,
                recycle: recycle_rx,
                closed: Arc::new(AtomicBool::new(false)),
                interval: Duration::ZERO,
            },
        )?;

        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("video-pipeline".into())
                .spawn(move || pipeline_loop(stats, sink, frame_rx, recycle_tx, stop))
                .context("thread pipeline vidéo")?
        };

        Ok(Self { control: Some(control), stop, worker: Some(worker) })
    }

    pub fn stop(mut self) {
        self.arreter();
    }

    fn arreter(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(control) = self.control.take() {
            if let Err(e) = control.stop() {
                tracing::warn!("arrêt de la capture : {e}");
            }
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Lâchée sans `stop()` — un démontage qui la laisse tomber —, la boucle
/// s'arrête quand même : sa capture et son fil survivaient à la poignée.
impl Drop for LocalLoop {
    fn drop(&mut self) {
        self.arreter();
    }
}

/// Le thread pipeline : consomme les trames BGRA, fait l'aller-retour codec
/// complet, livre du RGBA au sink. Gère le changement de dimensions à chaud
/// (redimensionnement, changement de résolution).
fn pipeline_loop(
    stats: Arc<StageStats>,
    sink: FrameSink,
    frames: std::sync::mpsc::Receiver<capture::CapturedFrame>,
    recycle: std::sync::mpsc::Sender<Vec<u8>>,
    stop: Arc<AtomicBool>,
) {
    let mut encoder: Option<Box<dyn VideoEncoder>> = None;
    let mut decoder = match Decoder::new() {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("décodeur H.264 : {e}");
            return;
        }
    };
    let mut yuv: Option<YUVBuffer> = None;
    let mut dims = (0u32, 0u32);

    loop {
        let frame = match frames.recv_timeout(Duration::from_millis(200)) {
            Ok(f) => f,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        };
        if stop.load(Ordering::Relaxed) {
            return;
        }

        // I420 exige des dimensions paires : on rogne d'un pixel si besoin.
        let (w, h) = (frame.width & !1, frame.height & !1);
        if (w, h) != dims {
            // Changement de résolution : nouvel encodeur (le décodeur suit
            // le flux de lui-même), nouveau tampon YUV, trame clé garantie.
            dims = (w, h);
            stats.set_dims(w, h);
            encoder = None;
            yuv = Some(YUVBuffer::new(w as usize, h as usize));
            tracing::info!("labo vidéo : source {w}x{h}");
        }
        let Some(yuv_buf) = yuv.as_mut() else { continue };

        // 1. Conversion BGRA -> I420 (SIMD, openh264).
        let t0 = Instant::now();
        // Vue serrée rognée aux dimensions paires.
        let src_w = frame.width as usize;
        let tight;
        let bgra: &[u8] = if (frame.width, frame.height) == (w, h) {
            &frame.bgra
        } else {
            tight = crop_bgra(&frame.bgra, src_w, w as usize, h as usize);
            &tight
        };
        yuv_buf.read_bgra8(BgraSliceU8::new(bgra, (w as usize, h as usize)));
        stats.convert_ms.record(t0.elapsed().as_secs_f32() * 1000.0);
        stats.converted.fetch_add(1, Ordering::Relaxed);

        // Le tampon BGRA repart au recyclage.
        let _ = recycle.send(frame.bgra);

        // 2. Encodage H.264 — le même choix d'encodeur que la diffusion :
        //    le labo teste ce qui partira réellement.
        let enc = match encoder.as_mut() {
            Some(e) => e,
            None => match creer_encodeur(EncoderChoice::Auto, w, h, 6_000_000, 30, 2, Profil::Diffusion, &stats, 0) {
                Ok(e) => encoder.insert(e),
                Err(e) => {
                    tracing::error!("encodeur H.264 : {e:#}");
                    return;
                }
            },
        };
        let t1 = Instant::now();
        let packet = match enc.encode(&*yuv_buf, false) {
            Ok(Some(p)) => {
                if p.idr {
                    stats.keyframes.fetch_add(1, Ordering::Relaxed);
                }
                p.data
            }
            Ok(None) => {
                stats.enc_skipped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            Err(e) => {
                tracing::warn!("encodage : {e:#}");
                encoder = None;
                continue;
            }
        };
        stats.encode_ms.record(t1.elapsed().as_secs_f32() * 1000.0);
        stats.encoded.fetch_add(1, Ordering::Relaxed);
        stats.encoded_bytes.fetch_add(packet.len() as u64, Ordering::Relaxed);

        // 3. Décodage (exactement ce que fera un viewer).
        let t2 = Instant::now();
        match decoder.decode(&packet) {
            Ok(Some(image)) => {
                let (dw, dh) = image.dimensions();
                let mut rgba = vec![0u8; dw * dh * 4];
                image.write_rgba8(&mut rgba);
                stats.decode_ms.record(t2.elapsed().as_secs_f32() * 1000.0);
                stats.decoded.fetch_add(1, Ordering::Relaxed);
                sink(RgbaFrame { width: dw, height: dh, rgba });
            }
            Ok(None) => {} // SPS/PPS seulement : la prochaine trame sortira
            Err(e) => tracing::warn!("décodage : {e}"),
        }
    }
}

/// Une trame encodée prête à partir sur le réseau — H.264 en clair : le
/// chiffrement et l'en-tête de transport appartiennent à la couche réseau,
/// ce module ne connaît que les pixels et le codec.
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub idr: bool,
    /// Horodatage de capture, en microsecondes depuis le début du stream.
    pub pts_us: u64,
    pub width: u16,
    pub height: u16,
    /// De la qualité basse (0.1.46) : la seconde image, plus petite, pour
    /// les connexions qui ne suivent pas la haute.
    pub basse: bool,
}

/// Les deux qualités d'une diffusion, réglées à chaud par l'interface sur
/// ordre du serveur (`StreamBudget`) et lues à chaque image : la basse — son
/// débit, 0 quand elle est éteinte, et une trame clé exigée —, et la haute
/// **suspendue** quand plus personne ne la regarde (tous les spectateurs en
/// basse) : la couche réseau ne l'envoie plus, l'aperçu continue.
#[derive(Default)]
pub struct Qualites {
    basse_kbps: AtomicU32,
    basse_idr: AtomicBool,
    haute_suspendue: AtomicBool,
}

impl Qualites {
    /// Le débit de la basse ; `None` l'éteint.
    pub fn regler_basse(&self, kbps: Option<u32>) {
        self.basse_kbps.store(kbps.unwrap_or(0), Ordering::Relaxed);
    }

    pub fn basse(&self) -> Option<u32> {
        let k = self.basse_kbps.load(Ordering::Relaxed);
        (k > 0).then_some(k)
    }

    /// La prochaine image basse sera une trame clé.
    pub fn force_keyframe_basse(&self) {
        self.basse_idr.store(true, Ordering::Relaxed);
    }

    fn prendre_idr_basse(&self) -> bool {
        self.basse_idr.swap(false, Ordering::Relaxed)
    }

    /// Suspend (ou reprend) l'envoi de la haute. Vrai si l'état change.
    pub fn suspendre_haute(&self, oui: bool) -> bool {
        self.haute_suspendue.swap(oui, Ordering::Relaxed) != oui
    }

    pub fn haute_suspendue(&self) -> bool {
        self.haute_suspendue.load(Ordering::Relaxed)
    }
}

/// La résolution (hauteur) et la cadence qui vont avec un débit : à débit
/// égal, une image plus petite et plus lente est nette là où du 1080p60
/// n'est que bouillie. Seuils : ~0,055 bit par pixel et par image en H.264
/// temps réel, la cadence descendue avant la résolution. `None` : pas de
/// plafond (7 Mbit/s et plus).
pub fn qualite_pour_debit(kbps: u32) -> Option<(u32, u32)> {
    match kbps {
        k if k >= 7000 => None,
        k if k >= 3500 => Some((1080, 30)),
        k if k >= 1500 => Some((720, 30)),
        k if k >= 900 => Some((540, 30)),
        k if k >= 650 => Some((480, 30)),
        _ => Some((360, 30)),
    }
}

/// La hauteur et la cadence de la qualité basse à un débit donné : celles
/// du débit, jamais au-dessus de la haute (`hauteur` et `fps` émis), 30 i/s
/// au plus.
pub fn qualite_basse(kbps: u32, hauteur: u32, fps: u32) -> (u32, u32) {
    let (h, f) = qualite_pour_debit(kbps).unwrap_or((720, 30));
    (h.min(hauteur.max(2)), f.min(fps.max(1)).min(30))
}

/// Réceptacle des trames encodées : la boucle streamer y verse chaque trame.
pub type FrameEmit = Arc<dyn Fn(EncodedFrame) + Send + Sync>;

/// Boucle streamer : capture -> H.264 -> **émission** + aperçu.
///
/// Chaque trame encodée part vers `emit` (la couche réseau chiffre et
/// envoie), et une trame clé peut être exigée à tout moment
/// (`force_keyframe`) — c'est ainsi qu'un nouveau spectateur obtient de quoi
/// décoder en moins d'une demi-seconde.
///
/// Deux chemins. **Sur la carte** d'abord (`diffusion_gpu`, 0.1.51) : sous
/// Windows avec une carte NVIDIA, la capture, la conversion et l'encodage
/// ne quittent pas la carte, et l'aperçu est une vignette de l'image
/// capturée. **Par le processeur** sinon (`streamer_pipeline`) : l'image
/// est lue, convertie et réduite sur un cœur, encodée par NVENC ou
/// openh264, et l'aperçu passe par le décodage local — le streamer y voit
/// exactement ce que reçoivent ses spectateurs, artefacts compris.
pub struct StreamerLoop {
    force_idr: Arc<AtomicBool>,
    /// `None` une fois arrêtée : l'arrêt passe aussi par `Drop`.
    boucle: Option<Boucle>,
}

enum Boucle {
    #[cfg(windows)]
    Carte(diffusion_gpu::DiffusionGpu),
    Processeur {
        control: capture::Control,
        stop: Arc<AtomicBool>,
        closed: Arc<AtomicBool>,
        worker: std::thread::JoinHandle<()>,
    },
}

/// Les réglages d'une diffusion, tels que l'interface les tient.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamConfig {
    pub source: CaptureSource,
    /// Hauteur plafond de l'image émise (0 = celle de la source). La source
    /// est réduite avant l'encodeur, jamais agrandie.
    pub max_height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
    pub cursor: bool,
    /// Décoder localement ce qui part, pour l'aperçu — ~3 ms par trame que
    /// l'on peut rendre au jeu en s'en passant.
    pub preview: bool,
    pub encoder: EncoderChoice,
    /// Longueur du groupe d'images, en secondes : deux pour diffuser, une
    /// pour un clip (qui se coupe à la trame clé).
    pub gop_s: u32,
    /// Diffusion ou clip : le réglage de l'encodeur.
    pub profil: Profil,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            source: CaptureSource::Monitor(0),
            max_height: 0,
            fps: 30,
            bitrate_bps: 6_000_000,
            cursor: true,
            preview: true,
            encoder: EncoderChoice::Auto,
            gop_s: 2,
            profil: Profil::Diffusion,
        }
    }
}

impl StreamerLoop {
    /// `force_idr` est partagé avec l'appelant : la couche réseau le lève
    /// quand elle jette une trame (le spectateur suivant a besoin d'une
    /// trame clé), l'interface quand le serveur transmet KeyframeNeeded.
    /// `origine` est l'instant zéro des horodatages : donné par l'appelant
    /// pour être le même que celui du son du jeu, et survivre à une
    /// relance de la capture.
    /// `qualites` : la qualité basse à produire en plus, quand le serveur la
    /// demande — `None` pour un clip, qui n'en a qu'une.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        stats: Arc<StageStats>,
        preview: FrameSink,
        emit: FrameEmit,
        config: StreamConfig,
        force_idr: Arc<AtomicBool>,
        origine: Instant,
        qualites: Option<Arc<Qualites>>,
    ) -> anyhow::Result<Self> {
        // La carte d'abord, pour une diffusion qui peut s'y faire : un clip
        // a sa propre chaîne (`ClipGpu`), le logiciel imposé reste logiciel.
        #[cfg(windows)]
        if config.profil == Profil::Diffusion && config.encoder != EncoderChoice::Logiciel {
            match diffusion_gpu::DiffusionGpu::demarrer(
                stats.clone(),
                preview.clone(),
                emit.clone(),
                config.clone(),
                force_idr.clone(),
                origine,
                qualites.clone(),
            ) {
                Ok(carte) => return Ok(Self { force_idr, boucle: Some(Boucle::Carte(carte)) }),
                Err(e) => journal(format!("diffusion : pas de chaîne sur la carte ({e:#}) — chemin du processeur")),
            }
        }
        stats.mark_started();
        let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<capture::CapturedFrame>(1);
        let (recycle_tx, recycle_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let closed = Arc::new(AtomicBool::new(false));
        let control = capture::start_capture(
            &config.source,
            config.cursor,
            config.fps,
            capture::CaptureFlags {
                stats: stats.clone(),
                tx: frame_tx,
                recycle: recycle_rx,
                closed: closed.clone(),
                interval: Duration::ZERO,
            },
        )?;
        let stop = Arc::new(AtomicBool::new(false));
        let sante = SanteCapture { arretee: control.halt_handle(), fermee: closed.clone() };
        let worker = {
            let (stop, force_idr) = (stop.clone(), force_idr.clone());
            std::thread::Builder::new()
                .name("video-streamer".into())
                .spawn(move || {
                    streamer_pipeline(
                        stats, preview, emit, config, frame_rx, recycle_tx, stop, force_idr,
                        origine, qualites, sante,
                    )
                })
                .context("thread streamer vidéo")?
        };
        Ok(Self { force_idr, boucle: Some(Boucle::Processeur { control, stop, closed, worker }) })
    }

    /// La prochaine trame encodée sera une trame clé (IDR) — pour un
    /// spectateur qui arrive ou qui a perdu pied.
    pub fn force_keyframe(&self) {
        self.force_idr.store(true, Ordering::Relaxed);
    }

    /// La source s'est évanouie (fenêtre fermée) : plus rien ne viendra,
    /// à l'appelant de conclure la diffusion.
    pub fn source_closed(&self) -> bool {
        match &self.boucle {
            #[cfg(windows)]
            Some(Boucle::Carte(carte)) => carte.source_fermee(),
            Some(Boucle::Processeur { closed, .. }) => closed.load(Ordering::Relaxed),
            None => false,
        }
    }

    /// La diffusion s'est arrêtée d'elle-même, sans que la source se ferme :
    /// la capture est morte (carte graphique réinitialisée, le plus souvent)
    /// ou l'encodeur a tout refusé. Plus rien ne part, et rien ne repartira
    /// — à l'appelant de conclure, comme pour une source fermée, mais sans
    /// dire qu'une fenêtre s'est fermée. L'avis posé dans les statistiques
    /// dit pourquoi, quand le fil a pu le dire.
    ///
    /// C'était l'image figée chez tous les spectateurs et « 0 i/s » chez le
    /// streamer, sans un mot : une erreur de lecture d'image arrête la
    /// capture sans passer par `on_closed`, et le pipeline attendait des
    /// images qui ne viendraient plus.
    pub fn en_panne(&self) -> bool {
        match &self.boucle {
            #[cfg(windows)]
            Some(Boucle::Carte(carte)) => carte.en_panne(),
            Some(Boucle::Processeur { control, closed, worker, .. }) => {
                !closed.load(Ordering::Relaxed) && (control.is_finished() || worker.is_finished())
            }
            None => false,
        }
    }

    pub fn stop(mut self) {
        self.arreter();
    }

    fn arreter(&mut self) {
        match self.boucle.take() {
            #[cfg(windows)]
            Some(Boucle::Carte(carte)) => carte.arreter(),
            Some(Boucle::Processeur { control, stop, worker, .. }) => {
                stop.store(true, Ordering::Relaxed);
                if let Err(e) = control.stop() {
                    tracing::warn!("arrêt de la capture : {e}");
                }
                let _ = worker.join();
            }
            None => {}
        }
    }
}

/// Lâchée sans `stop()`, la boucle s'arrête quand même : la capture et le
/// fil du chemin du processeur survivaient à la poignée, et continuaient
/// d'encoder et d'émettre une diffusion que plus rien ne tenait.
impl Drop for StreamerLoop {
    fn drop(&mut self) {
        self.arreter();
    }
}

/// Le chemin du processeur, sur le fil de l'appelant, jusqu'à `stop` : la
/// diffusion sur la carte qui renonce en route le prend ainsi à son compte,
/// sans couper le stream.
#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn diffuser_par_le_processeur(
    stats: Arc<StageStats>,
    preview: FrameSink,
    emit: FrameEmit,
    config: StreamConfig,
    force_idr: Arc<AtomicBool>,
    origine: Instant,
    qualites: Option<Arc<Qualites>>,
    stop: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<capture::CapturedFrame>(1);
    let (recycle_tx, recycle_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let control = capture::start_capture(
        &config.source,
        config.cursor,
        config.fps,
        capture::CaptureFlags {
            stats: stats.clone(),
            tx: frame_tx,
            recycle: recycle_rx,
            closed: closed.clone(),
            interval: Duration::ZERO,
        },
    )?;
    let sante = SanteCapture { arretee: control.halt_handle(), fermee: closed };
    streamer_pipeline(
        stats, preview, emit, config, frame_rx, recycle_tx, stop, force_idr, origine, qualites, sante,
    );
    if let Err(e) = control.stop() {
        tracing::warn!("arrêt de la capture : {e}");
    }
    Ok(())
}

/// Ce que le pipeline sait de la capture qui le nourrit.
struct SanteCapture {
    /// Levé par la capture quand elle s'arrête — sur demande, à la
    /// fermeture de la source, ou sur une erreur de lecture d'image.
    arretee: Arc<AtomicBool>,
    /// Levé quand la source s'est fermée (fenêtre diffusée fermée).
    fermee: Arc<AtomicBool>,
}

/// Le pipeline streamer : mêmes étages que le labo, plus l'émission.
#[allow(clippy::too_many_arguments)]
fn streamer_pipeline(
    stats: Arc<StageStats>,
    preview: FrameSink,
    emit: FrameEmit,
    config: StreamConfig,
    frames: std::sync::mpsc::Receiver<capture::CapturedFrame>,
    recycle: std::sync::mpsc::Sender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    force_idr: Arc<AtomicBool>,
    origine: Instant,
    qualites: Option<Arc<Qualites>>,
    sante: SanteCapture,
) {
    let mut encoder: Option<Box<dyn VideoEncoder>> = None;
    // La qualité basse : son encodeur, sa réduction, ses réglages en
    // vigueur (largeur, hauteur, débit, cadence), la dernière image émise —
    // et un refus d'encodeur, qui l'arrête pour cette capture (le serveur
    // s'en apercevra et s'en passera).
    let mut basse_enc: Option<Box<dyn VideoEncoder>> = None;
    let mut basse_scaler = scale::Scaler::new();
    let mut basse_params: Option<(u32, u32, u32, u32)> = None;
    let mut basse_pts: Option<u64> = None;
    let mut basse_hs = false;
    // Échecs d'encodage consécutifs de la qualité basse : au troisième, elle
    // est abandonnée pour cette capture (voir plus bas).
    let mut basse_echecs: u32 = 0;
    let basse_stats = StageStats::default();
    // L'encodeur voulu, et les refus de NVENC en cours de route : au second
    // (un par chemin d'entrée), on passe au logiciel et on le dit — plutôt
    // que de recréer une session à chaque image sans jamais émettre.
    let mut choix = config.encoder;
    let mut echecs_nvenc: u32 = 0;
    // L'aperçu n'existe que si on le demande : sans lui, pas de décodeur.
    let mut decoder = if config.preview {
        match Decoder::new() {
            Ok(d) => Some(d),
            Err(e) => {
                tracing::warn!("décodeur H.264 (aperçu) : {e} — diffusion sans aperçu");
                None
            }
        }
    } else {
        None
    };
    let mut yuv: Option<YUVBuffer> = None;
    let mut scaler = scale::Scaler::new();
    // Dimensions de la source, et celles que l'on émet (réduites ou non).
    let mut dims = (0u32, 0u32);
    let mut sortie = (0u32, 0u32);
    // La base des horodatages : l'instant zéro du stream, le même que celui
    // du son du jeu — c'est lui qui porte la synchronisation image/son.
    let depart = origine;
    // Le dernier horodatage émis : les suivants montent toujours (une trame
    // clé resservie d'une image immobile est datée de maintenant, l'image
    // neuve qui la suit, de sa capture).
    let mut dernier_pts: Option<u64> = None;
    // La capture arrêtée sans qu'on l'ait demandé, vue une première fois.
    // Une fenêtre qui se ferme lève l'arrêt un instant avant de se dire
    // fermée : on attend le tour suivant pour conclure.
    let mut arret_vu = false;

    loop {
        let frame = match frames.recv_timeout(Duration::from_millis(200)) {
            Ok(f) => Some(f),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::Relaxed) || sante.fermee.load(Ordering::Relaxed) {
                    return;
                }
                // Plus d'image à attendre : la capture s'est arrêtée seule.
                // Son canal ne se ferme pas pour autant (la poignée de la
                // capture le garde), d'où ce regard à chaque attente vaine.
                if sante.arretee.load(Ordering::Relaxed) {
                    if arret_vu {
                        journal("diffusion : la capture de l'écran s'est arrêtée seule");
                        stats.poser_avis(
                            "la capture de l'écran s'est arrêtée (carte graphique réinitialisée ?) : \
                             diffusion interrompue"
                                .into(),
                        );
                        return;
                    }
                    arret_vu = true;
                    continue;
                }
                // Rien ne bouge à l'écran : la capture ne livre aucune image,
                // et une trame clé demandée — un spectateur qui arrive — ne
                // partait qu'au premier mouvement. D'ici là, il voyait du
                // noir. Elle part donc de la dernière image convertie.
                if force_idr.load(Ordering::Relaxed) && encoder.is_some() && yuv.is_some() {
                    None
                } else {
                    continue;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        };
        if stop.load(Ordering::Relaxed) {
            return;
        }
        // Daté de sa capture, et non du moment où le pipeline s'en saisit :
        // l'image qui attendait dans la file partait avec une trame de
        // retard sur le son du jeu, lui daté à la capture.
        let capture_us = match &frame {
            Some(f) => f.instant.saturating_duration_since(depart).as_micros() as u64,
            None => depart.elapsed().as_micros() as u64,
        };
        let pts_us = capture_us.max(dernier_pts.map_or(0, |d| d + 1));
        dernier_pts = Some(pts_us);

        // I420 exige des dimensions paires : rognées, comme au labo. Sans
        // image neuve, celles de la dernière.
        let (w, h) = match &frame {
            Some(f) => (f.width & !1, f.height & !1),
            None => dims,
        };
        if (w, h) != dims {
            dims = (w, h);
            sortie = scale::target_dims(w, h, config.max_height);
            stats.set_dims(sortie.0, sortie.1);
            encoder = None;
            yuv = Some(YUVBuffer::new(w as usize, h as usize));
            if sortie == dims {
                journal(format!("diffusion : source {w}x{h}"));
            } else {
                journal(format!(
                    "diffusion : source {w}x{h}, émise en {}x{}",
                    sortie.0, sortie.1
                ));
            }
        }
        let Some(yuv_buf) = yuv.as_mut() else { continue };
        let (ow, oh) = sortie;

        // 1. Conversion BGRA -> I420, puis réduction si l'image émise est
        //    plus petite que la source. Sans image neuve, la dernière
        //    convertie est toujours là.
        let t0 = Instant::now();
        let neuve = frame.is_some();
        if let Some(frame) = frame {
            let src_w = frame.width as usize;
            let tight;
            let bgra: &[u8] = if (frame.width, frame.height) == (w, h) {
                &frame.bgra
            } else {
                tight = crop_bgra(&frame.bgra, src_w, w as usize, h as usize);
                &tight
            };
            yuv_buf.read_bgra8(BgraSliceU8::new(bgra, (w as usize, h as usize)));
            let _ = recycle.send(frame.bgra);
        }
        let reduit: Option<&scale::I420> = if (ow, oh) != (w, h) {
            Some(scaler.scale(yuv_buf, ow as usize, oh as usize))
        } else {
            None
        };
        if neuve {
            stats.convert_ms.record(t0.elapsed().as_secs_f32() * 1000.0);
            stats.converted.fetch_add(1, Ordering::Relaxed);
        }

        // 2. Encodage — trame clé exigée si un spectateur l'attend.
        let enc = match encoder.as_mut() {
            Some(e) => e,
            None => match creer_encodeur(
                choix,
                ow,
                oh,
                config.bitrate_bps,
                config.fps,
                config.gop_s,
                config.profil,
                &stats,
                echecs_nvenc,
            ) {
                Ok(e) => encoder.insert(e),
                Err(e) => {
                    // L'encodeur exigé ne s'ouvre pas — pilote NVIDIA trop
                    // ancien, session refusée par la carte… Le dire, à la
                    // personne qui diffuse comme au journal, et continuer en
                    // logiciel plutôt que de laisser un stream « en cours »
                    // qui n'émet plus une image. C'était le silence : le fil
                    // s'arrêtait, et l'interface n'en savait rien.
                    journal(format!("encodeur H.264 : {e:#}"));
                    if choix == EncoderChoice::Logiciel {
                        stats.poser_avis(format!("diffusion impossible : {e:#}"));
                        return;
                    }
                    match creer_encodeur(
                        EncoderChoice::Logiciel,
                        ow,
                        oh,
                        config.bitrate_bps,
                        config.fps,
                        config.gop_s,
                        config.profil,
                        &stats,
                        0,
                    ) {
                        Ok(logiciel) => {
                            stats.poser_avis(format!(
                                "NVENC indisponible : {e:#} — encodage logiciel à la place \
                                 (passe en 720p si ça saccade)"
                            ));
                            encoder.insert(logiciel)
                        }
                        Err(e2) => {
                            journal(format!("encodeur logiciel : {e2:#}"));
                            stats.poser_avis(format!("diffusion impossible : {e2:#}"));
                            return;
                        }
                    }
                }
            },
        };
        let force = force_idr.swap(false, Ordering::Relaxed);
        let t1 = Instant::now();
        let source: &dyn YUVSource = match reduit {
            Some(petit) => petit,
            None => &*yuv_buf,
        };
        let (packet, idr) = match enc.encode(source, force) {
            Ok(Some(p)) => {
                if p.idr {
                    stats.keyframes.fetch_add(1, Ordering::Relaxed);
                }
                (p.data, p.idr)
            }
            Ok(None) => {
                // L'encodeur a sauté la trame (budget de débit dépassé) ;
                // la demande de trame clé, elle, ne se perd pas.
                stats.enc_skipped.fetch_add(1, Ordering::Relaxed);
                if force {
                    force_idr.store(true, Ordering::Relaxed);
                }
                continue;
            }
            Err(e) => {
                // Un encodeur qui lâche (carte perdue, pilote) se recrée à
                // la trame suivante. NVENC a droit à deux refus — un par
                // chemin d'entrée — puis c'est le logiciel, et la personne
                // qui diffuse le sait : une RTX 2070 a passé une diffusion
                // entière à recréer sa session, sans une image émise.
                if stats.materiel.load(Ordering::Relaxed) {
                    echecs_nvenc += 1;
                    if echecs_nvenc >= 2 {
                        journal(format!("encodage : {e:#} — NVENC abandonné, encodeur logiciel"));
                        stats.poser_avis(format!(
                            "NVENC refuse d'encoder ({e:#}) — encodage logiciel à la place \
                             (passe en 720p si ça saccade)"
                        ));
                        choix = EncoderChoice::Logiciel;
                    } else {
                        journal(format!(
                            "encodage : {e:#} — encodeur recréé, entrée par tampon"
                        ));
                    }
                } else {
                    journal(format!("encodage : {e:#} — encodeur recréé"));
                }
                encoder = None;
                force_idr.store(true, Ordering::Relaxed);
                continue;
            }
        };
        stats.encode_ms.record(t1.elapsed().as_secs_f32() * 1000.0);
        stats.encoded.fetch_add(1, Ordering::Relaxed);
        stats.encoded_bytes.fetch_add(packet.len() as u64, Ordering::Relaxed);

        // 3. Aperçu local : le décodage de ce qui vient de partir — le
        // streamer voit ce que voient ses spectateurs.
        if let Some(dec) = decoder.as_mut() {
            let t2 = Instant::now();
            match dec.decode(&packet) {
                Ok(Some(image)) => {
                    let (dw, dh) = image.dimensions();
                    let mut rgba = vec![0u8; dw * dh * 4];
                    image.write_rgba8(&mut rgba);
                    stats.decode_ms.record(t2.elapsed().as_secs_f32() * 1000.0);
                    stats.decoded.fetch_add(1, Ordering::Relaxed);
                    preview(RgbaFrame { width: dw, height: dh, rgba });
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("décodage aperçu : {e}"),
            }
        }

        // 4. Émission : la couche réseau chiffre, encadre, envoie.
        emit(EncodedFrame { data: packet, idr, pts_us, width: ow as u16, height: oh as u16, basse: false });

        // 5. La qualité basse, quand le serveur la demande : la même image,
        //    réduite depuis la source, encodée à part et à sa cadence — pour
        //    les connexions qui ne suivent pas la haute.
        let Some(q) = qualites.as_ref() else { continue };
        let Some(kbps) = q.basse().filter(|_| !basse_hs) else {
            if basse_enc.take().is_some() {
                stats.set_basse_dims(0, 0);
                basse_params = None;
                journal("diffusion : qualité basse arrêtée");
            }
            continue;
        };
        let (hb, fb) = qualite_basse(kbps, oh, config.fps);
        let (bw, bh) = scale::target_dims(w, h, hb);
        if basse_params != Some((bw, bh, kbps, fb)) {
            basse_params = Some((bw, bh, kbps, fb));
            basse_enc = None;
        }
        // Sa cadence : une image sur deux quand la capture va deux fois
        // plus vite — à un quart d'intervalle près, la gigue de la capture.
        let pas_us = 1_000_000 / u64::from(fb.max(1));
        if basse_pts.is_some_and(|d| pts_us + pas_us / 4 < d + pas_us) {
            continue;
        }
        basse_pts = Some(pts_us);
        let t3 = Instant::now();
        if basse_enc.is_none() {
            let choix_basse = if choix == EncoderChoice::Logiciel { EncoderChoice::Logiciel } else { EncoderChoice::Auto };
            match creer_encodeur(choix_basse, bw, bh, kbps.saturating_mul(1000), fb, config.gop_s, Profil::Diffusion, &basse_stats, 0) {
                Ok(e) => {
                    journal(format!("diffusion : qualité basse {bw}x{bh} à {fb} i/s, {kbps} kbit/s"));
                    stats.set_basse_dims(bw, bh);
                    basse_enc = Some(e);
                }
                Err(e) => {
                    journal(format!("diffusion : qualité basse impossible ({e:#})"));
                    basse_hs = true;
                    continue;
                }
            }
        }
        let Some(enc) = basse_enc.as_mut() else { continue };
        let petite = basse_scaler.scale(&*yuv_buf, bw as usize, bh as usize);
        let force = q.prendre_idr_basse();
        match enc.encode(petite, force) {
            Ok(Some(p)) => {
                basse_echecs = 0;
                stats.basse_ms.record(t3.elapsed().as_secs_f32() * 1000.0);
                stats.basse_encoded.fetch_add(1, Ordering::Relaxed);
                stats.basse_bytes.fetch_add(p.data.len() as u64, Ordering::Relaxed);
                emit(EncodedFrame {
                    data: p.data,
                    idr: p.idr,
                    pts_us,
                    width: bw as u16,
                    height: bh as u16,
                    basse: true,
                });
            }
            Ok(None) => {
                if force {
                    q.force_keyframe_basse();
                }
            }
            Err(e) => {
                // Recréé à l'image suivante ; son premier paquet est une
                // trame clé. Pas indéfiniment : un encodeur qui s'ouvre puis
                // refuse chaque image était recréé à la cadence de la
                // qualité basse — jusqu'à trente sessions NVENC par seconde,
                // autant de lignes au journal. Au troisième échec d'affilée,
                // elle s'arrête pour cette capture ; la haute continue.
                basse_enc = None;
                basse_echecs += 1;
                if basse_echecs >= 3 {
                    journal(format!("diffusion : qualité basse abandonnée ({e:#}), trois échecs d'affilée"));
                    basse_hs = true;
                    basse_params = None;
                    stats.set_basse_dims(0, 0);
                } else {
                    journal(format!("diffusion : qualité basse, encodage raté ({e:#}) — encodeur recréé"));
                }
            }
        }
    }
}

/// Décodeur d'un spectateur : reçoit du H.264 en clair (déjà déchiffré et
/// remis en ordre par la couche réseau), rend des images prêtes à peindre.
/// `None` n'est pas une erreur : un paquet SPS/PPS seul ne produit pas
/// d'image, la suivante sortira.
pub struct ViewerDecoder {
    decoder: Decoder,
}

impl ViewerDecoder {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self { decoder: Decoder::new().context("décodeur H.264 spectateur")? })
    }

    pub fn decode(&mut self, h264: &[u8]) -> Option<RgbaFrame> {
        match self.decoder.decode(h264) {
            Ok(Some(image)) => {
                let (w, h) = image.dimensions();
                let mut rgba = vec![0u8; w * h * 4];
                image.write_rgba8(&mut rgba);
                Some(RgbaFrame { width: w, height: h, rgba })
            }
            Ok(None) => None,
            Err(e) => {
                tracing::debug!("décodage spectateur : {e}");
                None
            }
        }
    }
}

/// Rogne un buffer BGRA serré de `src_w` colonnes vers `w`x`h`.
fn crop_bgra(src: &[u8], src_w: usize, w: usize, h: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        let start = row * src_w * 4;
        out.extend_from_slice(&src[start..start + w * 4]);
    }
    out
}

/// Construit un encodeur H.264 configuré pour le partage d'écran temps réel.
/// GOP de deux secondes — les trames clés à la demande viennent de
/// `force_intra_frame`.
pub fn screen_encoder(
    width: u32,
    height: u32,
    bitrate_bps: u32,
    fps: u32,
    gop_s: u32,
) -> anyhow::Result<Encoder> {
    let fps = fps.clamp(1, 120);
    let gop_s = gop_s.clamp(1, 10);
    let config = EncoderConfig::new()
        .usage_type(UsageType::ScreenContentRealTime)
        .bitrate(BitRate::from_bps(bitrate_bps))
        .max_frame_rate(FrameRate::from_hz(fps as f32))
        .intra_frame_period(IntraFramePeriod::from_num_frames(gop_s * fps))
        .skip_frames(true)
        // Quelques threads d'encodage : le 1080p30 doit tenir même pendant
        // qu'un jeu occupe le reste du CPU.
        .num_threads(4);
    let _ = (width, height); // dimensions portées par chaque frame en 0.9
    Encoder::with_api_config(OpenH264API::from_source(), config).context("création encodeur H.264")
}

#[cfg(test)]
mod tests {
    use super::*;
    use openh264::decoder::Decoder;
    use openh264::formats::{YUVBuffer, YUVSource};

    /// Porte de build S0.5 : encode puis décode une image — si ceci passe en
    /// release + crt-static, le choix de codec est validé.
    #[test]
    fn h264_roundtrip_smoke() {
        let (w, h) = (320usize, 240usize);
        let mut encoder = screen_encoder(w as u32, h as u32, 500_000, 30, 2).unwrap();

        // Dégradé synthétique en I420.
        let mut yuv = vec![0u8; w * h + (w * h) / 2];
        for y in 0..h {
            for x in 0..w {
                yuv[y * w + x] = ((x + y) % 255) as u8;
            }
        }
        for c in yuv[w * h..].iter_mut() {
            *c = 128;
        }
        let buffer = YUVBuffer::from_vec(yuv, w, h);

        let bitstream = encoder.encode(&buffer).expect("encodage");
        let bytes = bitstream.to_vec();
        assert!(!bytes.is_empty(), "aucun NAL produit");

        let mut decoder = Decoder::new().expect("création décodeur");
        let decoded = decoder.decode(&bytes).expect("décodage");
        let frame = decoded.expect("une image décodée");
        assert_eq!(frame.dimensions(), (w, h));
    }

    /// La boucle telle que l'application la lance : sur une machine NVIDIA,
    /// elle prend la carte (encodeur matériel, aperçu en vignettes), émet
    /// du H.264 décodable par un spectateur, et s'arrête proprement.
    /// Ignoré par défaut (écran et carte NVIDIA) :
    /// `cargo test -p ki-video boucle_de_diffusion -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn la_boucle_de_diffusion_prend_la_carte() {
        let stats = Arc::new(StageStats::default());
        let trames = Arc::new(std::sync::Mutex::new(Vec::<(bool, Vec<u8>)>::new()));
        let emit: FrameEmit = {
            let t = trames.clone();
            Arc::new(move |f: EncodedFrame| t.lock().unwrap().push((f.idr, f.data)))
        };
        let apercus = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let preview: FrameSink = {
            let a = apercus.clone();
            Arc::new(move |_| {
                a.fetch_add(1, Ordering::Relaxed);
            })
        };
        let config = StreamConfig { fps: 60, bitrate_bps: 12_000_000, ..StreamConfig::default() };
        let boucle = StreamerLoop::start(
            stats.clone(),
            preview,
            emit,
            config,
            Arc::new(AtomicBool::new(false)),
            Instant::now(),
            Some(Arc::new(Qualites::default())),
        )
        .expect("la boucle démarre");
        std::thread::sleep(Duration::from_secs(2));
        assert!(!boucle.source_closed());
        boucle.stop();
        let trames = trames.lock().unwrap();
        eprintln!("{} trames, {} aperçus, {}", trames.len(), apercus.load(Ordering::Relaxed), stats.summary());
        if !stats.materiel.load(Ordering::Relaxed) {
            eprintln!("pas de carte NVIDIA ici : chemin du processeur, rien de plus à vérifier");
            return;
        }
        assert!(!trames.is_empty() && trames[0].0, "une trame clé en tête");
        assert!(apercus.load(Ordering::Relaxed) > 0, "des vignettes d'aperçu");
        // Ce qu'un spectateur en fait : le décodeur logiciel du visionnage.
        let mut spectateur = ViewerDecoder::new().unwrap();
        let images = trames.iter().filter(|(_, d)| spectateur.decode(d).is_some()).count();
        assert!(images * 10 >= trames.len() * 9, "{images} images décodées sur {} trames", trames.len());
    }

    /// Lance le pipeline sur une capture simulée : son canal d'images reste
    /// ouvert (comme quand la poignée de la vraie capture le garde), et
    /// l'appelant lève les drapeaux de santé. Rend le fil et les
    /// statistiques.
    fn pipeline_sur_capture_simulee(
        arretee: bool,
        fermee: bool,
    ) -> (
        std::thread::JoinHandle<()>,
        Arc<StageStats>,
        std::sync::mpsc::SyncSender<capture::CapturedFrame>,
    ) {
        let stats = Arc::new(StageStats::default());
        let (tx, rx) = std::sync::mpsc::sync_channel::<capture::CapturedFrame>(1);
        let (recycle_tx, _recycle_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let sante = SanteCapture {
            arretee: Arc::new(AtomicBool::new(arretee)),
            fermee: Arc::new(AtomicBool::new(fermee)),
        };
        let fil = {
            let stats = stats.clone();
            std::thread::spawn(move || {
                streamer_pipeline(
                    stats,
                    Arc::new(|_| {}),
                    Arc::new(|_: EncodedFrame| {}),
                    StreamConfig { preview: false, ..StreamConfig::default() },
                    rx,
                    recycle_tx,
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(AtomicBool::new(false)),
                    Instant::now(),
                    None,
                    sante,
                )
            })
        };
        (fil, stats, tx)
    }

    fn fini_sous(fil: &std::thread::JoinHandle<()>, delai: Duration) -> bool {
        let debut = Instant::now();
        while !fil.is_finished() && debut.elapsed() < delai {
            std::thread::sleep(Duration::from_millis(20));
        }
        fil.is_finished()
    }

    /// Une capture qui s'arrête seule (erreur de lecture d'image, carte
    /// graphique réinitialisée) ne ferme pas son canal : le pipeline
    /// attendait des images pour toujours, stream « en cours » et image figée
    /// chez les spectateurs. Il sort désormais, et laisse un avis qui dit
    /// pourquoi.
    #[test]
    fn une_capture_arretee_seule_arrete_le_pipeline_et_le_dit() {
        let (fil, stats, _garde) = pipeline_sur_capture_simulee(true, false);
        assert!(fini_sous(&fil, Duration::from_secs(3)), "le pipeline attend encore");
        let avis = stats.prendre_avis().expect("un avis pour l'interface");
        assert!(avis.contains("capture"), "{avis}");
    }

    /// Sur une image immobile, la capture ne livre plus rien : une trame clé
    /// demandée (un spectateur qui arrive) part quand même, de la dernière
    /// image convertie, au lieu d'attendre le premier mouvement.
    #[test]
    fn une_trame_cle_demandee_part_meme_sur_une_image_immobile() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<capture::CapturedFrame>(1);
        let (recycle_tx, _recycle_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let trames = Arc::new(std::sync::Mutex::new(Vec::<bool>::new()));
        let emit: FrameEmit = {
            let t = trames.clone();
            Arc::new(move |f: EncodedFrame| t.lock().unwrap().push(f.idr))
        };
        let force_idr = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let sante = SanteCapture {
            arretee: Arc::new(AtomicBool::new(false)),
            fermee: Arc::new(AtomicBool::new(false)),
        };
        let fil = {
            let (force_idr, stop) = (force_idr.clone(), stop.clone());
            std::thread::spawn(move || {
                streamer_pipeline(
                    Arc::new(StageStats::default()),
                    Arc::new(|_| {}),
                    emit,
                    StreamConfig {
                        preview: false,
                        encoder: EncoderChoice::Logiciel,
                        ..StreamConfig::default()
                    },
                    rx,
                    recycle_tx,
                    stop,
                    force_idr,
                    Instant::now(),
                    None,
                    sante,
                )
            })
        };
        let attendre = |n: usize| {
            let debut = Instant::now();
            while trames.lock().unwrap().len() < n && debut.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(10));
            }
            trames.lock().unwrap().len() >= n
        };
        // Une seule image, puis plus rien : l'écran ne bouge plus.
        tx.send(capture::CapturedFrame {
            width: 64,
            height: 64,
            bgra: vec![128; 64 * 64 * 4],
            instant: Instant::now(),
        })
        .unwrap();
        assert!(attendre(1), "la première image n'est pas partie");
        let avant = trames.lock().unwrap().len();
        force_idr.store(true, Ordering::Relaxed);
        assert!(attendre(avant + 1), "la trame clé demandée n'est pas partie");
        assert_eq!(trames.lock().unwrap().last(), Some(&true), "ce n'est pas une trame clé");
        stop.store(true, Ordering::Relaxed);
        fil.join().unwrap();
    }

    /// Une fenêtre fermée arrête aussi le pipeline, mais sans avis de
    /// panne : l'interface le dit à sa façon (« la fenêtre diffusée a été
    /// fermée »).
    #[test]
    fn une_source_fermee_arrete_le_pipeline_sans_avis_de_panne() {
        let (fil, stats, _garde) = pipeline_sur_capture_simulee(true, true);
        assert!(fini_sous(&fil, Duration::from_secs(3)), "le pipeline attend encore");
        assert!(stats.prendre_avis().is_none());
    }
}
