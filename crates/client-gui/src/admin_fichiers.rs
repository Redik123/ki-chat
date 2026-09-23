//! Admin → Fichiers : les fichiers partagés sur le serveur — qui les a
//! envoyés, où, ce qu'ils pèsent —, filtrés par membre et par genre, triés,
//! et supprimés en lot, avec les messages du chat qui les partagent.
//!
//! Né d'un soir où un membre a envoyé quinze vidéos d'affilée en croyant que
//! ça ne marchait pas : les retirer une à une du fil, sans savoir si elles
//! quittaient vraiment le disque du serveur, c'était un quart d'heure.
//! Ici : « envoyé par Kevin », tout cocher, supprimer.
//!
//! Tout ce qu'il sait vient du serveur (`AdminFichiers`) ; la permission
//! est « Supprimer les messages » — c'est de la modération.

use std::cmp::Reverse;
use std::collections::HashSet;

use eframe::egui::{self, RichText};
use ki_protocol::{ClientMsg, FichierPartage};

use crate::icons::Icon;
use crate::theme::{self, DANGER, TEXT, TEXT_DIM, TEXT_FAINT};
use crate::ui::{self, Tone};

/// Ce que l'onglet demande à l'application.
pub enum Action {
    Envoyer(ClientMsg),
    /// Ouvrir un fichier dans la visionneuse : son chemin sur le serveur, et
    /// si c'est une vidéo (sinon une image).
    Voir { chemin: String, video: bool },
    /// Copier le lien d'un fichier (son chemin sur le serveur).
    Copier(String),
}

/// Le filtre par genre.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Genre {
    Tout,
    Videos,
    Images,
    Autres,
}

impl Genre {
    const TOUS: [Genre; 4] = [Genre::Tout, Genre::Videos, Genre::Images, Genre::Autres];

    fn label(self) -> &'static str {
        match self {
            Genre::Tout => "Tout",
            Genre::Videos => "Vidéos",
            Genre::Images => "Images",
            Genre::Autres => "Autres",
        }
    }

    fn garde(self, f: &FichierPartage) -> bool {
        match self {
            Genre::Tout => true,
            Genre::Videos => f.genre == "video",
            Genre::Images => f.genre == "image",
            Genre::Autres => f.genre != "video" && f.genre != "image",
        }
    }
}

/// L'ordre de la liste.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tri {
    Recents,
    Lourds,
}

pub struct Fichiers {
    liste: Vec<FichierPartage>,
    total_octets: u64,
    plafond_octets: u64,
    tronque: bool,
    /// Demandée, pas encore reçue.
    en_attente: bool,
    selection: HashSet<String>,
    /// Le filtre « envoyé par » ; `None` : tout le monde.
    auteur: Option<String>,
    genre: Genre,
    tri: Tri,
    /// Retirer aussi du chat les messages qui partagent les fichiers
    /// supprimés — un lien mort n'a rien à y faire.
    avec_messages: bool,
    /// La demande de confirmation est affichée.
    confirmer: bool,
}

impl Default for Fichiers {
    fn default() -> Self {
        Self::new()
    }
}

impl Fichiers {
    pub fn new() -> Self {
        Self {
            liste: Vec::new(),
            total_octets: 0,
            plafond_octets: 0,
            tronque: false,
            en_attente: false,
            selection: HashSet::new(),
            auteur: None,
            genre: Genre::Tout,
            tri: Tri::Recents,
            avec_messages: true,
            confirmer: false,
        }
    }

    /// La liste est demandée : ouverture de l'onglet, rafraîchissement.
    pub fn demander(&mut self) -> ClientMsg {
        self.en_attente = true;
        ClientMsg::AdminListFichiers
    }

    /// La liste reçue. La sélection ne garde que ce qui existe encore, et
    /// le filtre « envoyé par » un membre qui a encore des fichiers.
    pub fn recevoir(&mut self, liste: Vec<FichierPartage>, total_octets: u64, plafond_octets: u64, tronque: bool) {
        self.selection.retain(|id| liste.iter().any(|f| &f.id == id));
        if self.auteur.as_ref().is_some_and(|a| !liste.iter().any(|f| &f.auteur == a)) {
            self.auteur = None;
        }
        self.liste = liste;
        self.total_octets = total_octets;
        self.plafond_octets = plafond_octets;
        self.tronque = tronque;
        self.en_attente = false;
        self.confirmer = false;
    }

