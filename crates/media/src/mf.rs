//! Media Foundation : le *Source Reader* démultiplexe et décode.
//!
//! On lui demande du NV12 pour l'image et du float entrelacé pour le son, à
//! la cadence et au nombre de voies **natifs** du fichier : le lecteur sait
//! décoder et convertir les formats de pixels, pas rééchantillonner — ça, on
//! le fait nous-mêmes (`son.rs`). Décodage logiciel (DXVA désactivé) : le
//! décodeur de Microsoft est rapide et n'a pas d'humeur de pilote ; le
//! matériel viendra si un portable en a besoin.
//!
//! Les flux sont choisis par leur numéro, pas par le raccourci « le premier
//! flux audio » de Media Foundation : sur un MP4 à plusieurs pistes son (un
//! clip : le mélange, puis le jeu, le micro, les copains), ce raccourci
//! tombait sur la **dernière** piste du fichier — et un clip s'ouvrait muet.
//! Voir `flux_disponibles`.
//!
//! L'aperçu de l'atelier, lui, mêle plusieurs pistes d'un clip (le jeu, le
//! micro, les copains) à des gains qu'on règle en cours de lecture : chaque
//! piste mêlée a son flux, son format et ce qu'elle a décodé d'avance (voir
//! `son_mele`).
//!
//! Tout ce qui parle à COM vit ici, derrière le trait `Lecteur`.

use std::collections::VecDeque;
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

use crate::pixels::Matrice;
use crate::son::{en_mono, Reechantillonneur};
use crate::{pixels, Flux, Image, Infos, Lecteur, Paquet};

const TOUS: u32 = MF_SOURCE_READER_ALL_STREAMS.0 as u32;
const SOURCE: u32 = MF_SOURCE_READER_MEDIASOURCE.0 as u32;

