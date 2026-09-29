//! Le changeur de voix : hauteur, couche une octave dessous, distorsion,
//! robot, talkie, filtre sombre, voile, écho, réverbération — et des
//! personnages tout prêts.
//!
//! Il se branche en fin de chaîne, après le limiteur, sur une **copie** de la
//! voix : la détection de parole continue d'écouter la vraie voix (une voix
//! de robot n'ouvrirait plus le micro en mode « à la voix »), et l'on choisit
//! où part la voix changée — ki-chat, les jeux, ou les deux. Un limiteur à
//! lui borne ce que distorsion et réverbération ajoutent.
//!
//! La hauteur : deux têtes de lecture relisent la voix plus vite ou plus
//! lentement qu'elle n'arrive, chacune en fondu, et se relaient. À chaque
//! relais, la tête qui revient est **calée sur la forme d'onde** de celle qui
//! joue (recherche de corrélation sur ±5 ms) : le raccord tombe en phase, au
//! lieu du « bouillonnement » des décaleurs naïfs. ~25 ms de retard quand la
//! hauteur change ; aucun quand elle ne change pas.

use crate::dynamique::Limiteur;
use crate::egaliseur::{Bande, Egaliseur, Forme, Q_NEUTRE};
use crate::SAMPLE_RATE;

/// Où part la voix changée.
pub const VERS_TOUT: u8 = 0;
pub const VERS_JEUX: u8 = 1;
pub const VERS_KICHAT: u8 = 2;

/// Les réglages du changeur.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReglagesChangeur {
    /// Hauteur, en demi-tons (-12 à +12).
    pub hauteur: f32,
    /// Une couche une octave sous la voix changée (0 à 1).
    pub sous_octave: f32,
    /// Distorsion (0 à 1).
    pub distorsion: f32,
    /// Robot : modulation en anneau (0 à 1), et sa fréquence.
    pub robot: f32,
    pub robot_hz: f32,
    /// Talkie : la bande d'une radio de poche (300 Hz – 3 kHz).
    pub talkie: bool,
    /// Filtre sombre : coupe au-dessus de cette fréquence (20 kHz : rien).
    pub sombre_hz: f32,
    /// Voile : un flanger lent, le chatoiement d'une voix d'outre-tombe (0 à 1).
    pub voile: f32,
    /// Écho : mélange (0 à 1) et délai.
    pub echo: f32,
    pub echo_ms: f32,
    /// Réverbération : mélange (0 à 1) et taille de la pièce (0 à 1).
    pub reverb: f32,
    pub reverb_taille: f32,
}

impl Default for ReglagesChangeur {
    fn default() -> Self {
        Self {
            hauteur: 0.0,
            sous_octave: 0.0,
            distorsion: 0.0,
            robot: 0.0,
            robot_hz: 50.0,
            talkie: false,
            sombre_hz: 20_000.0,
            voile: 0.0,
            echo: 0.0,
            echo_ms: 250.0,
            reverb: 0.0,
            reverb_taille: 0.5,
        }
    }
}

impl ReglagesChangeur {
    /// Ramenés dans leurs plages (une préférence abîmée ne casse rien).
    pub fn bornes(self) -> Self {
        let ok = |v: f32, min: f32, max: f32, d: f32| if v.is_finite() { v.clamp(min, max) } else { d };
        let d = Self::default();
        Self {
            hauteur: ok(self.hauteur, -12.0, 12.0, 0.0),
            sous_octave: ok(self.sous_octave, 0.0, 1.0, 0.0),
            distorsion: ok(self.distorsion, 0.0, 1.0, 0.0),
            robot: ok(self.robot, 0.0, 1.0, 0.0),
            robot_hz: ok(self.robot_hz, 10.0, 300.0, d.robot_hz),
            talkie: self.talkie,
            sombre_hz: ok(self.sombre_hz, 800.0, 20_000.0, d.sombre_hz),
            voile: ok(self.voile, 0.0, 1.0, 0.0),
            echo: ok(self.echo, 0.0, 1.0, 0.0),
            echo_ms: ok(self.echo_ms, 40.0, 900.0, d.echo_ms),
            reverb: ok(self.reverb, 0.0, 1.0, 0.0),
            reverb_taille: ok(self.reverb_taille, 0.0, 1.0, d.reverb_taille),
        }
    }
}

