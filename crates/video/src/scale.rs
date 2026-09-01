//! Réduction d'échelle I420 pour la diffusion : la source (1080p, 1440p,
//! 4K…) descend à la hauteur choisie avant l'encodeur — moins de pixels à
//! encoder, moins de débit sur le fil, et un jeu qui garde son CPU.
//!
//! Deux étapes : des moitiés exactes (moyenne 2×2, rapide et sans alias)
//! tant que la source fait au moins deux fois la cible, puis un bilinéaire
//! séparable pour le reste. Sur du texte d'écran, c'est net sans
//! scintiller ; un bilinéaire seul à ×3 aurait mangé les traits fins.

use openh264::formats::YUVSource;

/// Un tampon I420 serré (stride = largeur), possédé. Dimensions paires.
pub struct I420 {
    pub width: usize,
    pub height: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl I420 {
    pub fn new(width: usize, height: usize) -> Self {
        let mut b = Self { width: 0, height: 0, y: Vec::new(), u: Vec::new(), v: Vec::new() };
        b.resize(width, height);
        b
    }

    /// Redimensionne sans réallouer quand la capacité suffit.
    fn resize(&mut self, width: usize, height: usize) {
        debug_assert!(width.is_multiple_of(2) && height.is_multiple_of(2));
        self.width = width;
        self.height = height;
        self.y.resize(width * height, 0);
        self.u.resize((width / 2) * (height / 2), 0);
        self.v.resize((width / 2) * (height / 2), 0);
    }
}

impl YUVSource for I420 {
    fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn strides(&self) -> (usize, usize, usize) {
        (self.width, self.width / 2, self.width / 2)
    }

    fn y(&self) -> &[u8] {
        &self.y
    }

    fn u(&self) -> &[u8] {
        &self.u
    }

    fn v(&self) -> &[u8] {
        &self.v
    }
}

/// Les dimensions de sortie pour une hauteur plafond (0 = pas de
/// réduction) : le rapport est conservé, les dimensions restent paires, et
/// l'on n'agrandit jamais — une source plus petite que le plafond part
/// telle quelle.
pub fn target_dims(src_w: u32, src_h: u32, max_height: u32) -> (u32, u32) {
    if max_height == 0 || src_h <= max_height {
        return (src_w & !1, src_h & !1);
    }
    let h = max_height & !1;
    let w = ((src_w as u64 * h as u64 + src_h as u64 / 2) / src_h as u64) as u32 & !1;
    (w.max(2), h.max(2))
}

/// Un plan 8 bits vu en lecture : données, dimensions, stride.
#[derive(Clone, Copy)]
struct Plan<'a> {
    px: &'a [u8],
    w: usize,
    h: usize,
    stride: usize,
}

/// Le réducteur, avec ses tampons de travail : rien n'est alloué en régime
/// établi.
pub struct Scaler {
    out: I420,
    a: I420,
    b: I420,
    /// Deux lignes interpolées horizontalement (valeurs ×256), et l'index
    /// de la ligne source qu'elles représentent.
    row0: Vec<u32>,
    row1: Vec<u32>,
}

impl Default for Scaler {
    fn default() -> Self {
        Self::new()
    }
}

impl Scaler {
    pub fn new() -> Self {
        Self {
            out: I420::new(2, 2),
            a: I420::new(2, 2),
            b: I420::new(2, 2),
            row0: Vec::new(),
            row1: Vec::new(),
        }
    }

