//! Mise à jour automatique depuis les releases GitHub.
//!
//! Au démarrage, l'application demande à GitHub la dernière release publiée
//! et compare son étiquette à sa propre version. Si une version plus récente
//! existe, elle la propose — et ne touche à rien tant que l'utilisateur n'a
//! pas accepté. Un refus vaut pour cette version : on ne redemande qu'à la
//! suivante, pour que « non » veuille dire non plutôt que « pas cette fois ».
//!
//! Le remplacement du binaire en cours d'exécution repose sur une propriété
//! de Windows : on ne peut pas *supprimer* un exécutable chargé, mais on peut
//! le *renommer*. L'ancien est donc écarté sous un autre nom, le nouveau
//! prend sa place, et le résidu est balayé au démarrage suivant — moment où
//! il n'est plus chargé, donc effaçable.
//!
//! Tout se joue à côté de l'exécutable, sans élévation : l'installeur pose
//! l'application dans le profil de l'utilisateur (`%LOCALAPPDATA%`), pas dans
//! `Program Files`, précisément pour qu'elle puisse se remplacer toute seule.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;

/// Dépôt qui publie les releases.
const REPO: &str = "Redik123/ki-chat";
/// Nom de l'exécutable attaché à chaque release.
const ASSET: &str = "ki-chat.exe";
/// GitHub refuse les requêtes sans agent identifié.
const AGENT: &str = concat!("ki-chat/", env!("CARGO_PKG_VERSION"));
/// La vérification ne doit pas retarder le démarrage : au-delà, on abandonne
/// silencieusement — une mise à jour ratée n'est pas une panne.
const TIMEOUT: Duration = Duration::from_secs(10);
/// Un exécutable de plus de 200 Mo n'est pas le nôtre.
const MAX_BYTES: u64 = 200 * 1024 * 1024;

/// Version de l'application en cours d'exécution.
pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Une release plus récente, telle que GitHub la décrit.
#[derive(Clone)]
pub struct Release {
    /// Étiquette débarrassée de son `v` : `0.2.0`.
    pub version: String,
    /// Notes de version, telles qu'écrites dans la release.
    pub notes: String,
    /// Téléchargement direct de l'exécutable.
    url: String,
    /// Taille annoncée, pour la barre de progression.
    size: u64,
}

/// Où en est la mise à jour, du point de vue de l'interface.
#[derive(Clone)]
pub enum Status {
    /// Rien à afficher : vérification en cours, déjà à jour, ou refusée.
    Idle,
    /// Une version plus récente attend l'accord de l'utilisateur.
    Available(Release),
    /// Téléchargement en cours.
    Downloading { done: u64, total: u64 },
    /// Binaire remplacé : il ne reste qu'à redémarrer.
    Ready,
    /// Échec, avec de quoi comprendre.
    Failed(String),
}

/// Redémarrage demandé. Un booléen global plutôt qu'un canal : `main` le lit
/// une seule fois, après la fermeture propre de la fenêtre.
static RESTART: AtomicBool = AtomicBool::new(false);

/// Sonde GitHub au démarrage, puis pilote le téléchargement si on l'accepte.
pub struct Updater {
    state: Arc<Mutex<Status>>,
    /// Version que l'utilisateur a refusée, mémorisée d'une session à l'autre.
    skipped: Option<String>,
}

impl Updater {
    /// Lance la vérification en tâche de fond. Ne bloque jamais le démarrage :
    /// l'application s'ouvre pendant que la requête part.
    pub fn start(skipped: Option<String>, ctx: egui::Context) -> Self {
        sweep();

        let state = Arc::new(Mutex::new(Status::Idle));
        let slot = state.clone();
        let refused = skipped.clone();
        std::thread::spawn(move || {
            match fetch_latest() {
                Ok(Some(release)) if refused.as_deref() == Some(release.version.as_str()) => {
                    tracing::info!(version = %release.version, "mise à jour déjà refusée");
                }
                Ok(Some(release)) => {
                    tracing::info!(version = %release.version, "mise à jour disponible");
                    *slot.lock().unwrap() = Status::Available(release);
                }
                Ok(None) => tracing::debug!("déjà à jour"),
                // Pas de release publiée, pas de réseau, quota GitHub
                // atteint : rien de tout ça ne justifie d'alerter.
                Err(e) => tracing::info!("vérification des mises à jour : {e:#}"),
            }
            ctx.request_repaint();
        });

        Self { state, skipped }
    }