    /// Les fichiers affichés : les filtres, puis le tri.
    fn visibles(&self) -> Vec<FichierPartage> {
        let mut v: Vec<FichierPartage> = self
            .liste
            .iter()
            .filter(|f| self.genre.garde(f))
            .filter(|f| self.auteur.as_ref().is_none_or(|a| &f.auteur == a))
            .cloned()
            .collect();
        match self.tri {
            Tri::Recents => v.sort_by_key(|f| Reverse(f.date_ms)),
            Tri::Lourds => v.sort_by_key(|f| Reverse(f.octets)),
        }
        v
    }

    /// Les expéditeurs, du plus lourd au plus léger : (nom, fichiers,
    /// octets). Un nom vide rassemble ce dont on ne sait pas qui l'a envoyé.
    fn expediteurs(&self) -> Vec<(String, usize, u64)> {
        let mut par: Vec<(String, usize, u64)> = Vec::new();
        for f in &self.liste {
            match par.iter_mut().find(|(nom, _, _)| *nom == f.auteur) {
                Some((_, n, o)) => {
                    *n += 1;
                    *o += f.octets;
                }
                None => par.push((f.auteur.clone(), 1, f.octets)),
            }
        }
        par.sort_by_key(|(_, _, o)| Reverse(*o));
        par
    }

