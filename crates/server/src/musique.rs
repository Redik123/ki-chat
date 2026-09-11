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

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self as canal, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ki_protocol::{ChannelId, EtatMusique, Piste, ServerMsg, MUSIQUE_ID, VOICE_HEADER_LEN};
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

/// Les exécutables, et ce qui s'y ajoute.
pub struct Outils {
    yt_dlp: String,
    ffmpeg: String,
    /// `data/musique/cookies.txt`, s'il existe — déposé par l'admin.
    cookies: Option<PathBuf>,
    cache: PathBuf,
}

impl Outils {
    /// Les arguments communs à tout appel de yt-dlp.
    fn args_yt_dlp(&self) -> Vec<String> {
        let mut args = vec![
            "--no-playlist".to_string(),
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
    Rejoindre { salon: ChannelId },
    Ajouter { piste: Piste, maintenant: bool },
    Retirer(usize),
    Lecture,
    Pause,
    Suivant,
    Vider,
    Volume(u8),
    Arreter,
    /// Une erreur à montrer (adresse illisible…), sans rien changer.
    Erreur(String),
}

pub struct Musique {
    etat: Mutex<EtatMusique>,
    tx: tokio::sync::mpsc::UnboundedSender<Commande>,
    rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<Commande>>>,
    outils: Option<Arc<Outils>>,
}

impl Musique {
    /// Cherche yt-dlp et ffmpeg (variables `KI_YTDLP` / `KI_FFMPEG`, sinon
    /// le PATH) ; sans eux, le bot est indisponible et le dit.
    pub fn new(data_dir: &str) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let outils = detecter(data_dir).map(Arc::new);
        let etat = EtatMusique { disponible: outils.is_some(), volume: 60, ..Default::default() };
        Self { etat: Mutex::new(etat), tx, rx: Mutex::new(Some(rx)), outils }
    }

    pub fn disponible(&self) -> bool {
        self.outils.is_some()
    }

    pub fn etat(&self) -> EtatMusique {
        self.etat.lock().unwrap().clone()
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

/// La dernière ligne utile de la sortie d'erreur, bornée.
fn resume_erreur(stderr: &[u8]) -> String {
    let texte = String::from_utf8_lossy(stderr);
    let ligne = texte.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("échec");
    let ligne = ligne.trim_start_matches("ERROR: ");
    ligne.chars().take(160).collect()
}

/// Résout une adresse en piste — titre, artiste, durée, vignette — sans
/// rien télécharger. Bloquant : à appeler hors de la boucle asynchrone.
pub fn resoudre(outils: &Outils, url: &str) -> Result<Piste, String> {
    let mut cmd = Command::new(&outils.yt_dlp);
    cmd.args(outils.args_yt_dlp()).args(["--dump-single-json", "--skip-download", "--"]).arg(url);
    let sortie = executer_borne(&mut cmd, DELAI_RESOLUTION)?;
    let v: serde_json::Value = serde_json::from_slice(&sortie).map_err(|_| "réponse illisible".to_string())?;
    let titre = v["title"].as_str().unwrap_or(url).to_string();
    let artiste = ["artist", "uploader", "channel", "creator"]
        .iter()
        .find_map(|k| v[*k].as_str())
        .unwrap_or("")
        .to_string();
    let source = match v["extractor_key"].as_str().unwrap_or("").to_ascii_lowercase().as_str() {
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
                if let Err(err) = pomper(&outils, &url, &tx, &p) {
                    *e.lock().unwrap() = Some(err);
                }
                f.store(true, Ordering::Relaxed);
                p.store(true, Ordering::Relaxed);
            })
            .ok();
        Self { rx, pret, fini, erreur, position_ms: 0, demarre: Instant::now() }
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

/// yt-dlp → ffmpeg → blocs de 20 ms dans le canal. Rend quand la piste est
/// finie, ou quand le récepteur a été lâché, ou sur erreur.
fn pomper(outils: &Outils, url: &str, tx: &canal::SyncSender<Vec<f32>>, pret: &AtomicBool) -> Result<(), String> {
    let mut yt = Command::new(&outils.yt_dlp);
    yt.args(outils.args_yt_dlp())
        .args(["-f", "bestaudio/best", "-o", "-", "--quiet", "--"])
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut yt = Enfant(yt.spawn().map_err(|e| format!("yt-dlp : {e}"))?);
    let flux = yt.0.stdout.take().expect("stdout yt-dlp");
    let mut yt_err = yt.0.stderr.take().expect("stderr yt-dlp");
    let mut ff = Command::new(&outils.ffmpeg);
    ff.args(["-loglevel", "error", "-i", "pipe:0", "-vn", "-f", "f32le", "-ar", "48000", "-ac", "2", "pipe:1"])
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
        let bloc: Vec<f32> = octets.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
        if tx.send(bloc).is_err() {
            // Le cadenceur a lâché le canal : on arrête tout.
            return Ok(());
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
    Ok(())
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
        let mut opus = ki_opus::Encoder::new(48_000, ki_opus::Channels::Stereo, ki_opus::Application::Audio)
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
        let chiffre = self.cipher.encrypt(&XNonce::from(nonce), &self.sortie[..n]).ok()?;
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
        state.broadcast_all(&ServerMsg::Members { members: state.roster() });
    }
}

/// Passe à la piste suivante de la file — ou s'arrête d'attendre s'il n'y
/// en a plus. `erreur` : ce que la piste précédente a laissé.
fn suivante(state: &AppState, outils: &Arc<Outils>, lecteur: &mut Option<Lecteur>, erreur: Option<String>) {
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
    publier(state, true);
}

fn appliquer(state: &AppState, outils: &Arc<Outils>, lecteur: &mut Option<Lecteur>, commande: Commande) {
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
        Commande::Retirer(index) => {
            let mut e = state.musique.etat.lock().unwrap();
            if index < e.file.len() {
                e.file.remove(index);
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
        }
    }
}

/// La tâche du bot : commandes d'un côté, cadence de 20 ms de l'autre.
pub async fn boucle(state: Arc<AppState>) {
    let Some(outils) = state.musique.outils() else { return };
    let Some(mut rx) = state.musique.rx.lock().unwrap().take() else { return };
    let mut emetteur = match Emetteur::new(&state.voice_key) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("musique : {e} — bot désactivé");
            return;
        }
    };
    let mut lecteur: Option<Lecteur> = None;
    let mut cadence = tokio::time::interval(Duration::from_millis(20));
    cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut derniere_publication = Instant::now();
    loop {
        tokio::select! {
            commande = rx.recv() => {
                let Some(commande) = commande else { break };
                appliquer(&state, &outils, &mut lecteur, commande);
            }
            _ = cadence.tick() => {
                let (lecture, salon, volume) = {
                    let e = state.musique.etat.lock().unwrap();
                    (e.lecture, e.salon, e.volume)
                };
                let Some(l) = lecteur.as_mut() else { continue };
                if l.fini.load(Ordering::Relaxed) && l.rx.try_recv().is_err() {
                    // Terminée (ou ratée) et vidée : la suivante.
                    let erreur = l.erreur.lock().unwrap().clone();
                    if let Some(e) = &erreur {
                        tracing::warn!("musique : piste abandonnée : {e}");
                    }
                    suivante(&state, &outils, &mut lecteur, erreur);
                    continue;
                }
                if !l.pret.load(Ordering::Relaxed) {
                    if l.demarre.elapsed() > DELAI_PREMIER_SON {
                        suivante(&state, &outils, &mut lecteur, Some("pas de son au bout d'une minute".into()));
                    }
                    continue;
                }
                if !lecture {
                    continue;
                }
                match l.rx.try_recv() {
                    Ok(mut pcm) => {
                        let gain = volume as f32 / 100.0;
                        if gain < 0.999 {
                            for s in pcm.iter_mut() {
                                *s *= gain;
                            }
                        }
                        if let (Some(salon), Some(paquet)) = (salon, emetteur.trame(&pcm)) {
                            envoyer(&state, salon, paquet);
                        }
                        l.position_ms += 20;
                    }
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {}
                }
                if derniere_publication.elapsed() >= PUBLICATION {
                    derniere_publication = Instant::now();
                    state.musique.etat.lock().unwrap().position_ms = l.position_ms;
                    publier(&state, false);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La dernière ligne utile de la sortie d'erreur, sans le préfixe de
    /// yt-dlp, bornée.
    #[test]
    fn l_erreur_se_resume() {
        assert_eq!(resume_erreur(b"WARNING: x\nERROR: [youtube] abc: Video unavailable\n\n"), "[youtube] abc: Video unavailable");
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
        assert_eq!(ki_protocol::parse_voice_packet(&second).unwrap().counter, p.counter + 1);
    }

    /// Avec yt-dlp sur la machine : une adresse réelle se résout en titre,
    /// artiste, durée. Réseau et outils requis — lancé à la main :
    /// `cargo test -p ki-server resoudre -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn resoudre_une_adresse_reelle() {
        let outils = detecter(&std::env::temp_dir().join("ki-musique-test").to_string_lossy()).expect("yt-dlp et ffmpeg");
        let piste = resoudre(&outils, "https://www.youtube.com/watch?v=dQw4w9WgXcQ").expect("résolution");
        println!("{piste:?}");
        assert!(!piste.titre.is_empty() && piste.duree_s > 60 && piste.source == "youtube");
        let sc = resoudre(&outils, "https://soundcloud.com/forss/flickermood").expect("résolution SoundCloud");
        println!("{sc:?}");
        assert!(sc.source == "soundcloud" && sc.duree_s > 0);
    }

    /// Avec yt-dlp, ffmpeg et le réseau : la chaîne entière produit des
    /// trames chiffrées à partir d'une adresse réelle, puis se tue quand on
    /// lâche le lecteur. `cargo test -p ki-server chaine -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn la_chaine_produit_des_trames_depuis_une_adresse_reelle() {
        let outils = Arc::new(detecter(&std::env::temp_dir().join("ki-musique-test").to_string_lossy()).expect("yt-dlp et ffmpeg"));
        let debut = Instant::now();
        let lecteur = Lecteur::demarrer(outils, "https://soundcloud.com/forss/flickermood".into());
        while !lecteur.pret.load(Ordering::Relaxed) {
            assert!(debut.elapsed() < DELAI_PREMIER_SON, "pas de son au bout d'une minute");
            std::thread::sleep(Duration::from_millis(50));
        }
        println!("premier son après {:?}", debut.elapsed());
        assert!(lecteur.erreur.lock().unwrap().is_none());
        let mut emetteur = Emetteur::new(&[3u8; 32]).unwrap();
        let mut trames = 0;
        let mut energie = 0f32;
        while trames < 100 {
            let bloc = lecteur.rx.recv_timeout(Duration::from_secs(5)).expect("un bloc de son");
            assert_eq!(bloc.len(), TRAME);
            energie += bloc.iter().map(|s| s * s).sum::<f32>();
            let paquet = emetteur.trame(&bloc).expect("trame");
            assert!(paquet.len() > VOICE_HEADER_LEN + 16 && paquet.len() <= ki_protocol::VOICE_MAX_PACKET);
            trames += 1;
        }
        println!("{trames} trames, énergie {energie:.1}");
        assert!(energie > 0.0, "du son, pas du silence");
        // Lâcher le lecteur tue les enfants : le fil se termine.
        drop(lecteur);
        std::thread::sleep(Duration::from_millis(500));
    }

    /// Une commande qui n'existe pas se tue au bout du délai.
    #[test]
    fn un_enfant_qui_traine_est_tue() {
        let r = executer_borne(&mut Command::new("commande-qui-n-existe-pas-ki-chat"), Duration::from_secs(1));
        assert!(r.is_err());
    }
}
