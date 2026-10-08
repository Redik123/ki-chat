//! La boutique du jour de VALORANT, lue dans son **propre** client Riot.
//!
//! Pour soi, et pour soi seulement : rien ne part vers le serveur ki-chat,
//! rien n'est écrit vers le client Riot. Le client Riot doit être ouvert :
//! c'est lui qui prête ses jetons, par ses endpoints locaux, et ces jetons
//! ne quittent pas la machine — ils ne sont ni journalisés ni gardés, la
//! lecture faite ils disparaissent avec le fil qui les portait.
//!
//! Le chemin : [`valorant::Acces`] — le lockfile → la session (puuid) →
//! les jetons (`/entitlements/v1/token`) → la région (`-ares-deployment`
//! du jeu lancé, sinon celle du client Riot) — → la boutique
//! (`pd.<région>.a.pvp.net/store/v3/storefront`) → le nom et l'image de
//! chaque skin chez valorant-api.com, en français.
//! Rien de tout cela n'est supporté par Riot : un jour ça casse, et ce
//! jour-là la section dit « indisponible », sans plus.

use ki_ui::jetons::{espace, marge, rayon, texte};
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
    /// Ratée, et depuis quand : on réessaie tout seul au bout d'un moment
    /// — VALORANT vient peut-être d'être lancé.
    Erreur(String, std::time::Instant),
}

/// Le temps avant de retenter après un échec, tant que la page est ouverte.
const NOUVEL_ESSAI: Duration = Duration::from_secs(45);

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
                        Etat::Erreur(format!("{err:#}"), std::time::Instant::now())
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
                Etat::Erreur(_, depuis) => depuis.elapsed() >= NOUVEL_ESSAI,
                Etat::EnCours => false,
            }
        };
        if relire {
            self.demander(ui.ctx());
        }
        let mut etat = self.etat.lock().unwrap();
        match &mut *etat {
            Etat::Vide | Etat::EnCours => {
                ui.label(RichText::new("lecture dans ton client Riot…").color(TEXT_DIM).size(texte::PETIT));
            }
            Etat::Erreur(e, _) => {
                ui.label(RichText::new(format!("indisponible : {e}")).color(TEXT_DIM).size(texte::PETIT));
                ui.ctx().request_repaint_after(NOUVEL_ESSAI);
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
                            .corner_radius(egui::CornerRadius::same(rayon::L))
                            .inner_margin(marge::egale(espace::M))
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
                                    ui.add(egui::Label::new(RichText::new(&offre.nom).size(texte::PETIT)).truncate());
                                    ui.label(RichText::new(format!("{} VP", offre.prix)).color(ACCENT).strong().size(texte::COURANT));
                                });
                            });
                    }
                });
                let reste = b.expire_ms.saturating_sub(maintenant) / 1000;
                ui.label(
                    RichText::new(format!("se renouvelle dans {} h {:02} min", reste / 3600, (reste % 3600) / 60))
                        .color(TEXT_FAINT)
                        .size(texte::MINUSCULE),
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

/// Toute la lecture, sur le fil à part. Les jetons vivent dans l'accès
/// et nulle part ailleurs.
fn lire() -> anyhow::Result<Boutique> {
    let acces = valorant::Acces::ouvrir()?;
    let magasin = acces
        .post(&format!("/store/v3/storefront/{}", acces.puuid), "{}")
        .map_err(|e| anyhow::anyhow!("boutique : {e}"))?;
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
