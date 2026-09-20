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
            return medias::refus_assemblage(
                &e.replace("espace de partage", "espace des clips"),
                &username,
                user_id,
                "clip",
            )
        }
        Err(e) => {
            tracing::error!("assemblage d'un clip : {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "stockage indisponible").into_response();
        }
    };
    // Le message, au nom du membre : la légende s'il y en a une, et le
    // lien. Avant la fiche : la fabrique la relit en commençant et la
    // réécrit en finissant, ce qu'on y ajouterait entre les deux serait
    // perdu — et la fiche retient le message, pour l'effacer avec le clip.
    let url = format!("https://{hote}/files/{id}/{sortie}");
    let mut messages = Vec::new();
    if let Some(channel) = demande.channel {
        match poster_lien(
            &state,
            channel,
            user_id,
            &username,
            legende.as_deref(),
            &url,
        ) {
            Ok(ts) => messages.push((channel, ts)),
            Err(r) => return r.into_response(),
        }
    }
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
        messages,
        ..Default::default()
    };
    // Sans fiche, la carte de chacun dirait « en préparation » sans fin et
    // l'atelier relirait un 404 : mieux vaut le dire tout de suite. Le
    // clip reçu est jeté, le message posté reste (rare, et l'admin lit le
    // journal : c'est un disque plein).
    if let Err(e) = medias::ecrire_meta(&dossier_clip, &meta) {
        tracing::error!("clip {id} : fiche non écrite ({e}) — clip jeté");
        let _ = std::fs::remove_dir_all(&dossier_clip);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("stockage indisponible sur le serveur ({e}) — préviens un admin"),
        )
            .into_response();
    }
    let devant = state.medias.deposer(dossier_clip);
    tracing::info!(
        "clip reçu : {nom} ({} Mo) de {username} (id {user_id}), voix des copains : {}, {devant} devant en file",
        total / (1024 * 1024),
        if demande.voix {
            "gardées"
        } else {
            "retirées"
        }
    );
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
) -> Result<u64, (StatusCode, String)> {
    let texte = match legende {
        Some(l) => format!("{l}\n{url}"),
        None => url.to_string(),
    };
    let texte = ki_protocol::clean_chat(&texte).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    Ok(state.poster_membre(channel, user_id, username, &texte))
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

/// Les fichiers d'un clip que l'on peut donner à voir ou à partager : la
/// version partagée, et les exports finis. Jamais la source, avec ses
/// pistes séparées, ni le texte du titre.
pub(crate) fn fichier_connu(dossier: &Path, meta: &Meta, fichier: &str) -> bool {
    let partage = meta.etat == "pret" && meta.sortie.as_deref() == Some(fichier);
    // Un export : l'un des deux noms que l'atelier produit. Celui dont
    // `export.json` parle n'est un fichier que si l'état dit « prêt » — en
    // cours, il s'écrit encore ; en erreur, ce qui reste est un fichier
    // tronqué (ffmpeg tué avec le conteneur, tâche morte), que l'on ne
    // sert ni ne partage. L'autre nom, dont l'état ne parle pas, est un
    // export précédent, fini : les chemins qui forcent « erreur »
    // effacent leur sortie, il n'y reste rien de tronqué.
    let export = export::EXPORTS.contains(&fichier)
        && export::lire_etat(dossier)
            .is_none_or(|e| e.fichier.as_deref() != Some(fichier) || e.etat == "pret");
    (partage || export) && dossier.join(fichier).is_file()
}

/// Un export qui ne finira pas (redémarrage, tâche morte, état périmé) :
/// son état passe en `erreur`, et ce qu'il a laissé sur le disque part avec
/// — un MP4 sans index n'est pas un export, et `fichier_connu` ne doit pas
/// le retrouver sous l'autre nom quand un nouvel export aura réécrit
/// `export.json`.
pub(crate) fn abandonner_export(dossier: &Path, etat: &mut export::Etat, pourquoi: &str) {
    if let Some(f) = etat.fichier.as_deref().filter(|f| export::EXPORTS.contains(f)) {
        let _ = std::fs::remove_file(dossier.join(f));
    }
    etat.etat = "erreur".into();
    etat.message = Some(pourquoi.into());
    let _ = export::ecrire_etat(dossier, etat);
}

/// Ce que `GET /files/<id>/<nom>` peut servir d'un dossier de clip : les
/// fiches (`meta.json`, `export.json`), le poster, et les fichiers de
/// [`fichier_connu`]. Le reste — `source.mp4` et ses pistes séparées,
/// `titre.txt` — n'existe pas pour qui connaît le lien.
pub(crate) fn fichier_servable(dossier: &Path, nom: &str) -> bool {
    if matches!(nom, "meta.json" | "export.json" | "poster.jpg") {
        return dossier.join(nom).is_file();
    }
    medias::lire_meta(dossier).is_some_and(|meta| fichier_connu(dossier, &meta, nom))
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
    // Un export vraiment en cours : la fabrique le tient en file ou sous
    // ffmpeg. Un `export.json` « en cours » qu'elle ne connaît pas est un
    // reste (tâche morte, ancien démarrage) : il ne bloque pas.
    if let Some(mut etat) = export::lire_etat(&dossier_clip)
        .filter(|e| matches!(e.etat.as_str(), "en_attente" | "en_cours"))
    {
        if state.medias.export_vivant(&dossier_clip) {
            return (
                StatusCode::CONFLICT,
                "un export est déjà en cours sur ce clip",
            )
                .into_response();
        }
        tracing::warn!("export : clip {id} : un état « en cours » périmé, on repart");
        // Ce que la tâche morte a laissé ne doit pas se servir sous l'autre
        // nom une fois que l'état ne parlera plus que du nouvel export.
        abandonner_export(&dossier_clip, &mut etat, "export interrompu sur le serveur");
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
    // L'état d'abord, la file ensuite : si l'état ne peut pas s'écrire
    // (disque plein, droits), le membre le sait tout de suite au lieu de
    // relire un 404 pendant un quart d'heure. Le nombre de tâches devant
    // s'écrit avec (« en file derrière 2 ») : c'est celui d'avant le dépôt,
    // et le dépôt suit aussitôt — la fabrique réécrira « en cours » quand
    // elle prendra la tâche.
    let fabrique = state.medias.resume();
    let derriere = fabrique.en_file as u32 + u32::from(fabrique.en_cours.is_some());
    let en_attente = export::Etat {
        etat: "en_attente".into(),
        fichier: Some(fichier.to_string()),
        derriere: Some(derriere),
        ..Default::default()
    };
    if let Err(e) = export::ecrire_etat(&dossier_clip, &en_attente) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("stockage indisponible sur le serveur ({e}) — préviens un admin"),
        )
            .into_response();
    }
    state.medias.deposer_export(dossier_clip, recette);
    tracing::info!(
        "export demandé : clip {id} ({fichier}) par {username} (id {user_id}), {derriere} devant en file"
    );
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
        "url": format!("{}/tel/{jeton}", base_publique(&hote)),
        "expire_s": JETON_DUREE.as_secs(),
    }))
    .into_response()
}

