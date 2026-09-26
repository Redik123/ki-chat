//! Le catalogue VALORANT de valorant-api.com : les noms (files, cartes,
//! titres) et les images (portraits d'agents, bandeaux de cartes, cartes
//! de joueur) que la page du groupe, la fiche et la liste des membres
//! montrent.
//!
//! Sur le patron des icônes de rang ([`crate::rangs`]) : rien ne bloque,
//! rien ne clignote — tant qu'une image n'est pas là, le texte tient la
//! place. Le catalogue (quatre listes, quelques centaines de Ko) se relit
//! sur le disque et ne se retélécharge que quand la version du jeu
//! change : une petite requête `/v1/version` par session, et seulement
//! quand quelque chose de VALORANT s'affiche. Une image se télécharge la
//! première fois qu'on la montre, puis vit sur le disque à côté des
//! réglages. valorant-api.com est un miroir communautaire des ressources
//! du jeu, sans clé : on n'y envoie rien, on n'y lit que ça.
//!
//! Les noms servent aux modes et aux cartes sortis après cette version de
//! ki-chat : la table de ki-chat a le dernier mot, le catalogue ne comble
//! que ce qu'elle ignore ([`Index::nommer`]).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use eframe::egui;
use ki_protocol::JeuStatut;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const API: &str = "https://valorant-api.com/v1/";
const MEDIA: &str = "https://media.valorant-api.com/";
const TIMEOUT: Duration = Duration::from_secs(15);
/// Les listes pèsent une centaine de Ko chacune ; au-delà, on ne lit pas.
const JSON_MAX: u64 = 4 * 1024 * 1024;
/// Un portrait de minimap fait 6 Ko, un bandeau de carte 70, une carte de
/// joueur 100.
const IMAGE_MAX: u64 = 1024 * 1024;
/// La forme de `catalogue.json` : la changer invalide celui du disque.
/// 2 : les actes (pour les médailles).
const FORME: u32 = 2;

// ---------------------------------------------------------------------
// Les noms
// ---------------------------------------------------------------------

/// Ce qu'on garde des quatre listes, tel qu'on l'écrit sur le disque.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
struct Liste {
    #[serde(default)]
    forme: u32,
    /// La version du jeu que le catalogue décrit (`/v1/version`).
    #[serde(default)]
    version: String,
    /// (nom anglais — celui de HenrikDev —, portrait de minimap 64 px).
    #[serde(default)]
    agents: Vec<(String, String)>,
    /// (nom interne — la fin de `mapUrl` —, nom anglais, bandeau).
    #[serde(default)]
    cartes: Vec<(String, String, String)>,
    /// (identifiant de file, nom français).
    #[serde(default)]
    files: Vec<(String, String)>,
    /// (uuid, titre français).
    #[serde(default)]
    titres: Vec<(String, String)>,
    /// (uuid, « V26 · ACTE V », début, fin en millisecondes Unix).
    #[serde(default)]
    actes: Vec<(String, String, u64, u64)>,
}

/// Une table cherchée telle quelle d'abord — c'est le cas courant, sans
/// rien allouer à chaque image —, puis sans la casse.
#[derive(Debug, Default)]
struct Table<V> {
    exacte: HashMap<String, V>,
    minuscule: HashMap<String, V>,
}

impl<V: Clone> Table<V> {
    /// La première clé posée l'emporte : un nom affiché partagé par deux
    /// entrées (« The Range ») ne déloge pas la première.
    fn poser(&mut self, cle: &str, valeur: V) {
        self.exacte.entry(cle.to_string()).or_insert_with(|| valeur.clone());
        self.minuscule.entry(cle.to_lowercase()).or_insert(valeur);
    }

    fn chercher(&self, cle: &str) -> Option<&V> {
        if cle.is_empty() {
            return None;
        }
        self.exacte.get(cle).or_else(|| self.minuscule.get(&cle.to_lowercase()))
    }
}

