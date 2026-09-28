//! Signe un fichier avec la clé privée des releases, et imprime la clé
//! publique correspondante.
//!
//! ```text
//! SIGNING_KEY=<64 caractères hexadécimaux> \
//!   cargo run -p ki-signer -- <fichier> <fichier.sig> \
//!   [--manifeste <plateforme> <version> <fichier.manifeste>] \
//!   [--cle-attendue <clé publique en hexadécimal>]
//! ```
//!
//! # Pourquoi ici, et pas un script dans le workflow
//!
//! C'est un crate de cet espace de travail, donc il partage exactement la
//! même version d'`ed25519-dalek` que le code qui vérifie, dans le même
//! `Cargo.lock`. Une divergence d'implémentation entre le signeur et le
//! vérifieur est précisément le genre de défaut qui ne se voit qu'en
//! production, sur les machines des autres — et un outil fabriqué à la volée
//! par l'intégration continue l'aurait rendue possible.
//!
//! Un crate à part, et non plus un exemple du client (jusqu'à la 0.1.51) :
//! le travail de la CI qui signe ne compile que lui — une poignée de crates
//! de cryptographie — au lieu des mille de l'application, dont les scripts de
//! build s'exécutaient à côté de la clé privée.
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
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // La clé publique que le client porte, si on la donne : signer avec une
    // autre privée, c'est publier une release que tout client installé
    // refuserait — et plus aucune mise à jour ne passerait.
    let attendue = match args.iter().position(|a| a == "--cle-attendue") {
        Some(i) if i + 1 < args.len() => {
            let cle = args.remove(i + 1);
            args.remove(i);
            Some(cle)
        }
        Some(_) => usage(),
        None => None,
    };
    let (entree, sortie, manifeste) = match args.as_slice() {
        [entree, sortie] => (entree, sortie, None),
        [entree, sortie, drapeau, plateforme, version, dest] if drapeau == "--manifeste" => {
            (entree, sortie, Some((plateforme, version, dest)))
        }
        _ => usage(),
    };

    let hex = std::env::var("SIGNING_KEY")
        .map_err(|_| "SIGNING_KEY absent de l'environnement")?;
    let brut = decode_hex_32(hex.trim())?;
    let cle = SigningKey::from_bytes(&brut);
    let publique = to_hex(cle.verifying_key().as_bytes());
    if let Some(attendue) = attendue {
        if !attendue.trim().eq_ignore_ascii_case(&publique) {
            return Err(format!(
                "la clé privée donnée ne correspond pas à la clé publique attendue \
                 ({publique} au lieu de {}) : rien n'est signé",
                attendue.trim()
            )
            .into());
        }
    }

    // Imprimée à chaque signature : c'est elle qu'on grave dans le client
    // (voir deploy/SIGNATURE.md), et la relever d'un journal de workflow évite
    // d'avoir à la dériver à la main.
    println!("clé publique : {publique}");

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

    // Le manifeste : ce que la signature des seuls octets ne disait pas —
    // pour quelle plateforme, et quelle version. Sans lui, qui pouvait
    // publier une release resservait une ancienne version signée sous une
    // étiquette neuve, ou l'archive macOS renommée en ki-chat.exe : l'une et
    // l'autre passaient la vérification. La première ligne sépare les
    // domaines : une signature de manifeste ne vaut jamais pour un binaire,
    // ni l'inverse.
    if let Some((plateforme, version, dest)) = manifeste {
        use sha2::Digest as _;
        let empreinte = to_hex(&sha2::Sha256::digest(&data));
        let texte = texte_du_manifeste(plateforme, version, &empreinte);
        std::fs::write(dest, texte.as_bytes())?;
        let signature = cle.sign(texte.as_bytes());
        let dest_sig = format!("{dest}.sig");
        std::fs::write(&dest_sig, to_hex(&signature.to_bytes()).as_bytes())?;
        println!("manifeste écrit : {dest} (et {dest_sig})");
    }
    Ok(())
}

/// Le texte du manifeste, tel que `crates/client-gui/src/update.rs` le lit
/// ligne à ligne : la première sépare les domaines (une signature de
/// manifeste ne vaut jamais pour un binaire, ni l'inverse).
fn texte_du_manifeste(plateforme: &str, version: &str, empreinte: &str) -> String {
    format!("ki-chat-maj 1\nplateforme {plateforme}\nversion {version}\nsha256 {empreinte}\n")
}

fn usage() -> ! {
    eprintln!(
        "usage : SIGNING_KEY=<hex 64> ki-signer <fichier> <fichier.sig> \
         [--manifeste <plateforme> <version> <fichier.manifeste>] \
         [--cle-attendue <hex 64>]\n\
         la clé privée passe par l'environnement, jamais par la ligne de commande"
    );
    std::process::exit(2);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Le format que le client attend, à la ligne près : le changer ici sans
    /// le changer là-bas publierait des releases que personne n'installe.
    #[test]
    fn le_manifeste_a_le_format_que_le_client_lit() {
        assert_eq!(
            texte_du_manifeste("windows", "0.1.52", "ab12"),
            "ki-chat-maj 1\nplateforme windows\nversion 0.1.52\nsha256 ab12\n"
        );
    }

    #[test]
    fn la_cle_privee_se_lit_en_hexadecimal_de_64_caracteres() {
        let cle = decode_hex_32(&"07".repeat(32)).unwrap();
        assert_eq!(cle, [7u8; 32]);
        assert!(decode_hex_32("07").is_err());
        assert!(decode_hex_32(&"zz".repeat(32)).is_err());
    }
}
