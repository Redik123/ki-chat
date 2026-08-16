//! Banc d'essai headless de la boucle locale S1a : capture l'écran principal
//! pendant 5 secondes, fait l'aller-retour H.264 complet, imprime les stats
//! par étage. Aucune fenêtre, aucun réseau.
//!
//!     cargo run -p ki-video --example labo --release

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let stats = Arc::new(ki_video::StageStats::default());
    let delivered = Arc::new(AtomicU64::new(0));
    let sink: ki_video::FrameSink = {
        let delivered = delivered.clone();
        let stats = stats.clone();
        Arc::new(move |frame| {
            // Un vrai viewer peindrait ici ; on compte comme « peint ».
            assert_eq!(frame.rgba.len(), frame.width * frame.height * 4);
            delivered.fetch_add(1, Ordering::Relaxed);
            stats.painted.fetch_add(1, Ordering::Relaxed);
        })
    };

    println!("capture de l'écran principal pendant 5 s…");
    let handle = ki_video::LocalLoop::start(stats.clone(), sink)?;
    for i in 1..=5 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        println!("  t+{i}s  {}", stats.summary());
    }
    handle.stop();

    println!("\nRÉSULTAT : {}", stats.summary());
    let ok = delivered.load(Ordering::Relaxed) > 0;
    println!("verdict : {}", if ok { "OK — la boucle tourne" } else { "ÉCHEC — aucune image livrée" });
    std::process::exit(if ok { 0 } else { 1 });
}
