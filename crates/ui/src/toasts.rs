//! Les toasts : de courts messages qui se posent dans un coin de la fenêtre,
//! se lisent, et s'en vont seuls.
//!
//! Pour ce que l'appli a à dire sans que personne l'ait demandé — « connexion
//! rétablie », « réglages exportés » — et qui n'a pas de place à l'écran :
//! ces messages n'existaient que dans une ligne des fenêtres de réglages, et
//! ne se voyaient donc que là.
//!
//! Un toast reste le temps de se lire (plus long si le texte l'est, plus
//! long encore pour une erreur), se ferme d'un clic, et ne s'empile pas avec
//! son jumeau : le même message redit ne fait que repartir pour un tour.

use egui::{Align2, CornerRadius, Order, Sense, Stroke, Vec2};

use crate::typo::{Texte, Ton};
use crate::icones as icons;
use crate::jetons::couleur as theme;
use crate::jetons::{espace, marge, ombre, rayon};

/// Le temps de base à l'écran, en secondes.
const DUREE: f64 = 4.0;
/// Ce qu'on ajoute par caractère : un message long se lit plus longtemps.
const PAR_CARACTERE: f64 = 0.05;
/// Une erreur ou un avertissement reste davantage.
const DUREE_EN_PLUS_SI_GRAVE: f64 = 4.0;
/// Jamais plus que ça, quelle que soit la longueur.
const DUREE_MAX: f64 = 14.0;
/// La fin de vie, en fondu.
const FONDU: f64 = 0.35;
/// Au-delà, les plus anciens cèdent la place.
const EMPILES_MAX: usize = 4;
const LARGEUR: f32 = 340.0;

struct Toast {
    id: u64,
    texte: String,
    ton: Ton,
    /// L'heure d'egui à laquelle il est apparu (ou reparti pour un tour).
    depuis: f64,
}

impl Toast {
    fn duree(&self) -> f64 {
        let grave = self.ton.est_grave();
        let base = DUREE + self.texte.chars().count() as f64 * PAR_CARACTERE;
        (base + if grave { DUREE_EN_PLUS_SI_GRAVE } else { 0.0 }).min(DUREE_MAX)
    }

    fn reste(&self, maintenant: f64) -> f64 {
        self.depuis + self.duree() - maintenant
    }
}

/// La pile des toasts à l'écran.
#[derive(Default)]
pub struct Toasts {
    pile: Vec<Toast>,
    prochain: u64,
}

impl Toasts {
    /// Pose un message. `maintenant` : l'heure d'egui (`ctx.input(|i| i.time)`).
    pub fn ajouter(&mut self, ton: Ton, texte: impl Into<String>, maintenant: f64) {
        let texte = texte.into();
        if texte.trim().is_empty() {
            return;
        }
        // Le même message déjà à l'écran : il repart pour un tour, en bas de
        // la pile, plutôt que de s'afficher deux fois.
        if let Some(i) = self.pile.iter().position(|t| t.texte == texte && t.ton == ton) {
            let mut jumeau = self.pile.remove(i);
            jumeau.depuis = maintenant;
            self.pile.push(jumeau);
            return;
        }
        self.prochain += 1;
        self.pile.push(Toast { id: self.prochain, texte, ton, depuis: maintenant });
        let trop = self.pile.len().saturating_sub(EMPILES_MAX);
        self.pile.drain(..trop);
    }

    /// Retire ceux dont le temps est passé.
    pub fn purger(&mut self, maintenant: f64) {
        self.pile.retain(|t| t.reste(maintenant) > 0.0);
    }

    pub fn est_vide(&self) -> bool {
        self.pile.is_empty()
    }

    /// Les textes à l'écran, du plus ancien au plus récent.
    pub fn textes(&self) -> Vec<&str> {
        self.pile.iter().map(|t| t.texte.as_str()).collect()
    }

