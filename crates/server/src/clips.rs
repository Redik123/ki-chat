//! Les clips partagés, côté serveur (PLAN-CLIPS.md, jalon C2).
//!
//! Un clip monte par morceaux comme n'importe quelle vidéo (`/upload/
//! partiel`), mais finit ailleurs : dans `data/clips/<id>/`, avec son propre
//! plafond et sa propre durée de vie (`KI_CLIPS_MAX_BYTES`,
//! `KI_CLIPS_TTL_DAYS`), parce qu'un clip pèse dix fois une photo et qu'on ne
//! veut pas qu'une soirée de clips efface les partages de la semaine. La
//! source y **reste** après conversion : l'atelier (jalon C3) en aura
//! besoin, avec ses pistes son séparées.
//!
//! La version partagée, elle, ne porte qu'une piste : le mélange, ou un
//! mélange refait sans les voix des copains si le membre l'a demandé — les
//! pistes séparées ne sortent jamais du dossier du clip. Une fois le fichier
//! reçu, le serveur poste dans le salon choisi, **au nom du membre**, la
//! légende et le lien ; chez chacun, la carte vidéo dit « en préparation »
//! le temps de la conversion, comme pour toute vidéo.
//!
//! Les clips se servent par le même chemin que les fichiers
//! (`/files/<id>/…`) : les clients n'ont rien à apprendre, un clip est une
//! vidéo comme une autre pour eux.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path as Param, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use rand::Rng;
use serde::Deserialize;

use crate::export::{self, Recette};
use crate::files;
use crate::medias::{self, Meta};
use crate::state::AppState;

/// Plafond par défaut de `data/clips/` : 8 Gio, une centaine de clips.
pub const DEFAULT_MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Durée de vie par défaut : 60 jours. Un clip se regarde le soir même et
/// la semaine d'après ; passé deux mois, il a été exporté ou oublié.
pub const DEFAULT_TTL_DAYS: u64 = 60;
/// Une légende reste une légende.
const LEGENDE_MAX: usize = 500;
/// Les pistes que l'enregistreur sait écrire, après le mélange.
const PISTES: [&str; 3] = ["jeu", "micro", "copains"];

pub fn dossier(state: &AppState) -> PathBuf {
    PathBuf::from(&state.data_dir).join("clips")
}

#[derive(Deserialize)]
pub struct ParamsFin {
    upload: String,
    parts: u32,
}

/// Ce que le client dit du clip, dans le corps de `/clips/fin`.
#[derive(Deserialize)]
pub struct Demande {
    /// Le salon textuel où poster — ou rien : le clip est déposé pour
    /// l'atelier, sans message.
    #[serde(default)]
    channel: Option<ki_protocol::ChannelId>,
    #[serde(default)]
    legende: String,
    /// Les pistes de la source après le mélange, dans l'ordre du fichier —
    /// `None` si le client ne les connaît pas (un clip d'avant la fiche).
    #[serde(default)]
    pistes: Option<Vec<String>>,
    /// Garder les voix des copains dans la version partagée.
    #[serde(default = "vrai")]
    voix: bool,
    /// Le nom du clip chez le membre, pour le nom du fichier partagé.
    nom: String,
}

fn vrai() -> bool {
    true
}

