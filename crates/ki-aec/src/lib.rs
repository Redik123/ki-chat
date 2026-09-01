//! Annulation d'écho acoustique (AEC), sur l'annulateur MDF de SpeexDSP.
//!
//! Le problème qu'elle règle : quelqu'un écoute le vocal sur haut-parleurs,
//! son micro capte la voix des autres, et chacun s'entend revenir avec un
//! temps de retard. Le débruitage n'y peut rien — l'écho EST de la voix.
//! L'annulateur, lui, connaît ce qui vient d'être joué (le signal
//! « lointain ») : un filtre adaptatif apprend le trajet acoustique
//! haut-parleurs → pièce → micro et soustrait sa contribution de la capture,
//! puis la suppression de résidu efface ce que le filtre a laissé.
//!
//! Contrat d'intégration : appeler [`Aec::process`] pour CHAQUE trame de
//! capture, avec la trame de lecture de la même époque — y compris des
//! trames de silence quand rien ne joue. Le filtre vit de cette continuité ;
//! des trous dans le signal lointain lui font désapprendre le trajet.
//!
//! L'API publique de SpeexDSP est en entiers 16 bits : les conversions
//! aller-retour vivent ici, le moteur reste en f32.

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

    fn speex_preprocess_state_init(
        frame_size: c_int,
        sampling_rate: c_int,
    ) -> *mut SpeexPreprocessState;
    fn speex_preprocess_state_destroy(st: *mut SpeexPreprocessState);
    fn speex_preprocess_run(st: *mut SpeexPreprocessState, x: *mut i16) -> c_int;
    fn speex_preprocess_ctl(
        st: *mut SpeexPreprocessState,
        request: c_int,
        ptr: *mut c_void,
    ) -> c_int;
}

// Les identifiants de contrôle, tels que speex_echo.h et speex_preprocess.h
// les définissent.
const SPEEX_ECHO_SET_SAMPLING_RATE: c_int = 24;
const SPEEX_PREPROCESS_SET_DENOISE: c_int = 0;
const SPEEX_PREPROCESS_SET_AGC: c_int = 2;
const SPEEX_PREPROCESS_SET_ECHO_STATE: c_int = 24;

/// L'annulateur : filtre adaptatif + suppression de résidu, pour un flux
/// mono à fréquence fixe.
pub struct Aec {
    echo: *mut SpeexEchoState,
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
            let residu = speex_preprocess_state_init(frame as c_int, rate as c_int);
            if residu.is_null() {
                speex_echo_state_destroy(echo);
                return None;
            }
            // La suppression de résidu seulement : le débruitage et le gain
            // automatique de Speex restent éteints — la chaîne du moteur a
            // déjà les siens, et deux débruiteurs en série mangent la voix.
            let mut off: c_int = 0;
            speex_preprocess_ctl(
                residu,
                SPEEX_PREPROCESS_SET_DENOISE,
                &mut off as *mut c_int as *mut c_void,
            );
            let mut off2: c_int = 0;
            speex_preprocess_ctl(
                residu,
                SPEEX_PREPROCESS_SET_AGC,
                &mut off2 as *mut c_int as *mut c_void,
            );
            speex_preprocess_ctl(residu, SPEEX_PREPROCESS_SET_ECHO_STATE, echo as *mut c_void);
            Some(Self {
                echo,
                residu,
                frame,
                rec: vec![0; frame],
                play: vec![0; frame],
                out: vec![0; frame],
            })
        }
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
            speex_preprocess_run(self.residu, self.out.as_mut_ptr());
        }
        for (dst, &src) in near.iter_mut().zip(self.out.iter()) {
            *dst = src as f32 / 32768.0;
        }
    }
}

impl Drop for Aec {
    fn drop(&mut self) {
        unsafe {
            speex_preprocess_state_destroy(self.residu);
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
}
