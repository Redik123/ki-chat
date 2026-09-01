//! Pipeline vidéo du partage d'écran — voir PLAN-STREAM.md.
//!
//! Deux boucles sur les mêmes briques : celle du labo (capture -> I420 ->
//! H.264 -> décodage -> image RGBA, zéro réseau) et la boucle streamer — la
//! même, qui sait en plus réduire l'image avant l'encodeur et livre chaque
//! trame encodée à la couche réseau. Le tout instrumenté étage par étage
//! (`StageStats`). Le transport, lui, vit ailleurs : ce crate ne connaît
//! que les pixels et le codec.

pub mod capture;
pub mod scale;
pub mod stats;

pub use capture::{list_monitors, list_windows, CaptureSource, MonitorInfo, WindowInfo};
pub use stats::StageStats;

use std::sync::atomic::{AtomicBool, Ordering};
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

/// Boucle locale S1a : capture écran -> I420 -> H.264 -> décodage -> sink.
///
/// Threads : la capture vit sur le thread de windows-capture (rappel léger),
/// tout le travail (conversion, encodage, décodage) sur UN thread pipeline
/// dédié — jamais sur le thread réseau ni sur l'UI.
pub struct LocalLoop {
    control: windows_capture::capture::CaptureControl<
        capture::ScreenGrab,
        Box<dyn std::error::Error + Send + Sync>,
    >,
    stop: Arc<AtomicBool>,
    worker: std::thread::JoinHandle<()>,
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

        Ok(Self { control, stop, worker })
    }

    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Err(e) = self.control.stop() {
            tracing::warn!("arrêt de la capture : {e}");
        }
        let _ = self.worker.join();
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
    let mut encoder: Option<Encoder> = None;
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

        // 2. Encodage H.264.
        let enc = match encoder.as_mut() {
            Some(e) => e,
            None => match screen_encoder(w, h, 6_000_000, 30) {
                Ok(e) => encoder.insert(e),
                Err(e) => {
                    tracing::error!("encodeur H.264 : {e:#}");
                    return;
                }
            },
        };
        let t1 = Instant::now();
        let packet = match enc.encode(yuv_buf) {
            Ok(bs) => {
                if bs.frame_type() == FrameType::IDR {
                    stats.keyframes.fetch_add(1, Ordering::Relaxed);
                }
                bs.to_vec()
            }
            Err(e) => {
                tracing::warn!("encodage : {e}");
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
}

/// Réceptacle des trames encodées : la boucle streamer y verse chaque trame.
pub type FrameEmit = Arc<dyn Fn(EncodedFrame) + Send + Sync>;

/// Boucle streamer S1b : capture -> I420 -> H.264 -> **émission** + aperçu.
///
/// C'est la boucle locale (S1a) plus deux choses : chaque trame encodée part
/// vers `emit` (la couche réseau chiffre et envoie), et une trame clé peut
/// être exigée à tout moment (`force_keyframe`) — c'est ainsi qu'un nouveau
/// spectateur obtient de quoi décoder en moins d'une demi-seconde.
///
/// L'aperçu passe par le décodage local, comme au labo : ce que le streamer
/// voit est EXACTEMENT ce que ses spectateurs reçoivent, artefacts du codec
/// compris — jamais un aller-retour serveur.
pub struct StreamerLoop {
    control: capture::Control,
    stop: Arc<AtomicBool>,
    force_idr: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    worker: std::thread::JoinHandle<()>,
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
        }
    }
}

