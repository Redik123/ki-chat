//! Briques d'interface réutilisables : boutons à icône, avatars, vumètres,
//! bandeaux d'information. Tout ce qui est dessiné à la main plutôt que
//! délégué aux widgets standard d'egui vit ici.

use egui::{
    self, Align2, Color32, CornerRadius, FontId, Painter, Rect, Response, RichText, Sense, Stroke,
    StrokeKind, Ui, Vec2,
};

use crate::flex::{Aligne, Case, Flex};
use crate::icones::{self as icons, Icon};
use crate::jetons::couleur as theme;
use crate::jetons::{espace, marge, rayon, texte};

/// Ton d'un bandeau ou d'un bouton accentué.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Accent,
    Danger,
    Warn,
    Info,
}

impl Tone {
    fn color(self) -> Color32 {
        match self {
            Tone::Accent => theme::ACCENT,
            Tone::Danger => theme::DANGER,
            Tone::Warn => theme::WARN,
            Tone::Info => theme::INFO,
        }
    }

    fn icon(self) -> Icon {
        match self {
            Tone::Accent => Icon::Check,
            Tone::Danger => Icon::Warning,
            Tone::Warn => Icon::Warning,
            Tone::Info => Icon::Info,
        }
    }
}

// ---------------------------------------------------------------------
// Boutons
// ---------------------------------------------------------------------

/// Bouton carré ne contenant qu'une icône. Sans fond au repos, il ne
/// s'allume qu'au survol — idéal pour les barres d'outils denses.
pub fn icon_button(ui: &mut Ui, icon: Icon, tooltip: &str) -> Response {
    icon_button_ex(ui, icon, 32.0, tooltip, None)
}

/// Variante complète : taille du bouton et couleur imposée de l'icône
/// (pour un état actif, par exemple micro coupé en rouge).
pub fn icon_button_ex(
    ui: &mut Ui,
    icon: Icon,
    size: f32,
    tooltip: &str,
    tint: Option<Color32>,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    if ui.is_rect_visible(rect) {
        let pressed = response.is_pointer_button_down_on();
        let bg = if pressed {
            theme::BG_ACTIVE
        } else if response.hovered() {
            theme::BG_HOVER
        } else if let Some(c) = tint {
            theme::translucide(c, 28)
        } else {
            Color32::TRANSPARENT
        };
        if bg != Color32::TRANSPARENT {
            ui.painter().rect_filled(rect, CornerRadius::same(rayon::L), bg);
        }
        let fg = tint.unwrap_or(if response.hovered() {
            theme::TEXT
        } else {
            theme::TEXT_DIM
        });
        icons::draw(ui.painter(), rect.shrink(size * 0.24), icon, fg);
    }
    if tooltip.is_empty() {
        response
    } else {
        response.on_hover_text(tooltip)
    }
}

/// Bouton « icône + libellé », posé sur une surface en relief.
pub fn button(ui: &mut Ui, icon: Icon, label: &str) -> Response {
    styled_button(ui, Some(icon), label, Fill::Surface, None)
}

/// Bouton principal, fond plein accent. `width` à `None` = largeur du contenu.
pub fn primary_button(
    ui: &mut Ui,
    icon: Option<Icon>,
    label: &str,
    width: Option<f32>,
) -> Response {
    styled_button(ui, icon, label, Fill::Solid(theme::ACCENT), width)
}

/// Bouton teinté (danger, info…) : fond très léger, texte coloré.
pub fn tinted_button(ui: &mut Ui, icon: Option<Icon>, label: &str, tone: Tone) -> Response {
    styled_button(ui, icon, label, Fill::Tinted(tone.color()), None)
}

enum Fill {
    Surface,
    Solid(Color32),
    Tinted(Color32),
}

fn styled_button(
    ui: &mut Ui,
    icon: Option<Icon>,
    label: &str,
    fill: Fill,
    width: Option<f32>,
) -> Response {
    let font = FontId::proportional(texte::CORPS);
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), font, theme::TEXT));
    let icon_size = 16.0;
    let gap = if icon.is_some() && !label.is_empty() {
        7.0
    } else {
        0.0
    };
    let icon_w = if icon.is_some() { icon_size } else { 0.0 };
    let pad = Vec2::new(13.0, 8.0);
    let desired = Vec2::new(
        width.unwrap_or(icon_w + gap + galley.size().x + pad.x * 2.0),
        (galley.size().y).max(icon_size) + pad.y * 2.0,
    );

    let (rect, response) = ui.allocate_exact_size(desired, Sense::click());
    if ui.is_rect_visible(rect) {
        let enabled = ui.is_enabled();
        let hovered = response.hovered() && enabled;
        let pressed = response.is_pointer_button_down_on() && enabled;

        let (bg, border, fg) = match fill {
            // Bouton principal : fond plein qui s'éclaircit au survol.
            Fill::Solid(c) => {
                let base = if pressed {
                    theme::melanger(c, Color32::BLACK, 0.18)
                } else if hovered {
                    theme::melanger(c, Color32::WHITE, 0.12)
                } else {
                    c
                };
                (base, Stroke::NONE, theme::BG_DEEP)
            }
            Fill::Tinted(c) => {
                let bg = theme::translucide(
                    c,
                    if pressed {
                        66
                    } else if hovered {
                        44
                    } else {
                        24
                    },
                );
                (bg, Stroke::new(1.0_f32, theme::translucide(c, 90)), c)
            }
            Fill::Surface => {
                let bg = if pressed {
                    theme::BG_ACTIVE
                } else if hovered {
                    theme::BG_HOVER
                } else {
                    theme::BG_RAISED
                };
                let edge = if hovered {
                    theme::melanger(theme::BORDER, theme::ACCENT, 0.3)
                } else {
                    theme::BORDER
                };
                (
                    bg,
                    Stroke::new(1.0_f32, edge),
                    if hovered {
                        theme::TEXT
                    } else {
                        theme::TEXT_DIM
                    },
                )
            }
        };

        let alpha = if enabled { 1.0 } else { 0.45 };
        let painter = ui.painter();
        painter.rect(
            rect,
            CornerRadius::same(rayon::L),
            bg.gamma_multiply(alpha),
            Stroke::new(border.width, border.color.gamma_multiply(alpha)),
            StrokeKind::Inside,
        );

        let content_w = icon_w + gap + galley.size().x;
        let mut x = rect.center().x - content_w / 2.0;
        if let Some(icon) = icon {
            let icon_rect = Rect::from_min_size(
                egui::pos2(x, rect.center().y - icon_size / 2.0),
                Vec2::splat(icon_size),
            );
            icons::draw(painter, icon_rect, icon, fg.gamma_multiply(alpha));
            x += icon_w + gap;
        }
        painter.galley(
            egui::pos2(x, rect.center().y - galley.size().y / 2.0),
            galley,
            fg.gamma_multiply(alpha),
        );
    }
    response
}

