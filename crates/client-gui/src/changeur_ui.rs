//! La section « Changeur de voix » de la page Casque : l'interrupteur, les
//! personnages, où part la voix changée, un raccourci pour basculer — et, en
//! mode studio, chaque réglage à la main.

use eframe::egui::{self, RichText};
use ki_voice::changeur::{ReglagesChangeur, PERSONNAGES, VERS_JEUX, VERS_KICHAT, VERS_TOUT};

use crate::icons::Icon;
use crate::ptt::PttKey;
use crate::reglages_audio::curseur;
use crate::theme::{SPEAK, TEXT, TEXT_DIM, TEXT_FAINT, WARN};
use crate::ui;
use crate::KiApp;

/// Les réglages du changeur tels que les préférences les rangent.
pub(crate) fn ecrire_changeur(r: &ReglagesChangeur) -> String {
    format!(
        "h={};so={};d={};r={};rhz={};t={};s={};v={};e={};ems={};rv={};rt={}",
        r.hauteur,
        r.sous_octave,
        r.distorsion,
        r.robot,
        r.robot_hz,
        r.talkie as u8,
        r.sombre_hz,
        r.voile,
        r.echo,
        r.echo_ms,
        r.reverb,
        r.reverb_taille
    )
}

/// L'inverse d'[`ecrire_changeur`] ; ce qui manque garde sa valeur neutre.
pub(crate) fn lire_changeur(texte: &str) -> ReglagesChangeur {
    let mut r = ReglagesChangeur::default();
    for champ in texte.split(';') {
        let Some((cle, valeur)) = champ.split_once('=') else { continue };
        let Ok(v) = valeur.trim().parse::<f32>() else { continue };
        match cle.trim() {
            "h" => r.hauteur = v,
            "so" => r.sous_octave = v,
            "d" => r.distorsion = v,
            "r" => r.robot = v,
            "rhz" => r.robot_hz = v,
            "t" => r.talkie = v != 0.0,
            "s" => r.sombre_hz = v,
            "v" => r.voile = v,
            "e" => r.echo = v,
            "ems" => r.echo_ms = v,
            "rv" => r.reverb = v,
            "rt" => r.reverb_taille = v,
            _ => {}
        }
    }
    r.bornes()
}

/// Le personnage dont ces réglages sont exactement ceux, s'il y en a un.
pub(crate) fn personnage_de(r: &ReglagesChangeur) -> Option<usize> {
    PERSONNAGES.iter().position(|(_, _, fabrique)| fabrique() == *r)
}

/// Un curseur de 0 à 100 % sur une valeur de 0 à 1. Rend vrai au changement.
fn pourcent(ui: &mut egui::Ui, v: &mut f32) -> bool {
    let mut p = *v * 100.0;
    if curseur(ui, &mut p, 0.0..=100.0, " %", Some(1.0)) {
        *v = p / 100.0;
        true
    } else {
        false
    }
}

impl KiApp {
    /// La section du changeur. `apply` : un réglage du moteur a changé.
    pub(crate) fn changeur_ui(&mut self, ui: &mut egui::Ui, apply: &mut bool) {
        ui::section(
            ui,
            Icon::Star,
            "Changeur de voix",
            Some(
                "Un personnage pour ta voix, dans ki-chat, dans les jeux, ou les deux. \
                 Essaie-le avec « M'écouter » (onglet Audio).",
            ),
            |ui| {
                ui::ligne(ui, "Changeur", |ui| {
                    if ui::interrupteur(ui, &mut self.changeur_actif, "Changer ma voix").changed() {
                        *apply = true;
                    }
                    if self.changeur_actif {
                        let nom = personnage_de(&self.changeur).map(|i| PERSONNAGES[i].0).unwrap_or("réglage perso");
                        ui.add_space(4.0);
                        ui.horizontal(|ui| ui::status_dot(ui, SPEAK, &format!("allumé : {nom}"), 10.0));
                    }
                });
                ui::ligne(ui, "Personnage", |ui| {
                    let mut choix = personnage_de(&self.changeur).unwrap_or(usize::MAX);
                    let options: Vec<(usize, &str)> =
                        PERSONNAGES.iter().enumerate().map(|(i, (nom, _, _))| (i, *nom)).collect();
                    if ui::segmente(ui, &mut choix, &options) {
                        if let Some((_, _, fabrique)) = PERSONNAGES.get(choix) {
                            self.changeur = fabrique();
                            // Choisir un personnage, c'est vouloir l'entendre.
                            self.changeur_actif = true;
                            *apply = true;
                        }
                    }
                    let description = PERSONNAGES
                        .get(choix)
                        .map(|(_, d, _)| *d)
                        .unwrap_or("Ton propre mélange — il se règle en mode studio.");
                    ui::precision(ui, description);
                });
                ui::ligne(ui, "Où", |ui| {
                    if ui::segmente(
                        ui,
                        &mut self.changeur_vers,
                        &[(VERS_TOUT, "ki-chat et jeux"), (VERS_JEUX, "Jeux seulement"), (VERS_KICHAT, "ki-chat seulement")],
                    ) {
                        *apply = true;
                    }
                    ui::precision(
                        ui,
                        "Les jeux le reçoivent par le micro pour les jeux (plus haut). « M'écouter » \
                         le fait entendre dans tous les cas.",
                    );
                });
                ui::ligne(ui, "Raccourci", |ui| {
                    let actuel = self.hotkey_changeur.map(|k| k.label()).unwrap_or("aucun");
                    egui::ComboBox::from_id_salt("hotkey_changeur")
                        .width(120.0)
                        .selected_text(RichText::new(actuel).color(TEXT))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.hotkey_changeur, None, "aucun");
                            for key in PttKey::ALL {
                                ui.selectable_value(&mut self.hotkey_changeur, Some(key), key.label());
                            }
                        });
                    let prise = [self.hotkey_micro, self.hotkey_sourd].contains(&self.hotkey_changeur)
                        || (self.hotkey_changeur.is_some() && self.hotkey_changeur == Some(self.ptt_key));
                    if self.hotkey_changeur.is_some() && prise {
                        ui.label(RichText::new("cette touche sert déjà ailleurs").color(WARN).size(11.5));
                    }
                    ui::precision(ui, "Une pression allume ou coupe le changeur, même en jeu.");
                });