    /// Réduit `src` en `dw`×`dh` (pairs, pas plus grands que la source) et
    /// rend le tampon de sortie, réutilisé d'un appel à l'autre.
    pub fn scale(&mut self, src: &dyn YUVSource, dw: usize, dh: usize) -> &I420 {
        let (sw, sh) = src.dimensions();
        let (ys, us, vs) = src.strides();
        let mut w = sw;
        let mut h = sh;
        // Les moitiés exactes. L'image courante vit toujours dans `a` (la
        // source elle-même pour la première), la moitié s'écrit dans `b`,
        // puis les deux s'échangent — l'emprunteur voit deux champs
        // distincts, jamais le même des deux côtés.
        let mut etape = 0usize;
        while w / 2 >= dw && h / 2 >= dh && (w, h) != (dw, dh) {
            let (nw, nh) = ((w / 2) & !1, (h / 2) & !1);
            {
                let (src_y, src_u, src_v) = if etape == 0 {
                    plans_de(src, w, h, (ys, us, vs))
                } else {
                    plans_de(&self.a, w, h, (w, w / 2, w / 2))
                };
                let dst = &mut self.b;
                dst.resize(nw, nh);
                halve_plane(src_y, &mut dst.y, nw, nh);
                halve_plane(src_u, &mut dst.u, nw / 2, nh / 2);
                halve_plane(src_v, &mut dst.v, nw / 2, nh / 2);
            }
            std::mem::swap(&mut self.a, &mut self.b);
            w = nw;
            h = nh;
            etape += 1;
        }

        self.out.resize(dw, dh);
        let (src_y, src_u, src_v) = if etape == 0 {
            plans_de(src, w, h, (ys, us, vs))
        } else {
            plans_de(&self.a, w, h, (w, w / 2, w / 2))
        };
        bilinear_plane(src_y, &mut self.out.y, dw, dh, &mut self.row0, &mut self.row1);
        bilinear_plane(src_u, &mut self.out.u, dw / 2, dh / 2, &mut self.row0, &mut self.row1);
        bilinear_plane(src_v, &mut self.out.v, dw / 2, dh / 2, &mut self.row0, &mut self.row1);
        &self.out
    }
}

/// Les trois plans d'une source, avec les strides donnés.
fn plans_de<'a>(
    src: &'a dyn YUVSource,
    w: usize,
    h: usize,
    (ys, us, vs): (usize, usize, usize),
) -> (Plan<'a>, Plan<'a>, Plan<'a>) {
    (
        Plan { px: src.y(), w, h, stride: ys },
        Plan { px: src.u(), w: w / 2, h: h / 2, stride: us },
        Plan { px: src.v(), w: w / 2, h: h / 2, stride: vs },
    )
}

/// Moyenne 2×2 : chaque pixel de sortie est la moyenne exacte de quatre
/// pixels source. `dw`/`dh` ≤ moitié de la source.
fn halve_plane(src: Plan<'_>, dst: &mut [u8], dw: usize, dh: usize) {
    debug_assert!(2 * dw <= src.w && 2 * dh <= src.h);
    for y in 0..dh {
        let r0 = &src.px[2 * y * src.stride..];
        let r1 = &src.px[(2 * y + 1) * src.stride..];
        let out = &mut dst[y * dw..(y + 1) * dw];
        for (x, o) in out.iter_mut().enumerate() {
            let s = r0[2 * x] as u32 + r0[2 * x + 1] as u32 + r1[2 * x] as u32 + r1[2 * x + 1] as u32;
            *o = ((s + 2) >> 2) as u8;
        }
    }
}