/// Le catalogue en mémoire, prêt à chercher.
#[derive(Debug, Default)]
pub struct Index {
    agents: Table<String>,
    /// Par nom interne ET par nom anglais : (nom anglais, bandeau).
    cartes: Table<(String, String)>,
    files: Table<String>,
    titres: HashMap<String, String>,
    actes: Vec<(String, String, u64, u64)>,
}

impl Index {
    fn depuis(liste: Liste) -> Self {
        let mut index = Index::default();
        for (nom, url) in liste.agents {
            index.agents.poser(&nom, url);
        }
        for (interne, nom, bandeau) in liste.cartes {
            let valeur = (nom.clone(), bandeau);
            index.cartes.poser(&interne, valeur.clone());
            index.cartes.poser(&nom, valeur);
        }
        for (id, nom) in liste.files {
            index.files.poser(&id, nom);
        }
        for (uuid, titre) in liste.titres {
            index.titres.insert(uuid.to_lowercase(), titre);
        }
        index.actes = liste.actes;
        index
    }

    /// L'acte en cours à cette date : son uuid et son nom
    /// (« V26 · ACTE V »).
    pub fn acte_en_cours(&self, maintenant_ms: u64) -> Option<(&str, &str)> {
        self.actes
            .iter()
            .find(|(_, _, debut, fin)| *debut <= maintenant_ms && maintenant_ms < *fin)
            .map(|(uuid, nom, _, _)| (uuid.as_str(), nom.as_str()))
    }

    /// Le nom d'une carte, qu'on la donne par son nom interne
    /// (« Plummet ») ou par son nom affiché.
    pub fn nom_de_carte(&self, carte: &str) -> Option<&str> {
        self.cartes.chercher(carte).map(|(nom, _)| nom.as_str())
    }

    /// Le statut avec les noms du catalogue là où la table de ki-chat n'en
    /// a pas : une file inconnue de cette version, une carte restée à son
    /// nom interne chez un membre qui a une version d'avant. Rien à
    /// changer : le statut tel quel, sans copie.
    pub fn nommer<'a>(&self, j: &'a JeuStatut) -> Cow<'a, JeuStatut> {
        let (base, console) = match j.file.strip_prefix("console_") {
            Some(reste) => (reste, true),
            None => (j.file.as_str(), false),
        };
        let file = match ki_protocol::nom_de_file(base) {
            None => self.files.chercher(base),
            Some(_) => None,
        };
        let carte = self.nom_de_carte(&j.carte).filter(|nom| *nom != j.carte);
        if file.is_none() && carte.is_none() {
            return Cow::Borrowed(j);
        }
        let mut nomme = j.clone();
        if let Some(nom) = file {
            nomme.file = if console { format!("console_{nom}") } else { nom.clone() };
        }
        if let Some(nom) = carte {
            nomme.carte = nom.to_string();
        }
        Cow::Owned(nomme)
    }
}

/// L'état du catalogue pour toute l'application : il ne se lit qu'une
/// fois par session, et la liste des membres s'en sert sans rien tenir.
struct Global {
    lance: bool,
    index: Option<Arc<Index>>,
    /// De quoi redessiner quand il arrive.
    ctx: Option<egui::Context>,
}

static GLOBAL: Mutex<Global> = Mutex::new(Global { lance: false, index: None, ctx: None });

fn global() -> MutexGuard<'static, Global> {
    GLOBAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Le catalogue s'il est lu ; sinon lance sa lecture (une fois par
/// session) et rend `None` en attendant.
pub fn index() -> Option<Arc<Index>> {
    let mut g = global();
    if !g.lance {
        g.lance = true;
        let lance = std::thread::Builder::new().name("ki-valo-catalogue".into()).spawn(|| {
            let index = charger_index(dossier().as_deref()).map(Arc::new);
            let mut g = global();
            g.index = index;
            if let Some(ctx) = &g.ctx {
                ctx.request_repaint();
            }
        });
        if let Err(e) = lance {
            ki_voice::journal(format!("VALORANT : catalogue non lancé ({e})"));
        }
    }
    g.index.clone()
}

