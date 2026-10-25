//! Annulation d'écho acoustique (AEC), sur l'annulateur MDF de SpeexDSP.
//!
//! Le problème qu'elle règle : quelqu'un écoute le vocal sur haut-parleurs,
//! son micro capte la voix des autres, et chacun s'entend revenir avec un
//! temps de retard. Le débruitage n'y peut rien — l'écho EST de la voix.
//! L'annulateur, lui, connaît ce qui vient d'être joué (le signal
//! « lointain ») : un filtre adaptatif apprend le trajet acoustique
//! haut-parleurs → pièce → micro et soustrait sa contribution de la capture.
//!
//! Le filtre seul, sans l'étage « suppression de résidu » de Speex
//! (`speex_preprocess`) qui le suivait jusqu'à la 0.1.51. Débruitage coupé
//! — la chaîne du moteur a déjà le sien —, cet étage forçait un gain de 1
//! sur toutes les bandes (`preprocess.c`, « If noise suppression is off,
//! don't apply the gain ») : il ne supprimait rien. Et comme il travaille en
//! recouvrement sur deux trames, il rendait la trame PRÉCÉDENTE : 20 ms de
//! retard sur la voix de tout le monde, l'annulation étant active par
//! défaut. Le rallumer pour de bon (débruitage sans plancher) garde ces
//! 20 ms et mange la voix locale en double parole — 2,5 à 8,4 dB au banc,
//! contre 1,6 aujourd'hui. L'écho non linéaire (haut-parleurs qui saturent)
//! reste donc ce que le filtre linéaire ne sait pas enlever, comme avant.
//!
//! Sauf sur le haut-parleur d'un téléphone, où ce reste est le problème :
//! collé au micro, le haut-parleur y revient plus fort que la bouche de
//! celui qui parle, et le filtre seul (17 à 22 dB mesurés sur un Galaxy Z
//! Flip 6) laisse un écho au niveau de sa voix — aucun seuil ne les sépare
//! plus. [`Aec::avec_residu`] rallume donc l'étage, pour de bon cette fois
//! (débruitage allumé, sans plancher de bruit : il n'efface que l'écho), au
//! prix de ses 20 ms et d'un peu de la voix locale en double parole. Le PC
//! garde le filtre seul.
//!
//! Contrat d'intégration : appeler [`Aec::process`] pour CHAQUE trame de
//! capture, avec la trame de lecture de la même époque — y compris des
//! trames de silence quand rien ne joue. Le filtre vit de cette continuité ;
//! des trous dans le signal lointain lui font désapprendre le trajet.
//!
//! L'API publique de SpeexDSP est en entiers 16 bits : les conversions
//! aller-retour vivent ici, le moteur reste en f32.

mod garde;
mod recalage;
pub use garde::GardeEcho;
pub use recalage::Recaleur;

use std::os::raw::{c_int, c_void};

#[repr(C)]
struct SpeexEchoState {
    _opaque: [u8; 0],
}
#[repr(C)]
struct SpeexPreprocessState {
    _opaque: [u8; 0],
}
extern "C" {
    fn speex_echo_state_init(frame_size: c_int, filter_length: c_int) -> *mut SpeexEchoState;
    fn speex_echo_state_destroy(st: *mut SpeexEchoState);
    fn speex_echo_cancellation(
        st: *mut SpeexEchoState,
        rec: *const i16,
        play: *const i16,
        out: *mut i16,
    );
    fn speex_echo_ctl(st: *mut SpeexEchoState, request: c_int, ptr: *mut c_void) -> c_int;
    fn speex_preprocess_state_init(frame_size: c_int, sampling_rate: c_int) -> *mut SpeexPreprocessState;
    fn speex_preprocess_state_destroy(st: *mut SpeexPreprocessState);
    fn speex_preprocess_run(st: *mut SpeexPreprocessState, x: *mut i16) -> c_int;
    fn speex_preprocess_ctl(st: *mut SpeexPreprocessState, request: c_int, ptr: *mut c_void) -> c_int;
}

