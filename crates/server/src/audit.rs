//! Journal d'audit administratif : `data/audit.jsonl`.
//!
//! Même patron que [`crate::history`] — append-only, les N dernières entrées
//! gardées en mémoire pour les consultations. Ce qui y entre est tout ce
//! qu'un administrateur pourrait avoir à justifier plus tard : qui a banni
//! qui et pourquoi, quel code d'invitation a servi à créer quel compte,
//! qui a changé l'identité du serveur.
//!
//! Le fichier est volontairement lisible à la main : une entrée par ligne,
//! un verbe stable non traduit, de quoi le lire avec `grep` sur un serveur
//! sans y déployer d'outil.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ki_protocol::AuditRecord;

/// Entrées gardées en mémoire, donc consultables sans relire le fichier.
const MEM_CAP: usize = 500;

/// Au-delà, le fichier est mis de côté et un neuf le remplace. Sans cela il
/// grandirait indéfiniment sur un serveur de longue vie.
const ROTATE_BYTES: u64 = 8 * 1024 * 1024;

struct Inner {
    file: File,
    recent: VecDeque<AuditRecord>,
}

pub struct Audit {
    inner: Mutex<Inner>,
}

impl Audit {
    pub fn open(data_dir: &str) -> anyhow::Result<Self> {
        let dir = PathBuf::from(data_dir);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("audit.jsonl");
        rotate_if_large(&path)?;

        let mut recent = VecDeque::with_capacity(MEM_CAP);
        if path.exists() {
            let reader = BufReader::new(File::open(&path)?);
            for line in reader.lines() {
                let Ok(line) = line else { continue };
                if let Ok(rec) = serde_json::from_str::<AuditRecord>(&line) {
                    if recent.len() == MEM_CAP {
                        recent.pop_front();
                    }
                    recent.push_back(rec);
                }
            }
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self { inner: Mutex::new(Inner { file, recent }) })
    }

    /// Consigne une action. Ne renvoie rien et n'échoue jamais bruyamment :
    /// un journal en panne ne doit pas faire échouer la modération qu'il
    /// décrit. L'échec part dans les traces.
    pub fn record(&self, action: &str, actor: &str, target: &str, detail: &str) {
        let rec = AuditRecord {
            ts: crate::state::now_millis(),
            action: action.to_string(),
            actor: actor.to_string(),
            target: target.to_string(),
            detail: detail.to_string(),
        };
        let mut inner = self.inner.lock().unwrap();
        match serde_json::to_string(&rec) {
            Ok(line) => {
                if let Err(e) = writeln!(inner.file, "{line}") {
                    tracing::error!("écriture du journal d'audit impossible : {e}");
                }
            }
            Err(e) => tracing::error!("sérialisation d'une entrée d'audit impossible : {e}"),
        }
        if inner.recent.len() == MEM_CAP {
            inner.recent.pop_front();
        }
        inner.recent.push_back(rec);
    }

    /// Les `limit` dernières entrées, de la plus récente à la plus ancienne.
    pub fn recent(&self, limit: usize) -> Vec<AuditRecord> {
        let inner = self.inner.lock().unwrap();
        inner.recent.iter().rev().take(limit).cloned().collect()
    }
}

/// Met le journal de côté s'il a trop grossi. On garde le fichier plutôt que
/// de le tronquer : c'est une pièce à conviction, pas un cache.
fn rotate_if_large(path: &Path) -> anyhow::Result<()> {
    let Ok(meta) = std::fs::metadata(path) else { return Ok(()) };
    if meta.len() < ROTATE_BYTES {
        return Ok(());
    }
    let stamp = crate::state::now_millis();
    let archived = path.with_file_name(format!("audit-{stamp}.jsonl"));
    if let Err(e) = std::fs::rename(path, &archived) {
        tracing::error!("rotation du journal d'audit impossible : {e}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Les entrées ressortent de la plus récente à la plus ancienne, et
    /// survivent à une réouverture du fichier.
    #[test]
    fn records_are_persisted_newest_first() {
        let dir = std::env::temp_dir().join(format!("ki-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.to_str().unwrap();

        {
            let audit = Audit::open(path).unwrap();
            audit.record("invite.create", "kevin", "", "ki-abc");
            audit.record("member.ban", "kevin", "marie", "spam");
        }

        let audit = Audit::open(path).unwrap();
        let records = audit.recent(10);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].action, "member.ban");
        assert_eq!(records[0].target, "marie");
        assert_eq!(records[0].detail, "spam");
        assert_eq!(records[1].action, "invite.create");

        // La borne est respectée, et c'est bien le plus récent qui reste.
        let one = audit.recent(1);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].action, "member.ban");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