    /// État courant, pour l'affichage.
    pub fn status(&self) -> Status {
        self.state.lock().unwrap().clone()
    }

    /// Accepte la mise à jour : télécharge puis remplace le binaire.
    pub fn accept(&self, release: &Release, ctx: egui::Context) {
        let state = self.state.clone();
        let release = release.clone();
        *state.lock().unwrap() = Status::Downloading { done: 0, total: release.size };
        std::thread::spawn(move || {
            let outcome = download(&release, &state).and_then(|staged| {
                let installed = install(&staged);
                // Le fichier intermédiaire ne survit à rien.
                let _ = std::fs::remove_file(&staged);
                installed
            });
            *state.lock().unwrap() = match outcome {
                Ok(()) => Status::Ready,
                Err(e) => {
                    tracing::warn!("mise à jour : {e:#}");
                    Status::Failed(format!("{e:#}"))
                }
            };
            ctx.request_repaint();
        });
    }

    /// Refuse cette version : on n'en reparle plus.
    pub fn skip(&mut self, version: &str) {
        self.skipped = Some(version.to_string());
        *self.state.lock().unwrap() = Status::Idle;
    }

    /// Ferme le message d'échec sans rien refuser.
    pub fn dismiss(&self) {
        *self.state.lock().unwrap() = Status::Idle;
    }

    /// Version refusée, à persister entre deux sessions.
    pub fn skipped(&self) -> &str {
        self.skipped.as_deref().unwrap_or("")
    }

    /// Page des releases, pour l'utilisateur qui préfère faire à la main.
    pub fn releases_page() -> String {
        format!("https://github.com/{REPO}/releases/latest")
    }
}

/// Demande le redémarrage. La fenêtre se referme d'abord — eframe enregistre
/// alors les réglages et le moteur audio rend les périphériques — et `main`
/// relance le binaire une fois la boucle sortie.
pub fn request_restart() {
    RESTART.store(true, Ordering::SeqCst);
}

/// Relance l'exécutable si une mise à jour vient d'être installée. Appelé par
/// `main` après la fermeture de la fenêtre.
pub fn relaunch_if_requested() {
    if !RESTART.load(Ordering::SeqCst) {
        return;
    }
    match std::env::current_exe().map(std::process::Command::new) {
        Ok(mut cmd) => {
            if let Err(e) = cmd.spawn() {
                tracing::error!("redémarrage impossible : {e}");
            }
        }
        Err(e) => tracing::error!("exécutable introuvable : {e}"),
    }
}

/// Efface ce qu'une mise à jour a laissé derrière elle : le binaire écarté,
/// et un téléchargement resté en plan. Au démarrage, plus rien de tout ça
/// n'est chargé, donc tout est effaçable.
fn sweep() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let _ = std::fs::remove_file(exe.with_extension("old"));
    let _ = std::fs::remove_file(exe.with_extension("new"));
}

/// Interroge GitHub. `Ok(None)` = on est déjà à jour.
fn fetch_latest() -> anyhow::Result<Option<Release>> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body: serde_json::Value = ureq::get(&url)
        .set("User-Agent", AGENT)
        .set("Accept", "application/vnd.github+json")
        .timeout(TIMEOUT)
        .call()?
        .into_json()?;

    // `/releases/latest` écarte déjà brouillons et pré-versions.
    let tag = body["tag_name"].as_str().unwrap_or_default();
    let version = tag.trim_start_matches('v').trim().to_string();
    if version.is_empty() || !newer(&version, current()) {
        return Ok(None);
    }

    let asset = body["assets"]
        .as_array()
        .and_then(|assets| assets.iter().find(|a| a["name"].as_str() == Some(ASSET)))
        .ok_or_else(|| anyhow::anyhow!("release {tag} sans exécutable {ASSET}"))?;
    let url = asset["browser_download_url"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("release {tag} sans lien de téléchargement"))?
        .to_string();
    // GitHub sert ses téléchargements en HTTPS ; on refuse le reste, une
    // redirection en clair suffirait sinon à substituer le binaire.
    anyhow::ensure!(url.starts_with("https://"), "lien de téléchargement non chiffré");

    Ok(Some(Release {
        version,
        notes: body["body"].as_str().unwrap_or_default().trim().to_string(),
        url,
        size: asset["size"].as_u64().unwrap_or(0),
    }))
}

