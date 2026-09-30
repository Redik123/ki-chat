//! L'imitation assistée : la hauteur et le timbre d'une voix, mesurés sur un
//! extrait, et les réglages du changeur qui en rapprochent la sienne.
//!
//! La hauteur : la médiane de la fondamentale sur les trames chantées (YIN).
//! Le timbre : l'enveloppe moyenne de ces trames, et l'étirement qui aligne
//! au mieux la sienne sur celle de la voix visée — la corrélation de leurs
//! formes, pentes générales retirées : un micro plus sombre, une pièce plus
//! sourde ne doivent pas passer pour une autre bouche.

use crate::egaliseur::{Bande, Egaliseur, Forme, Q_NEUTRE};
use crate::timbre::{yin, Analyseur, CASES, TAILLE_FFT};
use crate::SAMPLE_RATE;

/// La grille où l'on compare les enveloppes : 160 points, de 150 Hz à
/// 6 kHz, espacés en logarithme — comme l'oreille.
const GRILLE_POINTS: usize = 160;
const GRILLE_BAS: f32 = 150.0;
const GRILLE_HAUT: f32 = 6_000.0;
/// Là où se jugent les formants : de 300 Hz à 4 kHz.
const COMPARE_BAS: f32 = 300.0;
const COMPARE_HAUT: f32 = 4_000.0;
/// L'étirement cherché, de -35 % à +55 %.
const ETIREMENT_MIN: f32 = 0.65;
const ETIREMENT_MAX: f32 = 1.55;
/// Pas assez de voix pour conclure en deçà : un dixième de seconde chanté.
const TRAMES_MIN: usize = 10;
const PAS_ANALYSE: usize = TAILLE_FFT / 2;
const DECIMATION: usize = 4;

fn grille(i: usize) -> f32 {
    GRILLE_BAS * (GRILLE_HAUT / GRILLE_BAS).powf(i as f32 / (GRILLE_POINTS - 1) as f32)
}

/// L'empreinte d'une voix : sa hauteur, et la forme de son timbre.
#[derive(Clone, Debug, PartialEq)]
pub struct EmpreinteVoix {
    /// La hauteur médiane, en Hz.
    pub f0_hz: f32,
    /// L'enveloppe moyenne en dB sur la grille, chaque trame ramenée à la
    /// même moyenne : la forme, pas le volume.
    enveloppe: Vec<f32>,
    /// Les trames chantées qui ont servi.
    pub trames: usize,
}

impl EmpreinteVoix {
    /// L'enveloppe lue à une fréquence quelconque de la grille.
    fn a(&self, f: f32) -> f32 {
        let x = (f / GRILLE_BAS).ln() / (GRILLE_HAUT / GRILLE_BAS).ln() * (GRILLE_POINTS - 1) as f32;
        let x = x.clamp(0.0, (GRILLE_POINTS - 1) as f32);
        let i = (x as usize).min(GRILLE_POINTS - 2);
        let t = x - i as f32;
        self.enveloppe[i] + (self.enveloppe[i + 1] - self.enveloppe[i]) * t
    }
}

