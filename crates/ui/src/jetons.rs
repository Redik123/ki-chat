//! Les jetons de design : la palette, et les échelles des espacements, des
//! rayons d'angle et des tailles de texte.
//!
//! Ils viennent de [`ki_ui_base`], commune aux applis ki-* : ki-studio en
//! tire les mêmes couleurs pour iced, ki-core pour l'appli mobile. Ici, ils
//! prennent les types d'egui — à la compilation, sans rien coûter.
//!
//! Les nouveaux écrans s'en tiennent à ces valeurs ; les anciens y viennent
//! à mesure qu'on les reprend. Une valeur qui manque se discute dans
//! ki-ui-base, pas au coin d'un écran.

use egui::Color32;
use ki_ui_base::Couleur;

pub use ki_ui_base::{espace, largeur, rayon, texte};

/// Une couleur de la base, en egui. La palette est opaque, et une couleur
/// translucide ne passe pas par ici : refusée à la compilation pour une
/// constante (egui prémultiplie l'alpha, la base non).
pub const fn en_egui(c: Couleur) -> Color32 {
    assert!(c.a == 255, "en_egui : couleur translucide");
    Color32::from_rgb(c.r, c.g, c.b)
}

const fn serie<const N: usize>(base: [Couleur; N]) -> [Color32; N] {
    let mut sortie = [Color32::TRANSPARENT; N];
    let mut i = 0;
    while i < N {
        sortie[i] = en_egui(base[i]);
        i += 1;
    }
    sortie
}

const fn nommees<const N: usize>(base: [(&'static str, Couleur); N]) -> [(&'static str, Color32); N] {
    let mut sortie = [("", Color32::TRANSPARENT); N];
    let mut i = 0;
    while i < N {
        sortie[i] = (base[i].0, en_egui(base[i].1));
        i += 1;
    }
    sortie
}

/// La palette : un gris-bleu profond façon poste de jeu, un unique accent
/// vert. Les rôles sont décrits dans [`ki_ui_base::couleur`].
pub mod couleur {
    use super::{en_egui, nommees, serie, Color32};
    use ki_ui_base::couleur as base;

    pub const BG_DEEP: Color32 = en_egui(base::BG_DEEP);
    pub const BG_SIDE: Color32 = en_egui(base::BG_SIDE);
    pub const BG_BASE: Color32 = en_egui(base::BG_BASE);
    pub const BG_RAISED: Color32 = en_egui(base::BG_RAISED);
    pub const BG_HOVER: Color32 = en_egui(base::BG_HOVER);
    pub const BG_ACTIVE: Color32 = en_egui(base::BG_ACTIVE);
    pub const BG_GHOST: Color32 = en_egui(base::BG_GHOST);

    pub const BORDER: Color32 = en_egui(base::BORDER);
    pub const BORDER_SOFT: Color32 = en_egui(base::BORDER_SOFT);
    pub const BORDER_STRONG: Color32 = en_egui(base::BORDER_STRONG);

    pub const TEXT: Color32 = en_egui(base::TEXT);
    pub const TEXT_DIM: Color32 = en_egui(base::TEXT_DIM);
    pub const TEXT_FAINT: Color32 = en_egui(base::TEXT_FAINT);

    pub const ACCENT: Color32 = en_egui(base::ACCENT);
    pub const SPEAK: Color32 = en_egui(base::SPEAK);
    pub const DANGER: Color32 = en_egui(base::DANGER);
    pub const WARN: Color32 = en_egui(base::WARN);
    pub const INFO: Color32 = en_egui(base::INFO);
    pub const INVITE: Color32 = en_egui(base::INVITE);

    /// Les huit teintes des pseudos, dans l'ordre du hachage.
    pub const PSEUDOS: [Color32; 8] = serie(base::PSEUDOS);

    /// La palette, nommée : pour la montrer (la vitrine) et pour vérifier
    /// que ses copies hors de Rust la suivent.
    pub const NOMMEES: [(&str, Color32); 19] = nommees(base::NOMMEES);

    /// Les couleurs d'une série — bandes d'un égaliseur, courbes d'un
    /// graphe.
    pub const SERIES: [Color32; 8] = serie(base::SERIES);

    /// Couleur attribuée à un pseudo — même pseudo, même couleur, partout
    /// (et d'une appli ki-* à l'autre) : [`base::pour_pseudo`].
    pub fn pour_pseudo(nom: &str) -> Color32 {
        en_egui(base::pour_pseudo(nom))
    }

    /// Couleur d'identité d'un serveur : teinte **continue** dérivée du nom.
    ///
    /// Contrairement aux pseudos, on n'a ici que quelques vignettes côte à
    /// côte et elles doivent se distinguer au premier coup d'œil : une
    /// palette de huit couleurs donnerait une collision une fois sur huit.
    /// Le hachage FNV-1a répartit sur tout le cercle chromatique.
    pub fn d_insigne(graine: &str) -> Color32 {
        let hash = graine
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325_u64, |acc, b| (acc ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3));
        let teinte = (hash % 3600) as f32 / 3600.0;
        egui::ecolor::Hsva::new(teinte, 0.60, 0.94, 1.0).into()
    }

    /// Mélange linéaire de deux couleurs (`t` = 0 → `a`, 1 → `b`).
    pub fn melanger(a: Color32, b: Color32, t: f32) -> Color32 {
        let t = t.clamp(0.0, 1.0);
        let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
        Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
    }

    /// Même couleur, translucide (utile pour les fonds teintés).
    pub fn translucide(c: Color32, a: u8) -> Color32 {
        Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
    }
}

