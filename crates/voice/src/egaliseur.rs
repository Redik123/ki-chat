//! L'égaliseur de ce qu'on entend dans ki-chat : cinq bandes taillées pour
//! la voix, sur le mix des voix reçues seulement.
//!
//! Pas sur les jeux : pour toucher au son de tout le PC, il faudrait un
//! pilote. Pas non plus sur « M'écouter » ni sur les effets : ce qu'on
//! s'entend dire doit rester ce que les autres entendent, et une
//! notification n'a rien à gagner à être égalisée.
//!
//! Des biquads du « cookbook » de Robert Bristow-Johnson, en forme directe
//! transposée II : une étagère aux deux bouts, trois cloches au milieu. Tout
//! à 0 dB, la chaîne est court-circuitée — le chemin par défaut ne coûte
//! rien.

use crate::SAMPLE_RATE;

/// Les bandes : (nom, fréquence centrale ou de coin en Hz).
pub const BANDES: [(&str, f32); 5] = [
    ("Graves", 150.0),
    ("Bas-médiums", 400.0),
    ("Médiums", 1_000.0),
    ("Présence", 3_000.0),
    ("Aigus", 8_000.0),
];

/// Le gain maximal d'une bande, dans un sens comme dans l'autre.
pub const GAIN_MAX_DB: f32 = 12.0;

/// Les préréglages proposés : (nom, gains en dB par bande).
pub const PREREGLAGES: [(&str, [f32; 5]); 4] = [
    ("Neutre", [0.0, 0.0, 0.0, 0.0, 0.0]),
    // Moins de boue, plus d'articulation : ce qu'on cherche en plein jeu.
    ("Voix claires", [-3.0, -2.0, 0.0, 3.0, 2.0]),
    // Un casque fermé qui gonfle le bas, un micro trop près de la bouche.
    ("Moins de basses", [-6.0, -3.0, 0.0, 0.0, 0.0]),
    // Des micros durs, un casque qui siffle.
    ("Plus doux", [2.0, 1.0, 0.0, -2.0, -4.0]),
];

#[derive(Clone, Copy, Debug)]
enum Forme {
    EtagereBasse,
    Cloche,
    EtagereHaute,
}