/// Le catalogue s'il est déjà lu, sans rien lancer.
fn index_lu() -> Option<Arc<Index>> {
    global().index.clone()
}

/// La ligne de statut d'un membre (« compétitive · Ascent · 7-5 »), avec
/// les noms que cette version ne connaît pas pris dans le catalogue — un
/// mode ou une carte sortis depuis.
pub fn ligne(j: &JeuStatut) -> String {
    if !j.est_valorant() {
        return j.ligne();
    }
    match index() {
        Some(index) => index.nommer(j).ligne(),
        None => j.ligne(),
    }
}

fn dossier() -> Option<PathBuf> {
    eframe::storage_dir("ki-chat").map(|d| d.join("valorant"))
}

/// Le disque si la version du jeu n'a pas changé, sinon les quatre
/// listes ; hors ligne, ce qu'on avait. `None` seulement la toute
/// première fois sans réseau.
fn charger_index(dossier: Option<&Path>) -> Option<Index> {
    let chemin = dossier.map(|d| d.join("catalogue.json"));
    let sur_disque: Option<Liste> = chemin
        .as_ref()
        .and_then(|c| std::fs::read(c).ok())
        .and_then(|octets| serde_json::from_slice::<Liste>(&octets).ok())
        .filter(|l| l.forme == FORME);
    match lire_json(&format!("{API}version")).map(|v| v["data"]["version"].as_str().unwrap_or("").to_string()) {
        Ok(version) if sur_disque.as_ref().is_some_and(|l| l.version == version) => sur_disque.map(Index::depuis),
        Ok(version) => match telecharger_liste(version) {
            Ok(liste) => {
                if let (Some(d), Some(c)) = (dossier, &chemin) {
                    let _ = std::fs::create_dir_all(d);
                    if let Ok(json) = serde_json::to_vec(&liste) {
                        // Écrit à côté puis renommé : jamais un catalogue à moitié.
                        let provisoire = c.with_extension("json.part");
                        if std::fs::write(&provisoire, json).is_ok() {
                            let _ = std::fs::rename(&provisoire, c);
                        }
                    }
                }
                ki_voice::journal(format!(
                    "VALORANT : catalogue {} lu ({} agents, {} cartes, {} files)",
                    liste.version,
                    liste.agents.len(),
                    liste.cartes.len(),
                    liste.files.len()
                ));
                Some(Index::depuis(liste))
            }
            Err(e) => {
                ki_voice::journal(format!("VALORANT : catalogue illisible ({e})"));
                sur_disque.map(Index::depuis)
            }
        },
        Err(e) => {
            ki_voice::journal(format!("VALORANT : catalogue injoignable ({e})"));
            sur_disque.map(Index::depuis)
        }
    }
}

fn telecharger_liste(version: String) -> anyhow::Result<Liste> {
    let agents = lire_json(&format!("{API}agents?isPlayableCharacter=true"))?;
    let cartes = lire_json(&format!("{API}maps"))?;
    let files = lire_json(&format!("{API}gamemodes/queues?language=fr-FR"))?;
    // Les titres et les actes ne sont que des détails de la fiche : sans
    // eux, le reste sert.
    let titres = lire_json(&format!("{API}playertitles?language=fr-FR")).unwrap_or(Value::Null);
    let saisons = lire_json(&format!("{API}seasons?language=fr-FR")).unwrap_or(Value::Null);
    let mut liste = liste_depuis(version, &agents, &cartes, &files, &titres);
    liste.actes = actes_depuis(&saisons);
    if liste.agents.is_empty() && liste.cartes.is_empty() {
        anyhow::bail!("catalogue vide");
    }
    Ok(liste)
}