/// L'empreinte d'une voix enregistrée (mono, 48 kHz). `None` : pas assez de
/// voix chantée pour conclure — du silence, du bruit, une musique.
pub fn empreinte(pcm: &[f32]) -> Option<EmpreinteVoix> {
    if pcm.len() < 2 * TAILLE_FFT {
        return None;
    }
    // Pour YIN : sous 1 kHz, décimé à 12 kHz.
    let mut bas = pcm.to_vec();
    let mut passe_bas = Egaliseur::new(&[Bande::new(Forme::PasseBas, 1_000.0, 0.0, Q_NEUTRE).raide()]);
    for bloc in bas.chunks_mut(960) {
        passe_bas.traiter_trame(bloc);
    }
    let decime: Vec<f32> = bas.iter().step_by(DECIMATION).copied().collect();
    let taux = SAMPLE_RATE as f32 / DECIMATION as f32;
    let debuts: Vec<usize> = (0..=pcm.len() - TAILLE_FFT).step_by(PAS_ANALYSE).collect();
    let energies: Vec<f32> = debuts
        .iter()
        .map(|&d| pcm[d..d + TAILLE_FFT].iter().map(|s| s * s).sum::<f32>() / TAILLE_FFT as f32)
        .collect();
    let mut triees = energies.clone();
    triees.sort_by(|a, b| b.total_cmp(a));
    let reference = triees[triees.len() / 4];
    if reference < 1e-9 {
        return None;
    }

    let mut analyseur = Analyseur::new();
    let mut env = vec![0f32; CASES];
    let mut somme = vec![0f32; GRILLE_POINTS];
    let mut hauteurs = Vec::new();
    let hz_par_case = SAMPLE_RATE as f32 / TAILLE_FFT as f32;
    for (&debut, &energie) in debuts.iter().zip(&energies) {
        // Les trames de voix : à moins de 20 dB des fortes, et chantées.
        if energie < reference / 100.0 {
            continue;
        }
        let d = debut / DECIMATION;
        let Some(extrait) = decime.get(d..d + TAILLE_FFT / 2) else { continue };
        let Some((hz, aperiodicite)) = yin(extrait, taux, 60.0, 600.0) else { continue };
        if aperiodicite > 0.25 {
            continue;
        }
        hauteurs.push(hz);
        analyseur.enveloppe(&pcm[debut..debut + TAILLE_FFT], Some(SAMPLE_RATE as f32 / hz), &mut env);
        // Sur la grille, en dB, ramenée à moyenne nulle.
        let mut points = [0f32; GRILLE_POINTS];
        for (i, p) in points.iter_mut().enumerate() {
            let x = (grille(i) / hz_par_case).min((CASES - 1) as f32);
            let k = (x as usize).min(CASES - 2);
            let t = x - k as f32;
            *p = (env[k] + (env[k + 1] - env[k]) * t) * 20.0 / std::f32::consts::LN_10;
        }
        let moyenne = points.iter().sum::<f32>() / GRILLE_POINTS as f32;
        for (s, p) in somme.iter_mut().zip(points) {
            *s += p - moyenne;
        }
    }
    if hauteurs.len() < TRAMES_MIN {
        return None;
    }
    let trames = hauteurs.len();
    hauteurs.sort_by(|a, b| a.total_cmp(b));
    Some(EmpreinteVoix {
        f0_hz: hauteurs[trames / 2],
        enveloppe: somme.into_iter().map(|s| s / trames as f32).collect(),
        trames,
    })
}

/// Ce qu'il faut régler au changeur pour s'approcher d'une voix.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rapprochement {
    /// La hauteur, en demi-tons, bornée à ±12 comme le changeur.
    pub hauteur: f32,
    /// Les formants : où placer les siens, en fois leur place d'origine.
    pub formants: f32,
    /// La hauteur voulue sortait des bornes : l'imitation sera approchée.
    pub bornee: bool,
    /// La ressemblance des deux timbres une fois alignés (corrélation, de
    /// -1 à 1) : sous ~0,5, la mesure du timbre est peu sûre.
    pub confiance: f32,
}

/// Les réglages qui rapprochent la voix `moi` de la voix `cible`.
pub fn rapprocher(moi: &EmpreinteVoix, cible: &EmpreinteVoix) -> Rapprochement {
    let brute = 12.0 * (cible.f0_hz / moi.f0_hz).log2();
    let hauteur = brute.clamp(-12.0, 12.0);
    // L'étirement : celui dont la forme, pentes retirées, colle le mieux.
    let mut meilleur = (1.0f32, f32::MIN);
    let mut alpha = ETIREMENT_MIN;
    while alpha <= ETIREMENT_MAX + 1e-6 {
        let mut paires = Vec::with_capacity(GRILLE_POINTS);
        for i in 0..GRILLE_POINTS {
            let f = grille(i);
            if !(COMPARE_BAS..=COMPARE_HAUT).contains(&f) {
                continue;
            }
            let source = f / alpha;
            if !(GRILLE_BAS..=GRILLE_HAUT).contains(&source) {
                continue;
            }
            paires.push((f.ln(), cible.a(f), moi.a(source)));
        }
        if paires.len() >= 20 {
            let c = correlation_sans_pente(&paires);
            if c > meilleur.1 {
                meilleur = (alpha, c);
            }
        }
        alpha += 0.005;
    }
    Rapprochement { hauteur, formants: meilleur.0, bornee: (brute - hauteur).abs() > 0.05, confiance: meilleur.1 }
}

