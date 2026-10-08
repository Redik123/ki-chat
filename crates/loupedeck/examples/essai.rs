//! Essai du Loupedeck Live : allume les écrans et les boutons, puis affiche
//! chaque appui, cran de molette et toucher pendant N secondes.
//!
//!   cargo run -p ki-loupedeck --example essai -- [secondes] [COM3]

use ki_loupedeck::{ouvrir, Controle, Evenement};
use std::time::{Duration, Instant};

const PALETTE: [(u8, u8, u8); 12] = [
    (231, 76, 60), (230, 126, 34), (241, 196, 15), (46, 204, 113),
    (26, 188, 156), (52, 152, 219), (155, 89, 182), (236, 64, 122),
    (149, 165, 166), (88, 101, 242), (255, 255, 255), (60, 60, 60),
];

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let duree = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(60u64);
    let port = args.get(2).map(String::as_str);

    let (mut ecr, mut lec) = ouvrir(port)?;
    println!("Loupedeck ouvert, poignée de main OK");

    ecr.luminosite(10)?;
    ecr.remplir_tout(0, 0, 0)?;
    for (i, &(r, g, b)) in PALETTE.iter().enumerate() {
        ecr.remplir_touche(i as u8, r, g, b)?;
    }
    for i in 0..8 {
        let (r, g, b) = PALETTE[i as usize];
        ecr.couleur_rond(i, r, g, b)?;
    }
    println!("écrans et boutons colorés ; à toi de jouer ({duree} s)");

    let mut lum: i16 = 10;
    let fin = Instant::now() + Duration::from_secs(duree);
    while Instant::now() < fin {
        let Some(ev) = lec.suivant()? else { continue };
        println!("{ev:?}");
        match ev {
            Evenement::Appui(Controle::Rond(i)) => ecr.couleur_rond(i, 255, 255, 255)?,
            Evenement::Relache(Controle::Rond(i)) => {
                let (r, g, b) = PALETTE[i as usize];
                ecr.couleur_rond(i, r, g, b)?
            }
            Evenement::Toucher { touche: Some(t), .. } => ecr.remplir_touche(t, 255, 255, 255)?,
            Evenement::Lever { touche: Some(t), .. } => {
                let (r, g, b) = PALETTE[t as usize];
                ecr.remplir_touche(t, r, g, b)?
            }
            Evenement::Tourne { molette: 0, crans } => {
                lum = (lum + crans as i16).clamp(0, 10);
                ecr.luminosite(lum as u8)?;
                println!("  luminosité {lum}");
            }
            _ => {}
        }
    }

    ecr.remplir_tout(0, 0, 0)?;
    for i in 0..8 {
        ecr.couleur_rond(i, 0, 0, 0)?;
    }
    println!("fin de l'essai");
    Ok(())
}