/// COM sur ce fil, Media Foundation pour le processus. Une fois chacun ;
/// on ne referme jamais Media Foundation — il vit autant que ki-chat.
pub(crate) fn preparer() -> anyhow::Result<()> {
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
#[derive(Clone, Debug)]
pub(crate) struct FormatVideo {
    /// La taille **codée** du tampon (1920×1088 pour du 1080p, souvent).
    codee: (usize, usize),
    /// L'image à montrer : décalage dans le tampon codé, et dimensions.
    decalage: (usize, usize),
    pub(crate) largeur: usize,
    pub(crate) hauteur: usize,
    /// Octets par ligne annoncés par le type (0 = inconnu, on prend la
    /// largeur codée).
    pas: usize,
    /// Pour un fichier, selon la hauteur ; le décodeur de trames la fixe.
    pub(crate) matrice: Matrice,
}

struct FormatAudio {
    cadence: u32,
    canaux: usize,
    reech: Reechantillonneur,
    /// Tampons de travail, gardés d'un paquet à l'autre.
    entrelace: Vec<f32>,
    mono: Vec<f32>,
}

/// Une piste son mêlée aux autres : les pistes se décodent chacune à son
/// rythme, le mélange prend ce que toutes ont.
struct PisteMelee {
    flux: u32,
    format: FormatAudio,
    /// Décodé d'avance, mono 48 kHz.
    attente: VecDeque<f32>,
    /// L'instant (ms) du paquet qui a commencé l'attente, et ce qui en a été
    /// pris depuis : l'instant du premier échantillon en attente, sans
    /// arrondi qui s'accumule.
    debut_ms: u64,
    pris: u64,
    fin: bool,
    gain: f32,
}

impl PisteMelee {
    fn instant_ms(&self) -> u64 {
        self.debut_ms + self.pris * 1000 / u64::from(crate::CADENCE)
    }
}

pub struct LecteurMf {
    reader: IMFSourceReader,
    infos: Infos,
    video: Option<FormatVideo>,
    audio: Option<FormatAudio>,
    /// Les numéros de flux retenus (`u32::MAX` : pas de tel flux).
    flux_video: u32,
    flux_audio: u32,
    /// Les pistes mêlées (`ouvrir_avec_pistes`) ; vide : la seule première
    /// piste son, `audio`.
    melange: Vec<PisteMelee>,
}

/// Ouvre le fichier avec le lecteur, sans encore choisir de flux.
fn lecteur_brut(chemin: &Path) -> anyhow::Result<IMFSourceReader> {
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
    Ok(reader)
}

/// Ouvre `chemin` : sa première piste son, ou — `rangs` non vide — ces
/// pistes son-là, mêlées (rangs dans l'ordre du fichier).
pub fn ouvrir(chemin: &Path, rangs: &[usize]) -> anyhow::Result<Box<dyn Lecteur>> {
    let reader = lecteur_brut(chemin)?;
    let (iv, ia) = flux_disponibles(&reader);
    let mut infos = Infos::default();
    let video = match iv.map(|i| choisir_video(&reader, i)) {
        Some(Ok(f)) => {
            infos.video = true;
            infos.largeur = f.largeur as u32;
            infos.hauteur = f.hauteur as u32;
            infos.fps = cadence_images(&reader, iv.unwrap_or(0));
            Some(f)
        }
        Some(Err(e)) => {
            tracing::info!("ki-media : pas d'image lisible dans {} ({e:#})", chemin.display());
            if let Some(i) = iv {
                unsafe {
                    let _ = reader.SetStreamSelection(i, false);
                }
            }
            None
        }
        None => None,
    };
    let mut melange = Vec::new();
    let audio = if rangs.is_empty() {
        match ia.map(|i| choisir_audio(&reader, i)) {
            Some(Ok(f)) => Some(f),
            Some(Err(e)) => {
                tracing::info!("ki-media : pas de son lisible dans {} ({e:#})", chemin.display());
                if let Some(i) = ia {
                    unsafe {
                        let _ = reader.SetStreamSelection(i, false);
                    }
                }
                None
            }
            None => None,
        }
    } else {
        let pistes = pistes_son(&reader);
        for &rang in rangs {
            let flux = *pistes
                .get(rang)
                .with_context(|| format!("pas de piste son n° {rang} ({} dans le fichier)", pistes.len()))?;
            melange.push(PisteMelee {
                flux,
                format: choisir_audio(&reader, flux)?,
                attente: VecDeque::new(),
                debut_ms: 0,
                pris: 0,
                fin: false,
                gain: 1.0,
            });
        }
        None
    };
    infos.audio = audio.is_some() || !melange.is_empty();
    if video.is_none() && !infos.audio {
        bail!("ni image ni son lisibles dans ce fichier");
    }
    infos.duree_ms = duree_ms(&reader).unwrap_or(0);
    Ok(Box::new(LecteurMf {
        reader,
        infos,
        flux_video: if video.is_some() { iv.unwrap_or(u32::MAX) } else { u32::MAX },
        flux_audio: if audio.is_some() { ia.unwrap_or(u32::MAX) } else { u32::MAX },
        video,
        audio,
        melange,
    }))
}

/// Les flux du fichier, dans l'ordre du lecteur : (numéro, type majeur).
fn enumerer(reader: &IMFSourceReader) -> Vec<(u32, GUID)> {
    let mut flux = Vec::new();
    for index in 0..32u32 {
        let Ok(t) = (unsafe { reader.GetNativeMediaType(index, 0) }) else { break };
        if let Ok(majeur) = unsafe { t.GetGUID(&MF_MT_MAJOR_TYPE) } {
            flux.push((index, majeur));
        }
    }
    flux
}

/// Le premier flux vidéo et le premier flux audio **du fichier**.
///
/// La source MPEG-4 de Media Foundation énumère les pistes à l'envers de
/// leur ordre dans le fichier (**vérifié** sur un MP4 de ffmpeg comme sur un
/// clip écrit par le Sink Writer : la première piste du fichier est le
/// dernier flux du lecteur). Les lecteurs ordinaires jouent la première
/// piste audio du fichier ; pour jouer la même, on prend le dernier flux de
/// chaque type.
fn flux_disponibles(reader: &IMFSourceReader) -> (Option<u32>, Option<u32>) {
    let flux = enumerer(reader);
    let dernier = |majeur: GUID| flux.iter().rev().find(|(_, m)| *m == majeur).map(|(i, _)| *i);
    (dernier(MFMediaType_Video), dernier(MFMediaType_Audio))
}

/// Les flux son dans l'ordre des pistes **du fichier** — le lecteur les
/// énumère à l'envers (voir `flux_disponibles`). Vérifié sur un clip : le
/// premier est le mélange, puis le jeu, le micro, les copains.
fn pistes_son(reader: &IMFSourceReader) -> Vec<u32> {
    enumerer(reader)
        .into_iter()
        .rev()
        .filter(|(_, m)| *m == MFMediaType_Audio)
        .map(|(i, _)| i)
        .collect()
}

/// Sélectionne la piste vidéo `index` et lui demande du NV12.
fn choisir_video(reader: &IMFSourceReader, index: u32) -> anyhow::Result<FormatVideo> {
    unsafe {
        reader.SetStreamSelection(index, true).context("pas de piste vidéo")?;
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        reader
            .SetCurrentMediaType(index, None, &t)
            .context("le décodeur ne sait pas rendre du NV12")?;
    }
    format_video(reader, index)
}

/// Lit le format d'image courant — au départ, et quand le lecteur annonce
/// qu'il a changé (une résolution qui change en cours de fichier).
fn format_video(reader: &IMFSourceReader, index: u32) -> anyhow::Result<FormatVideo> {
    let t = unsafe { reader.GetCurrentMediaType(index) }.context("format vidéo")?;
    format_du_type(&t)
}

/// Le format d'image d'un type NV12 : taille codée, ouverture d'affichage,
/// pas — que le type vienne d'un fichier ou du décodeur de trames.
pub(crate) fn format_du_type(t: &IMFMediaType) -> anyhow::Result<FormatVideo> {
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
    Ok(FormatVideo { codee, decalage, largeur, hauteur, pas, matrice: Matrice::selon_hauteur(hauteur) })
}

fn cadence_images(reader: &IMFSourceReader, index: u32) -> f32 {
    let Ok(t) = (unsafe { reader.GetCurrentMediaType(index) }) else { return 0.0 };
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

/// Sélectionne la piste audio `index` et lui demande du float, à la
/// cadence et aux voies du fichier.
fn choisir_audio(reader: &IMFSourceReader, index: u32) -> anyhow::Result<FormatAudio> {
    unsafe {
        reader.SetStreamSelection(index, true).context("pas de piste audio")?;
        let natif = reader.GetNativeMediaType(index, 0).context("format audio natif")?;
        let cadence = natif.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap_or(48_000);
        let canaux = natif.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(2).clamp(1, 8);
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
            .SetCurrentMediaType(index, None, &t)
            .context("le décodeur ne sait pas rendre du float")?;
    }
    format_audio(reader, index)
}

fn format_audio(reader: &IMFSourceReader, index: u32) -> anyhow::Result<FormatAudio> {
    let t = unsafe { reader.GetCurrentMediaType(index) }.context("format audio")?;
    let cadence = unsafe { t.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND) }.context("cadence")?;
    let canaux = unsafe { t.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS) }.context("voies")? as usize;
    // Bornée à ce qu'un fichier audio réel annonce (8 à 384 kHz) : le
    // rééchantillonneur tient au-delà, mais une cadence farfelue ne dit
    // qu'une chose, que le fichier ment.
    if !(8_000..=384_000).contains(&cadence) || canaux == 0 {
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
                if flux == self.flux_video {
                    let f = format_video(&self.reader, flux)?;
                    self.infos.largeur = f.largeur as u32;
                    self.infos.hauteur = f.hauteur as u32;
                    self.video = Some(f);
                } else {
                    let f = format_audio(&self.reader, flux)?;
                    match self.melange.iter_mut().find(|p| p.flux == flux) {
                        Some(p) => p.format = f,
                        None => self.audio = Some(f),
                    }
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
        image_nv12(v, sample, horodatage)
    }

    fn son(&mut self, sample: &IMFSample, horodatage: i64) -> anyhow::Result<Paquet> {
        let a = self.audio.as_mut().context("format audio inconnu")?;
        Ok(Paquet::Audio { pts_ms: horodatage.max(0) as u64 / 10_000, mono: decoder_son(a, sample)? })
    }

    /// Le son des pistes mêlées : chacune décode d'avance au moins un bloc,
    /// le mélange prend ce que toutes ont — une piste finie ne retient rien
    /// et compte pour du silence —, chacune à son gain.
    fn son_mele(&mut self) -> anyhow::Result<Paquet> {
        const BLOC: usize = 1024;
        for k in 0..self.melange.len() {
            while !self.melange[k].fin && self.melange[k].attente.len() < BLOC {
                let flux = self.melange[k].flux;
                match self.lire(flux)? {
                    Some((s, ts)) => {
                        let p = &mut self.melange[k];
                        let mono = decoder_son(&mut p.format, &s)?;
                        if p.attente.is_empty() {
                            p.debut_ms = ts.max(0) as u64 / 10_000;
                            p.pris = 0;
                        }
                        p.attente.extend(mono);
                    }
                    None => self.melange[k].fin = true,
                }
            }
        }
        // Une piste pas finie a au moins un bloc : n n'est nul que si toutes
        // sont finies et vidées.
        let n = self
            .melange
            .iter()
            .filter(|p| !p.fin)
            .map(|p| p.attente.len())
            .min()
            .unwrap_or_else(|| self.melange.iter().map(|p| p.attente.len()).max().unwrap_or(0));
        if n == 0 {
            return Ok(Paquet::Fin);
        }
        let pts_ms = self.melange.iter().find(|p| !p.attente.is_empty()).map_or(0, PisteMelee::instant_ms);
        let mut mono = vec![0.0f32; n];
        for p in &mut self.melange {
            let k = n.min(p.attente.len());
            let gain = p.gain;
            for (m, v) in mono.iter_mut().zip(p.attente.drain(..k)) {
                *m += gain * v;
            }
            p.pris += k as u64;
        }
        Ok(Paquet::Audio { pts_ms, mono })
    }
}

/// Un paquet de son décodé (float entrelacé, au format de `a`) rendu en
/// mono 48 kHz.
fn decoder_son(a: &mut FormatAudio, sample: &IMFSample) -> anyhow::Result<Vec<f32>> {
    let buffer = unsafe { sample.ConvertToContiguousBuffer() }.context("tampon de son")?;
    let mut ptr: *mut u8 = null_mut();
    let mut longueur = 0u32;
    unsafe { buffer.Lock(&mut ptr, None, Some(&mut longueur)) }.context("verrou du tampon")?;
    a.entrelace.clear();
    if !ptr.is_null() {
        let octets = unsafe { std::slice::from_raw_parts(ptr, longueur as usize) };
        // Copie par octets : l'alignement d'un tampon COM n'est pas garanti.
        a.entrelace.extend(octets.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)));
    }
    unsafe {
        let _ = buffer.Unlock();
    }
    en_mono(&a.entrelace, a.canaux, &mut a.mono);
    let mut mono = Vec::with_capacity(a.mono.len() * 48_000 / a.cadence as usize + 8);
    a.reech.pousser(&a.mono, &mut mono);
    Ok(mono)
}


