//! Diagnostics partagés : les clients volontaires téléversent leur journal
//! technique, le serveur l'archive par utilisateur, et un admin le relit à
//! distance — l'outil qui remplace « copie-colle-moi ton journal » quand
//! quelqu'un « a le bug ».
//!
//! # Ce qui transite, et ce qui ne transite jamais
//!
//! Le client n'envoie que du **technique**, et seulement si son utilisateur a
//! coché l'option : journal audio (ouvertures de périphériques, pertes,
//! réouvertures), rapport du docteur, version et système. Jamais le contenu
//! des messages, jamais l'audio. Le serveur n'y ajoute que l'heure de
//! réception : il archive, il n'interprète pas.
//!
//! # Accès
//!
//! - `POST /diag` : le client authentifié (jeton voix, comme l'upload de
//!   fichiers) ajoute des lignes JSONL à son propre dossier.
//! - `GET /diag` et `GET /diag/{fichier}` : lecture, par deux portes — la
//!   session d'un compte ADMINISTRATOR (c'est l'onglet « Diagnostics » du
//!   panneau d'administration), ou le jeton `data/diag.token` généré au
//!   premier démarrage (l'accès hors application, depuis une machine de
//!   confiance ; il ne circule jamais dans le protocole).
//!
//! Le stock est borné par utilisateur (rotation) : les diagnostics racontent
//! les derniers jours, pas l'histoire du monde.

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use rand::Rng;

use crate::state::AppState;

/// Taille max d'un lot téléversé : bien assez pour un journal complet
/// (200 événements font ~30 Ko), assez peu pour qu'on ne nous remplisse pas.
pub const MAX_BATCH: usize = 256 * 1024;

/// Au-delà, le fichier bascule en `.old` (une seule génération conservée) :
/// par utilisateur, le stock est donc borné à ~10 Mo.
const ROTATE_BYTES: u64 = 5 * 1024 * 1024;

fn diag_dir(state: &AppState) -> PathBuf {
    PathBuf::from(&state.data_dir).join("diag")
}

/// La version annoncée par le client (en-tête x-ki-version), réduite à un
/// nom de dossier sûr. Les archives sont **classées par version** : c'est
/// l'historique des bugs de chaque livraison, et ce qui permet de purger
/// « tout ce qui date de la 0.1.12 » d'un geste.
fn version_propre(v: Option<&str>) -> String {
    let propre: String = v
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .take(24)
        .collect();
    // Un nom fait uniquement de points (« .. » !) serait un chemin, pas une
    // version : au rebut avec les vides.
    if propre.is_empty() || propre.chars().all(|c| c == '.') {
        "inconnue".into()
    } else {
        propre
    }
}

