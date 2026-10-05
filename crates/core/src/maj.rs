//! La vérification des mises à jour, sans interface ni système : de quoi
//! s'assurer qu'un fichier téléchargé d'une release GitHub est bien celui
//! que drion a publié, pour cette plateforme et cette version.
//!
//! Les mêmes règles que le client PC (`update.rs` de client-gui), qui garde
//! encore les siennes : un manifeste texte signé en Ed25519 par la clé des
//! releases (`ki-signer`, voir deploy/SIGNATURE.md) lie la plateforme, la
//! version et l'empreinte SHA-256 du fichier.

use sha2::Digest as _;

/// La clé publique des releases, la même que celle du PC
/// (`RELEASE_PUBKEY_HEX` de client-gui) : une seule clé signe toutes les
/// plateformes.
pub const CLE_RELEASES_HEX: &str = "a739c476c6515dcb7937489e39753349ba2d878822565352ea8bd2328ba0e345";

/// La clé publique des releases, décodée.
pub fn cle_releases() -> anyhow::Result<ed25519_dalek::VerifyingKey> {
    let octets = ki_protocol::hex_decode(CLE_RELEASES_HEX)
        .ok_or_else(|| anyhow::anyhow!("clé de release illisible"))?;
    let octets: [u8; 32] =
        octets.try_into().map_err(|_| anyhow::anyhow!("clé de release de longueur inattendue"))?;
    Ok(ed25519_dalek::VerifyingKey::from_bytes(&octets)?)
}

/// Lit une signature, brute (64 octets) ou en hexadécimal.
pub fn lire_signature(brut: &[u8]) -> anyhow::Result<ed25519_dalek::Signature> {
    if brut.len() == 64 {
        let octets: [u8; 64] = brut.try_into().expect("longueur vérifiée");
        return Ok(ed25519_dalek::Signature::from_bytes(&octets));
    }
    let texte = std::str::from_utf8(brut)
        .map_err(|_| anyhow::anyhow!("signature de {} octets, illisible", brut.len()))?
        .trim();
    let octets = ki_protocol::hex_decode(texte)
        .ok_or_else(|| anyhow::anyhow!("signature ni brute ni hexadécimale"))?;
    let octets: [u8; 64] =
        octets.try_into().map_err(|_| anyhow::anyhow!("signature de longueur inattendue"))?;
    Ok(ed25519_dalek::Signature::from_bytes(&octets))
}

/// Vérifie qu'un fichier téléchargé est celui que le manifeste signé annonce
/// pour `plateforme` et `version_attendue`, et qu'il commence par `magie`
/// (la forme attendue : une archive zip pour un APK).
pub fn verifier_manifeste(
    cle: &ed25519_dalek::VerifyingKey,
    manifeste: &[u8],
    signature: &ed25519_dalek::Signature,
    plateforme: &str,
    version_attendue: &str,
    fichier: &[u8],
    magie: &[u8],
) -> anyhow::Result<()> {
    cle.verify_strict(manifeste, signature)
        .map_err(|_| anyhow::anyhow!("signature du manifeste invalide — mise à jour refusée"))?;
    let texte = std::str::from_utf8(manifeste).map_err(|_| anyhow::anyhow!("manifeste illisible"))?;
    anyhow::ensure!(texte.lines().next() == Some("ki-chat-maj 1"), "manifeste d'un format inconnu");
    let champ = |nom: &str| {
        texte
            .lines()
            .find_map(|l| l.strip_prefix(nom).and_then(|reste| reste.strip_prefix(' ')))
            .map(str::trim)
    };
    anyhow::ensure!(
        champ("plateforme") == Some(plateforme),
        "cette mise à jour est pour une autre plateforme — refusée"
    );
    anyhow::ensure!(
        champ("version") == Some(version_attendue),
        "le manifeste annonce une autre version que la release — refusée"
    );
    let empreinte: String = sha2::Sha256::digest(fichier).iter().map(|b| format!("{b:02x}")).collect();
    anyhow::ensure!(
        champ("sha256") == Some(empreinte.as_str()),
        "le fichier téléchargé n'est pas celui du manifeste — refusé"
    );
    anyhow::ensure!(fichier.starts_with(magie), "le fichier téléchargé n'a pas la forme attendue");
    Ok(())
}

/// `a` est-elle plus récente que `b` ? Comparaison numérique champ par champ
/// (« 0.10.0 » après « 0.9.0 »), suffixes `-…` et `+…` ignorés.
pub fn plus_recente(a: &str, b: &str) -> bool {
    champs(a) > champs(b)
}

fn champs(version: &str) -> [u32; 3] {
    let mut out = [0u32; 3];
    let coeur = version.split(['-', '+']).next().unwrap_or_default();
    for (place, champ) in out.iter_mut().zip(coeur.split('.')) {
        *place = champ.trim().parse().unwrap_or(0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer as _;

    fn signe(manifeste: &str) -> (ed25519_dalek::VerifyingKey, ed25519_dalek::Signature) {
        let cle = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        (cle.verifying_key(), cle.sign(manifeste.as_bytes()))
    }

    fn manifeste(plateforme: &str, version: &str, fichier: &[u8]) -> String {
        let h: String = sha2::Sha256::digest(fichier).iter().map(|b| format!("{b:02x}")).collect();
        format!("ki-chat-maj 1\nplateforme {plateforme}\nversion {version}\nsha256 {h}\n")
    }

    const APK: &[u8] = b"PK\x03\x04 un faux apk";

    #[test]
    fn un_apk_bien_signe_passe() {
        let m = manifeste("android", "0.1.60", APK);
        let (cle, sig) = signe(&m);
        verifier_manifeste(&cle, m.as_bytes(), &sig, "android", "0.1.60", APK, b"PK\x03\x04").unwrap();
    }

    #[test]
    fn la_mise_a_jour_du_pc_est_refusee_sur_android() {
        let m = manifeste("windows", "0.1.60", APK);
        let (cle, sig) = signe(&m);
        assert!(verifier_manifeste(&cle, m.as_bytes(), &sig, "android", "0.1.60", APK, b"PK").is_err());
    }

    #[test]
    fn un_fichier_altere_est_refuse() {
        let m = manifeste("android", "0.1.60", APK);
        let (cle, sig) = signe(&m);
        let altere = b"PK\x03\x04 un autre apk";
        assert!(verifier_manifeste(&cle, m.as_bytes(), &sig, "android", "0.1.60", altere, b"PK").is_err());
    }

    #[test]
    fn une_ancienne_version_resservie_est_refusee() {
        let m = manifeste("android", "0.1.50", APK);
        let (cle, sig) = signe(&m);
        assert!(verifier_manifeste(&cle, m.as_bytes(), &sig, "android", "0.1.60", APK, b"PK").is_err());
    }

    #[test]
    fn une_signature_d_une_autre_cle_est_refusee() {
        let m = manifeste("android", "0.1.60", APK);
        let (_, sig) = signe(&m);
        let autre = ed25519_dalek::SigningKey::from_bytes(&[8; 32]).verifying_key();
        assert!(verifier_manifeste(&autre, m.as_bytes(), &sig, "android", "0.1.60", APK, b"PK").is_err());
    }

    #[test]
    fn les_versions_se_comparent_en_nombres() {
        assert!(plus_recente("0.1.60", "0.1.58"));
        assert!(plus_recente("0.10.0", "0.9.9"));
        assert!(!plus_recente("0.1.58", "0.1.58"));
        assert!(!plus_recente("0.1.58-rc1", "0.1.58"));
    }

    #[test]
    fn la_cle_des_releases_se_lit() {
        cle_releases().unwrap();
    }
}
