//! Partage de fichiers : upload authentifié (jeton voix de la session),
//! téléchargement par lien. Stockage plat dans data/files/.
//!
//! Le stock est borné dans les deux dimensions (âge et volume total) et purgé
//! périodiquement : un serveur privé tourne sur un petit VPS, et un disque
//! plein n'emporte pas que le partage de fichiers — le chat n'écrit plus son
//! historique, les comptes ne se sauvegardent plus.

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use rand::Rng;
use serde::Deserialize;

use crate::state::AppState;

/// Taille max d'un fichier : 25 Mo (aligné sur la limite du routeur).
pub const MAX_FILE_SIZE: usize = 25 * 1024 * 1024;

/// Plafond global par défaut : 2 Gio. De quoi tenir des mois d'échanges à
/// trente, sans risquer le disque d'un VPS d'entrée de gamme.
pub const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Durée de vie par défaut : 30 jours. Un fichier partagé dans le chat sert
/// dans l'heure ; passé un mois, plus personne ne remonte le chercher.
pub const DEFAULT_TTL_DAYS: u64 = 30;

/// Périodicité de la purge. Assez rare pour ne rien coûter, assez fréquente
/// pour qu'un plafond dépassé ne le reste pas une journée entière.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(3600);

/// Bornes du stock de fichiers, réglées par variables d'environnement.
#[derive(Clone, Copy)]
pub struct Quota {
    /// Volume total autorisé dans `data/files/`. 0 = illimité.
    pub max_bytes: u64,
    /// Âge au-delà duquel un fichier est supprimé. 0 = pas de purge par âge.
    pub ttl_days: u64,
}

impl Quota {
    /// Vrai si au moins une des deux bornes est active : inutile de faire
    /// tourner une tâche de fond qui ne supprimerait jamais rien.
    pub fn enabled(&self) -> bool {
        self.max_bytes > 0 || self.ttl_days > 0
    }
}

#[derive(Deserialize)]
pub struct UploadParams {
    name: String,
}

fn files_dir(state: &AppState) -> PathBuf {
    PathBuf::from(&state.data_dir).join("files")
}

/// Ne garde que des caractères sûrs pour un nom de fichier.
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('.').to_string();
    if trimmed.is_empty() {
        "fichier".into()
    } else {
        trimmed.chars().take(120).collect()
    }
}

/// POST /upload?name=<nom> — corps brut, en-tête x-ki-token = jeton voix (hex).
pub async fn upload(
    State(state): State<Arc<AppState>>,
    Query(params): Query<UploadParams>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // Authentification : le jeton voix n'est connu que d'un client connecté.
    let token = headers
        .get("x-ki-token")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| u64::from_str_radix(s, 16).ok());
    let Some((user_id, username)) = token.and_then(|t| state.user_by_voice_token(t)) else {
        return (StatusCode::UNAUTHORIZED, "jeton invalide").into_response();
    };
    // Le partage de fichiers est une permission comme une autre : elle
    // s'affiche dans l'éditeur de rôles, elle doit donc être appliquée. Ce
    // chemin passe par HTTP et non par le flux de contrôle, d'où le contrôle
    // ici plutôt que dans `handle_msg`.
    if !state.holds(user_id, ki_protocol::perm::UPLOAD_FILE) {
        return (StatusCode::FORBIDDEN, "tu n'as pas le droit de partager des fichiers")
            .into_response();
    }
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "fichier vide").into_response();
    }

    // Le plafond global se vérifie AVANT d'écrire : accepter puis constater
    // que le disque est plein, c'est arrêter le serveur entier pour un
    // partage de fichier. Le parcours du stock part sur le pool bloquant.
    let max_bytes = state.files_quota.max_bytes;
    if max_bytes > 0 {
        let root = files_dir(&state);
        let used = tokio::task::spawn_blocking(move || used_bytes(&root))
            .await
            .unwrap_or(0);
        if used.saturating_add(body.len() as u64) > max_bytes {
            tracing::warn!("upload refusé : stock plein ({} Mo)", used / (1024 * 1024));
            return (
                StatusCode::INSUFFICIENT_STORAGE,
                "espace de partage saturé sur le serveur — préviens un admin",
            )
                .into_response();
        }
    }

    let name = sanitize(&params.name);
    let file_id = format!("{:016x}", rand::rng().random::<u64>());
    let dir = files_dir(&state).join(&file_id);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        tracing::error!("création dossier fichiers : {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, "stockage indisponible").into_response();
    }
    if let Err(e) = tokio::fs::write(dir.join(&name), &body).await {
        tracing::error!("écriture fichier : {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, "stockage indisponible").into_response();
    }
    tracing::info!(
        "fichier reçu : {name} ({} Ko) de {username} (id {user_id})",
        body.len() / 1024
    );
    Json(serde_json::json!({ "url": format!("/files/{file_id}/{name}") })).into_response()
}