/// Une image NV12 d'un échantillon Media Foundation, convertie en RGBA :
/// le pas réel par le tampon 2D, sinon celui du type.
pub(crate) fn image_nv12(v: &FormatVideo, sample: &IMFSample, horodatage: i64) -> anyhow::Result<Image> {
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
        // La longueur se compte depuis le début du tampon, et la première
        // ligne peut commencer plus loin : ce qui est lisible à partir
        // d'elle, c'est la longueur moins ce décalage. Prendre la
        // longueur entière depuis la première ligne lisait au-delà du
        // tampon.
        let lisible = (scan0 as usize)
            .checked_sub(debut as usize)
            .and_then(|decalage| (longueur as usize).checked_sub(decalage));
        let resultat = match lisible {
            Some(lisible) if pas > 0 && !scan0.is_null() && !debut.is_null() => {
                let octets = unsafe { std::slice::from_raw_parts(scan0, lisible) };
                convertir(v, octets, pas as usize, v.codee.1, &mut rgba)
            }
            _ => Err(anyhow::anyhow!("tampon d'image renversé, vide ou incohérent")),
        };
        unsafe {
            let _ = b2.Unlock2D();
        }
        resultat?;
    } else {
        let mut ptr: *mut u8 = null_mut();
        let mut longueur = 0u32;
        unsafe { buffer.Lock(&mut ptr, None, Some(&mut longueur)) }.context("verrou du tampon")?;
        let resultat = if ptr.is_null() {
            Err(anyhow::anyhow!("tampon d'image vide"))
        } else {
            let octets = unsafe { std::slice::from_raw_parts(ptr, longueur as usize) };
            let pas = if v.pas > 0 { v.pas } else { v.codee.0 };
            convertir(v, octets, pas, v.codee.1, &mut rgba)
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

/// Découpe les deux plans NV12 dans le tampon et convertit l'image utile.
/// `lignes_y` : les lignes du plan de luminance, après lesquelles commence
/// celui de chrominance — la hauteur codée, ou plus pour une texture que la
/// carte a allouée plus haute.
pub(crate) fn convertir(
    v: &FormatVideo,
    octets: &[u8],
    pas: usize,
    lignes_y: usize,
    rgba: &mut Vec<u8>,
) -> anyhow::Result<()> {
    let (lc, hc) = v.codee;
    let hauteur_uv = hc.div_ceil(2);
    if pas < lc || lignes_y < hc || octets.len() < pas * (lignes_y + hauteur_uv) {
        bail!(
            "tampon d'image trop court : {} octets pour {lc}x{hc} au pas {pas}",
            octets.len()
        );
    }
    let (dx, dy) = v.decalage;
    let plan_y = &octets[dy * pas + dx..pas * lignes_y];
    let plan_uv = &octets[pas * lignes_y + (dy / 2) * pas + (dx & !1)..];
    pixels::nv12_vers_rgba(plan_y, pas, plan_uv, pas, v.largeur, v.hauteur, v.matrice, rgba);
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
                    Anonymous: PROPVARIANT_0_0_0 { hVal: (ms.min(i64::MAX as u64 / 10_000) * 10_000) as i64 },
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
        for p in &mut self.melange {
            p.attente.clear();
            p.fin = false;
            p.format.reech.reinitialiser();
        }
        Ok(())
    }

    fn suivant(&mut self, flux: Flux) -> anyhow::Result<Paquet> {
        match flux {
            Flux::Video => {
                if self.video.is_none() {
                    return Ok(Paquet::Fin);
                }
                match self.lire(self.flux_video)? {
                    Some((s, ts)) => Ok(Paquet::Image(self.image(&s, ts)?)),
                    None => Ok(Paquet::Fin),
                }
            }
            Flux::Audio => {
                if !self.melange.is_empty() {
                    return self.son_mele();
                }
                if self.audio.is_none() {
                    return Ok(Paquet::Fin);
                }
                match self.lire(self.flux_audio)? {
                    Some((s, ts)) => self.son(&s, ts),
                    None => Ok(Paquet::Fin),
                }
            }
        }
    }

    fn regler_gains(&mut self, gains: &[f32]) {
        for (p, g) in self.melange.iter_mut().zip(gains) {
            p.gain = if g.is_finite() { g.clamp(0.0, 4.0) } else { 0.0 };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sonde manuelle : `KI_MEDIA_FICHIER=chemin cargo test -p ki-media
    /// sonde_flux -- --ignored --nocapture` liste les flux et l'énergie de
    /// chaque piste audio — pour comprendre l'ordre que le lecteur donne.
    #[test]
    #[ignore]
    fn sonde_flux() {
        let Some(chemin) = std::env::var_os("KI_MEDIA_FICHIER") else { return };
        let reader = lecteur_brut(Path::new(&chemin)).expect("ouverture");
        for (index, majeur) in enumerer(&reader) {
            // Un lecteur neuf par flux : lire un flux fait avancer la source
            // et jette ce que les autres, non sélectionnés, auraient rendu.
            let reader = lecteur_brut(Path::new(&chemin)).expect("ouverture");
            let genre = if majeur == MFMediaType_Video {
                "vidéo"
            } else if majeur == MFMediaType_Audio {
                "audio"
            } else {
                "autre"
            };
            let mut energie = 0.0f32;
            let mut n = 0usize;
            if genre == "audio" {
                let f = choisir_audio(&reader, index).expect("format audio");
                let mut l = LecteurMf {
                    reader: reader.clone(),
                    infos: Infos::default(),
                    video: None,
                    audio: Some(f),
                    flux_video: u32::MAX,
                    flux_audio: index,
                    melange: Vec::new(),
                };
                for _ in 0..50 {
                    match l.suivant(Flux::Audio) {
                        Ok(Paquet::Audio { mono, .. }) => {
                            n += mono.len();
                            energie += mono.iter().map(|v| v * v).sum::<f32>();
                        }
                        _ => break,
                    }
                }
                unsafe {
                    let _ = reader.SetStreamSelection(index, false);
                }
            }
            eprintln!("flux {index} : {genre} — {n} échantillons, énergie {energie}");
        }
    }
}