/// Un personnage : (nom, en quelques mots, réglages).
pub type Personnage = (&'static str, &'static str, fn() -> ReglagesChangeur);

pub const PERSONNAGES: [Personnage; 8] = [
    ("Spectre", "façon Omen : grave, voilé, caverneux", || ReglagesChangeur {
        hauteur: -4.0,
        sous_octave: 0.35,
        distorsion: 0.12,
        voile: 0.45,
        sombre_hz: 6_000.0,
        reverb: 0.3,
        reverb_taille: 0.8,
        ..Default::default()
    }),
    ("Ingénieure", "façon Killjoy : plus aiguë, plus claire", || ReglagesChangeur {
        hauteur: 4.0,
        ..Default::default()
    }),
    ("Robot", "voix de machine", || ReglagesChangeur {
        hauteur: -1.0,
        robot: 0.9,
        robot_hz: 55.0,
        distorsion: 0.1,
        ..Default::default()
    }),
    ("Démon", "très grave, saturé", || ReglagesChangeur {
        hauteur: -8.0,
        sous_octave: 0.5,
        distorsion: 0.45,
        sombre_hz: 5_000.0,
        reverb: 0.2,
        reverb_taille: 0.6,
        ..Default::default()
    }),
    ("Talkie", "radio de poche", || ReglagesChangeur { talkie: true, distorsion: 0.3, ..Default::default() }),
    ("Grotte", "grande réverbération", || ReglagesChangeur {
        reverb: 0.55,
        reverb_taille: 0.9,
        echo: 0.2,
        echo_ms: 320.0,
        ..Default::default()
    }),
    ("Écureuil", "très aiguë", || ReglagesChangeur { hauteur: 8.0, ..Default::default() }),
    ("Géant", "grave et ample", || ReglagesChangeur {
        hauteur: -5.0,
        sous_octave: 0.2,
        reverb: 0.15,
        reverb_taille: 0.7,
        ..Default::default()
    }),
];

// ---------------------------------------------------------------------------
// Hauteur
// ---------------------------------------------------------------------------

/// Le tampon circulaire du décaleur : 170 ms.
const TAILLE: usize = 8192;
/// La course d'une tête, du début à la fin de son fondu : 40 ms.
const FENETRE: f32 = 1920.0;
/// La recherche du raccord, de part et d'autre de la position prévue : 5 ms
/// — de quoi couvrir une période de voix grave.
const RECHERCHE: usize = 240;
/// La longueur comparée pour caler le raccord.
const COMPARAISON: usize = 256;
/// La marge minimale derrière la tête d'écriture.
const RETARD_MIN: f32 = 64.0;

#[derive(Clone, Copy, Debug)]
struct Tete {
    retard: f32,
    phase: f32,
}

/// Le décaleur de hauteur à deux têtes calées.
#[derive(Clone, Debug)]
struct Decaleur {
    tampon: Vec<f32>,
    ecriture: usize,
    /// De combien le retard d'une tête change à chaque échantillon : 1 − ratio.
    pente: f32,
    pas_phase: f32,
    tetes: [Tete; 2],
}

impl Decaleur {
    fn new(demi_tons: f32) -> Self {
        let ratio = 2f32.powf(demi_tons / 12.0);
        let pente = 1.0 - ratio;
        let base = RETARD_MIN + RECHERCHE as f32;
        // Chaque tête parcourt la fenêtre : vers le bas en montant la voix,
        // vers le haut en la descendant.
        let depart = |phase: f32| {
            if pente < 0.0 {
                base + FENETRE * (1.0 - phase)
            } else {
                base + FENETRE * phase
            }
        };
        Self {
            tampon: vec![0.0; TAILLE],
            ecriture: 0,
            pente,
            pas_phase: pente.abs() / FENETRE,
            tetes: [Tete { retard: depart(0.0), phase: 0.0 }, Tete { retard: depart(0.5), phase: 0.5 }],
        }
    }

    #[inline]
    fn lire(&self, retard: f32) -> f32 {
        let pos = (self.ecriture as f32 - retard).rem_euclid(TAILLE as f32);
        let i = pos as usize;
        let frac = pos - i as f32;
        let a = self.tampon[i % TAILLE];
        let b = self.tampon[(i + 1) % TAILLE];
        a + (b - a) * frac
    }

