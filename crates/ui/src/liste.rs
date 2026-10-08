//! La liste virtualisée : une liste qui défile et ne construit que ce qui
//! se voit.
//!
//! Le fil de discussion construisait, à chaque image, tous les messages
//! chargés — des centaines de blocs mis en page pour en montrer une
//! vingtaine. [`Liste`] ne construit que ceux qui sont à l'écran, retient
//! la hauteur de chacun et réserve la place des autres.
//!
//! Trois choses qu'une liste naïve rate, et que celle-ci tient :
//! - **des hauteurs variables** : un élément est mesuré la première fois
//!   qu'il approche de l'écran ; d'ici là, il compte pour la moyenne de
//!   ceux déjà mesurés ;
//! - **une ancre** : l'élément en haut de l'écran ne bouge pas d'un pixel
//!   quand ce qui est au-dessus change de hauteur — une page de messages
//!   plus anciens ajoutée en tête, une estimation corrigée. La liste
//!   rattrape l'écart en déplaçant le défilement, dans la même image ;
//! - **le bas** : collée en bas, elle y reste quand des éléments
//!   s'ajoutent.
//!
//! Un en-tête ou un pied (« charger les messages plus anciens ») sont des
//! éléments comme les autres : à l'appelant de les compter.

use egui::{pos2, scroll_area, vec2, Id, IdMap, IdSet, Rect, ScrollArea, Ui, UiBuilder};

/// Une liste défilante d'éléments de hauteurs variables, dont seuls ceux à
/// l'écran sont construits.
///
/// ```ignore
/// let sortie = Liste::new("fil").coller_en_bas(true).show(
///     ui,
///     messages.len(),
///     |i| messages[i].id,
///     |ui, i| { ui.label(&messages[i].texte); },
/// );
/// ```
///
/// La clé d'un élément le suit d'une image à l'autre, même quand on en
/// insère avant lui : c'est sous elle qu'est retenue sa hauteur, et d'elle
/// que dérivent les identifiants egui de ce qu'il contient.
#[must_use = "une liste ne s'affiche qu'avec `show`"]
pub struct Liste {
    id_salt: Id,
    etiquette: String,
    hauteur_estimee: f32,
    ecart: Option<f32>,
    marge: Option<f32>,
    coller_en_bas: bool,
    aller_en_bas: bool,
}

/// Ce que la liste a fait à cette image.
#[derive(Clone, Copy, Debug)]
pub struct Sortie {
    /// Le défilement, en points depuis le haut du contenu.
    pub decalage: f32,
    /// La hauteur de tout le contenu, estimations comprises.
    pub hauteur_contenu: f32,
    /// La zone visible, à l'écran.
    pub rect: Rect,
    /// La vue est-elle tout en bas ?
    pub en_bas: bool,
    /// Combien d'éléments ont été construits pour être montrés.
    pub dessines: usize,
    /// Combien ont été mesurés sans être montrés, aux abords de l'écran.
    pub mesures: usize,
    /// L'identifiant de la [`ScrollArea`] sous-jacente.
    pub id_defilement: Id,
}

#[derive(Clone, Copy)]
struct Mesure {
    hauteur: f32,
    /// La largeur à laquelle on l'a prise : une hauteur dépend du retour à
    /// la ligne.
    largeur: f32,
}

#[derive(Clone, Default)]
struct Etat {
    mesures: IdMap<Mesure>,
    /// La largeur de l'image précédente. Tant qu'elle bouge — une fenêtre
    /// qu'on redimensionne —, on ne remesure pas ce qui est hors de vue :
    /// ce serait tout remesurer à chaque image.
    largeur: f32,
    /// L'élément du haut de l'écran, et où on l'a peint, en points depuis
    /// le haut du contenu.
    ancre: Option<(Id, f32)>,
    en_bas: bool,
    deja_vue: bool,
    defilement: Option<Id>,
    hauteur_contenu: f32,
}

