//! L'overlay « qui parle » par-dessus le jeu.
//!
//! Une fenêtre à part — toujours au-dessus, sans bordure, transparente,
//! transparente aux clics et jamais active : le jeu ne sait pas qu'elle
//! existe, l'anti-triche non plus (rien n'est injecté nulle part). Windows
//! la compose comme la bulle de volume ou une notification : là où le jeu
//! passe par le compositeur (fenêtré sans bordure, ou « plein écran » avec
//! les optimisations plein écran, le défaut), elle se voit ; en vrai plein
//! écran exclusif, rien ne peut passer au-dessus et elle ne coûte rien.
//!
//! Elle ne se montre que quand ki-chat n'a pas le focus (on est ailleurs :
//! dans le jeu) et qu'on est en salon vocal ; elle se repeint quand ça
//! change, à dix fois par seconde pendant qu'on parle, jamais plus.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, RichText, Vec2, ViewportBuilder};

use crate::theme::{TEXT, TEXT_DIM};
use crate::ui;

/// Le coin de l'écran où l'overlay se pose.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Coin {
    HautGauche,
    HautDroite,
    BasGauche,
    BasDroite,
}

impl Coin {
    pub const TOUS: [(Coin, &'static str, &'static str); 4] = [
        (Coin::HautGauche, "haut-gauche", "En haut à gauche"),
        (Coin::HautDroite, "haut-droite", "En haut à droite"),
        (Coin::BasGauche, "bas-gauche", "En bas à gauche"),
        (Coin::BasDroite, "bas-droite", "En bas à droite"),
    ];

    fn id(self) -> &'static str {
        Self::TOUS.iter().find(|(c, _, _)| *c == self).map(|(_, id, _)| *id).unwrap_or("haut-gauche")
    }

    fn depuis(id: &str) -> Self {
        Self::TOUS.iter().find(|(_, i, _)| *i == id).map(|(c, _, _)| *c).unwrap_or(Coin::HautGauche)
    }

    pub fn libelle(self) -> &'static str {
        Self::TOUS.iter().find(|(c, _, _)| *c == self).map(|(_, _, l)| *l).unwrap_or("?")
    }
}

/// Un occupant du salon vocal, tel que l'overlay le montre.
pub struct Ligne {
    pub nom: String,
    pub parle: bool,
    pub photo: Option<egui::TextureHandle>,
}

/// L'overlay : ses réglages (persistés) et sa petite mémoire.
pub struct Overlay {
    pub actif: bool,
    pub coin: Coin,
    /// Montrer aussi ceux qui se taisent ; sinon, seulement qui parle.
    pub toujours: bool,
    /// Dernier signal de parole par pseudo : l'anneau tient un peu après,
    /// sans quoi il clignoterait au rythme des trames.
    recemment: HashMap<String, Instant>,
}

const LARGEUR: f32 = 220.0;
const LIGNE: f32 = 30.0;
const MARGE: f32 = 8.0;
/// L'anneau reste allumé tant de temps après le dernier signal.
const TENUE: Duration = Duration::from_millis(400);

impl Overlay {
    pub fn load(get: impl Fn(&str, &str) -> String) -> Self {
        Self {
            actif: get("overlay", "on") != "off",
            coin: Coin::depuis(&get("overlay_coin", "haut-gauche")),
            toujours: get("overlay_toujours", "on") != "off",
            recemment: HashMap::new(),
        }
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        storage.set_string("overlay", if self.actif { "on" } else { "off" }.into());
        storage.set_string("overlay_coin", self.coin.id().into());
        storage.set_string("overlay_toujours", if self.toujours { "on" } else { "off" }.into());
    }

