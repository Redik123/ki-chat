//! Identité du serveur : nom, logo et adresse web publique, réglés par les
//! admins et persistés dans `data/server.json`.
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
        Ok(Self {
            path,
            info: Mutex::new(info),
        })
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

    /// Les membres peuvent-ils ajouter des morceaux au bot musique ?
    pub fn set_musique_membres_ajoutent(&self, oui: bool) -> anyhow::Result<()> {
        self.update(|info| info.musique_membres_ajoutent = oui)
    }

    /// Le salon du fil de jeu VALORANT, ou rien.
    pub fn set_fil_valorant(&self, channel: Option<ki_protocol::ChannelId>) -> anyhow::Result<()> {
        self.update(|info| info.fil_valorant = channel)
    }

    /// L'adresse web publique, déjà normalisée par l'appelant
    /// (`ki_protocol::normaliser_adresse_web`) ; vide : retour à
    /// l'automatique.
    pub fn set_adresse_web(&self, adresse: &str) -> anyhow::Result<()> {
        self.update(|info| info.adresse_web = adresse.to_string())
    }

    /// L'adresse web publique du serveur, sans barre finale : celle qu'un
    /// admin a réglée (Admin → Serveur), sinon `KI_PUBLIC_URL`. Jamais
    /// l'en-tête `Host` d'une requête : c'est le visiteur qui le choisit.
    /// Lue à chaque page et à chaque lien — sans recopier le logo.
    pub fn base_publique(&self) -> Option<String> {
        let admin = self.info.lock().unwrap().adresse_web.clone();
        base_de(&admin, std::env::var("KI_PUBLIC_URL").ok().as_deref())
    }

    /// Le verrou est tenu **pendant** l'écriture, comme dans les trois autres
    /// magasins. Le relâcher avant permettait à deux admins simultanés de
    /// publier chacun son instantané : la mémoire gardait le dernier
    /// changement, le disque l'autre, et l'écart n'apparaissait qu'au
    /// redémarrage suivant — le nom ou le logo revenu en arrière.
    fn update(&self, change: impl FnOnce(&mut ServerInfo)) -> anyhow::Result<()> {
        let mut info = self.info.lock().unwrap();
        change(&mut info);
        let json = serde_json::to_string_pretty(&*info)?;
        crate::store::write_atomic(&self.path, json.as_bytes()).context("écriture de server.json")
    }
}

/// Le choix de la base, sans l'état ni l'environnement — pour le tester.
/// Le réglage de l'admin l'emporte sur la variable ; l'un comme l'autre
/// doit porter son schéma.
fn base_de(admin: &str, env: Option<&str>) -> Option<String> {
    [Some(admin), env]
        .into_iter()
        .flatten()
        .map(|u| u.trim().trim_end_matches('/'))
        .find(|u| u.starts_with("https://") || u.starts_with("http://"))
        .map(str::to_string)
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

    /// L'adresse web réglée par un admin survit au redémarrage, et
    /// l'emporte sur la variable d'environnement.
    #[test]
    fn l_adresse_web_survit_et_l_emporte_sur_la_variable() {
        let dir = scratch("adresse-web");
        {
            let meta = ServerMeta::open(&dir).unwrap();
            meta.set_adresse_web("https://ts.baws.fun:8080").unwrap();
        }
        let meta = ServerMeta::open(&dir).unwrap();
        assert_eq!(meta.get().adresse_web, "https://ts.baws.fun:8080");
        assert_eq!(
            base_de("https://ts.baws.fun:8080/", Some("https://autre.example")).as_deref(),
            Some("https://ts.baws.fun:8080")
        );
        assert_eq!(base_de("", Some(" https://autre.example/ ")).as_deref(), Some("https://autre.example"));
        assert_eq!(base_de("", Some("autre.example")), None, "sans schéma, la variable ne vaut rien");
        assert_eq!(base_de("", None), None);
    }

    #[test]
    fn a_corrupt_file_falls_back_to_empty_instead_of_crashing() {
        let dir = scratch("corrupt");
        std::fs::write(PathBuf::from(&dir).join("server.json"), "{ pas du json").unwrap();
        let meta = ServerMeta::open(&dir).unwrap();
        assert_eq!(meta.get(), ServerInfo::default());
    }
}
