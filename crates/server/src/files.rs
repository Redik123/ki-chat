//! Partage de fichiers : upload authentifié (jeton voix de la session),
//! téléchargement par lien. Stockage plat dans data/files/.
//!
//! Le stock est borné dans les deux dimensions (âge et volume total) et purgé
//! périodiquement : un serveur privé tourne sur un petit VPS, et un disque
//! plein n'emporte pas que le partage de fichiers — le chat n'écrit plus son
//! historique, les comptes ne se sauvegardent plus.
//!
//! Les fichiers se servent **en flux**, avec `Content-Length` et `Range` :
//! vingt personnes qui ouvrent le même clip de 60 Mo ne font pas monter le
//! serveur de 1,2 Go, et un lecteur (ou un téléphone) peut reprendre où il
//! en était.

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rand::Rng;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

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

/// Un identifiant de fichier neuf : seize caractères hexadécimaux aléatoires.
pub fn nouvel_id() -> String {
    format!("{:016x}", rand::rng().random::<u64>())
}

/// Le type MIME d'un fichier d'après son nom, et s'il se montre dans le
/// navigateur (`inline`) ou se télécharge (`attachment`). Les images, les
/// vidéos et les fiches se lisent sur place — c'est ce qu'un téléphone
/// attend d'un lien vers un clip.
fn type_mime(name: &str) -> (&'static str, bool) {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "mp4" | "m4v" => ("video/mp4", true),
        "mov" => ("video/quicktime", true),
        "webm" => ("video/webm", true),
        "jpg" | "jpeg" => ("image/jpeg", true),
        "png" => ("image/png", true),
        "gif" => ("image/gif", true),
        "webp" => ("image/webp", true),
        "json" => ("application/json", true),
        _ => ("application/octet-stream", false),
    }
}

/// Ne garde que des caractères sûrs pour un nom de fichier.
pub fn sanitize(name: &str) -> String {
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
        return (
            StatusCode::FORBIDDEN,
            "tu n'as pas le droit de partager des fichiers",
        )
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
    let file_id = nouvel_id();
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
    // Une vidéo est mise de côté et convertie ; l'adresse rendue est celle
    // du MP4 à venir.
    let url = crate::medias::finaliser(&state, &file_id, &dir, &name);
    Json(serde_json::json!({ "url": url })).into_response()
}

