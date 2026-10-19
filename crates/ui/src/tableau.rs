//! Un tableau de données, décrit par ses colonnes : un classement, une
//! liste de matchs.
//!
//! - Chaque colonne a une largeur minimale et une priorité : quand la
//!   place manque, les moins prioritaires se masquent d'abord, et ce qui
//!   reste se partage entre celles qui grandissent.
//! - Un clic sur l'en-tête d'une colonne triable la trie, un second
//!   renverse le tri. Le tableau ne trie rien lui-même : il dit le tri
//!   choisi ([`Tableau::tri`]), et l'appelant range ses lignes.
//! - Les lignes sont rayées, s'éclairent au survol, et se cliquent.
//!
//! ```ignore
//! let tableau = Tableau::new("classement")
//!     .colonne(Colonne::new("Membre", 160.0).grandit())
//!     .colonne(Colonne::new("Rang", 90.0).priorite(2))
//!     .colonne(Colonne::new("7 j", 56.0).droite().triable())
//!     .tri_par_defaut(2, false);
//! let tri = tableau.tri(ui);
//! // … ranger `lignes` selon `tri` …
//! if let Some(i) = tableau.show(ui, lignes.len(), |ui, ligne, colonne| { … }) { … }
//! ```

use egui::{self, Align, Align2, Color32, CornerRadius, FontId, Id, Layout, Rect, Sense, Stroke, Ui, UiBuilder, Vec2};

use crate::jetons::couleur as theme;
use crate::jetons::{espace, rayon, texte};

/// Une colonne : son titre, sa largeur minimale, et comment elle se tient.
#[derive(Clone, Debug)]
pub struct Colonne<'a> {
    titre: &'a str,
    largeur: f32,
    grandit: bool,
    priorite: u8,
    droite: bool,
    triable: bool,
}

impl<'a> Colonne<'a> {
    pub fn new(titre: &'a str, largeur: f32) -> Self {
        Self { titre, largeur, grandit: false, priorite: 0, droite: false, triable: false }
    }

    /// Elle prend la place qui reste (le nom d'un membre).
    pub fn grandit(mut self) -> Self {
        self.grandit = true;
        self
    }

    /// Plus elle est haute, plus tôt la colonne se masque quand la place
    /// manque ; 0 : jamais.
    pub fn priorite(mut self, priorite: u8) -> Self {
        self.priorite = priorite;
        self
    }

    /// Alignée à droite : un chiffre.
    pub fn droite(mut self) -> Self {
        self.droite = true;
        self
    }

    /// Son en-tête se clique pour trier.
    pub fn triable(mut self) -> Self {
        self.triable = true;
        self
    }
}

/// Le tri choisi : une colonne (par son rang parmi toutes), dans un sens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tri {
    pub colonne: usize,
    pub croissant: bool,
}

/// Un clic sur l'en-tête de `colonne` : la trier — les chiffres du plus
/// grand au plus petit d'abord, les mots dans l'ordre —, ou renverser son
/// tri si elle l'est déjà.
pub fn basculer(tri: Option<Tri>, colonne: usize, chiffre: bool) -> Tri {
    match tri {
        Some(t) if t.colonne == colonne => Tri { colonne, croissant: !t.croissant },
        _ => Tri { colonne, croissant: !chiffre },
    }
}

/// Les colonnes montrées dans `largeur`, avec `ecart` entre elles : pour
/// chacune, son rang parmi toutes, son abscisse et sa largeur. Les plus
/// hautes priorités se masquent d'abord (la dernière déclarée avant les
/// autres, à priorité égale) ; la place qui reste va à celles qui
/// grandissent, ou à toutes s'il n'y en a pas.
pub fn disposer(colonnes: &[Colonne<'_>], largeur: f32, ecart: f32) -> Vec<(usize, f32, f32)> {
    let mut montrees: Vec<usize> = (0..colonnes.len()).collect();
    let besoin = |m: &[usize]| {
        m.iter().map(|&i| colonnes[i].largeur).sum::<f32>() + ecart * m.len().saturating_sub(1) as f32
    };
    while besoin(&montrees) > largeur {
        let Some(k) = montrees
            .iter()
            .enumerate()
            .filter(|(_, &i)| colonnes[i].priorite > 0)
            .max_by_key(|(k, &i)| (colonnes[i].priorite, *k))
            .map(|(k, _)| k)
        else {
            break;
        };
        montrees.remove(k);
    }
    let reste = (largeur - besoin(&montrees)).max(0.0);
    let qui_grandit: Vec<usize> = montrees.iter().copied().filter(|&i| colonnes[i].grandit).collect();
    let part = if qui_grandit.is_empty() { reste / montrees.len().max(1) as f32 } else { reste / qui_grandit.len() as f32 };
    let mut x = 0.0;
    montrees
        .into_iter()
        .map(|i| {
            let plus = if qui_grandit.is_empty() || colonnes[i].grandit { part } else { 0.0 };
            let w = colonnes[i].largeur + plus;
            let case = (i, x, w);
            x += w + ecart;
            case
        })
        .collect()
}

/// Un tableau, le temps d'une image.
pub struct Tableau<'a> {
    id: Id,
    colonnes: Vec<Colonne<'a>>,
    hauteur_ligne: f32,
    tri_defaut: Option<Tri>,
}