// ---------------------------------------------------------------------
// Avatars
// ---------------------------------------------------------------------

/// Pastille ronde avec l'initiale du pseudo, dans sa couleur attitrée.
/// `speaking` ajoute l'anneau vert d'émission.
pub fn paint_avatar(
    p: &Painter,
    rect: Rect,
    name: &str,
    speaking: bool,
    photo: Option<&egui::TextureHandle>,
    backdrop: Color32,
) {
    let color = theme::pour_pseudo(name);
    let center = rect.center();
    let radius = rect.width().min(rect.height()) / 2.0;
    let inner = if speaking { radius - 3.0 } else { radius };

    if let Some(photo) = photo {
        // La vignette est carrée : on la détoure en rond avec un anneau de
        // la couleur du fond, pour qu'elle s'inscrive dans la même pastille
        // que les monogrammes.
        let square = Rect::from_center_size(center, Vec2::splat(inner * 2.0));
        let full = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        p.image(photo.id(), square, full, Color32::WHITE);
        round_off(&p.with_clip_rect(square), center, inner, backdrop);
        p.circle_stroke(
            center,
            inner - 0.5,
            Stroke::new(1.0_f32, theme::translucide(color, 90)),
        );
    } else {
        p.circle_filled(center, inner, theme::melanger(theme::BG_RAISED, color, 0.22));
        p.circle_stroke(
            center,
            inner - 0.5,
            Stroke::new(1.0_f32, theme::translucide(color, 110)),
        );

        let initial = name
            .chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".into());
        p.text(
            center,
            Align2::CENTER_CENTER,
            initial,
            FontId::proportional(inner * 0.95),
            color,
        );
    }

    // L'anneau vient en dernier : peint avant la photo, le détourage de
    // celle-ci le recouvrait, et seuls les monogrammes s'allumaient.
    if speaking {
        p.circle_stroke(center, radius - 1.0, Stroke::new(2.0_f32, theme::SPEAK));
    }
}

/// Rogne les coins d'une image carrée pour n'en garder qu'un disque : un
/// anneau plein de la couleur du fond, peint par-dessus. Le peintre doit être
/// limité au carré de l'image, sinon l'anneau mord sur les voisins.
fn round_off(p: &Painter, center: egui::Pos2, radius: f32, backdrop: Color32) {
    const STEPS: usize = 64;
    // Le carré inscrit déborde du disque de r·√2 : 1,45 couvre les coins.
    let outer = radius * 1.45;
    let mut mesh = egui::Mesh::default();
    for i in 0..=STEPS {
        let angle = i as f32 / STEPS as f32 * std::f32::consts::TAU;
        let dir = Vec2::new(angle.cos(), angle.sin());
        mesh.colored_vertex(center + dir * radius, backdrop);
        mesh.colored_vertex(center + dir * outer, backdrop);
    }
    for i in 0..STEPS as u32 {
        let (inner_v, outer_v) = (2 * i, 2 * i + 1);
        mesh.add_triangle(inner_v, outer_v, inner_v + 2);
        mesh.add_triangle(outer_v, inner_v + 2, outer_v + 2);
    }
    p.add(egui::Shape::mesh(mesh));
}

/// Logo d'un serveur : sa vignette si elle existe, sinon un monogramme
/// carré-arrondi dont la couleur découle du nom — deux serveurs différents
/// ne se ressemblent jamais.
pub fn paint_server_badge(
    p: &Painter,
    rect: Rect,
    name: &str,
    address: &str,
    texture: Option<&egui::TextureHandle>,
) {
    let side = rect.width().min(rect.height());
    let square = Rect::from_center_size(rect.center(), Vec2::splat(side));

    if let Some(texture) = texture {
        // Les coins sont déjà détourés dans la vignette elle-même.
        let full = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        p.image(texture.id(), square, full, Color32::WHITE);
        return;
    }

    let seed = if name.trim().is_empty() { address } else { name };
    let color = theme::d_insigne(seed);
    let radius = CornerRadius::same((side * 0.28).round().clamp(0.0, 255.0) as u8);
    p.rect(
        square,
        radius,
        theme::melanger(theme::BG_RAISED, color, 0.20),
        Stroke::new(1.0_f32, theme::translucide(color, 110)),
        StrokeKind::Inside,
    );
    let initial = seed
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into());
    p.text(
        square.center(),
        Align2::CENTER_CENTER,
        initial,
        FontId::proportional(side * 0.46),
        color,
    );
}

/// Halo radial doux, dessiné en maillage : sert de fond au logo sur l'écran
/// de connexion. epaint interpole les couleurs des sommets, il suffit donc
/// d'un éventail dont le pourtour est transparent.
pub fn glow(p: &Painter, center: egui::Pos2, radius: f32, color: Color32) {
    const STEPS: usize = 56;
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(center, color);
    for i in 0..=STEPS {
        let angle = i as f32 / STEPS as f32 * std::f32::consts::TAU;
        let edge = center + Vec2::new(angle.cos(), angle.sin()) * radius;
        mesh.colored_vertex(edge, Color32::TRANSPARENT);
    }
    for i in 0..STEPS {
        mesh.add_triangle(0, 1 + i as u32, 2 + i as u32);
    }
    p.add(egui::Shape::mesh(mesh));
}

