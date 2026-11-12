//! L'onglet « Bêta » des réglages : les fonctions encore en essai, et
//! celles que tout le monde n'utilise pas. Coupées d'origine — on les
//! allume si on en a l'usage, et qui n'en veut pas ne les voit nulle part
//! ailleurs. Aujourd'hui : le Loupedeck Live, l'overlay en jeu, et le
//! bouton de la soundboard (visible d'origine pour qui a déjà des sons).

use ki_ui::jetons::{espace, texte};
use eframe::egui::{self, RichText};

use crate::icons::Icon;
use crate::theme::TEXT_FAINT;
use crate::ui;
use crate::overlay;
use crate::KiApp;

impl KiApp {
    pub(crate) fn onglet_beta(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new(
                "Des fonctions encore en essai, coupées d'origine : allume celles qui te servent. Elles \
                 peuvent changer d'une version à l'autre.",
            )
            .color(TEXT_FAINT)
            .size(texte::COURANT),
        );
        ui.add_space(espace::L);

        ui::section(
            ui,
            Icon::Sliders,
            "Loupedeck Live",
            Some("Ses boutons, ses molettes et ses écrans tactiles, pilotés par ki-chat."),
            |ui| {
                ui::ligne(ui, "Loupedeck", |ui| self.loupedeck_ui(ui));
            },
        );

        ui::section(
            ui,
            Icon::User,
            "Overlay en jeu",
            Some("Qui parle, par-dessus le jeu — sans rien y injecter."),
            |ui| {
                ui::ligne(ui, "Overlay", |ui| {
                    overlay::reglages_ui(ui, &mut self.overlay);
                });
            },
        );

        ui::section(
            ui,
            Icon::Volume,
            "Soundboard",
            Some("Des sons à la touche, entendus par tout le salon vocal."),
            |ui| {
                ui::ligne(ui, "Bouton", |ui| {
                    ui::interrupteur(ui, &mut self.soundboard_visible, "Afficher le bouton « Soundboard »");
                    ui::precision(
                        ui,
                        "Caché, il quitte la barre du bas. Tes sons restent dans leur dossier, et le \
                         Loupedeck peut toujours les jouer.",
                    );
                });
            },
        );
    }
}
