//! Persistance de l'historique texte : un fichier JSONL par salon.
//!
//! Volontairement simple et 100 % Rust (pas de dépendance C) : append-only,
//! les N derniers messages par salon sont gardés en mémoire pour les requêtes
//! History. Migration possible vers redb/SQLite si le besoin grandit.

use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use ki_protocol::{ChannelId, ChannelInfo, ChatRecord};

/// Nombre max de messages gardés en mémoire par salon.
const MEM_CAP: usize = 1000;

struct ChannelLog {
    file: File,
    recent: VecDeque<ChatRecord>,
}

pub struct History {
    logs: Mutex<HashMap<ChannelId, ChannelLog>>,
}

impl History {
    pub fn open(data_dir: &str, channels: &[ChannelInfo]) -> anyhow::Result<Self> {
        let dir = PathBuf::from(data_dir);
        std::fs::create_dir_all(&dir)?;
        let mut logs = HashMap::new();
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
            logs.insert(ch.id, ChannelLog { file, recent });
        }
        Ok(Self { logs: Mutex::new(logs) })
    }

    pub fn append(&self, channel: ChannelId, rec: &ChatRecord) {
        let mut logs = self.logs.lock().unwrap();
        if let Some(log) = logs.get_mut(&channel) {
            if let Ok(line) = serde_json::to_string(rec) {
                let _ = writeln!(log.file, "{line}");
            }
            if log.recent.len() == MEM_CAP {
                log.recent.pop_front();
            }
            log.recent.push_back(rec.clone());
        }
    }

    pub fn recent(&self, channel: ChannelId, limit: usize) -> Vec<ChatRecord> {
        let logs = self.logs.lock().unwrap();
        match logs.get(&channel) {
            Some(log) => {
                let skip = log.recent.len().saturating_sub(limit);
                log.recent.iter().skip(skip).cloned().collect()
            }
            None => Vec::new(),
        }
    }
}
