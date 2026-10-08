//! Le thème de ki-chat : ce qui vient de ki-ui — la palette, l'allure
//! d'ensemble — sous ses noms de toujours, et ce qui n'appartient qu'à
//! ki-chat : la couleur d'un membre selon son rôle, l'icône de la fenêtre.

use eframe::egui::{self, Color32};

// ---------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------

/// La palette vit dans les jetons de ki-ui : une seule source pour toutes
/// les applis ki-*. Les noms restent ceux de toujours.
pub use ki_ui::jetons::couleur::*;
/// Les outils de couleur de ki-ui, sous leurs noms de toujours.
pub use ki_ui::jetons::couleur::{melanger as mix, pour_pseudo as color_for, translucide as alpha};

/// Couleur d'un membre : celle que son rôle lui donne, sinon le hachage de
/// son pseudo.
///
/// Le repli n'est pas un pis-aller : un serveur sans rôles colorés doit
/// continuer à afficher des pseudos distincts, comme avant.
pub fn member_color(color: Option<u32>, username: &str) -> Color32 {
    match color {
        Some(rgb) => Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8),
        None => color_for(username),
    }
}

/// L'allure de ki-chat — polices, palette, espacements, thème sombre —,
/// installée par ki-ui : la même pour toutes les applis ki-*.
pub use ki_ui::style::installer as install;

// ---------------------------------------------------------------------
// Icône de fenêtre
// ---------------------------------------------------------------------

/// Icône de fenêtre (barre des tâches, alt-tab) : le même pictogramme que
/// celui gravé dans l'exécutable, rendu pixel par pixel par `appicon` — pas
/// de fichier à embarquer, pas de carré blanc par défaut.
pub fn app_icon() -> egui::IconData {
    const S: u32 = 64;
    egui::IconData {
        rgba: crate::appicon::render(S),
        width: S,
        height: S,
    }
}