/// Les quatre réponses réduites à ce qu'on garde. Une image qui ne vient
/// pas de media.valorant-api.com ne passe pas.
fn liste_depuis(version: String, agents: &Value, cartes: &Value, files: &Value, titres: &Value) -> Liste {
    let donnees = |v: &Value| v["data"].as_array().cloned().unwrap_or_default();
    let mut liste = Liste { forme: FORME, version, ..Liste::default() };
    for a in donnees(agents) {
        if let (Some(nom), Some(url)) = (a["displayName"].as_str(), a["minimapPortrait"].as_str()) {
            if url.starts_with(MEDIA) && !nom.trim().is_empty() {
                liste.agents.push((nom.trim().to_string(), url.to_string()));
            }
        }
    }
    for c in donnees(cartes) {
        let (Some(chemin), Some(nom)) = (c["mapUrl"].as_str(), c["displayName"].as_str()) else { continue };
        let interne = chemin.rsplit('/').next().unwrap_or("").trim();
        let bandeau = c["listViewIcon"].as_str().filter(|u| u.starts_with(MEDIA)).unwrap_or("");
        if !interne.is_empty() && !nom.trim().is_empty() {
            liste.cartes.push((interne.to_string(), nom.trim().to_string(), bandeau.to_string()));
        }
    }
    for f in donnees(files) {
        if let (Some(id), Some(nom)) = (f["queueId"].as_str(), f["displayName"].as_str()) {
            if !id.is_empty() && !nom.trim().is_empty() {
                liste.files.push((id.to_string(), nom.trim().to_string()));
            }
        }
    }
    for t in donnees(titres) {
        let uuid = t["uuid"].as_str().and_then(ki_protocol::uuid_valorant);
        let texte = t["titleText"].as_str().map(str::trim).filter(|s| !s.is_empty());
        if let (Some(uuid), Some(texte)) = (uuid, texte) {
            liste.titres.push((uuid, texte.to_string()));
        }
    }
    liste
}

/// Les actes de `/v1/seasons` (ceux dont le type dit `Act`), nommés avec
/// leur épisode : « V26 · ACTE V ».
fn actes_depuis(saisons: &Value) -> Vec<(String, String, u64, u64)> {
    let liste = saisons["data"].as_array().cloned().unwrap_or_default();
    let nom_de = |uuid: &str| {
        liste
            .iter()
            .find(|s| s["uuid"].as_str() == Some(uuid))
            .and_then(|s| s["displayName"].as_str())
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    };
    let date = |v: &Value| {
        v.as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.timestamp_millis().max(0) as u64)
    };
    liste
        .iter()
        .filter(|s| s["type"].as_str().is_some_and(|t| t.ends_with("Act")))
        .filter_map(|s| {
            let uuid = ki_protocol::uuid_valorant(s["uuid"].as_str()?)?;
            let acte = s["displayName"].as_str()?.trim();
            let episode = s["parentUuid"].as_str().map(nom_de).unwrap_or_default();
            let nom = if episode.is_empty() { acte.to_string() } else { format!("{episode} · {acte}") };
            Some((uuid, nom, date(&s["startTime"])?, date(&s["endTime"])?))
        })
        .collect()
}

fn lire_json(url: &str) -> anyhow::Result<Value> {
    let lecteur = ureq::get(url).set("User-Agent", "ki-chat").timeout(TIMEOUT).call()?.into_reader();
    Ok(serde_json::from_reader(lecteur.take(JSON_MAX))?)
}

// ---------------------------------------------------------------------
// Les images
// ---------------------------------------------------------------------

/// Ce que le fil des images partage avec l'interface.
#[derive(Default)]
struct Partage {
    /// Les adresses déjà demandées — en file, en vol, prêtes ou ratées :
    /// jamais deux fois par session.
    connues: HashSet<String>,
    file: VecDeque<String>,
    /// Décodées, à monter en textures au prochain `preparer` ; `None` :
    /// ratée, on n'insiste pas avant la prochaine session.
    pretes: Vec<(String, Option<egui::ColorImage>)>,
    ouvrier: bool,
}

