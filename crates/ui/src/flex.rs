//! La mise en page flexbox : des rangées et des colonnes dont les éléments
//! se partagent la place, passent à la ligne quand elle manque, et se
//! répartissent comme on le dit.
//!
//! ```ignore
//! use ki_ui::flex::{Flex, Repartit};
//!
//! // [trombone] [ champ qui prend toute la place ] [envoyer]
//! Flex::ligne().ecart(espace::S).show(ui, "saisie", |f| {
//!     f.ui(|ui| ui.button("📎"));
//!     f.grandit(|ui| ui.add_sized(ui.available_size(), egui::TextEdit::singleline(&mut texte)));
//!     f.ui(|ui| ui.button("➤"));
//! });
//! ```
//!
//! Le calcul est celui de taffy (le moteur de Bevy, Dioxus, Zed), par
//! egui_taffy : la première image mesure le contenu sans le montrer ; quand
//! une taille change ensuite, la mise en page est refaite et egui rejoue
//! l'image avant de l'afficher — rien ne saute à l'écran. Le contenu de
//! chaque élément n'est donc appelé qu'une fois par image, comme en egui
//! ordinaire : boutons et champs s'y écrivent sans précaution.
//!
//! egui_taffy reste caché derrière ce module : les écrans ne parlent que
//! `Flex`, `Case` et `Contenu`.

use egui::Ui;
use egui_taffy::taffy::{self, prelude::{length, percent}};
use egui_taffy::{Tui, TuiBuilderLogic};

/// Où les éléments se placent sur l'axe transversal : en hauteur dans une
/// rangée, en largeur dans une colonne.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aligne {
    Debut,
    Centre,
    Fin,
    /// Toute la hauteur de la rangée (toute la largeur de la colonne).
    Etire,
    /// Les textes d'une rangée sur la même ligne de base.
    LigneDeBase,
}

impl Aligne {
    fn taffy(self) -> taffy::AlignItems {
        match self {
            Aligne::Debut => taffy::AlignItems::Start,
            Aligne::Centre => taffy::AlignItems::Center,
            Aligne::Fin => taffy::AlignItems::End,
            Aligne::Etire => taffy::AlignItems::Stretch,
            Aligne::LigneDeBase => taffy::AlignItems::Baseline,
        }
    }
}

/// Comment la place qui reste se répartit sur l'axe principal, quand aucun
/// élément ne grandit pour la prendre.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repartit {
    /// Tout contre le début.
    Debut,
    Centre,
    /// Tout contre la fin.
    Fin,
    /// Le premier au début, le dernier à la fin, le reste également espacé.
    Entre,
    /// Autant d'espace de part et d'autre de chaque élément.
    Autour,
    /// Des espaces tous égaux, bords compris.
    Egal,
}

impl Repartit {
    fn taffy(self) -> taffy::JustifyContent {
        match self {
            Repartit::Debut => taffy::JustifyContent::Start,
            Repartit::Centre => taffy::JustifyContent::Center,
            Repartit::Fin => taffy::JustifyContent::End,
            Repartit::Entre => taffy::JustifyContent::SpaceBetween,
            Repartit::Autour => taffy::JustifyContent::SpaceAround,
            Repartit::Egal => taffy::JustifyContent::SpaceEvenly,
        }
    }
}

/// Un conteneur : une rangée ou une colonne, et la façon d'y placer ses
/// éléments.
#[derive(Clone, Debug)]
#[must_use]
pub struct Flex {
    style: taffy::Style,
}

impl Flex {
    /// Une rangée, ses éléments centrés en hauteur — ce qu'on veut presque
    /// toujours pour une barre de boutons ou une ligne de réglage.
    pub fn ligne() -> Self {
        Self::vers(taffy::FlexDirection::Row).aligner(Aligne::Centre)
    }

    /// Une colonne, ses éléments étirés sur toute sa largeur.
    pub fn colonne() -> Self {
        Self::vers(taffy::FlexDirection::Column).aligner(Aligne::Etire)
    }

