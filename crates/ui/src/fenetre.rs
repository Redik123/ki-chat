//! Les fenêtres et les modales : un seul modèle pour tout ce qui s'ouvre
//! par-dessus l'écran.
//!
//! L'appli comptait vingt-trois `egui::Window`, chacune avec ses réglages :
//! cinq se repliaient et pas les autres, quatre tiraient leur identifiant
//! d'un titre qui change (leur place se mémorisait donc par pseudo), les
//! confirmations laissaient cliquer derrière elles, et Échap n'en fermait
//! que la moitié, dans un ordre écrit à la main.
//!
//! - [`Fenetre`] : un outil qu'on garde ouvert à côté du reste (réglages,
//!   clips, recherche). Identifiant fixe, jamais repliable, taille bornée à
//!   l'écran.
//! - [`Modale`] : une question à laquelle il faut répondre avant de
//!   continuer (confirmer une suppression, taper un mot de passe). Le reste
//!   de l'écran s'assombrit et ne répond plus tant qu'elle est là.
//! - [`echap`] : la touche Échap ferme ce qui est au-dessus — la modale s'il
//!   y en a une, sinon la fenêtre qu'on a touchée en dernier — et rien
//!   d'autre. À appeler une fois par image, avant de dessiner.
//!
//! ```ignore
//! // En tête d'image :
//! if ki_ui::fenetre::echap(ctx) { /* la touche est prise */ }
//!
//! Fenetre::new("reglages", "Réglages").ouverte(&mut self.ouverts).largeur(520.0).haute().show(ctx, |ui| { … });
//!
//! let sortie = Modale::new("supprimer_clip", "Supprimer ce clip ?").show(ctx, |ui| { … });
//! if sortie.fermee { /* Échap ou clic à côté : on annule */ }
//! ```

use egui::{Align2, Color32, Context, Id, Key, LayerId, Modifiers, Order, Ui, Vec2};

use crate::jetons::espace;
use crate::typo::Texte;

/// La marge gardée autour d'une fenêtre : elle ne dépasse jamais l'écran
/// moins cette marge, de chaque côté.
const MARGE_ECRAN: f32 = 20.0;
/// En dessous, on ne borne plus : une fenêtre de 300 points ne sert à rien.
const TAILLE_PLANCHER: Vec2 = Vec2::new(360.0, 300.0);
/// La hauteur d'une fenêtre « haute » : presque tout l'écran, sans excès.
const HAUTE: std::ops::RangeInclusive<f32> = 320.0..=900.0;

// ---------------------------------------------------------------------
// La pile : qui est au-dessus, et qui prend Échap
// ---------------------------------------------------------------------

/// Ce que ki-ui sait des fenêtres ouvertes, d'une image à l'autre. Gardé
/// dans la mémoire d'egui, le temps de la session.
#[derive(Clone, Default)]
struct Pile {
    /// L'image que décrivent `vues` et `modale`.
    image: u64,
    /// Les fenêtres à croix dessinées pendant cette image, avec le rang de
    /// leur calque : le plus haut est au-dessus des autres.
    vues: Vec<(Id, usize)>,
    /// La modale dessinée pendant cette image.
    modale: Option<Id>,
    /// Le bilan de l'image d'avant : la fenêtre du dessus, la modale.
    dessus: Option<Id>,
    modale_avant: Option<Id>,
    /// Ce à quoi [`echap`] a donné la touche : la fenêtre ou la modale se
    /// fermera en se dessinant.
    a_fermer: Option<Id>,
}

