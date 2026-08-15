//! Jeu d'icônes vectorielles, dessinées au trait sur une grille 24×24.
//!
//! Pourquoi ne pas utiliser des emoji ? Parce que les polices livrées avec
//! egui ne couvrent qu'une partie du Unicode : `●`, `↑`, `↓` ou `✕` sortent
//! en carrés vides, et les emoji disponibles viennent de deux polices
//! différentes — tailles et graisses incohérentes. En dessinant les icônes
//! on obtient un jeu homogène, net à toutes les tailles, et surtout aucun
//! caractère manquant possible.

use eframe::egui::{Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Mic,
    MicOff,
    Headphones,
    Gear,
    Sliders,
    Crown,
    Paperclip,
    Copy,
    Plus,
    Refresh,
    Play,
    Target,
    Logout,
    Hash,
    User,
    Close,
    ArrowUp,
    ArrowDown,
    Ban,
    Key,
    Volume,
    Send,
    Check,
    Warning,
    Info,
    Chat,
    Pencil,
    Trash,
    Server,
}

/// Dessine `icon` centrée dans `rect` (le carré inscrit est utilisé).
pub fn draw(p: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let side = rect.width().min(rect.height());
    let stroke = Stroke::new((side / 24.0 * 2.0).max(1.2), color);
    let pen = Pen {
        p,
        rect,
        side,
        stroke,
        color,
    };
    pen.render(icon);
}

// ---------------------------------------------------------------------

struct Pen<'a> {
    p: &'a Painter,
    rect: Rect,
    side: f32,
    stroke: Stroke,
    color: Color32,
}

