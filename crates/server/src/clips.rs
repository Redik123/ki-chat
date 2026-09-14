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

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;

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
    /// Le salon textuel où poster.
    channel: ki_protocol::ChannelId,
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
    if demande.legende.chars().count() > LEGENDE_MAX {
        return (StatusCode::BAD_REQUEST, "légende trop longue").into_response();
    }
    // La légende passe par le nettoyage d'un message — avant de ranger quoi
    // que ce soit : la refuser après coup laisserait un clip orphelin.
    let legende = match demande.legende.trim() {
        "" => None,
        l => match ki_protocol::clean_chat(l) {
            Ok(l) => Some(l),
            Err(e) => return (StatusCode::BAD_REQUEST, format!("légende : {e}")).into_response(),
        },
    };
    // Le salon : textuel, visible du membre, et le droit d'y écrire.
    let textuel = state
        .channels
        .list()
        .iter()
        .any(|c| c.id == demande.channel && c.kind == ki_protocol::ChannelKind::Text);
    if !textuel || !state.can_view(user_id, demande.channel) {
        return (StatusCode::BAD_REQUEST, "ce salon n'existe pas").into_response();
    }
    if !state.holds(user_id, ki_protocol::perm::SEND_MESSAGE) {
        return (
            StatusCode::FORBIDDEN,
            "tu n'as pas le droit d'écrire dans les salons",
        )
            .into_response();
    }
    // L'adresse que les autres liront : celle par laquelle ce client nous
    // parle, exactement ce qu'il aurait écrit lui-même.
    let Some(hote) = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::to_string)
    else {
        return (StatusCode::BAD_REQUEST, "en-tête Host manquant").into_response();
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
        salon: Some(demande.channel),
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
    let texte = match &legende {
        Some(l) => format!("{l}\n{url}"),
        None => url.clone(),
    };
    match ki_protocol::clean_chat(&texte) {
        Ok(texte) => state.poster_membre(demande.channel, user_id, &username, &texte),
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    }
    Json(serde_json::json!({ "id": id, "url": url })).into_response()
}

/// Au démarrage : les clips laissés « en préparation » repassent en file.
pub fn reprendre(state: &AppState) {
    medias::reprendre_dans(state, &dossier(state));
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
        assert_eq!(d.channel, 3);
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
