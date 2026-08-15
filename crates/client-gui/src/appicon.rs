//! Rastérisation du pictogramme ki-chat, à n'importe quelle taille.
//!
//! Ce module a deux lecteurs : `theme.rs`, qui en tire l'icône de fenêtre à
//! l'exécution, et `build.rs`, qui en grave un `.ico` multi-résolutions dans
//! le binaire (Explorateur, barre des tâches, raccourcis). Le même dessin des
//! deux côtés, sans fichier d'image à maintenir ni à garder synchronisé.
//!
//! D'où l'absence de toute dépendance ici : `build.rs` l'inclut tel quel, en
//! dehors du crate, donc rien de ce qui vient d'`egui` n'y a sa place.

/// Fond du pictogramme — `theme::BG_DEEP`.
pub const BG: [u8; 3] = [0x0c, 0x0f, 0x14];
/// Corps du pictogramme — `theme::ACCENT`.
pub const FG: [u8; 3] = [0x00, 0xd2, 0x6a];

/// Barres d'égaliseur, en coordonnées du dessin de référence (64 × 64) :
/// abscisse du centre, haut, bas.
const BARS: [(f32, f32, f32); 4] = [
    (14.0, 26.0, 38.0),
    (25.0, 16.0, 48.0),
    (36.0, 21.0, 43.0),
    (47.0, 29.0, 35.0),
];

/// Dessine l'icône en RGBA non prémultiplié, `size` × `size`.
///
/// La géométrie est décrite une fois pour toutes en 64 × 64 puis mise à
/// l'échelle : l'antialiasing est calculé dans les pixels réellement produits,
/// donc une icône de 16 px reste nette au lieu d'être un 64 px réduit.
pub fn render(size: u32) -> Vec<u8> {
    let k = size as f32 / 64.0;
    let mut rgba = vec![0u8; size as usize * size as usize * 4];

    for py in 0..size {
        for px in 0..size {
            let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
            let bg = coverage(x, y, 3.0 * k, 3.0 * k, 61.0 * k, 61.0 * k, 16.0 * k);
            if bg <= 0.0 {
                continue;
            }
            // Creux : union des quatre barres.
            let mut cut: f32 = 0.0;
            for (cx, top, bottom) in BARS {
                let c = coverage(
                    x,
                    y,
                    (cx - 2.5) * k,
                    top * k,
                    (cx + 2.5) * k,
                    bottom * k,
                    2.5 * k,
                );
                cut = cut.max(c);
            }
            let lit = bg * (1.0 - cut);
            let i = (py as usize * size as usize + px as usize) * 4;
            let c = mix(BG, FG, lit);
            rgba[i] = c[0];
            rgba[i + 1] = c[1];
            rgba[i + 2] = c[2];
            rgba[i + 3] = (bg * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }

    rgba
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

/// Interpolation linéaire entre deux couleurs.
fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    [f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2])]
}