    /// L'onglet, et ce qu'on y a demandé.
    pub fn onglet(&mut self, ui: &mut egui::Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        ui.horizontal(|ui| {
            ui::group_title(ui, Icon::Paperclip, "Fichiers partagés");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui::icon_button(ui, Icon::Refresh, "Relire la liste").clicked() {
                    actions.push(Action::Envoyer(self.demander()));
                }
            });
        });
        let plafond = match self.plafond_octets {
            0 => String::new(),
            p => format!(" sur {}", taille_lisible(p)),
        };
        let mut resume = format!(
            "{} · {}{plafond}",
            nombre_de_fichiers(self.liste.len()),
            taille_lisible(self.total_octets)
        );
        if self.tronque {
            resume.push_str(" — les plus récents seulement ; supprime, et la suite apparaîtra");
        }
        ui::hint(ui, &resume);
        if self.en_attente && self.liste.is_empty() {
            ui.add_space(8.0);
            ui::hint(ui, "lecture du stock…");
            return actions;
        }
        if self.liste.is_empty() {
            ui.add_space(8.0);
            ui::hint(ui, "aucun fichier partagé sur le serveur");
            return actions;
        }

        // Les filtres : qui, quoi, dans quel ordre.
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Envoyé par").color(TEXT_DIM).size(12.5));
            let choisi = match &self.auteur {
                None => "tout le monde".to_string(),
                Some(a) => nom_affiche(a),
            };
            egui::ComboBox::from_id_salt("fichiers_auteur")
                .width(200.0)
                .selected_text(choisi)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.auteur, None, "tout le monde");
                    for (nom, n, octets) in self.expediteurs() {
                        let libelle =
                            format!("{} — {}, {}", nom_affiche(&nom), nombre_de_fichiers(n), taille_lisible(octets));
                        ui.selectable_value(&mut self.auteur, Some(nom), libelle);
                    }
                });
        });
        ui.horizontal_wrapped(|ui| {
            for g in Genre::TOUS {
                if ui.selectable_label(self.genre == g, g.label()).clicked() {
                    self.genre = g;
                }
            }
            ui.separator();
            if ui.selectable_label(self.tri == Tri::Recents, "Récents").clicked() {
                self.tri = Tri::Recents;
            }
            if ui.selectable_label(self.tri == Tri::Lourds, "Plus lourds").clicked() {
                self.tri = Tri::Lourds;
            }
        });

        // La sélection.
        let visibles = self.visibles();
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if ui.small_button(format!("Tout cocher ({})", visibles.len())).clicked() {
                self.selection.extend(visibles.iter().map(|f| f.id.clone()));
            }
            if !self.selection.is_empty() && ui.small_button("Rien").clicked() {
                self.selection.clear();
                self.confirmer = false;
            }
        });
        ui.add_space(4.0);
        ui::hairline(ui);

        // La liste.
        if visibles.is_empty() {
            ui.add_space(6.0);
            ui::hint(ui, "rien ne correspond à ces filtres");
        }
        for f in &visibles {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let mut coche = self.selection.contains(&f.id);
                if ui.checkbox(&mut coche, "").changed() {
                    if coche {
                        self.selection.insert(f.id.clone());
                    } else {
                        self.selection.remove(&f.id);
                    }
                }
                let (icone, couleur) = match f.genre.as_str() {
                    "video" => (Icon::Film, theme::ACCENT),
                    "image" => (Icon::Screen, theme::INFO),
                    _ => (Icon::Paperclip, TEXT_DIM),
                };
                ui::glyph(ui, icone, 16.0, couleur);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        let nom = if f.nom.is_empty() { f.id.as_str() } else { f.nom.as_str() };
                        ui.add(
                            egui::Label::new(
                                RichText::new(ki_protocol::safe_display(nom, 80)).color(TEXT).size(13.0).strong(),
                            )
                            .truncate(),
                        );
                        ui.label(RichText::new(taille_lisible(f.octets)).color(TEXT_DIM).size(12.0));
                    });
                    ui.horizontal_wrapped(|ui| {
                        let qui = if f.auteur.is_empty() {
                            RichText::new("expéditeur inconnu").color(TEXT_FAINT)
                        } else {
                            RichText::new(nom_affiche(&f.auteur)).color(theme::color_for(&f.auteur))
                        };
                        ui.label(qui.size(11.5));
                        let mut details: Vec<String> = f.salons.iter().map(|s| format!("#{}", ki_protocol::safe_display(s, 40))).collect();
                        if f.date_ms > 0 {
                            details.push(crate::il_y_a(f.date_ms));
                        }
                        details.push(match f.messages {
                            0 => "dans aucun message".to_string(),
                            1 => "1 message".to_string(),
                            n => format!("{n} messages"),
                        });
                        ui.label(RichText::new(format!("· {}", details.join(" · "))).color(TEXT_FAINT).size(11.5));
                        if !f.chemin.is_empty() {
                            if (f.genre == "video" || f.genre == "image") && ui.small_button("voir").clicked() {
                                actions.push(Action::Voir { chemin: f.chemin.clone(), video: f.genre == "video" });
                            }
                            if ui.small_button("copier le lien").clicked() {
                                actions.push(Action::Copier(f.chemin.clone()));
                            }
                        }
                    });
                });
            });
        }

        // La suppression, avec sa confirmation.
        ui.add_space(8.0);
        ui::hairline(ui);
        ui.add_space(6.0);
        let choisis: Vec<&FichierPartage> = self.liste.iter().filter(|f| self.selection.contains(&f.id)).collect();
        let (n, octets) = (choisis.len(), choisis.iter().map(|f| f.octets).sum::<u64>());
        let messages: u32 = choisis.iter().map(|f| f.messages).sum();
        if n == 0 {
            ui::hint(ui, "coche des fichiers pour les supprimer");
            self.confirmer = false;
            return actions;
        }
        ui.label(
            RichText::new(format!("{} sélectionné(s) · {}", n, taille_lisible(octets)))
                .color(TEXT)
                .size(13.0),
        );
        ui.checkbox(
            &mut self.avec_messages,
            match messages {
                0 => "retirer aussi les messages qui les partagent".to_string(),
                m => format!("retirer aussi les {m} message(s) qui les partagent"),
            },
        );
        ui.add_space(4.0);
        if !self.confirmer {
            if ui::tinted_button(ui, Some(Icon::Trash), "Supprimer la sélection", Tone::Danger).clicked() {
                self.confirmer = true;
            }
        } else {
            ui.label(
                RichText::new(format!(
                    "Supprimer {} ({}) du serveur ? C'est définitif, pour tout le monde{}.",
                    nombre_de_fichiers(n),
                    taille_lisible(octets),
                    if self.avec_messages && messages > 0 { ", messages compris" } else { "" }
                ))
                .color(DANGER)
                .size(12.5),
            );
            ui.horizontal(|ui| {
                if ui::tinted_button(ui, Some(Icon::Trash), "Oui, supprimer", Tone::Danger).clicked() {
                    let mut ids: Vec<String> = self.selection.iter().cloned().collect();
                    ids.sort();
                    for lot in ids.chunks(ki_protocol::FICHIERS_PAR_LOT) {
                        actions.push(Action::Envoyer(ClientMsg::AdminSupprimerFichiers {
                            ids: lot.to_vec(),
                            messages: self.avec_messages,
                        }));
                    }
                    self.selection.clear();
                    self.confirmer = false;
                    self.en_attente = true;
                }
                if ui::button(ui, Icon::Close, "Annuler").clicked() {
                    self.confirmer = false;
                }
            });
        }
        actions
    }
}

