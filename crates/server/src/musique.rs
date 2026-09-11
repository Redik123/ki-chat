//! Le bot musique — jalon M1, la chaîne (voir PLAN-MUSIQUE.md).
//!
//! Un membre virtuel « Musique » dans un salon vocal : le serveur tire le
//! son d'une adresse YouTube ou SoundCloud avec yt-dlp, le décode avec
//! ffmpeg en PCM 48 kHz stéréo, l'encode en Opus avec ki-opus, le chiffre
//! comme n'importe quel membre — la clé de session est la sienne — et
//! l'envoie aux pairs du salon par le relais voix existant. Aucun client
//! ne télécharge rien ; aucun fichier audio n'est écrit sur disque.
//!
//! Deux fils par piste, hors de la boucle asynchrone : yt-dlp et ffmpeg en
//! processus enfants reliés par un tube, et un fil qui lit ffmpeg par blocs
//! de 20 ms dans un canal borné à une seconde — ffmpeg avance plus vite que
//! le temps réel, le canal le retient. Une tâche tokio cadence à 20 ms :
//! un bloc, une trame, un datagramme par pair. Lâcher le récepteur du canal
//! arrête tout : le fil voit son envoi refusé, tue les deux enfants et
//! s'en va.
//!
//! Sans yt-dlp ou ffmpeg sur la machine, le bot n'existe pas : l'état dit
//! « indisponible » et les commandes répondent poliment.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self as canal, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ki_protocol::{
    ChannelId, CompteursMusique, EtatMusique, Piste, ResumePlaylist, ServerMsg, MUSIQUE_ID, VOICE_HEADER_LEN,
};
use serde::{Deserialize, Serialize};
use rand::Rng;

use crate::state::AppState;

/// Une trame : 20 ms à 48 kHz, stéréo entrelacée.
const TRAME: usize = 960 * 2;
/// Le canal entre ffmpeg et le cadenceur : une seconde de son.
const TAMPON_BLOCS: usize = 50;
/// Ce qu'on laisse s'accumuler avant de commencer à émettre.
const AMORCE_BLOCS: usize = 10;
const DEBIT_OPUS: i32 = 96_000;
/// Résoudre une adresse (titre, durée) ne doit pas durer plus que ça.
const DELAI_RESOLUTION: Duration = Duration::from_secs(40);
/// Sans un premier bloc de son au bout de ce délai, la piste est jetée.
const DELAI_PREMIER_SON: Duration = Duration::from_secs(60);
/// L'état est republié à cette cadence pendant la lecture, pour la
/// position.
const PUBLICATION: Duration = Duration::from_secs(5);
/// Les clients YouTube que yt-dlp imite, dans l'ordre. Vérifié le
/// 2026-09-11 : le client par défaut résout la vidéo mais son flux est
/// refusé au téléchargement (403) sans PO token, même depuis une IP
/// résidentielle ; `web_embedded` donne l'audio Opus sans rien demander,
/// `mweb` prend le relais pour les vidéos non intégrables, et le défaut
/// reste en dernier recours.
const CLIENTS_YOUTUBE: [&str; 3] = ["web_embedded", "mweb", "default"];

/// Les exécutables, et ce qui s'y ajoute.
pub struct Outils {
    yt_dlp: String,
    ffmpeg: String,
    /// `data/musique/cookies.txt`, s'il existe — déposé par l'admin.
    cookies: Option<PathBuf>,
    cache: PathBuf,
}

impl Outils {
    /// Les arguments communs à tout appel de yt-dlp — une seule vidéo,
    /// même si l'adresse porte une liste.
    fn args_yt_dlp(&self) -> Vec<String> {
        let mut args = self.args_yt_dlp_liste();
        args.insert(0, "--no-playlist".to_string());
        args
    }

    /// Les mêmes, pour une adresse de playlist.
    fn args_yt_dlp_liste(&self) -> Vec<String> {
        let mut args = vec![
            "--no-warnings".to_string(),
            "--no-progress".to_string(),
            "--cache-dir".to_string(),
            self.cache.to_string_lossy().into_owned(),
        ];
        if let Some(c) = &self.cookies {
            args.push("--cookies".to_string());
            args.push(c.to_string_lossy().into_owned());
        }
        args
    }
}

/// Ce que le bot peut recevoir.
pub enum Commande {
    Rejoindre {
        salon: ChannelId,
    },
    Ajouter {
        piste: Piste,
        maintenant: bool,
    },
    /// Une playlist entière — devant la file si `maintenant`.
    AjouterPlusieurs(Vec<Piste>, bool),
    Retirer(usize),
    Deplacer(usize, usize),
    PlaylistEnregistrer(String),
    PlaylistCharger(String, bool),
    PlaylistSupprimer(String),
    PlaylistAjouterPiste(String, Piste),
    Lecture,
    Pause,
    Suivant,
    Vider,
    Volume(u8),
    Arreter,
    /// Une erreur à montrer (adresse illisible…), sans rien changer.
    Erreur(String),
}

/// Ce que le bot a fait depuis le démarrage, pour sa fiche.
#[derive(Default)]
struct Compteurs {
    pistes_jouees: AtomicU32,
    echecs: AtomicU32,
    premier_son_total_ms: AtomicU64,
    premier_son_n: AtomicU32,
}

/// Les vignettes connues : notre identifiant → l'adresse d'origine, et les
/// octets une fois tirés. Les clients ne parlent qu'à ki-chat ; c'est le
/// serveur qui va chercher l'image chez YouTube ou SoundCloud, une fois.
/// L'adresse d'origine d'une vignette, et ses octets une fois tirés.
type Vignette = (String, Option<Arc<Vec<u8>>>);

#[derive(Default)]
struct Vignettes {
    entrees: HashMap<String, Vignette>,
    ordre: VecDeque<String>,
}

/// Entrées gardées, et octets gardés (les plus récentes).
const VIGNETTES_MAX: usize = 200;
const VIGNETTE_OCTETS_MAX: u64 = 600 * 1024;

/// Ce qui survit à un redémarrage : le salon, la file (piste en cours en
/// tête), le volume. Rien de la position — la piste reprend du début, en
/// pause, quand un modérateur le demande.
#[derive(Default, Serialize, Deserialize)]
struct Sauvegarde {
    #[serde(default)]
    salon: Option<ChannelId>,
    #[serde(default)]
    file: Vec<Piste>,
    #[serde(default)]
    volume: u8,
}