/// `POST /clips/fin?upload=<id>&parts=<n>`, corps JSON [`Demande`] :
/// assemble le clip dans `data/clips/`, lance sa conversion, et poste le
/// message au nom du membre. Rend `{ "id": …, "url": … }`.
pub async fn fin(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ParamsFin>,
    headers: HeaderMap,
    Json(demande): Json<Demande>,
) -> impl IntoResponse {
    let (user_id, username) = match medias::authentifier(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r.into_response(),
    };
    if !medias::upload_valide(&params.upload)
        || params.parts == 0
        || params.parts > medias::MORCEAUX_MAX
    {
        return (StatusCode::BAD_REQUEST, "téléversement invalide").into_response();
    }
    // Sans ffmpeg, pas de version partagée : la source porte les pistes
    // séparées, elle ne se livre pas telle quelle.
    if !state.medias.disponible() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "le serveur n'a pas ffmpeg : pas de partage de clips",
        )
            .into_response();
    }
    if let Some(p) = &demande.pistes {
        let connues = p.iter().all(|n| PISTES.contains(&n.as_str()));
        let sans_doublon = p.iter().enumerate().all(|(i, n)| !p[..i].contains(n));
        if !connues || !sans_doublon {
            return (StatusCode::BAD_REQUEST, "pistes inconnues").into_response();
        }
    }
    // La légende passe par le nettoyage d'un message — avant de ranger quoi
    // que ce soit : la refuser après coup laisserait un clip orphelin.
    let legende = match legende_propre(&demande.legende) {
        Ok(l) => l,
        Err(r) => return r.into_response(),
    };
    if let Some(channel) = demande.channel {
        if let Err(r) = salon_ouvert(&state, user_id, channel) {
            return r.into_response();
        }
    }
    let hote = match hote_de(&headers) {
        Ok(h) => h,
        Err(r) => return r.into_response(),
    };

    let nom = files::sanitize(&demande.nom);
    let sortie = {
        let racine = std::path::Path::new(&nom)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| "clip".into());
        format!("{racine}.mp4")
    };
    let partiel = medias::dossier_partiel(&state, user_id, &params.upload);
    let racine = dossier(&state);
    let quota = state.clips_quota.max_bytes;
    let parts = params.parts;
    let assemble = tokio::task::spawn_blocking(move || {
        medias::assembler(&partiel, parts, &racine, quota, "source.mp4")
    })
    .await;
    let (id, dossier_clip, total) = match assemble {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => {
            let code = if e.contains("saturé") {
                StatusCode::INSUFFICIENT_STORAGE
            } else {
                StatusCode::BAD_REQUEST
            };
            return (code, e.replace("espace de partage", "espace des clips")).into_response();
        }
        Err(e) => {
            tracing::error!("assemblage d'un clip : {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "stockage indisponible").into_response();
        }
    };
    let meta = Meta {
        etat: "en_preparation".into(),
        source: Some("source.mp4".into()),
        sortie: Some(sortie.clone()),
        clip: true,
        garder_source: true,
        auteur: Some(user_id),
        salon: demande.channel,
        legende: legende.clone(),
        nom: Some(nom.clone()),
        pistes: demande.pistes.clone(),
        voix: Some(demande.voix),
        ..Default::default()
    };
    medias::ecrire_meta(&dossier_clip, &meta);
    state.medias.deposer(dossier_clip);
    tracing::info!(
        "clip reçu : {nom} ({} Mo) de {username} (id {user_id}), voix des copains : {}",
        total / (1024 * 1024),
        if demande.voix {
            "gardées"
        } else {
            "retirées"
        }
    );

    // Le message, au nom du membre : la légende s'il y en a une, et le lien.
    let url = format!("https://{hote}/files/{id}/{sortie}");
    if let Some(channel) = demande.channel {
        if let Err(r) = poster_lien(
            &state,
            channel,
            user_id,
            &username,
            legende.as_deref(),
            &url,
        ) {
            return r.into_response();
        }
    }
    Json(serde_json::json!({ "id": id, "url": url })).into_response()
}

// ---------------------------------------------------------------------
// Ce que les routes se partagent
// ---------------------------------------------------------------------

/// L'adresse par laquelle ce client nous parle : celle que les autres
/// liront, exactement ce qu'il aurait écrit lui-même.
fn hote_de(headers: &HeaderMap) -> Result<String, (StatusCode, &'static str)> {
    headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::to_string)
        .ok_or((StatusCode::BAD_REQUEST, "en-tête Host manquant"))
}

/// Une légende, nettoyée comme un message ; `None` si vide.
fn legende_propre(legende: &str) -> Result<Option<String>, (StatusCode, String)> {
    if legende.chars().count() > LEGENDE_MAX {
        return Err((StatusCode::BAD_REQUEST, "légende trop longue".into()));
    }
    match legende.trim() {
        "" => Ok(None),
        l => ki_protocol::clean_chat(l)
            .map(Some)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("légende : {e}"))),
    }
}

/// Le salon d'un partage : textuel, visible du membre, avec le droit d'y
/// écrire.
fn salon_ouvert(
    state: &AppState,
    user_id: u64,
    channel: ki_protocol::ChannelId,
) -> Result<(), (StatusCode, &'static str)> {
    let textuel = state
        .channels
        .list()
        .iter()
        .any(|c| c.id == channel && c.kind == ki_protocol::ChannelKind::Text);
    if !textuel || !state.can_view(user_id, channel) {
        return Err((StatusCode::BAD_REQUEST, "ce salon n'existe pas"));
    }
    if !state.holds(user_id, ki_protocol::perm::SEND_MESSAGE) {
        return Err((
            StatusCode::FORBIDDEN,
            "tu n'as pas le droit d'écrire dans les salons",
        ));
    }
    Ok(())
}

