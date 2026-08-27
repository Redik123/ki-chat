//! Sonde du moteur audio : démarre un moteur complet (micro + sortie, moteur
//! natif par défaut sous Windows), le laisse tourner deux secondes et demie,
//! puis imprime le niveau micro et le journal audio.
//!
//! C'est l'outil de diagnostic à distance : `cargo run -p ki-voice --example
//! sonde-audio` chez un utilisateur qui « a le bug » dit quel moteur s'est
//! ouvert, sur quel périphérique, et ce qui a cloché.

use std::sync::Arc;

fn main() -> anyhow::Result<()> {
    let cfg = ki_voice::VoiceConfig::new(1, [0u8; 32]);
    // Aucun réseau : l'émission part dans le vide, la réception reste muette.
    let send: ki_voice::DatagramSend = Arc::new(|_d: &[u8]| {});
    let (_tx, rx) = std::sync::mpsc::channel();

    let engine = ki_voice::VoiceEngine::start(cfg, send, rx)?;
    std::thread::sleep(std::time::Duration::from_millis(2500));
    let stats = engine.stats();
    // Relevé avant l'arrêt : le moteur est encore là pour être interrogé.
    let docteur = engine.docteur();
    engine.shutdown();

    println!("niveau micro (crête du dernier bloc) : {:.4}", stats.mic_peak);
    // La grandeur qui juge P4 : chaque trame incomplète est un trou parti vers
    // la carte son. Zéro est la seule bonne valeur — et sur une sonde à vide,
    // sans locuteur distant, il n'y a rien à jouer, donc rien à manquer.
    println!(
        "trames incomplètes (sous-alimentations) : {}{}",
        stats.underruns,
        if stats.underruns == 0 { "" } else { "  ← à signaler" }
    );
    // Le docteur : ce qui s'interpose, ce que Windows autorise, et ce qu'il
    // faut essayer. C'est la partie qu'on se fait copier-coller quand
    // quelqu'un « a le bug ».
    println!();
    print!("{}", docteur.rapport());
    println!();
    println!("--- journal audio ---");
    for (ts, msg) in ki_voice::journal_snapshot() {
        println!("[{ts}] {msg}");
    }
    Ok(())
}
