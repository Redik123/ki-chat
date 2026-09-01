//! Vérifie à la main la signature d'une release, avec la clé publique gravée
//! dans le client.
//!
//! ```text
//! cargo run -p ki-client-gui --example verifieur -- ki-chat.exe ki-chat.exe.sig
//! ```
//!
//! # Pourquoi cet outil existe
//!
//! Le pendant de `signer`, et pour la même raison : c'est un **exemple de ce
//! crate**, donc il partage le même `ed25519-dalek` et le même `Cargo.lock`
//! que le code qui vérifie au démarrage. Une divergence entre le vérifieur
//! d'ici et celui de là ne se verrait pas.
//!
//! Il répond à une question qu'on ne peut pas poser autrement : la chaîne de
//! publication a-t-elle vraiment signé ce qu'elle a vraiment publié ? Le
//! journal du workflow dit qu'une signature a été écrite ; il ne dit pas
//! qu'elle correspond à l'actif déposé. Sans cet outil, la réponse n'arrive
//! qu'à la release **suivante**, sur les machines des autres — au moment
//! précis où une erreur bloquerait toute mise à jour ultérieure.
//!
//! La clé publique n'est pas un argument : c'est celle du client, relue ici
//! depuis son propre fichier source. Lui en passer une autre reviendrait à
//! vérifier autre chose que ce que le client vérifiera.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

/// La clé gravée dans `update.rs`, extraite à la compilation.
///
/// `include_str!` plutôt qu'une copie : deux constantes finiraient par
/// diverger, et c'est justement la divergence qu'on cherche à exclure.
const SOURCE_UPDATE: &str = include_str!("../src/update.rs");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [fichier, signature] = args.as_slice() else {
        eprintln!("usage : verifieur <fichier> <fichier.sig>");
        std::process::exit(2);
    };

    let hex_cle = cle_publique_du_client()
        .ok_or("aucune clé publique gravée dans update.rs : rien à vérifier")?;
    println!("clé publique du client : {hex_cle}");

    let cle = VerifyingKey::from_bytes(&decode_hex::<32>(&hex_cle)?)?;
    let data = std::fs::read(fichier)?;
    // Le vérifieur du client accepte l'hexadécimal comme le binaire : on fait
    // pareil, sans quoi cet outil validerait un format que lui refuserait.
    let brut = std::fs::read(signature)?;
    let sig = match std::str::from_utf8(&brut).map(str::trim) {
        Ok(texte) if texte.len() == 128 => decode_hex::<64>(texte)?,
        _ if brut.len() == 64 => {
            let mut b = [0u8; 64];
            b.copy_from_slice(&brut);
            b
        }
        _ => return Err("signature illisible (ni 128 caractères hexadécimaux, ni 64 octets)".into()),
    };

    match cle.verify(&data, &Signature::from_bytes(&sig)) {
        Ok(()) => {
            println!("signature VALIDE pour {fichier} ({} octets)", data.len());
            Ok(())
        }
        Err(e) => Err(format!("signature INVALIDE : {e}").into()),
    }
}

/// Relit `RELEASE_PUBKEY_HEX` dans le source du client.
///
/// Une petite analyse de texte plutôt qu'un `pub const` exposé : la constante
/// n'a aucune raison de sortir de son module pour le confort d'un outil de
/// mise au point.
fn cle_publique_du_client() -> Option<String> {
    let apres = SOURCE_UPDATE.split_once("RELEASE_PUBKEY_HEX")?.1;
    let ouvrant = apres.find('"')?;
    let reste = &apres[ouvrant + 1..];
    let fermant = reste.find('"')?;
    let hex = &reste[..fermant];
    (!hex.is_empty()).then(|| hex.to_string())
}

fn decode_hex<const N: usize>(hex: &str) -> Result<[u8; N], Box<dyn std::error::Error>> {
    if hex.len() != N * 2 {
        return Err(format!("{} caractères hexadécimaux attendus, reçu {}", N * 2, hex.len()).into());
    }
    let mut out = [0u8; N];
    for (i, octet) in out.iter_mut().enumerate() {
        *octet = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}