    /// Montre (ou non) l'overlay pour cette image. `focus_principal` : la
    /// fenêtre de ki-chat a le clavier — alors on la voit, pas besoin de
    /// doublon. Sans ligne (hors vocal), rien.
    pub fn montrer(&mut self, ctx: &egui::Context, lignes: Vec<Ligne>, focus_principal: bool) {
        if !self.actif || focus_principal || lignes.is_empty() {
            return;
        }
        let now = Instant::now();
        for l in &lignes {
            if l.parle {
                self.recemment.insert(l.nom.clone(), now);
            }
        }
        // Les absents ne s'accumulent pas.
        self.recemment.retain(|_, t| now.duration_since(*t) < Duration::from_secs(60));
        let mut affichees: Vec<(String, bool, Option<egui::TextureHandle>)> = lignes
            .into_iter()
            .map(|l| {
                let tenu = self
                    .recemment
                    .get(&l.nom)
                    .is_some_and(|t| now.duration_since(*t) < TENUE);
                (l.nom, l.parle || tenu, l.photo)
            })
            .collect();
        if !self.toujours {
            affichees.retain(|(_, parle, _)| *parle);
        }
        if affichees.is_empty() {
            return;
        }

        let hauteur = MARGE * 2.0 + affichees.len() as f32 * LIGNE;
        let ecran = ctx
            .input(|i| i.viewport().monitor_size)
            .unwrap_or(Vec2::new(1920.0, 1080.0));
        let pos = match self.coin {
            Coin::HautGauche => Pos2::new(16.0, 16.0),
            Coin::HautDroite => Pos2::new(ecran.x - LARGEUR - 16.0, 16.0),
            // 64 px du bas : la barre des tâches, quand elle est là.
            Coin::BasGauche => Pos2::new(16.0, ecran.y - hauteur - 64.0),
            Coin::BasDroite => Pos2::new(ecran.x - LARGEUR - 16.0, ecran.y - hauteur - 64.0),
        };
        let anime = affichees.iter().any(|(_, parle, _)| *parle);
        let builder = ViewportBuilder::default()
            .with_title("ki-chat — qui parle")
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_taskbar(false)
            .with_resizable(false)
            .with_mouse_passthrough(true)
            .with_active(false)
            .with_inner_size([LARGEUR, hauteur])
            .with_position(pos);
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("overlay-qui-parle"),
            builder,
            move |ctx, _classe| {
                egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
                    let rect = ui.max_rect();
                    let painter = ui.painter();
                    // Une pilule sombre et translucide : lisible sur un jeu
                    // clair comme sombre, sans le cacher.
                    painter.rect_filled(
                        rect,
                        CornerRadius::same(10),
                        Color32::from_rgba_unmultiplied(12, 15, 20, 190),
                    );
                    for (i, (nom, parle, photo)) in affichees.iter().enumerate() {
                        let y = rect.top() + MARGE + i as f32 * LIGNE;
                        let avatar = Rect::from_min_size(
                            Pos2::new(rect.left() + MARGE + 2.0, y + 3.0),
                            Vec2::splat(24.0),
                        );
                        ui::paint_avatar(
                            painter,
                            avatar,
                            nom,
                            *parle,
                            photo.as_ref(),
                            Color32::from_rgb(12, 15, 20),
                        );
                        let couleur = if *parle { TEXT } else { TEXT_DIM };
                        painter.text(
                            Pos2::new(avatar.right() + 8.0, y + LIGNE / 2.0),
                            egui::Align2::LEFT_CENTER,
                            nom,
                            egui::FontId::proportional(13.0),
                            couleur,
                        );
                    }
                });
                if anime {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
            },
        );
    }
}

/// Les réglages de l'overlay, dans ⚙. Rend `true` si quelque chose a changé.
pub fn reglages_ui(ui: &mut egui::Ui, o: &mut Overlay) -> bool {
    let mut change = false;
    if ui
        .checkbox(&mut o.actif, "Afficher qui parle par-dessus le jeu")
        .on_hover_text(
            "une petite fenêtre toujours au-dessus, transparente aux clics, qui ne \
             touche pas au jeu — rien n'est injecté, l'anti-triche n'y voit rien. \
             Visible dès que ki-chat n'est pas au premier plan et qu'on est en vocal.",
        )
        .changed()
    {
        change = true;
    }
    if o.actif {
        ui.horizontal(|ui| {
            ui.label(RichText::new("position").color(TEXT_DIM).size(12.5));
            egui::ComboBox::from_id_salt("overlay_coin")
                .width(170.0)
                .selected_text(RichText::new(o.coin.libelle()).color(TEXT))
                .show_ui(ui, |ui| {
                    for (c, _, l) in Coin::TOUS {
                        if ui.selectable_label(o.coin == c, l).clicked() {
                            o.coin = c;
                            change = true;
                        }
                    }
                });
        });
        if ui
            .checkbox(&mut o.toujours, "Montrer aussi ceux qui se taisent")
            .on_hover_text("décoché : seuls ceux qui parlent apparaissent, l'overlay disparaît au silence")
            .changed()
        {
            change = true;
        }
    }
    ui::hint(
        ui,
        "se voit sur un jeu en fenêtré sans bordure ou en plein écran classique ; en vrai \
         plein écran exclusif (optimisations plein écran désactivées à la main), rien ne \
         peut passer au-dessus du jeu",
    );
    change
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_coins_font_l_aller_retour_par_leur_identifiant() {
        for (c, id, _) in Coin::TOUS {
            assert_eq!(Coin::depuis(id), c);
            assert_eq!(c.id(), id);
        }
        assert_eq!(Coin::depuis("n'importe quoi"), Coin::HautGauche);
    }

    #[test]
    fn les_reglages_se_relisent() {
        let mut memoire = std::collections::HashMap::new();
        struct M<'a>(&'a mut std::collections::HashMap<String, String>);
        impl eframe::Storage for M<'_> {
            fn get_string(&self, key: &str) -> Option<String> {
                self.0.get(key).cloned()
            }
            fn set_string(&mut self, key: &str, value: String) {
                self.0.insert(key.to_string(), value);
            }
            fn flush(&mut self) {}
        }
        let mut o = Overlay::load(|_, d| d.to_string());
        assert!(o.actif && o.toujours);
        o.actif = false;
        o.coin = Coin::BasDroite;
        o.toujours = false;
        o.save(&mut M(&mut memoire));
        let relu = Overlay::load(|k, d| memoire.get(k).cloned().unwrap_or_else(|| d.to_string()));
        assert!(!relu.actif && !relu.toujours);
        assert_eq!(relu.coin, Coin::BasDroite);
    }
}
