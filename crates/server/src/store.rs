//! Écriture de fichiers d'état sans risque de troncature.
//!
//! `std::fs::write` tronque le fichier avant d'écrire. Une coupure de
//! courant, un `docker stop` brutal ou un crash pendant cette fenêtre laisse
//! `users.json` ou `server.json` à zéro octet — et au redémarrage suivant le
//! serveur refuse de démarrer, comptes compris. On écrit donc à côté puis on
//! renomme : `rename` est atomique sur NTFS comme sur ext4, si bien qu'à tout
//! instant le chemin final désigne soit l'ancien contenu complet, soit le
//! nouveau, jamais un fichier à moitié écrit.
//!
//! Le renommage n'est atomique que pour les **métadonnées** : rien ne garantit
//! que les octets du temporaire soient réellement sur le plateau avant que le
//! renommage ne soit publié. Une coupure de courant peut alors laisser le
//! fichier final pointer sur des données jamais écrites (des zéros). On force
//! donc l'écriture du temporaire sur le disque (`sync_all`) **avant** de
//! renommer, puis, là où la plateforme le permet, on synchronise le répertoire
//! pour durabiliser le renommage lui-même.

use std::io::Write;
use std::path::Path;

/// Écrit `bytes` dans `path` de façon atomique **et durable**.
///
/// Le fichier temporaire est un frère du fichier visé, pas un fichier de
/// `TEMP` : `rename` entre volumes différents n'est pas atomique, et sous
/// Windows il échoue purement et simplement.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    // Le temporaire porte un suffixe unique. Un nom fixe dérivé de la cible
    // était partagé par deux écritures concurrentes du même fichier : chacune
    // le tronquait et y écrivait, et le premier renommage publiait un mélange
    // des deux — exactement la corruption que ce module existe pour empêcher.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("{}.{seq}.tmp", std::process::id()));
    // Écriture puis `sync_all` : les données du temporaire sont garanties sur
    // le disque avant le renommage. Sans ça, une coupure de courant juste
    // après le `rename` publierait un fichier au contenu jamais écrit.
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    // `rename` écrase la cible sur les deux plateformes visées. En cas
    // d'échec on retire le temporaire, sans quoi il resterait à traîner.
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // Synchroniser le répertoire durabilise le renommage lui-même. Best-effort,
    // et seulement là où c'est possible : sous Windows, ouvrir un répertoire
    // comme un fichier échoue, et le serveur de production tourne sous Linux.
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nombre de temporaires restés dans le dossier.
    fn leftovers(dir: &Path) -> usize {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count()
    }

    /// Le cas nominal : le contenu arrive, et le temporaire ne survit pas.
    #[test]
    fn writes_then_leaves_no_temporary() {
        let dir = std::env::temp_dir().join(format!("ki-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");

        write_atomic(&path, b"{\"a\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":1}");
        assert_eq!(leftovers(&dir), 0);

        // Réécriture : l'ancien contenu est remplacé, pas complété.
        write_atomic(&path, b"{}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
        assert_eq!(leftovers(&dir), 0);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Deux écritures simultanées du **même** fichier ne doivent pas se
    /// partager un temporaire : avec un nom fixe, chacune tronquait celui de
    /// l'autre et le fichier publié pouvait mêler les deux contenus.
    #[test]
    fn concurrent_writes_never_mix_their_contents() {
        let dir = std::env::temp_dir().join(format!("ki-store-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");

        // Deux contenus volumineux et distincts : la fenêtre d'écriture est
        // assez large pour que le mélange se produise s'il est possible.
        let a = vec![b'a'; 512 * 1024];
        let b = vec![b'b'; 512 * 1024];
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = [a.clone(), b.clone()]
            .into_iter()
            .map(|content| {
                let (path, barrier) = (path.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    write_atomic(&path, &content).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        // Le fichier publié est l'un des deux, entier — jamais un panaché.
        let written = std::fs::read(&path).unwrap();
        assert!(written == a || written == b, "contenus mélangés ({} octets)", written.len());
        assert_eq!(leftovers(&dir), 0);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