                if !self.mode_studio {
                    return;
                }
                // --- Le détail, en mode studio ---
                ui.add_space(4.0);
                ui.label(RichText::new("Réglages").color(TEXT).size(13.0).strong());
                ui.label(
                    RichText::new("Toucher un réglage fait ton propre personnage.").color(TEXT_FAINT).size(11.5),
                );
                ui.add_space(4.0);
                let avant = self.changeur;
                let r = &mut self.changeur;
                ui::ligne(ui, "Hauteur", |ui| {
                    curseur(ui, &mut r.hauteur, -12.0..=12.0, " demi-tons", Some(0.5));
                    ui::precision(ui, "Plus grave à gauche, plus aiguë à droite. 12 : une octave.");
                });
                ui::ligne(ui, "Couche grave", |ui| {
                    pourcent(ui, &mut r.sous_octave);
                    ui::precision(ui, "Une deuxième voix, une octave plus bas : la carrure d'un monstre.");
                });
                ui::ligne(ui, "Distorsion", |ui| {
                    pourcent(ui, &mut r.distorsion);
                });
                ui::ligne(ui, "Robot", |ui| {
                    pourcent(ui, &mut r.robot);
                    if r.robot > 0.005 {
                        curseur(ui, &mut r.robot_hz, 10.0..=300.0, " Hz", Some(1.0));
                    }
                });
                ui::ligne(ui, "Talkie", |ui| {
                    ui::interrupteur(ui, &mut r.talkie, "radio de poche");
                });
                ui::ligne(ui, "Sombre", |ui| {
                    let mut khz = r.sombre_hz / 1000.0;
                    if curseur(ui, &mut khz, 1.0..=20.0, " kHz", Some(0.5)) {
                        r.sombre_hz = khz * 1000.0;
                    }
                    ui::precision(ui, "Coupe les aigus au-dessus ; 20 kHz : rien.");
                });
                ui::ligne(ui, "Voile", |ui| {
                    pourcent(ui, &mut r.voile);
                    ui::precision(ui, "Un chatoiement lent, la voix d'outre-tombe.");
                });
                ui::ligne(ui, "Écho", |ui| {
                    pourcent(ui, &mut r.echo);
                    if r.echo > 0.005 {
                        curseur(ui, &mut r.echo_ms, 40.0..=900.0, " ms", Some(10.0));
                    }
                });
                ui::ligne(ui, "Réverbération", |ui| {
                    pourcent(ui, &mut r.reverb);
                    if r.reverb > 0.005 {
                        let mut taille = r.reverb_taille * 100.0;
                        if curseur(ui, &mut taille, 0.0..=100.0, " % de pièce", Some(1.0)) {
                            r.reverb_taille = taille / 100.0;
                        }
                    }
                });
                if *r != avant {
                    *r = r.bornes();
                    *apply = true;
                }
                if !self.changeur_actif {
                    ui.label(
                        RichText::new("Le changeur est coupé : allume-le pour entendre ces réglages.")
                            .color(TEXT_DIM)
                            .size(11.5),
                    );
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_reglages_font_l_aller_retour() {
        for (_, _, fabrique) in PERSONNAGES {
            let r = fabrique();
            assert_eq!(lire_changeur(&ecrire_changeur(&r)), r);
        }
        assert_eq!(lire_changeur(""), ReglagesChangeur::default());
        assert_eq!(lire_changeur("h=99"), ReglagesChangeur { hauteur: 12.0, ..Default::default() });
    }

    #[test]
    fn un_personnage_se_reconnait() {
        assert_eq!(personnage_de(&(PERSONNAGES[0].2)()), Some(0));
        assert_eq!(personnage_de(&ReglagesChangeur { hauteur: 1.5, ..Default::default() }), None);
    }
}
