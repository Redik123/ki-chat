//! Les égaliseurs de ki-chat : celui de sa propre voix (ce qui part, vers
//! ki-chat comme vers le micro des jeux) et celui des voix reçues.
//!
//! Paramétriques, comme en studio : jusqu'à huit bandes, chacune avec sa
//! forme (coupe-bas, étagères, cloche, coupe-haut, réjection), sa fréquence,
//! son gain et sa largeur (Q) ; les coupe-bas et coupe-haut en 12 ou
//! 24 dB par octave. Des biquads du « cookbook » de Robert Bristow-Johnson,
//! en forme directe transposée II. Sans bande qui agisse, la chaîne est
//! court-circuitée : le chemin par défaut ne coûte rien.
//!
//! La réponse en fréquence se calcule exactement, à partir des mêmes
//! coefficients : c'est la courbe que dessine l'interface — ce qu'on voit est
//! ce qu'on entend (testé).
//!
//! Pas sur les jeux, côté écoute : toucher au son de tout le PC demanderait
//! un pilote. Et rien sur « M'écouter » ni sur les effets : ce qu'on
//! s'entend dire doit rester ce que les autres entendent.

use crate::SAMPLE_RATE;

/// La forme d'une bande.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Forme {
    /// Coupe-bas : retire ce qui est sous la fréquence (grondements, effet
    /// de proximité d'un micro collé à la bouche).
    PasseHaut,
    EtagereBasse,
    Cloche,
    EtagereHaute,
    /// Coupe-haut : retire ce qui est au-dessus (souffle, sifflantes).
    PasseBas,
    /// Réjection : creuse une fréquence étroite (un sifflement, un bourdon).
    Coupe,
}

impl Forme {
    pub const TOUTES: [Forme; 6] = [
        Forme::PasseHaut,
        Forme::EtagereBasse,
        Forme::Cloche,
        Forme::EtagereHaute,
        Forme::PasseBas,
        Forme::Coupe,
    ];

    pub fn nom(self) -> &'static str {
        match self {
            Forme::PasseHaut => "Coupe-bas",
            Forme::EtagereBasse => "Étagère grave",
            Forme::Cloche => "Cloche",
            Forme::EtagereHaute => "Étagère aiguë",
            Forme::PasseBas => "Coupe-haut",
            Forme::Coupe => "Réjection",
        }
    }

    /// Les coupes n'ont pas de gain : elles retirent, c'est tout.
    pub fn a_un_gain(self) -> bool {
        matches!(self, Forme::EtagereBasse | Forme::Cloche | Forme::EtagereHaute)
    }

    /// Les coupe-bas et coupe-haut ont une pente : 12 ou 24 dB par octave.
    pub fn a_une_pente(self) -> bool {
        matches!(self, Forme::PasseHaut | Forme::PasseBas)
    }

    fn code(self) -> &'static str {
        match self {
            Forme::PasseHaut => "ph",
            Forme::EtagereBasse => "eb",
            Forme::Cloche => "cl",
            Forme::EtagereHaute => "eh",
            Forme::PasseBas => "pb",
            Forme::Coupe => "co",
        }
    }

    fn depuis_code(code: &str) -> Option<Self> {
        Forme::TOUTES.into_iter().find(|f| f.code() == code)
    }
}

/// Au plus tant de bandes par égaliseur.
pub const BANDES_MAX: usize = 8;
/// Le gain d'une bande, dans un sens comme dans l'autre.
pub const GAIN_MAX_DB: f32 = 18.0;
pub const FREQ_MIN: f32 = 20.0;
pub const FREQ_MAX: f32 = 20_000.0;
pub const Q_MIN: f32 = 0.1;
pub const Q_MAX: f32 = 16.0;
/// Le Q d'un filtre de Butterworth du second ordre : ni bosse ni creux.
pub const Q_NEUTRE: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Une bande de l'égaliseur.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bande {
    pub forme: Forme,
    pub frequence: f32,
    pub gain_db: f32,
    pub q: f32,
    /// Coupe-bas et coupe-haut : 24 dB par octave (deux sections de
    /// Butterworth) plutôt que 12.
    pub raide: bool,
    pub active: bool,
}

impl Bande {
    pub fn new(forme: Forme, frequence: f32, gain_db: f32, q: f32) -> Self {
        Self { forme, frequence, gain_db, q, raide: false, active: true }.bornee()
    }