/// Arc en rotation, pour une mesure en cours.
pub fn spinner(p: &Painter, center: egui::Pos2, radius: f32, time: f64, color: Color32) {
    const STEPS: usize = 14;
    let start = (time * 2.2) as f32 % std::f32::consts::TAU;
    let points: Vec<egui::Pos2> = (0..=STEPS)
        .map(|i| {
            let angle = start + i as f32 / STEPS as f32 * 4.2;
            center + Vec2::new(angle.cos(), angle.sin()) * radius
        })
        .collect();
    p.add(egui::Shape::line(points, Stroke::new(1.6_f32, color)));
}

/// Réserve la place d'un avatar et le dessine.
pub fn avatar(
    ui: &mut Ui,
    name: &str,
    size: f32,
    speaking: bool,
    photo: Option<&egui::TextureHandle>,
    backdrop: Color32,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    if ui.is_rect_visible(rect) {
        paint_avatar(ui.painter(), rect, name, speaking, photo, backdrop);
    }
    response
}

// ---------------------------------------------------------------------
// Vumètre
// ---------------------------------------------------------------------

/// Vumètre horizontal : rail creusé + barre arrondie.
pub fn meter(ui: &mut Ui, level: f32, size: Vec2, color: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        paint_meter(ui.painter(), rect, level, color);
    }
    response
}

pub fn paint_meter(p: &Painter, rect: Rect, level: f32, color: Color32) {
    let radius = CornerRadius::same((rect.height() / 2.0).round().clamp(0.0, 255.0) as u8);
    p.rect_filled(rect, radius, theme::BG_DEEP);
    let level = level.clamp(0.0, 1.0);
    if level > 0.001 {
        let w = (rect.width() * level).max(rect.height());
        let filled = Rect::from_min_size(rect.min, Vec2::new(w, rect.height()));
        p.rect_filled(filled, radius, color);
    }
}

/// Vumètre avec repère de seuil (calibration, activation vocale).
pub fn meter_with_threshold(
    ui: &mut Ui,
    level: f32,
    threshold: Option<f32>,
    size: Vec2,
    color: Color32,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        paint_meter(ui.painter(), rect, level, color);
        if let Some(t) = threshold {
            let x = rect.left() + rect.width() * t.clamp(0.0, 1.0);
            ui.painter().line_segment(
                [
                    egui::pos2(x, rect.top() - 2.0),
                    egui::pos2(x, rect.bottom() + 2.0),
                ],
                Stroke::new(1.5_f32, theme::WARN),
            );
        }
    }
    response
}

// ---------------------------------------------------------------------
// Mise en page
// ---------------------------------------------------------------------

/// Intitulé de section : capitales discrètes.
pub fn section_label(ui: &mut Ui, text: &str) {
    ui.add_space(espace::XS);
    ui.label(
        RichText::new(text.to_uppercase())
            .color(theme::TEXT_FAINT)
            .size(texte::MINUSCULE)
            .strong(),
    );
    ui.add_space(espace::XXS);
}

/// Titre de bloc dans une fenêtre : filet accentué + libellé.
pub fn group_title(ui: &mut Ui, icon: Icon, text: &str) {
    ui.add_space(espace::XXS);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(15.0), Sense::hover());
        icons::draw(ui.painter(), rect, icon, theme::ACCENT);
        ui.label(RichText::new(text).color(theme::TEXT).size(texte::TITRE).strong());
    });
    ui.add_space(espace::S);
}

/// Filet de séparation d'un pixel, plus discret que `ui.separator()`.
pub fn hairline(ui: &mut Ui) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 1.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, theme::BORDER_SOFT);
}

/// Petite étiquette au-dessus d'un champ de saisie.
pub fn field_label(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).color(theme::TEXT_DIM).size(texte::COURANT));
    ui.add_space(espace::XS);
}

/// Explication en petit sous un réglage.
pub fn hint(ui: &mut Ui, text: impl Into<String>) {
    ui.label(RichText::new(text).color(theme::TEXT_FAINT).size(texte::PETIT));
}

/// Un encart teinté de `couleur` : fond translucide, filet de la même
/// couleur. Le fond des bandeaux, des demandes, de ce qui doit se voir
/// sans crier.
pub fn encart<R>(ui: &mut Ui, couleur: Color32, contenu: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<R> {
    egui::Frame::NONE
        .fill(theme::translucide(couleur, 26))
        .stroke(Stroke::new(1.0_f32, theme::translucide(couleur, 70)))
        .corner_radius(CornerRadius::same(rayon::L))
        .inner_margin(marge::symetrique(espace::L, espace::M))
        .show(ui, contenu)
}

/// Bandeau d'information coloré. Renvoie `true` si l'utilisateur l'a fermé
/// (la croix n'apparaît que si `closable`).
pub fn banner(ui: &mut Ui, tone: Tone, text: &str, closable: bool) -> bool {
    let color = tone.color();
    let mut closed = false;
    encart(ui, color, |ui| {
        // Le texte passe à la ligne dans la largeur qui reste : posé tel
        // quel dans la rangée, un bandeau long élargissait toute la page
        // au-delà de l'écran.
        Flex::ligne().ecart(espace::S).show(ui, "bandeau", |f| {
            f.ui(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                icons::draw(ui.painter(), rect, tone.icon(), color);
            });
            f.grandit(|ui| {
                ui.add(egui::Label::new(RichText::new(text).color(color).size(texte::CORPS)).wrap());
            });
            if closable {
                f.ui(|ui| {
                    closed = icon_button_ex(ui, Icon::Close, 22.0, "Masquer", None).clicked();
                });
            }
        });
    });
    closed
}