fn avec_la_pile<R>(ctx: &Context, f: impl FnOnce(&mut Pile) -> R) -> R {
    let image = ctx.cumulative_frame_nr();
    ctx.data_mut(|d| {
        let pile = d.get_temp_mut_or_default::<Pile>(Id::unique("ki-ui-pile-des-fenetres"));
        if pile.image != image {
            // Une image de passée : ce qui s'y est dessiné devient le bilan.
            // Plusieurs images sans que personne ne passe ici, et c'est que
            // plus rien n'est ouvert : ce qu'on avait noté date, et Échap
            // n'a pas à aller à une fenêtre fermée depuis.
            let juste_avant = pile.image + 1 == image;
            pile.dessus = pile.vues.iter().max_by_key(|(_, rang)| *rang).map(|(id, _)| *id).filter(|_| juste_avant);
            pile.modale_avant = pile.modale.filter(|_| juste_avant);
            pile.vues.clear();
            pile.modale = None;
            pile.a_fermer = None;
            pile.image = image;
        }
        f(pile)
    })
}

/// Échap : la touche va à ce qui est au-dessus — la modale ouverte, sinon
/// la fenêtre à croix qu'on a touchée en dernier —, qui se fermera en se
/// dessinant. Rend vrai quand la touche a été prise : ce qui est derrière
/// (une zone de saisie, la page) ne doit plus la lire.
///
/// À appeler une fois par image, avant de dessiner. Un menu déroulant ouvert
/// garde la touche : Échap le referme d'abord, lui.
pub fn echap(ctx: &Context) -> bool {
    if !ctx.input(|i| i.key_pressed(Key::Escape)) || egui::Popup::is_any_open(ctx) {
        return false;
    }
    let cible = avec_la_pile(ctx, |pile| {
        let cible = pile.modale_avant.or(pile.dessus);
        pile.a_fermer = cible;
        cible
    });
    if cible.is_some() {
        ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
    }
    cible.is_some()
}

/// Y a-t-il une fenêtre à croix ou une modale à l'écran ? D'après l'image
/// d'avant : de quoi savoir, avant de dessiner, si Échap a quelque chose à
/// fermer.
pub fn une_est_ouverte(ctx: &Context) -> bool {
    avec_la_pile(ctx, |pile| pile.modale_avant.is_some() || pile.dessus.is_some())
}

/// Si [`echap`] a désigné `id` pendant cette image : vrai, une seule fois.
fn doit_se_fermer(ctx: &Context, id: Id) -> bool {
    avec_la_pile(ctx, |pile| {
        if pile.a_fermer == Some(id) {
            pile.a_fermer = None;
            true
        } else {
            false
        }
    })
}

/// La plus grande taille qu'une fenêtre peut prendre dans cet écran.
fn taille_max(ctx: &Context) -> Vec2 {
    (ctx.content_rect().size() - Vec2::splat(2.0 * MARGE_ECRAN)).max(TAILLE_PLANCHER)
}

// ---------------------------------------------------------------------
// La fenêtre
// ---------------------------------------------------------------------

/// Une fenêtre : un outil qu'on ouvre, déplace, et garde à côté du reste.
///
/// Son identifiant ne dépend pas de son titre — la place qu'on lui a donnée
/// se retrouve même si le titre change (« Salon de Bob », puis « Salon de
/// Lina »). Elle ne se replie pas, et ne dépasse jamais l'écran.
#[must_use = "une fenêtre se pose avec `.show(ctx, …)`"]
pub struct Fenetre<'a> {
    id: &'static str,
    titre: String,
    ouverte: Option<&'a mut bool>,
    largeur: Option<f32>,
    hauteur: Option<f32>,
    haute: bool,
    taille_min: Vec2,
    fixe: bool,
    ancre: Option<(Align2, Vec2)>,
    centree: bool,
}

impl<'a> Fenetre<'a> {
    /// `id` : un nom fixe, propre à cette fenêtre dans l'appli.
    pub fn new(id: &'static str, titre: impl Into<String>) -> Self {
        Self {
            id,
            titre: titre.into(),
            ouverte: None,
            largeur: None,
            hauteur: None,
            haute: false,
            taille_min: Vec2::ZERO,
            fixe: false,
            ancre: None,
            centree: false,
        }
    }