/// Seul dans le salon depuis ce temps, le bot se met en pause ; depuis
/// celui-là, il s'en va.
const SOLITUDE_PAUSE: Duration = Duration::from_secs(5 * 60);
const SOLITUDE_DEPART: Duration = Duration::from_secs(30 * 60);

pub struct Musique {
    etat: Mutex<EtatMusique>,
    tx: tokio::sync::mpsc::UnboundedSender<Commande>,
    rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<Commande>>>,
    outils: Option<Arc<Outils>>,
    compteurs: Compteurs,
    vignettes: Mutex<Vignettes>,
    /// Les playlists du groupe, par nom.
    playlists: Mutex<BTreeMap<String, Vec<Piste>>>,
    dossier: PathBuf,
}

impl Musique {
    /// Cherche yt-dlp et ffmpeg (variables `KI_YTDLP` / `KI_FFMPEG`, sinon
    /// le PATH) ; sans eux, le bot est indisponible et le dit.
    pub fn new(data_dir: &str) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let outils = detecter(data_dir).map(Arc::new);
        let dossier = PathBuf::from(data_dir).join("musique");
        let playlists: BTreeMap<String, Vec<Piste>> = std::fs::read_to_string(dossier.join("playlists.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let sauve: Sauvegarde = std::fs::read_to_string(dossier.join("file.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        // La file d'avant le redémarrage attend, en pause, que quelqu'un
        // relance ; le salon aussi, si le bot en avait un.
        let etat = EtatMusique {
            disponible: outils.is_some(),
            volume: if sauve.volume == 0 { 60 } else { sauve.volume.min(100) },
            salon: sauve.salon.filter(|_| outils.is_some() && !sauve.file.is_empty()),
            file: sauve.file,
            ..Default::default()
        };
        if !etat.file.is_empty() {
            tracing::info!("musique : {} piste(s) en file reprises du redémarrage, en pause", etat.file.len());
        }
        Self {
            etat: Mutex::new(etat),
            tx,
            rx: Mutex::new(Some(rx)),
            outils,
            compteurs: Compteurs::default(),
            vignettes: Mutex::new(Vignettes::default()),
            playlists: Mutex::new(playlists),
            dossier,
        }
    }

    fn sauver_playlists(&self) {
        let playlists = self.playlists.lock().unwrap();
        if let Ok(json) = serde_json::to_vec_pretty(&*playlists) {
            let _ = std::fs::create_dir_all(&self.dossier);
            if let Err(e) = crate::store::write_atomic(&self.dossier.join("playlists.json"), &json) {
                tracing::warn!("musique : playlists non écrites : {e}");
            }
        }
    }

    /// La file telle qu'elle est, pour la retrouver au redémarrage.
    fn sauver_file(&self) {
        let e = self.etat.lock().unwrap();
        let mut file = e.file.clone();
        if let Some(p) = &e.en_cours {
            file.insert(0, p.clone());
        }
        let sauve = Sauvegarde { salon: e.salon, file, volume: e.volume };
        drop(e);
        if let Ok(json) = serde_json::to_vec_pretty(&sauve) {
            let _ = std::fs::create_dir_all(&self.dossier);
            let _ = crate::store::write_atomic(&self.dossier.join("file.json"), &json);
        }
    }

    /// Remplace la vignette d'une piste (une adresse chez YouTube ou
    /// SoundCloud) par un chemin chez nous, que le client demandera au
    /// serveur.
    pub fn localiser_vignette(&self, piste: &mut Piste) {
        let Some(url) = piste.vignette.take() else { return };
        if !url.starts_with("https://") || url.len() > 400 {
            return;
        }
        let id = empreinte(&url);
        let mut v = self.vignettes.lock().unwrap();
        if !v.entrees.contains_key(&id) {
            v.entrees.insert(id.clone(), (url, None));
            v.ordre.push_back(id.clone());
            while v.ordre.len() > VIGNETTES_MAX {
                if let Some(vieille) = v.ordre.pop_front() {
                    v.entrees.remove(&vieille);
                }
            }
        }
        piste.vignette = Some(format!("/musique/vignette/{id}.jpg"));
    }

    /// L'image d'une vignette, tirée une fois chez sa source. Bloquant.
    fn vignette_octets(&self, id: &str) -> Option<Arc<Vec<u8>>> {
        let (url, octets) = self.vignettes.lock().unwrap().entrees.get(id).cloned()?;
        if let Some(o) = octets {
            return Some(o);
        }
        let mut corps = Vec::new();
        ureq::get(&url)
            .set("User-Agent", "ki-chat")
            .timeout(Duration::from_secs(10))
            .call()
            .ok()?
            .into_reader()
            .take(VIGNETTE_OCTETS_MAX)
            .read_to_end(&mut corps)
            .ok()?;
        if corps.is_empty() {
            return None;
        }
        let corps = Arc::new(corps);
        let mut v = self.vignettes.lock().unwrap();
        if let Some(e) = v.entrees.get_mut(id) {
            e.1 = Some(Arc::clone(&corps));
        }
        // Les octets ne sont gardés que pour les vignettes récentes.
        let gardees: Vec<String> = v.ordre.iter().rev().take(64).cloned().collect();
        for (k, e) in v.entrees.iter_mut() {
            if !gardees.contains(k) {
                e.1 = None;
            }
        }
        Some(corps)
    }

    pub fn disponible(&self) -> bool {
        self.outils.is_some()
    }

    pub fn etat(&self) -> EtatMusique {
        let mut e = self.etat.lock().unwrap().clone();
        e.playlists = self
            .playlists
            .lock()
            .unwrap()
            .iter()
            .map(|(nom, pistes)| ResumePlaylist {
                nom: nom.clone(),
                pistes: pistes.len() as u32,
                duree_s: pistes.iter().map(|p| p.duree_s).sum(),
            })
            .collect();
        let n = self.compteurs.premier_son_n.load(Ordering::Relaxed);
        e.compteurs = CompteursMusique {
            pistes_jouees: self.compteurs.pistes_jouees.load(Ordering::Relaxed),
            echecs: self.compteurs.echecs.load(Ordering::Relaxed),
            premier_son_ms: if n > 0 {
                (self.compteurs.premier_son_total_ms.load(Ordering::Relaxed) / n as u64) as u32
            } else {
                0
            },
        };
        e
    }

    pub fn commander(&self, commande: Commande) {
        let _ = self.tx.send(commande);
    }

    pub fn outils(&self) -> Option<Arc<Outils>> {
        self.outils.clone()
    }
}

fn detecter(data_dir: &str) -> Option<Outils> {
    let yt_dlp = std::env::var("KI_YTDLP").unwrap_or_else(|_| "yt-dlp".into());
    let ffmpeg = std::env::var("KI_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    let version = |exe: &str, arg: &str| -> Option<String> {
        let sortie = executer_borne(Command::new(exe).arg(arg), Duration::from_secs(20)).ok()?;
        let texte = String::from_utf8_lossy(&sortie);
        texte.lines().next().map(|l| l.chars().take(60).collect())
    };
    let (Some(v1), Some(v2)) = (version(&yt_dlp, "--version"), version(&ffmpeg, "-version")) else {
        tracing::info!("musique : yt-dlp ou ffmpeg introuvable — bot musique indisponible");
        return None;
    };
    tracing::info!("musique : yt-dlp {v1} · {v2}");
    let dossier = PathBuf::from(data_dir).join("musique");
    let _ = std::fs::create_dir_all(dossier.join("cache"));
    let cookies = dossier.join("cookies.txt");
    Some(Outils {
        yt_dlp,
        ffmpeg,
        cookies: cookies.exists().then_some(cookies),
        cache: dossier.join("cache"),
    })
}

/// Lance la commande, lit sa sortie standard, et la tue si elle dépasse le
/// délai — un extracteur qui traîne ne bloque jamais le serveur.
fn executer_borne(cmd: &mut Command, delai: Duration) -> Result<Vec<u8>, String> {
    let mut enfant = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("lancement impossible : {e}"))?;
    let mut sortie = enfant.stdout.take().expect("stdout");
    let mut erreur = enfant.stderr.take().expect("stderr");
    // Lecture sur un fil à part : le tube doit être vidé pendant qu'on
    // surveille le délai, sinon un enfant bavard se bloque dessus.
    let lecteur = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = sortie.read_to_end(&mut buf);
        buf
    });
    let lecteur_err = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = erreur.read_to_end(&mut buf);
        buf
    });
    let debut = Instant::now();
    let statut = loop {
        match enfant.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if debut.elapsed() > delai => {
                let _ = enfant.kill();
                let _ = enfant.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break None,
        }
    };
    let sortie = lecteur.join().unwrap_or_default();
    let erreur = lecteur_err.join().unwrap_or_default();
    match statut {
        Some(s) if s.success() => Ok(sortie),
        Some(_) => Err(resume_erreur(&erreur)),
        None => Err("délai dépassé".into()),
    }
}

