//! Ce que la page Casque voit de la carte son : volumes et gains du micro,
//! **en lecture seule** — la sonde ne règle rien.
//!
//! ```text
//! cargo run -p ki-voice --example sonde-casque
//! ```

use ki_voice::materiel::{scinder_nom, Materiel, Ordre};

fn main() {
    let m = Materiel::global();
    m.ordonner(Ordre::Suivre { entree: None, sortie: None });
    m.tenir_eveille();
    // Les amplifications arrivent après les volumes : huit secondes plus
    // tard sur certaines cartes USB (NICEHCK NK1 MAX).
    let mut e = m.etat();
    for _ in 0..300 {
        if e.disponible && e.topologie_lue {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        e = m.etat();
    }
    if !e.disponible {
        println!("réglages matériels indisponibles (Windows seulement)");
        return;
    }
    let nom = |n: &Option<String>| {
        n.as_deref().map(|n| {
            let (point, carte) = scinder_nom(n);
            format!("{point} — sur {}", carte.unwrap_or("?"))
        })
    };
    println!("sortie : {}", nom(&e.sortie_nom).unwrap_or_default());
    if let Some(v) = &e.sortie {
        println!(
            "  volume {:.0} % = {:.1} dB (de {:.1} à {:.1}) · balance {:?}",
            v.scalaire * 100.0,
            v.db,
            v.min_db,
            v.max_db,
            v.balance
        );
    }
    println!("micro : {}", nom(&e.entree_nom).unwrap_or_default());
    if let Some(v) = &e.entree {
        println!("  niveau {:.0} % = {:+.1} dB (de {:.1} à {:.1})", v.scalaire * 100.0, v.db, v.min_db, v.max_db);
    }
    for g in &e.amplis {
        println!("  {} : {:+.1} dB (de {:.1} à {:.1}, pas {:.1})", g.nom, g.db, g.min_db, g.max_db, g.pas_db);
    }
}