/// Les images du catalogue, par adresse. Les dessins la lisent en
/// lecture seule : une image absente s'y demande au passage, et
/// [`Catalogue::preparer`] la monte en texture à l'image suivante.
pub struct Catalogue {
    partage: Arc<Mutex<Partage>>,
    textures: HashMap<String, egui::TextureHandle>,
    index: Option<Arc<Index>>,
    ctx: Option<egui::Context>,
    dossier: Option<PathBuf>,
    /// Un dégradé noir, opaque à gauche et presque transparent à droite,
    /// pour lire un texte posé sur une image : une texture plutôt qu'un
    /// maillage, pour qu'il prenne les coins arrondis de l'image.
    voile: Option<egui::TextureHandle>,
}

impl Default for Catalogue {
    fn default() -> Self {
        Self::new()
    }
}

impl Catalogue {
    pub fn new() -> Self {
        Self {
            partage: Arc::default(),
            textures: HashMap::new(),
            index: None,
            ctx: None,
            dossier: dossier().map(|d| d.join("images")),
            voile: None,
        }
    }

    /// Le dégradé à poser sur une image sous un texte (voir `voile`).
    pub fn voile(&self) -> Option<&egui::TextureHandle> {
        self.voile.as_ref()
    }

    /// Une fois par image, avant de dessiner : retient de quoi redessiner,
    /// relève le catalogue s'il vient d'arriver, et monte en textures les
    /// images décodées depuis. Ne télécharge rien de lui-même.
    pub fn preparer(&mut self, ctx: &egui::Context) {
        if self.ctx.is_none() {
            self.ctx = Some(ctx.clone());
            global().ctx.get_or_insert_with(|| ctx.clone());
        }
        if self.voile.is_none() {
            const LARGEUR: usize = 64;
            let pixels = (0..LARGEUR)
                .map(|x| {
                    let t = x as f32 / (LARGEUR - 1) as f32;
                    egui::Color32::from_black_alpha((220.0 - 185.0 * t).round() as u8)
                })
                .collect();
            let image = egui::ColorImage::new([LARGEUR, 1], pixels);
            self.voile = Some(ctx.load_texture("valo:voile", image, egui::TextureOptions::LINEAR));
        }
        if self.index.is_none() {
            self.index = index_lu();
        }
        let pretes = std::mem::take(&mut self.partage.lock().unwrap_or_else(|e| e.into_inner()).pretes);
        for (url, image) in pretes {
            if let Some(image) = image {
                let texture = ctx.load_texture(format!("valo:{}", nom_de_fichier(&url)), image, egui::TextureOptions::LINEAR);
                self.textures.insert(url, texture);
            }
        }
    }

    /// La page ou la fiche s'ouvre : le catalogue doit venir.
    pub fn demarrer(&mut self) {
        if self.index.is_none() {
            self.index = index();
        }
    }

