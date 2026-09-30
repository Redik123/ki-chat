//! Le spectre moyen d'une voix, par tiers d'octave : de quoi dire si elle
//! sonne étouffée — et si ses aigus se perdent au micro ou dans la chaîne.
//!
//! Une moyenne de Welch (fenêtres de Hann de 4096 points, recouvrement de
//! trois quarts) sur les seules fenêtres de voix : les silences, que la
//! chaîne nettoie et que le micro brut garde, fausseraient la comparaison.
//! Puis l'énergie de chaque tiers d'octave, comparée au spectre moyen de la
//! parole.

use realfft::RealFftPlanner;

use crate::egaliseur::{reponse_db, Bande, Forme, Q_NEUTRE};
use crate::SAMPLE_RATE;

/// Nombre de tiers d'octave analysés.
pub const NB_TIERS: usize = 22;

/// Les centres des tiers d'octave analysés, de 100 Hz à 12,5 kHz.
pub const TIERS: [f32; NB_TIERS] = [
    100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0, 500.0, 630.0, 800.0, 1000.0, 1250.0, 1600.0,
    2000.0, 2500.0, 3150.0, 4000.0, 5000.0, 6300.0, 8000.0, 10000.0, 12500.0,
];

/// Le spectre moyen de la parole aux mêmes centres, en dB (Byrne et al.,
/// 1994 : voix d'hommes et de femmes confondues, effort normal).
const PAROLE: [f32; NB_TIERS] = [
    54.4, 57.7, 56.8, 60.2, 60.3, 59.0, 62.1, 62.1, 60.5, 56.8, 53.7, 53.0, 52.0, 48.7, 48.1, 46.8,
    45.6, 44.5, 44.3, 43.7, 43.4, 41.3,
];

/// Les graves : 100 à 250 Hz.
const GRAVES: std::ops::RangeInclusive<usize> = 0..=4;
/// Le médium, corps de la voix : 315 Hz à 1 kHz. La référence de toutes les
/// comparaisons — l'effet de proximité d'un micro-casque gonfle les graves
/// sous elle, pas elle.
const MEDIUM: std::ops::RangeInclusive<usize> = 5..=10;
/// La présence : 2 à 6,3 kHz, là où se jouent l'intelligibilité et la
/// clarté. Qu'elle manque, et la voix sonne étouffée.
const PRESENCE: std::ops::RangeInclusive<usize> = 13..=18;

const FENETRE: usize = 4096;
const PAS: usize = FENETRE / 4;

/// Le spectre moyen d'une voix.
#[derive(Clone, Debug, PartialEq)]
pub struct SpectreVoix {
    /// L'énergie de chaque tiers d'octave, en dB (échelle arbitraire : seules
    /// les différences comptent).
    pub niveaux_db: [f32; NB_TIERS],
}

impl SpectreVoix {
    /// Par tiers d'octave, l'écart à la parole moyenne en dB, calé à 0 sur le
    /// médium : 0 partout, c'est une voix au timbre moyen.
    pub fn ecarts_db(&self) -> [f32; NB_TIERS] {
        let cale = niveau_db(&self.niveaux_db, MEDIUM) - niveau_db(&PAROLE, MEDIUM);
        std::array::from_fn(|k| self.niveaux_db[k] - PAROLE[k] - cale)
    }

    /// La présence par rapport au médium, en dB au-dessus de la parole
    /// moyenne. Très négative : une voix étouffée.
    pub fn presence_db(&self) -> f32 {
        self.rapport_db(PRESENCE, MEDIUM)
    }

    /// Les graves par rapport au médium, de même. Très positifs : une voix
    /// qui gronde, « caverneuse ».
    pub fn graves_db(&self) -> f32 {
        self.rapport_db(GRAVES, MEDIUM)
    }