/// FNV-1a sur 64 bits, en hexadécimal : l'identifiant d'une vignette.
fn empreinte(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for o in s.bytes() {
        h ^= o as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// `GET /musique/vignette/<id>.jpg` : l'image d'une piste, servie par
/// ki-chat pour que les clients ne parlent jamais à YouTube.
pub async fn vignette(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let id = id.trim_end_matches(".jpg").to_string();
    if id.len() != 16 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    }
    let s = state.clone();
    let octets = tokio::task::spawn_blocking(move || s.musique.vignette_octets(&id)).await.ok().flatten();
    let Some(octets) = octets else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    let genre = match octets.get(..4) {
        Some([0x89, b'P', b'N', b'G']) => "image/png",
        Some([b'R', b'I', b'F', b'F']) => "image/webp",
        _ => "image/jpeg",
    };
    (
        [(axum::http::header::CONTENT_TYPE, genre), (axum::http::header::CACHE_CONTROL, "public, max-age=86400")],
        octets.as_ref().clone(),
    )
        .into_response()
}

/// Une adresse de playlist : YouTube (`/playlist?list=…`, YouTube Music
/// compris) ou un « set » SoundCloud. Une vidéo avec `list=` dans
/// l'adresse reste une vidéo — c'est un lien partagé, pas une playlist.
pub fn est_liste(url: &str) -> bool {
    let chemin = url.strip_prefix("https://").and_then(|r| r.find('/').map(|i| &r[i..])).unwrap_or("");
    chemin.starts_with("/playlist") || chemin.contains("/sets/")
}

/// Cherche dix pistes sur YouTube ou SoundCloud, sans rien télécharger :
/// yt-dlp en mode « liste à plat », une ligne JSON par résultat.
/// Bloquant : hors de la boucle asynchrone.
pub fn chercher(outils: &Outils, texte: &str, source: &str) -> Result<Vec<Piste>, String> {
    let (prefixe, nom_source) = if source == "soundcloud" { ("scsearch10:", "soundcloud") } else { ("ytsearch10:", "youtube") };
    let mut cmd = Command::new(&outils.yt_dlp);
    cmd.args(outils.args_yt_dlp())
        .args(["-j", "--flat-playlist", "--skip-download", "--"])
        .arg(format!("{prefixe}{texte}"));
    let sortie = executer_borne(&mut cmd, DELAI_RESOLUTION)?;
    Ok(pistes_a_plat(&sortie, nom_source))
}

/// Toutes les pistes d'une playlist, à plat — deux cents au plus — sans
/// rien télécharger. Bloquant.
pub fn resoudre_liste(outils: &Outils, url: &str) -> Result<Vec<Piste>, String> {
    let source = if url.contains("soundcloud.com") { "soundcloud" } else { "youtube" };
    let mut cmd = Command::new(&outils.yt_dlp);
    cmd.args(outils.args_yt_dlp_liste())
        .args(["-j", "--flat-playlist", "--skip-download", "--playlist-end"])
        .arg(ki_protocol::MAX_PISTES_PLAYLIST.to_string())
        .arg("--")
        .arg(url);
    let sortie = executer_borne(&mut cmd, Duration::from_secs(60))?;
    let pistes = pistes_a_plat(&sortie, source);
    if pistes.is_empty() {
        return Err("playlist vide ou introuvable".into());
    }
    Ok(pistes)
}

/// Les lignes JSON d'une sortie « à plat », en pistes.
fn pistes_a_plat(sortie: &[u8], nom_source: &str) -> Vec<Piste> {
    let mut pistes = Vec::new();
    for ligne in String::from_utf8_lossy(sortie).lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(ligne) else { continue };
        let Some(url) = v["webpage_url"].as_str().or_else(|| v["url"].as_str()) else { continue };
        if !ki_protocol::url_musique_valide(url) {
            continue;
        }
        let titre = v["title"].as_str().unwrap_or("").to_string();
        if titre.is_empty() || titre == "[Private video]" || titre == "[Deleted video]" {
            continue;
        }
        let artiste = ["artist", "uploader", "channel", "creator"]
            .iter()
            .find_map(|k| v[*k].as_str())
            .unwrap_or("")
            .to_string();
        pistes.push(Piste {
            source: nom_source.to_string(),
            url: url.to_string(),
            titre: ki_protocol::safe_display(&titre, 160),
            artiste: ki_protocol::safe_display(&artiste, 80),
            duree_s: v["duration"].as_f64().unwrap_or(0.0).max(0.0) as u32,
            vignette: meilleure_vignette(&v),
            ajoute_par: None,
        });
    }
    pistes
}