    /// La même, en 24 dB par octave.
    pub fn raide(mut self) -> Self {
        self.raide = true;
        self
    }

    /// Ramenée dans les plages : fréquence audible (et sous Nyquist), gain
    /// et largeur raisonnables.
    pub fn bornee(mut self) -> Self {
        let nyquist = SAMPLE_RATE as f32 * 0.45;
        self.frequence = self.frequence.clamp(FREQ_MIN, FREQ_MAX.min(nyquist));
        self.gain_db = if self.gain_db.is_finite() { self.gain_db.clamp(-GAIN_MAX_DB, GAIN_MAX_DB) } else { 0.0 };
        self.q = if self.q.is_finite() { self.q.clamp(Q_MIN, Q_MAX) } else { Q_NEUTRE };
        self
    }

    /// Les sections du second ordre de la bande : aucune quand elle n'agit
    /// pas (coupée, ou gain nul), deux pour une coupe raide.
    fn sections(&self) -> Vec<Biquad> {
        let b = self.bornee();
        if !b.active || (b.forme.a_un_gain() && b.gain_db.abs() < 0.05) {
            return Vec::new();
        }
        if b.forme.a_une_pente() && b.raide {
            // Butterworth du quatrième ordre : deux sections, deux Q.
            return [0.541_196_1, 1.306_563]
                .iter()
                .map(|&q| Biquad::new(b.forme, b.frequence, 0.0, q))
                .collect();
        }
        vec![Biquad::new(b.forme, b.frequence, b.gain_db, b.q)]
    }

    /// Ce que la bande fait, en dB, à la fréquence `f`.
    pub fn reponse_db(&self, f: f32) -> f32 {
        self.sections().iter().map(|s| s.reponse_db(f)).sum()
    }
}

