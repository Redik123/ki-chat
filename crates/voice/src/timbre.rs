//! Le timbre de la voix : son enveloppe spectrale — les formants, la forme
//! de la bouche et de la gorge —, mesurée trame à trame et déplacée à
//! volonté, **séparément de la hauteur**. Sculptés par cette même
//! enveloppe : un souffle qui dit les mêmes mots (le chuchotement), et une
//! note de synthèse qui parle avec ta bouche (le vocodeur).
//!
//! Une analyse-synthèse de Fourier à court terme : fenêtres de 1024 points
//! (21 ms) tous les 256, racine de Hann à l'analyse comme à la synthèse. Sur
//! chaque trame, l'enveloppe se lit par le cepstre — le logarithme du
//! spectre, lissé en n'en gardant que les premiers coefficients —, puis le
//! spectre est multiplié par le rapport entre l'enveloppe voulue (la même,
//! étirée) et l'enveloppe mesurée.
//!
//! Le décaleur de hauteur, en amont, déplace les formants avec la hauteur,
//! comme une bande qu'on accélère : c'est la voix d'écureuil. Ce rapport les
//! remet où l'on veut — à leur place (une voix plus grave, mais la même
//! bouche), ou ailleurs (une bouche plus grande, plus petite). Il se calcule
//! sur la voix déjà décalée, trame par trame : rien à réaligner dans le
//! temps. 21 ms de retard, et seulement quand il travaille.

use std::sync::Arc;

use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use crate::egaliseur::{Bande, Egaliseur, Forme, Q_NEUTRE};
use crate::SAMPLE_RATE;

/// La taille des trames d'analyse : 21 ms, le retard de l'étage.
pub const TAILLE_FFT: usize = 1024;
/// Le pas entre deux trames : trois quarts de recouvrement.
const PAS: usize = TAILLE_FFT / 4;
pub(crate) const CASES: usize = TAILLE_FFT / 2 + 1;
/// Le lissage du cepstre : on n'en garde que les premiers coefficients,
/// jusqu'à la moitié de la période de la voix — les harmoniques n'y entrent
/// pas, les formants si. Une voix grave (150 Hz, 320 échantillons) garde 160
/// coefficients, une enveloppe fine ; une voix très aiguë, moins. Sans
/// hauteur mesurée (une consonne, un souffle), un entre-deux.
const LIFTRE_MIN: usize = 40;
const LIFTRE_MAX: usize = 200;
const LIFTRE_SANS_HAUTEUR: usize = 100;

/// Le lissage pour une voix de cette période (en échantillons).
fn liftre(periode: Option<f32>) -> usize {
    periode.map(|p| ((0.5 * p) as usize).clamp(LIFTRE_MIN, LIFTRE_MAX)).unwrap_or(LIFTRE_SANS_HAUTEUR)
}
/// Le rapport d'enveloppes est borné à ±24 dB : au-delà, on relèverait du
/// bruit là où la voix n'a rien.
const GAIN_MAX: f32 = 2.763; // ln(15,85) : 24 dB en logarithme naturel
/// Le niveau de la voix retouchée reste à ±6 dB de celui d'origine.
const NIVEAU_MAX: f32 = 2.0;

/// L'estimation de hauteur (YIN) travaille à 12 kHz : assez pour la
/// fondamentale et ses premiers harmoniques, quatre fois moins de calcul.
const DECIMATION: usize = 4;
const TAUX_YIN: f32 = SAMPLE_RATE as f32 / DECIMATION as f32;
/// L'historique décimé : 43 ms, deux périodes d'une voix très grave.
const HISTOIRE_YIN: usize = 2048 / DECIMATION;

/// Ce que fait l'étage, trame par trame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReglagesTimbre {
    /// L'étirement lu dans l'enveloppe : 1,0 la laisse en place. Pour des
    /// formants à `alpha` fois leur place d'origine après un décalage de
    /// hauteur de rapport `r`, il vaut `r / alpha`.
    pub etirement: f32,
    /// Le chuchotement (0 à 1) : un souffle ajouté à la voix, qui la
    /// remplace tout à fait à 1.
    pub chuchotement: f32,
    /// Le vocodeur (0 à 1) : la part de la voix de synthèse.
    pub vocodeur: f32,
    /// La note du vocodeur : `None`, elle suit la voix (au demi-ton près,
    /// ce qui fait le robot) ; sinon une note fixe, en Hz.
    pub vocodeur_note: Option<f32>,
}

