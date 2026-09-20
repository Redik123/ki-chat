//! Le tableau de bord de l'administration : l'état du serveur en un écran,
//! pour l'onglet « Tableau de bord » du client — ce qu'on lisait avec curl
//! sur /diag-resume, structuré et à portée de clic. Réservé à
//! l'administration, comme les diagnostics ; rien ici ne contient de
//! message ni de voix.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use ki_protocol::{
    ChannelId, ChannelKind, TableauAdmin, TableauDiffusion, TableauFabrique, TableauMembre,
    TableauSalonVocal, TableauStock, UserId,
};

use crate::files;
use crate::state::AppState;

/// GET /admin/tableau — l'état du serveur, en JSON.
pub async fn tableau(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    if !crate::diag::lecteur_autorise(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "accès réservé à l'administration").into_response();
    }
    // Les dossiers se parcourent sur le pool bloquant, jamais sur la
    // boucle qui relaie la voix.
    let s = state.clone();
    match tokio::task::spawn_blocking(move || composer(&s)).await {
        Ok(t) => axum::Json(t).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Tout ce que le tableau montre, relevé d'un coup.
pub fn composer(state: &AppState) -> TableauAdmin {
    let channels = state.channels.list();
    let nom_salon = |id: ChannelId| channels.iter().find(|c| c.id == id).map(|c| c.name.clone());
    let comptes = state.accounts.list(&state.roles).len() as u32;

    let mut en_ligne = Vec::new();
    let mut occupants: HashMap<ChannelId, Vec<String>> = HashMap::new();
    let mut pseudos: HashMap<UserId, String> = HashMap::new();
    {
        let users = state.users.lock().unwrap();
        for (id, u) in users.iter() {
            pseudos.insert(*id, u.username.clone());
            if let Some(v) = u.voice {
                occupants.entry(v).or_default().push(u.username.clone());
            }
            en_ligne.push(TableauMembre {
                user_id: *id,
                pseudo: u.username.clone(),
                vocal: u.voice.and_then(nom_salon),
                diffuse: u.streaming.is_some(),
                jeu: u.jeu.as_ref().map(|j| j.ligne()),
            });
        }
    }
    en_ligne.sort_by_key(|m| m.pseudo.to_lowercase());

    let vocal: Vec<TableauSalonVocal> = channels
        .iter()
        .filter(|c| c.kind == ChannelKind::Voice)
        .map(|c| {
            let mut o = occupants.remove(&c.id).unwrap_or_default();
            o.sort_by_key(|p| p.to_lowercase());
            TableauSalonVocal { nom: c.name.clone(), occupants: o, verrouille: c.locked }
        })
        .collect();

    let mut diffusions: Vec<TableauDiffusion> = state
        .streams
        .resume()
        .into_iter()
        .map(|(streamer, spectateurs, meta, palier)| TableauDiffusion {
            streamer: pseudos
                .get(&streamer)
                .cloned()
                .unwrap_or_else(|| format!("membre {streamer}")),
            spectateurs: spectateurs as u32,
            largeur: meta.width,
            hauteur: meta.height,
            fps: meta.fps,
            kbps: meta.kbps,
            // Le palier ne se dit que s'il bride : au réglage, rien.
            palier: (palier > 0 && palier < meta.kbps).then_some(palier),
        })
        .collect();
    diffusions.sort_by_key(|d| d.streamer.to_lowercase());

    let data = PathBuf::from(&state.data_dir);
    TableauAdmin {
        version: env!("CARGO_PKG_VERSION").to_string(),
        depuis_s: state.demarrage.elapsed().as_secs(),
        comptes,
        en_ligne,
        salons_texte: channels.iter().filter(|c| c.kind == ChannelKind::Text).count() as u32,
        vocal,
        diffusions,
        fichiers: stock(&data.join("files"), state.files_quota),
        clips: stock(&crate::clips::dossier(state), state.clips_quota),
        disque_libre_octets: disque_libre(&data),
        memoire_octets: memoire_rss(),
        musique: state.musique.tableau(),
        valorant: state.valorant.compteurs_texte(),
        diagnostics: crate::diag::lignes_resume(&crate::diag::diag_dir(state)),
        fabrique: fabrique(state),
    }
}

/// La fabrique des vidéos : ce qu'elle fait et ce qui attend.
fn fabrique(state: &AppState) -> TableauFabrique {
    let r = state.medias.resume();
    TableauFabrique {
        en_file: r.en_file as u32,
        en_cours: r.en_cours.as_ref().map(|(nom, genre, _)| format!("{genre} de {nom}")),
        depuis_s: r.en_cours.as_ref().map(|(_, _, d)| d.as_secs()).unwrap_or(0),
    }
}

/// Un stock face à son plafond : combien, quel volume, quelles bornes.
fn stock(racine: &Path, quota: files::Quota) -> TableauStock {
    TableauStock {
        nombre: std::fs::read_dir(racine)
            .map(|d| d.flatten().count() as u32)
            .unwrap_or(0),
        octets: files::used_bytes(racine),
        plafond_octets: quota.max_bytes,
        ttl_jours: quota.ttl_days,
    }
}

/// La place qui reste sur le disque des données — par `df`, ce que la
/// bibliothèque standard ne dit pas. Rien hors Unix : le serveur de
/// production est un conteneur Linux, la machine de développement fait
/// sans.
fn disque_libre(dir: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        let sortie = std::process::Command::new("df")
            .args(["-k", "--output=avail"])
            .arg(dir)
            .output()
            .ok()?;
        let texte = String::from_utf8_lossy(&sortie.stdout);
        let ko: u64 = texte.lines().last()?.trim().parse().ok()?;
        Some(ko * 1024)
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        None
    }
}

/// La mémoire résidente du processus, d'après le noyau Linux.
fn memoire_rss() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statut = std::fs::read_to_string("/proc/self/status").ok()?;
        let ligne = statut.lines().find(|l| l.starts_with("VmRSS:"))?;
        let ko: u64 = ligne.split_whitespace().nth(1)?.parse().ok()?;
        Some(ko * 1024)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_stock_se_compte_et_un_dossier_absent_fait_zero() {
        let dir = std::env::temp_dir().join("ki-chat-tableau-stock");
        let _ = std::fs::remove_dir_all(&dir);
        let quota = files::Quota { max_bytes: 1000, ttl_days: 7 };
        let vide = stock(&dir, quota);
        assert_eq!((vide.nombre, vide.octets, vide.plafond_octets, vide.ttl_jours), (0, 0, 1000, 7));
        // Un stock est fait de dossiers, un par envoi, comme data/files et
        // data/clips : c'est ce que compte `used_bytes`.
        std::fs::create_dir_all(dir.join("x1")).unwrap();
        std::fs::create_dir_all(dir.join("x2")).unwrap();
        std::fs::write(dir.join("x1").join("a.bin"), [0u8; 300]).unwrap();
        std::fs::write(dir.join("x2").join("b.bin"), [0u8; 200]).unwrap();
        let plein = stock(&dir, quota);
        assert_eq!(plein.nombre, 2);
        assert_eq!(plein.octets, 500);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