/// Un rappel au-dessus d'un champ — « en réponse à… », « tu modifies ton
/// message » : une ligne discrète, tronquée s'il le faut, et une croix
/// dont `bulle` dit ce qu'elle fait. Rend vrai si la croix a été cliquée.
pub fn rappel(ui: &mut Ui, texte_rappel: &str, bulle: &str) -> bool {
    let mut ferme = false;
    egui::Frame::NONE
        .fill(theme::BG_RAISED)
        .corner_radius(CornerRadius::same(rayon::L))
        .inner_margin(marge::symetrique(espace::L, espace::XS))
        .show(ui, |ui| {
            Flex::ligne().ecart(espace::S).show(ui, "rappel", |f| {
                f.grandit(|ui| {
                    crate::emoji::label_tronque(
                        ui,
                        RichText::new(texte_rappel).color(theme::TEXT_DIM).size(texte::COURANT),
                    );
                });
                f.ui(|ui| {
                    ferme = icon_button_ex(ui, Icon::Close, 20.0, bulle, None).clicked();
                });
            });
        });
    ferme
}

// ---------------------------------------------------------------------
// Réglages : sections, lignes, choix segmentés, interrupteurs
// ---------------------------------------------------------------------

/// Largeur de la colonne des libellés d'une ligne de réglage.
const LIBELLE_W: f32 = 150.0;
/// La place minimale du contrôle à côté de son libellé : en dessous, il
/// passe sous le libellé — à 430 points de ligne, comme avant.
const CONTROLE_MIN: f32 = 430.0 - LIBELLE_W - espace::M;

/// Une section de réglages : une surface à peine relevée, son titre, une
/// phrase qui dit à quoi elle sert, puis ses lignes.
pub fn section(
    ui: &mut Ui,
    icon: Icon,
    titre: &str,
    sous_titre: Option<&str>,
    add: impl FnOnce(&mut Ui),
) {
    egui::Frame::NONE
        .fill(theme::BG_RAISED)
        .stroke(Stroke::new(1.0_f32, theme::BORDER_SOFT))
        .corner_radius(CornerRadius::same(rayon::XL))
        .inner_margin(marge::symetrique(espace::XL, espace::XL))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                icons::draw(ui.painter(), rect, icon, theme::ACCENT);
                ui.add_space(espace::XXS);
                ui.label(RichText::new(titre).color(theme::TEXT).size(texte::TITRE).strong());
            });
            if let Some(s) = sous_titre {
                ui.add_space(espace::XXS);
                ui.label(RichText::new(s).color(theme::TEXT_FAINT).size(texte::PETIT));
            }
            ui.add_space(espace::L);
            add(ui);
        });
    ui.add_space(espace::L);
}

/// Une ligne de réglage : le libellé sur sa colonne, le contrôle qui prend
/// le reste — et qui passe sous le libellé quand ils ne tiennent plus côte
/// à côte. Ce que `add` ajoute s'empile dans la colonne du contrôle (une
/// explication sous un curseur, par exemple).
pub fn ligne(ui: &mut Ui, libelle: &str, add: impl FnOnce(&mut Ui)) {
    let libelle_texte = RichText::new(libelle).color(theme::TEXT_DIM).size(texte::COURANT);
    // Le passage à la ligne, c'est le flex qui en décide (le contrôle a sa
    // largeur minimale), plus un seuil de largeur de fenêtre.
    Flex::ligne()
        .aligner(Aligne::Debut)
        .ecarts(espace::M, espace::XS)
        .passer_a_la_ligne()
        .show(ui, ("ligne", libelle), |f| {
            f.case(Case::new().largeur(LIBELLE_W).rigide(), |ui| {
                // Centré sur la hauteur d'un contrôle : en face de sa
                // première ligne.
                ui.allocate_ui_with_layout(
                    Vec2::new(LIBELLE_W, 24.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_min_width(LIBELLE_W);
                        ui.label(libelle_texte);
                    },
                );
            });
            f.case(Case::new().grandir(1.0).largeur_min(CONTROLE_MIN), |ui| {
                ui.vertical(|ui| add(ui));
            });
        });
    ui.add_space(espace::L);
}

/// Explication sous un contrôle, dans la colonne de la ligne.
pub fn precision(ui: &mut Ui, text: &str) {
    ui.add_space(espace::XXS);
    ui.add(egui::Label::new(RichText::new(text).color(theme::TEXT_FAINT).size(texte::PETIT)).wrap());
}

/// Choix exclusif en pastilles jointes (« Aucune · Douce · Forte ») :
/// toutes les options se voient d'un coup, sans liste à dérouler. Rend vrai
/// quand la valeur a changé.
pub fn segmente<T: PartialEq + Copy>(ui: &mut Ui, valeur: &mut T, choix: &[(T, &str)]) -> bool {
    let font = FontId::proportional(texte::COURANT);
    let textes: Vec<_> = choix
        .iter()
        .map(|(_, l)| ui.fonts_mut(|f| f.layout_no_wrap((*l).to_owned(), font.clone(), theme::TEXT)))
        .collect();
    let pad = 12.0;
    let tailles: Vec<f32> = textes.iter().map(|g| g.size().x + 2.0 * pad).collect();
    let n = choix.len().max(1) as f32;
    let egales = tailles.iter().fold(0.0, |m: f32, t| m.max(*t)) * n;
    let dispo = ui.available_width();
    // Des parts égales quand elles tiennent — étirées jusqu'à la colonne,
    // dans une limite raisonnable —, chacune à sa taille sinon.
    let largeurs: Vec<f32> = if egales + 6.0 <= dispo {
        let totale = dispo.min(380.0).max(egales + 6.0);
        vec![(totale - 6.0) / n; choix.len()]
    } else {
        tailles
    };
    let largeur = largeurs.iter().sum::<f32>() + 6.0;
    // Les segments tirent leur identité de celle de la piste, unique à chaque
    // contrôle : dérivée de l'Ui parent, elle était la même pour deux choix
    // segmentés d'une même section, et egui signalait le conflit en rouge.
    let (rect, piste) = ui.allocate_exact_size(Vec2::new(largeur, 30.0), Sense::hover());
    let mut change = false;
    if !ui.is_rect_visible(rect) {
        return false;
    }
    ui.painter().rect_filled(rect, CornerRadius::same(rayon::L), theme::BG_DEEP);
    let mut x = rect.left() + 3.0;
    for (i, ((v, _), galley)) in choix.iter().zip(textes).enumerate() {
        let seg = Rect::from_min_size(
            egui::pos2(x, rect.top() + 3.0),
            Vec2::new(largeurs[i], rect.height() - 6.0),
        );
        x += largeurs[i];
        let reponse = ui.interact(seg, piste.id.with(i), Sense::click());
        let actif = *valeur == *v;
        if reponse.clicked() && !actif {
            *valeur = *v;
            change = true;
        }
        let actif = *valeur == *v;
        let painter = ui.painter();
        if actif {
            painter.rect(
                seg,
                CornerRadius::same(rayon::L),
                theme::BG_ACTIVE,
                Stroke::new(1.0_f32, theme::translucide(theme::ACCENT, 90)),
                StrokeKind::Inside,
            );
        } else if reponse.hovered() {
            painter.rect_filled(seg, CornerRadius::same(rayon::L), theme::BG_HOVER);
        }
        let couleur = if actif { theme::TEXT } else { theme::TEXT_DIM };
        painter.galley_with_override_text_color(seg.center() - galley.size() / 2.0, galley, couleur);
    }
    change
}

