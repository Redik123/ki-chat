//! Le détail d'un essai « enregistrer et réécouter » gardé en WAV : le
//! spectre de sa voix par tiers d'octave, brute et envoyée, comparé à une
//! parole moyenne — pour voir où ses aigus se perdent.
//!
//! ```text
//! cargo run -p ki-voice --example analyse-essai -- essai-brut.wav essai-envoye.wav ["ph:90:0:0.7:1:1;…"]
//! ```
//!
//! Le troisième argument, facultatif : l'égaliseur de sa voix tel que le
//! garde `app.ron` (`egaliseur_micro`), pour séparer son effet de celui du
//! reste de la chaîne.

use ki_voice::egaliseur;
use ki_voice::effects::load_wav_file;
use ki_voice::spectre::{analyser, TIERS};

fn en_db(x: f32) -> f32 {
    20.0 * x.max(1e-9).log10()
}

/// Crête de la voix (la trame au rang du quart le plus fort) et bruit de
/// fond (efficace des 10 % de trames les plus calmes), en dBFS.
fn niveaux(pcm: &[f32]) -> (f32, f32) {
    let mut cretes: Vec<f32> = pcm.chunks(960).map(|t| t.iter().fold(0f32, |m, s| m.max(s.abs()))).collect();
    let mut efficaces: Vec<f32> =
        pcm.chunks(960).map(|t| (t.iter().map(|s| s * s).sum::<f32>() / t.len() as f32).sqrt()).collect();
    cretes.sort_by(|a, b| b.total_cmp(a));
    efficaces.sort_by(|a, b| a.total_cmp(b));
    let calmes = &efficaces[..(efficaces.len() / 10).max(1)];
    (en_db(cretes[cretes.len() / 4]), en_db(calmes.iter().sum::<f32>() / calmes.len() as f32))
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(args.len() >= 2, "usage : analyse-essai <brut.wav> <envoye.wav> [égaliseur]");
    let brute = load_wav_file(&args[0])?;
    let envoyee = load_wav_file(&args[1])?;
    let eq = args.get(2).map(|t| egaliseur::lire(t)).unwrap_or_default();

    for (nom, pcm) in [("brut", &brute), ("envoyé", &envoyee)] {
        let (voix, bruit) = niveaux(pcm);
        println!(
            "{nom:7} : {:.1} s, crête de voix {voix:+.1} dBFS, bruit de fond {bruit:+.1} dBFS (écart {:.0} dB)",
            pcm.len() as f32 / 48_000.0,
            voix - bruit
        );
    }
    let (Some(b), Some(e)) = (analyser(&brute), analyser(&envoyee)) else {
        anyhow::bail!("pas assez de voix pour analyser");
    };
    let avec_eq = b.filtre(|f| egaliseur::reponse_db(&eq, f));
    println!();
    println!("Écart à une parole moyenne, calé à 0 sur 315 Hz - 1 kHz :");
    println!("   Hz |   brut | brut+EQ | envoyé | chaîne hors EQ");
    let (eb, ee, eq_) = (b.ecarts_db(), e.ecarts_db(), avec_eq.ecarts_db());
    for k in 0..TIERS.len() {
        println!(
            "{:5} | {:+6.1} | {:+7.1} | {:+6.1} | {:+6.1}",
            TIERS[k], eb[k], eq_[k], ee[k], ee[k] - eq_[k]
        );
    }
    println!();
    println!(
        "présence (2-6,3 kHz) : brut {:+.1}, brut+EQ {:+.1}, envoyé {:+.1} dB",
        b.presence_db(),
        avec_eq.presence_db(),
        e.presence_db()
    );
    println!(
        "graves (100-250 Hz)  : brut {:+.1}, brut+EQ {:+.1}, envoyé {:+.1} dB",
        b.graves_db(),
        avec_eq.graves_db(),
        e.graves_db()
    );
    Ok(())
}
