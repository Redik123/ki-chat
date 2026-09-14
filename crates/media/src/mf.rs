//! Media Foundation : le *Source Reader* démultiplexe et décode.
//!
//! On lui demande du NV12 pour l'image et du float entrelacé pour le son, à
//! la cadence et au nombre de voies **natifs** du fichier : le lecteur sait
//! décoder et convertir les formats de pixels, pas rééchantillonner — ça, on
//! le fait nous-mêmes (`son.rs`). Décodage logiciel (DXVA désactivé) : le
//! décodeur de Microsoft est rapide et n'a pas d'humeur de pilote ; le
//! matériel viendra si un portable en a besoin.
//!
//! Tout ce qui parle à COM vit ici, derrière le trait `Lecteur`.

use std::mem::ManuallyDrop;
use std::path::Path;
use std::ptr::null_mut;
use std::sync::OnceLock;

use anyhow::{bail, Context};
use windows::core::{Interface, GUID, HSTRING};
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::Variant::{VT_I8, VT_UI8};

use crate::son::{en_mono, Reechantillonneur};
use crate::{pixels, Flux, Image, Infos, Lecteur, Paquet};

const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const AUDIO: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
const TOUS: u32 = MF_SOURCE_READER_ALL_STREAMS.0 as u32;
const SOURCE: u32 = MF_SOURCE_READER_MEDIASOURCE.0 as u32;

/// COM sur ce fil, Media Foundation pour le processus. Une fois chacun ;
/// on ne referme jamais Media Foundation — il vit autant que ki-chat.
fn preparer() -> anyhow::Result<()> {
    thread_local! {
        static COM: () = unsafe {
            let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
            if hr.is_err() && hr != RPC_E_CHANGED_MODE {
                tracing::warn!("CoInitializeEx : {hr:?}");
            }
        };
    }
    COM.with(|_| {});
    static MF: OnceLock<Result<(), String>> = OnceLock::new();
    MF.get_or_init(|| {
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }.map_err(|e| e.to_string())
    })
    .clone()
    .map_err(|e| anyhow::anyhow!("Media Foundation indisponible : {e}"))
}

/// Le format d'image tel que le lecteur le livre.
struct FormatVideo {
    /// La taille **codée** du tampon (1920×1088 pour du 1080p, souvent).
    codee: (usize, usize),
    /// L'image à montrer : décalage dans le tampon codé, et dimensions.
    decalage: (usize, usize),
    largeur: usize,
    hauteur: usize,
    /// Octets par ligne annoncés par le type (0 = inconnu, on prend la
    /// largeur codée).
    pas: usize,
}

struct FormatAudio {
    cadence: u32,
    canaux: usize,
    reech: Reechantillonneur,
    /// Tampons de travail, gardés d'un paquet à l'autre.
    entrelace: Vec<f32>,
    mono: Vec<f32>,
}

pub struct LecteurMf {
    reader: IMFSourceReader,
    infos: Infos,
    video: Option<FormatVideo>,
    audio: Option<FormatAudio>,
}

pub fn ouvrir(chemin: &Path) -> anyhow::Result<Box<dyn Lecteur>> {
    preparer()?;
    if !chemin.is_file() {
        bail!("fichier introuvable : {}", chemin.display());
    }
    let url = HSTRING::from(&*chemin.to_string_lossy());
    let attributs = {
        let mut a: Option<IMFAttributes> = None;
        unsafe { MFCreateAttributes(&mut a, 2) }.context("attributs du lecteur")?;
        let a = a.context("attributs du lecteur")?;
        unsafe {
            a.SetUINT32(&MF_SOURCE_READER_DISABLE_DXVA, 1)?;
            // Le lecteur peut insérer une conversion de pixels (un 10 bits
            // vers NV12, par exemple) : sans ça, il refuserait le format.
            a.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
        }
        a
    };
    let reader = unsafe { MFCreateSourceReaderFromURL(&url, &attributs) }
        .with_context(|| format!("Media Foundation n'ouvre pas {}", chemin.display()))?;
    unsafe { reader.SetStreamSelection(TOUS, false) }.context("sélection des flux")?;

    let mut infos = Infos::default();
    let video = match choisir_video(&reader) {
        Ok(f) => {
            infos.video = true;
            infos.largeur = f.largeur as u32;
            infos.hauteur = f.hauteur as u32;
            infos.fps = cadence_images(&reader);
            Some(f)
        }
        Err(e) => {
            tracing::info!(
                "ki-media : pas d'image lisible dans {} ({e:#})",
                chemin.display()
            );
            unsafe {
                let _ = reader.SetStreamSelection(VIDEO, false);
            }
            None
        }
    };
    let audio = match choisir_audio(&reader) {
        Ok(f) => {
            infos.audio = true;
            Some(f)
        }
        Err(e) => {
            tracing::info!(
                "ki-media : pas de son lisible dans {} ({e:#})",
                chemin.display()
            );
            unsafe {
                let _ = reader.SetStreamSelection(AUDIO, false);
            }
            None
        }
    };
    if video.is_none() && audio.is_none() {
        bail!("ni image ni son lisibles dans ce fichier");
    }
    infos.duree_ms = duree_ms(&reader).unwrap_or(0);
    Ok(Box::new(LecteurMf {
        reader,
        infos,
        video,
        audio,
    }))
}

