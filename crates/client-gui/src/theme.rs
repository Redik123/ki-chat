//! Thème visuel de ki-chat : palette, typographie, styles de widgets.
//!
//! Tout passe par ici pour que l'application ait une identité cohérente :
//! un gris-bleu profond façon poste de jeu, un unique accent vert, des
//! coins arrondis réguliers et une échelle typographique lisible.

use eframe::egui::{
    self,
    epaint::Shadow,
    style::{HandleShape, Selection},
    Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Vec2,
};

// ---------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------

/// Fond le plus profond : zone de conversation, champs « creusés ».
pub const BG_DEEP: Color32 = Color32::from_rgb(0x0c, 0x0f, 0x14);
/// Colonne de gauche (salons, membres).
pub const BG_SIDE: Color32 = Color32::from_rgb(0x11, 0x15, 0x1b);
/// Fond général des panneaux.
pub const BG_BASE: Color32 = Color32::from_rgb(0x15, 0x1a, 0x21);
/// Surfaces en relief : fenêtres, cartes, boutons.
pub const BG_RAISED: Color32 = Color32::from_rgb(0x1b, 0x21, 0x2a);
pub const BG_HOVER: Color32 = Color32::from_rgb(0x24, 0x2c, 0x37);
pub const BG_ACTIVE: Color32 = Color32::from_rgb(0x2d, 0x37, 0x44);
/// Survol très discret (lignes de message).
pub const BG_GHOST: Color32 = Color32::from_rgb(0x1a, 0x20, 0x28);

pub const BORDER: Color32 = Color32::from_rgb(0x28, 0x31, 0x3c);
pub const BORDER_SOFT: Color32 = Color32::from_rgb(0x1e, 0x25, 0x2e);
/// Trait ou texte très en retrait, mais encore lisible.
pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x44, 0x51, 0x60);

pub const TEXT: Color32 = Color32::from_rgb(0xe7, 0xed, 0xf4);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x94, 0xa2, 0xb2);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x63, 0x70, 0x7f);

/// Accent de la marque (vert ki-chat).
pub const ACCENT: Color32 = Color32::from_rgb(0x00, 0xd2, 0x6a);
/// Vert « quelqu'un parle ».
pub const SPEAK: Color32 = Color32::from_rgb(0x00, 0xe6, 0x76);
pub const DANGER: Color32 = Color32::from_rgb(0xff, 0x6b, 0x6b);
pub const WARN: Color32 = Color32::from_rgb(0xff, 0xa1, 0x57);
pub const INFO: Color32 = Color32::from_rgb(0x58, 0xa6, 0xff);

/// Couleurs de pseudos, stables par hachage du nom.
const PALETTE: [Color32; 8] = [
    Color32::from_rgb(0x2d, 0xd4, 0x8f),
    Color32::from_rgb(0x62, 0xa8, 0xff),
    Color32::from_rgb(0xff, 0xa9, 0x5c),
    Color32::from_rgb(0xff, 0x8a, 0xc4),
    Color32::from_rgb(0xba, 0x92, 0xff),
    Color32::from_rgb(0x2a, 0xd3, 0xdd),
    Color32::from_rgb(0xff, 0xd8, 0x63),
    Color32::from_rgb(0xff, 0x7d, 0x7d),
];

/// Couleur attribuée à un pseudo — même pseudo, même couleur, partout.
///
/// Une palette fermée de huit teintes : les pseudos se lisent dans le fil de
/// discussion, mieux vaut peu de couleurs mais franchement distinctes.
pub fn color_for(name: &str) -> Color32 {
    let h = name
        .bytes()
        .fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    PALETTE[(h % PALETTE.len() as u32) as usize]
}

/// Couleur d'identité d'un serveur : teinte **continue** dérivée du nom.
///
/// Contrairement aux pseudos, on n'a ici que quelques vignettes côte à côte
/// et elles doivent se distinguer au premier coup d'œil : une palette de
/// huit couleurs donnerait une collision une fois sur huit. Le hachage
/// FNV-1a répartit sur tout le cercle chromatique.
pub fn badge_color(seed: &str) -> Color32 {
    let hash = seed
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |acc, b| {
            (acc ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
        });
    let hue = (hash % 3600) as f32 / 3600.0;
    egui::ecolor::Hsva::new(hue, 0.60, 0.94, 1.0).into()
}