    /// Dessine la pile dans le coin bas droit, à `marge_bas` du bord — de
    /// quoi passer au-dessus d'une barre du bas. À appeler à chaque image :
    /// elle retire ce qui a fait son temps et demande l'image suivante tant
    /// qu'il reste quelque chose à faire disparaître.
    pub fn show(&mut self, ctx: &egui::Context, marge_bas: f32) {
        let maintenant = ctx.input(|i| i.time);
        self.purger(maintenant);
        if self.pile.is_empty() {
            return;
        }
        let mut ferme = None;
        egui::Area::new(egui::Id::unique("ki-ui-toasts"))
            .order(Order::Foreground)
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-espace::L, -marge_bas))
            .show(ctx, |ui| {
                ui.set_width(LARGEUR.min(ctx.content_rect().width() - 2.0 * espace::L));
                for toast in &self.pile {
                    let couleur = toast.ton.couleur();
                    // Le fondu de fin : il s'efface, il ne disparaît pas d'un coup.
                    let opacite = (toast.reste(maintenant) / FONDU).clamp(0.0, 1.0) as f32;
                    let cadre = ui
                        .scope(|ui| {
                            ui.set_opacity(opacite);
                            egui::Frame::NONE
                                .fill(theme::BG_RAISED)
                                .stroke(Stroke::new(1.0_f32, theme::translucide(couleur, 110)))
                                .corner_radius(CornerRadius::same(rayon::L))
                                .shadow(ombre::MENU)
                                .inner_margin(marge::symetrique(espace::L, espace::M))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal_top(|ui| {
                                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                                        icons::draw(ui.painter(), rect, toast.ton.icone(), couleur);
                                        ui.add(
                                            egui::Label::new(
                                                Texte::Corps.riche(ui, &toast.texte),
                                            )
                                            .wrap(),
                                        );
                                    });
                                })
                                .response
                        })
                        .inner;
                    // Un clic n'importe où dessus le ferme.
                    if cadre.interact(Sense::click()).on_hover_text("cliquer pour fermer").clicked() {
                        ferme = Some(toast.id);
                    }
                    ui.add_space(espace::S);
                }
            });
        if let Some(id) = ferme {
            self.pile.retain(|t| t.id != id);
        }
        // L'image suivante : tout de suite pendant un fondu, sinon à la
        // prochaine échéance.
        let prochaine = self.pile.iter().map(|t| t.reste(maintenant)).fold(f64::INFINITY, f64::min);
        if prochaine.is_finite() {
            let attente = if prochaine <= FONDU { 0.0 } else { prochaine - FONDU };
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(attente));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_toast_reste_le_temps_de_se_lire_puis_s_en_va() {
        let mut t = Toasts::default();
        t.ajouter(Ton::Info, "connexion rétablie", 100.0);
        // 4 s + 18 caractères × 0,05 = 4,9 s.
        t.purger(104.5);
        assert_eq!(t.textes(), ["connexion rétablie"]);
        t.purger(105.0);
        assert!(t.est_vide());
    }

    #[test]
    fn une_erreur_reste_plus_longtemps_et_rien_ne_reste_indefiniment() {
        let mut t = Toasts::default();
        t.ajouter(Ton::Info, "ok", 0.0);
        t.ajouter(Ton::Danger, "ko", 0.0);
        t.purger(5.0);
        assert_eq!(t.textes(), ["ko"]);
        t.purger(8.2);
        assert!(t.est_vide());
        // Un roman ne reste pas une minute.
        t.ajouter(Ton::Danger, "x".repeat(2000), 0.0);
        t.purger(DUREE_MAX - 0.1);
        assert!(!t.est_vide());
        t.purger(DUREE_MAX + 0.1);
        assert!(t.est_vide());
    }

    #[test]
    fn le_meme_message_redit_repart_pour_un_tour_sans_s_empiler() {
        let mut t = Toasts::default();
        t.ajouter(Ton::Info, "journal copié", 0.0);
        t.ajouter(Ton::Info, "autre chose", 1.0);
        t.ajouter(Ton::Info, "journal copié", 3.0);
        // Un seul exemplaire, redescendu en bas de la pile.
        assert_eq!(t.textes(), ["autre chose", "journal copié"]);
        // Il vit à partir de sa dernière annonce (3 s + 4,65 s).
        t.purger(7.0);
        assert_eq!(t.textes(), ["journal copié"]);
        // Le même texte sur un autre ton est un autre message.
        t.ajouter(Ton::Danger, "journal copié", 7.0);
        assert_eq!(t.textes().len(), 2);
    }

    #[test]
    fn la_pile_est_bornee_et_un_message_vide_ne_s_affiche_pas() {
        let mut t = Toasts::default();
        for i in 0..7 {
            t.ajouter(Ton::Info, format!("message {i}"), 0.0);
        }
        assert_eq!(t.textes(), ["message 3", "message 4", "message 5", "message 6"]);
        let mut vide = Toasts::default();
        vide.ajouter(Ton::Info, "   ", 0.0);
        assert!(vide.est_vide());
    }
}
