//! Les vidéos partagées, côté serveur (PLAN-CLIPS.md, jalon C0).
//!
//! Deux choses vivent ici :
//!
//! - **Le téléversement par morceaux.** Le partage d'un bloc plafonne à 25 Mo
//!   (la limite du routeur devant le serveur) ; une vidéo de téléphone ou un
//!   clip en font le double ou le triple. Le client envoie donc des morceaux
//!   de 8 Mo au plus, chacun sous la limite, puis demande l'assemblage.
//! - **La normalisation.** Toute vidéo reçue est refaite par ffmpeg en MP4
//!   H.264 + AAC, 1080p au plus, `+faststart`, rotation de téléphone
//!   appliquée — avec un poster JPEG et une fiche `meta.json` à côté. Ainsi
//!   les HEVC d'iPhone, les WebM, les AVI d'un autre âge deviennent lisibles
//!   par le même décodeur chez tout le monde. Un ffmpeg à la fois, sur le
//!   pool bloquant, jamais sur la boucle qui relaie la voix.
//!
//! La fiche est ce que le client lit pour la carte dans le fil : tant qu'elle
//! dit « en préparation », il patiente et redemande.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;

use crate::files;
use crate::musique::executer_borne;
use crate::state::AppState;

/// Taille maximale d'un morceau. Le routeur coupe à 25 Mo : huit laissent
/// de la marge, et une barre de progression qui bouge.
pub const MORCEAU_MAX: usize = 8 * 1024 * 1024;
/// Taille maximale d'un fichier assemblé, par défaut (`KI_FILES_MAX_FILE_MB`).
pub const DEFAULT_FICHIER_MAX_MB: u64 = 512;
/// Nombre maximal de morceaux : 512 Mo à 8 Mo le morceau.
pub(crate) const MORCEAUX_MAX: u32 = 64;
/// Un téléversement commencé et jamais terminé est jeté après ça.
pub const PARTIEL_AGE_MAX: Duration = Duration::from_secs(3600);
/// Temps accordé à ffmpeg pour une conversion.
const CONVERSION_MAX: Duration = Duration::from_secs(900);
/// Au-delà, on réencode : un clip déjà propre en dessous passe tel quel.
/// Le débit compté est celui de la piste vidéo quand le fichier le dit —
/// un clip « équilibré » de l'enregistreur vise 12 Mbit/s et les frôle.
const DEBIT_COPIE_MAX: u64 = 13_000_000;

/// Extensions traitées comme des vidéos.
const VIDEO_SUFFIXES: [&str; 6] = [".mp4", ".mov", ".webm", ".mkv", ".m4v", ".avi"];

pub fn est_video(nom: &str) -> bool {
    let bas = nom.to_lowercase();
    VIDEO_SUFFIXES.iter().any(|s| bas.ends_with(s))
}

/// Ce que le serveur écrit dans `meta.json`, à côté de la vidéo.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Meta {
    /// `en_preparation`, `pret` ou `erreur`.
    pub etat: String,
    #[serde(default)]
    pub duree_s: f32,
    #[serde(default)]
    pub largeur: u32,
    #[serde(default)]
    pub hauteur: u32,
    /// Chemin du poster (« /files/<id>/poster.jpg »).
    #[serde(default)]
    pub poster: Option<String>,
    #[serde(default)]
    pub taille: u64,
    #[serde(default)]
    pub message: Option<String>,
    /// Le fichier reçu, tant qu'il n'est pas converti (pour reprendre après
    /// un redémarrage).
    #[serde(default)]
    pub source: Option<String>,
    /// Le nom du MP4 à produire.
    #[serde(default)]
    pub sortie: Option<String>,
    /// Un clip partagé (`clips.rs`) plutôt qu'une vidéo ordinaire : la
    /// version partagée ne porte qu'une piste son.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clip: bool,
    /// Garder la source après conversion (l'atelier en aura besoin).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub garder_source: bool,
    /// Qui a partagé, où, avec quelle légende, sous quel nom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auteur: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salon: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legende: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nom: Option<String>,
    /// Les pistes de la source après le mélange, dans l'ordre du fichier ;
    /// `None` = inconnues.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pistes: Option<Vec<String>>,
    /// Garder les voix des copains dans la version partagée.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voix: Option<bool>,
    /// Les messages du fil qui portent ce clip (salon, horodatage) : à
    /// effacer avec lui.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<(u32, u64)>,
}

/// Les outils, trouvés au démarrage.
#[derive(Clone)]
pub struct Outils {
    pub(crate) ffmpeg: String,
    pub(crate) ffprobe: String,
}

/// Ce qu'il y a à faire dans un dossier : convertir la vidéo reçue, ou
/// exporter un clip d'après une recette (`export.rs`).
pub(crate) enum Tache {
    Normaliser,
    Exporter(crate::export::Recette),
}

/// Une tâche en file : son dossier, dans `data/files/` ou `data/clips/`.
struct Travail {
    dossier: PathBuf,
    tache: Tache,
}