/// Sélectionne la première piste vidéo et lui demande du NV12.
fn choisir_video(reader: &IMFSourceReader) -> anyhow::Result<FormatVideo> {
    unsafe {
        reader
            .SetStreamSelection(VIDEO, true)
            .context("pas de piste vidéo")?;
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        reader
            .SetCurrentMediaType(VIDEO, None, &t)
            .context("le décodeur ne sait pas rendre du NV12")?;
    }
    format_video(reader)
}

/// Lit le format d'image courant — au départ, et quand le lecteur annonce
/// qu'il a changé (une résolution qui change en cours de fichier).
fn format_video(reader: &IMFSourceReader) -> anyhow::Result<FormatVideo> {
    let t = unsafe { reader.GetCurrentMediaType(VIDEO) }.context("format vidéo")?;
    let taille = unsafe { t.GetUINT64(&MF_MT_FRAME_SIZE) }.context("taille d'image")?;
    let codee = ((taille >> 32) as usize, (taille & 0xffff_ffff) as usize);
    if codee.0 == 0 || codee.1 == 0 || codee.0 > 8192 || codee.1 > 8192 {
        bail!("dimensions aberrantes : {}x{}", codee.0, codee.1);
    }
    // L'ouverture d'affichage : le décodeur H.264 rend des tampons de 1088
    // lignes pour une image de 1080 — les huit du bas ne sont pas l'image.
    let (mut decalage, mut largeur, mut hauteur) = ((0usize, 0usize), codee.0, codee.1);
    let mut blob = [0u8; 16];
    let mut n = 0u32;
    if unsafe { t.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, &mut blob, Some(&mut n)) }.is_ok()
        && n == 16
    {
        // MFVideoArea : deux MFOffset (fraction u16, valeur i16), puis SIZE.
        let x = i16::from_le_bytes([blob[2], blob[3]]);
        let y = i16::from_le_bytes([blob[6], blob[7]]);
        let cx = i32::from_le_bytes([blob[8], blob[9], blob[10], blob[11]]);
        let cy = i32::from_le_bytes([blob[12], blob[13], blob[14], blob[15]]);
        if x >= 0 && y >= 0 && cx > 0 && cy > 0 {
            let (x, y, cx, cy) = (x as usize, y as usize, cx as usize, cy as usize);
            if x + cx <= codee.0 && y + cy <= codee.1 {
                decalage = (x, y);
                largeur = cx;
                hauteur = cy;
            }
        }
    }
    let pas = unsafe { t.GetUINT32(&MF_MT_DEFAULT_STRIDE) }
        .ok()
        .map(|p| p as i32)
        .filter(|p| *p > 0)
        .map(|p| p as usize)
        .unwrap_or(0);
    Ok(FormatVideo {
        codee,
        decalage,
        largeur,
        hauteur,
        pas,
    })
}

fn cadence_images(reader: &IMFSourceReader) -> f32 {
    let Ok(t) = (unsafe { reader.GetCurrentMediaType(VIDEO) }) else {
        return 0.0;
    };
    match unsafe { t.GetUINT64(&MF_MT_FRAME_RATE) } {
        Ok(fr) => {
            let (num, den) = ((fr >> 32) as u32, (fr & 0xffff_ffff) as u32);
            if den == 0 {
                0.0
            } else {
                num as f32 / den as f32
            }
        }
        Err(_) => 0.0,
    }
}