    /// Le statut avec les noms du catalogue, s'il est là.
    pub fn nommer<'a>(&self, j: &'a JeuStatut) -> Cow<'a, JeuStatut> {
        match &self.index {
            Some(index) if j.est_valorant() => index.nommer(j),
            _ => Cow::Borrowed(j),
        }
    }

    /// Le portrait d'un agent, par son nom anglais (celui de HenrikDev).
    pub fn agent(&self, nom: &str) -> Option<&egui::TextureHandle> {
        let url = self.index.as_ref()?.agents.chercher(nom.trim())?;
        self.texture(url)
    }

    /// Le bandeau d'une carte (456 × 100), par son nom affiché ou interne.
    pub fn bandeau(&self, carte: &str) -> Option<&egui::TextureHandle> {
        let (_, url) = self.index.as_ref()?.cartes.chercher(carte.trim())?;
        if url.is_empty() {
            return None;
        }
        self.texture(url)
    }

    /// Une carte de joueur en large (452 × 128), par son uuid. Comme les
    /// autres images, elle attend que le catalogue soit là : c'est lui qui
    /// dit qu'une page VALORANT s'est ouverte.
    pub fn carte_joueur(&self, uuid: &str) -> Option<&egui::TextureHandle> {
        self.index.as_ref()?;
        let uuid = ki_protocol::uuid_valorant(uuid)?;
        self.texture(&format!("{MEDIA}playercards/{uuid}/wideart.png"))
    }

    /// Le texte d'un titre de joueur, par son uuid.
    pub fn titre(&self, uuid: &str) -> Option<&str> {
        let uuid = ki_protocol::uuid_valorant(uuid)?;
        self.index.as_ref()?.titres.get(&uuid).map(String::as_str)
    }

    fn texture(&self, url: &str) -> Option<&egui::TextureHandle> {
        if let Some(texture) = self.textures.get(url) {
            return Some(texture);
        }
        self.vouloir(url);
        None
    }

    /// Met une image en file — une fois par session — et lance le fil
    /// s'il ne tourne pas.
    fn vouloir(&self, url: &str) {
        if !url.starts_with(MEDIA) {
            return;
        }
        let mut p = self.partage.lock().unwrap_or_else(|e| e.into_inner());
        // Demandée à chaque image tant qu'elle n'est pas là : on ne copie
        // l'adresse que la première fois.
        if p.connues.contains(url) {
            return;
        }
        p.connues.insert(url.to_string());
        p.file.push_back(url.to_string());
        if p.ouvrier {
            return;
        }
        p.ouvrier = true;
        let (partage, dossier, ctx) = (Arc::clone(&self.partage), self.dossier.clone(), self.ctx.clone());
        let lance = std::thread::Builder::new()
            .name("ki-valo-images".into())
            .spawn(move || ouvrier(&partage, dossier.as_deref(), ctx.as_ref()));
        if lance.is_err() {
            p.ouvrier = false;
        }
    }
}

/// Le fil des images : une à la fois, le disque d'abord, jusqu'à ce que
/// la file soit vide.
fn ouvrier(partage: &Mutex<Partage>, dossier: Option<&Path>, ctx: Option<&egui::Context>) {
    loop {
        let url = {
            let mut p = partage.lock().unwrap_or_else(|e| e.into_inner());
            match p.file.pop_front() {
                Some(url) => url,
                None => {
                    p.ouvrier = false;
                    return;
                }
            }
        };
        let image = charger_image(&url, dossier);
        partage.lock().unwrap_or_else(|e| e.into_inner()).pretes.push((url, image));
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    }
}

/// Une seule ligne de journal par session pour les images : hors ligne,
/// elles échoueraient toutes.
static IMAGE_RATEE: AtomicBool = AtomicBool::new(false);

fn charger_image(url: &str, dossier: Option<&Path>) -> Option<egui::ColorImage> {
    let chemin = dossier.map(|d| d.join(nom_de_fichier(url)));
    if let Some(image) = chemin.as_ref().and_then(|c| std::fs::read(c).ok()).and_then(|o| crate::images::decode(&o)) {
        return Some(image);
    }
    let octets = match telecharger(url) {
        Ok(octets) => octets,
        Err(e) => {
            if !IMAGE_RATEE.swap(true, Ordering::Relaxed) {
                ki_voice::journal(format!("VALORANT : image injoignable ({url}) : {e}"));
            }
            return None;
        }
    };
    let image = crate::images::decode(&octets)?;
    if let (Some(d), Some(c)) = (dossier, &chemin) {
        let _ = std::fs::create_dir_all(d);
        let _ = std::fs::write(c, &octets);
    }
    Some(image)
}

fn telecharger(url: &str) -> anyhow::Result<Vec<u8>> {
    let mut octets = Vec::new();
    ureq::get(url)
        .set("User-Agent", "ki-chat")
        .timeout(TIMEOUT)
        .call()?
        .into_reader()
        .take(IMAGE_MAX)
        .read_to_end(&mut octets)?;
    Ok(octets)
}