/// Le message au nom du membre : la légende s'il y en a une, et le lien.
fn poster_lien(
    state: &AppState,
    channel: ki_protocol::ChannelId,
    user_id: u64,
    username: &str,
    legende: Option<&str>,
    url: &str,
) -> Result<(), (StatusCode, String)> {
    let texte = match legende {
        Some(l) => format!("{l}\n{url}"),
        None => url.to_string(),
    };
    let texte = ki_protocol::clean_chat(&texte).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    state.poster_membre(channel, user_id, username, &texte);
    Ok(())
}

/// Un membre connecté, par son jeton voix — sans autre droit : disposer de
/// son propre clip n'en demande pas.
fn membre(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(u64, String), (StatusCode, &'static str)> {
    let token = headers
        .get("x-ki-token")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| u64::from_str_radix(s, 16).ok());
    token
        .and_then(|t| state.user_by_voice_token(t))
        .ok_or((StatusCode::UNAUTHORIZED, "jeton invalide"))
}

fn id_valide(id: &str) -> bool {
    id.len() == 16 && id.chars().all(|c| c.is_ascii_hexdigit())
}

/// Le dossier et la fiche d'un clip, si l'identifiant en désigne un.
fn clip_de(state: &AppState, id: &str) -> Option<(PathBuf, Meta)> {
    if !id_valide(id) {
        return None;
    }
    let dossier = dossier(state).join(id);
    let meta = medias::lire_meta(&dossier)?;
    meta.clip.then_some((dossier, meta))
}

/// Un clip dont le membre dispose — celui qui l'a déposé, ou un
/// administrateur — ou la réponse qui dit pourquoi pas.
fn clip_du_membre(
    state: &AppState,
    id: &str,
    user_id: u64,
) -> Result<(PathBuf, Meta), (StatusCode, &'static str)> {
    let (dossier, meta) = clip_de(state, id).ok_or((StatusCode::NOT_FOUND, "clip inconnu"))?;
    let dispose =
        meta.auteur == Some(user_id) || state.holds(user_id, ki_protocol::perm::ADMINISTRATOR);
    if !dispose {
        return Err((StatusCode::FORBIDDEN, "ce clip n'est pas à toi"));
    }
    Ok((dossier, meta))
}

/// Les fichiers d'un clip que l'on peut donner : la version partagée, et
/// l'export s'il est prêt. Jamais la source, avec ses pistes séparées.
fn fichier_connu(dossier: &Path, meta: &Meta, fichier: &str) -> bool {
    let partage = meta.etat == "pret" && meta.sortie.as_deref() == Some(fichier);
    let export = export::lire_etat(dossier)
        .is_some_and(|e| e.etat == "pret" && e.fichier.as_deref() == Some(fichier));
    (partage || export) && dossier.join(fichier).is_file()
}

// ---------------------------------------------------------------------
// L'atelier : export, téléphone, repartage, suppression
// ---------------------------------------------------------------------

/// `POST /clips/{id}/exporter`, corps JSON [`Recette`] : la recette est
/// validée contre la source, puis l'export part en file — `export.json`
/// dans le dossier dit la suite. Rend `{ "fichier": … }`.
pub async fn exporter(
    State(state): State<Arc<AppState>>,
    Param(id): Param<String>,
    headers: HeaderMap,
    Json(recette): Json<Recette>,
) -> impl IntoResponse {
    let (user_id, username) = match medias::authentifier(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r.into_response(),
    };
    let (dossier_clip, meta) = match clip_du_membre(&state, &id, user_id) {
        Ok(x) => x,
        Err(r) => return r.into_response(),
    };
    if meta.etat != "pret" || !dossier_clip.join("source.mp4").is_file() {
        return (StatusCode::CONFLICT, "le clip n'est pas prêt").into_response();
    }
    let Some(outils) = state.medias.outils() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "le serveur n'a pas ffmpeg").into_response();
    };
    if export::lire_etat(&dossier_clip)
        .is_some_and(|e| matches!(e.etat.as_str(), "en_attente" | "en_cours"))
    {
        return (
            StatusCode::CONFLICT,
            "un export est déjà en cours sur ce clip",
        )
            .into_response();
    }
    // La recette contre la source (ffprobe) : sur le pool bloquant.
    let verdict = {
        let (d, r, pistes) = (dossier_clip.clone(), recette.clone(), meta.pistes.clone());
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let sonde = medias::sonder(&outils, &d.join("source.mp4"))?;
            let source = export::Source::depuis(&sonde, pistes).ok_or("source sans image")?;
            export::valider(&r, &source, export::police().is_some())
        })
        .await
    };
    match verdict {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return (StatusCode::BAD_REQUEST, e).into_response(),
        Err(e) => {
            tracing::error!("sonde d'export : {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "sonde impossible").into_response();
        }
    }
    let fichier = export::nom_sortie(&recette);
    export::ecrire_etat(
        &dossier_clip,
        &export::Etat {
            etat: "en_attente".into(),
            fichier: Some(fichier.to_string()),
            ..Default::default()
        },
    );
    state.medias.deposer_export(dossier_clip, recette);
    tracing::info!("export demandé : clip {id} par {username} (id {user_id})");
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "fichier": fichier })),
    )
        .into_response()
}