impl Etat {
    /// La hauteur moyenne de ce qui a été mesuré : la meilleure estimation
    /// de ce qui ne l'a pas encore été.
    fn moyenne(&self) -> Option<f32> {
        if self.mesures.is_empty() {
            return None;
        }
        let somme: f32 = self.mesures.values().map(|m| m.hauteur).sum();
        Some(somme / self.mesures.len() as f32)
    }
}

impl Liste {
    pub fn new(id_salt: impl egui::AsIdSalt) -> Self {
        Self {
            etiquette: format!("{id_salt:?}"),
            id_salt: Id::new(id_salt),
            hauteur_estimee: 40.0,
            ecart: None,
            marge: None,
            coller_en_bas: false,
            aller_en_bas: false,
        }
    }

    /// La hauteur supposée d'un élément tant qu'aucun n'a été mesuré
    /// (40 par défaut). Ensuite, c'est la moyenne des mesures qui sert.
    pub fn hauteur_estimee(mut self, hauteur: f32) -> Self {
        self.hauteur_estimee = hauteur;
        self
    }

    /// L'espace entre deux éléments (par défaut, celui d'egui entre deux
    /// widgets).
    pub fn ecart(mut self, ecart: f32) -> Self {
        self.ecart = Some(ecart);
        self
    }

    /// Jusqu'où, au-dessus et au-dessous de l'écran, on mesure d'avance ce
    /// qui ne l'est pas encore (par défaut, une hauteur d'écran) : un
    /// élément qui entre à l'écran a déjà sa hauteur, il n'y a rien à
    /// rattraper.
    pub fn marge(mut self, marge: f32) -> Self {
        self.marge = Some(marge);
        self
    }

    /// Une liste qui commence en bas et y reste quand des éléments
    /// s'ajoutent, tant qu'on n'est pas remonté — un fil de discussion.
    pub fn coller_en_bas(mut self, oui: bool) -> Self {
        self.coller_en_bas = oui;
        self
    }

    /// Descend tout en bas à cette image (en entrant dans un salon, par
    /// exemple).
    pub fn aller_en_bas(mut self, oui: bool) -> Self {
        self.aller_en_bas = oui;
        self
    }

