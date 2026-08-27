//! Sons par défaut, synthétisés par le code — donc originaux, embarquables
//! dans un dépôt public, et remplaçables : un .wav du même nom déposé dans
//! le dossier « sons » prend la place du son généré.
//!
//! Le cahier des charges : discrets (on les entend cent fois par soirée),
//! distincts (on reconnaît l'événement sans regarder), et cohérents entre
//! eux (mêmes timbres, gammes voisines).

use std::collections::HashMap;

const RATE: f32 = 48_000.0;

/// Tous les sons par défaut, indexés par le nom d'événement du module `sfx`.
pub fn defaults() -> HashMap<String, Vec<f32>> {
    let mut sounds = HashMap::new();
    // Message reçu : une goutte d'eau, brève et douce.
    sounds.insert("message".into(), seq(&[(880.0, 70), (1174.7, 110)], 0.32));
    // Quelqu'un arrive dans mon vocal : deux notes montantes (do -> sol).
    sounds.insert("arrivee".into(), seq(&[(523.3, 90), (784.0, 130)], 0.4));
    // Quelqu'un part : les mêmes, descendantes.
    sounds.insert("depart".into(), seq(&[(784.0, 90), (523.3, 130)], 0.4));
    // Je rejoins un vocal : petit arpège majeur, l'accueil.
    sounds.insert(
        "rejoint-vocal".into(),
        seq(&[(392.0, 70), (523.3, 70), (659.3, 150)], 0.4),
    );
    // Je quitte : l'arpège replié.
    sounds.insert("quitte-vocal".into(), seq(&[(659.3, 70), (523.3, 70), (392.0, 150)], 0.4));
    // Micro coupé : un toc grave et mat.
    sounds.insert("micro-coupe".into(), seq(&[(233.1, 110)], 0.45));
    // Micro réactivé : le même, une octave au-dessus — plus « ouvert ».
    sounds.insert("micro-actif".into(), seq(&[(466.2, 110)], 0.45));
    sounds
}

/// Enchaîne des notes (fréquence en Hz, durée en ms) en un seul tampon.
fn seq(notes: &[(f32, u32)], gain: f32) -> Vec<f32> {
    let mut out = Vec::new();
    for &(freq, ms) in notes {
        tone_into(&mut out, freq, ms, gain);
    }
    // Petite queue de silence : évite qu'un périphérique coupe la fin.
    out.extend(std::iter::repeat_n(0.0, (RATE * 0.02) as usize));
    out
}

/// Une note : sinus + un peu de deuxième harmonique (chaleur), attaque de
/// 5 ms (pas de clic), décroissance exponentielle (percussif, pas de bourdon).
fn tone_into(out: &mut Vec<f32>, freq: f32, ms: u32, gain: f32) {
    let n = (RATE * ms as f32 / 1000.0) as usize;
    let attack = (RATE * 0.005) as usize;
    for i in 0..n {
        let t = i as f32 / RATE;
        let phase = 2.0 * std::f32::consts::PI * freq * t;
        let sample = phase.sin() + 0.25 * (2.0 * phase).sin();
        let env_in = if i < attack { i as f32 / attack as f32 } else { 1.0 };
        let env_out = (-4.5 * i as f32 / n as f32).exp();
        out.push(sample * env_in * env_out * gain);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_events_have_a_default() {
        let sounds = defaults();
        for name in [
            "message",
            "arrivee",
            "depart",
            "rejoint-vocal",
            "quitte-vocal",
            "micro-coupe",
            "micro-actif",
        ] {
            let pcm = sounds.get(name).unwrap_or_else(|| panic!("son manquant : {name}"));
            assert!(!pcm.is_empty());
            // Borné : jamais de saturation, jamais de silence total.
            let peak = pcm.iter().fold(0f32, |m, s| m.max(s.abs()));
            assert!(peak > 0.05 && peak <= 1.0, "{name} : crête {peak}");
        }
    }
}
