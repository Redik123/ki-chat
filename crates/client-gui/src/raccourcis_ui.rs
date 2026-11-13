//! La fenêtre « Raccourcis clavier » : tout ce que le clavier sait faire dans
//! ki-chat, sur une page. Les raccourcis de la fenêtre s'y changent — on
//! clique la combinaison, on appuie sur la nouvelle ; ceux qui ne changent
//! pas et ceux qui marchent partout, même en jeu, y sont rappelés.
//!
//! Le registre lui-même, ce qu'il accepte et ce qu'il refuse, est dans
//! [`crate::registre`].

use eframe::egui::{self, Key};
use ki_ui::jetons::espace;
use ki_ui::typo::Texte;

use crate::icons::Icon;
use crate::registre::{touche_enfoncee, Action, Combinaison, Groupe, Registre};
use crate::ui::{self, Taille, Ton};
use crate::MicMode;
use crate::KiApp;

const LARGEUR: f32 = 500.0;
const LIGNE: f32 = 30.0;
/// La place laissée à la barre de défilement.
const BARRE: f32 = 14.0;

/// L'état de la fenêtre.
#[derive(Default)]
pub(crate) struct Raccourcis {
    pub(crate) ouverte: bool,
    /// L'action dont on attend la nouvelle combinaison.
    capture: Option<Action>,
    /// La dernière combinaison refusée, et pourquoi.
    refus: Option<(Action, String)>,
}

impl Raccourcis {
    pub(crate) fn basculer(&mut self) {
        *self = Self { ouverte: !self.ouverte, ..Self::default() };
    }

    /// « Appuie sur ta combinaison » : à lire en tête d'image, avant tout le
    /// monde. Rend vrai tant qu'on choisit — le clavier est alors pris en
    /// entier, Échap comprise : elle annule le choix, pas la fenêtre.
    pub(crate) fn lire_la_capture(&mut self, ctx: &egui::Context, registre: &mut Registre) -> bool {
        let Some(action) = self.capture.filter(|_| self.ouverte) else {
            self.capture = None;
            return false;
        };
        let Some((touche, m)) = ctx.input_mut(touche_enfoncee) else { return true };
        let seule = !(m.command || m.ctrl || m.alt || m.shift);
        let (voulue, fini) = match touche {
            Key::Escape if seule => (None, true),
            Key::Delete | Key::Backspace if seule => (Some(None), true),
            _ => (Some(Some(Combinaison::tenue(touche, m))), false),
        };
        self.refus = None;
        match voulue.map(|c| (c, registre.donner(action, c))) {
            Some((Some(c), Err(refus))) => self.refus = Some((action, format!("{} : {}", c.libelle(), refus.phrase()))),
            Some(_) => self.capture = None,
            None => {}
        }
        if fini {
            self.capture = None;
        }
        true
    }
}

/// Une ligne : ce que ça fait à gauche, ce que `a_droite` pose à droite.
fn ligne(ui: &mut egui::Ui, libelle: &str, a_droite: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), LIGNE),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(Texte::Corps.riche(ui, libelle));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), a_droite);
        },
    );
}

/// Une ligne qui ne se change pas ici : la touche, écrite.
fn rappel(ui: &mut egui::Ui, libelle: &str, touches: &str) {
    ligne(ui, libelle, |ui| {
        ui.label(Texte::Corps.gras_ton(ui, Ton::Discret, touches));
    });
}

impl KiApp {
    /// Ouvre les réglages, ou les referme — en rangeant ce qu'ils avaient
    /// lancé, comme la croix.
    pub(crate) fn basculer_les_reglages(&mut self) {
        if self.show_settings {
            self.close_settings();
        } else {
            self.show_settings = true;
            let (i, o) = ki_voice::list_devices();
            self.input_devices = i;
            self.output_devices = o;
        }
    }

