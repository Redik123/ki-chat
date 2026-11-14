//! La page VALORANT dans sa propre fenêtre système, à poser sur un second
//! écran pendant qu'on joue sur le premier.
//!
//! Une fenêtre « différée » d'egui, comme celle du stream
//! (`regard_detache`) : elle a ses propres images et vit seule. C'est ce
//! qu'il faut ici — depuis eframe 0.34, quand la fenêtre principale est
//! réduite, plus aucune interface n'y est dessinée, et une fenêtre
//! « immédiate », dessinée depuis la sienne, se figerait avec elle : au
//! moment même où l'on regarde la page à côté du jeu.
//!
//! Vivre seule, c'est ne rien emprunter à l'application. La fenêtre a donc
//! sa copie de l'état de la page ([`PageValo`]) et le dernier
//! [`Instantane`] de ce qu'elle lit. L'application et elle tournent sur le
//! même fil, l'une après l'autre, jamais ensemble :
//!
//! - au début de chacune de ses images, l'application reprend ce que la
//!   fenêtre a fait depuis la dernière ([`Fenetre::reprendre`]) : l'état de
//!   la page si elle a été dessinée, ses demandes, sa fermeture ;
//! - à la fin, elle lui rend son état de la page et un instantané neuf
//!   ([`Fenetre::alimenter`]), et la réveille s'il y a du nouveau ;
//! - la fenêtre réveille l'application quand elle a changé l'état de la
//!   page ou demandé quelque chose — un onglet cliqué, c'est une demande au
//!   serveur à faire partir.
//!
//! L'application ne perd jamais son état de la page : ce sont des copies
//! qui passent, dans un sens puis dans l'autre.

use std::sync::{Arc, Mutex};

use eframe::egui;
use ki_ui::jetons::{espace, marge};

use crate::place::{Place, Rendu};
use crate::valo::{Demande, Instantane, PageValo};
use crate::{boutique, theme};

/// La fenêtre de la page détachée, pour qui veut la réveiller, la ramener
/// devant ou la photographier.
pub fn id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("valo-detachee")
}

/// La taille de la fenêtre la première fois, et la plus petite qu'elle
/// accepte : celle où la page tient encore, colonnes masquées.
const TAILLE: [f32; 2] = [1100.0, 760.0];
const TAILLE_MIN: [f32; 2] = [640.0, 420.0];

/// Ce que la fenêtre a fait depuis la dernière image de l'application.
#[derive(Default)]
pub struct Retour {
    /// L'état de la page tel que la fenêtre l'a laissé, si elle a été
    /// dessinée : à reprendre avant d'y toucher.
    pub page: Option<PageValo>,
    /// Ce que la page a demandé à l'application.
    pub demandes: Vec<Demande>,
    /// « Fermer », ou la croix de la fenêtre.
    pub fermer: bool,
}

/// La fenêtre détachée : vit tant que la page est ouverte et détachée.
pub struct Fenetre {
    etat: Arc<Mutex<Etat>>,
    icone: Arc<egui::IconData>,
}

/// Ce que la fenêtre et l'application se partagent. Le verrou n'est jamais
/// disputé : elles tournent l'une après l'autre.
struct Etat {
    // De l'application vers la fenêtre.
    instantane: Option<Instantane>,
    page: PageValo,
    boutique: boutique::Lecteur,
    // De la fenêtre vers l'application.
    dessinee: bool,
    demandes: Vec<Demande>,
    fermer: bool,
    // La place de la fenêtre : à rendre à sa naissance, puis relevée.
    place_a_rendre: Option<Rendu>,
    place: Option<Place>,
    barre_sombre: bool,
}

impl Fenetre {
    /// `place` : celle qu'avait la fenêtre la dernière fois ; rendue si elle
    /// touche encore un écran, sa taille seule sinon.
    pub fn new(page: PageValo, boutique: boutique::Lecteur, place: Option<Place>) -> Self {
        let etat = Etat {
            instantane: None,
            page,
            boutique,
            dessinee: false,
            demandes: Vec::new(),
            fermer: false,
            place_a_rendre: place.map(|p| Rendu::depuis(p, p.sur_un_ecran())),
            place,
            barre_sombre: false,
        };
        Self { etat: Arc::new(Mutex::new(etat)), icone: Arc::new(theme::app_icon()) }
    }

    /// Au début d'une image de l'application : ce que la fenêtre a fait
    /// depuis la dernière.
    pub fn reprendre(&self) -> Retour {
        let mut e = self.etat.lock().unwrap();
        Retour {
            page: std::mem::take(&mut e.dessinee).then(|| e.page.clone()),
            demandes: std::mem::take(&mut e.demandes),
            fermer: std::mem::take(&mut e.fermer),
        }
    }

