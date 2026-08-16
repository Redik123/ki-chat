//! Effets sonores (notifications) : chargement et mise en file.
//!
//! WAV uniquement, via `hound` : pur Rust, déjà présent dans l'arbre de
//! dépendances (tiré par nnnoiseless), donc le déclarer ici ne coûte ni
//! téléchargement ni compilation. Un décodeur MP3 ferait entrer un gros
//! morceau de code pour des sons de deux secondes.
//!
//! Le moteur ne travaille qu'en mono 48 kHz : tout est converti ici, une
//! fois au chargement, pour que le chemin temps réel n'ait rien à décider.

use std::collections::VecDeque;
use std::path::Path;

use anyhow::Context;

use crate::SAMPLE_RATE;

/// Plafond de la file d'effets, en échantillons (~3 s). Sans lui, une
/// rafale d'événements (dix arrivées d'affilée) empilerait des minutes de
/// notifications que l'utilisateur entendrait défiler longtemps après.
pub(crate) const QUEUE_CAP: usize = SAMPLE_RATE as usize * 3;

/// Ajoute un effet à la file en **s'ajoutant** aux échantillons déjà en
/// attente : deux notifications rapprochées se superposent au lieu que la
/// seconde coupe la première. Au-delà du plafond, l'effet est tronqué.
pub(crate) fn queue(buf: &mut VecDeque<f32>, pcm: &[f32], gain: f32) {
    let n = pcm.len().min(QUEUE_CAP);
    if buf.len() < n {
        buf.resize(n, 0.0);
    }
    for (dst, &src) in buf.iter_mut().zip(pcm[..n].iter()) {
        *dst += src * gain;
    }
}

/// Décode un WAV en mémoire vers du PCM mono 48 kHz.
pub fn load_wav(bytes: &[u8]) -> anyhow::Result<Vec<f32>> {
    let reader = hound::WavReader::new(std::io::Cursor::new(bytes)).context("en-tête WAV")?;
    decode(reader)
}

/// Idem depuis un fichier.
pub fn load_wav_file(path: impl AsRef<Path>) -> anyhow::Result<Vec<f32>> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).with_context(|| format!("lecture de {}", path.display()))?;
    load_wav(&bytes).with_context(|| format!("décodage de {}", path.display()))
}

/// Charge tous les WAV d'un dossier : rend (nom sans extension, PCM 48 kHz),
/// trié par nom. À l'appelant de décider quel son correspond à quel
/// événement — le moteur ne connaît aucun événement.
///
/// Un dossier absent rend une liste vide (aucun son installé, ce n'est pas
/// une erreur) et un fichier illisible est ignoré : une notification n'est
/// jamais assez importante pour empêcher le démarrage du client.
pub fn load_dir(dir: impl AsRef<Path>) -> Vec<(String, Vec<f32>)> {
    let dir = dir.as_ref();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!("aucun effet sonore dans {} : {e}", dir.display());
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav")) {
            continue;
        }
        let Some(name) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        match load_wav_file(&path) {
            Ok(pcm) => out.push((name, pcm)),
            Err(e) => tracing::warn!("effet sonore ignoré : {e:#}"),
        }
    }
    // Ordre stable : `read_dir` suit le système de fichiers, l'interface a
    // besoin d'une liste qui ne change pas d'un démarrage à l'autre.
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn decode<R: std::io::Read>(mut reader: hound::WavReader<R>) -> anyhow::Result<Vec<f32>> {
    let spec = reader.spec();
    anyhow::ensure!(spec.sample_rate > 0, "fréquence WAV nulle");
    let channels = (spec.channels as usize).max(1);

    // hound rend les entiers bruts : les ramener en -1..1 demande la
    // profondeur réelle, car le 24 bits est livré dans un i32.
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .context("échantillons WAV flottants")?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample.clamp(2, 32) - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()
                .context("échantillons WAV entiers")?
        }
    };

    let mono = crate::to_mono(&samples, channels);
    Ok(resample(&mono, spec.sample_rate, SAMPLE_RATE))
}

