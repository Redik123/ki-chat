//! Le mouchard des mises en page instables, dans les versions de
//! développement seulement.
//!
//! Un conteneur flex dont la mise en page est refaite à chaque image fait
//! rejouer chaque image à egui — deux fois le travail, sans fin, et le
//! bandeau rouge « request_discard has been called … frames in a row ».
//! Le bandeau ne dit pas lequel. Le mouchard, si : au journal, le
//! conteneur fautif et, pour chacune de ses cases dont la taille bouge,
//! l'avant et l'après.

use egui::{Id, Ui, Vec2};

/// À partir de combien d'images de suite on le dit.
const DE_SUITE: u32 = 10;

#[cfg(test)]
thread_local! {
    /// Combien de fois le mouchard a parlé sur ce fil : les tests le
    /// lisent pour savoir s'il a crié, ou s'il s'est tu.
    pub(crate) static DITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Clone, Copy, Default)]
struct Suivi {
    image: u64,
    de_suite: u32,
    dit: bool,
    taille: Vec2,
}

/// Le conteneur `etiquette` a-t-il, pendant cette image, demandé à egui de
/// la rejouer ? `avant` : la demande était-elle déjà là avant lui ;
/// `largeur` : celle de sa zone d'accueil.
///
/// Une zone d'accueil qui change de largeur — une fenêtre, une colonne
/// qu'on tire — refait la mise en page à chaque image, et c'est voulu :
/// sans cela, la rangée aurait une image de retard sur le bord. Ces
/// images-là ne comptent pas.
pub(crate) fn conteneur(ui: &Ui, cle: Id, etiquette: &str, avant: bool, largeur: f32) {
    let id = cle.with("ki-ui-mouchard");
    let precedente = ui.data(|d| d.get_temp::<Suivi>(id)).map(|s| s.taille.x);
    let redimensionne = precedente.is_some_and(|p| (p - largeur).abs() > 0.5);
    let rejoue = !avant && ui.ctx().will_discard() && !redimensionne;
    compter(ui, id, rejoue, Vec2::new(largeur, 0.0), |n| {
        tracing::warn!(
            "ki-ui : la mise en page de « {etiquette} » est refaite à chaque image ({n} de suite) — \
             egui la rejoue sans fin ; la case qui bouge est signalée juste avant, et s'il n'y en a \
             pas, c'est le style ou le nombre de ses cases qui change à chaque image"
        );
    });
}

/// La taille qu'une case rapporte : si elle change à chaque image, c'est
/// elle qui relance le calcul.
pub(crate) fn case(ui: &Ui, cle: Id, etiquette: &str, rang: usize, taille: Vec2) {
    let id = cle.with(("ki-ui-mouchard-case", rang));
    let precedente = ui.data(|d| d.get_temp::<Suivi>(id)).map(|s| s.taille);
    let bouge = precedente.is_some_and(|p| p != taille);
    compter(ui, id, bouge, taille, |n| {
        tracing::warn!(
            "ki-ui : « {etiquette} », case {rang} : sa taille change à chaque image ({n} de suite) — \
             {:?} → {:?}",
            precedente.unwrap_or_default(),
            taille
        );
    });
}

/// La liste `etiquette` a-t-elle dû, à cette image, recaler son défilement
/// sur son ancre ? Une fois de temps en temps, c'est son travail ; à chaque
/// image, un élément change de hauteur sans arrêt et la liste se repeint
/// sans fin.
pub(crate) fn liste(ui: &Ui, cle: Id, etiquette: &str, recalee: bool) {
    compter(ui, cle.with("ki-ui-mouchard-liste"), recalee, Vec2::ZERO, |n| {
        tracing::warn!(
            "ki-ui : la liste « {etiquette} » se recale à chaque image ({n} de suite) — \
             un élément au-dessus de l'écran change de hauteur sans arrêt"
        );
    });
}

fn compter(ui: &Ui, id: Id, evenement: bool, taille: Vec2, dire: impl FnOnce(u32)) {
    let image = ui.ctx().cumulative_frame_nr();
    let mut suivi = ui.data(|d| d.get_temp::<Suivi>(id)).unwrap_or_default();
    if suivi.image != image {
        // Une image de plus : l'événement de la précédente compte, ou la
        // série s'arrête.
        if evenement {
            suivi.de_suite += 1;
        } else {
            suivi.de_suite = 0;
            suivi.dit = false;
        }
        suivi.image = image;
        if suivi.de_suite >= DE_SUITE && !suivi.dit {
            suivi.dit = true;
            #[cfg(test)]
            DITS.with(|d| d.set(d.get() + 1));
            dire(suivi.de_suite);
        }
    }
    suivi.taille = taille;
    ui.data_mut(|d| d.insert_temp(id, suivi));
}