/// Choix exclusif en pastilles séparées qui passent à la ligne : pour une
/// liste trop longue pour une seule rangée — les personnages du changeur,
/// qui débordaient de la page. Rend vrai quand la valeur a changé.
pub fn pastilles<T: PartialEq + Copy>(ui: &mut Ui, valeur: &mut T, choix: &[(T, &str)]) -> bool {
    let font = FontId::proportional(texte::COURANT);
    let mut change = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::splat(espace::S);
        for (v, libelle) in choix {
            let galley = ui.fonts_mut(|f| f.layout_no_wrap((*libelle).to_owned(), font.clone(), theme::TEXT));
            let (rect, reponse) =
                ui.allocate_exact_size(Vec2::new(galley.size().x + 24.0, 28.0), Sense::click());
            if reponse.clicked() && *valeur != *v {
                *valeur = *v;
                change = true;
            }
            let actif = *valeur == *v;
            let painter = ui.painter();
            if actif {
                painter.rect(
                    rect,
                    CornerRadius::same(rayon::L),
                    theme::BG_ACTIVE,
                    Stroke::new(1.0_f32, theme::translucide(theme::ACCENT, 90)),
                    StrokeKind::Inside,
                );
            } else {
                let fond = if reponse.hovered() { theme::BG_HOVER } else { theme::BG_DEEP };
                painter.rect_filled(rect, CornerRadius::same(rayon::L), fond);
            }
            let couleur = if actif { theme::TEXT } else { theme::TEXT_DIM };
            painter.galley_with_override_text_color(rect.center() - galley.size() / 2.0, galley, couleur);
        }
    });
    change
}

/// Une barre d'onglets : les libellés côte à côte, celui qui est ouvert
/// souligné de l'accent ; elle passe à la ligne si la place manque. Rend
/// vrai quand l'onglet a changé.
pub fn onglets<T: PartialEq + Copy>(ui: &mut Ui, valeur: &mut T, choix: &[(T, &str)]) -> bool {
    let mut change = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = espace::XS;
        for (v, libelle) in choix {
            if onglet(ui, *valeur == *v, libelle).clicked() && *valeur != *v {
                *valeur = *v;
                change = true;
            }
        }
    });
    change
}

/// Un onglet seul, pour une barre qu'on monte soi-même — avec une bulle,
/// un bouton « + » au bout, ou un effet à chaque clic. Rend sa réponse.
pub fn onglet(ui: &mut Ui, ouvert: bool, libelle: &str) -> Response {
    let galley =
        ui.fonts_mut(|f| f.layout_no_wrap(libelle.to_owned(), FontId::proportional(texte::CORPS), theme::TEXT));
    let taille = Vec2::new(galley.size().x + 2.0 * espace::M, 30.0);
    let (rect, reponse) = ui.allocate_exact_size(taille, Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if reponse.hovered() && !ouvert {
            painter.rect_filled(rect, CornerRadius::same(rayon::M), theme::BG_HOVER);
        }
        let couleur = if ouvert || reponse.hovered() { theme::TEXT } else { theme::TEXT_DIM };
        painter.galley_with_override_text_color(rect.center() - galley.size() / 2.0, galley, couleur);
        if ouvert {
            let souligne = Rect::from_min_max(
                egui::pos2(rect.left() + espace::S, rect.bottom() - 2.0),
                egui::pos2(rect.right() - espace::S, rect.bottom()),
            );
            painter.rect_filled(souligne, CornerRadius::same(1), theme::ACCENT);
        }
    }
    reponse.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, ouvert, libelle)
    });
    reponse
}