    fn vers(direction: taffy::FlexDirection) -> Self {
        Self {
            style: taffy::Style {
                display: taffy::Display::Flex,
                flex_direction: direction,
                ..Default::default()
            },
        }
    }

    /// L'écart entre deux éléments, et entre deux lignes quand elles passent
    /// à la ligne (voir [`crate::jetons::espace`]).
    pub fn ecart(self, points: f32) -> Self {
        self.ecarts(points, points)
    }

    /// Des écarts différents entre colonnes (horizontal) et entre lignes
    /// (vertical).
    pub fn ecarts(mut self, horizontal: f32, vertical: f32) -> Self {
        self.style.gap = taffy::Size { width: length(horizontal), height: length(vertical) };
        self
    }

    pub fn aligner(mut self, aligne: Aligne) -> Self {
        self.style.align_items = Some(aligne.taffy());
        self
    }

    pub fn repartir(mut self, repartit: Repartit) -> Self {
        self.style.justify_content = Some(repartit.taffy());
        self
    }

    /// Les éléments qui ne tiennent plus passent sur une nouvelle ligne
    /// (une nouvelle colonne, dans une colonne).
    pub fn passer_a_la_ligne(mut self) -> Self {
        self.style.flex_wrap = taffy::FlexWrap::Wrap;
        self
    }

    /// Une marge intérieure, autour de tous les éléments.
    pub fn marge(mut self, points: f32) -> Self {
        self.style.padding = length(points);
        self
    }

    /// Pose le conteneur sur toute la largeur disponible de `ui`, et y
    /// place ce que `contenu` y ajoute. `id` le distingue de ses voisins :
    /// la mise en page de chaque conteneur est gardée d'une image à
    /// l'autre.
    pub fn show<R>(
        self,
        ui: &mut Ui,
        id: impl egui::AsIdSalt,
        contenu: impl FnOnce(&mut Contenu<'_>) -> R,
    ) -> R {
        let id = ui.id().with(id);
        // Toute la largeur : sans elle, la racine se contente de celle de
        // son contenu, et rien n'a de place à prendre ni à répartir.
        let style = taffy::Style { size: taffy::Size { width: percent(1.0_f32), ..self.style.size }, ..self.style };
        egui_taffy::tui(ui, id)
            .reserve_available_width()
            .style(style)
            .show(|tui| contenu(&mut Contenu { tui }))
    }
}

/// Comment un élément se comporte dans son conteneur : sa taille, et sa
/// part de la place qui reste ou qui manque.
#[derive(Clone, Debug)]
#[must_use]
pub struct Case {
    style: taffy::Style,
}

impl Default for Case {
    fn default() -> Self {
        Self::new()
    }
}

impl Case {
    /// À sa taille naturelle ; il rétrécit si la place manque (comme en CSS).
    pub fn new() -> Self {
        Self {
            style: taffy::Style {
                flex_grow: 0.0,
                flex_shrink: 1.0,
                ..Default::default()
            },
        }
    }

    /// Prend sa part de la place qui reste, au prorata de `facteur` — sans
    /// compter sa taille naturelle (`flex: facteur 1 0` en CSS) : deux
    /// éléments à 1 font exactement moitié-moitié. Il peut aussi devenir
    /// plus petit que son contenu, qui s'y adapte (un texte y passe à la
    /// ligne).
    pub fn grandir(mut self, facteur: f32) -> Self {
        self.style.flex_grow = facteur;
        self.style.flex_basis = length(0.0_f32);
        self.style.min_size = taffy::Size { width: length(0.0_f32), height: length(0.0_f32) };
        self
    }

    /// Sa taille de départ sur l'axe principal, avant partage.
    pub fn base(mut self, points: f32) -> Self {
        self.style.flex_basis = length(points);
        self
    }

    pub fn largeur(mut self, points: f32) -> Self {
        self.style.size.width = length(points);
        self
    }

    pub fn hauteur(mut self, points: f32) -> Self {
        self.style.size.height = length(points);
        self
    }

    /// En dessous, il ne rétrécit plus : la ligne passe à la ligne (si le
    /// conteneur le permet) ou déborde.
    pub fn largeur_min(mut self, points: f32) -> Self {
        self.style.min_size.width = length(points);
        self
    }

