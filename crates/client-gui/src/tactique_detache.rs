//! La carte tactique dans sa propre fenêtre système : sur un second écran,
//! à côté du jeu, là où l'on dessine la prochaine manche.
//!
//! Une fenêtre « différée » d'egui, comme la page VALORANT détachée
//! (`valo_detache`, qui dit pourquoi) : elle vit seule, et continue de se
//! dessiner quand la fenêtre de ki-chat est réduite.
//!
//! La carte se prête mieux au partage que la page : son état
//! ([`Tactique`]) est d'un seul tenant et ne lit presque rien d'ailleurs.
//! L'application et la fenêtre le tiennent donc à deux, sous un verrou
//! qu'elles ne se disputent jamais — elles tournent l'une après l'autre,
//! sur le même fil. La fenêtre y dessine ; l'application y range ce qui
//! arrive du serveur et en sort les gestes à envoyer. Du reste de
//! l'application, la fenêtre ne reçoit que le catalogue (les plans, les
//! minimaps), qui dessine, et sa couleur.

use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32};
use ki_protocol::UserId;
use ki_ui::jetons::{espace, marge};

use crate::place::{Place, PlaceTenue};
use crate::tactique::Tactique;
use crate::theme;
use crate::valo_catalogue::Catalogue;

/// La fenêtre de la carte détachée, pour qui veut la réveiller, la ramener
/// devant ou la photographier.
pub fn id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("tactique-detachee")
}

/// La taille de la fenêtre la première fois — celle de la fenêtre dans
/// ki-chat —, et la plus petite où la carte se dessine encore.
const TAILLE: [f32; 2] = [760.0, 820.0];
const TAILLE_MIN: [f32; 2] = [420.0, 460.0];

/// La fenêtre détachée : vit tant que la carte est ouverte et détachée.
pub struct Fenetre {
    tactique: Arc<Mutex<Tactique>>,
    etat: Arc<Mutex<Etat>>,
    icone: Arc<egui::IconData>,
}

/// Ce que la fenêtre reçoit de l'application, et ce qu'elle lui dit.
struct Etat {
    catalogue: Catalogue,
    moi: UserId,
    couleur: Color32,
    /// La croix de la fenêtre : la carte se ferme.
    fermer: bool,
    place: PlaceTenue,
    barre_sombre: bool,
}

impl Fenetre {
    /// `place` : celle qu'avait la fenêtre la dernière fois ; rendue si elle
    /// touche encore un écran, sa taille seule sinon.
    pub fn new(tactique: Arc<Mutex<Tactique>>, catalogue: Catalogue, place: Option<Place>) -> Self {
        let etat = Etat { catalogue, moi: 0, couleur: theme::ACCENT, fermer: false, place: PlaceTenue::new(place), barre_sombre: false };
        Self { tactique, etat: Arc::new(Mutex::new(etat)), icone: Arc::new(theme::app_icon()) }
    }

    /// À chaque image de l'application : qui je suis et ma couleur, le
    /// catalogue s'il a monté de nouvelles images — contre la fermeture de
    /// la fenêtre, si on a cliqué sa croix. Rend aussi vrai si la fenêtre
    /// a du nouveau à dessiner.
    pub fn echanger(&self, catalogue: &Catalogue, moi: UserId, couleur: Color32) -> (bool, bool) {
        let mut e = self.etat.lock().unwrap();
        let mut nouveau = e.moi != moi || e.couleur != couleur;
        e.moi = moi;
        e.couleur = couleur;
        if e.catalogue.montees() != catalogue.montees() {
            e.catalogue = catalogue.clone();
            nouveau = true;
        }
        (std::mem::take(&mut e.fermer), nouveau)
    }

    /// La place de la fenêtre, à retenir pour la prochaine fois.
    pub fn place(&self) -> Option<Place> {
        self.etat.lock().unwrap().place.place()
    }

    /// À chaque image dessinée de l'application, tant que la carte est
    /// détachée : sans cet appel, egui ferme la fenêtre.
    pub fn montrer(&self, ctx: &egui::Context) {
        let builder = egui::ViewportBuilder::default()
            .with_title("Carte tactique — ki-chat")
            .with_inner_size(TAILLE)
            .with_min_inner_size(TAILLE_MIN)
            .with_icon(self.icone.clone());
        let (etat, tactique) = (self.etat.clone(), self.tactique.clone());
        ctx.show_viewport_deferred(id(), builder, move |ui, _classe| {
            etat.lock().unwrap().dessiner(ui, &tactique);
        });
    }
}

impl Etat {
    fn dessiner(&mut self, ui: &mut egui::Ui, tactique: &Mutex<Tactique>) {
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
        let mut t = tactique.lock().unwrap();
        let etait_detachee = t.detachee;
        let plans = self.catalogue.plans();
        let minimap = t.choisi(plans).and_then(|p| self.catalogue.minimap(p));
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::BG_BASE).inner_margin(marge::egale(espace::L)))
            .show(ui, |ui| t.contenu(ui, plans, minimap, self.moi, self.couleur));
        // L'application envoie les gestes au serveur, et c'est elle qui
        // rattache : réduite, elle ne tourne que si on la réveille.
        if reveiller || t.a_des_envois() || t.detachee != etait_detachee {
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
    }
}
