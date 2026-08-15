//! Rééchantillonneur linéaire simple et sans allocation sur le chemin chaud.
//!
//! Suffisant pour la voix (le micro et la sortie sont presque toujours à
//! 44,1 ou 48 kHz). Remplaçable par rubato (sinc) si besoin de mieux.

use std::collections::VecDeque;

pub struct LinearResampler {
    /// Échantillons d'entrée consommés par échantillon de sortie.
    ratio: f64,
    buf: VecDeque<f32>,
    /// Position fractionnaire de lecture dans `buf`.
    pos: f64,
}

impl LinearResampler {
    pub fn new(ratio: f64) -> Self {
        assert!(ratio > 0.0);
        Self { ratio, buf: VecDeque::new(), pos: 0.0 }
    }

    pub fn push(&mut self, samples: &[f32]) {
        self.buf.extend(samples);
    }

    /// Vrai si `out_len` échantillons de sortie peuvent être produits.
    pub fn can_pull(&self, out_len: usize) -> bool {
        let end = self.pos + self.ratio * out_len as f64;
        (end.ceil() as usize) + 1 <= self.buf.len()
    }

    /// Produit `out.len()` échantillons par interpolation linéaire.
    /// Appeler `can_pull` d'abord ; sinon les échantillons manquants valent 0.
    pub fn pull(&mut self, out: &mut [f32]) {
        for o in out.iter_mut() {
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            let a = self.buf.get(i).copied().unwrap_or(0.0);
            let b = self.buf.get(i + 1).copied().unwrap_or(a);
            *o = a + (b - a) * frac;
            self.pos += self.ratio;
        }
        let consumed = (self.pos as usize).min(self.buf.len());
        self.buf.drain(..consumed);
        self.pos -= consumed as f64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_ratio_passes_through() {
        let mut r = LinearResampler::new(1.0);
        let input: Vec<f32> = (0..100).map(|i| i as f32).collect();
        r.push(&input);
        assert!(r.can_pull(64));
        let mut out = [0f32; 64];
        r.pull(&mut out);
        for (i, &s) in out.iter().enumerate() {
            assert!((s - i as f32).abs() < 1e-6);
        }
    }

    #[test]
    fn downsample_consumes_more_input() {
        // 44,1 kHz -> 48 kHz inversé : ratio > 1 consomme plus qu'il ne produit.
        let mut r = LinearResampler::new(48_000.0 / 44_100.0);
        r.push(&vec![0.5f32; 2000]);
        assert!(r.can_pull(960));
        let mut out = [0f32; 960];
        r.pull(&mut out);
        assert!(out.iter().all(|s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn cannot_pull_without_enough_input() {
        let mut r = LinearResampler::new(1.0);
        r.push(&[0.0; 100]);
        assert!(!r.can_pull(100)); // il faut un échantillon d'avance
        assert!(r.can_pull(99));
    }
}