    /// À la fin d'une image de l'application : l'état de la page tel
    /// qu'elle le tient, et ce que la page lit à présent. Rend vrai s'il y
    /// a du nouveau pour la fenêtre — de quoi la réveiller, et seulement
    /// alors.
    pub fn alimenter(&self, instantane: Instantane, page: &PageValo) -> bool {
        let mut e = self.etat.lock().unwrap();
        let nouveau = e.page != *page || e.instantane.as_ref().is_none_or(|ancien| !ancien.pareil(&instantane));
        if e.page != *page {
            e.page = page.clone();
        }
        e.instantane = Some(instantane);
        nouveau
    }

    /// La place de la fenêtre, à retenir pour la prochaine fois.
    pub fn place(&self) -> Option<Place> {
        self.etat.lock().unwrap().place
    }

    /// À chaque image dessinée de l'application, tant que la page est
    /// détachée : sans cet appel, egui ferme la fenêtre.
    pub fn montrer(&self, ctx: &egui::Context) {
        let builder = egui::ViewportBuilder::default()
            .with_title("VALORANT — ki-chat")
            .with_inner_size(TAILLE)
            .with_min_inner_size(TAILLE_MIN)
            .with_icon(self.icone.clone());
        let etat = self.etat.clone();
        ctx.show_viewport_deferred(id(), builder, move |ui, _classe| {
            etat.lock().unwrap().dessiner(ui);
        });
    }
}

impl Etat {
    /// La place de cette fenêtre : rendue à sa naissance, image après
    /// image, puis relevée tant qu'elle est ordinaire — ni minimisée, ni
    /// maximisée.
    fn tenir_la_place(&mut self, ctx: &egui::Context) {
        let (dehors, dedans, minimisee, maximisee) = ctx.input(|i| {
            let v = i.viewport();
            (v.outer_rect, v.inner_rect, v.minimized == Some(true), v.maximized == Some(true))
        });
        if minimisee {
            return;
        }
        let pixels_par_point = ctx.pixels_per_point();
        if let Some(rendu) = self.place_a_rendre.take() {
            let (commande, suite) = rendu.avancer(pixels_par_point);
            if let Some(commande) = commande {
                ctx.send_viewport_cmd(commande);
            }
            self.place_a_rendre = suite;
            ctx.request_repaint();
            return;
        }
        if maximisee {
            return;
        }
        if let Some(place) = dehors.zip(dedans).and_then(|(dehors, dedans)| Place::relevee(dehors, dedans, pixels_par_point)) {
            self.place = Some(place);
        }
    }

    fn dessiner(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if !self.barre_sombre {
            self.barre_sombre = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
        }
        self.tenir_la_place(&ctx);
        let mut reveiller = false;
        // La croix de la fenêtre ferme la page, comme « Fermer ».
        if ctx.input(|i| i.viewport().close_requested()) {
            self.fermer = true;
            reveiller = true;
        }
        let Etat { instantane, page, boutique, .. } = self;
        // Rien à lire encore : l'application n'a pas fini sa première image.
        let Some(instantane) = instantane.as_ref() else {
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(theme::BG_BASE)).show(ui, |_| {});
            return;
        };
        let avant = page.clone();
        let cx = instantane.contexte();
        let (ouvert, demandes) = egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::BG_BASE).inner_margin(marge::egale(espace::XL)))
            .show(ui, |ui| page.centre(ui, &cx, instantane.fiche.as_ref(), boutique))
            .inner;
        if *page != avant {
            self.dessinee = true;
            reveiller = true;
        }
        if !ouvert {
            self.fermer = true;
            reveiller = true;
        }
        if !demandes.is_empty() {
            self.demandes.extend(demandes);
            reveiller = true;
        }
        // L'application tourne à son rythme, parfois réduite : c'est elle
        // qui envoie les demandes au serveur et retient l'état de la page.
        if reveiller {
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> PageValo {
        PageValo::load(|_, defaut| defaut.to_string())
    }

    /// Les copies de l'état de la page passent dans les deux sens sans se
    /// perdre : ce que l'application change arrive à la fenêtre, et rien ne
    /// revient tant que la fenêtre n'a pas changé la sienne.
    #[test]
    fn l_etat_de_la_page_passe_dans_les_deux_sens() {
        let fenetre = Fenetre::new(page(), boutique::Lecteur::new(), None);
        let retour = fenetre.reprendre();
        assert!(retour.page.is_none() && retour.demandes.is_empty() && !retour.fermer, "rien ne s'est passé");

        // La fenêtre change un onglet, demande une actualisation, se ferme.
        let mut vue = page();
        vue.vers_profil();
        {
            let mut e = fenetre.etat.lock().unwrap();
            e.page = vue.clone();
            e.dessinee = true;
            e.demandes.push(Demande::Actualiser);
            e.fermer = true;
        }
        let retour = fenetre.reprendre();
        assert!(retour.page.as_ref() == Some(&vue), "l'application reprend l'état de la fenêtre");
        assert_eq!(retour.demandes, [Demande::Actualiser]);
        assert!(retour.fermer);
        // Une fois repris, rien ne revient deux fois.
        let retour = fenetre.reprendre();
        assert!(retour.page.is_none() && retour.demandes.is_empty() && !retour.fermer);
    }
}
