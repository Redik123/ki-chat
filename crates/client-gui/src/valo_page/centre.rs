//! La page VALORANT à la place du chat — V9 de PLAN-VALORANT-2.md, dans
//! ⚙ → Bêta d'abord. Une barre d'onglets (Accueil · Classement · Matchs ·
//! Profil · Carte tactique · Esport · Boutique), et sous elle l'onglet
//! choisi ; les salons, les membres et la barre vocale restent autour.
//!
//! L'accueil, le classement, les matchs et le profil sont les pages
//! neuves ; l'esport et la boutique reprennent ceux de la fenêtre.

use ki_ui::jetons::police;

use super::*;
use crate::icons::Icon;

/// Les onglets de la page au centre.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OngletCentre {
    Accueil,
    Classement,
    Matchs,
    Profil,
    Tactique,
    Esport,
    Boutique,
}

impl OngletCentre {
    const TOUS: [(OngletCentre, &'static str); 7] = [
        (OngletCentre::Accueil, "Accueil"),
        (OngletCentre::Classement, "Classement"),
        (OngletCentre::Matchs, "Matchs"),
        (OngletCentre::Profil, "Profil"),
        (OngletCentre::Tactique, "Carte tactique"),
        (OngletCentre::Esport, "Esport"),
        (OngletCentre::Boutique, "Boutique"),
    ];
}

impl PageValo {
    /// L'accueil, en ouvrant la page.
    pub fn vers_accueil(&mut self) {
        self.onglet_centre = OngletCentre::Accueil;
    }

    /// Le profil, en ouvrant une fiche.
    pub fn vers_profil(&mut self) {
        self.onglet_centre = OngletCentre::Profil;
    }

    /// L'accueil se montre : le fil de la soirée doit être frais.
    pub fn sur_l_accueil(&self) -> bool {
        self.onglet_centre == OngletCentre::Accueil
    }

    /// La page entière, dans toute la place qu'on lui donne. Rend si elle
    /// reste ouverte (« Fermer » ou Échap la referment) et ce qu'elle
    /// demande à l'application.
    pub fn centre(
        &mut self,
        ui: &mut Ui,
        cx: &Contexte<'_>,
        fiche: Option<&FicheOuverte>,
        boutique: &mut boutique::Lecteur,
    ) -> (bool, Vec<Demande>) {
        let mut ouvert = true;
        let mut demandes = Vec::new();
        ui.horizontal(|ui| {
            ui.label(RichText::new("VALORANT").font(police::moyenne(ui.ctx(), texte::TITRE_PAGE)).color(TEXT));
            let n = cx.stats.len();
            if n > 0 {
                let s = if n > 1 { "s" } else { "" };
                ui::hint(ui, format!("{n} joueur{s} lié{s}"));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui::button(ui, Icon::Close, "Fermer").on_hover_text("retour au chat (Échap)").clicked() {
                    ouvert = false;
                }
                if ui::button(ui, Icon::Refresh, "Actualiser").clicked() {
                    demandes.push(Demande::Actualiser);
                }
            });
        });
        ui.add_space(espace::S);
        ui.horizontal(|ui| {
            for (onglet, nom) in OngletCentre::TOUS {
                if ui::onglet(ui, self.onglet_centre == onglet, nom).clicked() {
                    match onglet {
                        // La carte vit dans sa propre fenêtre, partagée avec
                        // le salon vocal : l'onglet l'ouvre.
                        OngletCentre::Tactique => demandes.push(Demande::OuvrirTactique),
                        o => self.onglet_centre = o,
                    }
                }
            }
        });
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) && !ui.ctx().egui_wants_keyboard_input() {
            ouvert = false;
        }
        ui.add_space(espace::L);

        let maintenant = maintenant_ms();
        let (lignes, local) = lignes_du_groupe(cx.stats, maintenant);
        let noms = Annuaire::new(cx.stats, cx.membres);
        match self.onglet_centre {
            OngletCentre::Profil => match fiche {
                Some(ouverte) => self.profil_contenu(ui, ouverte, cx, &mut demandes),
                None => {
                    // Sans fiche ouverte, la sienne.
                    match cx.moi.and_then(|moi| cx.stats.iter().find(|f| f.user_id == moi)) {
                        Some(f) => demandes.push(Demande::OuvrirFiche(f.user_id, f.username.clone())),
                        None => ui::hint(ui, "Lie ton compte Riot dans ⚙ → Jeu pour avoir ta fiche ; clic droit sur un membre pour la sienne."),
                    }
                }
            },
            onglet => {
                egui::ScrollArea::vertical().id_salt("valo_centre").auto_shrink([false, false]).show(ui, |ui| {
                    let besoin_des_fiches = matches!(onglet, OngletCentre::Accueil | OngletCentre::Classement | OngletCentre::Matchs);
                    if besoin_des_fiches && local {
                        ui::hint(ui, "serveur d'avant : statistiques sur les cinq derniers matchs");
                        ui.add_space(espace::XS);
                    }
                    match onglet {
                        OngletCentre::Accueil => self.accueil(ui, cx, &lignes, &noms, &mut demandes),
                        OngletCentre::Classement | OngletCentre::Matchs if !cx.recu => {
                            ui.label(RichText::new("demande au serveur…").color(TEXT_DIM));
                        }
                        OngletCentre::Classement => self.onglet_classement(ui, cx, &lignes, &noms, &mut demandes),
                        OngletCentre::Matchs => self.matchs_du_groupe(ui, &lignes, &noms, cx, maintenant, &mut demandes),
                        OngletCentre::Esport => esports_ui(ui, cx.esports),
                        OngletCentre::Boutique => {
                            ui.label(RichText::new("Ma boutique du jour").strong().size(texte::CORPS));
                            ui::hint(
                                ui,
                                "lue dans ton client Riot, sur ce PC, pour toi seul — rien ne part vers le \
                                 serveur ; il faut VALORANT ouvert",
                            );
                            ui.add_space(espace::XS);
                            boutique.ui(ui);
                        }
                        OngletCentre::Profil | OngletCentre::Tactique => {}
                    }
                });
            }
        }
        (ouvert, demandes)
    }

    /// L'onglet Classement : la période, le tableau et les douze médailles
    /// du groupe, puis les records.
    fn onglet_classement(&mut self, ui: &mut Ui, cx: &Contexte<'_>, lignes: &[Ligne], noms: &Annuaire, demandes: &mut Vec<Demande>) {
        let aide = format!(
            "sur {} · un clic sur un en-tête change le tri, sur une ligne ouvre la fiche · coche deux lignes pour les comparer",
            self.periode.titre()
        );
        ui::section(ui, Icon::Crown, "Classement", Some(&aide), |ui| {
            self.choix_de_periode(ui);
            ui.add_space(espace::S);
            self.classement_tableau(ui, lignes, noms, cx, demandes);
            medailles_du_groupe(ui, lignes, noms, cx.moi, demandes, &Medaille::TOUTES);
        });
        let aide = format!("sur {}, {ASSEZ} classés au moins", self.periode.titre());
        ui::section(ui, Icon::Star, "Les records", Some(&aide), |ui| records(ui, lignes, self.periode, cx.rangs));
    }

    /// La période des agrégats du groupe : 7 ou 30 jours.
    pub(super) fn choix_de_periode(&mut self, ui: &mut Ui) {
        let periodes = Periode::TOUTES.map(|p| (p, p.label()));
        ui::pastilles(ui, &mut self.periode, &periodes);
    }
}
