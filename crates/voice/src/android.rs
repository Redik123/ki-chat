//! Le micro sur Android, ouvert par AAudio en mode « communication ».
//!
//! cpal ouvre le micro avec le préréglage par défaut d'AAudio, « reconnaissance
//! vocale » : aucun traitement du téléphone, et surtout pas son annulation
//! d'écho. Le haut-parleur d'un téléphone est à quelques centimètres de son
//! micro : mesurée sur un Galaxy Z Flip 6 (Android 16), la voix des autres
//! revenait dans le micro à −5 dB, presque à saturation et déformée — rien
//! que l'annulateur linéaire du moteur (`ki_aec`) ne sache retirer. Les
//! autres s'entendaient parler.
//!
//! Le préréglage « communication » est celui des appels : le téléphone y
//! branche son propre annulateur d'écho, réglé pour son haut-parleur et son
//! micro, avec ce qu'il joue comme référence. Au même banc, l'écho tombait à
//! −32 dB (−27 dB). La sortie, elle, ne change pas : un flux « communication »
//! partirait vers l'écouteur d'appel, pas vers le haut-parleur.
//!
//! `AAudioStreamBuilder_setInputPreset` n'existe qu'à partir de l'API 28,
//! l'appli démarre dès l'API 26 : la fonction est cherchée à l'exécution.
//! Absente, l'ouverture échoue, et le moteur retombe sur cpal comme avant.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::bail;
use ndk_sys as ffi;

use crate::{journal, ChunkTx, SAMPLE_RATE};

/// Ce que reçoivent les rappels d'AAudio, sur leur propre fil.
struct Rappel {
    tx: ChunkTx,
    alive: Arc<AtomicBool>,
}

/// Le flux du micro. Le lâcher l'arrête et le ferme.
pub(crate) struct MicroAndroid {
    flux: *mut ffi::AAudioStream,
    rappel: *mut Rappel,
}

// Le flux n'est touché qu'à l'ouverture et à la fermeture, par le fil qui
// le tient ; AAudio ne lit `rappel` que depuis ses rappels, que la fermeture
// arrête avant qu'il ne soit libéré.
unsafe impl Send for MicroAndroid {}

impl Drop for MicroAndroid {
    fn drop(&mut self) {
        unsafe {
            ffi::AAudioStream_requestStop(self.flux);
            // Attend la fin du rappel en cours : `rappel` peut partir ensuite.
            ffi::AAudioStream_close(self.flux);
            drop(Box::from_raw(self.rappel));
        }
    }
}

unsafe extern "C" fn donnees(
    _flux: *mut ffi::AAudioStream,
    user: *mut c_void,
    audio: *mut c_void,
    trames: i32,
) -> ffi::aaudio_data_callback_result_t {
    let rappel = &*(user as *const Rappel);
    let n = trames.max(0) as usize;
    let entree = std::slice::from_raw_parts(audio as *const f32, n);
    let mut buf = rappel.tx.take(n);
    buf.extend_from_slice(entree);
    let _ = rappel.tx.send(buf);
    ffi::AAUDIO_CALLBACK_RESULT_CONTINUE as ffi::aaudio_data_callback_result_t
}

/// Le flux tombe (micro débranché, casque qui change) : on le signale, et
/// `capture_loop` le rouvre — comme pour cpal.
unsafe extern "C" fn erreur(_flux: *mut ffi::AAudioStream, user: *mut c_void, code: ffi::aaudio_result_t) {
    let rappel = &*(user as *const Rappel);
    if rappel.alive.swap(false, Ordering::Relaxed) {
        let texte = texte_d_erreur(code);
        tracing::warn!("erreur flux micro AAudio : {texte}");
        journal(format!("erreur du flux micro (AAudio) : {texte}"));
    }
}