/// La fabrique : la file des conversions, et les outils s'ils existent.
pub struct Fabrique {
    outils: Option<Outils>,
    file: Mutex<Vec<Travail>>,
    reveil: tokio::sync::Notify,
    /// Plafond d'un fichier assemblé, en octets.
    pub fichier_max: u64,
}

impl Fabrique {
    pub fn new(fichier_max_mb: u64) -> Self {
        let outils = detecter();
        if outils.is_none() {
            tracing::info!(
                "médias : ffmpeg ou ffprobe introuvable — les vidéos partagées ne seront pas converties"
            );
        }
        Self {
            outils,
            file: Mutex::new(Vec::new()),
            reveil: tokio::sync::Notify::new(),
            fichier_max: fichier_max_mb.saturating_mul(1024 * 1024),
        }
    }

    pub fn disponible(&self) -> bool {
        self.outils.is_some()
    }

    pub(crate) fn deposer(&self, dossier: PathBuf) {
        self.file.lock().unwrap().push(Travail { dossier, tache: Tache::Normaliser });
        self.reveil.notify_one();
    }

    /// Un export de clip, d'après une recette déjà validée.
    pub(crate) fn deposer_export(&self, dossier: PathBuf, recette: crate::export::Recette) {
        self.file.lock().unwrap().push(Travail { dossier, tache: Tache::Exporter(recette) });
        self.reveil.notify_one();
    }

    pub(crate) fn outils(&self) -> Option<Outils> {
        self.outils.clone()
    }
}

pub(crate) fn detecter() -> Option<Outils> {
    let ffmpeg = std::env::var("KI_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    let ffprobe = std::env::var("KI_FFPROBE").unwrap_or_else(|_| {
        // À côté de ffmpeg quand il est donné par son chemin.
        let p = Path::new(&ffmpeg);
        match p.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => {
                let nom = if ffmpeg.ends_with(".exe") {
                    "ffprobe.exe"
                } else {
                    "ffprobe"
                };
                dir.join(nom).to_string_lossy().into_owned()
            }
            _ => "ffprobe".into(),
        }
    });
    let version = |exe: &str| -> Option<String> {
        let sortie =
            executer_borne(Command::new(exe).arg("-version"), Duration::from_secs(20)).ok()?;
        let texte = String::from_utf8_lossy(&sortie);
        texte.lines().next().map(|l| l.chars().take(60).collect())
    };
    let (v1, v2) = (version(&ffmpeg)?, version(&ffprobe)?);
    tracing::info!("médias : {v1} · {v2}");
    Some(Outils { ffmpeg, ffprobe })
}

// ---------------------------------------------------------------------
// Téléversement par morceaux
// ---------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ParamsMorceau {
    upload: String,
    index: u32,
}

#[derive(Deserialize)]
pub struct ParamsFin {
    upload: String,
    name: String,
    parts: u32,
}

/// Identifiant de téléversement : hexadécimal, 8 à 32 caractères, choisi
/// par le client. Il ne désigne jamais rien hors de son propre dossier.
pub(crate) fn upload_valide(id: &str) -> bool {
    (8..=32).contains(&id.len()) && id.chars().all(|c| c.is_ascii_hexdigit())
}

pub(crate) fn dossier_partiel(state: &AppState, user_id: u64, upload: &str) -> PathBuf {
    PathBuf::from(&state.data_dir)
        .join("upload-partiel")
        .join(user_id.to_string())
        .join(upload)
}

/// Qui envoie : le jeton voix de la session, et le droit de partager.
pub(crate) fn authentifier(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(u64, String), (StatusCode, &'static str)> {
    let token = headers
        .get("x-ki-token")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| u64::from_str_radix(s, 16).ok());
    let Some((user_id, username)) = token.and_then(|t| state.user_by_voice_token(t)) else {
        return Err((StatusCode::UNAUTHORIZED, "jeton invalide"));
    };
    if !state.holds(user_id, ki_protocol::perm::UPLOAD_FILE) {
        return Err((
            StatusCode::FORBIDDEN,
            "tu n'as pas le droit de partager des fichiers",
        ));
    }
    Ok((user_id, username))
}

/// `POST /upload/partiel?upload=<id>&index=<n>` : un morceau.
pub async fn upload_partiel(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ParamsMorceau>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let (user_id, _) = match authentifier(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r.into_response(),
    };
    if !upload_valide(&params.upload) || params.index >= MORCEAUX_MAX {
        return (StatusCode::BAD_REQUEST, "téléversement invalide").into_response();
    }
    if body.is_empty() || body.len() > MORCEAU_MAX {
        return (StatusCode::BAD_REQUEST, "morceau vide ou trop gros").into_response();
    }
    let dossier = dossier_partiel(&state, user_id, &params.upload);
    let max = state.medias.fichier_max;
    let ecrit = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        std::fs::create_dir_all(&dossier)?;
        // Le total reçu jusqu'ici, ce morceau compris, reste sous le plafond.
        let deja: u64 = std::fs::read_dir(&dossier)?
            .flatten()
            .filter_map(|e| e.metadata().ok().map(|m| m.len()))
            .sum();
        if deja + body.len() as u64 > max {
            let _ = std::fs::remove_dir_all(&dossier);
            return Err(std::io::Error::other("trop gros"));
        }
        std::fs::write(dossier.join(format!("{:05}", params.index)), &body)
    })
    .await;
    match ecrit {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) if e.to_string().contains("trop gros") => (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("fichier trop gros ({} Mo max)", max / (1024 * 1024)),
        )
            .into_response(),
        Ok(Err(e)) => {
            tracing::error!("morceau non écrit : {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "stockage indisponible").into_response()
        }
        Err(e) => {
            tracing::error!("morceau : {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "stockage indisponible").into_response()
        }
    }
}

