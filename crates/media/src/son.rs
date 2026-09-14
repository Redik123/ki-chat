//! Du son tel que le décodeur le rend (entrelacé, à sa cadence) vers ce que
//! le moteur vocal joue : mono, 48 kHz.
//!
//! Le mélange fait la moyenne des canaux — deux voies stéréo, ou les six
//! d'un 5.1, chacune à part égale : pas de norme savante, un clip de jeu
//! n'en a pas besoin. Le rééchantillonnage est une interpolation cubique à
//! quatre points, la même famille que celle du moteur vocal : sans
//! sifflement audible sur une voix ou une musique, et sans filtre à régler.

use crate::CADENCE;

/// Mélange `canaux` voies entrelacées en une : la moyenne.
pub fn en_mono(entrelace: &[f32], canaux: usize, sortie: &mut Vec<f32>) {
    sortie.clear();
    let canaux = canaux.max(1);
    let inv = 1.0 / canaux as f32;
    sortie.extend(
        entrelace
            .chunks_exact(canaux)
            .map(|trame| trame.iter().sum::<f32>() * inv),
    );
}

/// Rééchantillonneur mono vers 48 kHz, avec mémoire entre deux appels :
/// les blocs se suivent sans couture.
pub struct Reechantillonneur {
    /// Avance de la lecture par échantillon produit (source / 48 000).
    pas: f64,
    /// Position fractionnaire dans `attente`.
    position: f64,
    /// Les échantillons non encore consommés, précédés des trois derniers du
    /// bloc d'avant (l'interpolation regarde en arrière et en avant).
    attente: Vec<f32>,
}

impl Reechantillonneur {
    pub fn new(cadence_source: u32) -> Self {
        Self {
            pas: f64::from(cadence_source.max(1)) / f64::from(CADENCE),
            position: 0.0,
            attente: vec![0.0; 3],
        }
    }

    /// Vrai s'il n'y a rien à faire : la source est déjà à 48 kHz.
    pub fn identite(&self) -> bool {
        (self.pas - 1.0).abs() < 1e-9
    }

    /// Repart de zéro (après une recherche dans le fichier).
    pub fn reinitialiser(&mut self) {
        self.position = 0.0;
        self.attente.clear();
        self.attente.resize(3, 0.0);
    }

    /// Consomme `mono` (à la cadence source) et pousse le 48 kHz dans `sortie`.
    pub fn pousser(&mut self, mono: &[f32], sortie: &mut Vec<f32>) {
        if self.identite() {
            sortie.extend_from_slice(mono);
            return;
        }
        self.attente.extend_from_slice(mono);
        // Il faut un point après celui de droite : on s'arrête deux
        // échantillons avant la fin.
        while self.position + 2.0 < (self.attente.len() - 1) as f64 {
            let i = self.position.floor() as usize;
            let t = (self.position - i as f64) as f32;
            let a = &self.attente;
            sortie.push(cubique(a[i], a[i + 1], a[i + 2], a[i + 3], t));
            self.position += self.pas;
        }
        // On garde les trois derniers points consommés pour la suite.
        let consomme = self.position.floor() as usize;
        if consomme > 0 {
            self.attente.drain(..consomme);
            self.position -= consomme as f64;
        }
    }
}

/// Interpolation de Catmull-Rom entre `p1` et `p2` (`t` dans [0, 1]).
fn cubique(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let a = -0.5 * p0 + 1.5 * p1 - 1.5 * p2 + 0.5 * p3;
    let b = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
    let c = -0.5 * p0 + 0.5 * p2;
    ((a * t + b) * t + c) * t + p1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_mono_est_la_moyenne_des_voies() {
        let mut out = Vec::new();
        en_mono(&[1.0, 0.0, 0.5, 0.5, -1.0, 1.0], 2, &mut out);
        assert_eq!(out, vec![0.5, 0.5, 0.0]);
        en_mono(&[0.3, 0.3], 1, &mut out);
        assert_eq!(out, vec![0.3, 0.3]);
    }

    #[test]
    fn a_48_khz_rien_ne_change() {
        let mut r = Reechantillonneur::new(48_000);
        assert!(r.identite());
        let mut out = Vec::new();
        r.pousser(&[0.1, 0.2, 0.3], &mut out);
        assert_eq!(out, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn de_44100_a_48000_la_longueur_suit_le_rapport() {
        let mut r = Reechantillonneur::new(44_100);
        let mut out = Vec::new();
        let bloc: Vec<f32> = (0..441).map(|i| (i as f32 * 0.05).sin()).collect();
        for _ in 0..100 {
            r.pousser(&bloc, &mut out);
        }
        // 44 100 échantillons entrés → 48 000 attendus, à quelques-uns près
        // (l'amorce de trois points).
        assert!((47_990..=48_010).contains(&out.len()), "{}", out.len());
        // Pas d'explosion : une sinusoïde reste dans [-1, 1] et lisse.
        assert!(out.iter().all(|v| v.abs() <= 1.01));
        let saut_max = out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(saut_max < 0.06, "saut {saut_max}");
    }

    #[test]
    fn une_constante_reste_constante() {
        let mut r = Reechantillonneur::new(22_050);
        let mut out = Vec::new();
        r.pousser(&[0.5; 2205], &mut out);
        r.pousser(&[0.5; 2205], &mut out);
        // Passée l'amorce (les trois zéros de départ), tout vaut 0,5.
        assert!(out[8..].iter().all(|v| (v - 0.5).abs() < 1e-4));
    }
}
