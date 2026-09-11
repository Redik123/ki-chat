//! La boutique du jour de VALORANT, lue dans son **propre** client Riot.
//!
//! Pour soi, et pour soi seulement : rien ne part vers le serveur ki-chat,
//! rien n'est écrit vers le client Riot. Le client Riot doit être ouvert :
//! c'est lui qui prête ses jetons, par ses endpoints locaux, et ces jetons
//! ne quittent pas la machine — ils ne sont ni journalisés ni gardés, la
//! lecture faite ils disparaissent avec le fil qui les portait.
//!
//! Le chemin : le lockfile → la session (puuid) → les jetons
//! (`/entitlements/v1/token`) → la région (`-ares-deployment` de la session
//! externe) → la boutique (`pd.<région>.a.pvp.net/store/v3/storefront`) →
//! le nom et l'image de chaque skin chez valorant-api.com, en français.
//! Rien de tout cela n'est supporté par Riot : un jour ça casse, et ce
//! jour-là la section dit « indisponible », sans plus.

use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use egui::RichText;

use crate::theme::{ACCENT, TEXT_DIM, TEXT_FAINT};
use crate::valorant;

const TIMEOUT: Duration = Duration::from_secs(15);
/// L'image d'un skin pèse quelques centaines de kilo-octets.
const IMAGE_MAX: u64 = 4 * 1024 * 1024;
/// Ce que VALORANT lui-même annonce à ses serveurs ; sans quoi la porte
/// est fermée (Cloudflare, code 1010).
const AGENT_DU_JEU: &str = "ShooterGame/13 Windows/10.0.19043.1.256.64bit";

/// Une offre du jour : le skin, son prix en VP, son image.
pub struct Offre {
    pub nom: String,
    pub prix: u32,
    image: Option<egui::ColorImage>,
    texture: Option<egui::TextureHandle>,
}

pub struct Boutique {
    pub offres: Vec<Offre>,
    /// Quand la boutique tourne, en millisecondes Unix.
    pub expire_ms: u64,
}

enum Etat {
    Vide,
    EnCours,
    Prete(Boutique),
    Erreur(String),
}

/// La lecture, et ce qu'elle a donné.
pub struct Lecteur {
    etat: Arc<Mutex<Etat>>,
}

impl Default for Lecteur {
    fn default() -> Self {
        Self::new()
    }
}

impl Lecteur {
    pub fn new() -> Self {
        Self { etat: Arc::new(Mutex::new(Etat::Vide)) }
    }

    /// Lance une lecture si aucune n'est en cours.
    pub fn demander(&self, ctx: &egui::Context) {
        {
            let mut etat = self.etat.lock().unwrap();
            if matches!(*etat, Etat::EnCours) {
                return;
            }
            *etat = Etat::EnCours;
        }
        let etat = Arc::clone(&self.etat);
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("ki-boutique".into())
            .spawn(move || {
                let resultat = lire();
                let mut e = etat.lock().unwrap();
                *e = match resultat {
                    Ok(b) => {
                        ki_voice::journal(format!("boutique VALORANT lue : {} offres", b.offres.len()));
                        Etat::Prete(b)
                    }
                    Err(err) => {
                        ki_voice::journal(format!("boutique VALORANT : {err:#}"));
                        Etat::Erreur(format!("{err:#}"))
                    }
                };
                ctx.request_repaint();
            })
            .ok();
    }

    /// La section, dans les réglages : quatre offres, ou ce qui empêche
    /// de les lire. Une boutique périmée se relit d'elle-même.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let maintenant = maintenant_ms();
        let relire = {
            let etat = self.etat.lock().unwrap();
            match &*etat {
                Etat::Vide => true,
                Etat::Prete(b) => b.expire_ms <= maintenant,
                _ => false,
            }
        };
        if relire {
            self.demander(ui.ctx());
        }
        let mut etat = self.etat.lock().unwrap();
        match &mut *etat {
            Etat::Vide | Etat::EnCours => {
                ui.label(RichText::new("lecture dans ton client Riot…").color(TEXT_DIM).size(11.5));
            }
            Etat::Erreur(e) => {
                ui.label(RichText::new(format!("indisponible : {e}")).color(TEXT_DIM).size(11.5));
                if ui.button("Réessayer").clicked() {
                    *etat = Etat::Vide;
                }
            }
            Etat::Prete(b) => {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
                    for (i, offre) in b.offres.iter_mut().enumerate() {
                        if offre.texture.is_none() {
                            if let Some(image) = offre.image.take() {
                                offre.texture = Some(ui.ctx().load_texture(
                                    format!("boutique-{i}"),
                                    image,
                                    egui::TextureOptions::LINEAR,
                                ));
                            }
                        }
                        egui::Frame::new()
                            .fill(crate::theme::BG_RAISED)
                            .corner_radius(egui::CornerRadius::same(8))
                            .inner_margin(egui::Margin::same(8))
                            .show(ui, |ui| {
                                ui.set_width(150.0);
                                ui.vertical(|ui| {
                                    match &offre.texture {
                                        Some(t) => {
                                            ui.add(egui::Image::new(t).fit_to_exact_size(egui::vec2(134.0, 44.0)));
                                        }
                                        None => {
                                            ui.add_space(44.0);
                                        }
                                    }
                                    ui.add(egui::Label::new(RichText::new(&offre.nom).size(11.5)).truncate());
                                    ui.label(RichText::new(format!("{} VP", offre.prix)).color(ACCENT).strong().size(12.0));
                                });
                            });
                    }
                });
                let reste = b.expire_ms.saturating_sub(maintenant) / 1000;
                ui.label(
                    RichText::new(format!("se renouvelle dans {} h {:02} min", reste / 3600, (reste % 3600) / 60))
                        .color(TEXT_FAINT)
                        .size(10.5),
                );
                if ui.button("Relire").clicked() {
                    *etat = Etat::Vide;
                }
            }
        }
    }
}