// L'identifiant de contrôle, tel que speex_echo.h le définit.
const SPEEX_ECHO_SET_SAMPLING_RATE: c_int = 24;
// Ceux de speex_preprocess.h.
const SPEEX_PREPROCESS_SET_DENOISE: c_int = 0;
const SPEEX_PREPROCESS_SET_AGC: c_int = 2;
const SPEEX_PREPROCESS_SET_VAD: c_int = 4;
const SPEEX_PREPROCESS_SET_DEREVERB: c_int = 8;
const SPEEX_PREPROCESS_SET_NOISE_SUPPRESS: c_int = 18;
const SPEEX_PREPROCESS_SET_ECHO_SUPPRESS_ACTIVE: c_int = 22;
const SPEEX_PREPROCESS_SET_ECHO_STATE: c_int = 24;

/// L'annulateur : un filtre adaptatif, pour un flux mono à fréquence fixe.
pub struct Aec {
    echo: *mut SpeexEchoState,
    /// La suppression de résidu, si demandée ([`Aec::avec_residu`]).
    residu: *mut SpeexPreprocessState,
    frame: usize,
    rec: Vec<i16>,
    play: Vec<i16>,
    out: Vec<i16>,
}

// Les pointeurs appartiennent à cette structure seule, et l'annulateur est
// utilisé depuis un seul fil (celui de la capture) — le déplacer d'un fil à
// l'autre est sûr, le partager ne l'est pas (pas de Sync).
unsafe impl Send for Aec {}

impl Aec {
    /// `frame` échantillons par trame, une queue de filtre de `tail`
    /// échantillons (le trajet acoustique + les tampons doivent tenir
    /// dedans), à `rate` Hz.
    pub fn new(frame: usize, tail: usize, rate: u32) -> Option<Self> {
        unsafe {
            let echo = speex_echo_state_init(frame as c_int, tail as c_int);
            if echo.is_null() {
                return None;
            }
            let mut hz = rate as c_int;
            speex_echo_ctl(
                echo,
                SPEEX_ECHO_SET_SAMPLING_RATE,
                &mut hz as *mut c_int as *mut c_void,
            );
            Some(Self {
                echo,
                residu: std::ptr::null_mut(),
                frame,
                rec: vec![0; frame],
                play: vec![0; frame],
                out: vec![0; frame],
            })
        }
    }

    /// Le filtre, suivi de la suppression de résidu : ce qui reste d'écho
    /// après lui, l'étage l'efface bande par bande — jusqu'à −40 dB quand
    /// personne ne parle ici, −6 dB seulement quand on parle par-dessus.
    /// Speex en prévoit −15 : sur un téléphone, où la voix arrive au niveau
    /// du reste d'écho, il coupait la voix bande par bande et trame par
    /// trame, et elle sortait hachée, incompréhensible. Son
    /// débruitage est allumé — sans lui, il n'applique aucun gain — mais
    /// sans plancher de bruit : la chaîne du moteur a son propre débruiteur,
    /// et deux en série mangent la voix. Rend la trame avec 20 ms de retard.
    pub fn avec_residu(frame: usize, tail: usize, rate: u32) -> Option<Self> {
        let mut aec = Self::new(frame, tail, rate)?;
        unsafe {
            let residu = speex_preprocess_state_init(frame as c_int, rate as c_int);
            if residu.is_null() {
                return None;
            }
            let regler = |requete: c_int, mut valeur: c_int| {
                speex_preprocess_ctl(residu, requete, &mut valeur as *mut c_int as *mut c_void);
            };
            regler(SPEEX_PREPROCESS_SET_DENOISE, 1);
            regler(SPEEX_PREPROCESS_SET_NOISE_SUPPRESS, 0);
            regler(SPEEX_PREPROCESS_SET_AGC, 0);
            regler(SPEEX_PREPROCESS_SET_VAD, 0);
            regler(SPEEX_PREPROCESS_SET_DEREVERB, 0);
            regler(SPEEX_PREPROCESS_SET_ECHO_SUPPRESS_ACTIVE, 6);
            speex_preprocess_ctl(residu, SPEEX_PREPROCESS_SET_ECHO_STATE, aec.echo as *mut c_void);
            aec.residu = residu;
        }
        Some(aec)
    }

