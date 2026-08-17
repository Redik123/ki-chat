//! Persistance de l'historique texte : un fichier JSONL par salon.
//!
//! Volontairement simple et 100 % Rust (pas de dépendance C) : append-only,
//! les N derniers messages par salon sont gardés en mémoire pour les requêtes
//! History. Migration possible vers redb/SQLite si le besoin grandit.
//!
//! L'écriture sur disque ne se fait PAS dans le fil appelant : `append` est
//! appelé depuis la boucle asynchrone, sur le chemin le plus chaud du serveur
//! (chaque message de chat). Un `writeln!` y bloque un ouvrier Tokio le temps
//! d'un appel système, et c'est le relais des datagrammes vocaux qui hoquette.
//! Les lignes partent donc vers un fil d'écriture dédié, qui les traite dans
//! l'ordre où elles ont été produites.

use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Mutex;

use ki_protocol::{ChannelId, ChannelInfo, ChatRecord};

/// Nombre max de messages gardés en mémoire par salon.
const MEM_CAP: usize = 1000;

/// Budget d'octets d'une réponse d'historique, marge sous [`ki_protocol::MAX_LINE`].
///
/// Une réponse `History`/`HistoryPage` part sur le flux de contrôle en **une
/// seule ligne**. Or le lecteur d'en face refuse toute ligne au-delà de
/// `MAX_LINE` et **ferme la connexion** : sans borne à l'émission, quelques
/// dizaines de longs messages suffisaient à rendre un salon impossible à
/// ouvrir (déconnexion en boucle). On ne renvoie donc jamais plus que ce qui
/// tient dans une ligne ; le reste se récupère en remontant le fil.
const MAX_HISTORY_BYTES: usize = ki_protocol::MAX_LINE - 8 * 1024;

/// Ce qu'il faut faire d'une ligne rendue par `BufRead::lines`.
enum LineOutcome {
    Take(String),
    Skip,
    Stop,
}

/// Décide du sort d'une ligne de journal, et **c'est la seule règle** : les
/// trois lecteurs de ce format doivent se comporter pareil.
///
/// Une ligne illisible est *sautée*. Un fichier tronqué par un disque plein ou
/// une coupure de courant laisse souvent sa dernière ligne coupée au milieu
/// d'un caractère accentué : refuser de démarrer pour ça ferait perdre tout
/// l'historique, qui est par ailleurs parfaitement lisible.
///
/// Une vraie erreur d'entrée-sortie, elle, *arrête* la lecture. La distinction
/// n'est pas cosmétique : une erreur matérielle ne fait pas avancer le curseur,
/// et l'ignorer ferait tourner la boucle indéfiniment — un serveur figé, plus
/// difficile à diagnostiquer qu'un serveur qui refuse de démarrer.
fn keep_or_stop(line: std::io::Result<String>) -> LineOutcome {
    match line {
        Ok(line) => LineOutcome::Take(line),
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => LineOutcome::Skip,
        Err(e) => {
            tracing::error!("lecture d'un journal interrompue : {e}");
            LineOutcome::Stop
        }
    }
}

/// Tous les enregistrements lisibles d'un journal, selon `keep_or_stop`.
fn records_of(reader: impl BufRead) -> Vec<ChatRecord> {
    let mut out = Vec::new();
    for line in reader.lines() {
        match keep_or_stop(line) {
            LineOutcome::Take(line) => {
                if let Ok(rec) = serde_json::from_str::<ChatRecord>(&line) {
                    out.push(rec);
                }
            }
            LineOutcome::Skip => continue,
            LineOutcome::Stop => break,
        }
    }
    out
}

/// Ne garde que les messages les plus **récents** dont la taille sérialisée
/// cumulée tient dans [`MAX_HISTORY_BYTES`], l'ordre chronologique préservé.
///
/// Renvoie aussi `true` si des messages plus anciens ont dû être retirés —
/// l'appelant en a besoin pour dire au client qu'il en reste à charger.
fn fit_within(messages: Vec<ChatRecord>) -> (Vec<ChatRecord>, bool) {
    let mut total = 0usize;
    let mut start = messages.len();
    for (i, rec) in messages.iter().enumerate().rev() {
        let size = serde_json::to_string(rec).map(|s| s.len() + 1).unwrap_or(usize::MAX);
        if total + size > MAX_HISTORY_BYTES {
            break;
        }
        total += size;
        start = i;
    }
    // Toujours rendre au moins le dernier message : un message seul ne peut de
    // toute façon pas dépasser le budget (texte borné à MAX_CHAT_TEXT), mais on
    // ne veut en aucun cas renvoyer une page vide en prétendant qu'il reste à
    // charger — le client bouclerait.
    if start == messages.len() && !messages.is_empty() {
        start = messages.len() - 1;
    }
    let truncated = start > 0;
    (messages[start..].to_vec(), truncated)
}