/// Les ombres, de la plus discrète à la plus haute : ce qui se pose sur
/// la page, ce qui flotte au-dessus d'elle.
pub mod ombre {
    use egui::epaint::Shadow;
    use egui::Color32;

    /// Une carte posée sur la page.
    pub const DOUCE: Shadow = Shadow { offset: [0, 10], blur: 30, spread: 0, color: Color32::from_black_alpha(90) };
    /// Un menu, une bulle : juste au-dessus.
    pub const MENU: Shadow = Shadow { offset: [0, 6], blur: 20, spread: 0, color: Color32::from_black_alpha(120) };
    /// Une fenêtre, au-dessus de tout.
    pub const LEVEE: Shadow = Shadow { offset: [0, 14], blur: 40, spread: 0, color: Color32::from_black_alpha(140) };
}

/// Les polices au-delà du corps d'egui (Ubuntu Light).
pub mod police {
    use egui::{FontFamily, FontId};

    /// La famille d'Ubuntu Medium, installée par [`crate::style::installer`].
    pub const MOYENNE: &str = "ki-moyenne";

    /// Du texte qui pèse : un chiffre clé, un titre. egui n'a pas de
    /// graisse — `.strong()` n'y change que la couleur —, d'où une famille
    /// à part. Dans un contexte où l'allure de ki-ui n'est pas installée
    /// (un test), le corps ordinaire : egui paniquerait sur une famille
    /// inconnue.
    pub fn moyenne(ctx: &egui::Context, taille: f32) -> FontId {
        let famille = FontFamily::Name(MOYENNE.into());
        if ctx.fonts(|f| f.definitions().families.contains_key(&famille)) {
            FontId::new(taille, famille)
        } else {
            FontId::proportional(taille)
        }
    }
}

/// Les marges intérieures (cadres, cartes, bulles), faites des
/// espacements — egui les veut en entiers.
pub mod marge {
    use egui::Margin;

    /// La même marge des quatre côtés.
    pub fn egale(espace: f32) -> Margin {
        Margin::same(espace as i8)
    }

    /// Une marge à gauche et à droite, une autre en haut et en bas.
    pub fn symetrique(horizontale: f32, verticale: f32) -> Margin {
        Margin::symmetric(horizontale as i8, verticale as i8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La base et sa traduction egui disent la même chose, couleur par
    /// couleur — et le hachage des pseudos aussi.
    #[test]
    fn la_palette_egui_suit_la_base() {
        for ((nom, ici), (nom_base, base)) in couleur::NOMMEES.iter().zip(ki_ui_base::couleur::NOMMEES) {
            assert_eq!(*nom, nom_base);
            assert_eq!(ici.to_array(), [base.r, base.g, base.b, base.a], "{nom}");
        }
        for nom in ["Redik_", "drion", "mg4230", ""] {
            let base = ki_ui_base::couleur::pour_pseudo(nom);
            assert_eq!(couleur::pour_pseudo(nom), Color32::from_rgb(base.r, base.g, base.b));
        }
        assert_eq!(couleur::SERIES[0], couleur::ACCENT);
        assert_eq!(espace::M, 8.0);
        assert_eq!(rayon::XL, 12);
    }
}
