//! Les vidéos partagées, côté client : les reconnaître, savoir où le serveur
//! range leur fiche, et les garder sur le disque le temps de les regarder.
//!
//! Une vidéo du chat n'est qu'un lien vers notre serveur, comme une image.
//! À côté du fichier, le serveur tient `meta.json` — durée, dimensions,
//! poster, et l'état de la fabrication (une vidéo de téléphone est refaite en
//! MP4 lisible par tous avant d'être servie). Le client lit cette fiche pour
//! la carte dans le fil, puis télécharge le fichier **entier dans un cache
//! disque** avant de le lire : Media Foundation lit un fichier, pas un flux
//! sur notre certificat auto-signé, et un clip fait quelques dizaines de Mo —
//! une barre de progression de quelques secondes, puis l'avance est
//! instantanée.
//!
//! Même règle que les images : on ne télécharge que ce que **notre** serveur
//! héberge, avec le client HTTP épinglé sur son empreinte.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;

/// Extensions reconnues comme des vidéos. Le serveur les refait toutes en
/// `.mp4` ; les autres n'apparaissent que dans les liens d'avant.
const VIDEO_SUFFIXES: [&str; 6] = [".mp4", ".mov", ".webm", ".mkv", ".m4v", ".avi"];

/// Taille du cache disque des médias, au-delà de laquelle les plus anciens
/// partent.
pub const CACHE_MAX: u64 = 1024 * 1024 * 1024;

/// Temps accordé au téléchargement d'une vidéo.
const TIMEOUT: Duration = Duration::from_secs(600);

/// Vrai si l'adresse désigne une vidéo, d'après son extension.
pub fn looks_like_video(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_lowercase();
    VIDEO_SUFFIXES.iter().any(|suffix| path.ends_with(suffix))
}

/// La fiche d'une vidéo : `/files/<id>/<nom>` → `/files/<id>/meta.json`.
pub fn url_meta(url: &str) -> Option<String> {
    let sans = url.split(['?', '#']).next().unwrap_or(url);
    let (base, nom) = sans.rsplit_once('/')?;
    (!nom.is_empty() && base.contains("/files/")).then(|| format!("{base}/meta.json"))
}

/// Le nom du fichier, pour le titre de la visionneuse.
pub fn nom_du_fichier(url: &str) -> String {
    let sans = url.split(['?', '#']).next().unwrap_or(url);
    sans.rsplit_once('/')
        .map(|(_, n)| n)
        .unwrap_or(sans)
        .to_string()
}

/// La fiche telle que le serveur l'écrit (`serveur/medias.rs`).
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
pub struct Meta {
    #[serde(default)]
    pub etat: String,
    #[serde(default)]
    pub duree_s: f32,
    #[serde(default)]
    pub largeur: u32,
    #[serde(default)]
    pub hauteur: u32,
    /// Chemin du poster sur le serveur (« /files/<id>/poster.jpg »).
    #[serde(default)]
    pub poster: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

impl Meta {
    pub fn prete(&self) -> bool {
        self.etat == "pret"
    }

    pub fn en_erreur(&self) -> bool {
        self.etat == "erreur"
    }
}

/// Le dossier du cache, créé au besoin.
pub fn dossier_cache() -> Option<PathBuf> {
    let base = if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        PathBuf::from(local).join("ki-chat")
    } else {
        eframe::storage_dir("ki-chat")?
    };
    let dir = base.join("cache").join("medias");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// FNV-1a 64 bits : le nom du fichier en cache, c'est l'adresse.
fn empreinte(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Où cette adresse se range dans le cache. L'extension est reprise (courte
/// et sans surprise) pour que Media Foundation reconnaisse le conteneur.
pub fn chemin_cache(url: &str) -> Option<PathBuf> {
    let sans = url.split(['?', '#']).next().unwrap_or(url);
    let ext: String = sans
        .rsplit_once('.')
        .map(|(_, e)| e.to_lowercase())
        .filter(|e| e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "bin".into());
    Some(dossier_cache()?.join(format!("{:016x}.{ext}", empreinte(url))))
}

/// Fait le ménage : au-delà de `max` octets, les fichiers les moins
/// récemment touchés partent (les `.part` d'un téléchargement interrompu
/// aussi).
pub fn purger(dossier: &Path, max: u64) {
    let Ok(entrees) = std::fs::read_dir(dossier) else {
        return;
    };
    let mut fichiers: Vec<(std::time::SystemTime, u64, PathBuf)> = entrees
        .flatten()
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some((meta.modified().ok()?, meta.len(), e.path()))
        })
        .collect();
    let mut total: u64 = fichiers.iter().map(|f| f.1).sum();
    fichiers.sort_by_key(|f| f.0);
    for (_, taille, chemin) in fichiers {
        if total <= max {
            break;
        }
        if std::fs::remove_file(&chemin).is_ok() {
            total = total.saturating_sub(taille);
        }
    }
}

/// Un téléchargement en cours, suivi depuis l'interface.
pub struct Telechargement {
    pub recu: AtomicU64,
    /// 0 tant que le serveur n'a pas dit la taille.
    pub total: AtomicU64,
    pub fini: AtomicBool,
    pub erreur: Mutex<Option<String>>,
}

impl Telechargement {
    /// Avancement dans [0, 1], ou `None` si la taille est inconnue.
    pub fn avancement(&self) -> Option<f32> {
        let total = self.total.load(Ordering::Relaxed);
        (total > 0).then(|| (self.recu.load(Ordering::Relaxed) as f32 / total as f32).min(1.0))
    }
}