/// Un segment de chemin est-il une version telle que nous les écrivons ?
/// Tout le reste est refusé : pas de traversée, pas de `..`, pas de vide.
fn segment_version_valide(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 24
        && v != ".."
        && v != "."
        && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Nom de fichier d'un utilisateur : identifiant + pseudo assaini. C'est
/// l'identifiant qui fait l'unicité, le pseudo n'est là que pour l'humain
/// qui liste le dossier.
fn fichier_de(user_id: u64, username: &str) -> String {
    let propre: String = username
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_') { c } else { '_' })
        .take(40)
        .collect();
    format!("{user_id:08}-{propre}.jsonl")
}

/// POST /diag — corps : lignes JSONL, en-tête x-ki-token = jeton voix (hex).
/// Même authentification que l'upload de fichiers : le jeton n'est connu que
/// d'un client connecté, et le serveur retrouve qui il est.
pub async fn upload(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let token = headers
        .get("x-ki-token")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| u64::from_str_radix(s, 16).ok());
    let Some((user_id, username)) = token.and_then(|t| state.user_by_voice_token(t)) else {
        return (StatusCode::UNAUTHORIZED, "jeton invalide").into_response();
    };
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "lot vide").into_response();
    }
    // Du texte, rien d'autre : ces fichiers se relisent au pager et se
    // grep-ent. Un client qui envoie du binaire n'est pas notre client.
    let Ok(texte) = std::str::from_utf8(&body) else {
        return (StatusCode::BAD_REQUEST, "le lot doit être du texte UTF-8").into_response();
    };

    let version = version_propre(
        headers.get("x-ki-version").and_then(|v| v.to_str().ok()),
    );
    let dir = diag_dir(&state).join(&version);
    let chemin = dir.join(fichier_de(user_id, &username));
    let recu = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    // L'enveloppe du serveur date la réception : les horloges des clients
    // mentent parfois, celle-ci est la nôtre.
    let enveloppe = format!(
        "{{\"type\":\"recu\",\"t\":{recu},\"de\":\"{}\",\"octets\":{}}}\n",
        username.replace('"', "_"),
        body.len()
    );
    let texte = texte.to_owned();

    // Écriture sur le pool bloquant : ce chemin est rare (dix minutes au
    // mieux par client volontaire), mais un write n'a rien à faire sur la
    // boucle qui relaie la voix de tout le monde.
    let resultat = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        rotate_si_plein(&chemin)?;
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&chemin)?;
        f.write_all(enveloppe.as_bytes())?;
        f.write_all(texte.as_bytes())?;
        if !texte.ends_with('\n') {
            f.write_all(b"\n")?;
        }
        Ok(())
    })
    .await;
    match resultat {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "archivage indisponible").into_response(),
    }
}

/// Fait tourner le fichier s'il dépasse la borne : l'actuel devient `.old`
/// (en remplaçant la génération précédente), et l'append repart à neuf.
fn rotate_si_plein(chemin: &FsPath) -> std::io::Result<()> {
    let taille = std::fs::metadata(chemin).map(|m| m.len()).unwrap_or(0);
    if taille < ROTATE_BYTES {
        return Ok(());
    }
    let ancien = chemin.with_extension("jsonl.old");
    let _ = std::fs::remove_file(&ancien);
    std::fs::rename(chemin, &ancien)
}

/// Le jeton d'administration : lu depuis `data/diag.token`, généré au premier
/// besoin. Il vit sur le disque du serveur et nulle part ailleurs — le relire
/// demande d'être déjà chez soi sur la machine.
fn jeton_admin(state: &AppState) -> std::io::Result<String> {
    let chemin = PathBuf::from(&state.data_dir).join("diag.token");
    match std::fs::read_to_string(&chemin) {
        Ok(t) if !t.trim().is_empty() => Ok(t.trim().to_string()),
        _ => {
            let neuf = format!("{:032x}", rand::rng().random::<u128>());
            std::fs::write(&chemin, &neuf)?;
            tracing::info!("jeton d'accès aux diagnostics généré : {}", chemin.display());
            Ok(neuf)
        }
    }
}

/// À appeler au démarrage : matérialise le jeton (et sa ligne de journal
/// disant où le lire) sans attendre la première requête.
pub fn init(state: &AppState) {
    if let Err(e) = jeton_admin(state) {
        tracing::warn!("jeton de diagnostics indisponible : {e}");
    }
}

/// L'appelant a-t-il le droit de LIRE les archives ? Deux portes, une par
/// usage : le jeton d'administration du disque (en-tête x-ki-admin — l'accès
/// hors application, curl depuis une machine de confiance), ou la session
/// d'un compte ADMINISTRATOR (en-tête x-ki-token — l'onglet « Diagnostics »
/// du panneau d'administration).
fn lecteur_autorise(state: &AppState, headers: &HeaderMap) -> bool {
    let admin = headers.get("x-ki-admin").and_then(|v| v.to_str().ok());
    if let (Some(fourni), Ok(attendu)) = (admin, jeton_admin(state)) {
        if fourni == attendu {
            return true;
        }
    }
    let token = headers
        .get("x-ki-token")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| u64::from_str_radix(s, 16).ok());
    if let Some((user_id, _)) = token.and_then(|t| state.user_by_voice_token(t)) {
        return state.holds(user_id, ki_protocol::perm::ADMINISTRATOR);
    }
    false
}

