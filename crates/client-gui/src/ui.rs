//! Briques d'interface réutilisables : boutons à icône, avatars, vumètres,
//! bandeaux d'information. Tout ce qui est dessiné à la main plutôt que
//! délégué aux widgets standard d'egui vit ici.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Painter, Rect, Response, RichText, Sense, Stroke,
    StrokeKind, Ui, Vec2,
};

use crate::icons::{self, Icon};
use crate::theme;

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
            theme::alpha(c, 28)
        } else {
            Color32::TRANSPARENT
        };
        if bg != Color32::TRANSPARENT {
            ui.painter().rect_filled(rect, CornerRadius::same(8), bg);
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
    let font = FontId::proportional(14.0);
    let galley = ui.fonts(|f| f.layout_no_wrap(label.to_owned(), font, theme::TEXT));
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
                    theme::mix(c, Color32::BLACK, 0.18)
                } else if hovered {
                    theme::mix(c, Color32::WHITE, 0.12)
                } else {
                    c
                };
                (base, Stroke::NONE, theme::BG_DEEP)
            }
            Fill::Tinted(c) => {
                let bg = theme::alpha(
                    c,
                    if pressed {
                        66
                    } else if hovered {
                        44
                    } else {
                        24
                    },
                );
                (bg, Stroke::new(1.0_f32, theme::alpha(c, 90)), c)
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
                    theme::mix(theme::BORDER, theme::ACCENT, 0.3)
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
            CornerRadius::same(8),
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
    let color = theme::color_for(name);
    let center = rect.center();
    let radius = rect.width().min(rect.height()) / 2.0;

    if speaking {
        p.circle_stroke(center, radius - 1.0, Stroke::new(2.0_f32, theme::SPEAK));
    }
    let inner = if speaking { radius - 3.0 } else { radius };

    if let Some(photo) = photo {
        // La vignette est carrée : on la détoure en rond avec un anneau de
        // la couleur du fond, pour qu'elle s'inscrive dans la même pastille
        // que les monogrammes.
        let square = Rect::from_center_size(center, Vec2::splat(inner * 2.0));
        let full = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        p.image(photo.id(), square, full, Color32::WHITE);
        round_off(p, center, inner, backdrop);
        p.circle_stroke(
            center,
            inner - 0.5,
            Stroke::new(1.0_f32, theme::alpha(color, 90)),
        );
        return;
    }

    p.circle_filled(center, inner, theme::mix(theme::BG_RAISED, color, 0.22));
    p.circle_stroke(
        center,
        inner - 0.5,
        Stroke::new(1.0_f32, theme::alpha(color, 110)),
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

/// Rogne les coins d'une image carrée pour n'en garder qu'un disque : un
/// anneau plein de la couleur du fond, peint par-dessus.
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
    let color = theme::badge_color(seed);
    let radius = CornerRadius::same((side * 0.28).round().clamp(0.0, 255.0) as u8);
    p.rect(
        square,
        radius,
        theme::mix(theme::BG_RAISED, color, 0.20),
        Stroke::new(1.0_f32, theme::alpha(color, 110)),
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
    ui.add_space(3.0);
    ui.label(
        RichText::new(text.to_uppercase())
            .color(theme::TEXT_FAINT)
            .size(10.5)
            .strong(),
    );
    ui.add_space(2.0);
}

/// Titre de bloc dans une fenêtre : filet accentué + libellé.
pub fn group_title(ui: &mut Ui, icon: Icon, text: &str) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(15.0), Sense::hover());
        icons::draw(ui.painter(), rect, icon, theme::ACCENT);
        ui.label(RichText::new(text).color(theme::TEXT).size(14.5).strong());
    });
    ui.add_space(5.0);
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
    ui.label(RichText::new(text).color(theme::TEXT_DIM).size(12.0));
    ui.add_space(3.0);
}

/// Explication en petit sous un réglage.
pub fn hint(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).color(theme::TEXT_FAINT).size(11.5));
}

/// Bandeau d'information coloré. Renvoie `true` si l'utilisateur l'a fermé
/// (la croix n'apparaît que si `closable`).
pub fn banner(ui: &mut Ui, tone: Tone, text: &str, closable: bool) -> bool {
    let color = tone.color();
    let mut closed = false;
    egui::Frame::NONE
        .fill(theme::alpha(color, 26))
        .stroke(Stroke::new(1.0_f32, theme::alpha(color, 70)))
        .corner_radius(CornerRadius::same(9))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                icons::draw(ui.painter(), rect, tone.icon(), color);
                ui.add_space(2.0);
                if closable {
                    let text_width = (ui.available_width() - 26.0).max(40.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(text_width, 0.0),
                        egui::Layout::top_down(egui::Align::LEFT),
                        |ui| {
                            ui.label(RichText::new(text).color(color).size(13.0));
                        },
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button_ex(ui, Icon::Close, 22.0, "Masquer", None).clicked() {
                            closed = true;
                        }
                    });
                } else {
                    ui.label(RichText::new(text).color(color).size(13.0));
                }
            });
        });
    closed
}

/// Carte : surface en relief pour regrouper des contrôles.
pub fn card(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let response = egui::Frame::NONE
        .fill(theme::BG_RAISED)
        .stroke(Stroke::new(1.0_f32, theme::BORDER))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(egui::Margin::same(16))
        .shadow(egui::epaint::Shadow {
            offset: [0, 10],
            blur: 30,
            spread: 0,
            color: Color32::from_black_alpha(90),
        })
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
        Stroke::new(1.0_f32, theme::alpha(Color32::WHITE, 16)),
    );
}

// ---------------------------------------------------------------------
// Indicateurs
// ---------------------------------------------------------------------

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
        ui.label(RichText::new(label).color(color).size(12.5).strong());
    }
}

/// Suite « icône + valeur », peinte d'un seul bloc : reste lisible dans
/// n'importe quel sens de mise en page (y compris droite-à-gauche).
pub fn stat_row(ui: &mut Ui, items: &[(Icon, String, Color32)], size: f32) -> Response {
    let font = FontId::proportional(size);
    let galleys: Vec<_> = items
        .iter()
        .map(|(_, text, color)| ui.fonts(|f| f.layout_no_wrap(text.clone(), font.clone(), *color)))
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
    let font = FontId::proportional(11.5);
    let galley = ui.fonts(|f| f.layout_no_wrap(text.to_owned(), font, color));
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
        .margin(egui::Margin::symmetric(10, 7))
        .background_color(theme::BG_DEEP)
        .desired_width(f32::INFINITY)
}