    /// L'identifiant egui d'une fenêtre de ce nom.
    pub fn id_de(id: &'static str) -> Id {
        Id::unique(("ki-ui-fenetre", id))
    }

    /// La fenêtre a une croix, et Échap la ferme quand elle est au-dessus :
    /// `ouverte` passe à faux. Sans cet appel, elle reste là tant qu'on la
    /// dessine.
    pub fn ouverte(mut self, ouverte: &'a mut bool) -> Self {
        self.ouverte = Some(ouverte);
        self
    }

    /// La largeur à la première ouverture. Sans elle, celle du contenu.
    pub fn largeur(mut self, largeur: f32) -> Self {
        self.largeur = Some(largeur);
        self
    }

    /// La hauteur à la première ouverture. Sans elle, celle du contenu.
    pub fn hauteur(mut self, hauteur: f32) -> Self {
        self.hauteur = Some(hauteur);
        self
    }

    /// Une fenêtre qui prend presque toute la hauteur de l'écran : une
    /// liste, une page de réglages.
    pub fn haute(mut self) -> Self {
        self.haute = true;
        self
    }

    /// En dessous, elle ne rétrécit plus.
    pub fn taille_min(mut self, largeur: f32, hauteur: f32) -> Self {
        self.taille_min = Vec2::new(largeur, hauteur);
        self
    }

    /// Elle ne se redimensionne pas : sa taille est celle de son contenu.
    pub fn fixe(mut self) -> Self {
        self.fixe = true;
        self
    }

    /// Posée à un endroit de l'écran, d'où elle ne bouge pas : un message
    /// de l'appli plutôt qu'un outil.
    pub fn ancree(mut self, ou: Align2, decalage: impl Into<Vec2>) -> Self {
        self.ancre = Some((ou, decalage.into()));
        self
    }

    /// À sa première ouverture, au centre de l'écran (et déplaçable).
    pub fn centree(mut self) -> Self {
        self.centree = true;
        self
    }

    /// Dessine la fenêtre. Rend ce que rend `contenu`, ou rien si elle est
    /// fermée.
    pub fn show<R>(mut self, ctx: &Context, contenu: impl FnOnce(&mut Ui) -> R) -> Option<R> {
        let id = Self::id_de(self.id);
        if let Some(ouverte) = self.ouverte.as_deref_mut() {
            if !*ouverte {
                return None;
            }
            if doit_se_fermer(ctx, id) {
                *ouverte = false;
                return None;
            }
        }
        let ecran = ctx.content_rect();
        let max = taille_max(ctx);
        let mut fenetre = egui::Window::new(self.titre)
            .id(id)
            .collapsible(false)
            .resizable(!self.fixe)
            .min_size(self.taille_min.min(max))
            .max_size(max);
        if let Some(largeur) = self.largeur {
            fenetre = fenetre.default_width(largeur.min(max.x));
        }
        let hauteur = if self.haute { Some((ecran.height() - 120.0).clamp(*HAUTE.start(), *HAUTE.end())) } else { self.hauteur };
        if let Some(hauteur) = hauteur {
            fenetre = fenetre.default_height(hauteur.min(max.y));
        }
        if let Some((ou, decalage)) = self.ancre {
            fenetre = fenetre.anchor(ou, decalage);
        } else if self.centree {
            fenetre = fenetre.pivot(Align2::CENTER_CENTER).default_pos(ecran.center());
        }
        let reponse = match self.ouverte.as_deref_mut() {
            Some(ouverte) => fenetre.open(ouverte).show(ctx, contenu),
            None => fenetre.show(ctx, contenu),
        };
        // Dessinée avec sa croix, et pas fermée à l'instant : elle compte
        // parmi celles qu'Échap peut fermer.
        if reponse.is_some() && self.ouverte.as_deref().copied() == Some(true) {
            noter(ctx, id);
        }
        reponse.and_then(|r| r.inner)
    }
}

