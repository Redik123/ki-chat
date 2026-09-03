//! Relais des partages d'écran (SFU vidéo) — jalon S1b de PLAN-STREAM.md.
//!
//! Le serveur route des trames chiffrées sans jamais les décoder : l'en-tête
//! clair ([`ki_protocol::MediaHeader`]) lui dit tout ce qu'il a besoin de
//! savoir (qui, quelle séquence, trame clé ou non), la charge reste opaque.
//!
//! # Le principe qui gouverne le relais
//!
//! **Une tâche de diffusion par spectateur, nourrie par une file de deux
//! trames.** C'est la parade au spectateur lent : sa file déborde, SES trames
//! sont jetées, et les autres ne s'en aperçoivent pas. Une diffusion naïve
//! (écrire à tous depuis l'ingestion) aurait mis tout le salon au rythme du
//! plus lent. Quand on jette à quelqu'un, on le marque « en attente de trame
//! clé » : il ne recevra plus que du décodable — les trames P d'après une
//! trame jetée ne sont que de la bouillie — et le streamer est prié (au plus
//! une fois par demi-seconde) d'en produire une.
//!
//! La mémoire des trames en transit est comptée globalement et bornée : le
//! compteur monte à l'ingestion, redescend quand la **dernière** copie part
//! (les spectateurs partagent la même allocation).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ki_protocol::{ChannelId, MediaHeader, StreamMeta, UserId};

/// Deux diffusions au plus par salon (v1) : au-delà, plus personne ne sait
/// quel écran regarder, et la liaison montante du serveur non plus.
const MAX_PAR_SALON: usize = 2;
/// Mémoire totale des trames en transit vers les spectateurs.
const MEM_MAX: usize = 32 * 1024 * 1024;
/// File par spectateur : deux trames d'avance, pas une de plus.
const FILE_VIEWER: usize = 2;
/// Une demande de trame clé au plus par demi-seconde et par stream.
const IDR_COOLDOWN: Duration = Duration::from_millis(500);

/// Une trame prête à diffuser. Les octets sont partagés entre spectateurs
/// (Arc) ; la mémoire est rendue au compteur global quand la dernière copie
/// est écrite — c'est le Drop qui fait la comptabilité, aucun chemin ne peut
/// l'oublier.
pub struct Trame {
    pub bytes: Vec<u8>,
    pub seq: u64,
    /// Trame clé : elle rend caduques toutes celles d'avant.
    pub idr: bool,
    mem: Arc<AtomicUsize>,
}

impl Drop for Trame {
    fn drop(&mut self) {
        self.mem.fetch_sub(self.bytes.len(), Ordering::Relaxed);
    }
}

/// Un spectateur : sa file, son drapeau « il me faut une trame clé », et la
/// tâche qui écrit vers sa connexion.
struct Viewer {
    tx: tokio::sync::mpsc::Sender<Arc<Trame>>,
    needs_idr: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
    /// Sa connexion, pour le son du jeu : des datagrammes envoyés tels
    /// quels, sans file ni tâche — un paquet de son perdu ne s'attend pas.
    conn: quinn::Connection,
}

/// Une diffusion en cours.
struct Live {
    streamer: UserId,
    /// Salon vocal du streamer au démarrage : la condition d'accès.
    channel: ChannelId,
    key_hex: String,
    meta: StreamMeta,
    /// Première séquence vue : la base des priorités relatives.
    seq_start: Option<u64>,
    viewers: HashMap<UserId, Viewer>,
    last_idr_ask: Instant,
}

#[derive(Default)]
struct Inner {
    next_id: u32,
    by_id: HashMap<u32, Live>,
}

/// La table des diffusions du serveur.
pub struct Streams {
    inner: Mutex<Inner>,
    mem: Arc<AtomicUsize>,
}

/// Ce que l'ingestion d'une trame a donné.
pub enum Ingest {
    /// Relayée ; `ask_idr` dit s'il faut prier le streamer pour une trame clé.
    Ok { ask_idr: bool },
    /// Ce compte ne diffuse pas, ou l'en-tête ment sur le stream_id.
    Refuse,
}

