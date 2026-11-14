//! La table de cartes dans sa propre fenêtre système : la partie sur un
//! second écran, ki-chat réduit ou ailleurs.
//!
//! Une fenêtre « différée » d'egui, comme la page VALORANT détachée
//! (`valo_detache`, qui dit pourquoi) : elle vit seule, et continue de se
//! dessiner quand la fenêtre de ki-chat est réduite.
//!
//! Comme la carte tactique (`tactique_detache`), la table est tenue à deux,
//! sous un verrou que l'application et la fenêtre ne se disputent jamais —
//! elles tournent l'une après l'autre, sur le même fil. La fenêtre y
//! dessine ; l'application y range ce qui arrive du serveur.
//!
//! Deux choses passent par ici plutôt que par la table. Dans un sens, qui
//! est qui : la table ne connaît que des numéros de membres, et leurs noms,
//! leurs couleurs et leurs photos sont ceux de l'application, copiés à
//! chacune de ses images. Dans l'autre, mes gestes : c'est l'application qui
//! les envoie au serveur.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use eframe::egui;
use ki_protocol::{ActionCartes, UserId};
use ki_ui::jetons::{espace, marge};

use crate::cartes::{Cartes, Identite};
use crate::place::{Place, PlaceTenue};
use crate::theme;

/// La fenêtre de la table détachée, pour qui veut la réveiller, la ramener
/// devant ou la photographier.
pub fn id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("cartes-detachees")
}

/// La taille de la fenêtre la première fois — celle de la fenêtre dans
/// ki-chat —, et la plus petite où la table tient encore.
const TAILLE: [f32; 2] = [920.0, 660.0];
const TAILLE_MIN: [f32; 2] = [660.0, 560.0];

/// Qui est qui à la table, vu de l'application.
pub type Identites = HashMap<UserId, Identite>;

/// Ce que la fenêtre a fait depuis la dernière image de l'application.
#[derive(Default)]
pub struct Retour {
    /// Mes gestes, à envoyer au serveur.
    pub actions: Vec<ActionCartes>,
    /// La croix de la fenêtre : la table se ferme.
    pub fermer: bool,
    /// Ce que l'application vient de donner change ce que la fenêtre
    /// dessine : de quoi la réveiller, et seulement alors.
    pub nouveau: bool,
}

/// La fenêtre détachée : vit tant que la table est ouverte et détachée.
pub struct Fenetre {
    cartes: Arc<Mutex<Cartes>>,
    etat: Arc<Mutex<Etat>>,
    icone: Arc<egui::IconData>,
}

/// Ce que la fenêtre reçoit de l'application, et ce qu'elle lui dit.
struct Etat {
    identites: Identites,
    moi: Option<UserId>,
    en_vocal: bool,
    actions: Vec<ActionCartes>,
    fermer: bool,
    place: PlaceTenue,
    barre_sombre: bool,
}

impl Fenetre {
    /// `place` : celle qu'avait la fenêtre la dernière fois ; rendue si elle
    /// touche encore un écran, sa taille seule sinon.
    pub fn new(cartes: Arc<Mutex<Cartes>>, place: Option<Place>) -> Self {
        let etat = Etat {
            identites: Identites::new(),
            moi: None,
            en_vocal: false,
            actions: Vec::new(),
            fermer: false,
            place: PlaceTenue::new(place),
            barre_sombre: false,
        };
        Self { cartes, etat: Arc::new(Mutex::new(etat)), icone: Arc::new(theme::app_icon()) }
    }

    /// À chaque image de l'application : qui est qui, qui je suis et si je
    /// suis en vocal — contre mes gestes et la fermeture de la fenêtre.
    pub fn echanger(&self, identites: Identites, moi: Option<UserId>, en_vocal: bool) -> Retour {
        let mut e = self.etat.lock().unwrap();
        let nouveau = e.moi != moi || e.en_vocal != en_vocal || e.identites != identites;
        if nouveau {
            e.identites = identites;
            e.moi = moi;
            e.en_vocal = en_vocal;
        }
        Retour { actions: std::mem::take(&mut e.actions), fermer: std::mem::take(&mut e.fermer), nouveau }
    }

    /// La place de la fenêtre, à retenir pour la prochaine fois.
    pub fn place(&self) -> Option<Place> {
        self.etat.lock().unwrap().place.place()
    }

    /// À chaque image dessinée de l'application, tant que la table est
    /// détachée : sans cet appel, egui ferme la fenêtre.
    pub fn montrer(&self, ctx: &egui::Context) {
        let builder = egui::ViewportBuilder::default()
            .with_title("Dernière carte — ki-chat")
            .with_inner_size(TAILLE)
            .with_min_inner_size(TAILLE_MIN)
            .with_icon(self.icone.clone());
        let (etat, cartes) = (self.etat.clone(), self.cartes.clone());
        ctx.show_viewport_deferred(id(), builder, move |ui, _classe| {
            etat.lock().unwrap().dessiner(ui, &cartes);
        });
    }
}

impl Etat {
    fn dessiner(&mut self, ui: &mut egui::Ui, cartes: &Mutex<Cartes>) {
        let ctx = ui.ctx().clone();
        if !self.barre_sombre {
            self.barre_sombre = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
        }
        self.place.tenir(&ctx);
        let mut reveiller = false;
        if ctx.input(|i| i.viewport().close_requested()) {
            self.fermer = true;
            reveiller = true;
        }
        let Etat { identites, moi, en_vocal, actions, .. } = self;
        let mut table = cartes.lock().unwrap();
        let etait_detachee = table.detachee;
        let deja = actions.len();
        let qui = |id: UserId| identites.get(&id).cloned().unwrap_or_else(Identite::inconnue);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::BG_BASE).inner_margin(marge::egale(espace::M)))
            .show(ui, |ui| table.contenu(ui, *moi, *en_vocal, &qui, actions));
        // L'application envoie les gestes au serveur, et c'est elle qui
        // rattache : réduite, elle ne tourne que si on la réveille.
        if reveiller || actions.len() > deja || table.detachee != etait_detachee {
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identites(nom: &str) -> Identites {
        Identites::from([(7, Identite { nom: nom.into(), couleur: theme::ACCENT, photo: None })])
    }

    /// La fenêtre ne se réveille que si ce qu'elle lit a changé, et mes
    /// gestes ne partent qu'une fois.
    #[test]
    fn ce_qui_change_reveille_et_les_gestes_ne_partent_qu_une_fois() {
        let fenetre = Fenetre::new(Arc::new(Mutex::new(Cartes::default())), None);
        assert!(fenetre.echanger(identites("ana"), Some(7), true).nouveau, "la première fois, tout est neuf");
        assert!(!fenetre.echanger(identites("ana"), Some(7), true).nouveau, "rien n'a bougé");
        assert!(fenetre.echanger(identites("anna"), Some(7), true).nouveau, "un pseudo changé se redessine");
        assert!(fenetre.echanger(identites("anna"), Some(7), false).nouveau, "sorti du vocal aussi");

        {
            let mut e = fenetre.etat.lock().unwrap();
            e.actions.push(ActionCartes::Piocher);
            e.fermer = true;
        }
        let retour = fenetre.echanger(identites("anna"), Some(7), false);
        assert_eq!(retour.actions, [ActionCartes::Piocher]);
        assert!(retour.fermer && !retour.nouveau);
        let retour = fenetre.echanger(identites("anna"), Some(7), false);
        assert!(retour.actions.is_empty() && !retour.fermer, "une fois repris, rien ne revient deux fois");
    }
}