/// GET /files/{id}/{name}
pub async fn download(
    State(state): State<Arc<AppState>>,
    Path((file_id, name)): Path<(String, String)>,
) -> impl IntoResponse {
    // L'identifiant est un hex aléatoire de 16 caractères : on refuse tout
    // autre motif (pas de traversée de chemin possible).
    if file_id.len() != 16 || !file_id.chars().all(|c| c.is_ascii_hexdigit()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let name = sanitize(&name);
    let path = files_dir(&state).join(&file_id).join(&name);
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{name}\""),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Un fichier partagé tel qu'il vit sur le disque : la disposition est
/// `data/files/{id}/{nom}`, donc c'est le **dossier** qui est l'unité de
/// suppression — n'effacer que le fichier laisserait un dossier vide par
/// partage, et il en resterait des milliers.
struct Stored {
    dir: PathBuf,
    bytes: u64,
    /// Date du contenu, pas du dossier : sous Windows comme sous Linux, la
    /// date d'un dossier n'est pas fiable après une copie du volume de données.
    modified: SystemTime,
}

/// Inventaire du stock. Un dossier illisible est ignoré plutôt que
/// rapporté : la purge tourne sans surveillance, elle ne doit pas s'arrêter
/// sur un dossier de plus.
fn scan(root: &FsPath) -> Vec<Stored> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut stored = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let mut bytes = 0;
        let mut modified = SystemTime::UNIX_EPOCH;
        let Ok(files) = std::fs::read_dir(&dir) else { continue };
        for file in files.flatten() {
            let Ok(meta) = file.metadata() else { continue };
            bytes += meta.len();
            if let Ok(at) = meta.modified() {
                modified = modified.max(at);
            }
        }
        // Dossier vide ou dates illisibles : compté comme neuf. Mieux vaut
        // garder un partage de trop que supprimer celui d'hier.
        if modified == SystemTime::UNIX_EPOCH {
            modified = SystemTime::now();
        }
        stored.push(Stored { dir, bytes, modified });
    }
    stored
}

/// Volume total occupé par le partage de fichiers.
pub fn used_bytes(root: &FsPath) -> u64 {
    scan(root).iter().map(|s| s.bytes).sum()
}

/// Une passe de purge : d'abord ce qui a dépassé le TTL, puis, tant que le
/// plafond global est franchi, le plus ancien. Renvoie (fichiers supprimés,
/// octets libérés).
pub fn sweep(root: &FsPath, quota: Quota) -> (usize, u64) {
    let mut stored = scan(root);
    // Du plus ancien au plus récent : c'est l'ordre dans lequel on sacrifie.
    stored.sort_by_key(|s| s.modified);
    let mut total: u64 = stored.iter().map(|s| s.bytes).sum();
    let deadline = (quota.ttl_days > 0)
        .then(|| SystemTime::now().checked_sub(Duration::from_secs(quota.ttl_days * 86_400)))
        .flatten();

    let (mut removed, mut freed) = (0usize, 0u64);
    for entry in &stored {
        let too_old = deadline.is_some_and(|limit| entry.modified < limit);
        let over_quota = quota.max_bytes > 0 && total > quota.max_bytes;
        // La liste est triée : si celui-ci est assez jeune et que le volume
        // tient, tous les suivants aussi. Rien ne sert d'aller plus loin.
        if !too_old && !over_quota {
            break;
        }
        if let Err(e) = std::fs::remove_dir_all(&entry.dir) {
            tracing::error!("purge de {} impossible : {e}", entry.dir.display());
            continue;
        }
        total -= entry.bytes;
        removed += 1;
        freed += entry.bytes;
    }
    if removed > 0 {
        tracing::info!(
            "purge du partage : {removed} fichier(s) supprimé(s), {} Ko libérés, {} Ko restants",
            freed / 1024,
            total / 1024
        );
    }
    (removed, freed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dossier de travail jetable, propre à chaque test.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ki-chat-files-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Dépose un partage de `bytes` octets, daté d'il y a `age`.
    fn plant(root: &FsPath, id: &str, bytes: usize, age: Duration) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("capture.png");
        std::fs::write(&path, vec![0u8; bytes]).unwrap();
        // C'est la date du fichier, et non celle du dossier, qui décide.
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(SystemTime::now() - age).unwrap();
    }

    fn days(n: u64) -> Duration {
        Duration::from_secs(n * 86_400)
    }

    #[test]
    fn sweep_removes_whole_folders_past_the_ttl() {
        let root = scratch("ttl");
        plant(&root, "0000000000000001", 4096, days(40));
        plant(&root, "0000000000000002", 4096, days(2));

        let (removed, freed) = sweep(&root, Quota { max_bytes: 0, ttl_days: 30 });
        assert_eq!(removed, 1);
        assert_eq!(freed, 4096);
        // Le dossier entier part, pas seulement le fichier : sinon il
        // resterait un dossier vide par partage purgé.
        assert!(!root.join("0000000000000001").exists());
        assert!(root.join("0000000000000002").join("capture.png").exists());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn sweep_evicts_the_oldest_until_it_fits() {
        let root = scratch("quota");
        plant(&root, "000000000000000a", 1000, days(3));
        plant(&root, "000000000000000b", 1000, days(2));
        plant(&root, "000000000000000c", 1000, days(1));

        // 3000 octets pour un plafond de 2500 : le plus ancien seul suffit.
        let (removed, freed) = sweep(&root, Quota { max_bytes: 2500, ttl_days: 0 });
        assert_eq!((removed, freed), (1, 1000));
        assert!(!root.join("000000000000000a").exists());
        assert!(root.join("000000000000000b").exists());
        assert!(root.join("000000000000000c").exists());
        assert_eq!(used_bytes(&root), 2000);

        // Sous le plafond : une seconde passe ne touche plus à rien.
        assert_eq!(sweep(&root, Quota { max_bytes: 2500, ttl_days: 0 }), (0, 0));

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Sans borne configurée, la purge ne supprime rien — même très vieux.
    #[test]
    fn sweep_keeps_everything_when_unbounded() {
        let root = scratch("illimite");
        plant(&root, "000000000000000f", 2048, days(400));

        let quota = Quota { max_bytes: 0, ttl_days: 0 };
        assert!(!quota.enabled());
        assert_eq!(sweep(&root, quota), (0, 0));
        assert_eq!(used_bytes(&root), 2048);

        std::fs::remove_dir_all(&root).unwrap();
    }
}