pub struct History {
    /// Les N derniers messages de chaque salon. Le fichier correspondant est
    /// tenu par le fil d'écriture, lui seul y touche.
    logs: Mutex<HashMap<ChannelId, VecDeque<ChatRecord>>>,
    /// Vers le fil d'écriture. `Option` seulement pour pouvoir lâcher
    /// l'émetteur dans `Drop` avant d'attendre le fil : tant qu'un émetteur
    /// vit, le `recv` d'en face ne rend jamais la main.
    writes: Option<Sender<WriteCmd>>,
    writer: Option<std::thread::JoinHandle<()>>,
}

impl History {
    pub fn open(data_dir: &str, channels: &[ChannelInfo]) -> anyhow::Result<Self> {
        let dir = PathBuf::from(data_dir);
        std::fs::create_dir_all(&dir)?;
        let mut logs = HashMap::new();
        let mut files = HashMap::new();
        for ch in channels {
            let path = dir.join(format!("channel-{}.jsonl", ch.id));
            let mut recent = VecDeque::with_capacity(MEM_CAP);
            if path.exists() {
                let reader = BufReader::new(File::open(&path)?);
                // Lecture en flux, pour ne jamais tenir plus de MEM_CAP
                // messages en mémoire, et selon la règle de `keep_or_stop`.
                for line in reader.lines() {
                    match keep_or_stop(line) {
                        LineOutcome::Take(line) => {
                            if let Ok(rec) = serde_json::from_str::<ChatRecord>(&line) {
                                if recent.len() == MEM_CAP {
                                    recent.pop_front();
                                }
                                recent.push_back(rec);
                            }
                        }
                        LineOutcome::Skip => continue,
                        LineOutcome::Stop => break,
                    }
                }
            }
            let file = OpenOptions::new().create(true).append(true).open(&path)?;
            files.insert(ch.id, file);
            logs.insert(ch.id, recent);
        }
        // Canal non borné : le chat est déjà limité en amont par le seau à
        // jetons de chaque client, et faire attendre l'appelant ici serait
        // exactement ce qu'on cherche à éviter.
        let (writes, rx) = std::sync::mpsc::channel();
        let writer = std::thread::Builder::new()
            .name("ki-history".into())
            .spawn(move || writer_loop(files, rx))?;
        Ok(Self {
            logs: Mutex::new(logs),
            writes: Some(writes),
            writer: Some(writer),
        })
    }

    pub fn append(&self, channel: ChannelId, rec: &ChatRecord) {
        // Le cache mémoire est mis à jour tout de suite, sous le verrou : un
        // History demandé dans la foulée doit déjà voir ce message, même si
        // le disque, lui, n'a pas encore été touché.
        {
            let mut logs = self.logs.lock().unwrap();
            let Some(recent) = logs.get_mut(&channel) else { return };
            if recent.len() == MEM_CAP {
                recent.pop_front();
            }
            recent.push_back(rec.clone());
        }
        // Verrou relâché avant l'envoi : le fil d'écriture ne doit jamais
        // faire attendre un `recent()`.
        if let Ok(line) = serde_json::to_string(rec) {
            if let Some(writes) = &self.writes {
                let _ = writes.send(WriteCmd::Line(channel, line));
            }
        }
    }

