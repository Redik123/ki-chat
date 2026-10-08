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
use egui_taffy::{Tui, TuiBuilderLogic, TuiContainerResponse};

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

/// La hauteur de la zone où un conteneur se pose (voir [`Flex::show`]) :
/// fixe, et plus grande que tout ce qu'on y mettra.
const HAUTEUR_D_ACCUEIL: f32 = 100_000.0;

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
        let en_ligne = self.en_ligne();
        // Toute la largeur : sans elle, la racine se contente de celle de
        // son contenu, et rien n'a de place à prendre ni à répartir.
        let style = taffy::Style { size: taffy::Size { width: percent(1.0_f32), ..self.style.size }, ..self.style };
        // Une zone d'accueil de hauteur fixe. egui_taffy refait toute la
        // mise en page — et fait rejouer l'image — dès que la zone où il se
        // pose change de taille, hauteur comprise ; or la place qui reste
        // sous un conteneur change dès que ce qui est au-dessus change de
        // hauteur (une valeur en direct, un bandeau qui apparaît). La
        // hauteur ne sert pas au calcul : seule la largeur est réservée.
        let accueil = egui::Rect::from_min_size(
            ui.available_rect_before_wrap().min,
            egui::vec2(ui.available_width(), HAUTEUR_D_ACCUEIL),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(accueil), |ui| {
            egui_taffy::tui(ui, id)
                .reserve_available_width()
                .style(style)
                .show(|tui| contenu(&mut Contenu { tui, en_ligne }))
        })
        .inner
    }

    fn en_ligne(&self) -> bool {
        matches!(self.style.flex_direction, taffy::FlexDirection::Row | taffy::FlexDirection::RowReverse)
    }
}

/// Comment un élément se comporte dans son conteneur : sa taille, et sa
/// part de la place qui reste ou qui manque.
#[derive(Clone, Debug)]
#[must_use]
pub struct Case {
    style: taffy::Style,
    /// Sa largeur (sa hauteur) ne dépend pas de son contenu : elle est
    /// fixée, ou donnée par le partage.
    largeur_imposee: bool,
    hauteur_imposee: bool,
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
            largeur_imposee: false,
            hauteur_imposee: false,
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

    /// Grandit-il : sa taille sur l'axe principal vient du partage.
    fn grandit(&self) -> bool {
        self.style.flex_grow > 0.0
    }

    /// Sa taille de départ sur l'axe principal, avant partage.
    pub fn base(mut self, points: f32) -> Self {
        self.style.flex_basis = length(points);
        self
    }

    pub fn largeur(mut self, points: f32) -> Self {
        self.style.size.width = length(points);
        self.largeur_imposee = true;
        self
    }

    pub fn hauteur(mut self, points: f32) -> Self {
        self.style.size.height = length(points);
        self.hauteur_imposee = true;
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
    /// Le conteneur est une rangée (sinon, une colonne).
    en_ligne: bool,
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
    ///
    /// Sa taille mesurée n'est rapportée que là où elle compte. Celle que
    /// le partage ou un réglage impose n'a pas à l'être : une valeur en
    /// direct (un niveau en dB qui change de largeur à chaque image) y
    /// relançait le calcul de la mise en page — et egui rejouait chaque
    /// image deux fois, sans fin.
    pub fn case<R>(&mut self, case: Case, ajout: impl FnOnce(&mut Ui) -> R) -> R {
        let (ignorer_largeur, ignorer_hauteur) = (
            case.largeur_imposee || (self.en_ligne && case.grandit()),
            case.hauteur_imposee || (!self.en_ligne && case.grandit()),
        );
        (&mut *self.tui).style(case.style).ui_manual(|ui, _| {
            let inner = ajout(ui);
            let mut taille = ui.min_size();
            if ignorer_largeur {
                taille.x = 0.0;
            }
            if ignorer_hauteur {
                taille.y = 0.0;
            }
            TuiContainerResponse {
                inner,
                min_size: taille,
                intrinsic_size: None,
                max_size: taille,
                infinite: egui::Vec2b::FALSE,
            }
        })
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
        let en_ligne = flex.en_ligne();
        (&mut *self.tui).style(case.avec_conteneur(flex)).add(|tui| contenu(&mut Contenu { tui, en_ligne }))
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

#[cfg(test)]
mod stabilite {
    use super::*;
    use egui::{vec2, Rect};

    /// Le nombre de passes de la dernière de `images` images : 1 si la mise
    /// en page est stable, 2 si elle a encore demandé à rejouer l'image.
    fn passes(images: usize, mut dessin: impl FnMut(&mut Ui)) -> Vec<usize> {
        let ctx = egui::Context::default();
        let mut toutes = Vec::new();
        for _ in 0..images {
            let entree = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(700.0, 500.0))),
                ..Default::default()
            };
            let sortie = ctx.run_ui(entree, |ui| dessin(ui));
            toutes.push(sortie.platform_output.num_completed_passes);
            sortie.drop_without_applying_deltas();
        }
        toutes
    }

