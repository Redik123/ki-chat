//! Identité du serveur : nom et logo, réglés par les admins et persistés
//! dans `data/server.json`.
//!
//! Ces données appartiennent au serveur, pas au client : elles sont
//! distribuées à la connexion et repoussées à tout le monde dès qu'un admin
//! les change. Un membre ordinaire ne peut donc pas afficher un autre logo
//! que celui du serveur auquel il est réellement connecté.

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Context;
use ki_protocol::ServerInfo;

pub struct ServerMeta {
    path: PathBuf,
    info: Mutex<ServerInfo>,
}

impl ServerMeta {
    /// Charge `data/server.json`, ou part d'une identité vide.
    pub fn open(data_dir: &str) -> anyhow::Result<Self> {
        let path = PathBuf::from(data_dir).join("server.json");
        let info = match std::fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => ServerInfo::default(),
            Err(e) => return Err(e).context("lecture de server.json"),
        };
        Ok(Self { path, info: Mutex::new(info) })
    }

    pub fn get(&self) -> ServerInfo {
        self.info.lock().unwrap().clone()
    }

    pub fn set_name(&self, name: &str) -> anyhow::Result<()> {
        self.update(|info| info.name = name.to_string())
    }

    pub fn set_icon(&self, icon: Option<String>) -> anyhow::Result<()> {
        self.update(|info| info.icon = icon)
    }

    fn update(&self, change: impl FnOnce(&mut ServerInfo)) -> anyhow::Result<()> {
        let snapshot = {
            let mut info = self.info.lock().unwrap();
            change(&mut info);
            info.clone()
        };
        let json = serde_json::to_string_pretty(&snapshot)?;
        crate::store::write_atomic(&self.path, json.as_bytes()).context("écriture de server.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dossier de travail jetable, propre à chaque test.
    fn scratch(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("ki-chat-meta-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    #[test]
    fn starts_empty_when_the_file_is_missing() {
        let meta = ServerMeta::open(&scratch("missing")).unwrap();
        assert_eq!(meta.get(), ServerInfo::default());
    }

    #[test]
    fn name_and_icon_survive_a_restart() {
        let dir = scratch("restart");
        {
            let meta = ServerMeta::open(&dir).unwrap();
            meta.set_name("Chez Kévin").unwrap();
            meta.set_icon(Some("dmlnbmV0dGU=".into())).unwrap();
        }
        // Nouveau processus : on relit ce qui a été écrit.
        let meta = ServerMeta::open(&dir).unwrap();
        assert_eq!(meta.get().name, "Chez Kévin");
        assert_eq!(meta.get().icon.as_deref(), Some("dmlnbmV0dGU="));

        // Retirer le logo ne doit pas effacer le nom.
        meta.set_icon(None).unwrap();
        let reread = ServerMeta::open(&dir).unwrap();
        assert_eq!(reread.get().name, "Chez Kévin");
        assert!(reread.get().icon.is_none());
    }

    #[test]
    fn a_corrupt_file_falls_back_to_empty_instead_of_crashing() {
        let dir = scratch("corrupt");
        std::fs::write(PathBuf::from(&dir).join("server.json"), "{ pas du json").unwrap();
        let meta = ServerMeta::open(&dir).unwrap();
        assert_eq!(meta.get(), ServerInfo::default());
    }
}