/// La fenêtre `id` s'est dessinée pendant cette image, avec une croix : on
/// retient où elle est dans l'empilement.
fn noter(ctx: &Context, id: Id) {
    let calque = LayerId::new(Order::Middle, id);
    let rang = ctx.memory(|m| m.layer_ids().position(|l| l == calque)).unwrap_or(0);
    avec_la_pile(ctx, |pile| {
        pile.vues.retain(|(autre, _)| *autre != id);
        pile.vues.push((id, rang));
    });
}

// ---------------------------------------------------------------------
// La modale
// ---------------------------------------------------------------------

/// Ce qu'une modale rend.
pub struct Sortie<R> {
    /// Ce qu'a rendu son contenu.
    pub inner: R,
    /// On l'a quittée sans répondre — Échap, ou un clic à côté : à traiter
    /// comme « Annuler ».
    pub fermee: bool,
}

/// Une modale : une question posée par-dessus l'écran, qui attend sa
/// réponse. Tant qu'elle est là, le reste s'assombrit et ne répond plus —
/// on ne clique pas, par mégarde, derrière une confirmation.
///
/// Elle n'a pas de croix : ses boutons répondent. Échap et un clic à côté
/// la quittent sans répondre ([`Sortie::fermee`]), sauf
/// [`Modale::sans_echappatoire`].
#[must_use = "une modale se pose avec `.show(ctx, …)`"]
pub struct Modale {
    id: &'static str,
    titre: String,
    largeur: f32,
    echappatoire: bool,
}

impl Modale {
    /// `id` : un nom fixe, propre à cette modale dans l'appli.
    pub fn new(id: &'static str, titre: impl Into<String>) -> Self {
        Self { id, titre: titre.into(), largeur: 340.0, echappatoire: true }
    }

    /// La largeur de son contenu.
    pub fn largeur(mut self, largeur: f32) -> Self {
        self.largeur = largeur;
        self
    }

    /// Ni Échap ni un clic à côté ne la quittent : il faut choisir. Pour ce
    /// qui n'a pas de réponse « annuler ».
    pub fn sans_echappatoire(mut self) -> Self {
        self.echappatoire = false;
        self
    }