    /// Ouvre le journal d'un salon créé en cours d'exécution.
    ///
    /// À appeler **avant** d'annoncer le salon : `append` ignore en silence
    /// un salon qu'il ne connaît pas, et le premier message partirait dans
    /// le vide sans que rien ne le signale.
    pub fn open_channel(&self, data_dir: &str, channel: ChannelId) -> anyhow::Result<()> {
        let path = PathBuf::from(data_dir).join(format!("channel-{channel}.jsonl"));
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        self.logs
            .lock()
            .unwrap()
            .entry(channel)
            .or_insert_with(|| VecDeque::with_capacity(MEM_CAP));
        if let Some(writes) = &self.writes {
            let _ = writes.send(WriteCmd::Open(channel, file));
        }
        Ok(())
    }

    /// Referme le journal d'un salon supprimé. Le fichier reste sur le
    /// disque — c'est l'archivage, assuré par le magasin de salons.
    pub fn close_channel(&self, channel: ChannelId) {
        self.logs.lock().unwrap().remove(&channel);
        if let Some(writes) = &self.writes {
            let _ = writes.send(WriteCmd::Close(channel));
        }
    }

    pub fn recent(&self, channel: ChannelId, limit: usize) -> Vec<ChatRecord> {
        let logs = self.logs.lock().unwrap();
        match logs.get(&channel) {
            Some(recent) => {
                let skip = recent.len().saturating_sub(limit);
                let msgs: Vec<ChatRecord> = recent.iter().skip(skip).cloned().collect();
                // Borné à une ligne de contrôle. Le client complète en
                // remontant le fil (`HistoryBefore`) si tout ne tient pas.
                fit_within(msgs).0
            }
            None => Vec::new(),
        }
    }

    /// Les `limit` messages qui précèdent `before_ts`, du plus ancien au plus
    /// récent, plus un drapeau disant s'il en reste encore avant.
    ///
    /// Sert à remonter le fil. Le cache mémoire ne couvre que les 1000
    /// derniers messages : au-delà, il faut relire le fichier, ce qui est
    /// justement ce qui rendait le passé inatteignable jusqu'ici. La lecture
    /// disque est donc assumée — c'est une action rare, déclenchée par un
    /// défilement, et l'appelant la déporte hors de la boucle asynchrone.
    pub fn before(
        &self,
        data_dir: &str,
        channel: ChannelId,
        before_ts: u64,
        limit: usize,
    ) -> (Vec<ChatRecord>, bool) {
        // Le cache d'abord : le cas courant est de remonter de quelques
        // pages, ce qui ne touche pas le disque.
        let from_memory = {
            let logs = self.logs.lock().unwrap();
            let Some(recent) = logs.get(&channel) else { return (Vec::new(), false) };
            let older: Vec<ChatRecord> =
                recent.iter().filter(|r| r.ts < before_ts).cloned().collect();
            // Le cache est plein ET on en a épuisé le début : le reste est
            // sur le disque, il faut aller le chercher.
            let exhausted = older.len() < limit && recent.len() == MEM_CAP;
            if !exhausted {
                let skip = older.len().saturating_sub(limit);
                let more = skip > 0 || recent.len() == MEM_CAP;
                // Tronqué à une ligne de contrôle si besoin : dans ce cas il
                // reste forcément des messages à charger avant.
                let (page, truncated) = fit_within(older[skip..].to_vec());
                return (page, more || truncated);
            }
            older
        };

        let path = PathBuf::from(data_dir).join(format!("channel-{channel}.jsonl"));
        let Ok(file) = File::open(&path) else {
            // Pas de fichier : le cache est tout ce qui existe.
            let (page, truncated) = fit_within(from_memory);
            return (page, truncated);
        };
        // Relecture complète puis fenêtrage. Un salon de 100 000 messages
        // fait ~15 Mo — coûteux mais rare, et sans index il n'y a pas de
        // moyen honnête de faire mieux. C'est le moment de passer à SQLite
        // si les salons grossissent vraiment.
        // Une ligne illisible au milieu du fichier est sautée, et non prise
        // pour une fin de fichier — sinon tout le passé qui la suit
        // deviendrait injoignable.
        let mut older: Vec<ChatRecord> = records_of(BufReader::new(file))
            .into_iter()
            .filter(|r| r.ts < before_ts)
            .collect();
        older.sort_by_key(|r| r.ts);
        let skip = older.len().saturating_sub(limit);
        let (page, truncated) = fit_within(older[skip..].to_vec());
        (page, skip > 0 || truncated)
    }
}