impl<'a> Tableau<'a> {
    /// La hauteur de l'en-tête.
    pub const HAUTEUR_TETE: f32 = 26.0;

    pub fn new(id_salt: impl egui::AsId) -> Self {
        Self { id: Id::new(("ki-tableau", id_salt)), colonnes: Vec::new(), hauteur_ligne: 32.0, tri_defaut: None }
    }

    pub fn colonne(mut self, colonne: Colonne<'a>) -> Self {
        self.colonnes.push(colonne);
        self
    }

    pub fn hauteur_ligne(mut self, hauteur: f32) -> Self {
        self.hauteur_ligne = hauteur;
        self
    }

    /// Le tri tant qu'on n'a cliqué sur aucun en-tête.
    pub fn tri_par_defaut(mut self, colonne: usize, croissant: bool) -> Self {
        self.tri_defaut = Some(Tri { colonne, croissant });
        self
    }

    /// Le tri choisi, gardé d'une image à l'autre.
    pub fn tri(&self, ui: &Ui) -> Option<Tri> {
        ui.data(|d| d.get_temp::<Tri>(self.id)).or(self.tri_defaut)
    }

    /// L'en-tête puis `n` lignes ; `cellule(ui, ligne, colonne)` remplit
    /// chaque case montrée, `colonne` comptée parmi toutes. Rend la ligne
    /// cliquée.
    pub fn show(self, ui: &mut Ui, n: usize, mut cellule: impl FnMut(&mut Ui, usize, usize)) -> Option<usize> {
        let largeur = ui.available_width();
        let ecart = espace::M;
        let cases = disposer(&self.colonnes, largeur - 2.0 * espace::M, ecart);
        let hauteur = Self::HAUTEUR_TETE + n as f32 * self.hauteur_ligne;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(largeur, hauteur), Sense::hover());
        let gauche = rect.left() + espace::M;
        let tri = self.tri(ui);

