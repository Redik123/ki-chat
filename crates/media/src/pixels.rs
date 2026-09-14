//! NV12 -> RGBA : ce que le décodeur produit vers ce que l'écran affiche.
//!
//! NV12 : un plan de luminance (un octet par pixel), puis un plan de
//! chrominance entrelacée (U, V) à demi-résolution dans les deux sens. Les
//! coefficients suivent la norme de la source : BT.709 pour la haute
//! définition, BT.601 en dessous — c'est ce que les décodeurs supposent quand
//! le fichier ne dit rien, et ce que ffmpeg écrit.
//!
//! Arithmétique entière en virgule fixe (×256), deux pixels par pas (ils
//! partagent leur chrominance), et les lignes réparties entre les cœurs par
//! bandes : une image 1080p passe en quelques millisecondes.

use rayon::prelude::*;

/// Coefficients ×256 : (rv, gu, gv, bu).
const BT601: (i32, i32, i32, i32) = (409, 100, 208, 516);
const BT709: (i32, i32, i32, i32) = (459, 55, 136, 541);

/// Convertit une image NV12 en RGBA serré dans `sortie` (redimensionné).
///
/// `y` et `uv` sont les deux plans, chacun avec son pas (octets par ligne,
/// remplissage compris) ; `uv` fait `hauteur / 2` lignes de `largeur` octets
/// utiles. Dimensions impaires : la dernière colonne ou ligne prend la
/// chrominance de sa voisine.
pub fn nv12_vers_rgba(
    y: &[u8],
    pas_y: usize,
    uv: &[u8],
    pas_uv: usize,
    largeur: usize,
    hauteur: usize,
    sortie: &mut Vec<u8>,
) {
    sortie.resize(largeur * hauteur * 4, 0);
    if largeur == 0 || hauteur == 0 {
        return;
    }
    let coefs = if hauteur >= 720 { BT709 } else { BT601 };
    // Trente-deux lignes par tâche : assez de travail pour amortir la
    // distribution, assez de tâches pour occuper tous les cœurs en 1080p.
    const BANDE: usize = 32;
    sortie
        .par_chunks_mut(largeur * 4 * BANDE)
        .enumerate()
        .for_each(|(bande, lignes)| {
            let debut = bande * BANDE;
            for (i, ligne) in lignes.chunks_exact_mut(largeur * 4).enumerate() {
                let r = debut + i;
                let ly = &y[r * pas_y..r * pas_y + largeur];
                let ruv = (r / 2).min(hauteur.div_ceil(2) - 1);
                let luv = &uv[ruv * pas_uv..ruv * pas_uv + (largeur.div_ceil(2) * 2).min(pas_uv)];
                convertir_ligne(ly, luv, ligne, coefs);
            }
        });
}

fn convertir_ligne(ly: &[u8], luv: &[u8], sortie: &mut [u8], coefs: (i32, i32, i32, i32)) {
    let (rv, gu, gv, bu) = coefs;
    for (x, px) in sortie.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let c = 2 * (x / 2);
        let u = i32::from(luv.get(c).copied().unwrap_or(128)) - 128;
        let v = i32::from(luv.get(c + 1).copied().unwrap_or(128)) - 128;
        let yy = (i32::from(ly[x]) - 16).max(0) * 298;
        let r = (yy + rv * v + 128) >> 8;
        let g = (yy - gu * u - gv * v + 128) >> 8;
        let b = (yy + bu * u + 128) >> 8;
        px[0] = r.clamp(0, 255) as u8;
        px[1] = g.clamp(0, 255) as u8;
        px[2] = b.clamp(0, 255) as u8;
        px[3] = 255;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(y: u8, u: u8, v: u8, hd: bool) -> [u8; 4] {
        let (l, h) = if hd { (2, 720) } else { (2, 2) };
        let plan_y = vec![y; l * h];
        let plan_uv = {
            let mut p = vec![0u8; l * (h / 2)];
            for c in p.as_chunks_mut::<2>().0 {
                c[0] = u;
                c[1] = v;
            }
            p
        };
        let mut out = Vec::new();
        nv12_vers_rgba(&plan_y, l, &plan_uv, l, l, h, &mut out);
        [out[0], out[1], out[2], out[3]]
    }

    #[test]
    fn le_noir_et_le_blanc_video_sont_aux_bornes() {
        assert_eq!(pixel(16, 128, 128, false), [0, 0, 0, 255]);
        assert_eq!(pixel(235, 128, 128, false), [255, 255, 255, 255]);
        // Sous le noir vidéo, on ne passe pas en négatif.
        assert_eq!(pixel(0, 128, 128, true), [0, 0, 0, 255]);
    }

    #[test]
    fn un_gris_moyen_reste_neutre() {
        let [r, g, b, _] = pixel(126, 128, 128, false);
        assert!(
            (r as i32 - 128).abs() <= 2 && r == g && g == b,
            "{r} {g} {b}"
        );
    }

    #[test]
    fn le_rouge_pur_en_bt601_revient_rouge() {
        // (Y, U, V) du rouge saturé en BT.601 plage limitée : (81, 90, 240).
        let [r, g, b, _] = pixel(81, 90, 240, false);
        assert!(r >= 250, "r {r}");
        assert!(g <= 5 && b <= 5, "g {g} b {b}");
    }

    #[test]
    fn les_dimensions_impaires_passent() {
        let (l, h) = (3usize, 3usize);
        let y = vec![128u8; 4 * h];
        let uv = vec![128u8; 4 * 2];
        let mut out = Vec::new();
        nv12_vers_rgba(&y, 4, &uv, 4, l, h, &mut out);
        assert_eq!(out.len(), l * h * 4);
        assert!(out
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| p[3] == 255 && p[0] == p[1]));
    }
}
