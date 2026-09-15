//! yt-dlp se met à jour tout seul (PLAN-MUSIQUE.md, jalon M4).
//!
//! YouTube et SoundCloud changent, yt-dlp suit à la semaine, et l'image du
//! serveur ne se reconstruit qu'à nos releases : entre deux, le bot musique
//! cassait — SoundCloud, en septembre 2026, le temps qu'une release sorte.
//! D'où ce module : au démarrage puis chaque jour, le serveur regarde la
//! dernière release de yt-dlp, télécharge le binaire de son architecture
//! dans `data/outils/` si l'empreinte publiée n'est pas celle du fichier
//! qu'il a, la vérifie (SHA-256, comme le Dockerfile au build), et le bot
//! passe dessus sans redémarrer. `KI_YTDLP` posé par l'admin = pas de mise
//! à jour : il a choisi son binaire.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::state::AppState;

/// Où yt-dlp publie : la dernière release, ses binaires et leurs sommes.
const DEPOT: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";
/// Premier contrôle après le démarrage, puis chaque jour.
const PREMIER_DELAI: Duration = Duration::from_secs(20);
const PERIODE: Duration = Duration::from_secs(24 * 3600);
/// Le binaire fait une trentaine de mégaoctets.
const TELECHARGEMENT_MAX: u64 = 200 * 1024 * 1024;

/// Le nom du binaire publié pour cette machine, s'il en existe un.
pub fn nom_binaire() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("yt-dlp_linux"),
        ("linux", "aarch64") => Some("yt-dlp_linux_aarch64"),
        ("windows", "x86_64") => Some("yt-dlp.exe"),
        ("macos", _) => Some("yt-dlp_macos"),
        _ => None,
    }
}

/// Où l'on range le binaire téléchargé.
pub fn chemin_local(data_dir: &str) -> PathBuf {
    let nom = if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    };
    PathBuf::from(data_dir).join("outils").join(nom)
}

/// L'empreinte publiée pour `nom` dans le fichier `SHA2-256SUMS`
/// (« <hex>  <nom> », une ligne par fichier).
pub fn empreinte_attendue(sommes: &str, nom: &str) -> Option<String> {
    sommes.lines().find_map(|ligne| {
        let mut parts = ligne.split_whitespace();
        let hex = parts.next()?;
        let fichier = parts.next()?;
        (fichier == nom && hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| hex.to_ascii_lowercase())
    })
}

fn empreinte_de(chemin: &Path) -> Option<String> {
    let octets = std::fs::read(chemin).ok()?;
    Some(format!("{:x}", Sha256::digest(&octets)))
}

fn lire_borne(reponse: ureq::Response, max: u64) -> Result<Vec<u8>, String> {
    let mut octets = Vec::new();
    reponse
        .into_reader()
        .take(max + 1)
        .read_to_end(&mut octets)
        .map_err(|e| e.to_string())?;
    if octets.len() as u64 > max {
        return Err("réponse trop grosse".into());
    }
    Ok(octets)
}

/// Compare le binaire local à la dernière release, télécharge s'il diffère.
/// Rend le chemin du binaire s'il vient de changer, `None` s'il était à
/// jour — et l'erreur si le réseau ou la vérification a refusé.
pub fn mettre_a_jour(data_dir: &str, agent: &ureq::Agent) -> Result<Option<PathBuf>, String> {
    let nom = nom_binaire().ok_or("pas de binaire yt-dlp publié pour cette machine")?;
    let sommes = agent
        .get(&format!("{DEPOT}/SHA2-256SUMS"))
        .timeout(Duration::from_secs(30))
        .call()
        .map_err(|e| format!("sommes : {e}"))?;
    let sommes = String::from_utf8_lossy(&lire_borne(sommes, 64 * 1024)?).into_owned();
    let attendue = empreinte_attendue(&sommes, nom)
        .ok_or_else(|| format!("{nom} absent des sommes publiées"))?;
    let cible = chemin_local(data_dir);
    if empreinte_de(&cible).as_deref() == Some(attendue.as_str()) {
        return Ok(None);
    }
    let reponse = agent
        .get(&format!("{DEPOT}/{nom}"))
        .timeout(Duration::from_secs(300))
        .call()
        .map_err(|e| format!("téléchargement : {e}"))?;
    let octets = lire_borne(reponse, TELECHARGEMENT_MAX)?;
    let obtenue = format!("{:x}", Sha256::digest(&octets));
    if obtenue != attendue {
        return Err(
            "empreinte du binaire téléchargé différente de celle publiée — rien d'installé".into(),
        );
    }
    if let Some(dossier) = cible.parent() {
        std::fs::create_dir_all(dossier).map_err(|e| e.to_string())?;
    }
    let partiel = cible.with_extension("part");
    std::fs::write(&partiel, &octets).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&partiel, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    std::fs::rename(&partiel, &cible).map_err(|e| e.to_string())?;
    Ok(Some(cible))
}

/// La tâche : un contrôle peu après le démarrage, puis chaque jour. Tout le
/// réseau et le disque sur le pool bloquant.
pub async fn boucle(state: Arc<AppState>) {
    if std::env::var("KI_YTDLP").is_ok() {
        tracing::info!("musique : KI_YTDLP est posé — pas de mise à jour automatique de yt-dlp");
        return;
    }
    if nom_binaire().is_none() {
        return;
    }
    tokio::time::sleep(PREMIER_DELAI).await;
    loop {
        let data_dir = state.data_dir.clone();
        let resultat = tokio::task::spawn_blocking(move || {
            let agent = ureq::AgentBuilder::new().build();
            mettre_a_jour(&data_dir, &agent)
        })
        .await;
        match resultat {
            Ok(Ok(Some(chemin))) => {
                if state.musique.remplacer_yt_dlp(&chemin) {
                    tracing::info!("musique : yt-dlp mis à jour dans {}", chemin.display());
                } else {
                    tracing::warn!(
                        "musique : le yt-dlp téléchargé ne se lance pas — l'ancien reste"
                    );
                }
            }
            Ok(Ok(None)) => tracing::debug!("musique : yt-dlp à jour"),
            Ok(Err(e)) => tracing::warn!("musique : mise à jour de yt-dlp impossible : {e}"),
            Err(e) => tracing::error!("musique : tâche de mise à jour interrompue : {e}"),
        }
        tokio::time::sleep(PERIODE).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l_empreinte_se_lit_dans_les_sommes_publiees() {
        let sommes = "\
0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  yt-dlp
fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210  yt-dlp_linux
ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789  yt-dlp.exe
";
        assert_eq!(
            empreinte_attendue(sommes, "yt-dlp_linux").as_deref(),
            Some("fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210")
        );
        // En minuscules, quelle que soit la casse publiée.
        assert_eq!(
            empreinte_attendue(sommes, "yt-dlp.exe").as_deref(),
            Some("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789")
        );
        assert_eq!(empreinte_attendue(sommes, "yt-dlp_macos"), None);
        assert_eq!(
            empreinte_attendue("pas une somme  yt-dlp_linux\n", "yt-dlp_linux"),
            None
        );
    }

    #[test]
    fn le_binaire_et_son_chemin_suivent_la_machine() {
        // Sur les machines que l'on livre, il y a toujours un binaire.
        if cfg!(any(
            target_os = "linux",
            target_os = "windows",
            target_os = "macos"
        )) {
            assert!(nom_binaire().is_some());
        }
        let chemin = chemin_local("data");
        assert!(chemin.starts_with("data"));
        assert!(chemin.to_string_lossy().contains("outils"));
    }
}