impl Pen<'_> {
    /// Coordonnée de la grille 24×24 vers l'écran.
    fn at(&self, x: f32, y: f32) -> Pos2 {
        let origin = self.rect.center() - Vec2::splat(self.side / 2.0);
        Pos2::new(
            origin.x + x / 24.0 * self.side,
            origin.y + y / 24.0 * self.side,
        )
    }

    /// Longueur de la grille vers l'écran.
    fn u(&self, v: f32) -> f32 {
        v / 24.0 * self.side
    }

    fn pts(&self, pts: &[(f32, f32)]) -> Vec<Pos2> {
        pts.iter().map(|&(x, y)| self.at(x, y)).collect()
    }

    fn path(&self, pts: &[(f32, f32)]) {
        self.p.add(Shape::line(self.pts(pts), self.stroke));
    }

    fn closed(&self, pts: &[(f32, f32)]) {
        self.p.add(Shape::closed_line(self.pts(pts), self.stroke));
    }

    fn solid(&self, pts: &[(f32, f32)]) {
        self.p.add(Shape::convex_polygon(
            self.pts(pts),
            self.color,
            Stroke::NONE,
        ));
    }

    fn seg(&self, a: (f32, f32), b: (f32, f32)) {
        self.p
            .line_segment([self.at(a.0, a.1), self.at(b.0, b.1)], self.stroke);
    }

    fn ring(&self, c: (f32, f32), r: f32) {
        self.p
            .circle_stroke(self.at(c.0, c.1), self.u(r), self.stroke);
    }

    fn disc(&self, c: (f32, f32), r: f32) {
        self.p
            .circle_filled(self.at(c.0, c.1), self.u(r), self.color);
    }

    fn rrect(&self, x0: f32, y0: f32, x1: f32, y1: f32, r: f32, filled: bool) {
        let rect = Rect::from_min_max(self.at(x0, y0), self.at(x1, y1));
        let cr = CornerRadius::same(self.u(r).round().clamp(0.0, 255.0) as u8);
        if filled {
            self.p.rect_filled(rect, cr, self.color);
        } else {
            self.p
                .rect_stroke(rect, cr, self.stroke, StrokeKind::Middle);
        }
    }

    /// Points d'un arc, angles en degrés (0° à droite, sens horaire à l'écran).
    fn arc_pts(&self, c: (f32, f32), r: f32, a0: f32, a1: f32) -> Vec<(f32, f32)> {
        let steps = (((a1 - a0).abs() / 10.0).ceil() as usize).max(2);
        (0..=steps)
            .map(|i| {
                let a = (a0 + (a1 - a0) * i as f32 / steps as f32).to_radians();
                (c.0 + r * a.cos(), c.1 + r * a.sin())
            })
            .collect()
    }

    fn arc(&self, c: (f32, f32), r: f32, a0: f32, a1: f32) {
        let pts = self.arc_pts(c, r, a0, a1);
        self.path(&pts);
    }

    /// Pointe de flèche : `dir` doit être unitaire.
    fn head(&self, tip: (f32, f32), dir: (f32, f32), len: f32, half_width: f32) {
        let n = (-dir.1, dir.0);
        let base = (tip.0 - dir.0 * len, tip.1 - dir.1 * len);
        self.path(&[
            (base.0 + n.0 * half_width, base.1 + n.1 * half_width),
            tip,
            (base.0 - n.0 * half_width, base.1 - n.1 * half_width),
        ]);
    }

    fn mic_body(&self) {
        self.rrect(9.0, 2.0, 15.0, 13.5, 3.0, false);
        let mut pts = vec![(5.0, 10.5)];
        pts.extend(self.arc_pts((12.0, 12.0), 7.0, 180.0, 0.0));
        pts.push((19.0, 10.5));
        self.path(&pts);
        self.seg((12.0, 19.0), (12.0, 22.0));
        self.seg((8.5, 22.0), (15.5, 22.0));
    }

    fn speaker_body(&self) {
        self.solid(&[(3.5, 9.0), (8.0, 9.0), (8.0, 15.0), (3.5, 15.0)]);
        self.solid(&[(8.0, 9.0), (13.0, 4.0), (13.0, 20.0), (8.0, 15.0)]);
    }

    fn render(&self, icon: Icon) {
        match icon {
            Icon::Mic => self.mic_body(),
            Icon::MicOff => {
                self.mic_body();
                self.seg((3.5, 3.5), (20.5, 20.5));
            }
            Icon::Headphones => {
                self.arc((12.0, 13.0), 8.0, 180.0, 360.0);
                self.rrect(3.0, 12.5, 7.6, 20.5, 2.0, true);
                self.rrect(16.4, 12.5, 21.0, 20.5, 2.0, true);
            }
            Icon::Gear => {
                let teeth = 8;
                let step = 360.0 / teeth as f32;
                let mut pts = Vec::with_capacity(teeth * 4);
                for i in 0..teeth {
                    let a = i as f32 * step;
                    for (off, r) in [
                        (-0.34 * step, 6.4),
                        (-0.17 * step, 9.3),
                        (0.17 * step, 9.3),
                        (0.34 * step, 6.4),
                    ] {
                        let t = (a + off).to_radians();
                        pts.push((12.0 + r * t.cos(), 12.0 + r * t.sin()));
                    }
                }
                self.closed(&pts);
                self.ring((12.0, 12.0), 3.1);
            }
            Icon::Sliders => {
                for (y, knob) in [(6.5, 9.0), (12.0, 15.5), (17.5, 7.0)] {
                    self.seg((3.5, y), (20.5, y));
                    self.disc((knob, y), 2.7);
                }
            }
            Icon::Crown => {
                // Composée de formes convexes : epaint ne remplit correctement
                // que les polygones convexes.
                self.rrect(3.2, 15.0, 20.8, 19.4, 1.2, true);
                self.solid(&[(3.2, 16.5), (4.4, 6.8), (10.0, 16.5)]);
                self.solid(&[(20.8, 16.5), (19.6, 6.8), (14.0, 16.5)]);
                self.solid(&[(6.2, 16.5), (12.0, 4.6), (17.8, 16.5)]);
            }
            Icon::Paperclip => {
                let mut pts = vec![(16.8, 7.5), (16.8, 15.2)];
                pts.extend(self.arc_pts((12.3, 15.2), 4.5, 0.0, 180.0));
                pts.push((7.8, 6.8));
                pts.extend(self.arc_pts((10.6, 6.8), 2.8, 180.0, 360.0));
                pts.push((13.4, 15.6));
                pts.extend(self.arc_pts((11.9, 15.6), 1.5, 0.0, 180.0));
                pts.push((10.4, 9.0));
                self.path(&pts);
            }
            Icon::Copy => {
                self.rrect(9.0, 9.0, 20.5, 20.5, 2.4, false);
                self.path(&[(16.0, 4.5), (6.5, 4.5), (6.5, 14.5)]);
            }
            Icon::Plus => {
                self.seg((12.0, 5.5), (12.0, 18.5));
                self.seg((5.5, 12.0), (18.5, 12.0));
            }
            Icon::Refresh => {
                self.arc((12.0, 12.0), 7.6, -40.0, 250.0);
                let a = 250.0f32.to_radians();
                let tip = (12.0 + 7.6 * a.cos(), 12.0 + 7.6 * a.sin());
                self.head(tip, (-a.sin(), a.cos()), 3.6, 2.6);
            }
            Icon::Play => self.solid(&[(8.5, 5.0), (19.0, 12.0), (8.5, 19.0)]),
            Icon::Target => {
                self.ring((12.0, 12.0), 8.0);
                self.ring((12.0, 12.0), 3.4);
                self.seg((12.0, 1.6), (12.0, 6.0));
                self.seg((12.0, 18.0), (12.0, 22.4));
                self.seg((1.6, 12.0), (6.0, 12.0));
                self.seg((18.0, 12.0), (22.4, 12.0));
            }
            Icon::Logout => {
                self.path(&[(10.5, 4.0), (5.0, 4.0), (5.0, 20.0), (10.5, 20.0)]);
                self.seg((10.0, 12.0), (20.5, 12.0));
                self.head((20.5, 12.0), (1.0, 0.0), 4.2, 3.0);
            }
            Icon::Hash => {
                self.seg((10.2, 3.5), (8.2, 20.5));
                self.seg((17.0, 3.5), (15.0, 20.5));
                self.seg((4.2, 9.0), (19.6, 9.0));
                self.seg((3.4, 15.5), (18.8, 15.5));
            }
            Icon::User => {
                self.ring((12.0, 8.2), 3.9);
                self.arc((12.0, 21.5), 7.6, 180.0, 360.0);
            }
            Icon::Close => {
                self.seg((6.5, 6.5), (17.5, 17.5));
                self.seg((17.5, 6.5), (6.5, 17.5));
            }
            Icon::ArrowUp => {
                self.seg((12.0, 20.0), (12.0, 5.0));
                self.head((12.0, 5.0), (0.0, -1.0), 5.2, 4.2);
            }
            Icon::ArrowDown => {
                self.seg((12.0, 4.0), (12.0, 19.0));
                self.head((12.0, 19.0), (0.0, 1.0), 5.2, 4.2);
            }
            Icon::Ban => {
                self.ring((12.0, 12.0), 8.2);
                self.seg((6.2, 6.2), (17.8, 17.8));
            }
            Icon::Key => {
                self.ring((8.2, 15.8), 3.9);
                self.seg((11.0, 13.0), (20.5, 3.5));
                self.seg((16.4, 7.6), (19.0, 10.2));
                self.seg((13.8, 10.2), (15.8, 12.2));
            }
            Icon::Volume => {
                self.speaker_body();
                self.arc((13.0, 12.0), 4.6, -58.0, 58.0);
                self.arc((13.0, 12.0), 7.8, -52.0, 52.0);
            }
            Icon::Send => {
                self.closed(&[(21.0, 3.0), (3.0, 10.6), (10.4, 13.6), (13.2, 21.0)]);
                self.seg((10.4, 13.6), (21.0, 3.0));
            }
            Icon::Check => self.path(&[(5.0, 12.5), (10.0, 17.5), (19.0, 7.0)]),
            Icon::Warning => {
                self.closed(&[(12.0, 3.2), (22.0, 20.6), (2.0, 20.6)]);
                self.seg((12.0, 9.6), (12.0, 14.6));
                self.disc((12.0, 17.6), 1.1);
            }
            Icon::Info => {
                self.ring((12.0, 12.0), 8.6);
                self.seg((12.0, 11.0), (12.0, 17.0));
                self.disc((12.0, 7.4), 1.2);
            }
            Icon::Chat => {
                self.rrect(2.6, 4.0, 21.4, 17.0, 3.5, false);
                self.path(&[(8.0, 17.0), (7.0, 21.6), (13.0, 17.0)]);
            }
            Icon::Pencil => {
                self.closed(&[
                    (3.5, 20.5),
                    (4.8, 15.8),
                    (15.9, 4.7),
                    (19.3, 8.1),
                    (8.2, 19.2),
                ]);
                self.seg((13.5, 7.2), (16.9, 10.6));
            }
            Icon::Trash => {
                self.seg((3.4, 6.4), (20.6, 6.4));
                self.path(&[(9.0, 6.4), (9.0, 3.6), (15.0, 3.6), (15.0, 6.4)]);
                self.path(&[(5.8, 6.4), (6.8, 20.6), (17.2, 20.6), (18.2, 6.4)]);
                self.seg((10.0, 10.2), (10.3, 17.0));
                self.seg((14.0, 10.2), (13.7, 17.0));
            }
            Icon::Server => {
                self.rrect(3.0, 4.0, 21.0, 10.6, 2.0, false);
                self.rrect(3.0, 13.4, 21.0, 20.0, 2.0, false);
                self.disc((6.8, 7.3), 1.1);
                self.disc((6.8, 16.7), 1.1);
            }
        }
    }
}