    pub fn largeur_max(mut self, points: f32) -> Self {
        self.style.max_size.width = length(points);
        self
    }

    /// Ne rétrécit jamais, quoi qu'il manque.
    pub fn rigide(mut self) -> Self {
        self.style.flex_shrink = 0.0;
        self
    }

    /// Son propre alignement, à la place de celui du conteneur.
    pub fn s_aligner(mut self, aligne: Aligne) -> Self {
        self.style.align_self = Some(aligne.taffy());
        self
    }

    /// Le style d'un conteneur imbriqué : celui du conteneur, plus ce qui
    /// fait de lui un élément de son parent.
    fn avec_conteneur(self, conteneur: Flex) -> taffy::Style {
        let element = self.style;
        taffy::Style {
            flex_grow: element.flex_grow,
            flex_shrink: element.flex_shrink,
            flex_basis: element.flex_basis,
            size: element.size,
            min_size: element.min_size,
            max_size: element.max_size,
            align_self: element.align_self,
            margin: element.margin,
            ..conteneur.style
        }
    }
}

/// Ce qu'on ajoute dans un conteneur, dans l'ordre.
pub struct Contenu<'a> {
    tui: &'a mut Tui,
}

impl Contenu<'_> {
    /// Un élément à sa taille naturelle.
    pub fn ui<R>(&mut self, ajout: impl FnOnce(&mut Ui) -> R) -> R {
        self.case(Case::new(), ajout)
    }

    /// Un élément qui prend toute la place qui reste. Son `Ui` a la taille
    /// qu'il reçoit : `ui.available_size()` la donne.
    pub fn grandit<R>(&mut self, ajout: impl FnOnce(&mut Ui) -> R) -> R {
        self.case(Case::new().grandir(1.0), ajout)
    }

    /// Un élément réglé par `case`.
    pub fn case<R>(&mut self, case: Case, ajout: impl FnOnce(&mut Ui) -> R) -> R {
        (&mut *self.tui).style(case.style).ui(ajout)
    }

    /// Un conteneur imbriqué, à sa taille naturelle.
    pub fn flex<R>(&mut self, flex: Flex, contenu: impl FnOnce(&mut Contenu<'_>) -> R) -> R {
        self.flex_dans(Case::new(), flex, contenu)
    }

    /// Un conteneur imbriqué, qui se comporte dans son parent comme `case`
    /// le dit (qui grandit, par exemple).
    pub fn flex_dans<R>(
        &mut self,
        case: Case,
        flex: Flex,
        contenu: impl FnOnce(&mut Contenu<'_>) -> R,
    ) -> R {
        (&mut *self.tui).style(case.avec_conteneur(flex)).add(|tui| contenu(&mut Contenu { tui }))
    }

    /// Un vide qui prend toute la place qui reste : ce qui le suit est
    /// poussé au bout.
    pub fn ressort(&mut self) {
        self.case(Case::new().grandir(1.0), |_| {});
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{vec2, Rect, Sense};

    /// Quelques images d'un écran de `largeur` sur 300, et les rectangles
    /// relevés à la dernière : la première ne fait que mesurer.
    fn mesurer(largeur: f32, mut dessin: impl FnMut(&mut Ui, &mut Vec<Rect>)) -> Vec<Rect> {
        let ctx = egui::Context::default();
        let mut releves = Vec::new();
        for _ in 0..4 {
            let entree = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(largeur, 300.0))),
                ..Default::default()
            };
            ctx.run_ui(entree, |ui| {
                releves.clear();
                dessin(ui, &mut releves);
            })
            .drop_without_applying_deltas();
        }
        releves
    }

    /// Un bloc fixe, relevé.
    fn bloc(ui: &mut Ui, releves: &mut Vec<Rect>, l: f32, h: f32) {
        let (rect, _) = ui.allocate_exact_size(vec2(l, h), Sense::hover());
        releves.push(rect);
    }

    /// La place reçue par un élément qui grandit, relevée et occupée.
    fn place(ui: &mut Ui, releves: &mut Vec<Rect>) {
        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, Sense::hover());
        releves.push(rect);
    }

    fn proche(a: f32, b: f32) -> bool {
        (a - b).abs() <= 1.0
    }

    #[test]
    fn un_element_qui_grandit_prend_la_place_qui_reste() {
        let r = mesurer(400.0, |ui, rel| {
            Flex::ligne().ecart(8.0).show(ui, "t", |f| {
                f.ui(|ui| bloc(ui, rel, 50.0, 20.0));
                f.grandit(|ui| place(ui, rel));
                f.ui(|ui| bloc(ui, rel, 30.0, 20.0));
            });
        });
        assert_eq!(r.len(), 3);
        assert!(proche(r[1].width(), 400.0 - 50.0 - 30.0 - 2.0 * 8.0), "{r:?}");
        assert!(proche(r[2].max.x, 400.0), "{r:?}");
    }

    #[test]
    fn repartir_entre_colle_les_bouts_aux_bords() {
        let r = mesurer(400.0, |ui, rel| {
            Flex::ligne().repartir(Repartit::Entre).show(ui, "t", |f| {
                f.ui(|ui| bloc(ui, rel, 50.0, 20.0));
                f.ui(|ui| bloc(ui, rel, 30.0, 20.0));
            });
        });
        assert!(proche(r[0].min.x, 0.0), "{r:?}");
        assert!(proche(r[1].max.x, 400.0), "{r:?}");
    }

    #[test]
    fn un_ressort_pousse_la_suite_au_bout() {
        let r = mesurer(400.0, |ui, rel| {
            Flex::ligne().show(ui, "t", |f| {
                f.ui(|ui| bloc(ui, rel, 50.0, 20.0));
                f.ressort();
                f.ui(|ui| bloc(ui, rel, 30.0, 20.0));
            });
        });
        assert!(proche(r[1].max.x, 400.0), "{r:?}");
    }

    #[test]
    fn ce_qui_ne_tient_plus_passe_a_la_ligne() {
        let r = mesurer(400.0, |ui, rel| {
            Flex::ligne().ecart(10.0).passer_a_la_ligne().show(ui, "t", |f| {
                for _ in 0..3 {
                    f.case(Case::new().rigide(), |ui| bloc(ui, rel, 150.0, 20.0));
                }
            });
        });
        assert!(proche(r[0].min.y, r[1].min.y), "{r:?}");
        assert!(r[2].min.y >= r[0].max.y + 9.0, "le troisième passe dessous : {r:?}");
        assert!(proche(r[2].min.x, 0.0), "{r:?}");
    }

    #[test]
    fn une_colonne_empile_avec_son_ecart() {
        let r = mesurer(400.0, |ui, rel| {
            Flex::colonne().ecart(6.0).aligner(Aligne::Debut).show(ui, "t", |f| {
                f.ui(|ui| bloc(ui, rel, 100.0, 20.0));
                f.ui(|ui| bloc(ui, rel, 100.0, 30.0));
            });
        });
        assert!(proche(r[1].min.y, r[0].max.y + 6.0), "{r:?}");
    }

    #[test]
    fn deux_elements_qui_grandissent_partagent_au_prorata() {
        let r = mesurer(300.0, |ui, rel| {
            Flex::ligne().show(ui, "t", |f| {
                f.case(Case::new().grandir(1.0), |ui| place(ui, rel));
                f.case(Case::new().grandir(2.0), |ui| place(ui, rel));
            });
        });
        assert!(proche(r[0].width(), 100.0) && proche(r[1].width(), 200.0), "{r:?}");
    }

    #[test]
    fn un_conteneur_imbrique_qui_grandit() {
        let r = mesurer(400.0, |ui, rel| {
            Flex::ligne().show(ui, "t", |f| {
                f.ui(|ui| bloc(ui, rel, 100.0, 20.0));
                f.flex_dans(Case::new().grandir(1.0), Flex::ligne().repartir(Repartit::Fin), |f| {
                    f.ui(|ui| bloc(ui, rel, 40.0, 20.0));
                });
            });
        });
        assert!(proche(r[1].max.x, 400.0), "{r:?}");
    }
}