/// Sélectionne la première piste audio et lui demande du float, à la
/// cadence et aux voies du fichier.
fn choisir_audio(reader: &IMFSourceReader) -> anyhow::Result<FormatAudio> {
    unsafe {
        reader
            .SetStreamSelection(AUDIO, true)
            .context("pas de piste audio")?;
        let natif = reader
            .GetNativeMediaType(AUDIO, 0)
            .context("format audio natif")?;
        let cadence = natif
            .GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)
            .unwrap_or(48_000);
        let canaux = natif
            .GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)
            .unwrap_or(2)
            .clamp(1, 8);
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float)?;
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 32)?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, cadence)?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, canaux)?;
        t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4 * canaux)?;
        t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 4 * canaux * cadence)?;
        t.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        reader
            .SetCurrentMediaType(AUDIO, None, &t)
            .context("le décodeur ne sait pas rendre du float")?;
    }
    format_audio(reader)
}

fn format_audio(reader: &IMFSourceReader) -> anyhow::Result<FormatAudio> {
    let t = unsafe { reader.GetCurrentMediaType(AUDIO) }.context("format audio")?;
    let cadence = unsafe { t.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND) }.context("cadence")?;
    let canaux = unsafe { t.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS) }.context("voies")? as usize;
    if cadence == 0 || canaux == 0 {
        bail!("format audio aberrant : {cadence} Hz, {canaux} voies");
    }
    Ok(FormatAudio {
        cadence,
        canaux,
        reech: Reechantillonneur::new(cadence),
        entrelace: Vec::new(),
        mono: Vec::new(),
    })
}

/// La durée annoncée par le conteneur, en millisecondes.
fn duree_ms(reader: &IMFSourceReader) -> Option<u64> {
    let pv = unsafe { reader.GetPresentationAttribute(SOURCE, &MF_PD_DURATION) }.ok()?;
    unsafe {
        let interne = &pv.Anonymous.Anonymous;
        (interne.vt == VT_UI8 || interne.vt == VT_I8).then(|| interne.Anonymous.uhVal / 10_000)
    }
}

impl LecteurMf {
    /// Lit le prochain échantillon du flux : `None` en fin de flux.
    fn lire(&mut self, flux: u32) -> anyhow::Result<Option<(IMFSample, i64)>> {
        loop {
            let mut drapeaux = 0u32;
            let mut horodatage = 0i64;
            let mut sample: Option<IMFSample> = None;
            unsafe {
                self.reader.ReadSample(
                    flux,
                    0,
                    None,
                    Some(&mut drapeaux),
                    Some(&mut horodatage),
                    Some(&mut sample),
                )
            }
            .context("lecture d'un échantillon")?;
            let a = |f: MF_SOURCE_READER_FLAG| drapeaux & (f.0 as u32) != 0;
            if a(MF_SOURCE_READERF_ERROR) {
                bail!("le lecteur signale une erreur sur ce flux");
            }
            if a(MF_SOURCE_READERF_ENDOFSTREAM) {
                return Ok(None);
            }
            if a(MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED) {
                if flux == VIDEO {
                    let f = format_video(&self.reader)?;
                    self.infos.largeur = f.largeur as u32;
                    self.infos.hauteur = f.hauteur as u32;
                    self.video = Some(f);
                } else {
                    self.audio = Some(format_audio(&self.reader)?);
                }
            }
            if let Some(s) = sample {
                return Ok(Some((s, horodatage)));
            }
            // Un « tick » sans données (trou dans le flux) : on continue.
        }
    }

    fn image(&mut self, sample: &IMFSample, horodatage: i64) -> anyhow::Result<Image> {
        let v = self.video.as_ref().context("format vidéo inconnu")?;
        let buffer = unsafe { sample.ConvertToContiguousBuffer() }.context("tampon d'image")?;
        let mut rgba = Vec::new();
        // Le tampon 2D donne le pas réel ; à défaut, le tampon plat et le pas
        // annoncé par le type (ou la largeur codée).
        if let Ok(b2) = buffer.cast::<IMF2DBuffer2>() {
            let mut scan0: *mut u8 = null_mut();
            let mut pas = 0i32;
            let mut debut: *mut u8 = null_mut();
            let mut longueur = 0u32;
            unsafe {
                b2.Lock2DSize(
                    MF2DBuffer_LockFlags_Read,
                    &mut scan0,
                    &mut pas,
                    &mut debut,
                    &mut longueur,
                )
            }
            .context("verrou du tampon 2D")?;
            let resultat = if pas <= 0 || scan0.is_null() {
                Err(anyhow::anyhow!("tampon d'image renversé ou vide"))
            } else {
                let octets = unsafe { std::slice::from_raw_parts(scan0, longueur as usize) };
                convertir(v, octets, pas as usize, &mut rgba)
            };
            unsafe {
                let _ = b2.Unlock2D();
            }
            resultat?;
        } else {
            let mut ptr: *mut u8 = null_mut();
            let mut longueur = 0u32;
            unsafe { buffer.Lock(&mut ptr, None, Some(&mut longueur)) }
                .context("verrou du tampon")?;
            let resultat = if ptr.is_null() {
                Err(anyhow::anyhow!("tampon d'image vide"))
            } else {
                let octets = unsafe { std::slice::from_raw_parts(ptr, longueur as usize) };
                let pas = if v.pas > 0 { v.pas } else { v.codee.0 };
                convertir(v, octets, pas, &mut rgba)
            };
            unsafe {
                let _ = buffer.Unlock();
            }
            resultat?;
        }
        Ok(Image {
            pts_ms: horodatage.max(0) as u64 / 10_000,
            largeur: v.largeur as u32,
            hauteur: v.hauteur as u32,
            rgba,
        })
    }