/// La réponse d'une chaîne de bandes, en dB, à la fréquence `f` — la courbe
/// de l'interface.
pub fn reponse_db(bandes: &[Bande], f: f32) -> f32 {
    bandes.iter().map(|b| b.reponse_db(f)).sum()
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
    fn new(forme: Forme, frequence: f32, gain_db: f32, q: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * frequence / SAMPLE_RATE as f32;
        let (sin, cos) = w0.sin_cos();
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
            Forme::PasseHaut => (
                (1.0 + cos) / 2.0,
                -(1.0 + cos),
                (1.0 + cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
            Forme::PasseBas => (
                (1.0 - cos) / 2.0,
                1.0 - cos,
                (1.0 - cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
            Forme::Coupe => (1.0, -2.0 * cos, 1.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
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

    /// Le module de la réponse à `f`, en dB — calculé en f64 : près du
    /// continu, les coefficients d'un coupe-bas s'annulent presque, et le
    /// f32 y perdrait la courbe.
    fn reponse_db(&self, f: f32) -> f32 {
        let w = 2.0 * std::f64::consts::PI * f as f64 / SAMPLE_RATE as f64;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let (b0, b1, b2) = (self.b0 as f64, self.b1 as f64, self.b2 as f64);
        let (a1, a2) = (self.a1 as f64, self.a2 as f64);
        let (nr, ni) = (b0 + b1 * c1 + b2 * c2, -(b1 * s1 + b2 * s2));
        let (dr, di) = (1.0 + a1 * c1 + a2 * c2, -(a1 * s1 + a2 * s2));
        (10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di).max(1e-300)).max(1e-30).log10()) as f32
    }
}

/// L'égaliseur : ses bandes, et les filtres qui les jouent.
#[derive(Clone, Debug, Default)]
pub struct Egaliseur {
    bandes: Vec<Bande>,
    filtres: Vec<Biquad>,
}

impl Egaliseur {
    pub fn new(bandes: &[Bande]) -> Self {
        let mut e = Self::default();
        e.regler(bandes);
        e
    }

    /// Change les bandes. Les filtres ne sont refaits que si quelque chose a
    /// bougé : leur état (la queue du signal) survit à un réglage identique.
    pub fn regler(&mut self, bandes: &[Bande]) {
        let bandes: Vec<Bande> = bandes.iter().take(BANDES_MAX).map(|b| b.bornee()).collect();
        if bandes == self.bandes {
            return;
        }
        self.filtres = bandes.iter().flat_map(|b| b.sections()).collect();
        self.bandes = bandes;
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
        // Après un silence strict (la porte de bruit du micro en livre), la
        // queue des filtres s'éteint vers des nombres si petits que le
        // processeur les calcule cent fois plus lentement : on les remet à
        // zéro bien avant.
        for f in self.filtres.iter_mut() {
            if f.z1.abs() < 1e-15 && f.z2.abs() < 1e-15 {
                f.z1 = 0.0;
                f.z2 = 0.0;
            }
        }
    }
}

/// Des préréglages : (nom, bandes).
pub type Prereglage = (&'static str, fn() -> Vec<Bande>);

/// Pour sa voix, prise par un micro-casque.
pub const PREREGLAGES_VOIX: [Prereglage; 5] = [
    ("Neutre", Vec::new),
    // L'anti-« cave » sans « nez bouché » : on coupe SOUS la voix (le
    // grondement et l'effet de proximité), pas dans son corps.
    ("Micro-casque", || {
        vec![
            Bande::new(Forme::PasseHaut, 80.0, 0.0, Q_NEUTRE).raide(),
            Bande::new(Forme::Cloche, 250.0, -2.0, 1.0),
            Bande::new(Forme::Cloche, 3_500.0, 2.0, 0.9),
            Bande::new(Forme::EtagereHaute, 10_000.0, 2.0, Q_NEUTRE),
        ]
    }),
    // Le nasal vit vers 1 kHz ; un peu de présence et d'air autour.
    ("Moins nasal", || {
        vec![
            Bande::new(Forme::PasseHaut, 80.0, 0.0, Q_NEUTRE).raide(),
            Bande::new(Forme::Cloche, 1_000.0, -3.0, 1.4),
            Bande::new(Forme::Cloche, 4_000.0, 2.0, 1.0),
            Bande::new(Forme::EtagereHaute, 10_000.0, 2.0, Q_NEUTRE),
        ]
    }),
    // La voix d'antenne : du corps, pas de boue, de la présence.
    ("Radio", || {
        vec![
            Bande::new(Forme::PasseHaut, 90.0, 0.0, Q_NEUTRE).raide(),
            Bande::new(Forme::Cloche, 150.0, 2.0, 0.8),
            Bande::new(Forme::Cloche, 350.0, -3.0, 1.2),
            Bande::new(Forme::Cloche, 5_000.0, 3.0, 0.8),
            Bande::new(Forme::EtagereHaute, 12_000.0, 2.0, Q_NEUTRE),
        ]
    }),
    // Un micro dur, qui siffle.
    ("Plus doux", || {
        vec![
            Bande::new(Forme::Cloche, 3_000.0, -3.0, 1.0),
            Bande::new(Forme::EtagereHaute, 8_000.0, -4.0, Q_NEUTRE),
        ]
    }),
];

/// Pour les voix qu'on entend.
pub const PREREGLAGES_ECOUTE: [Prereglage; 4] = [
    ("Neutre", Vec::new),
    // Moins de boue, plus d'articulation : ce qu'on cherche en plein jeu.
    ("Voix claires", || {
        vec![
            Bande::new(Forme::PasseHaut, 90.0, 0.0, Q_NEUTRE),
            Bande::new(Forme::Cloche, 300.0, -2.0, 1.0),
            Bande::new(Forme::Cloche, 3_000.0, 3.0, 1.0),
            Bande::new(Forme::EtagereHaute, 8_000.0, 2.0, Q_NEUTRE),
        ]
    }),
    // Un casque fermé qui gonfle le bas.
    ("Moins de basses", || {
        vec![
            Bande::new(Forme::PasseHaut, 120.0, 0.0, Q_NEUTRE).raide(),
            Bande::new(Forme::EtagereBasse, 250.0, -3.0, Q_NEUTRE),
        ]
    }),
    // Des micros durs, un casque qui siffle.
    ("Plus doux", || {
        vec![
            Bande::new(Forme::Cloche, 3_000.0, -2.0, 1.0),
            Bande::new(Forme::EtagereHaute, 8_000.0, -4.0, Q_NEUTRE),
        ]
    }),
];

/// Les bandes telles que les préférences les rangent :
/// `forme:fréquence:gain:q:raide:active`, séparées par des `;`.
pub fn ecrire(bandes: &[Bande]) -> String {
    bandes
        .iter()
        .map(|b| {
            format!(
                "{}:{}:{}:{}:{}:{}",
                b.forme.code(),
                b.frequence,
                b.gain_db,
                b.q,
                b.raide as u8,
                b.active as u8
            )
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// L'inverse d'[`ecrire`] — et l'ancien format à cinq gains fixes (150,
/// 400, 1 000, 3 000, 8 000 Hz), repris tel qu'il sonnait.
pub fn lire(texte: &str) -> Vec<Bande> {
    let texte = texte.trim();
    if texte.is_empty() {
        return Vec::new();
    }
    if !texte.contains(':') {
        const ANCIENNES: [(Forme, f32); 5] = [
            (Forme::EtagereBasse, 150.0),
            (Forme::Cloche, 400.0),
            (Forme::Cloche, 1_000.0),
            (Forme::Cloche, 3_000.0),
            (Forme::EtagereHaute, 8_000.0),
        ];
        return ANCIENNES
            .iter()
            .zip(texte.split(','))
            .filter_map(|(&(forme, f), g)| {
                let g: f32 = g.trim().parse().ok()?;
                (g.abs() >= 0.05).then(|| Bande::new(forme, f, g, Q_NEUTRE))
            })
            .collect();
    }
    texte
        .split(';')
        .filter_map(|b| {
            let c: Vec<&str> = b.split(':').collect();
            let [forme, f, g, q, raide, active] = c.as_slice() else { return None };
            Some(
                Bande {
                    forme: Forme::depuis_code(forme)?,
                    frequence: f.parse().ok()?,
                    gain_db: g.parse().ok()?,
                    q: q.parse().ok()?,
                    raide: *raide == "1",
                    active: *active != "0",
                }
                .bornee(),
            )
        })
        .take(BANDES_MAX)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gain (dB) mesuré d'un sinus pur à `frequence` à travers l'égaliseur,
    /// en régime établi.
    fn mesure(bandes: &[Bande], frequence: f32) -> f32 {
        let mut e = Egaliseur::new(bandes);
        let n = SAMPLE_RATE as usize;
        let mut s: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * frequence * i as f32 / SAMPLE_RATE as f32).sin() * 0.25)
            .collect();
        e.traiter_trame(&mut s);
        let crete = s[n / 2..].iter().fold(0f32, |m, x| m.max(x.abs()));
        20.0 * (crete / 0.25).log10()
    }

    /// Sans bande, l'égaliseur ne touche à rien — pas même un bit.
    #[test]
    fn sans_bande_rien_ne_bouge() {
        for bandes in [Vec::new(), vec![Bande::new(Forme::Cloche, 1_000.0, 0.0, 1.0)]] {
            let mut e = Egaliseur::new(&bandes);
            let entree: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.01).sin() * 0.3).collect();
            let mut sortie = entree.clone();
            e.traiter_trame(&mut sortie);
            assert_eq!(entree, sortie);
        }
    }

    /// Ce qu'on voit est ce qu'on entend : la courbe calculée colle au son
    /// mesuré, pour chaque forme, à plusieurs fréquences.
    #[test]
    fn la_courbe_dit_ce_que_fait_le_son() {
        let cas = [
            Bande::new(Forme::PasseHaut, 100.0, 0.0, Q_NEUTRE).raide(),
            Bande::new(Forme::PasseHaut, 100.0, 0.0, Q_NEUTRE),
            Bande::new(Forme::EtagereBasse, 200.0, -6.0, Q_NEUTRE),
            Bande::new(Forme::Cloche, 1_000.0, 6.0, 2.0),
            Bande::new(Forme::EtagereHaute, 6_000.0, 4.0, Q_NEUTRE),
            Bande::new(Forme::PasseBas, 5_000.0, 0.0, Q_NEUTRE),
            Bande::new(Forme::Coupe, 2_000.0, 0.0, 4.0),
        ];
        for b in cas {
            for f in [50.0, 150.0, 700.0, 1_000.0, 2_500.0, 9_000.0] {
                let calcule = reponse_db(&[b], f);
                let mesure = mesure(&[b], f);
                // Au fond des coupes, la mesure bute sur la précision : on
                // compare jusqu'à -40 dB.
                if calcule > -40.0 {
                    assert!(
                        (calcule - mesure).abs() < 0.6,
                        "{:?} à {f} Hz : courbe {calcule:.1} dB, son {mesure:.1} dB",
                        b.forme
                    );
                }
            }
        }
    }

    /// Chaque forme fait ce que son nom dit.
    #[test]
    fn chaque_forme_fait_son_travail() {
        let ph = Bande::new(Forme::PasseHaut, 100.0, 0.0, Q_NEUTRE);
        assert!((reponse_db(&[ph], 100.0) + 3.0).abs() < 0.2, "-3 dB à la coupure");
        assert!(reponse_db(&[ph], 1_000.0).abs() < 0.2, "rien au-dessus");
        // 12 dB par octave, puis 24 en raide.
        assert!((reponse_db(&[ph], 25.0) + 24.0).abs() < 1.5);
        assert!((reponse_db(&[ph.raide()], 25.0) + 48.0).abs() < 2.0);
        let cloche = Bande::new(Forme::Cloche, 1_000.0, 9.0, 1.4);
        assert!((reponse_db(&[cloche], 1_000.0) - 9.0).abs() < 0.1);
        assert!(reponse_db(&[cloche], 100.0).abs() < 0.5);
        let coupe = Bande::new(Forme::Coupe, 2_000.0, 0.0, 4.0);
        assert!(reponse_db(&[coupe], 2_000.0) < -40.0);
        assert!(reponse_db(&[coupe], 1_000.0).abs() < 1.0);
    }

    /// Une bande coupée ne fait rien ; une bande bornée reste dans ses plages.
    #[test]
    fn une_bande_coupee_ou_aberrante_ne_fait_pas_de_degats() {
        let mut b = Bande::new(Forme::Cloche, 1_000.0, 12.0, 1.0);
        b.active = false;
        assert_eq!(reponse_db(&[b], 1_000.0), 0.0);
        let folle = Bande { forme: Forme::Cloche, frequence: 1e9, gain_db: 99.0, q: 0.0, raide: false, active: true }
            .bornee();
        assert!(folle.frequence <= FREQ_MAX && folle.gain_db == GAIN_MAX_DB && folle.q == Q_MIN);
        let nan = Bande { gain_db: f32::NAN, q: f32::NAN, ..folle }.bornee();
        assert_eq!((nan.gain_db, nan.q), (0.0, Q_NEUTRE));
    }

    /// Après un silence strict (la porte de bruit du micro en livre), les
    /// filtres retombent à zéro exact au lieu de s'éteindre sans fin vers
    /// des nombres minuscules, que le processeur calcule très lentement.
    #[test]
    fn apres_un_silence_les_filtres_retombent_a_zero() {
        let mut e = Egaliseur::new(&(PREREGLAGES_VOIX[1].1)());
        let mut voix: Vec<f32> = (0..960).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        e.traiter_trame(&mut voix);
        // Un silence neuf à chaque trame : la sortie ne doit pas repartir en
        // entrée, elle ferait boucle.
        let mut derniere = [0f32; 960];
        for _ in 0..50 {
            derniere = [0f32; 960];
            e.traiter_trame(&mut derniere);
        }
        assert!(e.filtres.iter().all(|f| f.z1 == 0.0 && f.z2 == 0.0));
        assert!(derniere.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn les_preferences_font_l_aller_retour() {
        for (_, fabrique) in PREREGLAGES_VOIX.iter().chain(PREREGLAGES_ECOUTE.iter()) {
            let bandes = fabrique();
            assert_eq!(lire(&ecrire(&bandes)), bandes);
            assert!(bandes.len() <= BANDES_MAX);
        }
        assert!(lire("").is_empty());
        assert!(lire("n'importe quoi").is_empty());
    }

    /// Les réglages de l'ancien égaliseur à cinq gains sont repris tels
    /// qu'ils sonnaient.
    #[test]
    fn l_ancien_format_est_repris() {
        let bandes = lire("-6,-3,0,0,0");
        assert_eq!(bandes.len(), 2);
        assert_eq!((bandes[0].forme, bandes[0].frequence, bandes[0].gain_db), (Forme::EtagereBasse, 150.0, -6.0));
        assert_eq!((bandes[1].forme, bandes[1].frequence, bandes[1].gain_db), (Forme::Cloche, 400.0, -3.0));
        assert!(lire("0,0,0,0,0").is_empty());
    }
}