/// GET /diag — la liste des archives : une ligne par fichier, taille et date.
pub async fn lister(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !lecteur_autorise(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "accès réservé à l'administration").into_response();
    }
    let dir = diag_dir(&state);
    // Deux niveaux : les dossiers de version, puis les archives des joueurs.
    // Une ligne par archive, « version/fichier », triée — les versions se
    // lisent groupées, l'historique des bugs saute aux yeux.
    let liste = tokio::task::spawn_blocking(move || {
        let Ok(versions) = std::fs::read_dir(&dir) else { return String::new() };
        let mut lignes: Vec<String> = Vec::new();
        for vdir in versions.flatten() {
            if !vdir.path().is_dir() {
                continue;
            }
            let version = vdir.file_name().to_string_lossy().to_string();
            let Ok(entrees) = std::fs::read_dir(vdir.path()) else { continue };
            for e in entrees.flatten() {
                let Ok(meta) = e.metadata() else { continue };
                let age = meta
                    .modified()
                    .ok()
                    .and_then(|m| m.elapsed().ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                lignes.push(format!(
                    "{version}/{}\t{} Ko\til y a {} min",
                    e.file_name().to_string_lossy(),
                    meta.len() / 1024,
                    age / 60
                ));
            }
        }
        lignes.sort();
        lignes.join("\n")
    })
    .await
    .unwrap_or_default();
    (StatusCode::OK, liste).into_response()
}