    /// La fenêtre des raccourcis, tant qu'elle est ouverte.
    pub(crate) fn raccourcis_window(&mut self, ctx: &egui::Context) {
        if !self.raccourcis.ouverte {
            return;
        }
        let mut ouverte = true;
        let etat = &mut self.raccourcis;
        let registre = &mut self.registre;
        let audio = &self.audio;
        let clip = self.clips.reglages.raccourci.label();
        ki_ui::fenetre::Fenetre::new("raccourcis", "Raccourcis clavier")
            .ouverte(&mut ouverte)
            .largeur(LARGEUR)
            .taille_min(380.0, 260.0)
            .haute()
            .centree()
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    // La barre de défilement se pose sur le bord droit : les
                    // combinaisons s'arrêtent avant.
                    ui.set_width(ui.available_width() - BARRE);
                    ui::hint(ui, "Clique une combinaison pour la changer.");
                    for groupe in Groupe::TOUS {
                        ui.add_space(espace::M);
                        ui::section_label(ui, groupe.titre());
                        for action in Action::TOUTES.into_iter().filter(|a| a.groupe() == groupe) {
                            ligne(ui, action.libelle(), |ui| {
                                if etat.capture == Some(action) {
                                    if ui::Bouton::icone_seule(Icon::Close).cote(24.0).bulle("Annuler (Échap)").show(ui).clicked() {
                                        etat.capture = None;
                                        etat.refus = None;
                                    }
                                    ui.label(Texte::Corps.riche_ton(ui, Ton::Accent, "appuie sur ta combinaison…"));
                                    return;
                                }
                                let actuelle = registre.libelle(action);
                                let bulle = "Changer — puis Suppr pour n'en mettre aucune";
                                let bouton = ui::Bouton::new(actuelle.as_deref().unwrap_or("aucune")).taille(Taille::S).bulle(bulle);
                                if bouton.show(ui).clicked() {
                                    etat.capture = Some(action);
                                    etat.refus = None;
                                }
                                if registre.changee(action) {
                                    let origine = format!("Remettre {}", action.origine().libelle());
                                    if ui::Bouton::icone_seule(Icon::Refresh).cote(24.0).bulle(&origine).show(ui).clicked() {
                                        etat.refus = registre.remettre(action).err().map(|r| {
                                            (action, format!("{} : {}", action.origine().libelle(), r.phrase()))
                                        });
                                    }
                                }
                            });
                            if let Some((_, pourquoi)) = etat.refus.as_ref().filter(|(a, _)| *a == action) {
                                ui.label(Texte::Aide.riche_ton(ui, Ton::Alerte, pourquoi));
                                ui.add_space(espace::XS);
                            }
                        }
                    }
                    if !registre.rien_de_change() {
                        ui.add_space(espace::M);
                        if ui::Bouton::new("Tout remettre comme au départ").icone(Icon::Refresh).taille(Taille::S).show(ui).clicked() {
                            registre.tout_remettre();
                            *etat = Raccourcis { ouverte: true, ..Raccourcis::default() };
                        }
                    }

                    ui.add_space(espace::L);
                    ui::section_label(ui, "Toujours les mêmes");
                    rappel(ui, "Envoyer le message", "Entrée");
                    rappel(ui, "Aller à la ligne", "Maj+Entrée");
                    rappel(ui, "Modifier son dernier message", "↑ dans la saisie vide");
                    rappel(ui, "Prendre une suggestion", "Tab ou Entrée");
                    rappel(ui, "Remonter, redescendre dans le fil", "Page haut, Page bas");
                    rappel(ui, "Fermer, annuler, revenir en bas du fil", "Échap");
                    rappel(ui, "Taille de l'interface", "Ctrl +, Ctrl −, Ctrl 0");

                    ui.add_space(espace::L);
                    ui::section_label(ui, "Partout, même en jeu");
                    let ou_aucune = |touche: Option<crate::ptt::PttKey>| touche.map_or("aucune", |t| t.label());
                    if audio.mode == MicMode::Ptt {
                        rappel(ui, "Parler (tant qu'on la tient)", audio.ptt_key.label());
                    }
                    rappel(ui, "Couper ou rouvrir le micro", ou_aucune(audio.hotkey_micro));
                    rappel(ui, "Ne plus entendre, ou entendre", ou_aucune(audio.hotkey_sourd));
                    rappel(ui, "Changeur de voix", ou_aucune(audio.hotkey_changeur));
                    rappel(ui, "Prendre un clip", &clip);
                    ui.add_space(espace::XS);
                    ui::hint(ui, "Ceux-là se choisissent dans les réglages : onglets Audio et Clips.");
                });
            });
        if !ouverte {
            self.raccourcis = Raccourcis::default();
        }
    }
}