/// La corrélation de deux courbes (`y1`, `y2` en fonction de `x`), chacune
/// débarrassée de sa pente générale (régression linéaire en `x`).
fn correlation_sans_pente(paires: &[(f32, f32, f32)]) -> f32 {
    let n = paires.len() as f32;
    let mx = paires.iter().map(|p| p.0).sum::<f32>() / n;
    let residus = |sel: fn(&(f32, f32, f32)) -> f32| -> Vec<f32> {
        let my = paires.iter().map(sel).sum::<f32>() / n;
        let (mut sxy, mut sxx) = (0f32, 0f32);
        for p in paires {
            sxy += (p.0 - mx) * (sel(p) - my);
            sxx += (p.0 - mx) * (p.0 - mx);
        }
        let pente = if sxx > 0.0 { sxy / sxx } else { 0.0 };
        paires.iter().map(|p| sel(p) - my - pente * (p.0 - mx)).collect()
    };
    let (a, b) = (residus(|p| p.1), residus(|p| p.2));
    let (mut ab, mut aa, mut bb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(&b) {
        ab += x * y;
        aa += x * x;
        bb += y * y;
    }
    if aa > 0.0 && bb > 0.0 {
        ab / (aa * bb).sqrt()
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deux voyelles qui alternent (« a » puis « i »), séparées de silences :
    /// une source glottale (harmoniques en 1/h, −6 dB par octave, la
    /// fondamentale la plus forte) passée par des formants à `echelle` fois
    /// ceux d'une voix d'homme, 20 dB au-dessus du creux entre eux.
    fn parole(f0: f32, echelle: f32) -> Vec<f32> {
        let voyelles: [&[f32]; 2] = [&[730.0, 1_090.0, 2_440.0], &[270.0, 2_290.0, 3_010.0]];
        let mut x = Vec::new();
        for n in 0..6 {
            let formants: Vec<f32> = voyelles[n % 2].iter().map(|f| f * echelle).collect();
            let enveloppe = |f: f32| -> f32 {
                let mut a = 0.1;
                for &fm in &formants {
                    let d = (f - fm) / (0.12 * fm + 40.0);
                    a += (-d * d).exp();
                }
                a
            };
            let longueur = SAMPLE_RATE as usize * 3 / 10;
            let debut = x.len();
            x.resize(debut + longueur, 0.0);
            let mut h = 1;
            while f0 * h as f32 <= 7_000.0 {
                let f = f0 * h as f32;
                let a = 0.15 * enveloppe(f) / h as f32;
                for (i, s) in x[debut..].iter_mut().enumerate() {
                    *s += a * (2.0 * std::f32::consts::PI * f * i as f32 / SAMPLE_RATE as f32 + h as f32).sin();
                }
                h += 1;
            }
            x.resize(x.len() + SAMPLE_RATE as usize / 10, 0.0);
        }
        x
    }

    #[test]
    fn une_voix_se_mesure() {
        let e = empreinte(&parole(120.0, 1.0)).expect("une empreinte");
        assert!((e.f0_hz - 120.0).abs() < 2.0, "{:.1} Hz", e.f0_hz);
        assert!(e.trames >= 40);
    }

    #[test]
    fn le_rapprochement_retrouve_hauteur_et_formants() {
        let moi = empreinte(&parole(120.0, 1.0)).unwrap();
        let cible = empreinte(&parole(180.0, 1.17)).unwrap();
        let r = rapprocher(&moi, &cible);
        let attendu = 12.0 * 1.5f32.log2();
        assert!((r.hauteur - attendu).abs() < 0.3, "hauteur {:.2}", r.hauteur);
        assert!((r.formants - 1.17).abs() < 0.04, "formants {:.3}", r.formants);
        assert!(r.confiance > 0.6 && !r.bornee, "{r:?}");
        // Et dans l'autre sens.
        let retour = rapprocher(&cible, &moi);
        assert!((retour.formants - 1.0 / 1.17).abs() < 0.04, "retour {:.3}", retour.formants);
    }

    #[test]
    fn un_micro_plus_sombre_n_est_pas_une_autre_bouche() {
        // La même voix, aigus atténués : -8 dB au-dessus de 2 kHz.
        let clair = parole(120.0, 1.0);
        let mut sombre = clair.clone();
        let mut filtre = Egaliseur::new(&[Bande::new(Forme::EtagereHaute, 2_000.0, -8.0, Q_NEUTRE)]);
        for bloc in sombre.chunks_mut(960) {
            filtre.traiter_trame(bloc);
        }
        let r = rapprocher(&empreinte(&clair).unwrap(), &empreinte(&sombre).unwrap());
        assert!((r.formants - 1.0).abs() < 0.04, "formants {:.3}", r.formants);
        assert!(r.hauteur.abs() < 0.3);
    }

    #[test]
    fn pas_d_empreinte_sans_voix() {
        assert!(empreinte(&[]).is_none());
        assert!(empreinte(&vec![0f32; SAMPLE_RATE as usize]).is_none());
        let mut alea = 1u32;
        let souffle: Vec<f32> = (0..SAMPLE_RATE)
            .map(|_| {
                alea ^= alea << 13;
                alea ^= alea >> 17;
                alea ^= alea << 5;
                (alea as f32 / u32::MAX as f32 - 0.5) * 0.2
            })
            .collect();
        assert!(empreinte(&souffle).is_none());
    }
}