    pub fn show<R>(self, ctx: &Context, contenu: impl FnOnce(&mut Ui) -> R) -> Sortie<R> {
        let id = Id::unique(("ki-ui-modale", self.id));
        let par_echap = doit_se_fermer(ctx, id);
        avec_la_pile(ctx, |pile| pile.modale = Some(id));
        let largeur = self.largeur.min(taille_max(ctx).x);
        let titre = self.titre;
        let cadre = egui::Frame::window(&ctx.global_style()).inner_margin(crate::jetons::marge::egale(espace::XL));
        let reponse = egui::Modal::new(id).frame(cadre).backdrop_color(Color32::from_black_alpha(150)).show(ctx, |ui| {
            ui.set_width(largeur);
            if !titre.is_empty() {
                ui.label(Texte::TitreSection.riche(ui, titre));
                ui.add_space(espace::M);
            }
            contenu(ui)
        });
        // Un clic à côté. Échap, elle, est passée par `echap` en tête d'image.
        let a_cote = reponse.backdrop_response.clicked();
        Sortie { inner: reponse.inner, fermee: self.echappatoire && (par_echap || a_cote) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Une image : les événements donnés, `echap` en tête, puis le dessin.
    /// Rend ce qu'`echap` a rendu.
    fn image(ctx: &Context, evenements: Vec<egui::Event>, mut dessin: impl FnMut(&Context)) -> bool {
        let entree = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1000.0, 700.0))),
            events: evenements,
            ..Default::default()
        };
        let mut prise = false;
        ctx.run_ui(entree, |ui| {
            prise = echap(ui.ctx());
            dessin(ui.ctx());
        })
        .drop_without_applying_deltas();
        prise
    }

    fn touche_echap() -> Vec<egui::Event> {
        vec![egui::Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }]
    }

    /// Échap ferme la fenêtre du dessus, et elle seule ; puis la suivante ;
    /// puis plus rien — la touche est alors laissée à ce qui est derrière.
    #[test]
    fn echap_ferme_la_fenetre_du_dessus_puis_la_suivante() {
        let ctx = Context::default();
        let (mut a, mut b) = (true, true);
        let dessiner = |ctx: &Context, a: &mut bool, b: &mut bool| {
            Fenetre::new("a", "A").ouverte(a).show(ctx, |ui| ui.label("a"));
            Fenetre::new("b", "B").ouverte(b).show(ctx, |ui| ui.label("b"));
        };
        // Deux images pour que la pile sache ce qui est ouvert.
        for _ in 0..2 {
            assert!(!image(&ctx, Vec::new(), |ctx| dessiner(ctx, &mut a, &mut b)));
        }
        assert!(une_est_ouverte(&ctx));
        // B, dessinée la dernière, est au-dessus.
        assert!(image(&ctx, touche_echap(), |ctx| dessiner(ctx, &mut a, &mut b)));
        assert!(a && !b);
        image(&ctx, Vec::new(), |ctx| dessiner(ctx, &mut a, &mut b));
        assert!(image(&ctx, touche_echap(), |ctx| dessiner(ctx, &mut a, &mut b)));
        assert!(!a && !b);
        image(&ctx, Vec::new(), |ctx| dessiner(ctx, &mut a, &mut b));
        assert!(!image(&ctx, touche_echap(), |ctx| dessiner(ctx, &mut a, &mut b)), "plus rien à fermer : la touche reste libre");
        assert!(!une_est_ouverte(&ctx));
    }

    /// Une fenêtre fermée autrement que par Échap — sa croix, un choix fait
    /// dedans — ne garde pas la touche. Personne n'interroge la pile pendant
    /// les images qui suivent (une page a pris la place du fil) : le
    /// prochain Échap va quand même à ce qui est derrière.
    #[test]
    fn une_fenetre_fermee_ne_garde_pas_echap() {
        let ctx = Context::default();
        // Une image sans Échap ni question à la pile : seulement le dessin.
        let passer = |ctx: &Context, ouverte: &mut bool| {
            let entree = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1000.0, 700.0))),
                ..Default::default()
            };
            ctx.run_ui(entree, |ui| {
                Fenetre::new("a", "A").ouverte(ouverte).show(ui.ctx(), |ui| ui.label("a"));
            })
            .drop_without_applying_deltas();
        };
        let mut ouverte = true;
        for _ in 0..2 {
            passer(&ctx, &mut ouverte);
        }
        ouverte = false;
        for _ in 0..3 {
            passer(&ctx, &mut ouverte);
        }
        assert!(!image(&ctx, touche_echap(), |_| {}), "la fenêtre est fermée depuis trois images : la touche reste libre");
        assert!(!une_est_ouverte(&ctx));
    }

    /// « Au-dessus » suit l'empilement, pas l'ordre du code : la fenêtre
    /// qu'on vient de toucher passe devant, et c'est elle qu'Échap ferme.
    #[test]
    fn echap_suit_la_fenetre_ramenee_devant() {
        let ctx = Context::default();
        let (mut a, mut b) = (true, true);
        let dessiner = |ctx: &Context, a: &mut bool, b: &mut bool| {
            Fenetre::new("a", "A").ouverte(a).show(ctx, |ui| ui.label("a"));
            Fenetre::new("b", "B").ouverte(b).show(ctx, |ui| ui.label("b"));
        };
        for _ in 0..2 {
            image(&ctx, Vec::new(), |ctx| dessiner(ctx, &mut a, &mut b));
        }
        // On ramène A devant, comme le fait un clic dessus.
        ctx.move_to_top(LayerId::new(Order::Middle, Fenetre::id_de("a")));
        // egui range l'empilement en fin d'image, et la pile le lit à la
        // suivante : deux images plus tard, c'est su. Personne n'appuie sur
        // Échap dans les trente millisecondes qui suivent un clic.
        for _ in 0..2 {
            image(&ctx, Vec::new(), |ctx| dessiner(ctx, &mut a, &mut b));
        }
        assert!(image(&ctx, touche_echap(), |ctx| dessiner(ctx, &mut a, &mut b)));
        assert!(!a && b, "A était devant : c'est elle qui se ferme");
    }

    /// Une modale passe avant toutes les fenêtres : Échap la quitte, et les
    /// fenêtres restent.
    #[test]
    fn une_modale_prend_echap_avant_les_fenetres() {
        let ctx = Context::default();
        let mut fenetre = true;
        let mut modale = true;
        let mut quittee = false;
        let dessiner = |ctx: &Context, fenetre: &mut bool, modale: &mut bool, quittee: &mut bool| {
            Fenetre::new("outil", "Outil").ouverte(fenetre).show(ctx, |ui| ui.label("outil"));
            if *modale {
                let sortie = Modale::new("question", "Supprimer ?").show(ctx, |ui| ui.label("vraiment ?"));
                if sortie.fermee {
                    *modale = false;
                    *quittee = true;
                }
            }
        };
        for _ in 0..2 {
            image(&ctx, Vec::new(), |ctx| dessiner(ctx, &mut fenetre, &mut modale, &mut quittee));
        }
        assert!(image(&ctx, touche_echap(), |ctx| dessiner(ctx, &mut fenetre, &mut modale, &mut quittee)));
        assert!(quittee && !modale && fenetre, "la modale est quittée, la fenêtre reste");
        // La modale partie, Échap revient à la fenêtre.
        image(&ctx, Vec::new(), |ctx| dessiner(ctx, &mut fenetre, &mut modale, &mut quittee));
        assert!(image(&ctx, touche_echap(), |ctx| dessiner(ctx, &mut fenetre, &mut modale, &mut quittee)));
        assert!(!fenetre);
    }

    /// Une modale sans échappatoire ne se quitte pas par Échap ; et une
    /// fenêtre sans croix n'est pas fermée par Échap non plus.
    #[test]
    fn ce_qui_n_a_pas_de_sortie_reste() {
        let ctx = Context::default();
        let mut quittee = false;
        let dessiner = |ctx: &Context, quittee: &mut bool| {
            Fenetre::new("message", "Mise à jour").fixe().show(ctx, |ui| ui.label("en cours"));
            let sortie = Modale::new("choix", "Choisis").sans_echappatoire().show(ctx, |ui| ui.label("l'un ou l'autre"));
            *quittee |= sortie.fermee;
        };
        for _ in 0..2 {
            image(&ctx, Vec::new(), |ctx| dessiner(ctx, &mut quittee));
        }
        // La touche est prise (rien derrière ne doit la lire), mais la
        // modale reste.
        assert!(image(&ctx, touche_echap(), |ctx| dessiner(ctx, &mut quittee)));
        assert!(!quittee);
    }

    /// Une fenêtre ne dépasse jamais l'écran, même si on lui demande plus.
    #[test]
    fn une_fenetre_tient_dans_l_ecran() {
        let ctx = Context::default();
        let mut ouverte = true;
        let mut rect = egui::Rect::NOTHING;
        for _ in 0..3 {
            image(&ctx, Vec::new(), |ctx| {
                Fenetre::new("geante", "Géante").ouverte(&mut ouverte).largeur(4000.0).hauteur(4000.0).taille_min(3000.0, 3000.0).show(ctx, |ui| {
                    ui.allocate_space(ui.available_size());
                });
                if let Some(r) = ctx.memory(|m| m.area_rect(Fenetre::id_de("geante"))) {
                    rect = r;
                }
            });
        }
        assert!(rect.width() <= 1000.0 && rect.height() <= 700.0, "{rect:?}");
        assert!(rect.width() > 600.0, "elle prend quand même la place qu'il y a : {rect:?}");
    }
}