/// Un lien pour le téléphone : un jeton aléatoire de 128 bits, valable une
/// heure, pour ce fichier seulement.
#[derive(Clone)]
pub struct JetonTelephone {
    pub chemin: PathBuf,
    /// Le nom que le téléphone verra.
    pub nom: String,
    pub expire: Instant,
}

pub type Jetons = Mutex<HashMap<String, JetonTelephone>>;

const JETON_DUREE: Duration = Duration::from_secs(3600);

#[derive(Deserialize)]
pub struct DemandeFichier {
    fichier: String,
}

/// `POST /clips/{id}/telephone`, corps `{ "fichier": … }` : rend
/// `{ "url": …, "expire_s": … }`, l'adresse que le QR code portera.
pub async fn telephone(
    State(state): State<Arc<AppState>>,
    Param(id): Param<String>,
    headers: HeaderMap,
    Json(d): Json<DemandeFichier>,
) -> impl IntoResponse {
    let (user_id, _) = match membre(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r.into_response(),
    };
    let (dossier_clip, meta) = match clip_du_membre(&state, &id, user_id) {
        Ok(x) => x,
        Err(r) => return r.into_response(),
    };
    let fichier = files::sanitize(&d.fichier);
    if !fichier_connu(&dossier_clip, &meta, &fichier) {
        return (StatusCode::NOT_FOUND, "ce fichier n'existe pas (encore)").into_response();
    }
    let hote = match hote_de(&headers) {
        Ok(h) => h,
        Err(r) => return r.into_response(),
    };
    let jeton = format!(
        "{:016x}{:016x}",
        rand::rng().random::<u64>(),
        rand::rng().random::<u64>()
    );
    let racine = meta
        .nom
        .as_deref()
        .and_then(|n| {
            Path::new(n)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "clip".into());
    let nom = match fichier.as_str() {
        "telephone.mp4" => format!("{racine}-tiktok.mp4"),
        "export.mp4" => format!("{racine}-court.mp4"),
        _ => fichier.clone(),
    };
    {
        let mut jetons = state.jetons_telephone.lock().unwrap();
        let maintenant = Instant::now();
        jetons.retain(|_, e| e.expire > maintenant);
        jetons.insert(
            jeton.clone(),
            JetonTelephone {
                chemin: dossier_clip.join(&fichier),
                nom,
                expire: maintenant + JETON_DUREE,
            },
        );
    }
    Json(serde_json::json!({
        "url": format!("https://{hote}/tel/{jeton}"),
        "expire_s": JETON_DUREE.as_secs(),
    }))
    .into_response()
}

/// `GET /tel/{jeton}` : le fichier, pour le téléphone qui a scanné le QR
/// code. Le jeton suffit — aléatoire, court dans le temps, un seul fichier.
pub async fn tel(
    State(state): State<Arc<AppState>>,
    Param(jeton): Param<String>,
) -> impl IntoResponse {
    if jeton.len() != 32 || !jeton.chars().all(|c| c.is_ascii_hexdigit()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let entree = {
        let mut jetons = state.jetons_telephone.lock().unwrap();
        let maintenant = Instant::now();
        jetons.retain(|_, e| e.expire > maintenant);
        jetons.get(&jeton).cloned()
    };
    let Some(e) = entree else {
        return (
            StatusCode::NOT_FOUND,
            "ce lien n'est plus valable — refais un QR code depuis ki-chat",
        )
            .into_response();
    };
    match tokio::fs::read(&e.chemin).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "video/mp4".to_string()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{}\"", e.nom),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Deserialize)]
pub struct DemandePartage {
    channel: ki_protocol::ChannelId,
    #[serde(default)]
    legende: String,
    fichier: String,
}

