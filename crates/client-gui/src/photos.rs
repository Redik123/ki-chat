//! Cache disque des photos de profil.
//!
//! Les vignettes sont rangées **par empreinte** : `avatars/<hash>.png`. Le
//! nom du fichier est donc le contenu, ce qui donne trois propriétés
//! gratuitement :
//!
//! - la liste des membres porte déjà l'empreinte de chaque photo, donc on
//!   sait sans index si on l'a déjà — un simple test d'existence ;
//! - une photo changée est un fichier différent, jamais une écriture par
//!   dessus : pas d'invalidation à gérer ;
//! - deux membres qui ont la même image ne l'occupent qu'une fois.
//!
//! Sans ce cache, tout se retéléchargeait à chaque démarrage : une trentaine
//! de vignettes de quelques kilo-octets à chaque lancement, pour des images
//! qui ne changent presque jamais.

use std::path::PathBuf;

/// Nombre de vignettes gardées. Au-delà, les plus anciennes partent : les
/// photos changent, le dossier ne doit pas gonfler indéfiniment.
const MAX_FILES: usize = 200;

/// Dossier du cache, créé au besoin.
fn dir() -> Option<PathBuf> {
    let dir = eframe::storage_dir("ki-chat")?.join("avatars");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Chemin d'une vignette. `None` si l'empreinte n'a pas la forme attendue —
/// elle vient du réseau, elle ne doit jamais pouvoir désigner un fichier
/// ailleurs sur le disque.
fn path_of(hash: &str) -> Option<PathBuf> {
    let sane = hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit());
    sane.then(|| dir().map(|d| d.join(format!("{hash}.png"))))?
}

/// Lit une vignette du cache.
pub fn load(hash: &str) -> Option<Vec<u8>> {
    std::fs::read(path_of(hash)?).ok()
}

/// Range une vignette. Un échec d'écriture n'est pas grave : on la
/// retéléchargera au prochain démarrage.
pub fn store(hash: &str, png: &[u8]) {
    let Some(path) = path_of(hash) else { return };
    if let Err(e) = std::fs::write(&path, png) {
        tracing::debug!("cache photo non écrit ({hash}) : {e}");
    }
}

/// Fait le ménage : au-delà de `MAX_FILES`, les vignettes les moins
/// récemment modifiées sont supprimées.
pub fn prune() {
    let Some(dir) = dir() else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .collect();
    if files.len() <= MAX_FILES {
        return;
    }
    files.sort_by_key(|(modified, _)| *modified);
    for (_, path) in files.iter().take(files.len() - MAX_FILES) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_malformed_hash_never_designates_a_file() {
        // L'empreinte vient du réseau : elle ne doit pas pouvoir sortir du
        // dossier de cache ni viser autre chose qu'un .png attendu.
        assert!(path_of("../../windows/system32/evil").is_none());
        assert!(path_of("..").is_none());
        assert!(path_of("court").is_none());
        assert!(path_of("zzzzzzzzzzzzzzzz").is_none()); // pas hexadécimal
        assert!(path_of("0123456789abcdef0").is_none()); // 17 caractères
    }

    #[test]
    fn a_well_formed_hash_stays_inside_the_cache_directory() {
        let Some(path) = path_of("0123456789abcdef") else {
            return; // pas de dossier de données sur cette machine
        };
        assert!(path.ends_with("0123456789abcdef.png"));
        assert_eq!(path.parent(), dir().as_deref());
    }

    #[test]
    fn a_missing_photo_reads_as_absent() {
        assert!(load("ffffffffffffffff").is_none());
        assert!(load("pas une empreinte").is_none());
    }
}
