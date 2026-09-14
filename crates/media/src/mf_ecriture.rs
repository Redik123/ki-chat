//! Media Foundation : le *Sink Writer* écrit le MP4 d'un clip.
//!
//! Le H.264 entre **tel quel** : le type d'entrée du flux vidéo est son type
//! de sortie, le writer n'insère donc aucun encodeur et se contente de ranger
//! les unités d'accès dans le conteneur (SPS et PPS lus dans le flux — NVENC
//! et openh264 les répètent à chaque trame clé). Le son entre en PCM 16 bits
//! et sort en AAC : c'est le writer qui encode, avec l'encodeur AAC de
//! Windows, présent partout depuis Windows 7.
//!
//! Plusieurs pistes audio possibles : les lecteurs ordinaires jouent la
//! première, l'atelier se sert des autres.

use std::path::Path;
use std::ptr::null_mut;

use anyhow::{bail, Context};
use windows::core::HSTRING;
use windows::Win32::Media::MediaFoundation::*;

use crate::mf::preparer;
use crate::{EcrivainInterne, FormatVideo};

/// 160 kbit/s par piste AAC, en octets par seconde — une des valeurs que
/// l'encodeur de Windows accepte (12 000, 16 000, 20 000, 24 000).
const AAC_OCTETS_PAR_S: u32 = 20_000;

pub struct EcrivainMf {
    writer: IMFSinkWriter,
    video: u32,
    audio: Vec<u32>,
    /// Le writer exige des horodatages qui montent : on retient le dernier
    /// écrit par flux et l'on refuse ce qui reculerait.
    dernier_video: i64,
    derniers_audio: Vec<i64>,
    termine: bool,
}

pub fn ouvrir(chemin: &Path, format: &FormatVideo, pistes_audio: usize) -> anyhow::Result<Box<dyn EcrivainInterne>> {
    preparer()?;
    if format.largeur == 0 || format.hauteur == 0 || format.fps == 0 {
        bail!("format vidéo incomplet");
    }
    let url = HSTRING::from(&*chemin.to_string_lossy());
    let attributs = {
        let mut a: Option<IMFAttributes> = None;
        unsafe { MFCreateAttributes(&mut a, 1) }.context("attributs du writer")?;
        let a = a.context("attributs du writer")?;
        // Pas de régulation : on écrit un fichier, pas une diffusion en
        // direct ; le writer ne doit jamais nous faire attendre.
        unsafe { a.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)? };
        a
    };
    let writer = unsafe { MFCreateSinkWriterFromURL(&url, None, &attributs) }
        .with_context(|| format!("Media Foundation ne crée pas {}", chemin.display()))?;

    // La vidéo : H.264 en entrée comme en sortie.
    let video = unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(format.largeur) << 32) | u64::from(format.hauteur))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(format.fps) << 32) | 1)?;
        t.SetUINT32(&MF_MT_AVG_BITRATE, format.debit_bps.max(1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        if let Some(p) = &format.parametres {
            t.SetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, p)?;
        }
        let index = writer.AddStream(&t).context("flux vidéo")?;
        writer
            .SetInputMediaType(index, &t, None)
            .context("le writer refuse le H.264 tel quel")?;
        index
    };

    // Les pistes audio : PCM 16 bits en entrée, AAC en sortie.
    let mut audio = Vec::with_capacity(pistes_audio);
    for _ in 0..pistes_audio {
        let index = unsafe {
            let sortie = MFCreateMediaType()?;
            sortie.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            sortie.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
            sortie.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            sortie.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, crate::CADENCE)?;
            sortie.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
            sortie.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AAC_OCTETS_PAR_S)?;
            sortie.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?;
            // AAC-LC, niveau 2 : ce que tout téléphone lit.
            sortie.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29)?;
            let index = writer.AddStream(&sortie).context("flux audio")?;
            let entree = MFCreateMediaType()?;
            entree.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            entree.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
            entree.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            entree.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, crate::CADENCE)?;
            entree.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
            entree.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
            entree.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 4 * crate::CADENCE)?;
            entree.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
            writer
                .SetInputMediaType(index, &entree, None)
                .context("l'encodeur AAC refuse le PCM 48 kHz stéréo")?;
            index
        };
        audio.push(index);
    }
    unsafe { writer.BeginWriting() }.context("début d'écriture")?;
    Ok(Box::new(EcrivainMf {
        writer,
        video,
        derniers_audio: vec![-1; audio.len()],
        audio,
        dernier_video: -1,
        termine: false,
    }))
}

/// Un échantillon Media Foundation à partir d'octets et d'horodatages en
/// centaines de nanosecondes.
fn echantillon(octets: &[u8], temps: i64, duree: i64) -> anyhow::Result<IMFSample> {
    unsafe {
        let tampon = MFCreateMemoryBuffer(octets.len().max(1) as u32)?;
        let mut ptr: *mut u8 = null_mut();
        tampon.Lock(&mut ptr, None, None)?;
        if !ptr.is_null() {
            std::ptr::copy_nonoverlapping(octets.as_ptr(), ptr, octets.len());
        }
        tampon.Unlock()?;
        tampon.SetCurrentLength(octets.len() as u32)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&tampon)?;
        sample.SetSampleTime(temps)?;
        sample.SetSampleDuration(duree.max(1))?;
        Ok(sample)
    }
}

impl EcrivainInterne for EcrivainMf {
    fn image(&mut self, annexb: &[u8], pts_us: u64, duree_us: u64, idr: bool) -> anyhow::Result<()> {
        if self.termine {
            bail!("fichier déjà terminé");
        }
        let temps = (pts_us as i64) * 10;
        if temps <= self.dernier_video && self.dernier_video >= 0 {
            // Une image qui n'avance pas : jetée, le writer la refuserait.
            return Ok(());
        }
        let sample = echantillon(annexb, temps, (duree_us as i64) * 10)?;
        if idr {
            unsafe { sample.SetUINT32(&MFSampleExtension_CleanPoint, 1)? };
        }
        unsafe { self.writer.WriteSample(self.video, &sample) }.context("écriture d'une image")?;
        self.dernier_video = temps;
        Ok(())
    }

    fn son(&mut self, piste: usize, stereo: &[f32], pts_us: u64) -> anyhow::Result<()> {
        if self.termine {
            bail!("fichier déjà terminé");
        }
        let Some(&index) = self.audio.get(piste) else { bail!("piste audio {piste} inconnue") };
        if stereo.len() < 2 {
            return Ok(());
        }
        let temps = (pts_us as i64) * 10;
        if temps < self.derniers_audio[piste] {
            return Ok(());
        }
        let mut octets = Vec::with_capacity(stereo.len() * 2);
        for s in stereo {
            let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            octets.extend_from_slice(&v.to_le_bytes());
        }
        let trames = (stereo.len() / 2) as i64;
        let duree = trames * 10_000_000 / i64::from(crate::CADENCE);
        let sample = echantillon(&octets, temps, duree)?;
        unsafe { self.writer.WriteSample(index, &sample) }.context("écriture du son")?;
        self.derniers_audio[piste] = temps + duree;
        Ok(())
    }

    fn terminer(&mut self) -> anyhow::Result<()> {
        if self.termine {
            return Ok(());
        }
        self.termine = true;
        unsafe { self.writer.Finalize() }.context("finalisation du fichier")
    }
}