/// Interrupteur : plus lisible qu'une case à cocher pour un réglage qui
/// s'allume ou s'éteint. Le libellé suit à droite. Rend la réponse, `changed`
/// quand on l'a basculé.
pub fn interrupteur(ui: &mut Ui, on: &mut bool, libelle: &str) -> Response {
    let taille = Vec2::new(36.0, 20.0);
    let reponse = ui
        .horizontal(|ui| {
            let (rect, mut r) = ui.allocate_exact_size(taille, Sense::click());
            let texte = ui.add(
                egui::Label::new(RichText::new(libelle).color(theme::TEXT).size(texte::CORPS))
                    .sense(Sense::click()),
            );
            if r.clicked() || texte.clicked() {
                *on = !*on;
                r.mark_changed();
            }
            if ui.is_rect_visible(rect) {
                let t = ui.ctx().animate_bool_responsive(r.id, *on);
                let rayon = rect.height() / 2.0;
                let fond = theme::melanger(theme::BG_DEEP, theme::ACCENT, t);
                let bord = if r.hovered() || texte.hovered() {
                    theme::BORDER_STRONG
                } else {
                    theme::BORDER
                };
                ui.painter().rect(
                    rect,
                    CornerRadius::same(rayon as u8),
                    fond,
                    Stroke::new(1.0_f32, theme::melanger(bord, theme::ACCENT, t)),
                    StrokeKind::Inside,
                );
                let x = egui::lerp((rect.left() + rayon)..=(rect.right() - rayon), t);
                let bouton = if *on { theme::BG_DEEP } else { theme::TEXT_DIM };
                ui.painter().circle_filled(egui::pos2(x, rect.center().y), rayon - 4.0, bouton);
            }
            r
        })
        .inner;
    reponse.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, *on, libelle)
    });
    reponse
}

/// Carte : surface en relief pour regrouper des contrôles.
pub fn card(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let response = egui::Frame::NONE
        .fill(theme::BG_RAISED)
        .stroke(Stroke::new(1.0_f32, theme::BORDER))
        .corner_radius(CornerRadius::same(rayon::XL))
        .inner_margin(marge::egale(espace::XL))
        .shadow(crate::jetons::ombre::DOUCE)
        .show(ui, |ui| add(ui))
        .response;

    // Filet clair sur l'arête haute : la carte accroche la lumière et se
    // détache du fond sans avoir à forcer la bordure.
    let rect = response.rect;
    ui.painter().line_segment(
        [
            egui::pos2(rect.left() + 16.0, rect.top() + 0.5),
            egui::pos2(rect.right() - 16.0, rect.top() + 0.5),
        ],
        Stroke::new(1.0_f32, theme::translucide(Color32::WHITE, 16)),
    );
}

/// La place laissée à droite d'un curseur pour sa valeur.
const PLACE_VALEUR: f32 = 76.0;

/// Les curseurs qui suivent prennent la largeur de la colonne, en laissant
/// à droite la place de leur valeur — pour un `egui::Slider` monté à la
/// main ; [`curseur`] le fait de lui-même.
pub fn curseurs_a_la_largeur(ui: &mut Ui) {
    ui.spacing_mut().slider_width = (ui.available_width() - PLACE_VALEUR).clamp(120.0, 260.0);
}

/// Un curseur à la largeur de la colonne, sa valeur à droite. `pas` :
/// `Some(1.0)` pour des entiers. Rend vrai au changement.
pub fn curseur(
    ui: &mut Ui,
    valeur: &mut f32,
    plage: std::ops::RangeInclusive<f32>,
    suffixe: &str,
    pas: Option<f64>,
) -> bool {
    curseurs_a_la_largeur(ui);
    let mut s = egui::Slider::new(valeur, plage).suffix(suffixe);
    if let Some(p) = pas {
        s = s.step_by(p);
        if p >= 1.0 {
            s = s.fixed_decimals(0);
        }
    }
    ui.add(s).changed()
}

// ---------------------------------------------------------------------
// Indicateurs
// ---------------------------------------------------------------------

/// Une étiquette à côté d'un nom — « BOT », « INVITÉ » : des capitales
/// sombres sur un fond franc.
pub fn etiquette(ui: &mut Ui, mot: &str, fond: Color32) -> Response {
    egui::Frame::new()
        .fill(fond)
        .corner_radius(CornerRadius::same(rayon::S))
        .inner_margin(marge::symetrique(espace::XS, 1.0))
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(RichText::new(mot).size(texte::MINUSCULE).strong().color(theme::BG_DEEP))
                    .selectable(false),
            );
        })
        .response
}

/// La même étiquette, peinte, pour les rangées dessinées au pinceau.
/// `gauche` : le milieu de son bord gauche. Rend la place qu'elle a prise.
pub fn peindre_etiquette(painter: &Painter, gauche: egui::Pos2, mot: &str, fond: Color32) -> Rect {
    let galley = painter.layout_no_wrap(mot.to_owned(), FontId::proportional(texte::MINUSCULE), theme::BG_DEEP);
    let rect = Rect::from_min_size(
        egui::pos2(gauche.x, gauche.y - 7.0),
        Vec2::new(galley.size().x + 2.0 * espace::XS, 14.0),
    );
    painter.rect_filled(rect, CornerRadius::same(rayon::S), fond);
    painter.galley(
        egui::pos2(rect.left() + espace::XS, rect.center().y - galley.size().y / 2.0),
        galley,
        theme::BG_DEEP,
    );
    rect
}

/// Petite icône décorative, sans interaction.
pub fn glyph(ui: &mut Ui, icon: Icon, size: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    if ui.is_rect_visible(rect) {
        icons::draw(ui.painter(), rect, icon, color);
    }
}

/// Pastille d'état : rond de couleur + libellé.
pub fn status_dot(ui: &mut Ui, color: Color32, label: &str, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    if ui.is_rect_visible(rect) {
        icons::dot(ui.painter(), rect.center(), size * 0.32, color);
    }
    if !label.is_empty() {
        ui.label(RichText::new(label).color(color).size(texte::COURANT).strong());
    }
}

/// Suite « icône + valeur », peinte d'un seul bloc : reste lisible dans
/// n'importe quel sens de mise en page (y compris droite-à-gauche).
pub fn stat_row(ui: &mut Ui, items: &[(Icon, String, Color32)], size: f32) -> Response {
    let font = FontId::proportional(size);
    let galleys: Vec<_> = items
        .iter()
        .map(|(_, text, color)| ui.fonts_mut(|f| f.layout_no_wrap(text.clone(), font.clone(), *color)))
        .collect();

    let icon = size * 1.25;
    let (gap, group_gap) = (3.0, 10.0);
    let width: f32 = galleys.iter().map(|g| g.size().x + icon + gap).sum::<f32>()
        + group_gap * items.len().saturating_sub(1) as f32;
    let height = galleys.iter().map(|g| g.size().y).fold(icon, f32::max);

    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let mut x = rect.left();
        for ((symbol, _, color), galley) in items.iter().zip(galleys) {
            let box_ = Rect::from_min_size(
                egui::pos2(x, rect.center().y - icon / 2.0),
                Vec2::splat(icon),
            );
            icons::draw(painter, box_, *symbol, *color);
            x += icon + gap;
            let y = rect.center().y - galley.size().y / 2.0;
            let advance = galley.size().x;
            painter.galley(egui::pos2(x, y), galley, *color);
            x += advance + group_gap;
        }
    }
    response
}