impl Streams {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner { next_id: 1, by_id: HashMap::new() }),
            mem: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Le stream que diffuse ce compte, s'il y en a un.
    pub fn stream_of(&self, user: UserId) -> Option<u32> {
        let inner = self.inner.lock().unwrap();
        inner.by_id.iter().find(|(_, l)| l.streamer == user).map(|(id, _)| *id)
    }

    /// Démarre une diffusion. Idempotent : rediffuser renvoie l'existant.
    pub fn start(
        &self,
        streamer: UserId,
        channel: ChannelId,
        key_hex: String,
        meta: StreamMeta,
    ) -> Result<u32, &'static str> {
        let mut inner = self.inner.lock().unwrap();
        if let Some((id, _)) = inner.by_id.iter().find(|(_, l)| l.streamer == streamer) {
            return Ok(*id);
        }
        let dans_le_salon =
            inner.by_id.values().filter(|l| l.channel == channel).count();
        if dans_le_salon >= MAX_PAR_SALON {
            return Err("deux diffusions tournent déjà dans ce salon");
        }
        let id = inner.next_id;
        inner.next_id = inner.next_id.wrapping_add(1).max(1);
        inner.by_id.insert(
            id,
            Live {
                streamer,
                channel,
                key_hex,
                meta,
                seq_start: None,
                viewers: HashMap::new(),
                last_idr_ask: Instant::now() - IDR_COOLDOWN,
            },
        );
        Ok(id)
    }

    /// Met à jour les caractéristiques annoncées ; rend l'identifiant pour la
    /// rediffusion au salon.
    pub fn meta_update(&self, streamer: UserId, meta: StreamMeta) -> Option<u32> {
        let mut inner = self.inner.lock().unwrap();
        let (id, live) = inner.by_id.iter_mut().find(|(_, l)| l.streamer == streamer)?;
        live.meta = meta;
        Some(*id)
    }

    /// Arrête la diffusion de ce compte (départ, déconnexion, ou volonté).
    /// Rend l'identifiant arrêté, pour l'annonce StreamStopped.
    pub fn stop_by_user(&self, streamer: UserId) -> Option<u32> {
        let mut inner = self.inner.lock().unwrap();
        let id = inner
            .by_id
            .iter()
            .find(|(_, l)| l.streamer == streamer)
            .map(|(id, _)| *id)?;
        if let Some(live) = inner.by_id.remove(&id) {
            for (_, v) in live.viewers {
                v.task.abort();
            }
        }
        Some(id)
    }

    /// Ce compte cesse de regarder ce stream.
    pub fn unwatch(&self, stream_id: u32, user: UserId) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(live) = inner.by_id.get_mut(&stream_id) {
            if let Some(v) = live.viewers.remove(&user) {
                v.task.abort();
            }
        }
    }

    /// Ce compte quitte la scène (salon ou serveur) : plus spectateur de
    /// rien. Sa propre diffusion se règle par `stop_by_user`, à part.
    pub fn drop_viewer_everywhere(&self, user: UserId) {
        let mut inner = self.inner.lock().unwrap();
        for live in inner.by_id.values_mut() {
            if let Some(v) = live.viewers.remove(&user) {
                v.task.abort();
            }
        }
    }

    /// Un spectateur demande à regarder. Vérifie qu'il partage le salon vocal
    /// du streamer, lance sa tâche de diffusion, et rend (clé, meta,
    /// faut-il demander une trame clé, à qui la demander).
    pub fn watch(
        &self,
        stream_id: u32,
        user: UserId,
        user_channel: Option<ChannelId>,
        conn: quinn::Connection,
    ) -> Result<(String, StreamMeta, bool, UserId), &'static str> {
        let mut inner = self.inner.lock().unwrap();
        let live = inner.by_id.get_mut(&stream_id).ok_or("cette diffusion est terminée")?;
        if live.streamer == user {
            return Err("tu es le streamer : ton aperçu est local");
        }
        if user_channel != Some(live.channel) {
            return Err("il faut être dans le salon vocal du streamer");
        }
        // Re-regarder remplace la tâche : une seule diffusion par spectateur.
        if let Some(v) = live.viewers.remove(&user) {
            v.task.abort();
        }
        let (tx, rx) = tokio::sync::mpsc::channel::<Arc<Trame>>(FILE_VIEWER);
        let needs_idr = Arc::new(AtomicBool::new(true));
        let seq_start = live.seq_start.unwrap_or(0);
        let task = tokio::spawn(diffuser(conn.clone(), rx, seq_start, needs_idr.clone()));
        live.viewers.insert(user, Viewer { tx, needs_idr, task, conn });
        let ask = live.last_idr_ask.elapsed() >= IDR_COOLDOWN;
        if ask {
            live.last_idr_ask = Instant::now();
        }
        Ok((live.key_hex.clone(), live.meta, ask, live.streamer))
    }

    /// Un datagramme de son du jeu arrive du streamer : vers chaque
    /// spectateur, tel quel — le serveur ne déchiffre rien, et un
    /// datagramme qui ne part pas (file pleine) est simplement perdu, comme
    /// la voix. `false` si ce compte ne diffuse pas ce stream.
    pub fn relayer_audio(&self, streamer: UserId, stream_id: u32, dat: &bytes::Bytes) -> bool {
        let inner = self.inner.lock().unwrap();
        let Some(live) = inner.by_id.get(&stream_id) else { return false };
        if live.streamer != streamer {
            return false;
        }
        for v in live.viewers.values() {
            let _ = v.conn.send_datagram(dat.clone());
        }
        true
    }

    /// Une trame arrive du streamer : validation, comptabilité mémoire, et
    /// distribution à chaque spectateur selon sa file et son état.
    pub fn ingest(&self, streamer: UserId, header: &MediaHeader, bytes: Vec<u8>) -> Ingest {
        let mut inner = self.inner.lock().unwrap();
        let Some(live) = inner.by_id.get_mut(&header.stream_id) else {
            return Ingest::Refuse;
        };
        if live.streamer != streamer {
            return Ingest::Refuse;
        }
        if live.seq_start.is_none() {
            live.seq_start = Some(header.seq);
        }

        // Le plafond mémoire d'abord : un relais qui gonfle emporte le
        // serveur entier, voix comprise. Une trame refusée ici laisse les
        // files se vider ; les spectateurs repartiront d'une trame clé.
        let taille = bytes.len();
        if self.mem.load(Ordering::Relaxed).saturating_add(taille) > MEM_MAX {
            for v in live.viewers.values() {
                v.needs_idr.store(true, Ordering::Relaxed);
            }
            let ask = live.last_idr_ask.elapsed() >= IDR_COOLDOWN;
            if ask {
                live.last_idr_ask = Instant::now();
            }
            return Ingest::Ok { ask_idr: ask };
        }
        self.mem.fetch_add(taille, Ordering::Relaxed);
        let trame =
            Arc::new(Trame { bytes, seq: header.seq, idr: header.idr, mem: self.mem.clone() });

        let mut ask = false;
        let mut partis: Vec<UserId> = Vec::new();
        for (user, v) in live.viewers.iter() {
            // En attente de trame clé : les P d'ici là ne seraient que de la
            // bouillie de macroblocs — on ne les envoie pas.
            if v.needs_idr.load(Ordering::Relaxed) {
                if !header.idr {
                    ask = true;
                    continue;
                }
                v.needs_idr.store(false, Ordering::Relaxed);
            }
            match v.tx.try_send(trame.clone()) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    // Spectateur lent : SA trame est jetée, il repartira
                    // d'une trame clé — les autres n'ont rien vu.
                    v.needs_idr.store(true, Ordering::Relaxed);
                    ask = true;
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    partis.push(*user);
                }
            }
        }
        for user in partis {
            if let Some(v) = live.viewers.remove(&user) {
                v.task.abort();
            }
        }
        let ask_idr = ask && live.last_idr_ask.elapsed() >= IDR_COOLDOWN;
        if ask_idr {
            live.last_idr_ask = Instant::now();
        }
        Ingest::Ok { ask_idr }
    }
}