/// Télécharge `cible` (adresse épinglée, en TLS) dans `chemin`, sur un fil.
/// Le fichier n'apparaît sous son nom qu'une fois complet : entre-temps
/// c'est un `.part`, qu'une interruption laisse et que la purge ramasse.
pub fn telecharger(
    agent: ureq::Agent,
    cible: String,
    chemin: PathBuf,
    ctx: egui::Context,
) -> Arc<Telechargement> {
    let suivi = Arc::new(Telechargement {
        recu: AtomicU64::new(0),
        total: AtomicU64::new(0),
        fini: AtomicBool::new(false),
        erreur: Mutex::new(None),
    });
    let s = suivi.clone();
    std::thread::Builder::new()
        .name("medias-telechargement".into())
        .spawn(move || {
            let resultat = (|| -> Result<(), String> {
                let reponse = agent
                    .get(&cible)
                    .timeout(TIMEOUT)
                    .call()
                    .map_err(|e| e.to_string())?;
                if let Some(total) = reponse
                    .header("Content-Length")
                    .and_then(|v| v.trim().parse::<u64>().ok())
                {
                    s.total.store(total, Ordering::Relaxed);
                }
                let partiel = chemin.with_extension("part");
                let mut fichier = std::fs::File::create(&partiel).map_err(|e| e.to_string())?;
                let mut lecteur = reponse.into_reader();
                let mut tampon = vec![0u8; 256 * 1024];
                let mut depuis_repeint = 0u64;
                loop {
                    let n = lecteur.read(&mut tampon).map_err(|e| e.to_string())?;
                    if n == 0 {
                        break;
                    }
                    fichier.write_all(&tampon[..n]).map_err(|e| e.to_string())?;
                    s.recu.fetch_add(n as u64, Ordering::Relaxed);
                    depuis_repeint += n as u64;
                    if depuis_repeint > 2 * 1024 * 1024 {
                        depuis_repeint = 0;
                        ctx.request_repaint();
                    }
                }
                fichier.flush().map_err(|e| e.to_string())?;
                drop(fichier);
                std::fs::rename(&partiel, &chemin).map_err(|e| e.to_string())?;
                Ok(())
            })();
            if let Err(e) = resultat {
                *s.erreur.lock().unwrap() = Some(e);
            }
            s.fini.store(true, Ordering::Relaxed);
            if let Some(dossier) = chemin.parent() {
                purger(dossier, CACHE_MAX);
            }
            ctx.request_repaint();
        })
        .ok();
    suivi
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_extensions_are_recognised() {
        assert!(looks_like_video("https://s:8080/files/ab/clip.mp4"));
        assert!(looks_like_video("https://s:8080/files/ab/IMG_0001.MOV?v=1"));
        assert!(!looks_like_video("https://s:8080/files/ab/photo.png"));
        assert!(!looks_like_video("https://s:8080/files/ab/notes.txt#a.mp4"));
    }

    #[test]
    fn the_meta_sits_next_to_the_file() {
        assert_eq!(
            url_meta("https://s:8080/files/0123456789abcdef/clip.mp4").as_deref(),
            Some("https://s:8080/files/0123456789abcdef/meta.json")
        );
        assert_eq!(url_meta("https://s:8080/musique/vignette/x.jpg"), None);
        assert_eq!(
            nom_du_fichier("https://s:8080/files/ab/clip.mp4"),
            "clip.mp4"
        );
    }

    #[test]
    fn a_cache_path_keeps_a_sane_extension_only() {
        let Some(p) = chemin_cache("https://s:8080/files/ab/clip.mp4") else {
            return;
        };
        assert!(p.to_string_lossy().ends_with(".mp4"));
        let Some(p) = chemin_cache("https://s:8080/files/ab/bizarre.tar.gz%20x") else {
            return;
        };
        assert!(p.to_string_lossy().ends_with(".bin"), "{}", p.display());
        // Deux adresses différentes, deux fichiers différents.
        let a = chemin_cache("https://s:8080/files/aa/clip.mp4");
        let b = chemin_cache("https://s:8080/files/ab/clip.mp4");
        assert_ne!(a, b);
    }

    #[test]
    fn purging_removes_the_oldest_first() {
        let dossier = std::env::temp_dir().join(format!("ki-medias-purge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dossier);
        std::fs::create_dir_all(&dossier).unwrap();
        for (i, nom) in ["vieux.mp4", "moyen.mp4", "recent.mp4"].iter().enumerate() {
            std::fs::write(dossier.join(nom), vec![0u8; 100]).unwrap();
            let t = std::time::SystemTime::now() - Duration::from_secs(300 - 100 * i as u64);
            let f = std::fs::File::options()
                .write(true)
                .open(dossier.join(nom))
                .unwrap();
            f.set_modified(t).unwrap();
        }
        purger(&dossier, 250);
        assert!(!dossier.join("vieux.mp4").exists());
        assert!(dossier.join("moyen.mp4").exists());
        assert!(dossier.join("recent.mp4").exists());
        let _ = std::fs::remove_dir_all(&dossier);
    }

    #[test]
    fn the_meta_parses_what_the_server_writes() {
        let m: Meta = serde_json::from_str(
            r#"{"etat":"pret","duree_s":30.5,"largeur":1920,"hauteur":1080,"poster":"/files/ab/poster.jpg"}"#,
        )
        .unwrap();
        assert!(m.prete());
        assert_eq!(m.poster.as_deref(), Some("/files/ab/poster.jpg"));
        let m: Meta = serde_json::from_str(r#"{"etat":"en_preparation"}"#).unwrap();
        assert!(!m.prete() && !m.en_erreur());
    }
}
