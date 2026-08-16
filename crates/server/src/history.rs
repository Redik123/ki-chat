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
                for line in reader.lines() {
                    let line = line?;
                    if let Ok(rec) = serde_json::from_str::<ChatRecord>(&line) {
                        if recent.len() == MEM_CAP {
                            recent.pop_front();
                        }
                        recent.push_back(rec);
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
                recent.iter().skip(skip).cloned().collect()
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
                return (older[skip..].to_vec(), more);
            }
            older
        };

        let path = PathBuf::from(data_dir).join(format!("channel-{channel}.jsonl"));
        let Ok(file) = File::open(&path) else {
            // Pas de fichier : le cache est tout ce qui existe.
            return (from_memory, false);
        };
        // Relecture complète puis fenêtrage. Un salon de 100 000 messages
        // fait ~15 Mo — coûteux mais rare, et sans index il n'y a pas de
        // moyen honnête de faire mieux. C'est le moment de passer à SQLite
        // si les salons grossissent vraiment.
        let mut older: Vec<ChatRecord> = BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|l| serde_json::from_str::<ChatRecord>(&l).ok())
            .filter(|r| r.ts < before_ts)
            .collect();
        older.sort_by_key(|r| r.ts);
        let skip = older.len().saturating_sub(limit);
        (older[skip..].to_vec(), skip > 0)
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
}