/// Barres de réseau + valeur, pour le ping.
pub fn signal_badge(ui: &mut Ui, lit: u8, text: &str, color: Color32) -> Response {
    let font = FontId::proportional(texte::PETIT);
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), font, color));
    let bars = 14.0;
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(bars + 4.0 + galley.size().x, bars.max(galley.size().y)),
        Sense::hover(),
    );
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let box_ = Rect::from_min_size(
            egui::pos2(rect.left(), rect.center().y - bars / 2.0),
            Vec2::splat(bars),
        );
        icons::signal(painter, box_, lit, color, theme::BORDER);
        let y = rect.center().y - galley.size().y / 2.0;
        painter.galley(egui::pos2(rect.left() + bars + 4.0, y), galley, color);
    }
    response
}

/// Champ de saisie à l'allure maison : fond creusé, coins arrondis.
pub fn text_field<'a>(text: &'a mut String, hint: &str, password: bool) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(text)
        .password(password)
        .hint_text(hint)
        .margin(marge::symetrique(espace::L, espace::M))
        .background_color(theme::BG_DEEP)
        .desired_width(f32::INFINITY)
}

// ---------------------------------------------------------------------
// Tableaux de bord : surfaces, tuiles de chiffres, grilles
// ---------------------------------------------------------------------

/// Une surface posée sur la page : une carte de tableau de bord, sans
/// titre. Plus discrète que [`card`] — ni ombre ni reflet —, pour en
/// aligner beaucoup.
pub fn surface<R>(ui: &mut Ui, contenu: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<R> {
    egui::Frame::NONE
        .fill(theme::BG_RAISED)
        .stroke(Stroke::new(1.0_f32, theme::BORDER_SOFT))
        .corner_radius(CornerRadius::same(rayon::XL))
        .inner_margin(marge::egale(espace::L))
        .show(ui, contenu)
}

/// L'écart d'un chiffre clé à sa référence (les matchs d'avant, la médiane
/// du groupe) : la flèche dit le sens, la couleur dit si c'est mieux.
#[derive(Clone, Debug, PartialEq)]
pub struct Ecart {
    /// Positif : en hausse.
    pub valeur: f32,
    /// Tel qu'on le lit (« +0,12 »).
    pub texte: String,
    /// La hausse est bonne (un K/D) ; fausse : mauvaise (des morts).
    pub hausse_bonne: bool,
}

impl Ecart {
    pub fn new(valeur: f32, texte: impl Into<String>) -> Self {
        Self { valeur, texte: texte.into(), hausse_bonne: true }
    }

    /// La hausse est mauvaise.
    pub fn a_l_envers(mut self) -> Self {
        self.hausse_bonne = false;
        self
    }

    /// Sa couleur : verte quand c'est mieux, rouge quand c'est pire, grise
    /// à plat.
    pub fn couleur(&self) -> Color32 {
        if self.valeur.abs() < f32::EPSILON || !self.valeur.is_finite() {
            theme::TEXT_FAINT
        } else if (self.valeur > 0.0) == self.hausse_bonne {
            theme::ACCENT
        } else {
            theme::DANGER
        }
    }
}

/// Une tuile de chiffre clé : un intitulé en capitales, une grande valeur
/// en Ubuntu Medium, puis l'écart et une précision ; une petite courbe en
/// fond, si on la donne.
///
/// ```ignore
/// ui::Kpi::new("K/D", "1,24").ecart(ui::Ecart::new(0.12, "+0,12")).precision("médiane 1,05").show(ui);
/// ```
pub struct Kpi<'a> {
    intitule: &'a str,
    valeur: String,
    couleur: Color32,
    ecart: Option<Ecart>,
    precision: Option<&'a str>,
    serie: &'a [f32],
    jauge: Option<(f32, Option<f32>, Color32)>,
}

impl<'a> Kpi<'a> {
    pub fn new(intitule: &'a str, valeur: impl Into<String>) -> Self {
        Self { intitule, valeur: valeur.into(), couleur: theme::TEXT, ecart: None, precision: None, serie: &[], jauge: None }
    }

    /// La couleur de la valeur (un rang, un résultat).
    pub fn couleur(mut self, couleur: Color32) -> Self {
        self.couleur = couleur;
        self
    }

    pub fn ecart(mut self, ecart: Ecart) -> Self {
        self.ecart = Some(ecart);
        self
    }

    pub fn precision(mut self, precision: &'a str) -> Self {
        self.precision = Some(precision);
        self
    }

    /// Une courbe discrète au bas de la tuile : la valeur match après
    /// match, la plus ancienne à gauche.
    pub fn serie(mut self, serie: &'a [f32]) -> Self {
        self.serie = serie;
        self
    }

    /// Une jauge au bas de la tuile, à la place de la courbe : la part
    /// `part` (de 0 à 1) en `couleur`, et un trait blanc au `repere` — la
    /// médiane du groupe, par exemple.
    pub fn jauge(mut self, part: f32, repere: Option<f32>, couleur: Color32) -> Self {
        self.jauge = Some((part, repere, couleur));
        self
    }

