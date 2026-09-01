//! Instrumentation du pipeline vidéo, étage par étage.
//!
//! C'est l'outil de validation de S1a : chaque étage (capture, conversion,
//! encodage, décodage, affichage) compte ses trames et publie une moyenne
//! glissante de son coût. Tout est atomique : lisible depuis l'UI à chaque
//! frame sans verrou.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

/// Moyenne glissante exponentielle d'une durée, stockée en bits f32.
#[derive(Default)]
pub struct EwmaMs(AtomicU32);

impl EwmaMs {
    pub fn record(&self, ms: f32) {
        let old = f32::from_bits(self.0.load(Ordering::Relaxed));
        let new = if old == 0.0 { ms } else { old + (ms - old) * 0.1 };
        self.0.store(new.to_bits(), Ordering::Relaxed);
    }

    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
}

/// Compteurs d'un pipeline vidéo complet. Partagé par `Arc`.
#[derive(Default)]
pub struct StageStats {
    /// Trames livrées par la capture.
    pub captured: AtomicU64,
    /// Trames abandonnées car l'encodeur était occupé (saut volontaire).
    pub skipped: AtomicU64,
    /// Trames converties BGRA -> I420.
    pub converted: AtomicU64,
    /// Trames encodées.
    pub encoded: AtomicU64,
    /// Dont trames clés (IDR).
    pub keyframes: AtomicU64,
    /// Octets H.264 produits.
    pub encoded_bytes: AtomicU64,
    /// Trames décodées.
    pub decoded: AtomicU64,
    /// Trames réellement peintes par l'UI (incrémenté par la GUI).
    pub painted: AtomicU64,
    /// Trames que l'encodeur a choisi de sauter (budget de débit dépassé).
    pub enc_skipped: AtomicU64,
    /// Trames encodées puis jetées faute de place vers le réseau.
    pub net_dropped: AtomicU64,
    /// L'encodeur en service est matériel (NVENC).
    pub materiel: std::sync::atomic::AtomicBool,

    pub convert_ms: EwmaMs,
    pub encode_ms: EwmaMs,
    pub decode_ms: EwmaMs,

    /// Dimensions courantes de la source (bits hauts = largeur).
    dims: AtomicU64,
    /// Départ de la session (pour les débits moyens).
    started: std::sync::OnceLock<Instant>,
}

impl StageStats {
    pub fn mark_started(&self) {
        let _ = self.started.set(Instant::now());
    }

    pub fn elapsed_secs(&self) -> f32 {
        self.started.get().map(|t| t.elapsed().as_secs_f32()).unwrap_or(0.0).max(0.001)
    }

    pub fn set_dims(&self, w: u32, h: u32) {
        self.dims.store(((w as u64) << 32) | h as u64, Ordering::Relaxed);
    }

    pub fn dims(&self) -> (u32, u32) {
        let d = self.dims.load(Ordering::Relaxed);
        ((d >> 32) as u32, d as u32)
    }

    /// Débit vidéo moyen en kbps depuis le début de session.
    pub fn kbps(&self) -> f32 {
        self.encoded_bytes.load(Ordering::Relaxed) as f32 * 8.0 / 1000.0 / self.elapsed_secs()
    }

    /// Résumé une ligne pour les logs / l'exemple headless.
    pub fn summary(&self) -> String {
        let secs = self.elapsed_secs();
        let (w, h) = self.dims();
        format!(
            "{}x{} | capturées {:.1}/s (sautées {}) | encodées {:.1}/s dont {} IDR à {:.0} kbps | \
             décodées {:.1}/s | peintes {:.1}/s | conv {:.1} ms, enc {:.1} ms, déc {:.1} ms",
            w,
            h,
            self.captured.load(Ordering::Relaxed) as f32 / secs,
            self.skipped.load(Ordering::Relaxed),
            self.encoded.load(Ordering::Relaxed) as f32 / secs,
            self.keyframes.load(Ordering::Relaxed),
            self.kbps(),
            self.decoded.load(Ordering::Relaxed) as f32 / secs,
            self.painted.load(Ordering::Relaxed) as f32 / secs,
            self.convert_ms.get(),
            self.encode_ms.get(),
            self.decode_ms.get(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ewma_converges() {
        let e = EwmaMs::default();
        e.record(10.0);
        assert!((e.get() - 10.0).abs() < 1e-3);
        for _ in 0..200 {
            e.record(20.0);
        }
        assert!((e.get() - 20.0).abs() < 0.5);
    }

    #[test]
    fn dims_roundtrip() {
        let s = StageStats::default();
        s.set_dims(1920, 1080);
        assert_eq!(s.dims(), (1920, 1080));
    }
}