    fn ligne(ui: &mut Ui, n: usize) {
        Flex::ligne().aligner(Aligne::Debut).ecarts(8.0, 4.0).passer_a_la_ligne().show(ui, ("l", n), |f| {
            f.case(Case::new().base(150.0).rigide(), |ui| {
                ui.label("Libellé");
            });
            f.case(Case::new().grandir(1.0).largeur_min(272.0), |ui| {
                ui.vertical(|ui| {
                    ui.add(egui::Label::new(
                        "Une longue précision qui passe à la ligne parce qu'elle ne tient pas sur une \
                         seule ligne dans la colonne du contrôle, comme dans les réglages.",
                    ).wrap());
                    let mut v = 0.5_f32;
                    ui.spacing_mut().slider_width = (ui.available_width() - 76.0).clamp(120.0, 260.0);
                    ui.add(egui::Slider::new(&mut v, 0.0..=1.0));
                });
            });
        });
    }

    /// Une valeur en direct (un niveau en dB) qui change de largeur à
    /// chaque image, au bout d'une rangée qui remplit la colonne.
    #[test]
    fn valeur_en_direct() {
        let mut image = 0u32;
        let passes = passes(8, |ui| {
                image += 1;
                Flex::ligne().ecart(8.0).show(ui, "v", |f| {
                    f.case(Case::new().base(150.0).rigide(), |ui| {
                        ui.label("Marge");
                    });
                    f.case(Case::new().grandir(1.0).largeur_min(272.0), |ui| {
                        ui.horizontal(|ui| {
                            ui.add_space((ui.available_width() - 80.0).max(0.0));
                            ui.label(format!("{} dB", if image.is_multiple_of(2) { -9 } else { -122 }));
                        });
                    });
                });
            });
        // La première mesure, puis plus rien : la largeur de la case vient du
        // partage, pas de ce qu'elle affiche.
        assert!(passes[2..].iter().all(|&p| p == 1), "{passes:?}");
    }

    /// Au-dessus de la ligne, un bloc dont la hauteur change à chaque image
    /// (une valeur en direct qui passe sur deux lignes, un bandeau qui
    /// apparaît) : la place qui reste sous lui change, la ligne non.
    #[test]
    fn la_place_au_dessus_change() {
        let mut image = 0u32;
        let p = passes(8, |ui| {
            image += 1;
            ui.add_space(if image.is_multiple_of(2) { 10.0 } else { 27.0 });
            ligne(ui, 0);
        });
        assert!(p[2..].iter().all(|&n| n == 1), "{p:?}");
    }

    #[test]
    fn ligne_seule() {
        let p = passes(8, |ui| ligne(ui, 0));
        assert!(p[2..].iter().all(|&n| n == 1), "{p:?}");
    }

    #[test]
    fn lignes_dans_un_defilement() {
        let p = passes(8, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for n in 0..6 {
                    ligne(ui, n);
                }
            });
        });
        assert!(p[2..].iter().all(|&n| n == 1), "{p:?}");
    }
}