/// Priorité d'une trame : la même base pour tout le monde, moins l'ancienneté
/// — le plus ancien d'abord DANS un stream, round-robin naturel ENTRE
/// streams. Arithmétique saturante : le cast naïf s'inverserait à 2³¹ trames.
fn priorite(seq: u64, seq_start: u64) -> i32 {
    let age = seq.saturating_sub(seq_start);
    0i32.saturating_sub(age.min(i32::MAX as u64) as i32)
}

/// Trames d'un spectateur encore en vol (écrites, pas forcément arrivées)
/// au-delà desquelles on annule les plus anciennes : trois secondes à
/// 30 i/s, le temps qu'un lien lent se rattrape ou qu'une trame clé passe.
const EN_VOL_MAX: usize = 90;
/// Une écriture qui n'aboutit pas dans ce délai, c'est un tampon d'envoi
/// plein depuis trop longtemps : le lien du spectateur ne suit pas.
const ECRITURE_MAX: Duration = Duration::from_millis(400);

/// La tâche d'un spectateur : chaque trame part dans SON flux QUIC
/// unidirectionnel — fiabilité par trame, sans blocage de tête de ligne
/// entre trames. Une erreur d'écriture termine la tâche ; l'ingestion
/// constatera la file fermée et retirera le spectateur.
///
/// Le retard ne s'accumule pas : une trame clé annule (RESET_STREAM) toutes
/// celles d'avant encore en vol — le spectateur saute à la trame clé au lieu
/// de rattraper des images périmées —, et une écriture qui bloque trop
/// longtemps annule sa trame et remet le spectateur en attente de trame clé.
/// C'est ce qui manquait quand un spectateur au lien trop court prenait dix
/// secondes de vidéo en retard, la voix de tout le salon faisant la queue
/// derrière.
async fn diffuser(
    conn: quinn::Connection,
    mut rx: tokio::sync::mpsc::Receiver<Arc<Trame>>,
    seq_start: u64,
    needs_idr: Arc<AtomicBool>,
) {
    let mut en_vol: std::collections::VecDeque<quinn::SendStream> =
        std::collections::VecDeque::new();
    while let Some(trame) = rx.recv().await {
        if trame.idr {
            for mut vieux in en_vol.drain(..) {
                // Déjà arrivée : l'annulation est refusée, sans conséquence.
                let _ = vieux.reset(quinn::VarInt::from_u32(0));
            }
        }
        let Ok(mut flux) = conn.open_uni().await else { return };
        let _ = flux.set_priority(priorite(trame.seq, seq_start));
        match tokio::time::timeout(ECRITURE_MAX, flux.write_all(&trame.bytes)).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return,
            Err(_) => {
                let _ = flux.reset(quinn::VarInt::from_u32(0));
                needs_idr.store(true, Ordering::Relaxed);
                continue;
            }
        }
        let _ = flux.finish();
        en_vol.push_back(flux);
        while en_vol.len() > EN_VOL_MAX {
            if let Some(mut vieux) = en_vol.pop_front() {
                let _ = vieux.reset(quinn::VarInt::from_u32(0));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> StreamMeta {
        StreamMeta { width: 1920, height: 1080, fps: 30, kbps: 6000 }
    }

    #[test]
    fn un_stream_par_compte_et_deux_par_salon() {
        let s = Streams::new();
        let a = s.start(1, 10, "k1".into(), meta()).unwrap();
        // Idempotent : le même compte retrouve SON stream.
        assert_eq!(s.start(1, 10, "k1bis".into(), meta()).unwrap(), a);
        let _b = s.start(2, 10, "k2".into(), meta()).unwrap();
        // Troisième diffusion du salon : refusée.
        assert!(s.start(3, 10, "k3".into(), meta()).is_err());
        // Mais un autre salon a son propre quota.
        assert!(s.start(3, 11, "k3".into(), meta()).is_ok());
        // L'arrêt libère la place.
        assert_eq!(s.stop_by_user(1), Some(a));
        assert!(s.start(4, 10, "k4".into(), meta()).is_ok());
        assert_eq!(s.stream_of(1), None);
    }

    #[test]
    fn la_priorite_decroit_et_sature() {
        assert_eq!(priorite(100, 100), 0);
        assert_eq!(priorite(103, 100), -3);
        // Bien au-delà de 2³¹ trames : pas d'inversion, le plancher tient.
        assert_eq!(priorite(u64::MAX, 0), i32::MIN + 1);
        // Une séquence qui aurait reculé (impossible par contrat, mais un
        // pair hostile écrit ce qu'il veut) ne devient pas prioritaire.
        assert_eq!(priorite(50, 100), 0);
    }

    /// La comptabilité mémoire est portée par le Drop de la trame : quand la
    /// dernière copie part, le compteur redescend — chemin d'erreur compris.
    #[test]
    fn la_memoire_se_rend_au_drop() {
        let mem = Arc::new(AtomicUsize::new(0));
        mem.fetch_add(1000, Ordering::Relaxed);
        let t = Arc::new(Trame { bytes: vec![0u8; 1000], seq: 1, idr: false, mem: mem.clone() });
        let t2 = t.clone();
        drop(t);
        assert_eq!(mem.load(Ordering::Relaxed), 1000, "une copie vit encore");
        drop(t2);
        assert_eq!(mem.load(Ordering::Relaxed), 0);
    }
}