impl ReglagesTimbre {
    /// Rien à faire : l'étage peut être retiré.
    pub fn neutre(&self) -> bool {
        (self.etirement - 1.0).abs() < 0.002 && self.chuchotement < 0.005 && self.vocodeur < 0.005
    }
}

/// La porteuse du vocodeur : une dent de scie adoucie à ses ruptures
/// (polyBLEP, pas de repliement audible), ou un souffle sur les consonnes.
#[derive(Clone, Debug)]
struct Porteuse {
    phase: f32,
    pas: f32,
    voisee: bool,
    alea: u32,
}

impl Porteuse {
    fn suivant(&mut self) -> f32 {
        if !self.voisee {
            return 0.5 * bruit(&mut self.alea);
        }
        let t = self.phase;
        let dt = self.pas;
        let mut y = 2.0 * t - 1.0;
        // La correction polyBLEP, de part et d'autre de la rupture.
        if t < dt {
            let x = t / dt;
            y -= x + x - x * x - 1.0;
        } else if t > 1.0 - dt {
            let x = (t - 1.0) / dt;
            y -= x * x + x + x + 1.0;
        }
        self.phase += dt;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        0.5 * y
    }
}

/// Un bruit blanc uniforme entre -1 et 1 (xorshift : rapide, sans état
/// partagé).
fn bruit(etat: &mut u32) -> f32 {
    let mut x = *etat;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *etat = x;
    (x as f32 / u32::MAX as f32) * 2.0 - 1.0
}

/// La hauteur d'un signal par YIN (de Cheveigné et Kawahara, 2002) :
/// `(fréquence, apériodicité)`. L'apériodicité va de 0 (parfaitement
/// périodique) à 1 (du bruit) ; au-dessus de ~0,3, ce n'est plus une voix
/// qui chante ses voyelles. `None` : rien d'assez périodique entre `f_min` et
/// `f_max`.
pub fn yin(x: &[f32], taux: f32, f_min: f32, f_max: f32) -> Option<(f32, f32)> {
    let tau_min = ((taux / f_max).floor() as usize).max(2);
    let tau_max = (taux / f_min).ceil() as usize;
    if x.len() < 2 * tau_max + 2 {
        return None;
    }
    let fenetre = x.len() - tau_max - 1;
    let mut d = vec![0f32; tau_max + 2];
    for (tau, dt) in d.iter_mut().enumerate().skip(1) {
        let mut s = 0.0;
        for j in 0..fenetre {
            let e = x[j] - x[j + tau];
            s += e * e;
        }
        *dt = s;
    }
    // La différence cumulée normalisée.
    let mut cumul = 0.0;
    let mut dn = vec![1f32; tau_max + 2];
    for tau in 1..=tau_max + 1 {
        cumul += d[tau];
        dn[tau] = if cumul > 0.0 { d[tau] * tau as f32 / cumul } else { 1.0 };
    }
    // Le premier creux sous le seuil, descendu jusqu'à son fond ; sinon le
    // plus profond.
    let mut choix = None;
    let mut tau = tau_min;
    while tau <= tau_max {
        if dn[tau] < 0.15 {
            while tau < tau_max && dn[tau + 1] < dn[tau] {
                tau += 1;
            }
            choix = Some(tau);
            break;
        }
        tau += 1;
    }
    let mut tau = choix.unwrap_or_else(|| {
        (tau_min..=tau_max).min_by(|&a, &b| dn[a].total_cmp(&dn[b])).unwrap_or(tau_min)
    });
    // L'erreur d'octave : quand un formant bas renforce le deuxième
    // harmonique (le « i », formant à 270 Hz sur une voix à 120), le signal
    // se répète presque à la demi-période. S'il se répète nettement mieux au
    // double, c'est le double la vraie période.
    if 2 * tau + 2 <= tau_max {
        let double = (2 * tau - 2..=2 * tau + 2).min_by(|&a, &b| dn[a].total_cmp(&dn[b])).unwrap_or(2 * tau);
        if dn[double] < 0.5 * dn[tau] {
            tau = double;
        }
    }
    // Affinage parabolique autour du creux.
    let (a, b, c) = (dn[tau - 1], dn[tau], dn[tau + 1]);
    let courbure = a + c - 2.0 * b;
    let decalage = if courbure.abs() > 1e-9 { (0.5 * (a - c) / courbure).clamp(-0.5, 0.5) } else { 0.0 };
    let periode = tau as f32 + decalage;
    Some((taux / periode, b.clamp(0.0, 1.0)))
}

