//! Enveloppe sûre au-dessus de libopus 1.6 (compilé par build.rs avec
//! DRED + Deep PLC + OSCE). La surface imite le crate `opus` 0.3 pour que
//! le moteur audio migre sans se réécrire, et ajoute l'API DRED que
//! l'écosystème Rust n'expose nulle part.

use std::os::raw::{c_int, c_uchar};

// ---------------------------------------------------------------------------
// FFI brute
// ---------------------------------------------------------------------------

#[repr(C)]
struct OpusEncoderC {
    _priv: [u8; 0],
}
#[repr(C)]
struct OpusDecoderC {
    _priv: [u8; 0],
}
#[repr(C)]
struct OpusDREDDecoderC {
    _priv: [u8; 0],
}
#[repr(C)]
struct OpusDREDC {
    _priv: [u8; 0],
}

extern "C" {
    fn opus_encoder_create(
        fs: i32,
        channels: c_int,
        application: c_int,
        error: *mut c_int,
    ) -> *mut OpusEncoderC;
    fn opus_encoder_destroy(st: *mut OpusEncoderC);
    fn opus_encoder_ctl(st: *mut OpusEncoderC, request: c_int, ...) -> c_int;
    fn opus_encode_float(
        st: *mut OpusEncoderC,
        pcm: *const f32,
        frame_size: c_int,
        data: *mut c_uchar,
        max_data_bytes: i32,
    ) -> i32;

    fn opus_decoder_create(fs: i32, channels: c_int, error: *mut c_int) -> *mut OpusDecoderC;
    fn opus_decoder_destroy(st: *mut OpusDecoderC);
    fn opus_decoder_ctl(st: *mut OpusDecoderC, request: c_int, ...) -> c_int;
    fn opus_decode_float(
        st: *mut OpusDecoderC,
        data: *const c_uchar,
        len: i32,
        pcm: *mut f32,
        frame_size: c_int,
        decode_fec: c_int,
    ) -> c_int;

    // --- API DRED (redondance neuronale), expérimentale dans 1.5/1.6 ---
    fn opus_dred_decoder_create(error: *mut c_int) -> *mut OpusDREDDecoderC;
    fn opus_dred_decoder_destroy(dec: *mut OpusDREDDecoderC);
    fn opus_dred_alloc(error: *mut c_int) -> *mut OpusDREDC;
    fn opus_dred_free(dec: *mut OpusDREDC);
    fn opus_dred_parse(
        dred_dec: *mut OpusDREDDecoderC,
        dred: *mut OpusDREDC,
        data: *const c_uchar,
        len: i32,
        max_dred_samples: i32,
        sampling_rate: i32,
        dred_end: *mut c_int,
        defer_processing: c_int,
    ) -> c_int;
    fn opus_decoder_dred_decode_float(
        st: *mut OpusDecoderC,
        dred: *const OpusDREDC,
        dred_offset: i32,
        pcm: *mut f32,
        frame_size: c_int,
    ) -> c_int;
}

// Constantes d'opus_defines.h — stables depuis opus 1.0 pour les classiques,
// et vérifiées dans les sources 1.6 pour le DRED.
const OPUS_APPLICATION_VOIP: c_int = 2048;
/// Musique et son de jeu : pas de modèle de parole, la bande passante à la
/// fidélité.
const OPUS_APPLICATION_AUDIO: c_int = 2049;
const OPUS_SET_BITRATE_REQUEST: c_int = 4002;
const OPUS_SET_COMPLEXITY_REQUEST: c_int = 4010;
const OPUS_SET_INBAND_FEC_REQUEST: c_int = 4012;
const OPUS_SET_PACKET_LOSS_PERC_REQUEST: c_int = 4014;
/// Vérifié dans include/opus_defines.h de libopus 1.6.1. La valeur est le
/// nombre de trames redondantes de 10 ms (0..=104) : 100 ≈ 1 s.
const OPUS_SET_DRED_DURATION_REQUEST: c_int = 4050;