/// Rééchantillonnage linéaire, volontairement local et sans état.
///
/// Le rééchantillonneur du moteur est fait pour un flux continu (position
/// fractionnaire conservée entre les blocs) ; ici on convertit un son
/// complet, une seule fois, hors du chemin temps réel. Pour une
/// notification de deux secondes, l'interpolation linéaire est inaudible —
/// dépendre du module de flux n'apporterait rien et le couplerait à un
/// usage qui n'est pas le sien.
fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.len() < 2 {
        return input.to_vec();
    }
    // Longueur calculée en entier : c'est elle qui garantit la durée exacte
    // du son (un calcul en flottant perd ou ajoute un échantillon).
    let n = (input.len() as u64 * to as u64 / from as u64) as usize;
    let step = from as f64 / to as f64;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let pos = i as f64 * step;
        let idx = pos as usize;
        let frac = (pos - idx as f64) as f32;
        let a = input.get(idx).copied().unwrap_or(0.0);
        let b = input.get(idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fabrique un WAV en mémoire (hound sait écrire dans un Cursor).
    fn make_wav(rate: u32, channels: u16, frames: usize) -> Vec<u8> {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut w = hound::WavWriter::new(&mut cursor, spec).unwrap();
            for i in 0..frames {
                for _ in 0..channels {
                    let v = ((i as f32 * 0.05).sin() * 8000.0) as i16;
                    w.write_sample(v).unwrap();
                }
            }
            w.finalize().unwrap();
        }
        cursor.into_inner()
    }

    #[test]
    fn wav_is_decoded_mono_at_48k() {
        // 44,1 kHz stéréo, 1 s -> 48 000 échantillons mono.
        let bytes = make_wav(44_100, 2, 44_100);
        let pcm = load_wav(&bytes).expect("décodage WAV");
        assert_eq!(pcm.len(), 48_000, "durée non conservée");
        assert!(pcm.iter().all(|s| s.abs() <= 1.0), "échelle hors -1..1");
        assert!(pcm.iter().any(|s| s.abs() > 0.1), "signal perdu");

        // Déjà à 48 kHz mono : passe-plat, échantillon pour échantillon.
        let bytes = make_wav(48_000, 1, 4_800);
        let pcm = load_wav(&bytes).expect("décodage WAV");
        assert_eq!(pcm.len(), 4_800);

        // 24 kHz : exactement deux fois plus d'échantillons en sortie.
        let bytes = make_wav(24_000, 1, 1_000);
        assert_eq!(load_wav(&bytes).unwrap().len(), 2_000);
    }

    #[test]
    fn stereo_is_averaged() {
        // Deux canaux identiques : la moyenne ne doit pas diviser le niveau.
        let mono = load_wav(&make_wav(48_000, 1, 500)).unwrap();
        let stereo = load_wav(&make_wav(48_000, 2, 500)).unwrap();
        assert_eq!(mono.len(), stereo.len());
        for (a, b) in mono.iter().zip(stereo.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(load_wav(b"pas du tout un WAV").is_err());
        assert!(load_wav(&[]).is_err());
    }

    #[test]
    fn queue_adds_and_caps() {
        // Deux effets superposés : la file contient la somme.
        let mut buf = VecDeque::new();
        queue(&mut buf, &[0.2; 100], 1.0);
        queue(&mut buf, &[0.3; 100], 1.0);
        assert_eq!(buf.len(), 100);
        assert!(buf.iter().all(|s| (s - 0.5).abs() < 1e-6), "mixage non additif");

        // Le second effet, plus long, allonge la file sans écraser le début.
        let mut buf = VecDeque::new();
        queue(&mut buf, &[0.25; 10], 1.0);
        queue(&mut buf, &[0.25; 30], 2.0);
        assert_eq!(buf.len(), 30);
        assert!((buf[0] - 0.75).abs() < 1e-6, "début écrasé : {}", buf[0]);
        assert!((buf[20] - 0.5).abs() < 1e-6, "queue perdue : {}", buf[20]);

        // Plafond : une rafale ne fait pas enfler la file.
        let mut buf = VecDeque::new();
        for _ in 0..20 {
            queue(&mut buf, &[0.01; QUEUE_CAP * 2], 1.0);
        }
        assert_eq!(buf.len(), QUEUE_CAP);
    }
}