    /// Affiche les `nombre` éléments : `cle(i)` identifie le i-ème,
    /// `element(ui, i)` le dessine. `element` n'est appelé que pour ceux
    /// qui sont à l'écran, et pour mesurer, sans les montrer, ceux qui en
    /// approchent — `ui.is_visible()` les distingue.
    pub fn show<K: egui::AsIdSalt>(
        self,
        ui: &mut Ui,
        nombre: usize,
        cle: impl Fn(usize) -> K,
        mut element: impl FnMut(&mut Ui, usize),
    ) -> Sortie {
        let id = ui.make_persistent_id(self.id_salt);
        let mut etat = ui.data_mut(|d| d.remove_temp::<Etat>(id)).unwrap_or_default();
        if !etat.deja_vue {
            etat.deja_vue = true;
            etat.en_bas = self.coller_en_bas;
        }
        let au_bas = self.aller_en_bas || (self.coller_en_bas && etat.en_bas);
        let ecart = self.ecart.unwrap_or(ui.spacing().item_spacing.y);

        // Le plan : la hauteur de chacun, mesurée ou estimée, et où il
        // commence.
        let cles: Vec<Id> = (0..nombre).map(|i| id.with(cle(i))).collect();
        let estimee = etat.moyenne().unwrap_or(self.hauteur_estimee);
        let mut hauteurs: Vec<f32> =
            cles.iter().map(|c| etat.mesures.get(c).map_or(estimee, |m| m.hauteur)).collect();
        let (hauts, total) = cumuler(&hauteurs, ecart);

        // Le défilement, recalé AVANT de dessiner : si ce qui précède
        // l'ancre a changé de hauteur, la vue suit l'ancre, et rien ne
        // bouge à l'écran.
        let ancre_avant =
            etat.ancre.and_then(|(c, y)| cles.iter().position(|k| *k == c).map(|i| (i, y)));
        let hauteur_vue = ui.available_rect_before_wrap().height();
        let impose = if au_bas {
            Some((total - hauteur_vue).max(0.0))
        } else {
            let decalage =
                etat.defilement.and_then(|d| scroll_area::State::load(ui.ctx(), d)).map(|s| s.offset.y);
            match (ancre_avant, decalage) {
                (Some((i, y)), Some(d)) if (hauts[i] - y).abs() > 0.1 => Some((d + hauts[i] - y).max(0.0)),
                _ => None,
            }
        };
        let mut zone = ScrollArea::vertical()
            .id_salt(id.with("defilement"))
            .auto_shrink(false)
            .stick_to_bottom(self.coller_en_bas);
        if let Some(d) = impose {
            zone = zone.vertical_scroll_offset(d);
        }

        let marge_voulue = self.marge;
        let mut dessines = 0;
        let mut mesures = 0;
        let mut nouvelle_ancre = None;
        let mut recalee = false;
        let sortie = zone.show_viewport(ui, |ui, vue| {
            let origine = ui.max_rect().min;
            let largeur = ui.max_rect().width();
            let largeur_stable = (largeur - etat.largeur).abs() <= 0.5;
            etat.largeur = largeur;
            if nombre == 0 {
                etat.hauteur_contenu = 0.0;
                return;
            }
            let marge = marge_voulue.unwrap_or_else(|| vue.height().max(200.0));

            // Dessine — ou mesure seulement — l'élément `i` à `y` points
            // du haut du contenu ; rend sa hauteur. La mesure a ses propres
            // identifiants : un élément peut être mesuré puis dessiné dans
            // la même image sans que ses widgets se marchent dessus.
            let mut poser = |ui: &mut Ui, i: usize, y: f32, montrer: bool| -> f32 {
                let rect =
                    Rect::from_min_size(pos2(origine.x, origine.y + y), vec2(largeur, vue.height().max(1.0)));
                let cadre = if montrer {
                    UiBuilder::new().id(cles[i]).max_rect(rect)
                } else {
                    UiBuilder::new().id(cles[i].with("mesure")).max_rect(rect).invisible()
                };
                let mut enfant = ui.new_child(cadre);
                element(&mut enfant, i);
                (enfant.min_rect().bottom() - rect.top()).max(0.0)
            };
            let a_mesurer = |etat: &Etat, i: usize| match etat.mesures.get(&cles[i]) {
                None => true,
                Some(m) => largeur_stable && (m.largeur - largeur).abs() > 0.5,
            };

            // L'ancre : celle de l'image précédente si elle est encore à
            // l'écran, sinon le premier élément visible.
            let visible = |i: usize| hauts[i] < vue.max.y && hauts[i] + hauteurs[i] > vue.min.y;
            let ia = match ancre_avant {
                Some((i, _)) if !au_bas && visible(i) => i,
                _ => premier_visible(&hauts, &hauteurs, vue.min.y).min(nombre - 1),
            };

            // Au-dessus de l'ancre, en remontant. L'ancre ne bouge pas,
            // quoi qu'on découvre au-dessus d'elle : ce qui entre à l'écran
            // par le haut est mesuré d'abord, puis posé exactement au-dessus
            // — sa hauteur retenue a pu vieillir hors de vue (une réaction,
            // un aperçu de lien chargé entre-temps). Plus haut, hors de vue,
            // on mesure seulement ce qui ne l'a jamais été.
            let mut au_dessus = Vec::new();
            let mut y = hauts[ia];
            let mut i = ia;
            while i > 0 && y - ecart > vue.min.y - marge {
                i -= 1;
                let bas = y - ecart;
                if bas > vue.min.y {
                    hauteurs[i] = poser(ui, i, bas - hauteurs[i], false);
                    etat.mesures.insert(cles[i], Mesure { hauteur: hauteurs[i], largeur });
                    mesures += 1;
                    au_dessus.push((i, bas - hauteurs[i]));
                } else if a_mesurer(&etat, i) {
                    hauteurs[i] = poser(ui, i, bas - hauteurs[i], false);
                    etat.mesures.insert(cles[i], Mesure { hauteur: hauteurs[i], largeur });
                    mesures += 1;
                }
                y = bas - hauteurs[i];
            }

            // Ce qui se voit, de haut en bas.
            let mut peints = Vec::new();
            for &(i, haut) in au_dessus.iter().rev() {
                hauteurs[i] = poser(ui, i, haut, true);
                etat.mesures.insert(cles[i], Mesure { hauteur: hauteurs[i], largeur });
                peints.push((i, haut));
            }
            let mut y = hauts[ia];
            let mut i = ia;
            while i < nombre && (i == ia || y < vue.max.y) {
                hauteurs[i] = poser(ui, i, y, true);
                etat.mesures.insert(cles[i], Mesure { hauteur: hauteurs[i], largeur });
                peints.push((i, y));
                y += hauteurs[i] + ecart;
                i += 1;
            }
            dessines = peints.len();

            // Sous l'écran : mesurer d'avance.
            while i < nombre && y < vue.max.y + marge {
                if a_mesurer(&etat, i) {
                    hauteurs[i] = poser(ui, i, y, false);
                    etat.mesures.insert(cles[i], Mesure { hauteur: hauteurs[i], largeur });
                    mesures += 1;
                }
                y += hauteurs[i] + ecart;
                i += 1;
            }

            // La prochaine ancre : le premier élément peint qui se voit.
            let (k, haut_k) = peints
                .iter()
                .copied()
                .find(|&(i, haut)| haut + hauteurs[i] > vue.min.y)
                .unwrap_or(peints[0]);
            nouvelle_ancre = Some((cles[k], haut_k));

            // Le contenu, tel qu'il est peint : l'ancre à sa place, le reste
            // autour d'elle avec les hauteurs à jour. S'il ne commence plus
            // en haut, l'image suivante recale le défilement d'autant.
            let (hauts_a_jour, total_a_jour) = cumuler(&hauteurs, ecart);
            let decale = haut_k - hauts_a_jour[k];
            let hauteur_contenu = (total_a_jour + decale).max(0.0);
            ui.set_min_size(vec2(largeur, hauteur_contenu));

            recalee = decale.abs() > 0.5;
            if recalee || (hauteur_contenu - etat.hauteur_contenu).abs() > 0.5 {
                ui.ctx().request_repaint();
            }
            etat.hauteur_contenu = hauteur_contenu;
        });

        #[cfg(debug_assertions)]
        crate::mouchard::liste(ui, id, &self.etiquette, recalee);
        #[cfg(not(debug_assertions))]
        let _ = (&self.etiquette, recalee);

        let decalage = sortie.state.offset.y;
        let fond = (sortie.content_size.y - sortie.inner_rect.height()).max(0.0);
        etat.en_bas = if au_bas {
            // Envoyée en bas à cette image, elle y reste tant qu'on n'est pas
            // remonté : les éléments mesurés en chemin ont pu repousser le
            // fond (ou le rapprocher), le décalage, lui, n'a pas reculé.
            decalage >= impose.unwrap_or(0.0).min(fond) - 0.5
        } else {
            decalage >= fond - 1.0
        };
        etat.defilement = Some(sortie.id);
        etat.ancre = nouvelle_ancre;
        // Le cache ne garde que ce qui est encore dans la liste : remonter
        // des mois de conversation n'y laisse pas une entrée par message lu.
        if etat.mesures.len() > 2 * nombre + 64 {
            let vivantes: IdSet = cles.iter().copied().collect();
            etat.mesures.retain(|c, _| vivantes.contains(c));
        }
        let en_bas = etat.en_bas;
        ui.data_mut(|d| d.insert_temp(id, etat));

        Sortie {
            decalage,
            hauteur_contenu: sortie.content_size.y,
            rect: sortie.inner_rect,
            en_bas,
            dessines,
            mesures,
            id_defilement: sortie.id,
        }
    }
}