/// Un pseudo à l'écran — « expéditeur inconnu » s'il manque.
fn nom_affiche(nom: &str) -> String {
    if nom.is_empty() {
        "expéditeur inconnu".to_string()
    } else {
        ki_protocol::safe_display(nom, ki_protocol::MAX_USERNAME)
    }
}

/// « 1 fichier », « 15 fichiers ».
fn nombre_de_fichiers(n: usize) -> String {
    match n {
        0 => "aucun fichier".to_string(),
        1 => "1 fichier".to_string(),
        n => format!("{n} fichiers"),
    }
}

/// Une taille lisible, virgule à la française : « 850 Ko », « 12,4 Mo »,
/// « 1,2 Go ».
pub fn taille_lisible(octets: u64) -> String {
    const KO: f64 = 1024.0;
    let o = octets as f64;
    let texte = if o < KO * KO {
        format!("{:.0} Ko", (o / KO).max(if octets > 0 { 1.0 } else { 0.0 }))
    } else if o < KO * KO * KO {
        format!("{:.1} Mo", o / (KO * KO))
    } else {
        format!("{:.1} Go", o / (KO * KO * KO))
    };
    texte.replace('.', ",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fichier(id: &str, auteur: &str, genre: &str, octets: u64, date_ms: u64) -> FichierPartage {
        FichierPartage {
            id: id.into(),
            nom: format!("{id}.bin"),
            octets,
            date_ms,
            genre: genre.into(),
            auteur: auteur.into(),
            ..Default::default()
        }
    }

    #[test]
    fn une_taille_se_lit_a_la_francaise() {
        assert_eq!(taille_lisible(0), "0 Ko");
        assert_eq!(taille_lisible(200), "1 Ko");
        assert_eq!(taille_lisible(850 * 1024), "850 Ko");
        assert_eq!(taille_lisible(13_002_342), "12,4 Mo");
        assert_eq!(taille_lisible(1_288_490_189), "1,2 Go");
    }

    /// Les filtres gardent ce qu'il faut, le tri range, les expéditeurs se
    /// comptent — et une liste reçue ne garde de la sélection que ce qui
    /// existe encore.
    #[test]
    fn les_filtres_le_tri_et_la_selection() {
        let mut f = Fichiers::new();
        f.recevoir(
            vec![
                fichier("a", "kevin", "video", 50, 1),
                fichier("b", "kevin", "video", 300, 2),
                fichier("c", "lea", "image", 10, 3),
                fichier("d", "", "autre", 5, 4),
            ],
            365,
            0,
            false,
        );
        assert_eq!(f.visibles().iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), vec!["d", "c", "b", "a"]);
        f.auteur = Some("kevin".into());
        assert_eq!(f.visibles().len(), 2);
        f.tri = Tri::Lourds;
        assert_eq!(f.visibles()[0].id, "b");
        f.auteur = None;
        f.genre = Genre::Images;
        assert_eq!(f.visibles().iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), vec!["c"]);
        f.genre = Genre::Autres;
        assert_eq!(f.visibles()[0].id, "d");
        let par = f.expediteurs();
        assert_eq!(par[0], ("kevin".to_string(), 2, 350));
        assert_eq!(par.len(), 3);

        // Après une suppression, la liste revient sans « b » : la sélection
        // l'oublie, et le filtre sur un membre sans fichier tombe.
        f.selection.extend(["a".to_string(), "b".to_string()]);
        f.auteur = Some("lea".into());
        f.recevoir(vec![fichier("a", "kevin", "video", 50, 1)], 50, 0, false);
        assert_eq!(f.selection.len(), 1);
        assert!(f.selection.contains("a"));
        assert_eq!(f.auteur, None);
    }
}
