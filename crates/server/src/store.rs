//! Écriture de fichiers d'état sans risque de troncature.
//!
//! `std::fs::write` tronque le fichier avant d'écrire. Une coupure de
//! courant, un `docker stop` brutal ou un crash pendant cette fenêtre laisse
//! `users.json` ou `server.json` à zéro octet — et au redémarrage suivant le
//! serveur refuse de démarrer, comptes compris. On écrit donc à côté puis on
//! renomme : `rename` est atomique sur NTFS comme sur ext4, si bien qu'à tout
//! instant le chemin final désigne soit l'ancien contenu complet, soit le
//! nouveau, jamais un fichier à moitié écrit.

use std::path::Path;

/// Écrit `bytes` dans `path` de façon atomique.
///
/// Le fichier temporaire est un frère du fichier visé, pas un fichier de
/// `TEMP` : `rename` entre volumes différents n'est pas atomique, et sous
/// Windows il échoue purement et simplement.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    // `rename` écrase la cible sur les deux plateformes visées. En cas
    // d'échec on retire le temporaire, sans quoi il resterait à traîner.
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Le cas nominal : le contenu arrive, et le temporaire ne survit pas.
    #[test]
    fn writes_then_leaves_no_temporary() {
        let dir = std::env::temp_dir().join(format!("ki-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");

        write_atomic(&path, b"{\"a\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":1}");
        assert!(!path.with_extension("tmp").exists());

        // Réécriture : l'ancien contenu est remplacé, pas complété.
        write_atomic(&path, b"{}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