/// Télécharge le nouvel exécutable à côté de l'ancien — même volume, donc le
/// remplacement se fera par un simple renommage, sans copie ni fenêtre où le
/// fichier serait à moitié écrit.
fn download(release: &Release, state: &Arc<Mutex<Status>>) -> anyhow::Result<PathBuf> {
    let staged = std::env::current_exe()?.with_extension("new");

    let response = ureq::get(&release.url).set("User-Agent", AGENT).call()?;
    let total = response
        .header("Content-Length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(release.size);
    anyhow::ensure!(total <= MAX_BYTES, "téléchargement anormalement gros ({total} octets)");

    let mut reader = response.into_reader().take(MAX_BYTES);
    let mut file = std::fs::File::create(&staged)?;
    let mut buffer = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])?;
        done += read as u64;
        *state.lock().unwrap() = Status::Downloading { done, total };
    }
    file.sync_all()?;
    drop(file);

    // Une connexion coupée en route laisse un exécutable tronqué, qui
    // remplacerait silencieusement un binaire qui marche par un qui plante.
    if total > 0 && done != total {
        let _ = std::fs::remove_file(&staged);
        anyhow::bail!("téléchargement incomplet ({done} sur {total} octets)");
    }

    Ok(staged)
}

/// Met le binaire téléchargé à la place du binaire courant.
fn install(staged: &Path) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let old = exe.with_extension("old");
    let _ = std::fs::remove_file(&old);

    std::fs::rename(&exe, &old).map_err(|e| {
        anyhow::anyhow!("écriture impossible dans {} : {e}", parent(&exe))
    })?;
    if let Err(e) = std::fs::rename(staged, &exe) {
        // Remettre l'ancien en place : une version dépassée vaut mieux
        // qu'un dossier d'installation sans exécutable.
        let _ = std::fs::rename(&old, &exe);
        anyhow::bail!("remplacement impossible : {e}");
    }
    Ok(())
}

/// Dossier d'un chemin, pour les messages d'erreur.
fn parent(path: &Path) -> String {
    path.parent().unwrap_or(path).display().to_string()
}

/// `a` est-il strictement postérieur à `b` ?
fn newer(a: &str, b: &str) -> bool {
    parts(a) > parts(b)
}

/// Découpe une version en trois nombres. La comparaison est numérique champ
/// par champ — comparer les chaînes classerait `0.10.0` avant `0.9.0`.
fn parts(version: &str) -> [u32; 3] {
    let mut out = [0u32; 3];
    // Un suffixe de pré-version (`-rc1`) ou de build (`+abc`) ne participe
    // pas à l'ordre : il n'est là que pour les humains.
    let core = version.split(['-', '+']).next().unwrap_or_default();
    for (slot, field) in out.iter_mut().zip(core.split('.')) {
        *slot = field.trim().parse().unwrap_or(0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::newer;

    #[test]
    fn ordre_numerique_pas_lexicographique() {
        assert!(newer("0.10.0", "0.9.0"));
        assert!(newer("1.0.0", "0.99.99"));
        assert!(newer("0.1.1", "0.1.0"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn suffixes_et_champs_manquants_tolérés() {
        assert!(newer("0.2", "0.1.9"));
        assert!(!newer("0.1.0-rc1", "0.1.0"));
        assert!(newer("0.2.0+build7", "0.1.0"));
        assert!(!newer("", "0.1.0"));
    }
}