// ---------------------------------------------------------------------------
// Enveloppe sûre (surface compatible avec le crate `opus` 0.3)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(pub i32);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "erreur opus ({})", self.0)
    }
}
impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn check(code: c_int) -> Result<c_int> {
    if code < 0 {
        Err(Error(code))
    } else {
        Ok(code)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Channels {
    Mono = 1,
    Stereo = 2,
}

#[derive(Debug, Clone, Copy)]
pub enum Application {
    Voip,
    /// Musique, son de jeu : fidélité plutôt que modèle de parole.
    Audio,
}

#[derive(Debug, Clone, Copy)]
pub enum Bitrate {
    Bits(i32),
}

pub struct Encoder {
    ptr: *mut OpusEncoderC,
    /// Pour convertir la longueur du tampon PCM (entrelacé) en trame par
    /// canal : libopus compte en échantillons par canal, pas en flottants.
    channels: usize,
}
// libopus n'a pas d'état global : un état par objet, déplaçable entre threads.
unsafe impl Send for Encoder {}

impl Encoder {
    pub fn new(sample_rate: u32, channels: Channels, app: Application) -> Result<Self> {
        let mut err: c_int = 0;
        let application = match app {
            Application::Voip => OPUS_APPLICATION_VOIP,
            Application::Audio => OPUS_APPLICATION_AUDIO,
        };
        let ptr = unsafe {
            opus_encoder_create(sample_rate as i32, channels as c_int, application, &mut err)
        };
        if ptr.is_null() || err != 0 {
            return Err(Error(err));
        }
        Ok(Self { ptr, channels: channels as usize })
    }

    pub fn set_bitrate(&mut self, bitrate: Bitrate) -> Result<()> {
        let Bitrate::Bits(bits) = bitrate;
        unsafe { check(opus_encoder_ctl(self.ptr, OPUS_SET_BITRATE_REQUEST, bits)).map(|_| ()) }
    }

    pub fn set_inband_fec(&mut self, on: bool) -> Result<()> {
        unsafe {
            check(opus_encoder_ctl(self.ptr, OPUS_SET_INBAND_FEC_REQUEST, on as c_int)).map(|_| ())
        }
    }

    pub fn set_packet_loss_perc(&mut self, pct: i32) -> Result<()> {
        unsafe {
            check(opus_encoder_ctl(self.ptr, OPUS_SET_PACKET_LOSS_PERC_REQUEST, pct)).map(|_| ())
        }
    }

    pub fn set_complexity(&mut self, c: i32) -> Result<()> {
        unsafe { check(opus_encoder_ctl(self.ptr, OPUS_SET_COMPLEXITY_REQUEST, c)).map(|_| ()) }
    }

    /// Durée de redondance neuronale DRED embarquée dans chaque paquet.
    /// 0 = désactivé. Unités : voir opus_defines.h (renseigné par build).
    pub fn set_dred_duration(&mut self, value: i32) -> Result<()> {
        unsafe {
            check(opus_encoder_ctl(self.ptr, OPUS_SET_DRED_DURATION_REQUEST, value)).map(|_| ())
        }
    }

    /// `pcm` : une trame entrelacée (tous les canaux) ; sa longueur divisée
    /// par le nombre de canaux donne la taille de trame que libopus attend.
    pub fn encode_float(&mut self, pcm: &[f32], out: &mut [u8]) -> Result<usize> {
        let n = unsafe {
            opus_encode_float(
                self.ptr,
                pcm.as_ptr(),
                (pcm.len() / self.channels) as c_int,
                out.as_mut_ptr(),
                out.len() as i32,
            )
        };
        check(n).map(|n| n as usize)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { opus_encoder_destroy(self.ptr) };
    }
}

pub struct Decoder {
    ptr: *mut OpusDecoderC,
    /// Même raison que pour l'encodeur : la capacité du tampon PCM se
    /// compte par canal.
    channels: usize,
}
unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new(sample_rate: u32, channels: Channels) -> Result<Self> {
        let mut err: c_int = 0;
        let ptr = unsafe { opus_decoder_create(sample_rate as i32, channels as c_int, &mut err) };
        if ptr.is_null() || err != 0 {
            return Err(Error(err));
        }
        Ok(Self { ptr, channels: channels as usize })
    }

    /// Complexité du décodeur : >= 5 active le Deep PLC (masquage de perte
    /// neuronal) quand libopus est compilé avec.
    pub fn set_complexity(&mut self, c: i32) -> Result<()> {
        unsafe { check(opus_decoder_ctl(self.ptr, OPUS_SET_COMPLEXITY_REQUEST, c)).map(|_| ()) }
    }

    /// data vide = dissimulation de perte (PLC). fec = reconstruire la trame
    /// précédente depuis la redondance LBRR de ce paquet. `pcm` est
    /// entrelacé ; rend le nombre d'échantillons **par canal** décodés.
    pub fn decode_float(&mut self, data: &[u8], pcm: &mut [f32], fec: bool) -> Result<usize> {
        let (ptr, len) = if data.is_empty() {
            (std::ptr::null(), 0)
        } else {
            (data.as_ptr(), data.len() as i32)
        };
        let n = unsafe {
            opus_decode_float(
                self.ptr,
                ptr,
                len,
                pcm.as_mut_ptr(),
                (pcm.len() / self.channels) as c_int,
                fec as c_int,
            )
        };
        check(n).map(|n| n as usize)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { opus_decoder_destroy(self.ptr) };
    }
}

/// Décodeur de redondance neuronale : extrait d'un paquet reçu l'audio des
/// trames PERDUES qui l'ont précédé (jusqu'à ~1 s), et le resynthétise.
pub struct Dred {
    dec: *mut OpusDREDDecoderC,
    state: *mut OpusDREDC,
    /// Échantillons de redondance disponibles après le dernier parse.
    available: i32,
}
unsafe impl Send for Dred {}

impl Dred {
    pub fn new() -> Result<Self> {
        let mut err: c_int = 0;
        let dec = unsafe { opus_dred_decoder_create(&mut err) };
        if dec.is_null() || err != 0 {
            return Err(Error(err));
        }
        let state = unsafe { opus_dred_alloc(&mut err) };
        if state.is_null() || err != 0 {
            unsafe { opus_dred_decoder_destroy(dec) };
            return Err(Error(err));
        }
        Ok(Self { dec, state, available: 0 })
    }

    /// Analyse la redondance d'un paquet. Retourne le nombre d'échantillons
    /// de passé reconstructibles (0 = pas de DRED dans ce paquet).
    pub fn parse(&mut self, packet: &[u8], max_samples: i32, sample_rate: i32) -> Result<i32> {
        let mut dred_end: c_int = 0;
        let n = unsafe {
            opus_dred_parse(
                self.dec,
                self.state,
                packet.as_ptr(),
                packet.len() as i32,
                max_samples,
                sample_rate,
                &mut dred_end,
                0,
            )
        };
        self.available = check(n)?;
        Ok(self.available)
    }

    pub fn available(&self) -> i32 {
        self.available
    }

    /// Resynthétise une trame perdue. `offset_samples` = distance entre la
    /// trame à reconstruire et le paquet porteur du DRED (en échantillons).
    pub fn decode_into(
        &self,
        decoder: &mut Decoder,
        offset_samples: i32,
        pcm: &mut [f32],
    ) -> Result<usize> {
        let n = unsafe {
            opus_decoder_dred_decode_float(
                decoder.ptr,
                self.state,
                offset_samples,
                pcm.as_mut_ptr(),
                pcm.len() as c_int,
            )
        };
        check(n).map(|n| n as usize)
    }
}

impl Drop for Dred {
    fn drop(&mut self) {
        unsafe {
            opus_dred_free(self.state);
            opus_dred_decoder_destroy(self.dec);
        }
    }
}
