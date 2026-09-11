//! Les icônes de rang VALORANT, de valorant-api.com.
//!
//! Vingt-cinq petits PNG (Fer 1 à Radiant), téléchargés une fois sur un
//! fil à part, gardés sur disque à côté des réglages, montrés à côté du
//! pseudo, dans la fiche et sur la page Stats. Sans réseau, ou tant qu'ils
//! ne sont pas là, le nom du rang en couleur tient la place — rien ne
//! bloque, rien ne clignote.
//!
//! valorant-api.com est un miroir communautaire des ressources du jeu,
//! sans clé ; on n'y lit que le catalogue des paliers et les images.

use std::collections::HashMap;

use eframe::egui;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const CATALOGUE: &str = "https://valorant-api.com/v1/competitivetiers";
const TIMEOUT: Duration = Duration::from_secs(15);
/// Une petite icône pèse quelques dizaines de kilo-octets ; au-delà, on
/// ne lit pas.
const ICONE_MAX: u64 = 512 * 1024;
/// Les paliers qui ont une icône à montrer : Fer 1 à Radiant.
const PALIERS: std::ops::RangeInclusive<u8> = 3..=27;

/// Ce que le fil de téléchargement partage avec l'interface.
#[derive(Default)]
struct Partage {
    /// L'image décodée par palier ; `None` : ratée, on n'insiste pas
    /// avant la prochaine session.
    images: HashMap<u8, Option<egui::ColorImage>>,
    lance: bool,
}

pub struct Rangs {
    partage: Arc<Mutex<Partage>>,
    textures: HashMap<u8, egui::TextureHandle>,
    dossier: Option<PathBuf>,
}

impl Default for Rangs {
    fn default() -> Self {
        Self::new()
    }
}

impl Rangs {
    pub fn new() -> Self {
        Self {
            partage: Arc::default(),
            textures: HashMap::new(),
            dossier: eframe::storage_dir("ki-chat").map(|d| d.join("valorant").join("rangs")),
        }
    }

    /// Fait ce qu'il faut pour que l'icône du palier soit là bientôt :
    /// une texture depuis l'image déjà décodée, ou le fil de
    /// téléchargement s'il n'est pas parti.
    pub fn preparer(&mut self, ctx: &egui::Context, tier: u8) {
        if !PALIERS.contains(&tier) || self.textures.contains_key(&tier) {
            return;
        }
        let image = {
            let mut partage = self.partage.lock().unwrap();
            match partage.images.get(&tier) {
                Some(image) => image.clone(),
                None => {
                    if !partage.lance {
                        partage.lance = true;
                        self.lancer(ctx.clone());
                    }
                    None
                }
            }
        };
        if let Some(image) = image {
            let texture = ctx.load_texture(format!("rang-{tier}"), image, egui::TextureOptions::LINEAR);
            self.textures.insert(tier, texture);
        }
    }

    /// La texture d'un palier, si elle est prête.
    pub fn texture(&self, tier: u8) -> Option<&egui::TextureHandle> {
        self.textures.get(&tier)
    }

    fn lancer(&self, ctx: egui::Context) {
        let partage = Arc::clone(&self.partage);
        let dossier = self.dossier.clone();
        std::thread::Builder::new()
            .name("ki-rangs".into())
            .spawn(move || charger(&partage, dossier.as_deref(), &ctx))
            .ok();
    }
}

/// Le disque d'abord, le réseau pour ce qui manque, une image décodée à
/// la fois — l'interface se redessine à chacune.
fn charger(partage: &Mutex<Partage>, dossier: Option<&std::path::Path>, ctx: &egui::Context) {
    let mut manquants = Vec::new();
    for tier in PALIERS {
        let sur_disque = dossier
            .map(|d| d.join(format!("{tier}.png")))
            .and_then(|chemin| std::fs::read(chemin).ok())
            .and_then(|octets| crate::images::decode(&octets));
        match sur_disque {
            Some(image) => {
                partage.lock().unwrap().images.insert(tier, Some(image));
                ctx.request_repaint();
            }
            None => manquants.push(tier),
        }
    }
    if manquants.is_empty() {
        return;
    }
    let urls = match catalogue() {
        Ok(urls) => urls,
        Err(e) => {
            ki_voice::journal(format!("icônes de rang : catalogue injoignable ({e})"));
            let mut partage = partage.lock().unwrap();
            for tier in manquants {
                partage.images.insert(tier, None);
            }
            return;
        }
    };
    if let Some(d) = dossier {
        let _ = std::fs::create_dir_all(d);
    }
    for tier in manquants {
        let image = urls.get(&tier).and_then(|url| telecharger(url).ok()).and_then(|octets| {
            if let Some(d) = dossier {
                let _ = std::fs::write(d.join(format!("{tier}.png")), &octets);
            }
            crate::images::decode(&octets)
        });
        partage.lock().unwrap().images.insert(tier, image);
        ctx.request_repaint();
    }
}

/// Le catalogue des paliers : le dernier jeu d'icônes publié, palier →
/// URL de la petite icône.
fn catalogue() -> anyhow::Result<HashMap<u8, String>> {
    let corps: serde_json::Value =
        ureq::get(CATALOGUE).set("User-Agent", "ki-chat").timeout(TIMEOUT).call()?.into_json()?;
    let jeu = corps["data"].as_array().and_then(|d| d.last()).ok_or_else(|| anyhow::anyhow!("catalogue vide"))?;
    let mut urls = HashMap::new();
    for palier in jeu["tiers"].as_array().into_iter().flatten() {
        let (Some(tier), Some(url)) = (palier["tier"].as_u64(), palier["smallIcon"].as_str()) else {
            continue;
        };
        if url.starts_with("https://media.valorant-api.com/") {
            urls.insert(tier as u8, url.to_string());
        }
    }
    Ok(urls)
}

fn telecharger(url: &str) -> anyhow::Result<Vec<u8>> {
    let mut octets = Vec::new();
    ureq::get(url)
        .set("User-Agent", "ki-chat")
        .timeout(TIMEOUT)
        .call()?
        .into_reader()
        .take(ICONE_MAX)
        .read_to_end(&mut octets)?;
    Ok(octets)
}