/// Parmi les vignettes d'un résultat, une de taille moyenne : chez YouTube
/// la première d'au moins 300 px de large ; chez SoundCloud, qui ne liste
/// que des miniatures, la même adresse en 300×300.
fn meilleure_vignette(v: &serde_json::Value) -> Option<String> {
    if let Some(t) = v["thumbnail"].as_str() {
        return Some(t.to_string());
    }
    let liste = v["thumbnails"].as_array()?;
    let moyenne = liste
        .iter()
        .filter(|t| t["width"].as_u64().is_some_and(|w| w >= 300))
        .min_by_key(|t| t["width"].as_u64().unwrap_or(u64::MAX))
        .or_else(|| liste.last())?;
    let url = moyenne["url"].as_str()?;
    if url.contains("sndcdn.com") {
        if let Some(pos) = url.rfind('-') {
            return Some(format!("{}-t300x300.jpg", &url[..pos]));
        }
    }
    Some(url.to_string())
}

/// La dernière ligne utile de la sortie d'erreur, bornée.
fn resume_erreur(stderr: &[u8]) -> String {
    let texte = String::from_utf8_lossy(stderr);
    let ligne = texte
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("échec");
    let ligne = ligne.trim_start_matches("ERROR: ");
    ligne.chars().take(160).collect()
}

/// Résout une adresse en piste — titre, artiste, durée, vignette — sans
/// rien télécharger. Bloquant : à appeler hors de la boucle asynchrone.
pub fn resoudre(outils: &Outils, url: &str) -> Result<Piste, String> {
    let mut cmd = Command::new(&outils.yt_dlp);
    cmd.args(outils.args_yt_dlp())
        .args(["--dump-single-json", "--skip-download", "--"])
        .arg(url);
    let sortie = executer_borne(&mut cmd, DELAI_RESOLUTION)?;
    let v: serde_json::Value =
        serde_json::from_slice(&sortie).map_err(|_| "réponse illisible".to_string())?;
    let titre = v["title"].as_str().unwrap_or(url).to_string();
    let artiste = ["artist", "uploader", "channel", "creator"]
        .iter()
        .find_map(|k| v[*k].as_str())
        .unwrap_or("")
        .to_string();
    let source = match v["extractor_key"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        s if s.starts_with("youtube") => "youtube",
        s if s.starts_with("soundcloud") => "soundcloud",
        _ => "autre",
    }
    .to_string();
    Ok(Piste {
        source,
        url: v["webpage_url"].as_str().unwrap_or(url).to_string(),
        titre: ki_protocol::safe_display(&titre, 160),
        artiste: ki_protocol::safe_display(&artiste, 80),
        duree_s: v["duration"].as_f64().unwrap_or(0.0).max(0.0) as u32,
        vignette: v["thumbnail"].as_str().map(str::to_string),
        ajoute_par: None,
    })
}

// ---------------------------------------------------------------------------
// La lecture d'une piste
// ---------------------------------------------------------------------------

/// Une piste en cours de lecture : le canal de blocs PCM, et ce que le fil
/// de pompage raconte.
struct Lecteur {
    rx: canal::Receiver<Vec<f32>>,
    pret: Arc<AtomicBool>,
    fini: Arc<AtomicBool>,
    erreur: Arc<Mutex<Option<String>>>,
    position_ms: u64,
    demarre: Instant,
    /// Le délai jusqu'au premier son a été compté.
    mesure: bool,
}

impl Lecteur {
    fn demarrer(outils: Arc<Outils>, url: String) -> Self {
        let (tx, rx) = canal::sync_channel::<Vec<f32>>(TAMPON_BLOCS);
        let pret = Arc::new(AtomicBool::new(false));
        let fini = Arc::new(AtomicBool::new(false));
        let erreur = Arc::new(Mutex::new(None));
        let (p, f, e) = (Arc::clone(&pret), Arc::clone(&fini), Arc::clone(&erreur));
        std::thread::Builder::new()
            .name("ki-musique".into())
            .spawn(move || {
                // YouTube : un client après l'autre tant qu'aucun son n'est
                // sorti ; une piste qui a commencé ne se relance pas.
                let clients: &[Option<&str>] = if url.contains("youtu") {
                    &[
                        Some(CLIENTS_YOUTUBE[0]),
                        Some(CLIENTS_YOUTUBE[1]),
                        Some(CLIENTS_YOUTUBE[2]),
                    ]
                } else {
                    &[None]
                };
                let mut derniere = None;
                for client in clients {
                    match pomper(&outils, &url, *client, &tx, &p) {
                        Ok(envoyes) if envoyes > 0 => {
                            derniere = None;
                            break;
                        }
                        Ok(_) => derniere = Some("aucun son".to_string()),
                        Err(err) => {
                            tracing::info!("musique : {} — {err}", client.unwrap_or("source"));
                            derniere = Some(err);
                        }
                    }
                    if tx.send(Vec::new()).is_err() {
                        break; // le lecteur a été lâché entre deux essais
                    }
                }
                if let Some(err) = derniere {
                    *e.lock().unwrap() = Some(err);
                }
                f.store(true, Ordering::Relaxed);
                p.store(true, Ordering::Relaxed);
            })
            .ok();
        Self {
            rx,
            pret,
            fini,
            erreur,
            position_ms: 0,
            demarre: Instant::now(),
            mesure: false,
        }
    }
}

/// Un enfant qu'on n'oublie pas : tué s'il est encore là quand on le lâche.
struct Enfant(Child);