// ---------------------------------------------------------------------
// Pictogrammes composés
// ---------------------------------------------------------------------

/// Barres de signal : `lit` barres allumées sur 4. Sert d'indicateur de ping.
pub fn signal(p: &Painter, rect: Rect, lit: u8, on: Color32, off: Color32) {
    let side = rect.width().min(rect.height());
    let pen = Pen {
        p,
        rect,
        side,
        stroke: Stroke::NONE,
        color: on,
    };
    for i in 0..4u8 {
        let x = 3.0 + i as f32 * 5.0;
        let height = 4.5 + i as f32 * 4.8;
        let bar = Rect::from_min_max(pen.at(x, 21.0 - height), pen.at(x + 3.2, 21.0));
        let color = if i < lit { on } else { off };
        p.rect_filled(bar, CornerRadius::same(1), color);
    }
}

/// Le logo de ki-chat : pastille arrondie + égaliseur en creux.
pub fn logo(p: &Painter, rect: Rect, fg: Color32, bg: Color32) {
    let side = rect.width().min(rect.height());
    let radius = CornerRadius::same((side * 0.26).round().clamp(0.0, 255.0) as u8);
    let square = Rect::from_center_size(rect.center(), Vec2::splat(side));
    p.rect_filled(square, radius, fg);

    let pen = Pen {
        p,
        rect,
        side,
        stroke: Stroke::NONE,
        color: bg,
    };
    let bar_radius = CornerRadius::same((side / 24.0 * 1.3).round().clamp(0.0, 255.0) as u8);
    for (x, top, bottom) in [
        (5.4f32, 9.5f32, 14.5f32),
        (9.6, 6.0, 18.0),
        (13.8, 8.0, 16.0),
        (18.0, 10.5, 13.5),
    ] {
        let bar = Rect::from_min_max(pen.at(x, top), pen.at(x + 2.2, bottom));
        p.rect_filled(bar, bar_radius, bg);
    }
}

/// Pastille ronde pleine, remplaçant le `●` manquant des polices.
pub fn dot(p: &Painter, center: Pos2, radius: f32, color: Color32) {
    p.circle_filled(center, radius, color);
}
