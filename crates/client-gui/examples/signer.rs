//! Signe un fichier avec la clé privée des releases, et imprime la clé
//! publique correspondante.
//!
//! ```text
//! SIGNING_KEY=<64 caractères hexadécimaux> \
//!   cargo run -p ki-client-gui --example signer -- <fichier> <fichier.sig>
//! ```
//!
//! # Pourquoi ici, et pas un script dans le workflow
//!
//! C'est un **exemple de ce crate**, donc il partage exactement la même
//! version d'`ed25519-dalek` que le code qui vérifie, dans le même
//! `Cargo.lock`. Une divergence d'implémentation entre le signeur et le
//! vérifieur est précisément le genre de défaut qui ne se voit qu'en
//! production, sur les machines des autres — et un outil fabriqué à la volée
//! par l'intégration continue l'aurait rendue possible.
//!
//! Accessoirement, il est relu comme le reste, couvert par
//! `clippy --all-targets`, et utilisable à la main le jour où il faut signer
//! sans passer par GitHub.
//!
//! La clé privée arrive par l'environnement, jamais en argument : la ligne de
//! commande d'un processus est lisible par les autres processus de la machine.

use std::io::Write as _;

use ed25519_dalek::{Signer, SigningKey};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [entree, sortie] = args.as_slice() else {
        eprintln!(
            "usage : SIGNING_KEY=<hex 64> signer <fichier> <fichier.sig>\n\
             la clé privée passe par l'environnement, jamais par la ligne de commande"
        );
        std::process::exit(2);
    };

    let hex = std::env::var("SIGNING_KEY")
        .map_err(|_| "SIGNING_KEY absent de l'environnement")?;
    let brut = decode_hex_32(hex.trim())?;
    let cle = SigningKey::from_bytes(&brut);

    // Imprimée à chaque signature : c'est elle qu'on grave dans le client
    // (voir deploy/SIGNATURE.md), et la relever d'un journal de workflow évite
    // d'avoir à la dériver à la main.
    println!("clé publique : {}", to_hex(cle.verifying_key().as_bytes()));

    let data = std::fs::read(entree)?;
    let signature = cle.sign(&data);
    // En hexadécimal plutôt qu'en binaire : un fichier de signature finit par
    // passer entre des mains humaines — collé dans un ticket, recopié — et un
    // format lisible évite d'y perdre des octets en chemin. Le vérifieur
    // accepte les deux.
    let mut fichier = std::fs::File::create(sortie)?;
    fichier.write_all(to_hex(&signature.to_bytes()).as_bytes())?;
    fichier.sync_all()?;
    println!("signature écrite : {sortie}");
    Ok(())
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_hex_32(hex: &str) -> Result<[u8; 32], Box<dyn std::error::Error>> {
    if hex.len() != 64 {
        return Err(format!(
            "la clé privée doit faire 64 caractères hexadécimaux, reçu {}",
            hex.len()
        )
        .into());
    }
    let mut out = [0u8; 32];
    for (i, octet) in out.iter_mut().enumerate() {
        *octet = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}