/// `POST /upload/fin?upload=<id>&name=<nom>&parts=<n>` : assemble, range,
/// et lance la conversion si c'est une vidéo. Rend `{ "url": "/files/…" }`.
pub async fn upload_fin(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ParamsFin>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let (user_id, username) = match authentifier(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r.into_response(),
    };
    if !upload_valide(&params.upload) || params.parts == 0 || params.parts > MORCEAUX_MAX {
        return (StatusCode::BAD_REQUEST, "téléversement invalide").into_response();
    }
    let partiel = dossier_partiel(&state, user_id, &params.upload);
    let nom = files::sanitize(&params.name);
    let nom_ferme = nom.clone();
    let racine = PathBuf::from(&state.data_dir).join("files");
    let quota = state.files_quota.max_bytes;
    let parts = params.parts;
    let assemble =
        tokio::task::spawn_blocking(move || assembler(&partiel, parts, &racine, quota, &nom_ferme))
            .await;
    let (file_id, dossier, total) = match assemble {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => {
            let code = if e.contains("saturé") {
                StatusCode::INSUFFICIENT_STORAGE
            } else {
                StatusCode::BAD_REQUEST
            };
            return (code, e).into_response();
        }
        Err(e) => {
            tracing::error!("assemblage : {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "stockage indisponible").into_response();
        }
    };
    tracing::info!(
        "fichier reçu par morceaux : {nom} ({} Ko) de {username} (id {user_id})",
        total / 1024
    );
    let url = finaliser(&state, &file_id, &dossier, &nom);
    Json(serde_json::json!({ "url": url })).into_response()
}

/// Assemble les morceaux d'un téléversement dans `racine/<id neuf>/<nom>`,
/// sous le plafond du stock (0 = sans plafond). Rend l'identifiant, le
/// dossier et la taille. Le dossier des morceaux est jeté dans tous les cas.
pub(crate) fn assembler(
    partiel: &Path,
    parts: u32,
    racine: &Path,
    quota: u64,
    nom: &str,
) -> Result<(String, PathBuf, u64), String> {
    let mut total: u64 = 0;
    let mut morceaux = Vec::with_capacity(parts as usize);
    for i in 0..parts {
        let m = partiel.join(format!("{i:05}"));
        let taille = std::fs::metadata(&m)
            .map_err(|_| "il manque un morceau".to_string())?
            .len();
        total += taille;
        morceaux.push(m);
    }
    if quota > 0 && files::used_bytes(racine).saturating_add(total) > quota {
        let _ = std::fs::remove_dir_all(partiel);
        return Err("espace de partage saturé sur le serveur — préviens un admin".into());
    }
    let file_id = files::nouvel_id();
    let dossier = racine.join(&file_id);
    std::fs::create_dir_all(&dossier).map_err(|e| e.to_string())?;
    let cible = dossier.join(nom);
    {
        let mut sortie = std::fs::File::create(&cible).map_err(|e| e.to_string())?;
        for m in &morceaux {
            let mut entree = std::fs::File::open(m).map_err(|e| e.to_string())?;
            std::io::copy(&mut entree, &mut sortie).map_err(|e| e.to_string())?;
        }
    }
    let _ = std::fs::remove_dir_all(partiel);
    Ok((file_id, dossier, total))
}

/// Un fichier vient d'être rangé dans son dossier : s'il s'agit d'une
/// vidéo, on le met de côté et l'on prépare sa conversion. Rend l'adresse à
/// écrire dans le chat — celle du MP4 à venir, pour une vidéo.
pub fn finaliser(state: &AppState, file_id: &str, dossier: &Path, nom: &str) -> String {
    if !est_video(nom) || !state.medias.disponible() {
        return format!("/files/{file_id}/{nom}");
    }
    let racine = Path::new(nom)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned());
    let racine = racine
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "video".into());
    let sortie = format!("{racine}.mp4");
    let ext = Path::new(nom)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_else(|| "bin".into());
    let source = format!("source.{ext}");
    if let Err(e) = std::fs::rename(dossier.join(nom), dossier.join(&source)) {
        tracing::error!("vidéo non mise de côté : {e}");
        return format!("/files/{file_id}/{nom}");
    }
    let meta = Meta {
        etat: "en_preparation".into(),
        source: Some(source),
        sortie: Some(sortie.clone()),
        ..Default::default()
    };
    ecrire_meta(dossier, &meta);
    state.medias.deposer(dossier.to_path_buf());
    format!("/files/{file_id}/{sortie}")
}