/// Les transformées d'une trame, et leurs brouillons : tout est alloué une
/// fois pour toutes, rien ne l'est dans le fil audio.
#[derive(Clone)]
struct Fourier {
    avant: Arc<dyn RealToComplex<f32>>,
    arriere: Arc<dyn ComplexToReal<f32>>,
    brouillon_avant: Vec<Complex32>,
    brouillon_arriere: Vec<Complex32>,
    reel: Vec<f32>,
    spectre: Vec<Complex32>,
}

impl Fourier {
    fn new() -> Self {
        let mut plan = RealFftPlanner::<f32>::new();
        let avant = plan.plan_fft_forward(TAILLE_FFT);
        let arriere = plan.plan_fft_inverse(TAILLE_FFT);
        Self {
            brouillon_avant: avant.make_scratch_vec(),
            brouillon_arriere: arriere.make_scratch_vec(),
            reel: avant.make_input_vec(),
            spectre: avant.make_output_vec(),
            avant,
            arriere,
        }
    }

    /// Le spectre de `self.reel` dans `self.spectre` (le réel est détruit).
    fn directe(&mut self) {
        let _ = self.avant.process_with_scratch(&mut self.reel, &mut self.spectre, &mut self.brouillon_avant);
    }

    /// Le signal de `self.spectre` dans `self.reel`, non normalisé (×N).
    fn inverse(&mut self) {
        self.spectre[0].im = 0.0;
        self.spectre[CASES - 1].im = 0.0;
        let _ = self.arriere.process_with_scratch(&mut self.spectre, &mut self.reel, &mut self.brouillon_arriere);
    }

    /// L'enveloppe d'un spectre en logarithme naturel de l'amplitude, par
    /// le cepstre lissé à `liftre` coefficients.
    fn enveloppe(&mut self, spectre: &[Complex32], sortie: &mut [f32], liftre: usize) {
        for (s, c) in self.spectre.iter_mut().zip(spectre) {
            *s = Complex32::new(0.5 * (c.norm_sqr() + 1e-18).ln(), 0.0);
        }
        self.inverse();
        // Le cepstre, lissé : ses premiers coefficients, et leurs symétriques.
        let n = TAILLE_FFT as f32;
        for (i, v) in self.reel.iter_mut().enumerate() {
            let garde = i <= liftre || i >= TAILLE_FFT - liftre;
            *v = if garde { *v / n } else { 0.0 };
        }
        self.directe();
        for (e, s) in sortie.iter_mut().zip(&self.spectre) {
            *e = s.re;
        }
    }
}

/// L'enveloppe de trames prises hors du fil audio (l'imitation assistée) :
/// la même lecture que l'étage, fenêtre et lissage compris.
pub(crate) struct Analyseur {
    fourier: Fourier,
    fenetre: Vec<f32>,
    spectre: Vec<Complex32>,
}