fn maintenant_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Toute la lecture, sur le fil à part. Les jetons vivent ici et nulle
/// part ailleurs.
fn lire() -> anyhow::Result<Boutique> {
    let lf = valorant::lire_lockfile().ok_or_else(|| anyhow::anyhow!("client Riot fermé"))?;
    let client = valorant::Client::new(&lf);
    let session: valorant::Session = client.get("/chat/v1/session")?;
    if session.puuid.is_empty() {
        anyhow::bail!("pas encore connecté au client Riot");
    }
    let jetons: serde_json::Value = client.get("/entitlements/v1/token")?;
    let acces = jetons["accessToken"].as_str().unwrap_or("").to_string();
    let droit = jetons["token"].as_str().unwrap_or("").to_string();
    if acces.is_empty() || droit.is_empty() {
        anyhow::bail!("le client Riot n'a pas de jeton (pas connecté ?)");
    }
    let sessions: serde_json::Value = client.get("/product-session/v1/external-sessions")?;
    let region = sessions
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
        .filter_map(|s| s["launchConfiguration"]["arguments"].as_array())
        .flatten()
        .filter_map(|a| a.as_str())
        .find_map(|a| a.strip_prefix("-ares-deployment="))
        .ok_or_else(|| anyhow::anyhow!("VALORANT n'est pas lancé (région inconnue)"))?
        .to_string();
    if !region.chars().all(|c| c.is_ascii_lowercase()) {
        anyhow::bail!("région inattendue");
    }

    let version: serde_json::Value = ureq::get("https://valorant-api.com/v1/version")
        .set("User-Agent", "ki-chat")
        .timeout(TIMEOUT)
        .call()?
        .into_json()?;
    let version = version["data"]["riotClientVersion"].as_str().unwrap_or("").to_string();
    let plateforme = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        r#"{"platformType":"PC","platformOS":"Windows","platformOSVersion":"10.0.19042.1.256.64bit","platformChipset":"Unknown"}"#,
    );
    let url = format!("https://pd.{region}.a.pvp.net/store/v3/storefront/{}", session.puuid);
    let reponse = ureq::post(&url)
        .set("User-Agent", AGENT_DU_JEU)
        .set("Authorization", &format!("Bearer {acces}"))
        .set("X-Riot-Entitlements-JWT", &droit)
        .set("X-Riot-ClientPlatform", &plateforme)
        .set("X-Riot-ClientVersion", &version)
        .set("Content-Type", "application/json")
        .timeout(TIMEOUT)
        .send_string("{}")
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => anyhow::anyhow!("boutique refusée (HTTP {code})"),
            e => anyhow::anyhow!("boutique injoignable : {e}"),
        })?;
    let magasin: serde_json::Value = reponse.into_json()?;
    let panneau = &magasin["SkinsPanelLayout"];
    let reste = panneau["SingleItemOffersRemainingDurationInSeconds"].as_u64().unwrap_or(0);
    let mut offres = Vec::new();
    for offre in panneau["SingleItemStoreOffers"].as_array().into_iter().flatten().take(4) {
        let Some(item) = offre["Rewards"][0]["ItemID"].as_str() else { continue };
        if !item.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            continue;
        }
        let prix = offre["Cost"].as_object().and_then(|c| c.values().next()).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let (nom, image) = skin(item);
        offres.push(Offre { nom, prix, image, texture: None });
    }
    if offres.is_empty() {
        anyhow::bail!("boutique vide ou format inconnu");
    }
    Ok(Boutique { offres, expire_ms: maintenant_ms() + reste * 1000 })
}

/// Le nom français et l'image d'un niveau de skin, chez valorant-api.com.
/// Un raté laisse l'identifiant et pas d'image — mieux que rien.
fn skin(item: &str) -> (String, Option<egui::ColorImage>) {
    let fiche: Option<serde_json::Value> =
        ureq::get(&format!("https://valorant-api.com/v1/weapons/skinlevels/{item}?language=fr-FR"))
            .set("User-Agent", "ki-chat")
            .timeout(TIMEOUT)
            .call()
            .ok()
            .and_then(|r| r.into_json().ok());
    let Some(fiche) = fiche else { return (item.to_string(), None) };
    let nom = fiche["data"]["displayName"].as_str().unwrap_or(item).to_string();
    let image = fiche["data"]["displayIcon"]
        .as_str()
        .filter(|u| u.starts_with("https://media.valorant-api.com/"))
        .and_then(|u| {
            let mut octets = Vec::new();
            ureq::get(u)
                .set("User-Agent", "ki-chat")
                .timeout(TIMEOUT)
                .call()
                .ok()?
                .into_reader()
                .take(IMAGE_MAX)
                .read_to_end(&mut octets)
                .ok()?;
            crate::images::decode(&octets)
        });
    (nom, image)
}