    #[inline]
    fn a(&self, retard_entier: usize) -> f32 {
        self.tampon[(self.ecriture + TAILLE - retard_entier % TAILLE) % TAILLE]
    }

    /// Le retard où reprendre une tête : à l'opposé de l'autre dans la
    /// fenêtre, calé sur sa forme d'onde.
    fn raccord(&self, autre: f32) -> f32 {
        let prevu = if self.pente < 0.0 { autre + FENETRE / 2.0 } else { autre - FENETRE / 2.0 };
        let bas = RETARD_MIN as usize + 1;
        let haut = TAILLE - COMPARAISON - 2;
        let autre_i = autre.round() as usize;
        let mut meilleur = prevu;
        let mut score_max = f32::MIN;
        let centre = prevu.round() as isize;
        for decalage in -(RECHERCHE as isize)..=(RECHERCHE as isize) {
            let d = centre + decalage;
            if d < bas as isize || d > haut as isize {
                continue;
            }
            let d = d as usize;
            let (mut produit, mut energie) = (0f32, 1e-9f32);
            for k in 0..COMPARAISON {
                let x = self.a(d + k);
                produit += x * self.a(autre_i + k);
                energie += x * x;
            }
            let score = produit / energie.sqrt();
            if score > score_max {
                score_max = score;
                meilleur = d as f32 + (autre - autre_i as f32);
            }
        }
        meilleur.clamp(RETARD_MIN, (TAILLE - 2) as f32)
    }

    #[inline]
    fn traiter(&mut self, x: f32) -> f32 {
        self.tampon[self.ecriture] = x;
        let mut y = 0.0;
        for t in &self.tetes {
            let w = (std::f32::consts::PI * t.phase).sin();
            y += self.lire(t.retard) * w * w;
        }
        for k in 0..2 {
            self.tetes[k].retard += self.pente;
            self.tetes[k].phase += self.pas_phase;
            if self.tetes[k].phase >= 1.0 {
                self.tetes[k].phase -= 1.0;
                self.tetes[k].retard = self.raccord(self.tetes[1 - k].retard);
            }
        }
        self.ecriture = (self.ecriture + 1) % TAILLE;
        y
    }
}

// ---------------------------------------------------------------------------
// Effets
// ---------------------------------------------------------------------------

/// Le voile : un flanger lent (retard de 1 à 4 ms, balancé à 0,25 Hz).
#[derive(Clone, Debug)]
struct Voile {
    tampon: Vec<f32>,
    ecriture: usize,
    phase: f32,
}

impl Voile {
    fn new() -> Self {
        Self { tampon: vec![0.0; 512], ecriture: 0, phase: 0.0 }
    }

    #[inline]
    fn traiter(&mut self, x: f32, melange: f32) -> f32 {
        let n = self.tampon.len();
        self.phase = (self.phase + 0.25 / SAMPLE_RATE as f32) % 1.0;
        let lfo = 0.5 + 0.5 * (2.0 * std::f32::consts::PI * self.phase).sin();
        let retard = 48.0 + 144.0 * lfo;
        let pos = (self.ecriture as f32 - retard).rem_euclid(n as f32);
        let i = pos as usize;
        let frac = pos - i as f32;
        let r = self.tampon[i % n] + (self.tampon[(i + 1) % n] - self.tampon[i % n]) * frac;
        self.tampon[self.ecriture] = x + 0.45 * r;
        self.ecriture = (self.ecriture + 1) % n;
        (x + melange * r) / (1.0 + 0.5 * melange)
    }
}

/// L'écho : une ligne à retard réinjectée.
#[derive(Clone, Debug)]
struct Echo {
    tampon: Vec<f32>,
    ecriture: usize,
}

impl Echo {
    fn new() -> Self {
        Self { tampon: vec![0.0; SAMPLE_RATE as usize], ecriture: 0 }
    }

    #[inline]
    fn traiter(&mut self, x: f32, melange: f32, retard: usize) -> f32 {
        let n = self.tampon.len();
        let r = self.tampon[(self.ecriture + n - retard.min(n - 1)) % n];
        self.tampon[self.ecriture] = x + 0.4 * r;
        self.ecriture = (self.ecriture + 1) % n;
        x + melange * r
    }
}