impl Analyseur {
    pub(crate) fn new() -> Self {
        let fenetre = (0..TAILLE_FFT)
            .map(|i| (0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / TAILLE_FFT as f32).cos()).sqrt())
            .collect();
        Self { fourier: Fourier::new(), fenetre, spectre: vec![Complex32::new(0.0, 0.0); CASES] }
    }

    /// L'enveloppe (logarithme naturel de l'amplitude, [`CASES`] valeurs)
    /// d'une trame de [`TAILLE_FFT`] échantillons, lissée pour une voix de
    /// cette période.
    pub(crate) fn enveloppe(&mut self, trame: &[f32], periode: Option<f32>, sortie: &mut [f32]) {
        for ((r, x), w) in self.fourier.reel.iter_mut().zip(trame).zip(&self.fenetre) {
            *r = x * w;
        }
        self.fourier.directe();
        self.spectre.copy_from_slice(&self.fourier.spectre);
        let spectre = std::mem::take(&mut self.spectre);
        self.fourier.enveloppe(&spectre, sortie, liftre(periode));
        self.spectre = spectre;
    }
}

/// L'étage de timbre : l'enveloppe déplacée, le chuchotement, le vocodeur.
#[derive(Clone)]
pub struct Timbre {
    r: ReglagesTimbre,
    fourier: Fourier,
    fenetre: Vec<f32>,
    /// Les dernières `TAILLE_FFT` entrées, et la porteuse du vocodeur au
    /// même instant.
    entree: Vec<f32>,
    porteuse_passee: Vec<f32>,
    ecriture: usize,
    compte: usize,
    /// La sortie en cours de recouvrement, indexée par l'instant modulo la
    /// taille de trame.
    sortie: Vec<f32>,
    temps: usize,
    enveloppe: Vec<f32>,
    enveloppe_porteuse: Vec<f32>,
    voix: Vec<Complex32>,
    spectre_porteuse: Vec<Complex32>,
    porteuse: Porteuse,
    alea: u32,
    // La hauteur, pour le vocodeur qui suit la voix.
    passe_bas: Egaliseur,
    bloc: Vec<f32>,
    histoire: Vec<f32>,
    trames_depuis_yin: usize,
    /// La période de la voix qui entre, en échantillons (`None` : pas de
    /// hauteur, une consonne ou un silence).
    periode: Option<f32>,
}

impl std::fmt::Debug for Timbre {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Timbre").field("r", &self.r).field("periode", &self.periode).finish_non_exhaustive()
    }
}

