//! Pipeline vidéo du partage d'écran — voir PLAN-STREAM.md.
//!
//! État actuel : S1a, la boucle locale — capture d'écran -> conversion
//! I420 -> encodage H.264 -> décodage -> image RGBA prête à peindre, le
//! tout instrumenté étage par étage (`StageStats`). Aucun réseau ici : le
//! transport arrivera en S1b, par-dessus les mêmes briques.

pub mod capture;
pub mod stats;

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

        let control = capture::start_primary_monitor(capture::CaptureFlags {
            stats: stats.clone(),
            tx: frame_tx,
            recycle: recycle_rx,
        })?;

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
            None => match screen_encoder(w, h, 6_000_000) {
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
/// GOP de 60 trames (2 s à 30 fps) — les trames clés à la demande viendront
/// de `force_intra_frame` en S1b.
pub fn screen_encoder(width: u32, height: u32, bitrate_bps: u32) -> anyhow::Result<Encoder> {
    let config = EncoderConfig::new()
        .usage_type(UsageType::ScreenContentRealTime)
        .bitrate(BitRate::from_bps(bitrate_bps))
        .max_frame_rate(FrameRate::from_hz(30.0))
        .intra_frame_period(IntraFramePeriod::from_num_frames(60))
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
        let mut encoder = screen_encoder(w as u32, h as u32, 500_000).unwrap();

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