pub(crate) fn ecrire_meta(dossier: &Path, meta: &Meta) {
    if let Ok(json) = serde_json::to_vec_pretty(meta) {
        if let Err(e) = crate::store::write_atomic(&dossier.join("meta.json"), &json) {
            tracing::error!("meta.json non écrit : {e}");
        }
    }
}

pub(crate) fn lire_meta(dossier: &Path) -> Option<Meta> {
    let bytes = std::fs::read(dossier.join("meta.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

// ---------------------------------------------------------------------
// La fabrique
// ---------------------------------------------------------------------

/// Au démarrage : les vidéos laissées « en préparation » par un arrêt
/// repassent en file.
pub fn reprendre(state: &AppState) {
    reprendre_dans(state, &PathBuf::from(&state.data_dir).join("files"));
}

/// Idem pour un stock donné (les fichiers, ou les clips).
pub(crate) fn reprendre_dans(state: &AppState, racine: &Path) {
    let Ok(entrees) = std::fs::read_dir(racine) else {
        return;
    };
    let mut n = 0;
    for e in entrees.flatten() {
        let dossier = e.path();
        if let Some(m) = lire_meta(&dossier) {
            if m.etat == "en_preparation"
                && m.source.as_ref().is_some_and(|s| dossier.join(s).is_file())
            {
                state.medias.deposer(dossier);
                n += 1;
            }
        }
    }
    if n > 0 {
        tracing::info!("médias : {n} vidéo(s) à reprendre dans {}", racine.display());
    }
}

/// La tâche de fond : une conversion à la fois.
pub async fn boucle(state: Arc<AppState>) {
    let Some(outils) = state.medias.outils.clone() else {
        return;
    };
    loop {
        let travail = state.medias.file.lock().unwrap().pop();
        let Some(travail) = travail else {
            state.medias.reveil.notified().await;
            continue;
        };
        let o = outils.clone();
        let dossier = travail.dossier.clone();
        match travail.tache {
            Tache::Normaliser => {
                let resultat = tokio::task::spawn_blocking(move || normaliser(&o, &dossier)).await;
                match resultat {
                    Ok(Ok(meta)) => tracing::info!(
                        "médias : {} prête ({}x{}, {:.1} s, {} Ko)",
                        travail.dossier.display(),
                        meta.largeur,
                        meta.hauteur,
                        meta.duree_s,
                        meta.taille / 1024
                    ),
                    Ok(Err(e)) => tracing::warn!("médias : {} : {e}", travail.dossier.display()),
                    Err(e) => tracing::error!("médias : tâche interrompue : {e}"),
                }
            }
            Tache::Exporter(recette) => {
                let resultat =
                    tokio::task::spawn_blocking(move || crate::export::executer(&o, &dossier, &recette)).await;
                match resultat {
                    Ok(Ok(e)) => tracing::info!(
                        "export : {} prêt ({}x{}, {:.1} s, {} Ko)",
                        travail.dossier.display(),
                        e.largeur,
                        e.hauteur,
                        e.duree_s,
                        e.taille / 1024
                    ),
                    Ok(Err(e)) => tracing::warn!("export : {} : {e}", travail.dossier.display()),
                    Err(e) => tracing::error!("export : tâche interrompue : {e}"),
                }
            }
        }
    }
}

/// Ce que ffprobe dit d'un fichier.
#[derive(Default)]
pub(crate) struct Sonde {
    pub(crate) duree_s: f32,
    pub(crate) debit: u64,
    /// Le débit de la piste vidéo seule, si le conteneur le porte (MP4) :
    /// c'est lui qui décide de la copie, pas le total avec quatre pistes son.
    pub(crate) debit_video: u64,
    pub(crate) conteneur: String,
    /// Codec, largeur, hauteur de la première piste vidéo.
    pub(crate) video: Option<(String, u32, u32)>,
    pub(crate) audio: Option<String>,
    /// Nombre de pistes son.
    pub(crate) pistes_audio: u32,
    /// Images par seconde de la piste vidéo (0 si inconnue).
    pub(crate) cadence: f32,
}

/// « 60/1 », « 30000/1001 » : la cadence que ffprobe écrit en fraction.
fn cadence_de(fraction: &str) -> Option<f32> {
    let (num, den) = fraction.split_once('/')?;
    let (num, den): (f32, f32) = (num.parse().ok()?, den.parse().ok()?);
    (den > 0.0 && num > 0.0).then_some(num / den)
}

pub(crate) fn sonder(outils: &Outils, source: &Path) -> Result<Sonde, String> {
    let sortie = executer_borne(
        Command::new(&outils.ffprobe)
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration,bit_rate,format_name:stream=codec_type,codec_name,width,height,bit_rate,r_frame_rate",
                "-of",
                "json",
            ])
            .arg(source),
        Duration::from_secs(60),
    )?;
    let v: serde_json::Value =
        serde_json::from_slice(&sortie).map_err(|e| format!("ffprobe : {e}"))?;
    let mut sonde = Sonde::default();
    if let Some(f) = v.get("format") {
        sonde.duree_s = f["duration"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);
        sonde.debit = f["bit_rate"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        sonde.conteneur = f["format_name"].as_str().unwrap_or("").to_string();
    }
    for s in v["streams"].as_array().into_iter().flatten() {
        let codec = s["codec_name"].as_str().unwrap_or("").to_string();
        match s["codec_type"].as_str() {
            Some("video") if sonde.video.is_none() => {
                let w = s["width"].as_u64().unwrap_or(0) as u32;
                let h = s["height"].as_u64().unwrap_or(0) as u32;
                sonde.debit_video = s["bit_rate"].as_str().and_then(|b| b.parse().ok()).unwrap_or(0);
                sonde.cadence = s["r_frame_rate"].as_str().and_then(cadence_de).unwrap_or(0.0);
                sonde.video = Some((codec, w, h));
            }
            Some("audio") => {
                sonde.pistes_audio += 1;
                if sonde.audio.is_none() {
                    sonde.audio = Some(codec);
                }
            }
            _ => {}
        }
    }
    Ok(sonde)
}

/// Comment le son de la version partagée est composé. Une vidéo ordinaire
/// garde tout ; un clip ne livre qu'une piste — le mélange, ou un mélange
/// refait sans les voix des copains — : les pistes séparées ne quittent
/// pas le dossier du clip.
#[derive(Debug, PartialEq, Eq)]
enum Son {
    /// Vidéo ordinaire : tout ce que la source a.
    Tout,
    /// Une piste de la source, copiée (`0:a:N`).
    Piste(u32),
    /// Un mélange refait de ces pistes de la source.
    Melange(Vec<u32>),
    /// Muet.
    Aucun,
}

fn plan_son(meta: &Meta) -> Son {
    if !meta.clip {
        return Son::Tout;
    }
    // La source d'un clip : le mélange en première piste, puis chaque
    // source dans l'ordre de `pistes` — quand le client les connaît.
    let pistes = meta.pistes.clone().unwrap_or_default();
    let copains = pistes.iter().position(|p| p == "copains");
    if meta.pistes.as_ref().is_some_and(|p| p.is_empty()) {
        return Son::Aucun;
    }
    if meta.voix.unwrap_or(true) || copains.is_none() {
        return Son::Piste(0);
    }
    let gardees: Vec<u32> = pistes
        .iter()
        .enumerate()
        .filter(|(_, p)| *p != "copains")
        .map(|(i, _)| i as u32 + 1)
        .collect();
    match gardees.len() {
        0 => Son::Aucun,
        1 => Son::Piste(gardees[0]),
        _ => Son::Melange(gardees),
    }
}

/// Les arguments vidéo d'un réencodage : x264 rapide, 1080p au plus.
const VIDEO_X264: [&str; 17] = [
    "-c:v", "libx264", "-preset", "veryfast", "-crf", "23",
    "-maxrate", "8M", "-bufsize", "16M", "-profile:v", "high", "-bf", "0",
    "-pix_fmt", "yuv420p",
    "-vf",
];
const ECHELLE_1080: &str =
    "scale='min(1920,iw)':'min(1920,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2";

/// Refait la vidéo du dossier en MP4 lisible partout, avec poster et fiche.
fn normaliser(outils: &Outils, dossier: &Path) -> Result<Meta, String> {
    let mut meta = lire_meta(dossier).ok_or("fiche absente")?;
    let source = meta.source.clone().ok_or("fiche sans source")?;
    let sortie = meta.sortie.clone().ok_or("fiche sans sortie")?;
    let chemin_source = dossier.join(&source);
    let chemin_sortie = dossier.join(&sortie);
    let resultat = (|| -> Result<(), String> {
        let sonde = sonder(outils, &chemin_source)?;
        let (codec_v, w, h) = sonde.video.clone().ok_or("aucune piste vidéo")?;
        let debit = if sonde.debit_video > 0 { sonde.debit_video } else { sonde.debit };
        let copie = sonde.conteneur.contains("mp4")
            && codec_v == "h264"
            && sonde.audio.as_deref().is_none_or(|a| a == "aac")
            && w <= 1920
            && h <= 1920
            && debit > 0
            && debit <= DEBIT_COPIE_MAX;
        let mut cmd = Command::new(&outils.ffmpeg);
        cmd.args(["-y", "-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
            .arg(&chemin_source);
        match plan_son(&meta) {
            Son::Tout if copie => {
                cmd.args(["-map", "0", "-c", "copy"]);
            }
            Son::Tout => {
                cmd.args(["-map", "0:v:0", "-map", "0:a:0?"])
                    .args(VIDEO_X264)
                    .arg(ECHELLE_1080)
                    .args(["-c:a", "aac", "-b:a", "160k", "-ar", "48000", "-ac", "2"]);
            }
            son => {
                // Un clip : la vidéo telle quelle si elle est propre, et
                // une seule piste son, composée d'après la fiche.
                if copie {
                    cmd.args(["-map", "0:v:0", "-c:v", "copy"]);
                } else {
                    cmd.args(["-map", "0:v:0"]).args(VIDEO_X264).arg(ECHELLE_1080);
                }
                match son {
                    Son::Aucun => {
                        cmd.arg("-an");
                    }
                    Son::Piste(n) => {
                        cmd.args(["-map", &format!("0:a:{n}?"), "-c:a", "copy"]);
                    }
                    Son::Melange(indices) => {
                        let entrees: String = indices.iter().map(|i| format!("[0:a:{i}]")).collect();
                        cmd.args([
                            "-filter_complex",
                            &format!("{entrees}amix=inputs={}:normalize=0[son]", indices.len()),
                            "-map", "[son]",
                            "-c:a", "aac", "-b:a", "160k", "-ar", "48000", "-ac", "2",
                        ]);
                    }
                    Son::Tout => unreachable!("traité au-dessus"),
                }
            }
        }
        cmd.args(["-movflags", "+faststart"]).arg(&chemin_sortie);
        executer_borne(&mut cmd, CONVERSION_MAX).map_err(|e| format!("ffmpeg : {e}"))?;
        let apres = sonder(outils, &chemin_sortie)?;
        let (_, w2, h2) = apres.video.ok_or("la conversion n'a pas produit d'image")?;
        // Le poster, pris dans le résultat : même orientation que ce que le
        // client lira.
        let poster = dossier.join("poster.jpg");
        let a = if apres.duree_s > 1.0 { "0.5" } else { "0" };
        executer_borne(
            Command::new(&outils.ffmpeg)
                .args([
                    "-y",
                    "-nostdin",
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-ss",
                    a,
                    "-i",
                ])
                .arg(&chemin_sortie)
                .args([
                    "-frames:v",
                    "1",
                    "-vf",
                    "scale='min(640,iw)':-2",
                    "-q:v",
                    "4",
                ])
                .arg(&poster),
            Duration::from_secs(120),
        )
        .map_err(|e| format!("poster : {e}"))?;
        meta.duree_s = apres.duree_s;
        meta.largeur = w2;
        meta.hauteur = h2;
        meta.taille = std::fs::metadata(&chemin_sortie)
            .map(|m| m.len())
            .unwrap_or(0);
        Ok(())
    })();
    let id = dossier
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // La source d'un clip reste pour l'atelier ; celle d'une vidéo
    // ordinaire n'a plus d'usage une fois convertie.
    let lacher_source = |meta: &mut Meta| {
        if !meta.garder_source {
            meta.source = None;
            let _ = std::fs::remove_file(&chemin_source);
        }
    };
    match resultat {
        Ok(()) => {
            meta.etat = "pret".into();
            meta.poster = Some(format!("/files/{id}/poster.jpg"));
            meta.message = None;
            lacher_source(&mut meta);
            ecrire_meta(dossier, &meta);
            Ok(meta)
        }
        Err(e) => {
            meta.etat = "erreur".into();
            meta.message = Some(format!(
                "vidéo illisible : {}",
                e.chars().take(160).collect::<String>()
            ));
            lacher_source(&mut meta);
            let _ = std::fs::remove_file(&chemin_sortie);
            ecrire_meta(dossier, &meta);
            Err(e)
        }
    }
}

/// Jette les téléversements par morceaux abandonnés depuis plus d'une heure.
pub fn purger_partiels(data_dir: &str) -> usize {
    purger_partiels_depuis(data_dir, PARTIEL_AGE_MAX)
}

fn purger_partiels_depuis(data_dir: &str, age_max: Duration) -> usize {
    let racine = PathBuf::from(data_dir).join("upload-partiel");
    let Ok(utilisateurs) = std::fs::read_dir(&racine) else {
        return 0;
    };
    let mut n = 0;
    for u in utilisateurs.flatten() {
        let Ok(uploads) = std::fs::read_dir(u.path()) else {
            continue;
        };
        for up in uploads.flatten() {
            let vieux = up
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age >= age_max);
            if vieux && std::fs::remove_dir_all(up.path()).is_ok() {
                n += 1;
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_names_are_recognised() {
        assert!(est_video("clip.mp4"));
        assert!(est_video("IMG_0001.MOV"));
        assert!(!est_video("photo.png"));
        assert!(!est_video("archive.zip"));
    }

    #[test]
    fn le_son_d_un_clip_se_compose_d_apres_la_fiche() {
        let clip = |pistes: Option<&[&str]>, voix: Option<bool>| Meta {
            clip: true,
            pistes: pistes.map(|p| p.iter().map(|s| s.to_string()).collect()),
            voix,
            ..Default::default()
        };
        // Une vidéo ordinaire garde tout.
        assert_eq!(plan_son(&Meta::default()), Son::Tout);
        // Le mélange, tel quel : voix gardées, ou pas de piste des copains,
        // ou pistes inconnues.
        assert_eq!(plan_son(&clip(Some(&["jeu", "micro", "copains"]), Some(true))), Son::Piste(0));
        assert_eq!(plan_son(&clip(Some(&["jeu", "micro"]), Some(false))), Son::Piste(0));
        assert_eq!(plan_son(&clip(None, Some(false))), Son::Piste(0));
        // Sans les copains : un mélange refait des autres, ou l'autre seule.
        assert_eq!(
            plan_son(&clip(Some(&["jeu", "micro", "copains"]), Some(false))),
            Son::Melange(vec![1, 2])
        );
        assert_eq!(plan_son(&clip(Some(&["jeu", "copains"]), Some(false))), Son::Piste(1));
        assert_eq!(plan_son(&clip(Some(&["micro", "copains"]), Some(false))), Son::Piste(1));
        // Rien que les copains, retirés : muet. Aucune piste : muet.
        assert_eq!(plan_son(&clip(Some(&["copains"]), Some(false))), Son::Aucun);
        assert_eq!(plan_son(&clip(Some(&[]), Some(true))), Son::Aucun);
    }

    #[test]
    fn la_cadence_se_lit_en_fraction() {
        assert_eq!(cadence_de("60/1"), Some(60.0));
        assert!((cadence_de("30000/1001").unwrap() - 29.97).abs() < 0.01);
        assert_eq!(cadence_de("0/0"), None);
        assert_eq!(cadence_de("abc"), None);
    }

    #[test]
    fn upload_ids_are_hex_of_a_sane_length() {
        assert!(upload_valide("0123456789abcdef"));
        assert!(!upload_valide("../../etc"));
        assert!(!upload_valide("abc"));
        assert!(!upload_valide(&"f".repeat(40)));
    }

    #[test]
    fn the_meta_round_trips() {
        let m = Meta {
            etat: "pret".into(),
            duree_s: 30.5,
            largeur: 1920,
            hauteur: 1080,
            poster: Some("/files/ab/poster.jpg".into()),
            taille: 1234,
            ..Default::default()
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: Meta = serde_json::from_str(&json).unwrap();
        assert_eq!(back.etat, "pret");
        assert_eq!(back.largeur, 1920);
        assert_eq!(back.poster.as_deref(), Some("/files/ab/poster.jpg"));
    }

    /// Un fichier d'essai fabriqué par le ffmpeg de la machine ; `None` s'il
    /// n'y en a pas — le test est alors sauté.
    fn fabriquer(dossier: &Path, nom: &str, args: &[&str]) -> Option<PathBuf> {
        let chemin = dossier.join(nom);
        let statut = Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error"])
            .args(args)
            .arg(&chemin)
            .status()
            .ok()?;
        statut.success().then_some(chemin)
    }

    #[test]
    fn a_phone_video_is_normalised_with_a_poster() {
        let Some(outils) = detecter() else {
            eprintln!("ffmpeg absent : test sauté");
            return;
        };
        let dossier = std::env::temp_dir().join(format!("ki-medias-norm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dossier);
        std::fs::create_dir_all(&dossier).unwrap();
        // Un « iPhone » : HEVC en portrait avec une rotation déclarée, son
        // mono à 44,1 kHz — tout ce que le client ne saurait pas lire tel quel.
        let Some(_) = fabriquer(
            &dossier,
            "source.mov",
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=720x1280:rate=30",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=660:sample_rate=44100",
                "-t",
                "2",
                "-c:v",
                "libx265",
                "-preset",
                "ultrafast",
                "-tag:v",
                "hvc1",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-ac",
                "1",
                "-metadata:s:v",
                "rotate=90",
            ],
        ) else {
            eprintln!("libx265 absent : test sauté");
            return;
        };
        let meta = Meta {
            etat: "en_preparation".into(),
            source: Some("source.mov".into()),
            sortie: Some("IMG_0001.mp4".into()),
            ..Default::default()
        };
        ecrire_meta(&dossier, &meta);
        let meta = normaliser(&outils, &dossier).expect("normalisation");
        assert_eq!(meta.etat, "pret");
        assert!(meta.largeur > 0 && meta.hauteur > 0);
        assert!(
            (1.5..=2.5).contains(&meta.duree_s),
            "durée {}",
            meta.duree_s
        );
        assert!(dossier.join("IMG_0001.mp4").is_file());
        assert!(dossier.join("poster.jpg").is_file());
        assert!(
            !dossier.join("source.mov").exists(),
            "la source est effacée"
        );
        let relue = lire_meta(&dossier).unwrap();
        assert_eq!(
            relue.poster.as_deref().map(|p| p.ends_with("/poster.jpg")),
            Some(true)
        );
        // Le résultat est bien du H.264 + AAC 48 kHz stéréo.
        let sonde = sonder(&outils, &dossier.join("IMG_0001.mp4")).unwrap();
        assert_eq!(sonde.video.as_ref().map(|v| v.0.as_str()), Some("h264"));
        assert_eq!(sonde.audio.as_deref(), Some("aac"));
        let _ = std::fs::remove_dir_all(&dossier);
    }

    #[test]
    fn un_clip_partage_ne_livre_qu_une_piste_son_et_garde_sa_source() {
        let Some(outils) = detecter() else {
            eprintln!("ffmpeg absent : test sauté");
            return;
        };
        let dossier = std::env::temp_dir().join(format!("ki-medias-clip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dossier);
        std::fs::create_dir_all(&dossier).unwrap();
        // Ce que l'enregistreur écrit : H.264 propre, et quatre pistes AAC —
        // le mélange, le jeu, le micro, les copains.
        let Some(_) = fabriquer(
            &dossier,
            "source.mp4",
            &[
                "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30",
                "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
                "-f", "lavfi", "-i", "sine=frequency=660:sample_rate=48000",
                "-f", "lavfi", "-i", "sine=frequency=880:sample_rate=48000",
                "-f", "lavfi", "-i", "sine=frequency=1100:sample_rate=48000",
                "-t", "2",
                "-map", "0:v", "-map", "1:a", "-map", "2:a", "-map", "3:a", "-map", "4:a",
                "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                "-c:a", "aac", "-ac", "2",
            ],
        ) else {
            eprintln!("libx264 absent : test sauté");
            return;
        };
        assert_eq!(sonder(&outils, &dossier.join("source.mp4")).unwrap().pistes_audio, 4);
        let meta = Meta {
            etat: "en_preparation".into(),
            source: Some("source.mp4".into()),
            sortie: Some("clip.mp4".into()),
            clip: true,
            garder_source: true,
            pistes: Some(vec!["jeu".into(), "micro".into(), "copains".into()]),
            voix: Some(false),
            ..Default::default()
        };
        ecrire_meta(&dossier, &meta);
        let meta = normaliser(&outils, &dossier).expect("normalisation du clip");
        assert_eq!(meta.etat, "pret");
        assert!((1.5..=2.5).contains(&meta.duree_s), "durée {}", meta.duree_s);
        // Une seule piste : le mélange refait du jeu et du micro, sans les
        // copains. Les pistes séparées ne sortent pas du dossier.
        let sonde = sonder(&outils, &dossier.join("clip.mp4")).unwrap();
        assert_eq!(sonde.pistes_audio, 1);
        assert_eq!(sonde.video.as_ref().map(|v| v.0.as_str()), Some("h264"));
        assert!(dossier.join("poster.jpg").is_file());
        // La source reste, et la fiche s'en souvient.
        assert!(dossier.join("source.mp4").is_file(), "la source d'un clip est gardée");
        let relue = lire_meta(&dossier).unwrap();
        assert_eq!(relue.source.as_deref(), Some("source.mp4"));
        assert!(relue.clip && relue.garder_source);
        let _ = std::fs::remove_dir_all(&dossier);
    }

    #[test]
    fn a_clean_clip_is_copied_not_reencoded() {
        let Some(outils) = detecter() else {
            eprintln!("ffmpeg absent : test sauté");
            return;
        };
        let dossier = std::env::temp_dir().join(format!("ki-medias-copie-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dossier);
        std::fs::create_dir_all(&dossier).unwrap();
        let Some(_) = fabriquer(
            &dossier,
            "source.mp4",
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=1280x720:rate=30",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-t",
                "2",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-ac",
                "2",
            ],
        ) else {
            return;
        };
        let avant = std::fs::metadata(dossier.join("source.mp4")).unwrap().len();
        ecrire_meta(
            &dossier,
            &Meta {
                etat: "en_preparation".into(),
                source: Some("source.mp4".into()),
                sortie: Some("clip.mp4".into()),
                ..Default::default()
            },
        );
        let meta = normaliser(&outils, &dossier).expect("normalisation");
        assert_eq!(meta.etat, "pret");
        // Copié tel quel : même taille à quelques kilo-octets près (l'index
        // déplacé en tête par +faststart).
        let apres = std::fs::metadata(dossier.join("clip.mp4")).unwrap().len();
        assert!(apres.abs_diff(avant) < 64 * 1024, "{avant} -> {apres}");
        let _ = std::fs::remove_dir_all(&dossier);
    }

    #[test]
    fn stale_partial_uploads_are_swept() {
        let base = std::env::temp_dir().join(format!("ki-partiel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let vieux = base
            .join("upload-partiel")
            .join("7")
            .join("0123456789abcdef");
        let recent = base
            .join("upload-partiel")
            .join("7")
            .join("fedcba9876543210");
        std::fs::create_dir_all(&vieux).unwrap();
        std::fs::write(vieux.join("00000"), b"x").unwrap();
        // La date d'un dossier ne se règle pas partout : on fait varier
        // l'âge limite plutôt que l'horloge.
        assert_eq!(
            purger_partiels_depuis(&base.to_string_lossy(), Duration::ZERO),
            1
        );
        assert!(!vieux.exists());
        std::fs::create_dir_all(&recent).unwrap();
        assert_eq!(
            purger_partiels_depuis(&base.to_string_lossy(), Duration::from_secs(3600)),
            0
        );
        assert!(recent.exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