impl Drop for History {
    /// Lâcher l'émetteur ferme le canal ; le fil écrit ce qui reste en file
    /// avant de s'arrêter. Sans cette attente, les derniers messages reçus
    /// juste avant un arrêt propre n'atteindraient jamais le fichier.
    fn drop(&mut self) {
        self.writes.take();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

/// Sérialise les écritures : un seul fil, une seule file, donc les lignes
/// d'un salon arrivent sur le disque dans l'ordre où elles ont été acceptées.
fn writer_loop(mut files: HashMap<ChannelId, File>, rx: Receiver<WriteCmd>) {
    // `recv` continue de rendre ce qui est déjà en file après la fermeture du
    // canal : rien de ce qui a été accepté n'est perdu à l'arrêt.
    while let Ok(cmd) = rx.recv() {
        match cmd {
            WriteCmd::Line(channel, line) => {
                let Some(file) = files.get_mut(&channel) else { continue };
                if let Err(e) = writeln!(file, "{line}") {
                    tracing::error!(
                        "écriture de l'historique du salon {channel} impossible : {e}"
                    );
                }
            }
            // L'ouverture passe par la même file que les écritures : c'est
            // ce qui garantit qu'aucune ligne ne peut la précéder.
            WriteCmd::Open(channel, file) => {
                files.insert(channel, file);
            }
            WriteCmd::Close(channel) => {
                files.remove(&channel);
            }
        }
    }
}

/// Ordres envoyés au fil d'écriture.
enum WriteCmd {
    Line(ChannelId, String),
    Open(ChannelId, File),
    Close(ChannelId),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("ki-chat-history-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn text_channel(id: ChannelId) -> ChannelInfo {
        ChannelInfo {
            id,
            name: format!("salon-{id}"),
            kind: ki_protocol::ChannelKind::Text,
            position: 0,
            locked: false,
            allowed_roles: None,
        }
    }

    fn record(text: &str) -> ChatRecord {
        ChatRecord { user_id: 1, username: "kevin".into(), text: text.into(), ts: 42 }
    }

    fn stamped(n: u64) -> ChatRecord {
        ChatRecord { user_id: 1, username: "kevin".into(), text: format!("m{n}"), ts: n }
    }

    /// Remonter le fil doit rendre les messages **antérieurs**, du plus
    /// ancien au plus récent, et dire honnêtement s'il en reste avant : un
    /// `more` toujours vrai ferait boucler le client indéfiniment.
    #[test]
    fn paging_walks_backwards_until_the_start() {
        let dir = scratch("before");
        let history = History::open(&dir, &[text_channel(1)]).unwrap();
        for n in 1..=10 {
            history.append(1, &stamped(n));
        }

        // Les 3 messages précédant le 8e : 5, 6, 7 — dans cet ordre.
        let (page, more) = history.before(&dir, 1, 8, 3);
        assert_eq!(page.iter().map(|r| r.ts).collect::<Vec<_>>(), vec![5, 6, 7]);
        assert!(more, "il reste 1 à 4 avant");

        // Au ras du début : on ne rend que ce qui existe, et plus rien après.
        let (page, more) = history.before(&dir, 1, 3, 10);
        assert_eq!(page.iter().map(|r| r.ts).collect::<Vec<_>>(), vec![1, 2]);
        assert!(!more, "1 et 2 épuisent le salon");

        // Avant le tout premier message : page vide, et surtout pas de
        // `more` — sinon le client redemanderait sans fin.
        let (page, more) = history.before(&dir, 1, 1, 10);
        assert!(page.is_empty());
        assert!(!more);

        // Un salon inconnu ne doit pas paniquer.
        assert_eq!(history.before(&dir, 999, 100, 10).0.len(), 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Le disque est écrit ailleurs, mais la mémoire, elle, est à jour tout de
    /// suite : sans ça un client qui poste puis demande son historique ne
    /// verrait pas son propre message.
    #[test]
    fn recent_sees_a_message_right_after_append() {
        let history = History::open(&scratch("immediate"), &[text_channel(1)]).unwrap();
        history.append(1, &record("coucou"));
        let seen = history.recent(1, 10);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].text, "coucou");
        // Un salon inconnu ne fabrique pas d'historique au passage.
        history.append(9, &record("nulle part"));
        assert!(history.recent(9, 10).is_empty());
    }

    /// Rien ne se perd ni ne se réordonne entre l'appel et le fichier.
    #[test]
    fn every_line_reaches_the_file_in_order() {
        let dir = scratch("ordered");
        {
            let history = History::open(&dir, &[text_channel(1), text_channel(2)]).unwrap();
            for i in 0..200 {
                history.append(1, &record(&format!("m{i}")));
            }
            history.append(2, &record("autre salon"));
            // La sortie de bloc attend le fil d'écriture.
        }
        // Nouveau processus : ce qu'on relit est ce qui a atteint le disque.
        let history = History::open(&dir, &[text_channel(1), text_channel(2)]).unwrap();
        let seen = history.recent(1, 1000);
        assert_eq!(seen.len(), 200);
        assert_eq!(seen[0].text, "m0");
        assert_eq!(seen[199].text, "m199");
        // Les salons ne se mélangent pas.
        let autre = history.recent(2, 10);
        assert_eq!(autre.len(), 1);
        assert_eq!(autre[0].text, "autre salon");
    }

    /// Une ligne abîmée (octets non-UTF-8 d'un fichier tronqué) ne doit pas
    /// empêcher le serveur de démarrer : elle est sautée, le reste est lu.
    #[test]
    fn a_corrupt_line_does_not_stop_the_server_from_starting() {
        let dir = scratch("corrupt-line");
        let path = PathBuf::from(&dir).join("channel-1.jsonl");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(serde_json::to_string(&stamped(1)).unwrap().as_bytes());
        bytes.push(b'\n');
        // Ligne coupée au milieu d'un caractère : octets non-UTF-8.
        bytes.extend_from_slice(&[0xff, 0xfe, 0x00]);
        bytes.push(b'\n');
        bytes.extend_from_slice(serde_json::to_string(&stamped(3)).unwrap().as_bytes());
        bytes.push(b'\n');
        std::fs::write(&path, &bytes).unwrap();

        // Ne panique pas, et lit les deux lignes saines de part et d'autre.
        let history = History::open(&dir, &[text_channel(1)]).unwrap();
        let seen = history.recent(1, 10);
        assert_eq!(seen.iter().map(|r| r.ts).collect::<Vec<_>>(), vec![1, 3]);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Une réponse d'historique ne doit jamais dépasser une ligne de contrôle :
    /// au-delà, le client la refuse et se déconnecte. On en rend donc moins que
    /// demandé plutôt que de produire un salon « piège ».
    #[test]
    fn a_history_response_stays_within_a_control_line() {
        let dir = scratch("budget");
        let history = History::open(&dir, &[text_channel(1)]).unwrap();
        let big = "x".repeat(ki_protocol::MAX_CHAT_TEXT);
        for n in 1..=100 {
            history.append(
                1,
                &ChatRecord { user_id: 1, username: "k".into(), text: big.clone(), ts: n },
            );
        }

        // Ce qui est mesuré est le **message réellement émis**, enveloppe
        // comprise, et non le tableau nu : c'est cette ligne-là que le client
        // refuse au-delà de MAX_LINE, saut de ligne inclus.
        let line_of = |msg: &ki_protocol::ServerMsg| serde_json::to_string(msg).unwrap().len() + 1;

        // 100 messages de 4000 caractères dépasseraient largement MAX_LINE.
        let page = history.recent(1, 100);
        assert!(page.len() < 100, "la réponse aurait dû être tronquée");
        assert!(!page.is_empty());
        // Ce sont les plus récents qui sont conservés.
        assert_eq!(page.last().unwrap().ts, 100);
        let sent = ki_protocol::ServerMsg::History { messages: page };
        assert!(line_of(&sent) <= ki_protocol::MAX_LINE, "ligne de {} octets", line_of(&sent));

        // Et en remontant le fil, la page reste elle aussi bornée, en signalant
        // honnêtement qu'il en reste avant.
        let (older, more) = history.before(&dir, 1, 101, 100);
        assert!(more, "il reste des messages plus anciens à charger");
        let sent = ki_protocol::ServerMsg::HistoryPage { messages: older, more };
        assert!(line_of(&sent) <= ki_protocol::MAX_LINE, "ligne de {} octets", line_of(&sent));

        std::fs::remove_dir_all(&dir).ok();
    }
}