    fn son(&mut self, sample: &IMFSample, horodatage: i64) -> anyhow::Result<Paquet> {
        let a = self.audio.as_mut().context("format audio inconnu")?;
        let buffer = unsafe { sample.ConvertToContiguousBuffer() }.context("tampon de son")?;
        let mut ptr: *mut u8 = null_mut();
        let mut longueur = 0u32;
        unsafe { buffer.Lock(&mut ptr, None, Some(&mut longueur)) }.context("verrou du tampon")?;
        a.entrelace.clear();
        if !ptr.is_null() {
            let octets = unsafe { std::slice::from_raw_parts(ptr, longueur as usize) };
            // Copie par octets : l'alignement d'un tampon COM n'est pas garanti.
            a.entrelace.extend(
                octets
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| f32::from_le_bytes(*c)),
            );
        }
        unsafe {
            let _ = buffer.Unlock();
        }
        en_mono(&a.entrelace, a.canaux, &mut a.mono);
        let mut mono = Vec::with_capacity(a.mono.len() * 48_000 / a.cadence as usize + 8);
        a.reech.pousser(&a.mono, &mut mono);
        Ok(Paquet::Audio {
            pts_ms: horodatage.max(0) as u64 / 10_000,
            mono,
        })
    }
}

/// Découpe les deux plans NV12 dans le tampon et convertit l'image utile.
fn convertir(v: &FormatVideo, octets: &[u8], pas: usize, rgba: &mut Vec<u8>) -> anyhow::Result<()> {
    let (lc, hc) = v.codee;
    let hauteur_uv = hc.div_ceil(2);
    if pas < lc || octets.len() < pas * (hc + hauteur_uv) {
        bail!(
            "tampon d'image trop court : {} octets pour {lc}x{hc} au pas {pas}",
            octets.len()
        );
    }
    let (dx, dy) = v.decalage;
    let plan_y = &octets[dy * pas + dx..pas * hc];
    let plan_uv = &octets[pas * hc + (dy / 2) * pas + (dx & !1)..];
    pixels::nv12_vers_rgba(plan_y, pas, plan_uv, pas, v.largeur, v.hauteur, rgba);
    Ok(())
}

impl Lecteur for LecteurMf {
    fn infos(&self) -> &Infos {
        &self.infos
    }

    fn chercher(&mut self, ms: u64) -> anyhow::Result<()> {
        let position = PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_I8,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: PROPVARIANT_0_0_0 {
                        hVal: (ms.min(i64::MAX as u64 / 10_000) * 10_000) as i64,
                    },
                }),
            },
        };
        unsafe {
            self.reader.Flush(TOUS).context("vidange avant recherche")?;
            self.reader
                .SetCurrentPosition(&GUID::zeroed(), &position)
                .context("recherche dans le fichier")?;
        }
        if let Some(a) = self.audio.as_mut() {
            a.reech.reinitialiser();
        }
        Ok(())
    }

    fn suivant(&mut self, flux: Flux) -> anyhow::Result<Paquet> {
        match flux {
            Flux::Video => {
                if self.video.is_none() {
                    return Ok(Paquet::Fin);
                }
                match self.lire(VIDEO)? {
                    Some((s, ts)) => Ok(Paquet::Image(self.image(&s, ts)?)),
                    None => Ok(Paquet::Fin),
                }
            }
            Flux::Audio => {
                if self.audio.is_none() {
                    return Ok(Paquet::Fin);
                }
                match self.lire(AUDIO)? {
                    Some((s, ts)) => self.son(&s, ts),
                    None => Ok(Paquet::Fin),
                }
            }
        }
    }
}