/// Où commence chaque élément, et la hauteur du tout.
fn cumuler(hauteurs: &[f32], ecart: f32) -> (Vec<f32>, f32) {
    let mut hauts = Vec::with_capacity(hauteurs.len());
    let mut y = 0.0;
    for h in hauteurs {
        hauts.push(y);
        y += h + ecart;
    }
    let total = if hauteurs.is_empty() { 0.0 } else { y - ecart };
    (hauts, total)
}

/// Le premier élément dont le bas descend sous `y` — une recherche
/// dichotomique : les bas sont croissants.
fn premier_visible(hauts: &[f32], hauteurs: &[f32], y: f32) -> usize {
    let (mut bas, mut haut) = (0, hauts.len());
    while bas < haut {
        let milieu = (bas + haut) / 2;
        if hauts[milieu] + hauteurs[milieu] <= y {
            bas = milieu + 1;
        } else {
            haut = milieu;
        }
    }
    bas
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Context, Pos2, RawInput, Sense, Vec2};

    /// Un écran de test.
    struct Banc {
        ctx: Context,
        taille: Vec2,
        /// Ce qui s'ajoute à la hauteur de certains éléments : leur contenu
        /// a changé.
        en_plus: std::cell::RefCell<std::collections::HashMap<u64, f32>>,
    }

    /// Ce qu'une image a montré.
    struct Image {
        sortie: Sortie,
        /// (clé, rectangle à l'écran) de chaque élément montré, de haut en
        /// bas.
        montres: Vec<(u64, Rect)>,
        passes: usize,
    }

    impl Image {
        /// Le premier élément montré qui se voit : celui qu'on lit.
        fn lu(&self) -> (u64, Rect) {
            *self.montres.iter().find(|(_, r)| r.bottom() > self.sortie.rect.top()).expect("rien à l'écran")
        }

        fn ou(&self, cle: u64) -> Rect {
            self.montres.iter().find(|(c, _)| *c == cle).map(|(_, r)| *r).expect("élément absent de l'écran")
        }
    }

    /// La hauteur d'un élément, pour une largeur de 500 : de 20 à 80
    /// points, et d'autant plus haut que la liste est étroite, comme un
    /// texte qui passe à la ligne.
    fn hauteur(cle: u64, largeur: f32) -> f32 {
        (20.0 + ((cle * 37) % 61) as f32) * 500.0 / largeur
    }

    fn serie(cles: std::ops::Range<u64>) -> Vec<u64> {
        cles.collect()
    }

    impl Banc {
        fn new(largeur: f32, hauteur: f32) -> Self {
            Self { ctx: Context::default(), taille: vec2(largeur, hauteur), en_plus: Default::default() }
        }

        fn image(&self, elements: &[u64], reglage: impl Fn(Liste) -> Liste) -> Image {
            let entree = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.taille)),
                ..Default::default()
            };
            let mut montres = Vec::new();
            let mut sortie = None;
            let fin = self.ctx.run_ui(entree, |ui| {
                montres.clear();
                let liste = reglage(Liste::new("banc").ecart(6.0));
                sortie = Some(liste.show(ui, elements.len(), |i| elements[i], |ui, i| {
                    let h = hauteur(elements[i], ui.available_width())
                        + self.en_plus.borrow().get(&elements[i]).copied().unwrap_or(0.0);
                    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
                    if ui.is_visible() {
                        montres.push((elements[i], rect));
                    }
                }));
            });
            let passes = fin.platform_output.num_completed_passes;
            fin.drop_without_applying_deltas();
            montres.sort_by(|a, b| a.1.top().total_cmp(&b.1.top()));
            Image { sortie: sortie.expect("pas de liste"), montres, passes }
        }

        /// Défile comme le ferait la molette : seul le décalage change.
        fn defiler(&self, image: &Image, decalage: f32) {
            let id = image.sortie.id_defilement;
            let mut etat = scroll_area::State::load(&self.ctx, id).expect("pas de défilement");
            etat.offset.y = decalage;
            etat.store(&self.ctx, id);
        }

        fn images(&self, n: usize, elements: &[u64], reglage: impl Fn(Liste) -> Liste) -> Image {
            let mut derniere = self.image(elements, &reglage);
            for _ in 1..n {
                derniere = self.image(elements, &reglage);
            }
            derniere
        }
    }

    /// Ce qui est montré se suit sans trou ni chevauchement, et couvre
    /// l'écran.
    fn bien_range(image: &Image) {
        for paire in image.montres.windows(2) {
            let (a, b) = (paire[0].1, paire[1].1);
            assert!(
                (b.top() - a.bottom() - 6.0).abs() < 0.5,
                "{} puis {} : {:?} puis {:?}",
                paire[0].0,
                paire[1].0,
                a,
                b
            );
        }
        let vue = image.sortie.rect;
        let premier = image.montres.first().expect("rien à l'écran").1;
        let dernier = image.montres.last().unwrap().1;
        // L'écart entre deux éléments peut tomber au bord : ce n'est pas un
        // blanc.
        assert!(premier.top() - 6.0 <= vue.top() + 1.0 || image.sortie.decalage < 0.5, "un blanc en haut : {premier:?}");
        assert!(dernier.bottom() + 6.0 >= vue.bottom() - 1.0 || image.sortie.en_bas, "un blanc en bas : {dernier:?}");
    }

    #[test]
    fn ne_construit_que_ce_qui_se_voit() {
        let banc = Banc::new(500.0, 400.0);
        let elements = serie(0..1000);
        let image = banc.images(6, &elements, |l| l.coller_en_bas(true));
        assert!(image.sortie.en_bas);
        assert!(image.sortie.dessines <= 400 / 20 + 2, "{} construits", image.sortie.dessines);
        assert_eq!(image.sortie.mesures, 0, "tout est mesuré, rien à refaire");
        let (cle, rect) = *image.montres.last().unwrap();
        assert_eq!(cle, 999);
        assert!((rect.bottom() - image.sortie.rect.bottom()).abs() < 1.0, "{rect:?}");
        bien_range(&image);
        assert_eq!(image.passes, 1);
    }

    #[test]
    fn au_milieu_tout_se_suit() {
        let banc = Banc::new(500.0, 400.0);
        let elements = serie(0..1000);
        let image = banc.image(&elements, |l| l);
        banc.defiler(&image, 20_000.0);
        let image = banc.images(3, &elements, |l| l);
        bien_range(&image);
        assert!(image.sortie.dessines <= 400 / 20 + 2);
    }

    #[test]
    fn une_page_ajoutee_au_dessus_ne_fait_rien_bouger() {
        let banc = Banc::new(500.0, 400.0);
        let mut elements = serie(1000..1300);
        let image = banc.image(&elements, |l| l);
        banc.defiler(&image, 6000.0);
        let avant = banc.images(3, &elements, |l| l);
        let (cle, rect) = avant.lu();

        // Cent éléments jamais vus arrivent en tête : la ligne qu'on lit
        // reste où elle est, dès cette image.
        elements.splice(0..0, serie(900..1000));
        let apres = banc.image(&elements, |l| l);
        assert!((apres.ou(cle).top() - rect.top()).abs() < 0.5, "{:?} → {:?}", rect, apres.ou(cle));
        assert!(apres.sortie.decalage > avant.sortie.decalage + 100.0 * 26.0);
        bien_range(&apres);
        let encore = banc.image(&elements, |l| l);
        assert!((encore.ou(cle).top() - rect.top()).abs() < 0.5);
        assert_eq!(apres.passes, 1);
    }

    #[test]
    fn remonter_dans_l_inconnu_ne_fait_pas_sauter() {
        let banc = Banc::new(500.0, 400.0);
        let elements = serie(0..400);
        // Une estimation très fausse : chaque élément découvert en remontant
        // corrige le plan de 10 à 70 points.
        let mut image = banc.images(4, &elements, |l| l.aller_en_bas(true).hauteur_estimee(10.0));
        for pas in 0..60 {
            let (cle, rect) = image.lu();
            banc.defiler(&image, image.sortie.decalage - 150.0);
            let suivante = banc.image(&elements, |l| l.hauteur_estimee(10.0));
            if suivante.sortie.decalage < 0.5 {
                break; // tout en haut : plus rien à rattraper
            }
            let ici = suivante.ou(cle);
            assert!(
                (ici.top() - (rect.top() + 150.0)).abs() < 1.0,
                "pas {pas} : l'élément {cle} devait descendre de 150, de {:?} à {:?}",
                rect.top(),
                ici.top()
            );
            assert_eq!(suivante.passes, 1);
            image = suivante;
        }
    }

    #[test]
    fn ce_qui_a_change_hors_de_vue_ne_fait_pas_sauter() {
        let banc = Banc::new(500.0, 400.0);
        let elements = serie(0..400);
        let image = banc.image(&elements, |l| l);
        banc.defiler(&image, 8000.0);
        let mut image = banc.images(3, &elements, |l| l);
        // Tout ce qui est au-dessus de l'écran, déjà mesuré, grandit : une
        // réaction, un aperçu de lien arrivé pendant qu'on lisait plus bas.
        let (lu, _) = image.lu();
        banc.en_plus.borrow_mut().extend((0..lu).map(|c| (c, 40.0)));
        for pas in 0..20 {
            let (cle, rect) = image.lu();
            banc.defiler(&image, image.sortie.decalage - 100.0);
            let suivante = banc.image(&elements, |l| l);
            let ici = suivante.ou(cle);
            assert!(
                (ici.top() - (rect.top() + 100.0)).abs() < 1.0,
                "pas {pas} : l'élément {cle} devait descendre de 100, de {:?} à {:?}",
                rect.top(),
                ici.top()
            );
            bien_range(&suivante);
            image = suivante;
        }
    }

    #[test]
    fn collee_en_bas_elle_suit_ce_qui_arrive() {
        let banc = Banc::new(500.0, 400.0);
        let mut elements = serie(0..50);
        banc.images(4, &elements, |l| l.coller_en_bas(true));
        for cle in 50..70 {
            elements.push(cle);
            let image = banc.images(2, &elements, |l| l.coller_en_bas(true));
            let (derniere, rect) = *image.montres.last().unwrap();
            assert_eq!(derniere, cle);
            assert!((rect.bottom() - image.sortie.rect.bottom()).abs() < 1.0, "{cle} : {rect:?}");
            assert!(image.sortie.en_bas);
        }
    }

    #[test]
    fn remontee_elle_ne_redescend_pas_seule() {
        let banc = Banc::new(500.0, 400.0);
        let mut elements = serie(0..200);
        let image = banc.images(4, &elements, |l| l.aller_en_bas(true));
        banc.defiler(&image, image.sortie.decalage - 1000.0);
        let avant = banc.images(2, &elements, |l| l);
        let (cle, rect) = avant.lu();
        elements.extend(200..210);
        let apres = banc.images(2, &elements, |l| l);
        assert!(!apres.sortie.en_bas);
        assert!((apres.ou(cle).top() - rect.top()).abs() < 0.5);
    }

    #[test]
    fn aller_en_bas_y_va() {
        let banc = Banc::new(500.0, 400.0);
        let elements = serie(0..300);
        banc.images(3, &elements, |l| l);
        let image = banc.images(3, &elements, |l| l.aller_en_bas(true));
        assert!(image.sortie.en_bas);
        assert_eq!(image.montres.last().unwrap().0, 299);
    }

    /// Entrer dans un salon où l'on était remonté : une seule image
    /// `aller_en_bas`, avec des éléments jamais mesurés, et l'estimation
    /// fausse dans un sens ou dans l'autre — le fil doit finir en bas et y
    /// rester.
    #[test]
    fn aller_en_bas_une_seule_image_suffit() {
        for estimee in [10.0, 200.0] {
            let banc = Banc::new(500.0, 400.0);
            let avant = serie(0..300);
            banc.images(3, &avant, |l| l.coller_en_bas(true).aller_en_bas(true));
            // Remonté : deux images sans coller, pour qu'egui oublie lui
            // aussi qu'il était en bas — ce que fait la molette.
            let image = banc.image(&avant, |l| l);
            banc.defiler(&image, 2000.0);
            banc.images(2, &avant, |l| l);
            let image = banc.images(2, &avant, |l| l.coller_en_bas(true));
            assert!(!image.sortie.en_bas);

            let salon = serie(5000..5300);
            banc.image(&salon, |l| l.coller_en_bas(true).aller_en_bas(true).hauteur_estimee(estimee));
            let image = banc.images(4, &salon, |l| l.coller_en_bas(true).hauteur_estimee(estimee));
            assert!(image.sortie.en_bas, "estimation {estimee} : plus en bas");
            let (derniere, rect) = *image.montres.last().unwrap();
            assert_eq!(derniere, 5299, "estimation {estimee}");
            assert!((rect.bottom() - image.sortie.rect.bottom()).abs() < 1.0, "estimation {estimee} : {rect:?}");
        }
    }

    #[test]
    fn redimensionner_garde_la_ligne_sans_tout_remesurer() {
        let mut banc = Banc::new(600.0, 400.0);
        let elements = serie(0..1000);
        let image = banc.image(&elements, |l| l);
        banc.defiler(&image, 25_000.0);
        let image = banc.images(3, &elements, |l| l);
        let (cle, rect) = image.lu();
        for pas in 1..=20 {
            banc.taille.x = 600.0 - 10.0 * pas as f32;
            let image = banc.image(&elements, |l| l);
            // Chaque largeur change toutes les hauteurs : seul ce qui se
            // voit est reconstruit.
            assert!(image.sortie.dessines <= 400 / 20 + 2, "{} construits", image.sortie.dessines);
            assert_eq!(image.sortie.mesures, 0, "rien hors de vue pendant qu'on tire le bord");
            assert!(
                (image.ou(cle).top() - rect.top()).abs() < 1.0,
                "pas {pas} : la ligne a bougé, de {:?} à {:?} ; montrés {:?}",
                rect,
                image.ou(cle),
                &image.montres[..3]
            );
            assert_eq!(image.passes, 1);
        }
        // Le bord lâché, les abords se remettent à jour, une fois.
        let image = banc.image(&elements, |l| l);
        assert!(image.sortie.mesures > 0);
        assert!(image.sortie.mesures <= 2 * 400 / 20 + 4, "{} mesurés", image.sortie.mesures);
        let image = banc.image(&elements, |l| l);
        assert_eq!(image.sortie.mesures, 0);
        bien_range(&image);
    }

    #[test]
    fn vide_ou_seul() {
        let banc = Banc::new(500.0, 400.0);
        let image = banc.images(2, &[], |l| l.coller_en_bas(true));
        assert_eq!(image.sortie.dessines, 0);
        let image = banc.images(3, &[7], |l| l.coller_en_bas(true));
        assert_eq!(image.montres.len(), 1);
        assert!(image.sortie.en_bas);
    }
}