/// Le nom sur le disque d'une image du catalogue : son chemin chez
/// media.valorant-api.com, à plat, sans rien d'autre que des lettres,
/// des chiffres, des points et des tirets — et préfixé, pour qu'aucun
/// nom ne soit jamais « .. ».
fn nom_de_fichier(url: &str) -> String {
    let plat: String = url
        .strip_prefix(MEDIA)
        .unwrap_or(url)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect();
    format!("v-{plat}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ki_protocol::JeuEtat;

    fn liste_de_test() -> Liste {
        let agents = serde_json::json!({"data": [
            {"displayName": "KAY/O", "minimapPortrait": "https://media.valorant-api.com/agents/601dbbe7/minimapportrait.png"},
            {"displayName": "Pirate", "minimapPortrait": "https://ailleurs.example/x.png"}
        ]});
        let cartes = serde_json::json!({"data": [
            {"mapUrl": "/Game/Maps/Plummet/Plummet", "displayName": "Summit", "listViewIcon": "https://media.valorant-api.com/maps/756d/listviewicon.png"},
            {"mapUrl": "/Game/Maps/HURM/HURM_Bowl/HURM_Bowl", "displayName": "Kasbah", "listViewIcon": null}
        ]});
        let files = serde_json::json!({"data": [
            {"queueId": "abilitydraftarena", "displayName": "Gauntlet: Glitched"},
            {"queueId": "futur", "displayName": "Mode du futur"},
            {"queueId": "console_futur", "displayName": "Mode du futur"}
        ]});
        let titres = serde_json::json!({"data": [
            {"uuid": "48D870A2-4493-EBF8-7D6F-979BE914DC43", "titleText": "Fortune "},
            {"uuid": "pas-un-uuid", "titleText": "Rien"}
        ]});
        liste_depuis("13.06".into(), &agents, &cartes, &files, &titres)
    }

    /// Les réponses se réduisent à ce qu'on garde ; une image d'ailleurs et
    /// un uuid douteux ne passent pas.
    #[test]
    fn les_listes_se_reduisent() {
        let l = liste_de_test();
        assert_eq!(l.agents.len(), 1);
        assert_eq!(l.cartes[0], ("Plummet".into(), "Summit".into(), "https://media.valorant-api.com/maps/756d/listviewicon.png".into()));
        assert_eq!(l.cartes[1].2, "");
        assert_eq!(l.titres, vec![("48d870a2-4493-ebf8-7d6f-979be914dc43".to_string(), "Fortune".to_string())]);
        // Et le disque les relit telles quelles.
        let relue: Liste = serde_json::from_slice(&serde_json::to_vec(&l).unwrap()).unwrap();
        assert_eq!(relue, l);
    }

    /// Le catalogue ne comble que ce que la table de ki-chat ignore : une
    /// file connue garde son nom français, une inconnue prend celui du
    /// catalogue, console comprise ; une carte restée à son nom interne
    /// prend son nom de joueur.
    #[test]
    fn le_catalogue_nomme_ce_que_ki_chat_ignore() {
        let index = Index::depuis(liste_de_test());
        let statut = |file: &str, carte: &str| JeuStatut {
            etat: JeuEtat::EnJeu,
            file: file.into(),
            carte: carte.into(),
            score_allie: 3,
            score_adverse: 1,
            ..JeuStatut::default()
        };
        assert_eq!(index.nommer(&statut("futur", "Plummet")).ligne(), "Mode du futur · Summit · 3-1");
        assert_eq!(index.nommer(&statut("console_futur", "Summit")).libelle_file(), "Mode du futur (console)");
        // Connue de ki-chat : son nom à elle, pas « Gauntlet: Glitched ».
        assert_eq!(index.nommer(&statut("abilitydraftarena", "Kasbah")).ligne(), "gauntlet · Kasbah · 3-1");
        assert_eq!(index.nommer(&statut("inconnue", "Zeta")).ligne(), "inconnue · Zeta · 3-1");
        assert_eq!(index.nom_de_carte("hurm_bowl"), Some("Kasbah"));
    }

    /// Contre le vrai valorant-api.com — réseau, donc ignoré :
    /// `cargo test -p ki-client-gui -- --ignored le_vrai_catalogue`.
    /// Les noms de HenrikDev (« KAY/O ») retrouvent leur portrait, la
    /// carte de la 13.06 son nom, et la seconde lecture vient du disque.
    #[test]
    #[ignore]
    fn le_vrai_catalogue_se_lit() {
        let dossier = std::env::temp_dir().join(format!("ki-valo-catalogue-{}", std::process::id()));
        let index = charger_index(Some(&dossier)).expect("le catalogue se lit");
        for agent in ["Jett", "KAY/O", "Gekko", "Sova"] {
            assert!(index.agents.chercher(agent).is_some(), "{agent}");
        }
        assert_eq!(index.nom_de_carte("Plummet"), Some("Summit"));
        assert_eq!(index.nom_de_carte("HURM_Alley"), Some("District"));
        assert!(index.files.chercher("abilitydraftarena").is_some());
        assert!(!index.titres.is_empty());
        // Le 27 septembre 2026, c'est l'acte V de V26.
        assert_eq!(
            index.acte_en_cours(1_790_467_200_000),
            Some(("8102cd81-43a0-d0d7-bd59-47b8fe9bed1b", "V26 · ACTE V"))
        );
        assert!(dossier.join("catalogue.json").exists());
        let url = index.agents.chercher("Jett").unwrap().clone();
        let image = charger_image(&url, Some(&dossier)).expect("le portrait se télécharge");
        assert_eq!(image.size, [64, 64]);
        assert!(dossier.join(nom_de_fichier(&url)).exists(), "et se garde sur le disque");
        let _ = std::fs::remove_dir_all(&dossier);
    }

    /// Les actes se nomment avec leur épisode et se trouvent à leur date ;
    /// un épisode n'est pas un acte.
    #[test]
    fn l_acte_en_cours_se_trouve_a_sa_date() {
        let saisons = serde_json::json!({"data": [
            {"uuid": "3737c391-497a-6e82-aeb5-cc9f701f72e2", "displayName": "V26", "type": null,
             "startTime": "2026-06-24T00:00:00Z", "endTime": "2027-01-06T00:00:00Z"},
            {"uuid": "8102cd81-43a0-d0d7-bd59-47b8fe9bed1b", "displayName": "ACTE V", "type": "EAresSeasonType::Act",
             "startTime": "2026-08-19T00:00:00Z", "endTime": "2026-10-14T00:00:00Z", "parentUuid": "3737c391-497a-6e82-aeb5-cc9f701f72e2"},
            {"uuid": "d816f426-48ea-f052-117f-9697a155b319", "displayName": "ACTE VI", "type": "EAresSeasonType::Act",
             "startTime": "2026-10-14T00:00:00Z", "endTime": "pas une date"}
        ]});
        let actes = actes_depuis(&saisons);
        assert_eq!(actes.len(), 1, "l'épisode et l'acte mal daté ne passent pas");
        let index = Index::depuis(Liste { actes, ..Liste::default() });
        assert_eq!(index.acte_en_cours(1_790_467_200_000), Some(("8102cd81-43a0-d0d7-bd59-47b8fe9bed1b", "V26 · ACTE V")));
        assert_eq!(index.acte_en_cours(1_700_000_000_000), None);
    }

    #[test]
    fn une_image_se_range_sous_un_nom_sans_surprise() {
        assert_eq!(
            nom_de_fichier("https://media.valorant-api.com/agents/601dbbe7/minimapportrait.png"),
            "v-agents_601dbbe7_minimapportrait.png"
        );
        assert_eq!(nom_de_fichier("https://media.valorant-api.com/../../x?y=z"), "v-.._.._x_y_z");
        assert_eq!(nom_de_fichier("https://media.valorant-api.com/.."), "v-..");
    }
}