/// Mélange linéaire de deux couleurs (`t` = 0 → `a`, 1 → `b`).
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

/// Même couleur, translucide (utile pour les fonds teintés).
pub fn alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

// ---------------------------------------------------------------------
// Installation
// ---------------------------------------------------------------------

/// Applique polices, couleurs et espacements au contexte egui.
pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);

    let mut style = (*ctx.style()).clone();
    style.visuals = visuals();
    style.text_styles = text_styles();

    let s = &mut style.spacing;
    s.item_spacing = Vec2::new(8.0, 6.0);
    s.button_padding = Vec2::new(11.0, 6.0);
    s.window_margin = Margin::same(16);
    s.menu_margin = Margin::same(8);
    s.interact_size = Vec2::new(40.0, 26.0);
    s.slider_width = 170.0;
    s.slider_rail_height = 6.0;
    s.combo_width = 150.0;
    s.icon_width = 18.0;
    s.icon_width_inner = 11.0;
    s.menu_spacing = 4.0;
    s.tooltip_width = 320.0;
    s.scroll.bar_width = 9.0;
    s.scroll.floating = true;
    s.scroll.floating_width = 5.0;
    s.scroll.floating_allocated_width = 0.0;
    s.scroll.dormant_handle_opacity = 0.0;
    s.scroll.active_handle_opacity = 0.5;
    s.scroll.interact_handle_opacity = 0.85;

    style.interaction.tooltip_delay = 0.35;
    style.interaction.selectable_labels = true;

    ctx.set_style(style);
}

/// Ajoute Hack à la famille proportionnelle.
///
/// Par défaut egui n'y met que Ubuntu-Light + les deux polices emoji : les
/// symboles géométriques (`●`, `↑`, `↓`…) n'y existent pas et s'affichaient
/// donc en carrés « tofu ». Hack les couvre — filet de sécurité pour tout
/// caractère un peu exotique qui traverserait l'interface.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    if let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
        family.push("Hack".to_owned());
    }
    ctx.set_fonts(fonts);
}

fn text_styles() -> std::collections::BTreeMap<TextStyle, FontId> {
    use FontFamily::{Monospace, Proportional};
    [
        (TextStyle::Heading, FontId::new(19.0, Proportional)),
        (TextStyle::Body, FontId::new(14.0, Proportional)),
        (TextStyle::Button, FontId::new(14.0, Proportional)),
        (TextStyle::Small, FontId::new(11.5, Proportional)),
        (TextStyle::Monospace, FontId::new(13.0, Monospace)),
    ]
    .into()
}

fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();

    v.panel_fill = BG_BASE;
    v.window_fill = BG_RAISED;
    v.extreme_bg_color = BG_DEEP;
    v.text_edit_bg_color = Some(BG_DEEP);
    v.faint_bg_color = BG_GHOST;
    v.code_bg_color = BG_DEEP;

    v.window_stroke = Stroke::new(1.0_f32, BORDER);
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_shadow = Shadow {
        offset: [0, 14],
        blur: 40,
        spread: 0,
        color: Color32::from_black_alpha(140),
    };
    v.popup_shadow = Shadow {
        offset: [0, 6],
        blur: 20,
        spread: 0,
        color: Color32::from_black_alpha(120),
    };

    // Sert au surlignage du texte, au remplissage des curseurs, et au
    // contour du champ de saisie qui a le focus — d'où l'accent.
    v.selection = Selection {
        bg_fill: alpha(ACCENT, 105),
        stroke: Stroke::new(1.0_f32, ACCENT),
    };
    v.hyperlink_color = INFO;
    v.error_fg_color = DANGER;
    v.warn_fg_color = WARN;
    v.weak_text_color = Some(TEXT_FAINT);
    v.slider_trailing_fill = true;
    v.handle_shape = HandleShape::Circle;
    v.striped = false;

    // Deux fonds distincts, et c'est important :
    //   `bg_fill`      → creux imposés (rail de curseur, case à cocher) ;
    //   `weak_bg_fill` → surfaces optionnelles (boutons, listes déroulantes).
    // Les confondre rendait le rail des curseurs invisible sur la fenêtre.
    let radius = CornerRadius::same(6);

    // Texte et traits « non interactifs » : labels, séparateurs.
    let w = &mut v.widgets.noninteractive;
    w.bg_fill = BG_DEEP;
    w.weak_bg_fill = BG_BASE;
    w.bg_stroke = Stroke::new(1.0_f32, BORDER_SOFT);
    w.fg_stroke = Stroke::new(1.0_f32, TEXT);
    w.corner_radius = radius;
    w.expansion = 0.0;

    // Boutons, combos, champs au repos.
    let w = &mut v.widgets.inactive;
    w.bg_fill = BG_DEEP;
    w.weak_bg_fill = BG_RAISED;
    w.bg_stroke = Stroke::new(1.0_f32, BORDER);
    w.fg_stroke = Stroke::new(1.6_f32, TEXT_DIM);
    w.corner_radius = radius;
    w.expansion = 0.0;

    let w = &mut v.widgets.hovered;
    w.bg_fill = BG_ACTIVE;
    w.weak_bg_fill = BG_HOVER;
    w.bg_stroke = Stroke::new(1.0_f32, mix(BORDER, ACCENT, 0.35));
    w.fg_stroke = Stroke::new(1.8_f32, TEXT);
    w.corner_radius = radius;
    w.expansion = 1.0;

    let w = &mut v.widgets.active;
    w.bg_fill = BG_ACTIVE;
    w.weak_bg_fill = BG_ACTIVE;
    w.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    w.fg_stroke = Stroke::new(1.8_f32, TEXT);
    w.corner_radius = radius;
    w.expansion = 0.0;

    let w = &mut v.widgets.open;
    w.bg_fill = BG_DEEP;
    w.weak_bg_fill = BG_HOVER;
    w.bg_stroke = Stroke::new(1.0_f32, BORDER);
    w.fg_stroke = Stroke::new(1.0_f32, TEXT);
    w.corner_radius = radius;

    v
}

// ---------------------------------------------------------------------
// Icône de fenêtre
// ---------------------------------------------------------------------

/// Génère l'icône de l'application (barre des tâches, alt-tab) : le même
/// pictogramme que le logo, rendu pixel par pixel — pas de fichier à
/// embarquer, pas de carré blanc par défaut.
pub fn app_icon() -> egui::IconData {
    const S: i32 = 64;
    let mut rgba = vec![0u8; (S * S * 4) as usize];

    // Rectangle arrondi de fond, puis les barres d'égaliseur en creux.
    let bars: [(f32, f32, f32); 4] = [
        (14.0, 26.0, 38.0),
        (25.0, 16.0, 48.0),
        (36.0, 21.0, 43.0),
        (47.0, 29.0, 35.0),
    ];

    for py in 0..S {
        for px in 0..S {
            let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
            let bg = coverage(x, y, 3.0, 3.0, 61.0, 61.0, 16.0);
            if bg <= 0.0 {
                continue;
            }
            // Creux : union des quatre barres.
            let mut cut: f32 = 0.0;
            for (cx, top, bottom) in bars {
                cut = cut.max(coverage(x, y, cx - 2.5, top, cx + 2.5, bottom, 2.5));
            }
            let lit = bg * (1.0 - cut);
            let i = ((py * S + px) * 4) as usize;
            let c = mix(BG_DEEP, ACCENT, lit);
            rgba[i] = c.r();
            rgba[i + 1] = c.g();
            rgba[i + 2] = c.b();
            rgba[i + 3] = (bg * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }

    egui::IconData {
        rgba,
        width: S as u32,
        height: S as u32,
    }
}

/// Couverture antialiasée d'un pixel par un rectangle arrondi.
fn coverage(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> f32 {
    let (hx, hy) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
    let r = r.min(hx).min(hy);
    let dx = (x - (x0 + hx)).abs() - (hx - r);
    let dy = (y - (y0 + hy)).abs() - (hy - r);
    let outside = dx.max(0.0).hypot(dy.max(0.0));
    let inside = dx.max(dy).min(0.0);
    (0.5 - (outside + inside - r)).clamp(0.0, 1.0)
}