    /// La même voix passée par un filtre de gain `gain_db(fréquence)` : ce
    /// qu'un égaliseur en ferait, tiers d'octave par tiers d'octave.
    pub fn filtre(&self, gain_db: impl Fn(f32) -> f32) -> SpectreVoix {
        SpectreVoix { niveaux_db: std::array::from_fn(|k| self.niveaux_db[k] + gain_db(TIERS[k])) }
    }

    /// L'énergie de `bande` par rapport à celle de `reference`, moins le
    /// même rapport pour la parole moyenne.
    fn rapport_db(&self, bande: std::ops::RangeInclusive<usize>, reference: std::ops::RangeInclusive<usize>) -> f32 {
        let voix = niveau_db(&self.niveaux_db, bande.clone()) - niveau_db(&self.niveaux_db, reference.clone());
        let parole = niveau_db(&PAROLE, bande) - niveau_db(&PAROLE, reference);
        voix - parole
    }
}

/// La présence visée par la correction : un rien sous la parole moyenne —
/// une voix de micro-casque poussée plus haut siffle.
pub const PRESENCE_VISEE_DB: f32 = -1.0;
/// Les graves visés : un peu de chaleur au-dessus de la parole moyenne.
pub const GRAVES_VISES_DB: f32 = 2.0;
/// Au-delà, c'est le micro qu'il faut changer, pas l'égaliseur. La cloche
/// de présence va plus haut : large, elle ne relève qu'aux trois quarts
/// d'elle-même la bande qu'elle vise.
const GRAVES_MAX_DB: f32 = 9.0;
const PRESENCE_MAX_DB: f32 = 12.0;

/// Un égaliseur qui rapproche cette voix d'une voix claire, réglé sur sa
/// mesure : une coupe raide sous 80 Hz (le grondement), une étagère qui
/// ramène les graves en trop, une cloche large qui rend la présence qui
/// manque — chacune seulement si elle a de quoi faire. Chaque bande
/// débordant un peu sur les autres, les gains se règlent en quelques passes
/// sur la voix corrigée prévue.
pub fn egaliseur_correctif(voix: &SpectreVoix) -> Vec<Bande> {
    let bandes = |graves: f32, presence: f32| {
        let mut b = vec![Bande::new(Forme::PasseHaut, 80.0, 0.0, Q_NEUTRE).raide()];
        if graves < -0.2 {
            b.push(Bande::new(Forme::EtagereBasse, 220.0, graves, Q_NEUTRE));
        }
        if presence > 0.2 {
            b.push(Bande::new(Forme::Cloche, 3_500.0, presence, 0.7));
        }
        b
    };
    let corriger_graves = voix.graves_db() > GRAVES_VISES_DB + 2.0;
    let corriger_presence = voix.presence_db() < PRESENCE_VISEE_DB - 2.0;
    let (mut graves, mut presence) = (0f32, 0f32);
    for _ in 0..8 {
        let prevue = voix.filtre(|f| reponse_db(&bandes(graves, presence), f));
        if corriger_graves {
            graves = (graves + GRAVES_VISES_DB - prevue.graves_db()).clamp(-GRAVES_MAX_DB, 0.0);
        }
        if corriger_presence {
            presence = (presence + PRESENCE_VISEE_DB - prevue.presence_db()).clamp(0.0, PRESENCE_MAX_DB);
        }
    }
    // Au demi-décibel, comme l'éditeur les affiche.
    bandes((graves * 2.0).round() / 2.0, (presence * 2.0).round() / 2.0)
}

/// L'énergie totale de quelques tiers d'octave, en dB.
fn niveau_db(niveaux: &[f32; NB_TIERS], bandes: std::ops::RangeInclusive<usize>) -> f32 {
    let energie: f64 = niveaux[bandes].iter().map(|db| 10f64.powf(*db as f64 / 10.0)).sum();
    (10.0 * energie.max(1e-30).log10()) as f32
}

