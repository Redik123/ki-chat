//! Deux voix dans un seul micro : la sienne, collée à la perche, et celle de
//! la personne assise à côté. La sonde les enregistre tour à tour, telles
//! que la carte son les livre, et mesure ce qui les sépare — de quoi régler
//! la détection pour qu'elle n'ouvre que sur la première.
//!
//! ```text
//! cargo run --release -p ki-voice --example sonde-voisine -- ["External Mic (Sound Blaster G8 USB-1)"] [dossier]
//! ```
//!
//! Sans nom de micro : le micro par défaut de Windows. Le dossier
//! (`target/diag/voisine` par défaut) reçoit `voisine-brut.wav` et
//! `voisine-phases.txt`, les bornes de chaque phase en échantillons.

use std::io::Write;

use ki_voice::effects::ecrire_wav;
use ki_voice::silero::Silero;
use ki_voice::{capturer_micro, FRAME_SAMPLES, SAMPLE_RATE};

/// Une consigne, et combien de temps on la tient.
struct Phase {
    cle: &'static str,
    consigne: &'static str,
    secondes: usize,
}

const PHASES: [Phase; 6] = [
    Phase { cle: "silence", consigne: "SILENCE — personne ne parle", secondes: 5 },
    Phase { cle: "moi", consigne: "TOI SEUL — parle normalement, comme en jeu", secondes: 10 },
    Phase { cle: "elle", consigne: "ELLE SEULE — elle parle depuis sa place, toi tu te tais", secondes: 10 },
    Phase { cle: "deux", consigne: "LES DEUX — vous parlez en même temps", secondes: 10 },
    Phase { cle: "elle-fort", consigne: "ELLE, FORT — elle rit ou parle fort, toi tu te tais", secondes: 6 },
    Phase { cle: "moi-doux", consigne: "TOI, DOUCEMENT — ta voix la plus basse en jeu", secondes: 6 },
];

/// Entre deux consignes : le temps de lire la suivante. Hors mesure.
const PAUSE: usize = 3;

fn en_db(x: f32) -> f32 {
    20.0 * x.max(1e-9).log10()
}

fn efficace(t: &[f32]) -> f32 {
    (t.iter().map(|s| s * s).sum::<f32>() / t.len().max(1) as f32).sqrt()
}

fn centile(tries: &[f32], c: f32) -> f32 {
    tries[((tries.len() - 1) as f32 * c) as usize]
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let micro = args.first().map(String::as_str).filter(|n| !n.is_empty());
    let dossier = std::path::PathBuf::from(args.get(1).map_or("target/diag/voisine", String::as_str));
    std::fs::create_dir_all(&dossier)?;

    // Le plan : début et fin de chaque phase, en échantillons.
    let rate = SAMPLE_RATE as usize;
    let mut bornes = Vec::new();
    let mut t = PAUSE * rate;
    for p in &PHASES {
        bornes.push((t, t + p.secondes * rate));
        t += (p.secondes + PAUSE) * rate;
    }
    let total = t - PAUSE * rate;

    println!("Sonde « la voix d'à côté » — {} s en tout.", total / rate);
    println!("Lis la consigne à chaque bip. Rien n'est envoyé nulle part.\n");
    let mut courante = usize::MAX;
    let (pcm, nom) = capturer_micro(micro, true, true, |pris, trame| {
        let phase = bornes.iter().position(|&(d, f)| pris >= d.saturating_sub(PAUSE * rate) && pris < f);
        if let Some(k) = phase {
            if k != courante {
                courante = k;
                println!("\n\x07>>> {}/{} : {}", k + 1, PHASES.len(), PHASES[k].consigne);
            }
            let (debut, fin) = bornes[k];
            let crete = trame.iter().fold(0f32, |m, s| m.max(s.abs()));
            let barre = "#".repeat(((en_db(crete) + 60.0).clamp(0.0, 60.0) / 2.0) as usize);
            if pris < debut {
                print!("\r    prépare-toi… {:>2} s {:<32}", (debut - pris).div_ceil(rate), "");
            } else {
                print!("\r    ENREGISTRE   {:>2} s |{barre:<30}|", (fin - pris).div_ceil(rate));
            }
            let _ = std::io::stdout().flush();
        }
        pris < total
    })?;
    println!("\n\x07\nFini. Micro : {nom}\n");

    ecrire_wav(dossier.join("voisine-brut.wav"), &pcm)?;
    let mut plan = String::new();
    for (p, (d, f)) in PHASES.iter().zip(&bornes) {
        plan.push_str(&format!("{} {} {}\n", p.cle, d, f));
    }
    std::fs::write(dossier.join("voisine-phases.txt"), plan)?;

    // Ce que Silero pense du micro brut, trame par trame.
    let mut vad = Silero::new()?;
    let probas: Vec<f32> = pcm
        .as_chunks::<FRAME_SAMPLES>()
        .0
        .iter()
        .map(|t| {
            vad.traiter(t);
            vad.derniere()
        })
        .collect();

    println!("phase      | crête max | efficace 50 % / 90 % | fond 10 % | Silero ≥ 0,5 | Silero médiane");
    for (p, &(d, f)) in PHASES.iter().zip(&bornes) {
        let trames = pcm[d..f.min(pcm.len())].as_chunks::<FRAME_SAMPLES>().0;
        let mut eff: Vec<f32> = trames.iter().map(|t| efficace(t)).collect();
        eff.sort_by(f32::total_cmp);
        let crete = pcm[d..f.min(pcm.len())].iter().fold(0f32, |m, s| m.max(s.abs()));
        let mut ps: Vec<f32> = probas[d / FRAME_SAMPLES..(f / FRAME_SAMPLES).min(probas.len())].to_vec();
        let hauts = ps.iter().filter(|&&p| p >= 0.5).count() as f32 / ps.len().max(1) as f32;
        ps.sort_by(f32::total_cmp);
        println!(
            "{:<10} | {:+8.1}  | {:+8.1} / {:+6.1}    | {:+7.1}   | {:>9.0} %  | {:.2}",
            p.cle,
            en_db(crete),
            en_db(centile(&eff, 0.5)),
            en_db(centile(&eff, 0.9)),
            en_db(centile(&eff, 0.1)),
            hauts * 100.0,
            centile(&ps, 0.5),
        );
    }
    println!("\nEnregistré dans {}", dossier.display());
    Ok(())
}
