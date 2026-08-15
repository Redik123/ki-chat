//! Partage de fichiers : upload authentifié (jeton voix de la session),
//! téléchargement par lien. Stockage plat dans data/files/.

use std::path::PathBuf;
use std::sync::Arc;

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
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "fichier vide").into_response();
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