impl StreamerLoop {
    /// `force_idr` est partagé avec l'appelant : la couche réseau le lève
    /// quand elle jette une trame (le spectateur suivant a besoin d'une
    /// trame clé), l'interface quand le serveur transmet KeyframeNeeded.
    pub fn start(
        stats: Arc<StageStats>,
        preview: FrameSink,
        emit: FrameEmit,
        config: StreamConfig,
        force_idr: Arc<AtomicBool>,
    ) -> anyhow::Result<Self> {
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
            },
        )?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let (stop, force_idr) = (stop.clone(), force_idr.clone());
            std::thread::Builder::new()
                .name("video-streamer".into())
                .spawn(move || {
                    streamer_pipeline(
                        stats, preview, emit, config, frame_rx, recycle_tx, stop, force_idr,
                    )
                })
                .context("thread streamer vidéo")?
        };
        Ok(Self { control, stop, force_idr, closed, worker })
    }

    /// La prochaine trame encodée sera une trame clé (IDR) — pour un
    /// spectateur qui arrive ou qui a perdu pied.
    pub fn force_keyframe(&self) {
        self.force_idr.store(true, Ordering::Relaxed);
    }

    /// La source s'est évanouie (fenêtre fermée) : plus rien ne viendra,
    /// à l'appelant de conclure la diffusion.
    pub fn source_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Err(e) = self.control.stop() {
            tracing::warn!("arrêt de la capture : {e}");
        }
        let _ = self.worker.join();
    }
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
) {
    let mut encoder: Option<Encoder> = None;
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
    // La base des horodatages : l'instant zéro du stream. Tout pts_us en
    // découle — c'est lui qui portera la synchronisation A/V en S3.
    let depart = Instant::now();

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
        let pts_us = depart.elapsed().as_micros() as u64;

        // I420 exige des dimensions paires : rognées, comme au labo.
        let (w, h) = (frame.width & !1, frame.height & !1);
        if (w, h) != dims {
            dims = (w, h);
            sortie = scale::target_dims(w, h, config.max_height);
            stats.set_dims(sortie.0, sortie.1);
            encoder = None;
            yuv = Some(YUVBuffer::new(w as usize, h as usize));
            if sortie == dims {
                tracing::info!("diffusion : source {w}x{h}");
            } else {
                tracing::info!("diffusion : source {w}x{h}, émise en {}x{}", sortie.0, sortie.1);
            }
        }
        let Some(yuv_buf) = yuv.as_mut() else { continue };
        let (ow, oh) = sortie;

        // 1. Conversion BGRA -> I420, puis réduction si l'image émise est
        //    plus petite que la source.
        let t0 = Instant::now();
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
        let reduit: Option<&scale::I420> = if (ow, oh) != (w, h) {
            Some(scaler.scale(yuv_buf, ow as usize, oh as usize))
        } else {
            None
        };
        stats.convert_ms.record(t0.elapsed().as_secs_f32() * 1000.0);
        stats.converted.fetch_add(1, Ordering::Relaxed);

        // 2. Encodage — trame clé exigée si un spectateur l'attend.
        let enc = match encoder.as_mut() {
            Some(e) => e,
            None => match screen_encoder(ow, oh, config.bitrate_bps, config.fps) {
                Ok(e) => encoder.insert(e),
                Err(e) => {
                    tracing::error!("encodeur H.264 : {e:#}");
                    return;
                }
            },
        };
        if force_idr.swap(false, Ordering::Relaxed) {
            enc.force_intra_frame();
        }
        let t1 = Instant::now();
        let encode = match reduit {
            Some(petit) => enc.encode(petit),
            None => enc.encode(&*yuv_buf),
        };
        let (packet, idr) = match encode {
            Ok(bs) => {
                let idr = bs.frame_type() == FrameType::IDR;
                if idr {
                    stats.keyframes.fetch_add(1, Ordering::Relaxed);
                }
                (bs.to_vec(), idr)
            }
            Err(e) => {
                tracing::warn!("encodage : {e}");
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
        emit(EncodedFrame { data: packet, idr, pts_us, width: ow as u16, height: oh as u16 });
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
) -> anyhow::Result<Encoder> {
    let fps = fps.clamp(1, 120);
    let config = EncoderConfig::new()
        .usage_type(UsageType::ScreenContentRealTime)
        .bitrate(BitRate::from_bps(bitrate_bps))
        .max_frame_rate(FrameRate::from_hz(fps as f32))
        .intra_frame_period(IntraFramePeriod::from_num_frames(2 * fps))
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
        let mut encoder = screen_encoder(w as u32, h as u32, 500_000, 30).unwrap();

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
}