/// GET /files/{id}/{name}
pub async fn download(
    State(state): State<Arc<AppState>>,
    Path((file_id, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // L'identifiant est un hex aléatoire de 16 caractères : on refuse tout
    // autre motif (pas de traversée de chemin possible).
    if file_id.len() != 16 || !file_id.chars().all(|c| c.is_ascii_hexdigit()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let name = sanitize(&name);
    let (mime, en_ligne) = type_mime(&name);
    let disposition = if en_ligne { "inline" } else { "attachment" };
    // Les fichiers partagés d'abord : tout ce qui est dans le dossier.
    let path = files_dir(&state).join(&file_id).join(&name);
    if tokio::fs::metadata(&path).await.is_ok_and(|m| m.is_file()) {
        return servir_fichier(&path, mime, disposition, &name, &headers).await;
    }
    // Puis les clips, par le même chemin — mais pas tout : la source avec
    // ses pistes séparées et le texte du titre restent dans le dossier
    // (`clips::fichier_servable`). L'identifiant est dans l'URL du message,
    // visible de tout le salon : « sans les voix des copains » doit tenir.
    let dossier = crate::clips::dossier(&state).join(&file_id);
    let (d, n) = (dossier.clone(), name.clone());
    let servable = tokio::task::spawn_blocking(move || crate::clips::fichier_servable(&d, &n))
        .await
        .unwrap_or(false);
    if servable {
        return servir_fichier(&dossier.join(&name), mime, disposition, &name, &headers).await;
    }
    StatusCode::NOT_FOUND.into_response()
}

/// Une demande `Range: bytes=a-b` (ou `a-`, ou `-n`) contre un fichier de
/// `total` octets : la tranche `[debut, fin]` à servir. `None` si l'en-tête
/// est absent ou n'est pas une plage d'octets simple (on sert alors tout),
/// `Some(Err(()))` si la plage sort du fichier (416).
pub(crate) fn tranche(range: Option<&str>, total: u64) -> Option<Result<(u64, u64), ()>> {
    let spec = range?.trim().strip_prefix("bytes=")?;
    // Plusieurs plages : on n'en sert qu'une, la première ; c'est permis.
    let premiere = spec.split(',').next()?.trim();
    let (a, b) = premiere.split_once('-')?;
    let (a, b) = (a.trim(), b.trim());
    if total == 0 {
        return Some(Err(()));
    }
    let plage = match (a.is_empty(), b.is_empty()) {
        // `-n` : les n derniers octets.
        (true, false) => {
            let n: u64 = b.parse().ok()?;
            if n == 0 {
                return Some(Err(()));
            }
            (total.saturating_sub(n), total - 1)
        }
        // `a-` : de a à la fin.
        (false, true) => {
            let debut: u64 = a.parse().ok()?;
            if debut >= total {
                return Some(Err(()));
            }
            (debut, total - 1)
        }
        (false, false) => {
            let (debut, fin): (u64, u64) = (a.parse().ok()?, b.parse().ok()?);
            if debut > fin || debut >= total {
                return Some(Err(()));
            }
            (debut, fin.min(total - 1))
        }
        (true, true) => return None,
    };
    Some(Ok(plage))
}

/// Sert un fichier en flux : `Content-Length`, `Accept-Ranges`, et une
/// tranche (206) si l'on en demande une. Le fichier n'est jamais lu entier
/// en mémoire — un clip de 60 Mo ouvert par vingt spectateurs coûte vingt
/// tampons de 64 Ko, pas 1,2 Go.
pub(crate) async fn servir_fichier(
    path: &FsPath,
    mime: &str,
    disposition: &str,
    nom: &str,
    headers: &HeaderMap,
) -> Response {
    let Ok(mut fichier) = tokio::fs::File::open(path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let total = match fichier.metadata().await {
        Ok(m) if m.is_file() => m.len(),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let (statut, debut, fin) = match tranche(range, total) {
        None => (StatusCode::OK, 0, total.saturating_sub(1)),
        Some(Ok((a, b))) => (StatusCode::PARTIAL_CONTENT, a, b),
        Some(Err(())) => {
            return (
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{total}"))],
            )
                .into_response();
        }
    };
    let longueur = if total == 0 { 0 } else { fin - debut + 1 };
    if debut > 0 && fichier.seek(std::io::SeekFrom::Start(debut)).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let flux = tokio_util::io::ReaderStream::with_capacity(fichier.take(longueur), 64 * 1024);
    let mut reponse = Response::builder()
        .status(statut)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, longueur)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(
            header::CONTENT_DISPOSITION,
            format!("{disposition}; filename=\"{nom}\""),
        );
    if statut == StatusCode::PARTIAL_CONTENT {
        reponse = reponse.header(header::CONTENT_RANGE, format!("bytes {debut}-{fin}/{total}"));
    }
    reponse
        .body(Body::from_stream(flux))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
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
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut stored = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let mut bytes = 0;
        let mut modified = SystemTime::UNIX_EPOCH;
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
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
        stored.push(Stored {
            dir,
            bytes,
            modified,
        });
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

        let (removed, freed) = sweep(
            &root,
            Quota {
                max_bytes: 0,
                ttl_days: 30,
            },
        );
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
        let (removed, freed) = sweep(
            &root,
            Quota {
                max_bytes: 2500,
                ttl_days: 0,
            },
        );
        assert_eq!((removed, freed), (1, 1000));
        assert!(!root.join("000000000000000a").exists());
        assert!(root.join("000000000000000b").exists());
        assert!(root.join("000000000000000c").exists());
        assert_eq!(used_bytes(&root), 2000);

        // Sous le plafond : une seconde passe ne touche plus à rien.
        assert_eq!(
            sweep(
                &root,
                Quota {
                    max_bytes: 2500,
                    ttl_days: 0
                }
            ),
            (0, 0)
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn une_plage_d_octets_se_lit_et_se_borne() {
        assert_eq!(tranche(None, 100), None);
        assert_eq!(tranche(Some("bytes=0-9"), 100), Some(Ok((0, 9))));
        assert_eq!(tranche(Some("bytes=10-"), 100), Some(Ok((10, 99))));
        assert_eq!(tranche(Some("bytes=-10"), 100), Some(Ok((90, 99))));
        // Une fin au-delà du fichier se ramène au dernier octet.
        assert_eq!(tranche(Some("bytes=90-500"), 100), Some(Ok((90, 99))));
        // Hors du fichier, ou à l'envers : 416.
        assert_eq!(tranche(Some("bytes=100-"), 100), Some(Err(())));
        assert_eq!(tranche(Some("bytes=20-10"), 100), Some(Err(())));
        assert_eq!(tranche(Some("bytes=0-"), 0), Some(Err(())));
        // Pas une plage d'octets : on sert tout.
        assert_eq!(tranche(Some("items=0-9"), 100), None);
        assert_eq!(tranche(Some("bytes=abc"), 100), None);
    }

    /// Le fichier part en flux, avec sa longueur ; une tranche donne un
    /// 206 avec `Content-Range` et juste ces octets-là.
    #[tokio::test]
    async fn un_fichier_se_sert_entier_ou_par_tranche() {
        let root = scratch("flux");
        let chemin = root.join("clip.mp4");
        let contenu: Vec<u8> = (0..=255u8).collect();
        std::fs::write(&chemin, &contenu).unwrap();
        let corps = |r: Response| async move {
            let (parts, body) = r.into_parts();
            let octets = axum::body::to_bytes(body, 1 << 20).await.unwrap();
            (parts.status, parts.headers, octets.to_vec())
        };

        let entier = servir_fichier(&chemin, "video/mp4", "inline", "clip.mp4", &HeaderMap::new()).await;
        let (statut, en_tetes, octets) = corps(entier).await;
        assert_eq!(statut, StatusCode::OK);
        assert_eq!(en_tetes[header::CONTENT_LENGTH], "256");
        assert_eq!(en_tetes[header::ACCEPT_RANGES], "bytes");
        assert_eq!(en_tetes[header::CONTENT_TYPE], "video/mp4");
        assert_eq!(octets, contenu);

        let mut h = HeaderMap::new();
        h.insert(header::RANGE, "bytes=10-19".parse().unwrap());
        let tranche = servir_fichier(&chemin, "video/mp4", "attachment", "x.mp4", &h).await;
        let (statut, en_tetes, octets) = corps(tranche).await;
        assert_eq!(statut, StatusCode::PARTIAL_CONTENT);
        assert_eq!(en_tetes[header::CONTENT_LENGTH], "10");
        assert_eq!(en_tetes[header::CONTENT_RANGE], "bytes 10-19/256");
        assert_eq!(octets, &contenu[10..20]);

        let mut h = HeaderMap::new();
        h.insert(header::RANGE, "bytes=-16".parse().unwrap());
        let fin = servir_fichier(&chemin, "video/mp4", "inline", "x.mp4", &h).await;
        let (statut, _, octets) = corps(fin).await;
        assert_eq!(statut, StatusCode::PARTIAL_CONTENT);
        assert_eq!(octets, &contenu[240..]);

        let mut h = HeaderMap::new();
        h.insert(header::RANGE, "bytes=300-".parse().unwrap());
        let hors = servir_fichier(&chemin, "video/mp4", "inline", "x.mp4", &h).await;
        let (statut, en_tetes, _) = corps(hors).await;
        assert_eq!(statut, StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(en_tetes[header::CONTENT_RANGE], "bytes */256");

        let absent = servir_fichier(&root.join("rien.mp4"), "video/mp4", "inline", "x", &HeaderMap::new()).await;
        assert_eq!(absent.status(), StatusCode::NOT_FOUND);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Sans borne configurée, la purge ne supprime rien — même très vieux.
    #[test]
    fn sweep_keeps_everything_when_unbounded() {
        let root = scratch("illimite");
        plant(&root, "000000000000000f", 2048, days(400));

        let quota = Quota {
            max_bytes: 0,
            ttl_days: 0,
        };
        assert!(!quota.enabled());
        assert_eq!(sweep(&root, quota), (0, 0));
        assert_eq!(used_bytes(&root), 2048);

        std::fs::remove_dir_all(&root).unwrap();
    }
}