/// `POST /clips/{id}/partager` : poster un fichier du clip — l'export, en
/// général — dans un salon, au nom du membre.
pub async fn partager(
    State(state): State<Arc<AppState>>,
    Param(id): Param<String>,
    headers: HeaderMap,
    Json(d): Json<DemandePartage>,
) -> impl IntoResponse {
    let (user_id, username) = match medias::authentifier(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r.into_response(),
    };
    let (dossier_clip, meta) = match clip_du_membre(&state, &id, user_id) {
        Ok(x) => x,
        Err(r) => return r.into_response(),
    };
    let fichier = files::sanitize(&d.fichier);
    if !fichier_connu(&dossier_clip, &meta, &fichier) {
        return (StatusCode::NOT_FOUND, "ce fichier n'existe pas (encore)").into_response();
    }
    if let Err(r) = salon_ouvert(&state, user_id, d.channel) {
        return r.into_response();
    }
    let legende = match legende_propre(&d.legende) {
        Ok(l) => l,
        Err(r) => return r.into_response(),
    };
    let hote = match hote_de(&headers) {
        Ok(h) => h,
        Err(r) => return r.into_response(),
    };
    let url = format!("https://{hote}/files/{id}/{fichier}");
    if let Err(r) = poster_lien(
        &state,
        d.channel,
        user_id,
        &username,
        legende.as_deref(),
        &url,
    ) {
        return r.into_response();
    }
    Json(serde_json::json!({ "url": url })).into_response()
}

/// `DELETE /clips/{id}` : le dossier entier — la source, la version
/// partagée, l'export. Le message du fil reste, avec son lien mort.
pub async fn supprimer(
    State(state): State<Arc<AppState>>,
    Param(id): Param<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let (user_id, username) = match membre(&state, &headers) {
        Ok(u) => u,
        Err(r) => return r.into_response(),
    };
    let (dossier_clip, _) = match clip_du_membre(&state, &id, user_id) {
        Ok(x) => x,
        Err(r) => return r.into_response(),
    };
    match tokio::fs::remove_dir_all(&dossier_clip).await {
        Ok(()) => {
            tracing::info!("clip {id} supprimé par {username} (id {user_id})");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            tracing::error!("suppression du clip {id} : {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "suppression impossible").into_response()
        }
    }
}

/// Au démarrage : les clips laissés « en préparation » repassent en file ;
/// un export interrompu, lui, ne reprend pas — il faudrait sa recette — et
/// on le dit, pour qu'un nouveau puisse partir.
pub fn reprendre(state: &AppState) {
    let racine = dossier(state);
    medias::reprendre_dans(state, &racine);
    let Ok(entrees) = std::fs::read_dir(&racine) else {
        return;
    };
    for e in entrees.flatten() {
        let d = e.path();
        if let Some(mut etat) = export::lire_etat(&d) {
            if matches!(etat.etat.as_str(), "en_attente" | "en_cours") {
                etat.etat = "erreur".into();
                etat.message = Some("interrompu par un redémarrage du serveur".into());
                export::ecrire_etat(&d, &etat);
            }
        }
    }
}

/// L'état des deux stocks, pour `/diag-resume` : ce qu'ils pèsent face à
/// leur plafond. Parcourt le disque — sur le pool bloquant.
pub fn resume_stockage(state: &AppState) -> String {
    let ligne = |quoi: &str, racine: &std::path::Path, quota: files::Quota| {
        let n = std::fs::read_dir(racine)
            .map(|d| d.flatten().count())
            .unwrap_or(0);
        let mo = files::used_bytes(racine) / (1024 * 1024);
        let plafond = match quota.max_bytes {
            0 => "sans plafond".to_string(),
            b => format!("{} Mo", b / (1024 * 1024)),
        };
        let age = match quota.ttl_days {
            0 => "sans limite d'âge".to_string(),
            j => format!("{j} jours"),
        };
        format!("{quoi} : {n} · {mo} Mo / {plafond} · {age}")
    };
    format!(
        "stockage\n{}\n{}",
        ligne(
            "fichiers partagés",
            &PathBuf::from(&state.data_dir).join("files"),
            state.files_quota
        ),
        ligne("clips", &dossier(state), state.clips_quota)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_demande_se_lit_avec_ses_defauts() {
        let d: Demande = serde_json::from_str(r#"{"channel":3,"nom":"clip.mp4"}"#).unwrap();
        assert_eq!(d.channel, Some(3));
        let d: Demande = serde_json::from_str(r#"{"nom":"clip.mp4"}"#).unwrap();
        assert_eq!(d.channel, None, "sans salon : déposé pour l'atelier");
        assert!(d.voix);
        assert!(d.pistes.is_none());
        assert_eq!(d.legende, "");
        let d: Demande = serde_json::from_str(
            r#"{"channel":1,"nom":"x.mp4","legende":"gg","pistes":["jeu","copains"],"voix":false}"#,
        )
        .unwrap();
        assert_eq!(
            d.pistes.as_deref(),
            Some(&["jeu".to_string(), "copains".to_string()][..])
        );
        assert!(!d.voix);
    }
}