fn texte_d_erreur(code: ffi::aaudio_result_t) -> String {
    unsafe {
        let p = ffi::AAudio_convertResultToText(code);
        if p.is_null() {
            format!("code {code}")
        } else {
            std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

type ReglerPreset = unsafe extern "C" fn(*mut ffi::AAudioStreamBuilder, ffi::aaudio_input_preset_t);

/// `AAudioStreamBuilder_setInputPreset`, si ce téléphone l'a (API 28).
fn regler_preset() -> Option<ReglerPreset> {
    unsafe {
        let lib = libc::dlopen(c"libaaudio.so".as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD);
        let lib = if lib.is_null() { libc::dlopen(c"libaaudio.so".as_ptr(), libc::RTLD_NOW) } else { lib };
        if lib.is_null() {
            return None;
        }
        let f = libc::dlsym(lib, c"AAudioStreamBuilder_setInputPreset".as_ptr());
        (!f.is_null()).then(|| std::mem::transmute::<*mut c_void, ReglerPreset>(f))
    }
}

/// Ouvre le micro, mono en flottants, et le démarre. `communication` :
/// le préréglage des appels, annulation d'écho du téléphone comprise ;
/// sinon celui de cpal (« reconnaissance vocale », sans traitement).
/// Rend le flux et sa fréquence réelle.
pub(crate) fn ouvrir_micro(
    communication: bool,
    tx: ChunkTx,
    alive: Arc<AtomicBool>,
) -> anyhow::Result<(MicroAndroid, u32)> {
    let Some(preset) = regler_preset() else {
        bail!("AAudioStreamBuilder_setInputPreset absent (Android antérieur à 9)");
    };
    let rappel = Box::into_raw(Box::new(Rappel { tx, alive }));
    unsafe {
        let mut b: *mut ffi::AAudioStreamBuilder = std::ptr::null_mut();
        let r = ffi::AAudio_createStreamBuilder(&mut b);
        if r != 0 || b.is_null() {
            drop(Box::from_raw(rappel));
            bail!("AAudio : {}", texte_d_erreur(r));
        }
        ffi::AAudioStreamBuilder_setDirection(b, ffi::AAUDIO_DIRECTION_INPUT as _);
        ffi::AAudioStreamBuilder_setFormat(b, ffi::AAUDIO_FORMAT_PCM_FLOAT as _);
        ffi::AAudioStreamBuilder_setChannelCount(b, 1);
        ffi::AAudioStreamBuilder_setSampleRate(b, SAMPLE_RATE as i32);
        ffi::AAudioStreamBuilder_setSharingMode(b, ffi::AAUDIO_SHARING_MODE_SHARED as _);
        ffi::AAudioStreamBuilder_setPerformanceMode(b, ffi::AAUDIO_PERFORMANCE_MODE_LOW_LATENCY as _);
        preset(
            b,
            if communication {
                ffi::AAUDIO_INPUT_PRESET_VOICE_COMMUNICATION as _
            } else {
                ffi::AAUDIO_INPUT_PRESET_VOICE_RECOGNITION as _
            },
        );
        ffi::AAudioStreamBuilder_setDataCallback(b, Some(donnees), rappel as *mut c_void);
        ffi::AAudioStreamBuilder_setErrorCallback(b, Some(erreur), rappel as *mut c_void);
        let mut flux: *mut ffi::AAudioStream = std::ptr::null_mut();
        let r = ffi::AAudioStreamBuilder_openStream(b, &mut flux);
        ffi::AAudioStreamBuilder_delete(b);
        if r != 0 || flux.is_null() {
            drop(Box::from_raw(rappel));
            bail!("ouverture du micro AAudio : {}", texte_d_erreur(r));
        }
        let micro = MicroAndroid { flux, rappel };
        // Mono en flottants, ou rien : `donnees` lit le tampon comme tel.
        let canaux = ffi::AAudioStream_getChannelCount(flux);
        let format = ffi::AAudioStream_getFormat(flux);
        if canaux != 1 || format != ffi::AAUDIO_FORMAT_PCM_FLOAT as ffi::aaudio_format_t {
            bail!("micro AAudio en {canaux} canaux, format {format} : non géré");
        }
        let r = ffi::AAudioStream_requestStart(flux);
        if r != 0 {
            bail!("démarrage du micro AAudio : {}", texte_d_erreur(r));
        }
        let frequence = ffi::AAudioStream_getSampleRate(flux).max(1) as u32;
        Ok((micro, frequence))
    }
}
