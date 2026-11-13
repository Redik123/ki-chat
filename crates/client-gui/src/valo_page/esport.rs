//! L'onglet Esport : les prochains matchs, tels que le serveur les a lus
//! chez HenrikDev — un mot s'il n'en a pas.
//!
//! Un `ui::Tableau` : le moment et l'affiche restent quoi qu'il arrive ; le
//! format, le tournoi puis la ligue se masquent quand la place manque. Un
//! nom de tournoi trop long se coupe, et se lit en entier au survol.

use chrono::{Datelike, NaiveDate, TimeZone};
use ki_ui::tableau::{Colonne, Tableau};

use super::*;
use crate::icons::Icon;

/// Ce que montre une colonne.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    Quand,
    Affiche,
    Ligue,
    Tournoi,
    Format,
}

/// Les colonnes, dans l'ordre : ce qu'elles montrent, leur titre, leur
/// largeur minimale, et quand elles se masquent — plus haut, plus tôt ; 0,
/// jamais. Le tournoi prend la place qui reste.
const COLONNES: [(Col, &str, f32, u8); 5] = [
    (Col::Quand, "Quand", 128.0, 0),
    (Col::Affiche, "Affiche", 120.0, 0),
    (Col::Ligue, "Ligue", 150.0, 1),
    (Col::Tournoi, "Tournoi", 130.0, 2),
    (Col::Format, "Format", 48.0, 3),
];

const JOURS_COURTS: [&str; 7] = ["lun.", "mar.", "mer.", "jeu.", "ven.", "sam.", "dim."];

fn colonne(col: Col, titre: &'static str, largeur: f32, priorite: u8) -> Colonne<'static> {
    let c = Colonne::new(titre, largeur).priorite(priorite);
    if col == Col::Tournoi {
        c.grandit()
    } else {
        c
    }
}

/// Le jour d'un match, vu d'aujourd'hui : « aujourd'hui », « demain », puis
/// le jour de la semaine et la date — « sam. 16/11 ».
fn jour_a_venir(jour: NaiveDate, aujourd_hui: NaiveDate) -> String {
    match (jour - aujourd_hui).num_days() {
        0 => "aujourd'hui".to_string(),
        1 => "demain".to_string(),
        _ => format!("{} {}", JOURS_COURTS[jour.weekday().num_days_from_monday() as usize], jour.format("%d/%m")),
    }
}

/// « demain · 21:00 », à l'heure d'ici.
fn quand(date_ms: u64) -> String {
    let Some(t) = chrono::Local.timestamp_millis_opt(date_ms as i64).single() else {
        return String::new();
    };
    format!("{} · {}", jour_a_venir(t.date_naive(), chrono::Local::now().date_naive()), t.format("%H:%M"))
}

pub(super) fn esports_ui(ui: &mut Ui, matchs: &[MatchEsport]) {
    ui::section(ui, Icon::Screen, "Esport", Some("les prochains matchs, lus chez HenrikDev par le serveur"), |ui| {
        if matchs.is_empty() {
            ui::hint(ui, "rien de prévu chez HenrikDev pour l'instant");
            return;
        }
        let tableau = COLONNES
            .iter()
            .fold(Tableau::new("valo_esport"), |t, &(col, titre, largeur, priorite)| t.colonne(col, colonne(col, titre, largeur, priorite)));
        tableau.show(ui, matchs.len(), |ui, i, col| {
            let m = &matchs[i];
            match col {
                Col::Quand => {
                    let texte = if m.etat == "inProgress" {
                        Texte::Aide.gras(ui, "en cours").color(SPEAK)
                    } else {
                        Texte::Aide.riche_ton(ui, Ton::Discret, quand(m.date))
                    };
                    ui.label(texte);
                }
                Col::Affiche => {
                    ui.spacing_mut().item_spacing.x = espace::S;
                    for (k, equipe) in m.equipes.iter().enumerate() {
                        if k > 0 {
                            ui.label(Texte::Aide.riche_ton(ui, Ton::Pale, "vs"));
                        }
                        ui.label(Texte::Corps.gras(ui, equipe));
                    }
                }
                Col::Ligue => {
                    let ligue = if m.region.is_empty() { m.ligue.clone() } else { format!("{} · {}", m.ligue, m.region) };
                    ui.label(RichText::new(&ligue).color(TEXT_DIM)).on_hover_text(ligue);
                }
                Col::Tournoi => {
                    ui.label(Texte::Aide.riche(ui, &m.tournoi)).on_hover_text(&m.tournoi);
                }
                Col::Format => {
                    if m.format.is_empty() {
                        cellule(ui, TIRET, TEXT_FAINT);
                    } else {
                        ui.label(Texte::Aide.riche(ui, &m.format));
                    }
                }
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn montrees(largeur: f32) -> Vec<Col> {
        let colonnes: Vec<Colonne> = COLONNES.iter().map(|&(col, titre, l, p)| colonne(col, titre, l, p)).collect();
        ki_ui::tableau::disposer(&colonnes, largeur, espace::M).iter().map(|c| COLONNES[c.0].0).collect()
    }

    /// Tout tient à 720 px de page ; plus étroit, le format part, puis le
    /// tournoi, puis la ligue ; le moment et l'affiche restent.
    #[test]
    fn l_esport_garde_le_moment_et_l_affiche() {
        assert_eq!(montrees(624.0).len(), COLONNES.len());
        assert_eq!(montrees(580.0), [Col::Quand, Col::Affiche, Col::Ligue, Col::Tournoi]);
        assert_eq!(montrees(500.0), [Col::Quand, Col::Affiche, Col::Ligue]);
        assert_eq!(montrees(300.0), [Col::Quand, Col::Affiche]);
        assert_eq!(montrees(100.0), [Col::Quand, Col::Affiche]);
    }

    /// Aujourd'hui et demain se disent ; au-delà, le jour de la semaine et
    /// la date — y compris d'un mois et d'une année sur l'autre.
    #[test]
    fn le_jour_d_un_match_se_dit_d_ici() {
        let jour = |a, m, j| NaiveDate::from_ymd_opt(a, m, j).unwrap();
        let vendredi = jour(2026, 11, 13);
        assert_eq!(jour_a_venir(vendredi, vendredi), "aujourd'hui");
        assert_eq!(jour_a_venir(jour(2026, 11, 14), vendredi), "demain");
        assert_eq!(jour_a_venir(jour(2026, 11, 15), vendredi), "dim. 15/11");
        assert_eq!(jour_a_venir(jour(2026, 11, 16), vendredi), "lun. 16/11");
        assert_eq!(jour_a_venir(jour(2027, 1, 1), jour(2026, 12, 31)), "demain");
        assert_eq!(jour_a_venir(jour(2027, 1, 2), jour(2026, 12, 31)), "sam. 02/01");
        // Un match d'hier que la liste porte encore : sa date, sans plus.
        assert_eq!(jour_a_venir(jour(2026, 11, 12), vendredi), "jeu. 12/11");
    }
}