/// Analyse une voix (mono, 48 kHz). `None` s'il n'y a pas assez de voix pour
/// conclure — moins d'un quart de seconde, ou rien que du silence.
pub fn analyser(pcm: &[f32]) -> Option<SpectreVoix> {
    if pcm.len() < FENETRE {
        return None;
    }
    let debuts: Vec<usize> = (0..=pcm.len() - FENETRE).step_by(PAS).collect();
    let energies: Vec<f32> = debuts
        .iter()
        .map(|&d| pcm[d..d + FENETRE].iter().map(|s| s * s).sum::<f32>() / FENETRE as f32)
        .collect();
    // Les fenêtres de voix : à moins de 20 dB de celle au rang du quart le
    // plus fort — les silences et le souffle restent dehors.
    let mut triees = energies.clone();
    triees.sort_by(|a, b| b.total_cmp(a));
    let reference = triees[triees.len() / 4];
    if reference < 1e-10 {
        return None;
    }
    let seuil = reference / 100.0;

    let mut planificateur = RealFftPlanner::<f32>::new();
    let fft = planificateur.plan_fft_forward(FENETRE);
    let hann: Vec<f32> = (0..FENETRE)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / FENETRE as f32).cos())
        .collect();
    let mut entree = fft.make_input_vec();
    let mut sortie = fft.make_output_vec();
    let mut puissance = vec![0f64; FENETRE / 2 + 1];
    let mut fenetres = 0usize;
    for (&debut, &energie) in debuts.iter().zip(&energies) {
        if energie < seuil {
            continue;
        }
        for ((x, s), w) in entree.iter_mut().zip(&pcm[debut..debut + FENETRE]).zip(&hann) {
            *x = s * w;
        }
        fft.process(&mut entree, &mut sortie).ok()?;
        for (p, c) in puissance.iter_mut().zip(&sortie) {
            *p += c.norm_sqr() as f64;
        }
        fenetres += 1;
    }
    // Un quart de seconde de voix au moins (fenêtres espacées de 21 ms).
    if fenetres < 12 {
        return None;
    }
    let hz_par_case = SAMPLE_RATE as f32 / FENETRE as f32;
    let demi_tiers = 2f32.powf(1.0 / 6.0);
    let niveaux_db = std::array::from_fn(|k| {
        let (bas, haut) = (TIERS[k] / demi_tiers, TIERS[k] * demi_tiers);
        let premiere = (bas / hz_par_case).ceil() as usize;
        let derniere = ((haut / hz_par_case).floor() as usize).min(FENETRE / 2);
        let energie: f64 = puissance[premiere..=derniere].iter().sum::<f64>() / fenetres as f64;
        (10.0 * energie.max(1e-30).log10()) as f32
    });
    Some(SpectreVoix { niveaux_db })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Une « voix » au timbre de la parole moyenne : un son par tiers
    /// d'octave, à son niveau, en syllabes — une enveloppe douce, dont les
    /// creux passent sous le seuil de la voix sans éclabousser le spectre
    /// comme le ferait une coupure nette. `aigus_db` s'ajoute au-dessus de
    /// 1,6 kHz.
    fn voix(aigus_db: f32) -> Vec<f32> {
        voix_timbree(0.0, aigus_db)
    }

    /// De même, avec `graves_db` en plus sous 300 Hz.
    fn voix_timbree(graves_db: f32, aigus_db: f32) -> Vec<f32> {
        let n = 5 * SAMPLE_RATE as usize;
        let mut x = vec![0f32; n];
        for (k, (&f, &db)) in TIERS.iter().zip(&PAROLE).enumerate() {
            let db = db
                + if TIERS[k] > 1600.0 { aigus_db } else { 0.0 }
                + if TIERS[k] < 300.0 { graves_db } else { 0.0 };
            let a = 10f32.powf((db - 70.0) / 20.0);
            for (i, s) in x.iter_mut().enumerate() {
                *s += a * (2.0 * std::f32::consts::PI * f * i as f32 / SAMPLE_RATE as f32 + k as f32).sin();
            }
        }
        for (i, s) in x.iter_mut().enumerate() {
            let t = i as f32 / SAMPLE_RATE as f32;
            *s *= 0.5 - 0.5 * (2.0 * std::f32::consts::PI * t / 0.6).cos();
        }
        x
    }

    #[test]
    fn une_voix_moyenne_n_a_pas_d_ecart() {
        let s = analyser(&voix(0.0)).unwrap();
        assert!(s.presence_db().abs() < 0.5, "présence {:.2}", s.presence_db());
        assert!(s.graves_db().abs() < 1.0, "graves {:.2}", s.graves_db());
        for (k, e) in s.ecarts_db().iter().enumerate().skip(2) {
            assert!(e.abs() < 1.0, "{} Hz : {e:.2} dB", TIERS[k]);
        }
    }

    #[test]
    fn une_voix_etouffee_se_voit() {
        let s = analyser(&voix(-12.0)).unwrap();
        assert!((s.presence_db() + 12.0).abs() < 0.7, "présence {:.2}", s.presence_db());
        // Le médium reste la référence : les graves ne bougent pas.
        assert!(s.graves_db().abs() < 1.0);
    }

    #[test]
    fn un_egaliseur_se_predit() {
        let s = analyser(&voix(-12.0)).unwrap();
        // +12 dB au-dessus de 1,6 kHz : on retombe sur la voix moyenne.
        let corrige = s.filtre(|f| if f > 1600.0 { 12.0 } else { 0.0 });
        assert!(corrige.presence_db().abs() < 0.7);
    }

    #[test]
    fn le_correctif_rend_une_voix_claire() {
        // Étouffée et grondante : -8 dB d'aigus, +6 dB de graves.
        let s = analyser(&voix_timbree(6.0, -8.0)).unwrap();
        let eq = egaliseur_correctif(&s);
        let corrigee = s.filtre(|f| reponse_db(&eq, f));
        assert!((corrigee.presence_db() - PRESENCE_VISEE_DB).abs() < 0.6, "présence {:.2}", corrigee.presence_db());
        assert!((corrigee.graves_db() - GRAVES_VISES_DB).abs() < 0.6, "graves {:.2}", corrigee.graves_db());
        // Coupe, étagère, cloche : trois bandes.
        assert_eq!(eq.len(), 3);
        // Bien plus grondante et étouffée : chaque bande plafonne, sans
        // chercher à tout rattraper, et la voix s'améliore quand même.
        let s = analyser(&voix_timbree(12.0, -14.0)).unwrap();
        let eq = egaliseur_correctif(&s);
        assert!(eq.iter().all(|b| b.gain_db >= -GRAVES_MAX_DB && b.gain_db <= PRESENCE_MAX_DB));
        let corrigee = s.filtre(|f| reponse_db(&eq, f));
        assert!(corrigee.presence_db() > s.presence_db() + 6.0);
        assert!(corrigee.graves_db() < s.graves_db() - 4.0);
        // Une voix déjà claire n'a que la coupe du grondement.
        let claire = egaliseur_correctif(&analyser(&voix(0.0)).unwrap());
        assert_eq!(claire.len(), 1);
        assert_eq!(claire[0].forme, Forme::PasseHaut);
        // Sans presque aucun aigu : la présence plafonne.
        let perdue = egaliseur_correctif(&analyser(&voix(-30.0)).unwrap());
        assert!(perdue.iter().any(|b| b.forme == Forme::Cloche && b.gain_db == PRESENCE_MAX_DB));
    }

    #[test]
    fn pas_de_verdict_sans_voix() {
        assert!(analyser(&[]).is_none());
        assert!(analyser(&vec![0f32; 5 * SAMPLE_RATE as usize]).is_none());
        // Un dixième de seconde : trop court.
        assert!(analyser(&voix(0.0)[..4800]).is_none());
    }
}