/// DELETE /diag/{version} — purge toutes les archives d'une version : le
/// grand ménage quand une livraison ancienne n'a plus rien à apprendre.
pub async fn supprimer(
    State(state): State<Arc<AppState>>,
    Path(version): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !lecteur_autorise(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "accès réservé à l'administration").into_response();
    }
    if !segment_version_valide(&version) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let dir = diag_dir(&state).join(&version);
    match tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&dir)).await {
        Ok(Ok(())) => {
            tracing::info!("diagnostics de la version {version} supprimés");
            StatusCode::NO_CONTENT.into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(serde::Deserialize)]
pub struct LireParams {
    /// Ne renvoyer que les N derniers octets (borné à 512 Ko) : la page
    /// d'administration débogue le passé récent, pas l'histoire complète.
    tail: Option<u64>,
}

/// GET /diag/{version}/{fichier}[?tail=N] — une archive, en texte brut.
pub async fn lire(
    State(state): State<Arc<AppState>>,
    Path((version, fichier)): Path<(String, String)>,
    axum::extract::Query(params): axum::extract::Query<LireParams>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !lecteur_autorise(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "accès réservé à l'administration").into_response();
    }
    // Seuls les noms que nous générons passent : pas de traversée de chemin.
    let valide = segment_version_valide(&version)
        && fichier
            .strip_suffix(".jsonl")
            .or_else(|| fichier.strip_suffix(".jsonl.old"))
            .is_some_and(|base| {
                !base.is_empty()
                    && base
                        .chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
            });
    if !valide {
        return StatusCode::NOT_FOUND.into_response();
    }
    match tokio::fs::read_to_string(diag_dir(&state).join(&version).join(&fichier)).await {
        Ok(texte) => {
            let texte = match params.tail {
                Some(n) => {
                    let vise = texte.len().saturating_sub(n.min(512 * 1024) as usize);
                    // Découpe à une frontière de caractère : on rend du texte,
                    // pas un début d'UTF-8 tronqué.
                    let debut = (vise..=texte.len())
                        .find(|i| texte.is_char_boundary(*i))
                        .unwrap_or(0);
                    texte[debut..].to_string()
                }
                None => texte,
            };
            (StatusCode::OK, texte).into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Lecture max par archive pour le résumé : la fin suffit — c'est le passé
/// récent qui compte, et le parcours relit tous les fichiers d'un coup.
const RESUME_LECTURE_MAX: u64 = 2 * 1024 * 1024;

/// Ce que le résumé compte dans les archives d'une version : les marqueurs
/// que les clients écrivent, tels quels. Des compteurs indicatifs — une même
/// ligne peut en nourrir plusieurs — mais qui suffisent à dire où ça casse.
#[derive(Default)]
struct Compte {
    /// Lignes « moteur vocal démarré » : autant de sessions vocales.
    sessions: u64,
    /// Lignes contenant « réouverture » : périphériques perdus puis repris.
    reouvertures: u64,
    /// Lignes contenant « affamé » : la signature du micro tenu par un jeu.
    famines: u64,
    /// Lignes contenant « erreur », quelle qu'elle soit.
    erreurs: u64,
    /// Lignes `"type":"crash"` : les rapports de plantage embarqués.
    crashs: u64,
}

fn compter_lignes(texte: &str, c: &mut Compte) {
    for ligne in texte.lines() {
        if ligne.contains("moteur vocal démarré") {
            c.sessions += 1;
        }
        if ligne.contains("réouverture") {
            c.reouvertures += 1;
        }
        if ligne.contains("affamé") {
            c.famines += 1;
        }
        if ligne.contains("erreur") {
            c.erreurs += 1;
        }
        if ligne.contains("\"type\":\"crash\"") {
            c.crashs += 1;
        }
    }
}

/// La fin d'un fichier, bornée à `max` octets. Quand la borne coupe, la
/// première ligne — entamée au milieu — est écartée : une demi-ligne
/// compterait de travers.
fn fin_de_fichier(chemin: &FsPath, max: u64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(chemin)?;
    let taille = f.metadata()?.len();
    if taille <= max {
        let mut octets = Vec::with_capacity(taille as usize);
        f.read_to_end(&mut octets)?;
        return Ok(String::from_utf8_lossy(&octets).into_owned());
    }
    f.seek(SeekFrom::End(-(max as i64)))?;
    let mut octets = Vec::with_capacity(max as usize);
    f.read_to_end(&mut octets)?;
    let texte = String::from_utf8_lossy(&octets);
    Ok(texte.split_once('\n').map(|(_, reste)| reste).unwrap_or("").to_owned())
}

/// Construit le résumé : une ligne tabulée par version, triée, sous une
/// ligne d'en-tête. Vide s'il n'y a aucune archive.
fn resume_versions(dir: &FsPath) -> String {
    let Ok(versions) = std::fs::read_dir(dir) else { return String::new() };
    let mut lignes: Vec<String> = Vec::new();
    for vdir in versions.flatten() {
        if !vdir.path().is_dir() {
            continue;
        }
        let version = vdir.file_name().to_string_lossy().to_string();
        let Ok(entrees) = std::fs::read_dir(vdir.path()) else { continue };
        let mut joueurs = 0u64;
        let mut taille = 0u64;
        let mut compte = Compte::default();
        for e in entrees.flatten() {
            // Un joueur = son archive vivante ; la génération `.old` du même
            // joueur nourrit les compteurs et la taille, pas l'effectif.
            if e.file_name().to_string_lossy().ends_with(".jsonl") {
                joueurs += 1;
            }
            taille += e.metadata().map(|m| m.len()).unwrap_or(0);
            if let Ok(texte) = fin_de_fichier(&e.path(), RESUME_LECTURE_MAX) {
                compter_lignes(&texte, &mut compte);
            }
        }
        lignes.push(format!(
            "{version}\t{joueurs}\t{}\t{}\t{}\t{}\t{}\t{} Ko",
            compte.sessions,
            compte.reouvertures,
            compte.famines,
            compte.erreurs,
            compte.crashs,
            taille / 1024
        ));
    }
    if lignes.is_empty() {
        return String::new();
    }
    lignes.sort();
    format!(
        "version\tjoueurs\tsessions\tréouvertures\tfamines\terreurs\tcrashs\ttaille\n{}",
        lignes.join("\n")
    )
}

/// GET /diag-resume — l'état des lieux en un écran : une ligne par version,
/// les compteurs qui disent la santé de chaque livraison, avant de plonger
/// dans le détail des archives.
pub async fn resume(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !lecteur_autorise(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "accès réservé à l'administration").into_response();
    }
    let dir = diag_dir(&state);
    // Le résumé relit la fin de chaque archive : sur le pool bloquant, comme
    // tout ce qui touche au disque — jamais sur la boucle qui relaie la voix.
    let texte = tokio::task::spawn_blocking(move || resume_versions(&dir))
        .await
        .unwrap_or_default();
    let texte = if texte.is_empty() {
        "aucune archive de diagnostic pour l'instant".to_string()
    } else {
        texte
    };
    (StatusCode::OK, texte).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_nom_de_fichier_est_assaini_et_stable() {
        // Les lettres restent (accents compris — is_alphanumeric est Unicode),
        // les séparateurs de chemin deviennent des tirets bas.
        assert_eq!(fichier_de(7, "rédik/../.."), "00000007-rédik______.jsonl");
        assert_eq!(fichier_de(42, "Joueur_1"), "00000042-Joueur_1.jsonl");
        // Le pseudo est borné : un pseudo interminable ne fait pas un chemin
        // interminable.
        assert!(fichier_de(1, &"a".repeat(200)).len() < 60);
    }

    #[test]
    fn la_rotation_garde_une_seule_generation() {
        let dir = std::env::temp_dir().join("ki-chat-diag-rotation");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let chemin = dir.join("00000001-test.jsonl");

        // Sous la borne : rien ne bouge.
        std::fs::write(&chemin, b"petit\n").unwrap();
        rotate_si_plein(&chemin).unwrap();
        assert!(chemin.exists());
        assert!(!chemin.with_extension("jsonl.old").exists());

        // Au-delà : l'actuel devient .old, et un second dépassement remplace
        // la génération précédente au lieu d'en empiler une troisième.
        std::fs::write(&chemin, vec![b'x'; ROTATE_BYTES as usize]).unwrap();
        rotate_si_plein(&chemin).unwrap();
        assert!(!chemin.exists());
        assert!(chemin.with_extension("jsonl.old").exists());
        std::fs::write(&chemin, vec![b'y'; ROTATE_BYTES as usize]).unwrap();
        rotate_si_plein(&chemin).unwrap();
        let vieux = std::fs::read(chemin.with_extension("jsonl.old")).unwrap();
        assert_eq!(vieux[0], b'y');

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn la_version_est_reduite_a_un_dossier_sur() {
        assert_eq!(version_propre(Some("0.1.15")), "0.1.15");
        assert_eq!(version_propre(Some("../../etc")), "....etc");
        assert_eq!(version_propre(None), "inconnue");
        assert_eq!(version_propre(Some("")), "inconnue");
        // « .. » filtré reste « .. » : sans ce garde-fou, l'écriture
        // remonterait d'un dossier. Au rebut.
        assert_eq!(version_propre(Some("..")), "inconnue");
        assert_eq!(version_propre(Some("//")), "inconnue");
        // Et côté chemin, seuls nos dossiers passent.
        assert!(segment_version_valide("0.1.15"));
        assert!(segment_version_valide("inconnue"));
        assert!(!segment_version_valide(".."));
        assert!(!segment_version_valide(""));
        assert!(!segment_version_valide("a/b"));
    }

    #[test]
    fn le_resume_compte_par_version_triee() {
        let dir = std::env::temp_dir().join("ki-chat-diag-resume");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("0.1.15")).unwrap();
        std::fs::create_dir_all(dir.join("0.1.14")).unwrap();
        // Deux joueurs sur la 0.1.15 : sessions, réouverture, famine, erreur
        // et un crash se répartissent entre eux.
        std::fs::write(
            dir.join("0.1.15").join("00000001-a.jsonl"),
            "{\"type\":\"journal\",\"t\":1,\"msg\":\"moteur vocal démarré\"}\n\
             {\"type\":\"journal\",\"t\":2,\"msg\":\"micro perdu — réouverture\"}\n\
             {\"type\":\"journal\",\"t\":3,\"msg\":\"flux affamé, bascule proposée\"}\n\
             {\"type\":\"crash\",\"t\":4,\"rapport\":\"panic : boum\"}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("0.1.15").join("00000002-b.jsonl"),
            "{\"type\":\"journal\",\"t\":1,\"msg\":\"moteur vocal démarré\"}\n\
             {\"type\":\"journal\",\"t\":2,\"msg\":\"erreur d'ouverture du micro\"}\n",
        )
        .unwrap();
        // Un seul joueur sur la 0.1.14, mais avec une génération .old : elle
        // nourrit les compteurs, pas l'effectif.
        std::fs::write(
            dir.join("0.1.14").join("00000001-a.jsonl"),
            "{\"type\":\"journal\",\"t\":9,\"msg\":\"moteur vocal démarré\"}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("0.1.14").join("00000001-a.jsonl.old"),
            "{\"type\":\"journal\",\"t\":1,\"msg\":\"erreur ancienne\"}\n",
        )
        .unwrap();

        let resume = resume_versions(&dir);
        let lignes: Vec<&str> = resume.lines().collect();
        assert_eq!(lignes.len(), 3, "en-tête + une ligne par version : {resume}");
        assert!(lignes[0].starts_with("version\tjoueurs\tsessions\t"));
        // Triées par version, colonnes : joueurs, sessions, réouvertures,
        // famines, erreurs, crashs.
        assert!(lignes[1].starts_with("0.1.14\t1\t1\t0\t0\t1\t0\t"), "0.1.14 : {}", lignes[1]);
        assert!(lignes[2].starts_with("0.1.15\t2\t2\t1\t1\t1\t1\t"), "0.1.15 : {}", lignes[2]);

        // Et sans archives du tout, rien — l'appelant mettra les mots.
        let vide = std::env::temp_dir().join("ki-chat-diag-resume-vide");
        let _ = std::fs::remove_dir_all(&vide);
        assert_eq!(resume_versions(&vide), "");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn le_resume_ne_lit_que_des_lignes_entieres_de_la_fin() {
        let dir = std::env::temp_dir().join("ki-chat-diag-resume-fin");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let chemin = dir.join("00000001-a.jsonl");
        std::fs::write(&chemin, "erreur un\nerreur deux\nerreur trois\n").unwrap();

        // Assez de place : tout est lu.
        assert_eq!(
            fin_de_fichier(&chemin, 1024).unwrap(),
            "erreur un\nerreur deux\nerreur trois\n"
        );
        // Borne serrée : la ligne coupée en tête est écartée, seule la fin
        // entière reste — et elle seule compte.
        let fin = fin_de_fichier(&chemin, 18).unwrap();
        assert_eq!(fin, "erreur trois\n");
        let mut c = Compte::default();
        compter_lignes(&fin, &mut c);
        assert_eq!(c.erreurs, 1);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn seuls_nos_noms_de_fichiers_sont_lisibles() {
        // La validation de `lire` : nos noms passent, le reste non.
        let valide = |f: &str| {
            f.strip_suffix(".jsonl")
                .or_else(|| f.strip_suffix(".jsonl.old"))
                .is_some_and(|base| {
                    !base.is_empty()
                        && base.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
                })
        };
        assert!(valide("00000007-r_dik.jsonl"));
        assert!(valide("00000007-r_dik.jsonl.old"));
        assert!(!valide("../comptes.json"));
        assert!(!valide("00000007/../x.jsonl"));
        assert!(!valide(".jsonl"));
        assert!(!valide("diag.token"));
    }
}
