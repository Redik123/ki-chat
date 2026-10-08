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

#[derive(Clone, Copy, Default)]
struct Suivi {
    image: u64,
    de_suite: u32,
    dit: bool,
    taille: Vec2,
}

/// Le conteneur `etiquette` a-t-il, pendant cette image, demandé à egui de
/// la rejouer ? `avant` : la demande était-elle déjà là avant lui.
pub(crate) fn conteneur(ui: &Ui, cle: Id, etiquette: &str, avant: bool) {
    let rejoue = !avant && ui.ctx().will_discard();
    compter(ui, cle.with("ki-ui-mouchard"), rejoue, Vec2::ZERO, |n| {
        tracing::warn!(
            "ki-ui : la mise en page de « {etiquette} » est refaite à chaque image ({n} de suite) — \
             egui la rejoue sans fin ; la case qui bouge est signalée juste avant"
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
            dire(suivi.de_suite);
        }
    }
    suivi.taille = taille;
    ui.data_mut(|d| d.insert_temp(id, suivi));
}