/// Réverbération à la Freeverb, en mono : quatre peignes amortis en
/// parallèle, deux passe-tout en série.
#[derive(Clone, Debug)]
struct Reverb {
    peignes: Vec<(Vec<f32>, usize, f32)>,
    passe_tout: Vec<(Vec<f32>, usize)>,
}

impl Reverb {
    fn new() -> Self {
        let echelle = SAMPLE_RATE as f32 / 44_100.0;
        let taille = |n: f32| (n * echelle) as usize;
        Self {
            peignes: [1557.0, 1617.0, 1491.0, 1422.0].iter().map(|&n| (vec![0.0; taille(n)], 0, 0.0)).collect(),
            passe_tout: [556.0, 441.0].iter().map(|&n| (vec![0.0; taille(n)], 0)).collect(),
        }
    }

    #[inline]
    fn traiter(&mut self, x: f32, melange: f32, taille: f32) -> f32 {
        let retour = 0.7 + 0.28 * taille;
        let amorti = 0.3;
        let mut humide = 0.0;
        for (buf, pos, memoire) in self.peignes.iter_mut() {
            let sortie = buf[*pos];
            *memoire = sortie * (1.0 - amorti) + *memoire * amorti;
            buf[*pos] = x * 0.3 + *memoire * retour;
            *pos = (*pos + 1) % buf.len();
            humide += sortie;
        }
        for (buf, pos) in self.passe_tout.iter_mut() {
            let b = buf[*pos];
            let sortie = -humide + b;
            buf[*pos] = humide + b * 0.5;
            *pos = (*pos + 1) % buf.len();
            humide = sortie;
        }
        x * (1.0 - 0.5 * melange) + humide * melange * 0.5
    }
}

// ---------------------------------------------------------------------------
// Le changeur
// ---------------------------------------------------------------------------

/// Le changeur de voix, réglé.
#[derive(Clone, Debug)]
pub struct Changeur {
    r: ReglagesChangeur,
    hauteur: Option<Decaleur>,
    sous_octave: Option<Decaleur>,
    filtres: Egaliseur,
    phase_robot: f32,
    voile: Voile,
    echo: Echo,
    reverb: Reverb,
    limiteur: Limiteur,
}

impl Changeur {
    pub fn new(r: ReglagesChangeur) -> Self {
        let mut c = Self {
            r: ReglagesChangeur::default(),
            hauteur: None,
            sous_octave: None,
            filtres: Egaliseur::default(),
            phase_robot: 0.0,
            voile: Voile::new(),
            echo: Echo::new(),
            reverb: Reverb::new(),
            limiteur: Limiteur::new(),
        };
        c.regler(r);
        c
    }

    /// Change les réglages. Les têtes de lecture ne sont refaites que si la
    /// hauteur change — glisser la réverbération ne fait pas sauter la voix.
    pub fn regler(&mut self, r: ReglagesChangeur) {
        let r = r.bornes();
        if r.hauteur != self.r.hauteur || self.hauteur.is_none() != (r.hauteur.abs() < 0.01) {
            self.hauteur = (r.hauteur.abs() >= 0.01).then(|| Decaleur::new(r.hauteur));
        }
        let veut_sous = r.sous_octave > 0.005;
        if r.hauteur != self.r.hauteur || self.sous_octave.is_some() != veut_sous {
            self.sous_octave = veut_sous.then(|| Decaleur::new(r.hauteur - 12.0));
        }
        let mut bandes = Vec::new();
        if r.talkie {
            bandes.push(Bande::new(Forme::PasseHaut, 300.0, 0.0, Q_NEUTRE).raide());
            bandes.push(Bande::new(Forme::PasseBas, 3_000.0, 0.0, Q_NEUTRE).raide());
            bandes.push(Bande::new(Forme::Cloche, 1_500.0, 4.0, 1.0));
        }
        if r.sombre_hz < 19_000.0 {
            bandes.push(Bande::new(Forme::PasseBas, r.sombre_hz, 0.0, Q_NEUTRE).raide());
        }
        self.filtres.regler(&bandes);
        self.r = r;
    }

    pub fn reglages(&self) -> ReglagesChangeur {
        self.r
    }