/// Bilinéaire séparable en virgule fixe (poids sur 8 bits), avec un cache de
/// deux lignes interpolées horizontalement : chaque ligne source n'est
/// traitée qu'une fois, la verticale ne coûte qu'une addition par pixel.
/// Sur une égalité de dimensions, c'est une copie.
fn bilinear_plane(
    src: Plan<'_>,
    dst: &mut [u8],
    dw: usize,
    dh: usize,
    row0: &mut Vec<u32>,
    row1: &mut Vec<u32>,
) {
    if (src.w, src.h) == (dw, dh) {
        for y in 0..dh {
            dst[y * dw..(y + 1) * dw].copy_from_slice(&src.px[y * src.stride..y * src.stride + dw]);
        }
        return;
    }
    // Centres de pixels alignés : le pixel de sortie x couvre
    // [x, x+1) × sw/dw dans la source.
    let taps_x: Vec<(usize, usize, u32)> = (0..dw)
        .map(|x| {
            let fx = ((x as f64 + 0.5) * src.w as f64 / dw as f64 - 0.5).max(0.0);
            let x0 = (fx.floor() as usize).min(src.w - 1);
            let x1 = (x0 + 1).min(src.w - 1);
            (x0, x1, ((fx - x0 as f64) * 256.0).round() as u32)
        })
        .collect();
    row0.resize(dw, 0);
    row1.resize(dw, 0);
    let mut idx0 = usize::MAX;
    let mut idx1 = usize::MAX;
    let remplir = |buf: &mut [u32], r: usize| {
        let ligne = &src.px[r * src.stride..r * src.stride + src.w];
        for (o, &(x0, x1, wx)) in buf.iter_mut().zip(&taps_x) {
            *o = ligne[x0] as u32 * (256 - wx) + ligne[x1] as u32 * wx;
        }
    };
    for y in 0..dh {
        let fy = ((y as f64 + 0.5) * src.h as f64 / dh as f64 - 0.5).max(0.0);
        let y0 = (fy.floor() as usize).min(src.h - 1);
        let y1 = (y0 + 1).min(src.h - 1);
        let wy = ((fy - y0 as f64) * 256.0).round() as u32;
        if idx0 != y0 {
            if idx1 == y0 {
                std::mem::swap(row0, row1);
                idx0 = y0;
                idx1 = usize::MAX;
            } else {
                remplir(row0, y0);
                idx0 = y0;
            }
        }
        if idx1 != y1 {
            remplir(row1, y1);
            idx1 = y1;
        }
        let out = &mut dst[y * dw..(y + 1) * dw];
        for ((o, &a), &b) in out.iter_mut().zip(row0.iter()).zip(row1.iter()) {
            *o = ((a * (256 - wy) + b * wy + (1 << 15)) >> 16) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_dimensions_cibles_gardent_le_rapport_et_restent_paires() {
        assert_eq!(target_dims(1920, 1080, 0), (1920, 1080));
        assert_eq!(target_dims(1920, 1080, 1080), (1920, 1080));
        assert_eq!(target_dims(1920, 1080, 720), (1280, 720));
        assert_eq!(target_dims(1920, 1080, 480), (852, 480));
        assert_eq!(target_dims(2560, 1440, 720), (1280, 720));
        assert_eq!(target_dims(3840, 2160, 1080), (1920, 1080));
        // Une source plus petite que le plafond n'est pas agrandie.
        assert_eq!(target_dims(1280, 720, 1080), (1280, 720));
        // Impair : rogné au pair.
        assert_eq!(target_dims(1366, 767, 0), (1366, 766));
    }

    fn uni(w: usize, h: usize, val: u8) -> I420 {
        let mut b = I420::new(w, h);
        b.y.fill(val);
        b.u.fill(128);
        b.v.fill(200);
        b
    }

    #[test]
    fn une_image_unie_reste_unie_a_toutes_les_echelles() {
        let src = uni(1920, 1080, 77);
        let mut s = Scaler::new();
        for (dw, dh) in [(1280usize, 720usize), (852, 480), (960, 540), (480, 270)] {
            let out = s.scale(&src, dw, dh);
            assert_eq!(out.dimensions(), (dw, dh));
            assert!(out.y.iter().all(|&p| p == 77), "{dw}x{dh} luma");
            assert!(out.u.iter().all(|&p| p == 128), "{dw}x{dh} u");
            assert!(out.v.iter().all(|&p| p == 200), "{dw}x{dh} v");
        }
    }

    #[test]
    fn un_degrade_horizontal_reste_monotone_et_couvre_la_plage() {
        let (w, h) = (1920usize, 1080usize);
        let mut src = I420::new(w, h);
        for y in 0..h {
            for x in 0..w {
                src.y[y * w + x] = (x * 255 / (w - 1)) as u8;
            }
        }
        let mut s = Scaler::new();
        // ×1,5 (bilinéaire seul) puis ×4 (deux moitiés puis rien).
        for (dw, dh) in [(1280usize, 720usize), (480, 270)] {
            let out = s.scale(&src, dw, dh);
            let ligne = &out.y[..dw];
            assert!(ligne.windows(2).all(|p| p[0] <= p[1]), "{dw}x{dh}");
            assert!(ligne[0] <= 2 && ligne[dw - 1] >= 253, "{dw}x{dh} : {} .. {}", ligne[0], ligne[dw - 1]);
        }
    }

    #[test]
    fn la_moitie_exacte_moyenne_quatre_pixels() {
        let mut src = I420::new(4, 2);
        src.y.copy_from_slice(&[0, 20, 100, 200, 40, 20, 100, 200]);
        let mut s = Scaler::new();
        let out = s.scale(&src, 2, 2);
        // 2x2 → même hauteur exige un bilinéaire vertical à l'identité ;
        // la largeur, elle, est bien divisée par deux.
        assert_eq!(out.dimensions(), (2, 2));
        assert_eq!(&out.y[..2], &[10, 150]);
    }
}