/// L'adresse publique du serveur, pour un lien qu'un téléphone suivra :
/// `KI_PUBLIC_URL` si l'admin l'a posée (derrière un mandataire, l'en-tête
/// Host peut être une adresse interne), sinon celle par laquelle le client
/// nous parle.
fn base_publique(hote: &str) -> String {
    std::env::var("KI_PUBLIC_URL")
        .ok()
        .map(|u| u.trim().trim_end_matches('/').to_string())
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
        .unwrap_or_else(|| format!("https://{hote}"))
}

/// `GET /tel/{jeton}` : le fichier, pour le téléphone qui a scanné le QR
/// code. Le jeton suffit — aléatoire, court dans le temps, un seul fichier.
pub async fn tel(
    State(state): State<Arc<AppState>>,
    Param(jeton): Param<String>,
    headers: HeaderMap,
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
    // En flux, avec `Range` : le téléphone reprend un téléchargement coupé
    // et le serveur ne charge jamais le fichier entier en mémoire.
    files::servir_fichier(&e.chemin, "video/mp4", "attachment", &e.nom, &headers).await
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
    let ts = match poster_lien(
        &state,
        d.channel,
        user_id,
        &username,
        legende.as_deref(),
        &url,
    ) {
        Ok(ts) => ts,
        Err(r) => return r.into_response(),
    };
    // La fiche retient ce message aussi, pour l'effacer avec le clip.
    let mut meta = meta;
    meta.messages.push((d.channel, ts));
    let _ = medias::ecrire_meta(&dossier_clip, &meta);
    tracing::info!("clip {id} : {fichier} partagé par {username} (id {user_id}) dans le salon {}", d.channel);
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
    let (dossier_clip, meta) = match clip_du_membre(&state, &id, user_id) {
        Ok(x) => x,
        Err(r) => return r.into_response(),
    };
    match tokio::fs::remove_dir_all(&dossier_clip).await {
        Ok(()) => {
            // Les messages du fil qui portaient le clip partent avec lui :
            // un lien mort n'a rien à faire dans l'historique.
            let auteur = meta.auteur.unwrap_or(user_id);
            let mut effaces = 0;
            for (salon, ts) in &meta.messages {
                let message = ki_protocol::MsgRef {
                    user_id: auteur,
                    ts: *ts,
                };
                if state.history.delete(*salon, message) {
                    state.broadcast(
                        *salon,
                        None,
                        &ki_protocol::ServerMsg::MessageDeleted {
                            channel: *salon,
                            message,
                        },
                    );
                    effaces += 1;
                }
            }
            tracing::info!(
                "clip {id} supprimé par {username} (id {user_id}), {effaces} message(s) effacé(s)"
            );
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
/// on le dit, pour qu'un nouveau puisse partir. Le fichier qu'il écrivait
/// part avec : Watchtower recrée le conteneur à chaque image, ffmpeg meurt
/// avec lui, et un `export.mp4` sans index resterait sinon à partager.
pub fn reprendre(state: &AppState) {
    let racine = dossier(state);
    medias::reprendre_dans(state, &racine);
    abandonner_exports_dans(&racine);
}

fn abandonner_exports_dans(racine: &Path) {
    let Ok(entrees) = std::fs::read_dir(racine) else {
        return;
    };
    for e in entrees.flatten() {
        let d = e.path();
        if let Some(mut etat) = export::lire_etat(&d) {
            if matches!(etat.etat.as_str(), "en_attente" | "en_cours") {
                abandonner_export(&d, &mut etat, "interrompu par un redémarrage du serveur");
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
        "stockage\n{}\n{}\n{}",
        ligne(
            "fichiers partagés",
            &PathBuf::from(&state.data_dir).join("files"),
            state.files_quota
        ),
        ligne("clips", &dossier(state), state.clips_quota),
        state.medias.ligne()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dossier_d_essai(nom: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ki-clips-{nom}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Ce qu'un dossier de clip donne à voir : la fiche, le poster,
    /// l'état d'export, la version partagée, les exports finis — jamais la
    /// source ni le titre, ni un export que la fabrique écrit encore.
    #[test]
    fn un_clip_ne_sert_que_sa_liste_blanche() {
        let d = dossier_d_essai("blanche");
        for nom in ["source.mp4", "clip.mp4", "poster.jpg", "titre.txt", "export.mp4", "telephone.mp4"] {
            std::fs::write(d.join(nom), b"x").unwrap();
        }
        // Pas de fiche du tout : rien que les fichiers de fiche existants.
        assert!(!fichier_servable(&d, "clip.mp4"));
        assert!(!fichier_servable(&d, "meta.json"));
        assert!(fichier_servable(&d, "poster.jpg"));
        assert!(!fichier_servable(&d, "source.mp4"));

        let meta = Meta {
            etat: "en_preparation".into(),
            clip: true,
            garder_source: true,
            source: Some("source.mp4".into()),
            sortie: Some("clip.mp4".into()),
            ..Default::default()
        };
        medias::ecrire_meta(&d, &meta).unwrap();
        assert!(fichier_servable(&d, "meta.json"));
        assert!(!fichier_servable(&d, "clip.mp4"), "pas prête : pas servie");
        assert!(!fichier_servable(&d, "source.mp4"));
        assert!(!fichier_servable(&d, "titre.txt"));
        // Les exports existent et aucun n'est en cours : servis.
        assert!(fichier_servable(&d, "export.mp4"));
        assert!(fichier_servable(&d, "telephone.mp4"));
        assert!(!fichier_servable(&d, "export.json"), "pas encore d'export.json");

        let prete = Meta { etat: "pret".into(), ..meta };
        medias::ecrire_meta(&d, &prete).unwrap();
        assert!(fichier_servable(&d, "clip.mp4"));
        assert!(!fichier_servable(&d, "source.mp4"), "la source ne sort jamais");
        assert!(!fichier_servable(&d, "titre.txt"));
        assert!(!fichier_servable(&d, "autre.mp4"));

        // Un export téléphone en cours : le fichier qu'il écrit n'est pas
        // servi, l'autre export l'est toujours ; l'état, lui, se lit.
        export::ecrire_etat(
            &d,
            &export::Etat { etat: "en_cours".into(), fichier: Some("telephone.mp4".into()), ..Default::default() },
        )
        .unwrap();
        assert!(fichier_servable(&d, "export.json"));
        assert!(!fichier_servable(&d, "telephone.mp4"));
        assert!(!fichier_connu(&d, &prete, "telephone.mp4"));
        assert!(fichier_servable(&d, "export.mp4"));
        assert!(fichier_connu(&d, &prete, "export.mp4"));
        assert!(fichier_connu(&d, &prete, "clip.mp4"));
        assert!(!fichier_connu(&d, &prete, "source.mp4"));
        // Fini : les deux.
        export::ecrire_etat(
            &d,
            &export::Etat { etat: "pret".into(), fichier: Some("telephone.mp4".into()), ..Default::default() },
        )
        .unwrap();
        assert!(fichier_servable(&d, "telephone.mp4"));
        // Un export en erreur qui aurait laissé un fichier (ffmpeg tué avec
        // le conteneur) : ce qui reste est tronqué, pas servi ; l'autre
        // export, fini avant, l'est toujours.
        export::ecrire_etat(
            &d,
            &export::Etat { etat: "erreur".into(), fichier: Some("telephone.mp4".into()), ..Default::default() },
        )
        .unwrap();
        assert!(!fichier_servable(&d, "telephone.mp4"));
        assert!(!fichier_connu(&d, &prete, "telephone.mp4"));
        assert!(fichier_servable(&d, "export.mp4"));
        // Un export raté laisse un fichier absent : pas servi non plus.
        std::fs::remove_file(d.join("telephone.mp4")).unwrap();
        assert!(!fichier_servable(&d, "telephone.mp4"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Un redémarrage pendant un export (Watchtower recrée le conteneur,
    /// ffmpeg meurt avec lui) : au démarrage suivant, l'état passe en
    /// erreur **et** le fichier à moitié écrit disparaît — sans quoi
    /// `/partager` et `/files/<id>/export.mp4` le donnaient à tout le salon.
    /// L'export fini d'avant, sous l'autre nom, reste ; un dossier sans
    /// export en cours n'est pas touché.
    #[test]
    fn un_export_interrompu_par_un_redemarrage_perd_son_fichier_tronque() {
        let racine = dossier_d_essai("reprise");
        let coupe = racine.join("0123456789abcdef");
        let fini = racine.join("fedcba9876543210");
        for d in [&coupe, &fini] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("export.mp4"), b"fini").unwrap();
            std::fs::write(d.join("telephone.mp4"), b"tronq").unwrap();
            medias::ecrire_meta(
                d,
                &Meta { etat: "pret".into(), clip: true, sortie: Some("clip.mp4".into()), ..Default::default() },
            )
            .unwrap();
        }
        export::ecrire_etat(
            &coupe,
            &export::Etat { etat: "en_cours".into(), fichier: Some("telephone.mp4".into()), pour_cent: 37, ..Default::default() },
        )
        .unwrap();
        export::ecrire_etat(
            &fini,
            &export::Etat { etat: "pret".into(), fichier: Some("telephone.mp4".into()), ..Default::default() },
        )
        .unwrap();

        abandonner_exports_dans(&racine);

        let etat = export::lire_etat(&coupe).unwrap();
        assert_eq!(etat.etat, "erreur");
        assert_eq!(etat.fichier.as_deref(), Some("telephone.mp4"));
        assert_eq!(etat.message.as_deref(), Some("interrompu par un redémarrage du serveur"));
        assert!(!coupe.join("telephone.mp4").exists(), "le fichier tronqué est parti");
        assert!(coupe.join("export.mp4").is_file(), "l'autre export, fini, reste");
        assert!(fichier_servable(&coupe, "export.mp4"));
        assert!(!fichier_servable(&coupe, "telephone.mp4"));
        // Et si un nouvel export sous l'autre nom réécrit l'état, le
        // tronqué n'est plus là pour être servi « par défaut ».
        export::ecrire_etat(
            &coupe,
            &export::Etat { etat: "pret".into(), fichier: Some("export.mp4".into()), ..Default::default() },
        )
        .unwrap();
        assert!(!fichier_servable(&coupe, "telephone.mp4"));

        let etat = export::lire_etat(&fini).unwrap();
        assert_eq!(etat.etat, "pret", "un export fini n'est pas touché");
        assert!(fini.join("telephone.mp4").is_file());
        assert!(fichier_servable(&fini, "telephone.mp4"));
        let _ = std::fs::remove_dir_all(&racine);
    }

    /// Le circuit de l'atelier, fonctions à la suite : les morceaux
    /// s'assemblent dans le stock des clips, la fiche s'écrit, la fabrique
    /// normalise, l'export part et `export.json` raconte la suite — jusqu'à
    /// un fichier que `fichier_connu` accepte de partager. Sauté sans
    /// ffmpeg.
    #[test]
    fn depot_puis_export_d_un_clip_synthetique() {
        let Some(outils) = medias::detecter() else {
            eprintln!("ffmpeg absent : test sauté");
            return;
        };
        let racine = dossier_d_essai("circuit");
        // Le clip tel que l'enregistreur l'écrit : H.264, quatre pistes.
        let source = racine.join("clip-source.mp4");
        let statut = std::process::Command::new(&outils.ffmpeg)
            .args(["-y", "-loglevel", "error"])
            .args([
                "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30",
                "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
                "-f", "lavfi", "-i", "sine=frequency=660:sample_rate=48000",
                "-f", "lavfi", "-i", "sine=frequency=880:sample_rate=48000",
                "-f", "lavfi", "-i", "sine=frequency=1100:sample_rate=48000",
                "-t", "4",
                "-map", "0:v", "-map", "1:a", "-map", "2:a", "-map", "3:a", "-map", "4:a",
                "-c:v", "libx264", "-preset", "ultrafast", "-g", "30", "-pix_fmt", "yuv420p",
                "-c:a", "aac", "-ac", "2",
            ])
            .arg(&source)
            .status();
        if !statut.map(|s| s.success()).unwrap_or(false) {
            eprintln!("libx264 absent : test sauté");
            return;
        }
        // Les morceaux, comme `/upload/partiel` les range : 64 Kio chacun.
        let octets = std::fs::read(&source).unwrap();
        let partiel = racine.join("upload-partiel").join("7").join("0123456789abcdef");
        std::fs::create_dir_all(&partiel).unwrap();
        let morceaux: Vec<&[u8]> = octets.chunks(64 * 1024).collect();
        for (i, m) in morceaux.iter().enumerate() {
            std::fs::write(partiel.join(format!("{i:05}")), m).unwrap();
        }
        let stock = racine.join("clips");
        std::fs::create_dir_all(&stock).unwrap();
        // `/clips/fin` : assemblage sous le plafond, fiche, mise en file.
        let (id, dossier_clip, total) =
            medias::assembler(&partiel, morceaux.len() as u32, &stock, 0, "source.mp4").unwrap();
        assert!(id_valide(&id));
        assert_eq!(total, octets.len() as u64);
        assert!(!partiel.exists(), "les morceaux sont jetés");
        let meta = Meta {
            etat: "en_preparation".into(),
            source: Some("source.mp4".into()),
            sortie: Some("clip.mp4".into()),
            clip: true,
            garder_source: true,
            auteur: Some(7),
            nom: Some("clip.mp4".into()),
            pistes: Some(vec!["jeu".into(), "micro".into(), "copains".into()]),
            voix: Some(true),
            ..Default::default()
        };
        medias::ecrire_meta(&dossier_clip, &meta).unwrap();
        let fabrique = medias::Fabrique::avec(Some(outils.clone()), 512);
        assert_eq!(fabrique.deposer(dossier_clip.clone()), 0);
        // Avant la conversion : l'export est refusé (« pas prêt »), et la
        // version partagée n'est pas servie.
        let (d, m) = clip_de_dans(&stock, &id).unwrap();
        assert_eq!(m.etat, "en_preparation");
        assert!(!fichier_servable(&d, "clip.mp4"));
        // La fabrique, à la main : ce que `boucle` fait.
        let travail = fabrique.prochaine().unwrap();
        medias::normaliser_pour_test(&outils, &travail.dossier).expect("normalisation");
        fabrique.terminee();
        let (_, m) = clip_de_dans(&stock, &id).unwrap();
        assert_eq!(m.etat, "pret");
        assert!(fichier_servable(&dossier_clip, "clip.mp4"));
        assert!(fichier_servable(&dossier_clip, "poster.jpg"));
        assert!(!fichier_servable(&dossier_clip, "source.mp4"));

        // `/clips/{id}/exporter` : la recette validée, l'état « en attente »
        // écrit avant la file, puis la fabrique.
        let recette: Recette = serde_json::from_str(
            r#"{"debut_ms":1000,"fin_ms":3000,"format":{"type":"original"},"audio":{"jeu":1.0,"micro":1.0,"copains":0.0}}"#,
        )
        .unwrap();
        let sonde = medias::sonder(&outils, &dossier_clip.join("source.mp4")).unwrap();
        let src = export::Source::depuis(&sonde, m.pistes.clone()).unwrap();
        export::valider(&recette, &src, false).unwrap();
        assert!(export::coupe_en_copie(&recette, &src), "une coupe seule : en copie");
        export::ecrire_etat(
            &dossier_clip,
            &export::Etat {
                etat: "en_attente".into(),
                fichier: Some(export::nom_sortie(&recette).into()),
                derriere: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
        fabrique.deposer_export(dossier_clip.clone(), recette.clone());
        assert!(fabrique.export_vivant(&dossier_clip), "en file : vivant, un second export serait un 409");
        let etat = export::lire_etat(&dossier_clip).unwrap();
        assert_eq!((etat.etat.as_str(), etat.fichier.as_deref(), etat.derriere), ("en_attente", Some("export.mp4"), Some(0)));
        assert!(!fichier_servable(&dossier_clip, "export.mp4"), "rien à servir encore");

        let travail = fabrique.prochaine().unwrap();
        let fini = export::executer(&outils, &travail.dossier, &recette).expect("export");
        fabrique.terminee();
        assert_eq!(fini.etat, "pret");
        assert_eq!(fini.mode.as_deref(), Some("copie"));
        assert!((1.0..=4.5).contains(&fini.duree_s), "coupe à la trame clé : {} s", fini.duree_s);
        let etat = export::lire_etat(&dossier_clip).unwrap();
        assert_eq!((etat.etat.as_str(), etat.pour_cent, etat.fichier.as_deref()), ("pret", 100, Some("export.mp4")));
        assert!(etat.depuis.is_some());
        assert!(!fabrique.export_vivant(&dossier_clip), "fini : un nouvel export peut partir");
        // Ce que `/partager` et `/files/<id>/export.mp4` acceptent.
        let (_, m) = clip_de_dans(&stock, &id).unwrap();
        assert!(fichier_connu(&dossier_clip, &m, "export.mp4"));
        assert!(fichier_servable(&dossier_clip, "export.mp4"));
        assert!(!fichier_servable(&dossier_clip, "titre.txt"));
        assert!(!fichier_servable(&dossier_clip, "source.mp4"));
        // Le résultat est bien du H.264 avec une seule piste son.
        let apres = medias::sonder(&outils, &dossier_clip.join("export.mp4")).unwrap();
        assert_eq!(apres.video.as_ref().map(|v| v.0.as_str()), Some("h264"));
        assert_eq!(apres.pistes_audio, 1);
        let _ = std::fs::remove_dir_all(&racine);
    }

    /// `clip_de` sans `AppState` : le même contrôle, sur un stock donné.
    fn clip_de_dans(stock: &Path, id: &str) -> Option<(PathBuf, Meta)> {
        if !id_valide(id) {
            return None;
        }
        let dossier = stock.join(id);
        let meta = medias::lire_meta(&dossier)?;
        meta.clip.then_some((dossier, meta))
    }

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