        // L'en-tête : les titres en capitales, la flèche du tri.
        let tete = Rect::from_min_size(rect.min, Vec2::new(largeur, Self::HAUTEUR_TETE));
        ui.painter().line_segment([tete.left_bottom(), tete.right_bottom()], Stroke::new(1.0_f32, theme::BORDER_SOFT));
        for &(i, x, w) in &cases {
            let c = &self.colonnes[i];
            let case = Rect::from_min_size(egui::pos2(gauche + x, tete.top()), Vec2::new(w, Self::HAUTEUR_TETE));
            let clic = c.triable
                && ui
                    .interact(case, self.id.with(("tete", i)), Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked();
            if clic {
                let nouveau = basculer(tri, i, c.droite);
                ui.data_mut(|d| d.insert_temp(self.id, nouveau));
            }
            let trie = tri.filter(|t| t.colonne == i);
            let couleur = if trie.is_some() { theme::TEXT_DIM } else { theme::TEXT_FAINT };
            let (ancre, px) = if c.droite { (Align2::RIGHT_CENTER, case.right()) } else { (Align2::LEFT_CENTER, case.left()) };
            let titre = ui.painter().text(
                egui::pos2(px, case.center().y),
                ancre,
                c.titre.to_uppercase(),
                FontId::proportional(texte::MINUSCULE),
                couleur,
            );
            if let Some(t) = trie {
                // La flèche, du côté où le titre a de la place.
                let cx = if c.droite { titre.left() - 7.0 } else { titre.right() + 7.0 };
                let cy = case.center().y;
                let pointe = if t.croissant {
                    vec![egui::pos2(cx, cy - 3.0), egui::pos2(cx + 3.5, cy + 2.5), egui::pos2(cx - 3.5, cy + 2.5)]
                } else {
                    vec![egui::pos2(cx, cy + 3.0), egui::pos2(cx - 3.5, cy - 2.5), egui::pos2(cx + 3.5, cy - 2.5)]
                };
                ui.painter().add(egui::Shape::convex_polygon(pointe, couleur, Stroke::NONE));
            }
        }

        // Les lignes : rayées, éclairées au survol, cliquables. Le texte
        // n'y est pas sélectionnable : il prendrait le clic de la ligne.
        let mut cliquee = None;
        for ligne in 0..n {
            let haut = tete.bottom() + ligne as f32 * self.hauteur_ligne;
            let bande = Rect::from_min_size(egui::pos2(rect.left(), haut), Vec2::new(largeur, self.hauteur_ligne));
            if !ui.is_rect_visible(bande) {
                continue;
            }
            let reponse = ui.interact(bande, self.id.with(("ligne", ligne)), Sense::click());
            let fond = if reponse.hovered() {
                theme::BG_HOVER
            } else if ligne % 2 == 1 {
                theme::translucide(Color32::WHITE, 5)
            } else {
                Color32::TRANSPARENT
            };
            if fond != Color32::TRANSPARENT {
                ui.painter().rect_filled(bande, CornerRadius::same(rayon::M), fond);
            }
            if reponse.clicked() {
                cliquee = Some(ligne);
            }
            for &(i, x, w) in &cases {
                let case = Rect::from_min_size(egui::pos2(gauche + x, haut), Vec2::new(w, self.hauteur_ligne));
                let mise = if self.colonnes[i].droite {
                    Layout::right_to_left(Align::Center)
                } else {
                    Layout::left_to_right(Align::Center)
                };
                let mut dedans = ui.new_child(UiBuilder::new().max_rect(case).layout(mise).id_salt((self.id, ligne, i)));
                dedans.set_clip_rect(case.intersect(ui.clip_rect()));
                dedans.style_mut().interaction.selectable_labels = false;
                dedans.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                cellule(&mut dedans, ligne, i);
            }
        }
        cliquee
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colonnes() -> Vec<Colonne<'static>> {
        vec![
            Colonne::new("Membre", 160.0).grandit(),
            Colonne::new("Rang", 90.0).priorite(1),
            Colonne::new("Forme", 80.0).priorite(2),
            Colonne::new("7 j", 50.0).droite().triable(),
        ]
    }

    /// Tout tient : la place qui reste va à la colonne qui grandit.
    #[test]
    fn la_place_qui_reste_va_a_qui_grandit() {
        let cases = disposer(&colonnes(), 600.0, 8.0);
        assert_eq!(cases.iter().map(|c| c.0).collect::<Vec<_>>(), [0, 1, 2, 3]);
        let total: f32 = cases.iter().map(|c| c.2).sum::<f32>() + 3.0 * 8.0;
        assert!((total - 600.0).abs() < 0.01, "{total}");
        assert!(cases[0].2 > 160.0 && cases[1].2 == 90.0, "{cases:?}");
        assert!((cases[3].1 + cases[3].2 - 600.0).abs() < 0.01, "la dernière finit au bord");
    }

    /// La place manque : la plus haute priorité part d'abord, puis la
    /// suivante ; celles à 0 restent quoi qu'il arrive.
    #[test]
    fn les_colonnes_se_masquent_par_priorite() {
        let montrees = |l| disposer(&colonnes(), l, 8.0).iter().map(|c| c.0).collect::<Vec<_>>();
        assert_eq!(montrees(400.0), [0, 1, 3], "la forme part la première");
        assert_eq!(montrees(250.0), [0, 3]);
        assert_eq!(montrees(50.0), [0, 3], "le nom et le chiffre restent");
        let sans_grandir = [Colonne::new("a", 50.0), Colonne::new("b", 50.0)];
        let cases = disposer(&sans_grandir, 200.0, 0.0);
        assert_eq!((cases[0].2, cases[1].2), (100.0, 100.0), "sans colonne qui grandit, toutes se partagent");
    }

    /// Un premier clic trie les chiffres du plus grand au plus petit et les
    /// mots dans l'ordre ; un second renverse.
    #[test]
    fn un_clic_trie_un_second_renverse() {
        let t = basculer(None, 3, true);
        assert_eq!(t, Tri { colonne: 3, croissant: false });
        assert_eq!(basculer(Some(t), 3, true), Tri { colonne: 3, croissant: true });
        assert_eq!(basculer(Some(t), 0, false), Tri { colonne: 0, croissant: true });
    }
}