    pub fn traiter_trame(&mut self, trame: &mut [f32]) {
        let r = self.r;
        let pousse = 1.0 + 20.0 * r.distorsion;
        let pas_robot = r.robot_hz / SAMPLE_RATE as f32;
        for s in trame.iter_mut() {
            let x = *s;
            let mut y = match self.hauteur.as_mut() {
                Some(d) => d.traiter(x),
                None => x,
            };
            if let Some(d) = self.sous_octave.as_mut() {
                y = (y + r.sous_octave * d.traiter(x)) / (1.0 + 0.5 * r.sous_octave);
            }
            if r.distorsion > 0.005 {
                y = (1.0 - r.distorsion) * y + r.distorsion * (pousse * y).tanh() * 0.4;
            }
            if r.robot > 0.005 {
                self.phase_robot = (self.phase_robot + pas_robot) % 1.0;
                let porteuse = (2.0 * std::f32::consts::PI * self.phase_robot).sin();
                y *= 1.0 - r.robot + r.robot * porteuse;
            }
            *s = y;
        }
        self.filtres.traiter_trame(trame);
        let retard_echo = (r.echo_ms / 1000.0 * SAMPLE_RATE as f32) as usize;
        for s in trame.iter_mut() {
            let mut y = *s;
            if r.voile > 0.005 {
                y = self.voile.traiter(y, r.voile);
            }
            if r.echo > 0.005 {
                y = self.echo.traiter(y, r.echo, retard_echo);
            }
            if r.reverb > 0.005 {
                y = self.reverb.traiter(y, r.reverb, r.reverb_taille);
            }
            *s = y;
        }
        self.limiteur.traiter_trame(trame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sinus(frequence: f32, amplitude: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amplitude * (2.0 * std::f32::consts::PI * frequence * i as f32 / SAMPLE_RATE as f32).sin())
            .collect()
    }

    /// L'énergie d'un signal à une fréquence (Goertzel).
    fn energie_a(x: &[f32], f: f32) -> f32 {
        let k = 2.0 * (2.0 * std::f32::consts::PI * f / SAMPLE_RATE as f32).cos();
        let (mut s1, mut s2) = (0f32, 0f32);
        for &v in x {
            let s0 = v + k * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        s1 * s1 + s2 * s2 - k * s1 * s2
    }

    /// La hauteur bouge d'autant de demi-tons qu'on le demande : une octave
    /// au-dessus, 200 Hz devient 400 ; au-dessous, 100.
    #[test]
    fn la_hauteur_change_de_ce_qu_on_demande() {
        for (demi_tons, attendu) in [(12.0, 400.0), (-12.0, 100.0), (7.0, 200.0 * 2f32.powf(7.0 / 12.0))] {
            let mut c = Changeur::new(ReglagesChangeur { hauteur: demi_tons, ..Default::default() });
            let mut x = sinus(200.0, 0.3, SAMPLE_RATE as usize);
            c.traiter_trame(&mut x);
            let fin = &x[SAMPLE_RATE as usize / 2..];
            let voulu = energie_a(fin, attendu);
            let origine = energie_a(fin, 200.0);
            assert!(voulu > 20.0 * origine, "{demi_tons} demi-tons : {voulu:e} à {attendu} Hz contre {origine:e} à 200 Hz");
        }
    }

    /// Neutre, le changeur rend la voix telle quelle.
    #[test]
    fn neutre_il_ne_change_rien() {
        let mut c = Changeur::new(ReglagesChangeur::default());
        let entree = sinus(220.0, 0.5, 4_800);
        let mut sortie = entree.clone();
        c.traiter_trame(&mut sortie);
        assert_eq!(entree, sortie);
    }

    /// Chaque personnage garde une sortie finie et sous le plafond, même sur
    /// une voix forte.
    #[test]
    fn chaque_personnage_reste_borne() {
        for (nom, _, reglages) in PERSONNAGES {
            let mut c = Changeur::new(reglages());
            for _ in 0..20 {
                let mut x = sinus(180.0, 0.89, 960);
                c.traiter_trame(&mut x);
                assert!(x.iter().all(|s| s.is_finite() && s.abs() <= Limiteur::PLAFOND + 1e-4), "{nom}");
            }
        }
    }

    /// Des réglages abîmés sont ramenés dans leurs plages.
    #[test]
    fn des_reglages_abimes_sont_bornes() {
        let r = ReglagesChangeur { hauteur: 99.0, reverb: f32::NAN, echo_ms: 5.0, ..Default::default() }.bornes();
        assert_eq!((r.hauteur, r.reverb, r.echo_ms), (12.0, 0.0, 40.0));
    }
}