    /// La tuile, à la largeur qu'on lui laisse, [`Kpi::HAUTEUR`] de haut.
    pub fn show(self, ui: &mut Ui) -> Response {
        let largeur = ui.available_width().max(crate::jetons::largeur::TUILE_MIN * 0.6);
        let (rect, reponse) = ui.allocate_exact_size(Vec2::new(largeur, Self::HAUTEUR), Sense::hover());
        if !ui.is_rect_visible(rect) {
            return reponse;
        }
        let p = ui.painter_at(rect);
        p.rect(rect, CornerRadius::same(rayon::XL), theme::BG_RAISED, Stroke::new(1.0_f32, theme::BORDER_SOFT), StrokeKind::Inside);
        let dedans = rect.shrink(espace::L);
        if let Some((part, repere, couleur)) = self.jauge {
            let fond = Rect::from_min_max(egui::pos2(dedans.left(), dedans.bottom() - 4.0), dedans.right_bottom());
            p.rect_filled(fond, CornerRadius::same(2), theme::BG_ACTIVE);
            let part = if part.is_finite() { part.clamp(0.0, 1.0) } else { 0.0 };
            let plein = Rect::from_min_max(fond.min, egui::pos2(fond.left() + fond.width() * part, fond.bottom()));
            p.rect_filled(plein, CornerRadius::same(2), couleur);
            if let Some(r) = repere.filter(|r| r.is_finite()) {
                let x = fond.left() + fond.width() * r.clamp(0.0, 1.0);
                p.line_segment([egui::pos2(x, fond.top() - 3.0), egui::pos2(x, fond.bottom() + 3.0)], Stroke::new(2.0_f32, theme::TEXT));
            }
        } else if self.serie.len() >= 2 {
            // Sous l'écart et la précision, au ras de la tuile.
            courbe_discrete(&p, Rect::from_min_max(egui::pos2(dedans.left(), dedans.bottom() - 10.0), dedans.right_bottom()), self.serie);
        }
        p.text(
            dedans.left_top(),
            Align2::LEFT_TOP,
            self.intitule.to_uppercase(),
            FontId::proportional(texte::MINUSCULE),
            theme::TEXT_FAINT,
        );
        let valeur = p.text(
            dedans.left_top() + Vec2::new(0.0, 15.0),
            Align2::LEFT_TOP,
            &self.valeur,
            crate::jetons::police::moyenne(ui.ctx(), texte::KPI),
            self.couleur,
        );
        let mut x = dedans.left();
        let y = valeur.bottom() + espace::XS;
        if let Some(ecart) = &self.ecart {
            let couleur = ecart.couleur();
            if ecart.valeur.abs() >= f32::EPSILON && ecart.valeur.is_finite() {
                let c = egui::pos2(x + 4.0, y + 7.0);
                let (h, l) = (4.0, 4.5);
                let pointe = if ecart.valeur > 0.0 {
                    vec![egui::pos2(c.x, c.y - h), egui::pos2(c.x + l, c.y + h), egui::pos2(c.x - l, c.y + h)]
                } else {
                    vec![egui::pos2(c.x, c.y + h), egui::pos2(c.x - l, c.y - h), egui::pos2(c.x + l, c.y - h)]
                };
                p.add(egui::Shape::convex_polygon(pointe, couleur, Stroke::NONE));
                x += 12.0;
            }
            let r = p.text(egui::pos2(x, y), Align2::LEFT_TOP, &ecart.texte, FontId::proportional(texte::PETIT), couleur);
            x = r.right() + espace::S;
        }
        if let Some(precision) = self.precision {
            p.text(egui::pos2(x, y), Align2::LEFT_TOP, precision, FontId::proportional(texte::PETIT), theme::TEXT_FAINT);
        }
        reponse
    }
}

impl Kpi<'_> {
    /// La hauteur d'une tuile : l'intitulé, la valeur, l'écart.
    pub const HAUTEUR: f32 = 92.0;
}

/// Une courbe sans axe, en filigrane : la tendance, pas les valeurs.
fn courbe_discrete(p: &Painter, rect: Rect, serie: &[f32]) {
    let (min, max) = serie.iter().filter(|v| v.is_finite()).fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
    if min > max {
        return;
    }
    let etendue = (max - min).max(f32::EPSILON);
    let n = (serie.len() - 1).max(1) as f32;
    let points: Vec<egui::Pos2> = serie
        .iter()
        .enumerate()
        .filter(|(_, v)| v.is_finite())
        .map(|(i, v)| egui::pos2(rect.left() + rect.width() * i as f32 / n, rect.bottom() - rect.height() * (v - min) / etendue))
        .collect();
    p.add(egui::Shape::line(points, Stroke::new(1.5_f32, theme::translucide(theme::ACCENT, 90))));
}

/// Combien de colonnes de `largeur_min` tiennent dans `largeur`, avec
/// `ecart` entre elles — une au moins, `n` au plus.
pub fn colonnes_de_grille(largeur: f32, largeur_min: f32, ecart: f32, n: usize) -> usize {
    let tiennent = ((largeur + ecart) / (largeur_min + ecart)).floor().max(1.0) as usize;
    tiennent.min(n.max(1))
}

/// Une grille qui passe à la ligne : `n` cases d'égale largeur, autant par
/// rangée qu'il en tient à `largeur_min` au moins, `ecart` entre elles.
/// `case` remplit la case `i`. Les rangées prennent la hauteur de leur plus
/// haute case.
pub fn grille(ui: &mut Ui, n: usize, largeur_min: f32, ecart: f32, mut case: impl FnMut(&mut Ui, usize)) {
    if n == 0 {
        return;
    }
    let largeur = ui.available_width();
    let colonnes = colonnes_de_grille(largeur, largeur_min, ecart, n);
    let cote = ((largeur - ecart * (colonnes as f32 - 1.0)) / colonnes as f32).floor();
    let mut i = 0;
    while i < n {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = ecart;
            for k in i..(i + colonnes).min(n) {
                ui.allocate_ui_with_layout(Vec2::new(cote, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_width(cote);
                    case(ui, k);
                });
            }
        });
        i += colonnes;
        if i < n {
            ui.add_space(ecart);
        }
    }
}