/// Un filtre du second ordre, normalisé (a0 = 1).
#[derive(Clone, Copy, Debug)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    fn new(forme: Forme, frequence: f32, gain_db: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * frequence / SAMPLE_RATE as f32;
        let (sin, cos) = w0.sin_cos();
        // Q = 0,707 : près de deux octaves de large pour les cloches, et la
        // pente douce (S = 1) pour les étagères.
        let q = std::f32::consts::FRAC_1_SQRT_2;
        let alpha = sin / (2.0 * q);
        let (b0, b1, b2, a0, a1, a2) = match forme {
            Forme::Cloche => (
                1.0 + alpha * a,
                -2.0 * cos,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos,
                1.0 - alpha / a,
            ),
            Forme::EtagereBasse => {
                let r = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cos + r),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                    a * ((a + 1.0) - (a - 1.0) * cos - r),
                    (a + 1.0) + (a - 1.0) * cos + r,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - r,
                )
            }
            Forme::EtagereHaute => {
                let r = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cos + r),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                    a * ((a + 1.0) + (a - 1.0) * cos - r),
                    (a + 1.0) - (a - 1.0) * cos + r,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - r,
                )
            }
        };
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[inline]
    fn traiter(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// L'égaliseur : cinq filtres en série, ou rien du tout quand tout est à plat.
#[derive(Clone, Debug, Default)]
pub struct Egaliseur {
    filtres: Vec<Biquad>,
    gains: [f32; 5],
}

impl Egaliseur {
    /// Un égaliseur réglé sur ces gains (dB, bornés à ±`GAIN_MAX_DB`).
    pub fn new(gains: [f32; 5]) -> Self {
        let mut e = Self::default();
        e.regler(gains);
        e
    }

    /// Change les gains. Les filtres ne sont refaits que si quelque chose a
    /// bougé : leur état (la queue du signal) survit à un réglage identique.
    pub fn regler(&mut self, gains: [f32; 5]) {
        let gains = gains.map(|g| g.clamp(-GAIN_MAX_DB, GAIN_MAX_DB));
        if gains == self.gains && (self.filtres.is_empty() == Self::a_plat(&gains)) {
            return;
        }
        self.gains = gains;
        self.filtres.clear();
        if Self::a_plat(&gains) {
            return;
        }
        for (i, ((_, frequence), gain)) in BANDES.iter().zip(gains).enumerate() {
            let forme = match i {
                0 => Forme::EtagereBasse,
                4 => Forme::EtagereHaute,
                _ => Forme::Cloche,
            };
            self.filtres.push(Biquad::new(forme, *frequence, gain));
        }
    }

    fn a_plat(gains: &[f32; 5]) -> bool {
        gains.iter().all(|g| g.abs() < 0.05)
    }

    /// Oublie la queue du signal : la prochaine trame repart d'un silence.
    pub fn reinitialiser(&mut self) {
        for f in self.filtres.iter_mut() {
            f.z1 = 0.0;
            f.z2 = 0.0;
        }
    }

    /// Égalise une trame en place.
    pub fn traiter_trame(&mut self, trame: &mut [f32]) {
        if self.filtres.is_empty() {
            return;
        }
        for s in trame.iter_mut() {
            let mut x = *s;
            for f in self.filtres.iter_mut() {
                x = f.traiter(x);
            }
            *s = x;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gain (dB) d'un sinus pur à `frequence` à travers l'égaliseur, en
    /// régime établi.
    fn gain_a(e: &mut Egaliseur, frequence: f32) -> f32 {
        let n = SAMPLE_RATE as usize;
        let mut s: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * frequence * i as f32 / SAMPLE_RATE as f32).sin() * 0.25)
            .collect();
        e.traiter_trame(&mut s);
        let crete = s[n / 2..].iter().fold(0f32, |m, x| m.max(x.abs()));
        20.0 * (crete / 0.25).log10()
    }

    /// À plat, l'égaliseur ne touche à rien — pas même un bit.
    #[test]
    fn a_plat_rien_ne_bouge() {
        let mut e = Egaliseur::new([0.0; 5]);
        let entree: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.01).sin() * 0.3).collect();
        let mut sortie = entree.clone();
        e.traiter_trame(&mut sortie);
        assert_eq!(entree, sortie);
    }

    /// Chaque bande relève sa fréquence du gain demandé, et laisse les
    /// autres à peu près tranquilles.
    #[test]
    fn chaque_bande_agit_ou_on_l_attend() {
        // Où mesurer l'effet (les étagères au fond de leur plateau, les
        // cloches à leur centre), et où il doit s'être éteint.
        let mesures = [(30.0, 2_000.0), (400.0, 2_400.0), (1_000.0, 6_000.0), (3_000.0, 500.0), (20_000.0, 1_000.0)];
        for (i, ((nom, _), (ou, loin))) in BANDES.iter().zip(mesures).enumerate() {
            let mut gains = [0.0; 5];
            gains[i] = 9.0;
            let g = gain_a(&mut Egaliseur::new(gains), ou);
            assert!((g - 9.0).abs() < 1.5, "{nom} : {g:.1} dB à {ou} Hz au lieu de 9");
            let g_loin = gain_a(&mut Egaliseur::new(gains), loin);
            assert!(g_loin.abs() < 3.0, "{nom} déborde : {g_loin:.1} dB à {loin} Hz");
        }
    }

    /// Les gains sont bornés : un réglage aberrant ne fait pas exploser le son.
    #[test]
    fn les_gains_sont_bornes() {
        let mut e = Egaliseur::new([40.0, 0.0, 0.0, 0.0, 0.0]);
        let g = gain_a(&mut e, 50.0);
        assert!(g <= GAIN_MAX_DB + 1.0, "{g:.1} dB");
    }

    /// Les préréglages restent dans les bornes.
    #[test]
    fn les_prereglages_sont_dans_les_bornes() {
        for (nom, gains) in PREREGLAGES {
            assert!(gains.iter().all(|g| g.abs() <= GAIN_MAX_DB), "{nom}");
        }
    }
}