    /// Nettoie `near` (la capture) en place, connaissant `far` (ce qui vient
    /// d'être joué à la même époque). Les deux tranches font `frame`
    /// échantillons — c'est un invariant du moteur, pas une entrée.
    pub fn process(&mut self, near: &mut [f32], far: &[f32]) {
        assert_eq!(near.len(), self.frame);
        assert_eq!(far.len(), self.frame);
        for (dst, &src) in self.rec.iter_mut().zip(near.iter()) {
            *dst = (src.clamp(-1.0, 1.0) * 32767.0) as i16;
        }
        for (dst, &src) in self.play.iter_mut().zip(far.iter()) {
            *dst = (src.clamp(-1.0, 1.0) * 32767.0) as i16;
        }
        unsafe {
            speex_echo_cancellation(
                self.echo,
                self.rec.as_ptr(),
                self.play.as_ptr(),
                self.out.as_mut_ptr(),
            );
            if !self.residu.is_null() {
                speex_preprocess_run(self.residu, self.out.as_mut_ptr());
            }
        }
        for (dst, &src) in near.iter_mut().zip(self.out.iter()) {
            *dst = src as f32 / 32768.0;
        }
    }
}

impl Drop for Aec {
    fn drop(&mut self) {
        unsafe {
            if !self.residu.is_null() {
                speex_preprocess_state_destroy(self.residu);
            }
            speex_echo_state_destroy(self.echo);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: usize = 960; // 20 ms à 48 kHz, la trame du moteur.
    const RATE: u32 = 48_000;

    /// Bruit pseudo-aléatoire déterministe : le pire signal pour tricher,
    /// le meilleur pour faire converger un filtre adaptatif.
    struct Noise(u32);
    impl Noise {
        fn frame(&mut self) -> Vec<f32> {
            (0..FRAME)
                .map(|_| {
                    self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
                    (self.0 as f32 / u32::MAX as f32 - 0.5) * 0.6
                })
                .collect()
        }
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / x.len() as f64).sqrt()
    }

    /// Le cœur du contrat : un écho pur (le lointain, retardé et atténué,
    /// revenu dans le micro) doit fondre une fois le filtre convergé.
    #[test]
    fn un_echo_pur_est_annule() {
        let mut aec = Aec::new(FRAME, 4800, RATE).expect("création de l'annulateur");
        let mut noise = Noise(0x2A2A2A2A);
        // Trajet acoustique simulé : 10 ms de retard, moitié du niveau.
        const DELAY: usize = 480;
        let mut ligne = vec![0f32; DELAY];
        let (mut brut, mut nettoye) = (0f64, 0f64);
        for n in 0..200 {
            let far = noise.frame();
            ligne.extend_from_slice(&far);
            let mut near: Vec<f32> = ligne.drain(..FRAME).map(|s| s * 0.5).collect();
            if n >= 150 {
                brut += rms(&near);
            }
            aec.process(&mut near, &far);
            if n >= 150 {
                nettoye += rms(&near);
            }
        }
        assert!(
            nettoye < brut * 0.25,
            "écho insuffisamment annulé : {nettoye:.4} pour {brut:.4} brut"
        );
    }

    /// Un haut-parleur qui sature : le filtre linéaire laisse un reste, que
    /// la suppression de résidu efface — c'est tout ce qu'elle doit au
    /// téléphone.
    #[test]
    fn la_suppression_de_residu_efface_ce_que_le_filtre_laisse() {
        let mesurer = |residu: bool| -> f64 {
            let mut aec = if residu {
                Aec::avec_residu(FRAME, 4800, RATE)
            } else {
                Aec::new(FRAME, 4800, RATE)
            }
            .expect("création de l'annulateur");
            let mut noise = Noise(0x0EC0_0EC0);
            const DELAY: usize = 480;
            let mut ligne = vec![0f32; DELAY];
            let (mut brut, mut nettoye) = (0f64, 0f64);
            for n in 0..250 {
                let far = noise.frame();
                ligne.extend_from_slice(&far);
                // Écrêtage doux : la part non linéaire que le filtre ne sait
                // pas modéliser.
                let mut near: Vec<f32> = ligne.drain(..FRAME).map(|s| (s * 2.5).tanh() * 0.3).collect();
                if n >= 200 {
                    brut += rms(&near);
                }
                aec.process(&mut near, &far);
                if n >= 200 {
                    nettoye += rms(&near);
                }
            }
            20.0 * (brut / nettoye.max(1e-12)).log10()
        };
        let filtre = mesurer(false);
        let avec = mesurer(true);
        assert!(
            avec > filtre + 6.0,
            "avec la suppression de résidu {avec:.1} dB, le filtre seul {filtre:.1} dB"
        );
    }

    /// Et la voix seule, sans rien au haut-parleur, la traverse : la
    /// suppression ne vise que l'écho, pas le bruit ni la parole.
    #[test]
    fn la_suppression_de_residu_laisse_passer_la_voix() {
        let mut aec = Aec::avec_residu(FRAME, 4800, RATE).expect("création de l'annulateur");
        let silence = vec![0f32; FRAME];
        let mut phase = 0f32;
        let (mut avant, mut apres) = (0f64, 0f64);
        for n in 0..100 {
            let mut near: Vec<f32> = (0..FRAME)
                .map(|_| {
                    phase += 0.05;
                    phase.sin() * 0.3
                })
                .collect();
            if n >= 50 {
                avant += rms(&near);
            }
            aec.process(&mut near, &silence);
            if n >= 50 {
                apres += rms(&near);
            }
        }
        assert!(apres > avant * 0.8, "la voix a été mangée : {apres:.4} pour {avant:.4}");
    }

    /// Et sans rien au haut-parleur, la voix capturée ressort entière :
    /// l'annulateur ne doit pas manger ce qu'il n'a pas à enlever.
    #[test]
    fn sans_lointain_la_voix_passe() {
        let mut aec = Aec::new(FRAME, 4800, RATE).expect("création de l'annulateur");
        let silence = vec![0f32; FRAME];
        let mut phase = 0f32;
        let (mut avant, mut apres) = (0f64, 0f64);
        for n in 0..100 {
            let mut near: Vec<f32> = (0..FRAME)
                .map(|_| {
                    phase += 0.05;
                    phase.sin() * 0.3
                })
                .collect();
            if n >= 50 {
                avant += rms(&near);
            }
            aec.process(&mut near, &silence);
            if n >= 50 {
                apres += rms(&near);
            }
        }
        assert!(
            apres > avant * 0.7,
            "la voix a été mangée sans écho à enlever : {apres:.4} pour {avant:.4}"
        );
    }

    /// Corrélation normalisée : 1 pour deux signaux identiques, autour de 0
    /// pour deux bruits indépendants — et 0 face au silence.
    fn correlation(a: &[f32], b: &[f32]) -> f64 {
        let (mut ab, mut aa, mut bb) = (0f64, 0f64, 0f64);
        for (&x, &y) in a.iter().zip(b) {
            ab += x as f64 * y as f64;
            aa += x as f64 * x as f64;
            bb += y as f64 * y as f64;
        }
        if aa == 0.0 || bb == 0.0 {
            return 0.0;
        }
        ab / (aa * bb).sqrt()
    }

    /// Et sans retard : la trame rendue est celle qu'on vient de donner, pas
    /// la précédente. L'étage de suppression de résidu rendait la trame
    /// d'avant, à l'échantillon près — 20 ms de plus sur toute voix qui
    /// partait, sans que les deux tests du dessus, qui ne mesurent que des
    /// niveaux, puissent le voir.
    #[test]
    fn la_capture_ressort_sans_retard() {
        let mut aec = Aec::new(FRAME, 4800, RATE).expect("création de l'annulateur");
        let silence = vec![0f32; FRAME];
        let mut noise = Noise(0x5EED_1234);
        let mut precedente: Option<Vec<f32>> = None;
        for n in 0..20 {
            let entree = noise.frame();
            let mut near = entree.clone();
            aec.process(&mut near, &silence);
            let avec_elle = correlation(&near, &entree);
            assert!(
                avec_elle > 0.95,
                "trame {n} : la sortie ne ressemble pas à l'entrée ({avec_elle:.3})"
            );
            if let Some(avant) = &precedente {
                let avec_l_autre = correlation(&near, avant);
                assert!(
                    avec_l_autre.abs() < 0.2,
                    "trame {n} : la sortie ressemble à la trame d'avant ({avec_l_autre:.3})"
                );
            }
            precedente = Some(entree);
        }
    }
}