impl Drop for Enfant {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// yt-dlp → ffmpeg → blocs de 20 ms dans le canal. Rend le nombre de blocs
/// envoyés quand la piste est finie ou que le récepteur a été lâché ; une
/// erreur avant le premier bloc dit pourquoi. `client` : le client YouTube
/// à imiter, `None` pour le choix de yt-dlp.
fn pomper(
    outils: &Outils,
    url: &str,
    client: Option<&str>,
    tx: &canal::SyncSender<Vec<f32>>,
    pret: &AtomicBool,
) -> Result<usize, String> {
    let mut yt = Command::new(&outils.yt_dlp);
    yt.args(outils.args_yt_dlp());
    if let Some(c) = client.filter(|c| *c != "default") {
        yt.args(["--extractor-args", &format!("youtube:player_client={c}")]);
    }
    yt.args(["-f", "bestaudio/best", "-o", "-", "--quiet", "--"])
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut yt = Enfant(yt.spawn().map_err(|e| format!("yt-dlp : {e}"))?);
    let flux = yt.0.stdout.take().expect("stdout yt-dlp");
    let mut yt_err = yt.0.stderr.take().expect("stderr yt-dlp");
    let mut ff = Command::new(&outils.ffmpeg);
    ff.args([
        "-loglevel",
        "error",
        "-i",
        "pipe:0",
        "-vn",
        "-f",
        "f32le",
        "-ar",
        "48000",
        "-ac",
        "2",
        "pipe:1",
    ])
    .stdin(Stdio::from(flux))
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    let mut ff = Enfant(ff.spawn().map_err(|e| format!("ffmpeg : {e}"))?);
    let mut pcm = ff.0.stdout.take().expect("stdout ffmpeg");

    let mut octets = vec![0u8; TRAME * 4];
    let mut envoyes = 0usize;
    loop {
        // Un bloc entier, ou ce qui reste à la fin.
        let mut lu = 0;
        while lu < octets.len() {
            match pcm.read(&mut octets[lu..]) {
                Ok(0) => break,
                Ok(n) => lu += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        if lu == 0 {
            break;
        }
        octets[lu..].fill(0);
        let bloc: Vec<f32> = octets
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect();
        if tx.send(bloc).is_err() {
            // Le cadenceur a lâché le canal : on arrête tout.
            return Ok(envoyes.max(1));
        }
        envoyes += 1;
        if envoyes == AMORCE_BLOCS {
            pret.store(true, Ordering::Relaxed);
        }
    }
    if envoyes == 0 {
        let mut err = Vec::new();
        let _ = yt_err.read_to_end(&mut err);
        return Err(resume_erreur(&err));
    }
    Ok(envoyes)
}

// ---------------------------------------------------------------------------
// L'émission
// ---------------------------------------------------------------------------

/// L'encodeur Opus et le chiffrement du bot : la clé de session du serveur,
/// un compteur qui part d'un tirage, comme chez les membres.
struct Emetteur {
    opus: ki_opus::Encoder,
    cipher: XChaCha20Poly1305,
    compteur: u64,
    sortie: Vec<u8>,
}

impl Emetteur {
    fn new(cle: &[u8; 32]) -> Result<Self, String> {
        let mut opus = ki_opus::Encoder::new(
            48_000,
            ki_opus::Channels::Stereo,
            ki_opus::Application::Audio,
        )
        .map_err(|e| format!("encodeur Opus : {e:?}"))?;
        let _ = opus.set_bitrate(ki_opus::Bitrate::Bits(DEBIT_OPUS));
        let _ = opus.set_complexity(5);
        Ok(Self {
            opus,
            cipher: XChaCha20Poly1305::new(cle.into()),
            compteur: rand::rng().random::<u64>() >> 1,
            sortie: vec![0u8; 1400],
        })
    }

    fn trame(&mut self, pcm: &[f32]) -> Option<bytes::Bytes> {
        let n = self.opus.encode_float(pcm, &mut self.sortie).ok()?;
        let mut nonce = [0u8; 24];
        nonce[..8].copy_from_slice(&MUSIQUE_ID.to_le_bytes());
        nonce[8..16].copy_from_slice(&self.compteur.to_le_bytes());
        let chiffre = self
            .cipher
            .encrypt(&XNonce::from(nonce), &self.sortie[..n])
            .ok()?;
        let mut paquet = vec![0u8; VOICE_HEADER_LEN + chiffre.len()];
        ki_protocol::write_voice_header(&mut paquet, MUSIQUE_ID, self.compteur);
        paquet[VOICE_HEADER_LEN..].copy_from_slice(&chiffre);
        self.compteur = self.compteur.wrapping_add(1);
        Some(bytes::Bytes::from(paquet))
    }
}

fn envoyer(state: &AppState, salon: ChannelId, paquet: bytes::Bytes) {
    let routes = state.voice_routes.read().unwrap();
    if let Some(pairs) = routes.peers.get(&salon) {
        for (_, conn) in pairs {
            let _ = conn.send_datagram(paquet.clone());
        }
    }
}

// ---------------------------------------------------------------------------
// La boucle
// ---------------------------------------------------------------------------

/// Publie l'état à tout le monde — et le roster quand le bot apparaît,
/// disparaît ou se tait.
fn publier(state: &AppState, roster: bool) {
    let etat = state.musique.etat();
    state.broadcast_all(&ServerMsg::MusiqueEtat { etat });
    if roster {
        state.broadcast_all(&ServerMsg::Members {
            members: state.roster(),
        });
    }
}

/// Passe à la piste suivante de la file — ou s'arrête d'attendre s'il n'y
/// en a plus. `erreur` : ce que la piste précédente a laissé.
fn suivante(
    state: &AppState,
    outils: &Arc<Outils>,
    lecteur: &mut Option<Lecteur>,
    erreur: Option<String>,
) {
    *lecteur = None;
    let prochaine = {
        let mut e = state.musique.etat.lock().unwrap();
        e.erreur = erreur;
        e.position_ms = 0;
        if e.file.is_empty() {
            e.en_cours = None;
            None
        } else {
            let p = e.file.remove(0);
            e.en_cours = Some(p.clone());
            Some(p)
        }
    };
    if let Some(p) = prochaine {
        tracing::info!("musique : lecture de « {} »", p.titre);
        *lecteur = Some(Lecteur::demarrer(Arc::clone(outils), p.url));
    }
    state.musique.sauver_file();
    publier(state, true);
}

fn appliquer(
    state: &AppState,
    outils: &Arc<Outils>,
    lecteur: &mut Option<Lecteur>,
    commande: Commande,
) {
    match commande {
        Commande::Rejoindre { salon } => {
            let change = {
                let mut e = state.musique.etat.lock().unwrap();
                let change = e.salon != Some(salon);
                e.salon = Some(salon);
                change
            };
            publier(state, change);
        }
        Commande::Ajouter { piste, maintenant } => {
            let demarrer = {
                let mut e = state.musique.etat.lock().unwrap();
                e.erreur = None;
                if maintenant {
                    e.file.insert(0, piste);
                    true
                } else {
                    e.file.push(piste);
                    e.en_cours.is_none()
                }
            };
            if demarrer {
                let mut e = state.musique.etat.lock().unwrap();
                e.lecture = true;
                drop(e);
                suivante(state, outils, lecteur, None);
            } else {
                publier(state, false);
            }
        }
        Commande::AjouterPlusieurs(pistes, maintenant) => {
            let demarrer = {
                let mut e = state.musique.etat.lock().unwrap();
                e.erreur = None;
                let place = ki_protocol::MAX_FILE_MUSIQUE.saturating_sub(e.file.len());
                let pistes: Vec<Piste> = pistes.into_iter().take(place).collect();
                if pistes.is_empty() {
                    e.erreur = Some("la file est pleine".into());
                    false
                } else if maintenant {
                    let reste = std::mem::take(&mut e.file);
                    e.file = pistes;
                    e.file.extend(reste);
                    true
                } else {
                    e.file.extend(pistes);
                    e.en_cours.is_none()
                }
            };
            if demarrer {
                state.musique.etat.lock().unwrap().lecture = true;
                suivante(state, outils, lecteur, None);
            } else {
                publier(state, false);
            }
        }
        Commande::Retirer(index) => {
            let mut e = state.musique.etat.lock().unwrap();
            if index < e.file.len() {
                e.file.remove(index);
            }
            drop(e);
            publier(state, false);
        }
        Commande::Deplacer(de, vers) => {
            let mut e = state.musique.etat.lock().unwrap();
            if de < e.file.len() {
                let p = e.file.remove(de);
                let vers = vers.min(e.file.len());
                e.file.insert(vers, p);
            }
            drop(e);
            publier(state, false);
        }
        Commande::Lecture => {
            let relancer = {
                let mut e = state.musique.etat.lock().unwrap();
                e.lecture = true;
                e.en_cours.is_none() && !e.file.is_empty()
            };
            if relancer {
                suivante(state, outils, lecteur, None);
            } else {
                publier(state, true);
            }
        }
        Commande::Pause => {
            state.musique.etat.lock().unwrap().lecture = false;
            publier(state, true);
        }
        Commande::Suivant => suivante(state, outils, lecteur, None),
        Commande::Vider => {
            state.musique.etat.lock().unwrap().file.clear();
            publier(state, false);
        }
        Commande::Volume(v) => {
            state.musique.etat.lock().unwrap().volume = v.min(100);
            publier(state, false);
        }
        Commande::Arreter => {
            *lecteur = None;
            {
                let mut e = state.musique.etat.lock().unwrap();
                e.file.clear();
                e.en_cours = None;
                e.lecture = false;
                e.salon = None;
                e.position_ms = 0;
                e.erreur = None;
            }
            publier(state, true);
        }
        Commande::Erreur(message) => {
            state.musique.etat.lock().unwrap().erreur = Some(message);
            publier(state, false);
            return;
        }
        Commande::PlaylistEnregistrer(nom) => {
            let pistes: Vec<Piste> = {
                let e = state.musique.etat.lock().unwrap();
                e.en_cours.iter().chain(e.file.iter()).take(ki_protocol::MAX_PISTES_PLAYLIST).cloned().collect()
            };
            let mut playlists = state.musique.playlists.lock().unwrap();
            if pistes.is_empty() {
                state.musique.etat.lock().unwrap().erreur = Some("rien à enregistrer : la file est vide".into());
            } else if !playlists.contains_key(&nom) && playlists.len() >= ki_protocol::MAX_PLAYLISTS {
                state.musique.etat.lock().unwrap().erreur = Some("trop de playlists — supprime-en une".into());
            } else {
                playlists.insert(nom, pistes);
            }
            drop(playlists);
            state.musique.sauver_playlists();
            publier(state, false);
        }
        Commande::PlaylistCharger(nom, remplacer) => {
            let pistes = state.musique.playlists.lock().unwrap().get(&nom).cloned();
            let Some(pistes) = pistes else {
                state.musique.etat.lock().unwrap().erreur = Some("playlist inconnue".into());
                publier(state, false);
                return;
            };
            let demarrer = {
                let mut e = state.musique.etat.lock().unwrap();
                e.erreur = None;
                if remplacer {
                    e.file = pistes;
                    e.lecture = true;
                    true
                } else {
                    e.file.extend(pistes);
                    e.en_cours.is_none()
                }
            };
            if demarrer {
                state.musique.etat.lock().unwrap().lecture = true;
                suivante(state, outils, lecteur, None);
            } else {
                publier(state, false);
            }
        }
        Commande::PlaylistSupprimer(nom) => {
            state.musique.playlists.lock().unwrap().remove(&nom);
            state.musique.sauver_playlists();
            publier(state, false);
        }
        Commande::PlaylistAjouterPiste(nom, piste) => {
            let mut playlists = state.musique.playlists.lock().unwrap();
            let message = if !playlists.contains_key(&nom) && playlists.len() >= ki_protocol::MAX_PLAYLISTS {
                Some("trop de playlists — supprime-en une".to_string())
            } else {
                let liste = playlists.entry(nom.clone()).or_default();
                if liste.iter().any(|p| p.url == piste.url) {
                    Some(format!("déjà dans « {nom} »"))
                } else if liste.len() >= ki_protocol::MAX_PISTES_PLAYLIST {
                    Some(format!("« {nom} » est pleine"))
                } else {
                    liste.push(piste);
                    None
                }
            };
            drop(playlists);
            state.musique.sauver_playlists();
            state.musique.etat.lock().unwrap().erreur = message;
            publier(state, false);
        }
    }
    state.musique.sauver_file();
}

/// La tâche du bot : commandes d'un côté, cadence de 20 ms de l'autre.
pub async fn boucle(state: Arc<AppState>) {
    let Some(outils) = state.musique.outils() else {
        return;
    };
    let Some(mut rx) = state.musique.rx.lock().unwrap().take() else {
        return;
    };
    let mut emetteur = match Emetteur::new(&state.voice_key) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("musique : {e} — bot désactivé");
            return;
        }
    };
    let mut lecteur: Option<Lecteur> = None;
    // La cadence se tient à l'horloge, pas au minuteur. Sur Windows le
    // minuteur a un grain de 15,6 ms : un intervalle de 20 ms « en retard »
    // y prenait 31 ms, le flux tournait aux deux tiers de sa vitesse et le
    // client se retrouvait à sec toutes les secondes — la « pause de 0,1 s »
    // du premier essai. Ici on dort jusqu'à l'échéance, puis on envoie
    // **toutes** les trames dues : réveillé avec 30 ms de retard, deux
    // trames partent d'un coup, et le débit moyen reste exact. Une machine
    // en retard de plus d'une seconde (veille, pause du débogueur) se recale
    // sans rafale.
    #[cfg(windows)]
    unsafe {
        // Le grain du minuteur Windows passe à 1 ms pour ce processus : la
        // machine de développement cadence aussi bien que le serveur Linux.
        windows::Win32::Media::timeBeginPeriod(1);
    }
    let mut echeance = tokio::time::Instant::now();
    let mut derniere_publication = Instant::now();
    // Seul dans le salon : en pause au bout de cinq minutes, parti au bout
    // de trente. Quelqu'un revient : la lecture reprend d'elle-même si
    // c'est la solitude qui l'avait arrêtée.
    let mut controle_salon = Instant::now();
    let mut seul_depuis: Option<Instant> = None;
    let mut pause_solitude = false;
    loop {
        tokio::select! {
            commande = rx.recv() => {
                let Some(commande) = commande else { break };
                appliquer(&state, &outils, &mut lecteur, commande);
            }
            _ = tokio::time::sleep_until(echeance) => {
                let maintenant = tokio::time::Instant::now();
                if maintenant.duration_since(echeance) > Duration::from_secs(1) {
                    echeance = maintenant;
                }
                while echeance <= maintenant {
                    echeance += Duration::from_millis(20);
                    pas(&state, &outils, &mut lecteur, &mut emetteur);
                }
                if derniere_publication.elapsed() >= PUBLICATION {
                    derniere_publication = Instant::now();
                    if let Some(l) = &lecteur {
                        state.musique.etat.lock().unwrap().position_ms = l.position_ms;
                        publier(&state, false);
                    }
                }
                if controle_salon.elapsed() >= Duration::from_secs(1) {
                    controle_salon = Instant::now();
                    let (salon, lecture) = {
                        let e = state.musique.etat.lock().unwrap();
                        (e.salon, e.lecture)
                    };
                    if let Some(salon) = salon {
                        let pairs = state.voice_routes.read().unwrap().peers.get(&salon).map_or(0, |p| p.len());
                        if pairs == 0 {
                            let depuis = *seul_depuis.get_or_insert_with(Instant::now);
                            if depuis.elapsed() >= SOLITUDE_DEPART {
                                tracing::info!("musique : seul depuis trente minutes, le bot s'en va");
                                seul_depuis = None;
                                pause_solitude = false;
                                appliquer(&state, &outils, &mut lecteur, Commande::Arreter);
                            } else if lecture && depuis.elapsed() >= SOLITUDE_PAUSE {
                                pause_solitude = true;
                                let mut e = state.musique.etat.lock().unwrap();
                                e.lecture = false;
                                e.erreur = Some("en pause : plus personne dans le salon".into());
                                drop(e);
                                publier(&state, true);
                            }
                        } else {
                            seul_depuis = None;
                            if pause_solitude {
                                pause_solitude = false;
                                let mut e = state.musique.etat.lock().unwrap();
                                e.lecture = true;
                                e.erreur = None;
                                drop(e);
                                publier(&state, true);
                            }
                        }
                    } else {
                        seul_depuis = None;
                        pause_solitude = false;
                    }
                }
            }
        }
    }
}

/// Une échéance de 20 ms : une trame part si la piste en cours en a une ;
/// une piste finie laisse la place à la suivante.
fn pas(state: &Arc<AppState>, outils: &Arc<Outils>, lecteur: &mut Option<Lecteur>, emetteur: &mut Emetteur) {
    let (lecture, salon, volume) = {
        let e = state.musique.etat.lock().unwrap();
        (e.lecture, e.salon, e.volume)
    };
    let Some(l) = lecteur.as_mut() else { return };
    if l.fini.load(Ordering::Relaxed) && l.rx.try_recv().is_err() {
        // Terminée (ou ratée) et vidée : la suivante.
        let erreur = l.erreur.lock().unwrap().clone();
        if let Some(e) = &erreur {
            tracing::warn!("musique : piste abandonnée : {e}");
            state.musique.compteurs.echecs.fetch_add(1, Ordering::Relaxed);
        } else {
            state.musique.compteurs.pistes_jouees.fetch_add(1, Ordering::Relaxed);
        }
        suivante(state, outils, lecteur, erreur);
        return;
    }
    if !l.mesure && l.pret.load(Ordering::Relaxed) {
        l.mesure = true;
        state.musique.compteurs.premier_son_total_ms.fetch_add(l.demarre.elapsed().as_millis() as u64, Ordering::Relaxed);
        state.musique.compteurs.premier_son_n.fetch_add(1, Ordering::Relaxed);
    }
    if !l.pret.load(Ordering::Relaxed) {
        if l.demarre.elapsed() > DELAI_PREMIER_SON {
            suivante(state, outils, lecteur, Some("pas de son au bout d'une minute".into()));
        }
        return;
    }
    if !lecture {
        return;
    }
    match l.rx.try_recv() {
        Ok(pcm) if pcm.is_empty() => {}
        Ok(mut pcm) => {
            let gain = volume as f32 / 100.0;
            if gain < 0.999 {
                for s in pcm.iter_mut() {
                    *s *= gain;
                }
            }
            if let (Some(salon), Some(paquet)) = (salon, emetteur.trame(&pcm)) {
                envoyer(state, salon, paquet);
            }
            l.position_ms += 20;
        }
        Err(TryRecvError::Empty) => {}
        Err(TryRecvError::Disconnected) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La dernière ligne utile de la sortie d'erreur, sans le préfixe de
    /// yt-dlp, bornée.
    #[test]
    fn l_erreur_se_resume() {
        assert_eq!(
            resume_erreur(b"WARNING: x\nERROR: [youtube] abc: Video unavailable\n\n"),
            "[youtube] abc: Video unavailable"
        );
        assert_eq!(resume_erreur(b""), "échec");
        assert!(resume_erreur("é".repeat(400).as_bytes()).chars().count() <= 160);
    }

    /// Une trame chiffrée porte l'en-tête voix du bot et tient dans un
    /// datagramme.
    #[test]
    fn une_trame_du_bot_se_chiffre() {
        let mut e = Emetteur::new(&[7u8; 32]).expect("encodeur");
        let pcm = vec![0.1f32; TRAME];
        let paquet = e.trame(&pcm).expect("trame");
        let p = ki_protocol::parse_voice_packet(&paquet).expect("en-tête");
        assert_eq!(p.id, MUSIQUE_ID);
        assert!(paquet.len() <= ki_protocol::VOICE_MAX_PACKET);
        let second = e.trame(&pcm).expect("trame");
        assert_eq!(
            ki_protocol::parse_voice_packet(&second).unwrap().counter,
            p.counter + 1
        );
    }

    /// Avec yt-dlp sur la machine : une adresse réelle se résout en titre,
    /// artiste, durée. Réseau et outils requis — lancé à la main :
    /// `cargo test -p ki-server resoudre -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn resoudre_une_adresse_reelle() {
        let outils = detecter(
            &std::env::temp_dir()
                .join("ki-musique-test")
                .to_string_lossy(),
        )
        .expect("yt-dlp et ffmpeg");
        let piste =
            resoudre(&outils, "https://www.youtube.com/watch?v=dQw4w9WgXcQ").expect("résolution");
        println!("{piste:?}");
        assert!(!piste.titre.is_empty() && piste.duree_s > 60 && piste.source == "youtube");
        let sc = resoudre(&outils, "https://soundcloud.com/forss/flickermood")
            .expect("résolution SoundCloud");
        println!("{sc:?}");
        assert!(sc.source == "soundcloud" && sc.duree_s > 0);
    }

    /// Avec yt-dlp, ffmpeg et le réseau : la chaîne entière produit des
    /// trames chiffrées à partir d'une adresse réelle, puis se tue quand on
    /// lâche le lecteur. `cargo test -p ki-server chaine -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn la_chaine_produit_des_trames_depuis_une_adresse_reelle() {
        let outils = Arc::new(
            detecter(
                &std::env::temp_dir()
                    .join("ki-musique-test")
                    .to_string_lossy(),
            )
            .expect("yt-dlp et ffmpeg"),
        );
        for url in [
            "https://soundcloud.com/forss/flickermood",
            "https://www.youtube.com/watch?v=GDAGQOAVWa8&list=RDGDAGQOAVWa8",
        ] {
            let debut = Instant::now();
            let lecteur = Lecteur::demarrer(Arc::clone(&outils), url.into());
            while !lecteur.pret.load(Ordering::Relaxed) {
                assert!(
                    debut.elapsed() < DELAI_PREMIER_SON,
                    "pas de son au bout d'une minute"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
            println!("premier son après {:?}", debut.elapsed());
            assert!(lecteur.erreur.lock().unwrap().is_none());
            let mut emetteur = Emetteur::new(&[3u8; 32]).unwrap();
            let mut trames = 0;
            let mut energie = 0f32;
            while trames < 100 {
                let bloc = lecteur
                    .rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("un bloc de son");
                assert_eq!(bloc.len(), TRAME);
                energie += bloc.iter().map(|s| s * s).sum::<f32>();
                let paquet = emetteur.trame(&bloc).expect("trame");
                assert!(
                    paquet.len() > VOICE_HEADER_LEN + 16
                        && paquet.len() <= ki_protocol::VOICE_MAX_PACKET
                );
                trames += 1;
            }
            println!("{trames} trames, énergie {energie:.1}");
            assert!(energie > 0.0, "du son, pas du silence");
            // Lâcher le lecteur tue les enfants : le fil se termine.
            drop(lecteur);
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// Une playlist se reconnaît à son chemin ; une vidéo avec `list=`
    /// reste une vidéo.
    #[test]
    fn les_playlists_se_reconnaissent() {
        assert!(est_liste("https://music.youtube.com/playlist?list=OLAK5uy_x"));
        assert!(est_liste("https://www.youtube.com/playlist?list=PLx"));
        assert!(est_liste("https://soundcloud.com/forss/sets/soulhack"));
        assert!(!est_liste("https://www.youtube.com/watch?v=a&list=RDa"));
        assert!(!est_liste("https://soundcloud.com/forss/flickermood"));
        let plat = b"{\"title\":\"A\",\"url\":\"https://www.youtube.com/watch?v=1\",\"duration\":10,\"uploader\":\"X\"}\n{\"title\":\"[Private video]\",\"url\":\"https://www.youtube.com/watch?v=2\"}\n";
        let pistes = pistes_a_plat(plat, "youtube");
        assert_eq!(pistes.len(), 1);
        assert_eq!((pistes[0].titre.as_str(), pistes[0].artiste.as_str(), pistes[0].duree_s), ("A", "X", 10));
    }

    /// Les vignettes ont un identifiant stable, et SoundCloud passe en 300×300.
    #[test]
    fn les_vignettes_se_choisissent() {
        assert_eq!(empreinte("a"), empreinte("a"));
        assert_ne!(empreinte("a"), empreinte("b"));
        assert_eq!(empreinte("x").len(), 16);
        let yt = serde_json::json!({"thumbnails": [
            {"url": "https://i.ytimg.com/vi/x/hq720.jpg?a", "width": 360, "height": 202},
            {"url": "https://i.ytimg.com/vi/x/hq720.jpg?b", "width": 720, "height": 404}
        ]});
        assert_eq!(meilleure_vignette(&yt).as_deref(), Some("https://i.ytimg.com/vi/x/hq720.jpg?a"));
        let sc = serde_json::json!({"thumbnails": [
            {"url": "https://i1.sndcdn.com/artworks-abc-mini.jpg", "width": 16, "height": 16},
            {"url": "https://i1.sndcdn.com/artworks-abc-small.jpg", "width": 32, "height": 32}
        ]});
        assert_eq!(meilleure_vignette(&sc).as_deref(), Some("https://i1.sndcdn.com/artworks-abc-t300x300.jpg"));
    }

    /// Une commande qui n'existe pas se tue au bout du délai.
    #[test]
    fn un_enfant_qui_traine_est_tue() {
        let r = executer_borne(
            &mut Command::new("commande-qui-n-existe-pas-ki-chat"),
            Duration::from_secs(1),
        );
        assert!(r.is_err());
    }
}