impl Timbre {
    pub fn new(r: ReglagesTimbre) -> Self {
        let fenetre = (0..TAILLE_FFT)
            .map(|i| (0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / TAILLE_FFT as f32).cos()).sqrt())
            .collect();
        Self {
            r,
            fourier: Fourier::new(),
            fenetre,
            entree: vec![0.0; TAILLE_FFT],
            porteuse_passee: vec![0.0; TAILLE_FFT],
            ecriture: 0,
            compte: 0,
            sortie: vec![0.0; TAILLE_FFT],
            temps: 0,
            enveloppe: vec![0.0; CASES],
            enveloppe_porteuse: vec![0.0; CASES],
            voix: vec![Complex32::new(0.0, 0.0); CASES],
            spectre_porteuse: vec![Complex32::new(0.0, 0.0); CASES],
            porteuse: Porteuse { phase: 0.0, pas: 110.0 / SAMPLE_RATE as f32, voisee: true, alea: 0x9E37_79B9 },
            alea: 0x2545_F491,
            passe_bas: Egaliseur::new(&[Bande::new(Forme::PasseBas, 1_000.0, 0.0, Q_NEUTRE).raide()]),
            bloc: Vec::with_capacity(PAS),
            histoire: vec![0.0; HISTOIRE_YIN],
            trames_depuis_yin: 0,
            periode: None,
        }
    }

    pub fn regler(&mut self, r: ReglagesTimbre) {
        self.r = r;
    }

    /// Le retard de l'étage, en échantillons.
    pub fn retard() -> usize {
        TAILLE_FFT - 1
    }

    /// Un échantillon entre, un échantillon sort — celui d'il y a
    /// [`Timbre::retard`].
    pub fn traiter(&mut self, x: f32) -> f32 {
        let c = if self.r.vocodeur > 0.005 { self.porteuse.suivant() } else { 0.0 };
        self.entree[self.ecriture] = x;
        self.porteuse_passee[self.ecriture] = c;
        self.ecriture = (self.ecriture + 1) % TAILLE_FFT;
        self.bloc.push(x);
        self.compte += 1;
        if self.compte == PAS {
            self.compte = 0;
            self.trame();
        }
        // L'instant qui sort est complet : la dernière trame qui le couvre
        // vient d'être ajoutée.
        let i = (self.temps + 1) % TAILLE_FFT;
        let y = self.sortie[i];
        self.sortie[i] = 0.0;
        self.temps = self.temps.wrapping_add(1);
        y
    }

    /// La hauteur de la voix qui entre — pour le lissage de l'enveloppe, et
    /// pour le vocodeur qui la suit : un bloc passé sous 1 kHz, décimé,
    /// ajouté à l'historique ; YIN toutes les deux trames (11 ms).
    fn suivre_hauteur(&mut self) {
        self.passe_bas.traiter_trame(&mut self.bloc);
        let n = self.bloc.len() / DECIMATION;
        self.histoire.rotate_left(n);
        let debut = self.histoire.len() - n;
        for (k, h) in self.histoire[debut..].iter_mut().enumerate() {
            *h = self.bloc[k * DECIMATION];
        }
        self.trames_depuis_yin += 1;
        if self.trames_depuis_yin < 2 {
            return;
        }
        self.trames_depuis_yin = 0;
        let energie = self.histoire.iter().map(|v| v * v).sum::<f32>() / self.histoire.len() as f32;
        let mesure = if energie > 1e-7 { yin(&self.histoire, TAUX_YIN, 60.0, 600.0) } else { None };
        let voisee = matches!(mesure, Some((_, aperiodicite)) if aperiodicite < 0.3);
        self.periode = mesure.filter(|_| voisee).map(|(hz, _)| SAMPLE_RATE as f32 / hz);
        let note = match (self.r.vocodeur_note, mesure) {
            (Some(hz), _) => Some(hz),
            // Au demi-ton près : la voix du robot marche par marches.
            (None, Some((hz, _))) if voisee => Some(440.0 * 2f32.powf((12.0 * (hz / 440.0).log2()).round() / 12.0)),
            _ => None,
        };
        self.porteuse.voisee = voisee;
        if let Some(hz) = note {
            self.porteuse.pas = hz.clamp(30.0, 2_000.0) / SAMPLE_RATE as f32;
        }
    }

    fn trame(&mut self) {
        self.suivre_hauteur();
        self.bloc.clear();
        let r = self.r;

        // Le spectre de la voix.
        for i in 0..TAILLE_FFT {
            let j = (self.ecriture + i) % TAILLE_FFT;
            self.fourier.reel[i] = self.entree[j] * self.fenetre[i];
        }
        self.fourier.directe();
        self.voix.copy_from_slice(&self.fourier.spectre);
        let voix = std::mem::take(&mut self.voix);
        let mut enveloppe = std::mem::take(&mut self.enveloppe);
        self.fourier.enveloppe(&voix, &mut enveloppe, liftre(self.periode));

        // L'enveloppe voulue : la même, lue plus haut ou plus bas.
        let lire = |e: &[f32], position: f32| -> f32 {
            let p = position.clamp(0.0, (CASES - 1) as f32);
            let i = p as usize;
            let f = p - i as f32;
            if i + 1 < CASES {
                e[i] + (e[i + 1] - e[i]) * f
            } else {
                e[CASES - 1]
            }
        };
        let mut energie_avant = 0.0f32;
        let mut energie_apres = 0.0f32;
        let mut sortie = [Complex32::new(0.0, 0.0); CASES];
        for k in 0..CASES {
            let voulue = lire(&enveloppe, k as f32 * r.etirement);
            let g = (voulue - enveloppe[k]).clamp(-GAIN_MAX, GAIN_MAX).exp();
            sortie[k] = voix[k] * g;
            energie_avant += voix[k].norm_sqr();
            energie_apres += sortie[k].norm_sqr();
        }
        // Le niveau d'origine, à ±6 dB : déplacer les formants ne doit ni
        // gonfler ni éteindre la voix.
        let niveau = if energie_apres > 1e-20 {
            (energie_avant / energie_apres).sqrt().clamp(1.0 / NIVEAU_MAX, NIVEAU_MAX)
        } else {
            1.0
        };
        let energie_voix = energie_avant;

        // Le chuchotement : un souffle façonné par l'enveloppe voulue, de
        // l'énergie de la voix. Voix et souffle ne se ressemblent pas : leurs
        // énergies s'ajoutent, d'où la normalisation.
        let w = r.chuchotement;
        let (part_voix, part_souffle) = ((2.0 - 2.0 * w).min(1.0), (2.0 * w).min(1.0));
        let somme = (part_voix * part_voix + part_souffle * part_souffle).sqrt().max(1e-6);
        let (part_voix, part_souffle) = (part_voix / somme, part_souffle / somme);
        if part_souffle > 0.001 {
            let mut energie_souffle = 0.0;
            for k in 0..CASES {
                let a = lire(&enveloppe, k as f32 * r.etirement).exp();
                energie_souffle += a * a;
            }
            // Deux fois l'amplitude : à phases tirées au hasard, les trames
            // qui se recouvrent s'ajoutent en énergie et non en amplitude
            // comme celles d'une vraie voix — le souffle sortait 6 dB sous
            // elle.
            let echelle =
                if energie_souffle > 1e-20 { 2.0 * (energie_voix / energie_souffle).sqrt() } else { 0.0 };
            for (k, s) in sortie.iter_mut().enumerate() {
                let a = lire(&enveloppe, k as f32 * r.etirement).exp() * echelle;
                let phase = std::f32::consts::PI * bruit(&mut self.alea);
                *s = *s * (niveau * part_voix) + Complex32::from_polar(a * part_souffle, phase);
            }
        } else {
            for s in sortie.iter_mut() {
                *s *= niveau;
            }
        }

        // Le vocodeur : la porteuse, blanchie par sa propre enveloppe, prend
        // celle de la voix ; à l'énergie de la voix, en fondu à puissance
        // constante.
        if r.vocodeur > 0.005 {
            for i in 0..TAILLE_FFT {
                let j = (self.ecriture + i) % TAILLE_FFT;
                self.fourier.reel[i] = self.porteuse_passee[j] * self.fenetre[i];
            }
            self.fourier.directe();
            let mut porteuse = std::mem::take(&mut self.spectre_porteuse);
            porteuse.copy_from_slice(&self.fourier.spectre);
            let mut env_p = std::mem::take(&mut self.enveloppe_porteuse);
            // La porteuse a sa propre période : son lissage en dépend.
            let periode_porteuse = self.porteuse.voisee.then(|| 1.0 / self.porteuse.pas);
            self.fourier.enveloppe(&porteuse, &mut env_p, liftre(periode_porteuse));
            let mut synthese = [Complex32::new(0.0, 0.0); CASES];
            let mut energie_synthese = 0.0;
            for k in 0..CASES {
                let voulue = lire(&enveloppe, k as f32 * r.etirement);
                let g = (voulue - env_p[k]).clamp(-3.0 * GAIN_MAX, 3.0 * GAIN_MAX).exp();
                synthese[k] = porteuse[k] * g;
                energie_synthese += synthese[k].norm_sqr();
            }
            let echelle = if energie_synthese > 1e-20 { (energie_voix / energie_synthese).sqrt() } else { 0.0 };
            let (a, b) = ((1.0 - r.vocodeur).sqrt(), r.vocodeur.sqrt() * echelle);
            for (s, v) in sortie.iter_mut().zip(&synthese) {
                *s = *s * a + *v * b;
            }
            self.enveloppe_porteuse = env_p;
            self.spectre_porteuse = porteuse;
        }

        // Retour au temps, fenêtré, ajouté à la sortie en recouvrement.
        self.fourier.spectre.copy_from_slice(&sortie);
        self.fourier.inverse();
        // Racine de Hann deux fois, recouvrement de trois quarts : la somme
        // des fenêtres vaut 2 ; et la transformée inverse n'est pas
        // normalisée.
        let echelle = 1.0 / (2.0 * TAILLE_FFT as f32);
        for i in 0..TAILLE_FFT {
            // L'échantillon i de la trame est l'instant temps − N + 1 + i.
            let slot = (self.temps + 1 + i) % TAILLE_FFT;
            self.sortie[slot] += self.fourier.reel[i] * self.fenetre[i] * echelle;
        }
        self.voix = voix;
        self.enveloppe = enveloppe;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Une voyelle de synthèse : les harmoniques d'une fondamentale, chacun
    /// pesé par une enveloppe à formants (des bosses à `formants`).
    pub(crate) fn voyelle(f0: f32, formants: &[f32], duree_s: f32) -> Vec<f32> {
        let n = (duree_s * SAMPLE_RATE as f32) as usize;
        let enveloppe = |f: f32| -> f32 {
            let mut a = 0.02;
            for &fm in formants {
                let d = (f - fm) / (0.12 * fm);
                a += (-d * d).exp();
            }
            a / (1.0 + f / 3000.0)
        };
        let mut x = vec![0f32; n];
        let mut h = 1;
        while f0 * h as f32 <= 8_000.0 {
            let f = f0 * h as f32;
            let a = 0.08 * enveloppe(f);
            for (i, s) in x.iter_mut().enumerate() {
                *s += a * (2.0 * std::f32::consts::PI * f * i as f32 / SAMPLE_RATE as f32 + h as f32).sin();
            }
            h += 1;
        }
        x
    }

    /// Le centre de gravité spectral entre `bas` et `haut` Hz — pour voir où
    /// se trouve la bosse d'énergie.
    fn centre(x: &[f32], bas: f32, haut: f32) -> f32 {
        let n = 8192;
        let debut = x.len() - n;
        let mut plan = RealFftPlanner::<f32>::new();
        let fft = plan.plan_fft_forward(n);
        let mut entree: Vec<f32> = x[debut..]
            .iter()
            .enumerate()
            .map(|(i, v)| v * (0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos()))
            .collect();
        let mut spectre = fft.make_output_vec();
        fft.process(&mut entree, &mut spectre).unwrap();
        let hz = SAMPLE_RATE as f32 / n as f32;
        let (mut somme, mut poids) = (0.0, 0.0);
        for (k, c) in spectre.iter().enumerate() {
            let f = k as f32 * hz;
            if f >= bas && f <= haut {
                somme += f * c.norm_sqr();
                poids += c.norm_sqr();
            }
        }
        somme / poids
    }

    fn passer(t: &mut Timbre, x: &[f32]) -> Vec<f32> {
        x.iter().map(|&v| t.traiter(v)).collect()
    }

    #[test]
    fn neutre_il_rend_la_voix_avec_son_retard() {
        let x = voyelle(150.0, &[700.0, 1_200.0, 2_600.0], 0.5);
        let mut t = Timbre::new(ReglagesTimbre { etirement: 1.0, chuchotement: 0.0, vocodeur: 0.0, vocodeur_note: None });
        let y = passer(&mut t, &x);
        let d = Timbre::retard();
        let (mut ecart, mut energie) = (0f64, 0f64);
        for i in (d + TAILLE_FFT)..x.len() {
            ecart += ((y[i] - x[i - d]) as f64).powi(2);
            energie += (x[i - d] as f64).powi(2);
        }
        let rapport_db = 10.0 * (energie / ecart.max(1e-30)).log10();
        assert!(rapport_db > 40.0, "reconstruction à {rapport_db:.1} dB seulement");
    }

    #[test]
    fn les_formants_se_deplacent_sans_la_hauteur() {
        // Une seule bosse, à 1 kHz ; on la veut 25 % plus haut.
        let x = voyelle(150.0, &[1_000.0], 1.0);
        let mut t =
            Timbre::new(ReglagesTimbre { etirement: 1.0 / 1.25, chuchotement: 0.0, vocodeur: 0.0, vocodeur_note: None });
        let y = passer(&mut t, &x);
        let (avant, apres) = (centre(&x, 400.0, 2_500.0), centre(&y, 400.0, 2_500.0));
        assert!((apres / avant - 1.25).abs() < 0.08, "bosse de {avant:.0} à {apres:.0} Hz");
        // La hauteur ne bouge pas : l'énergie reste sur les harmoniques de
        // 150 Hz.
        let harmonique = |s: &[f32], f: f32| centre(s, f - 20.0, f + 20.0);
        assert!((harmonique(&y, 1_200.0) - 1_200.0).abs() < 8.0);
    }

    #[test]
    fn le_chuchotement_garde_l_energie_et_perd_la_hauteur() {
        let x = voyelle(150.0, &[700.0, 1_200.0, 2_600.0], 1.0);
        let mut t = Timbre::new(ReglagesTimbre { etirement: 1.0, chuchotement: 1.0, vocodeur: 0.0, vocodeur_note: None });
        let y = passer(&mut t, &x);
        let d = Timbre::retard() + TAILLE_FFT;
        let e = |s: &[f32]| s[d..].iter().map(|v| v * v).sum::<f32>() / (s.len() - d) as f32;
        let rapport_db = 10.0 * (e(&y) / e(&x)).log10();
        assert!(rapport_db.abs() < 3.0, "énergie à {rapport_db:.1} dB");
        // Plus de hauteur : YIN n'y trouve plus rien de périodique.
        let decime: Vec<f32> = y[d..].iter().step_by(DECIMATION).copied().collect();
        let a = yin(&decime[..600], TAUX_YIN, 60.0, 600.0).map(|(_, a)| a).unwrap_or(1.0);
        assert!(a > 0.3, "apériodicité {a:.2} : encore une voix chantée");
    }

    #[test]
    fn le_vocodeur_parle_sur_sa_note() {
        let x = voyelle(150.0, &[700.0, 1_200.0, 2_600.0], 1.0);
        let mut t =
            Timbre::new(ReglagesTimbre { etirement: 1.0, chuchotement: 0.0, vocodeur: 1.0, vocodeur_note: Some(110.0) });
        let y = passer(&mut t, &x);
        let d = Timbre::retard() + TAILLE_FFT;
        let decime: Vec<f32> = y[d..].iter().step_by(DECIMATION).copied().collect();
        let (hz, a) = yin(&decime[..1_200], TAUX_YIN, 60.0, 600.0).expect("une note");
        assert!((hz - 110.0).abs() < 3.0 && a < 0.3, "{hz:.1} Hz, apériodicité {a:.2}");
    }

    #[test]
    fn yin_ne_saute_pas_a_l_octave() {
        // Une fondamentale faible sous un deuxième harmonique fort : le « i »
        // d'une voix d'homme. La période reste celle de 120 Hz.
        let n = SAMPLE_RATE as usize / 10;
        let x: Vec<f32> = (0..n)
            .map(|i| {
                let t = 2.0 * std::f32::consts::PI * i as f32 / SAMPLE_RATE as f32;
                0.08 * (120.0 * t).sin() + 0.8 * (240.0 * t).sin() + 0.25 * (360.0 * t).sin()
            })
            .collect();
        let decime: Vec<f32> = x.iter().step_by(DECIMATION).copied().collect();
        let (hz, _) = yin(&decime, TAUX_YIN, 60.0, 600.0).expect("une hauteur");
        assert!((hz - 120.0).abs() < 2.0, "lu {hz:.1} Hz");
    }

    #[test]
    fn yin_trouve_la_fondamentale() {
        for f0 in [85.0, 120.0, 210.0, 330.0] {
            let x = voyelle(f0, &[700.0, 1_200.0], 0.1);
            let decime: Vec<f32> = x.iter().step_by(DECIMATION).copied().collect();
            let (hz, a) = yin(&decime, TAUX_YIN, 60.0, 600.0).expect("une hauteur");
            assert!((hz / f0 - 1.0).abs() < 0.02 && a < 0.2, "{f0} Hz lu {hz:.1} ({a:.2})");
        }
    }
}
