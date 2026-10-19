//! Le classement du groupe — V9.3 de PLAN-VALORANT-2.md, sur l'accueil et
//! dans son onglet. Un `ui::Tableau` : l'avatar, l'icône de rang, la
//! tendance, la forme et les 7 jours restent quoi qu'il arrive ; les autres
//! colonnes se masquent par priorité quand la place manque. Jamais de
//! défilement de côté.

use ki_ui::jetons::police;
use ki_ui::tableau::{Colonne, Tableau};

use super::*;
use crate::icons::Icon;

/// Ce que montre une colonne du classement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    Place,
    Membre,
    Rang,
    Tendance,
    Sept,
    Trente,
    Forme,
    Bilan,
    Kd,
    Acs,
    Adr,
    Kast,
    Tete,
    Matchs,
    /// La case « comparer ».
    Comparer,
}

/// Les colonnes, dans l'ordre : ce qu'elles montrent, leur titre, leur
/// largeur minimale, et quand elles se masquent — plus haut, plus tôt ;
/// 0, jamais. À 980 px partent la tête, le KAST et l'ADR ; à 720 px, il
/// reste le rang, la tendance, les 7 jours, la forme et le K/D.
const COLONNES: [(Col, &str, f32, u8); 15] = [
    (Col::Place, "#", 22.0, 0),
    (Col::Membre, "Membre", 130.0, 0),
    (Col::Rang, "Rang", 132.0, 0),
    (Col::Tendance, "Tendance", 68.0, 4),
    (Col::Sept, "7 j", 48.0, 0),
    (Col::Trente, "30 j", 48.0, 7),
    (Col::Forme, "Forme", 88.0, 3),
    (Col::Bilan, "Bilan", 108.0, 9),
    (Col::Kd, "K/D", 40.0, 5),
    (Col::Acs, "ACS", 40.0, 6),
    (Col::Adr, "ADR", 40.0, 10),
    (Col::Kast, "KAST", 44.0, 11),
    (Col::Tete, "Tête", 44.0, 12),
    (Col::Matchs, "Matchs", 48.0, 8),
    (Col::Comparer, "", 22.0, 0),
];

/// La colonne des 7 jours : le tri tant qu'on n'a cliqué sur rien.
const COL_SEPT: usize = 4;
/// La hauteur d'une ligne : l'avatar et l'icône de rang y respirent.
const HAUTEUR_LIGNE: f32 = 34.0;
const AVATAR: f32 = 22.0;
const ICONE_RANG: f32 = 22.0;

impl Col {
    fn droite(self) -> bool {
        matches!(self, Col::Sept | Col::Trente | Col::Kd | Col::Acs | Col::Adr | Col::Kast | Col::Tete | Col::Matchs)
    }

    /// La valeur qui trie une ligne : `None` pour une colonne qui ne trie
    /// pas, `Some(None)` pour une ligne sans valeur (un taux sans match),
    /// qui passe derrière les autres.
    fn cle(self, l: &Ligne, periode: Periode) -> Option<Option<f64>> {
        let b = l.b(periode);
        let r = &l.f.fiche.rang;
        Some(match self {
            Col::Rang => Some(f64::from(r.tier) * 1000.0 + f64::from(r.rr)),
            Col::Sept => Some(f64::from(l.bilan.sept_jours.rr)),
            Col::Trente => Some(f64::from(l.bilan.trente_jours.rr)),
            Col::Kd => b.kd().map(f64::from),
            Col::Acs => b.acs().map(f64::from),
            Col::Adr => b.adr().map(f64::from),
            Col::Kast => b.kast_pct().map(f64::from),
            Col::Tete => b.tete_pct().map(f64::from),
            Col::Matchs => Some(f64::from(b.matchs)),
            Col::Place | Col::Membre | Col::Tendance | Col::Forme | Col::Bilan | Col::Comparer => return None,
        })
    }

    fn colonne(self, titre: &'static str, largeur: f32, priorite: u8) -> Colonne<'static> {
        let mut c = Colonne::new(titre, largeur).priorite(priorite);
        if self == Col::Membre {
            c = c.grandit();
        }
        if self.droite() {
            c = c.droite();
        }
        if self == Col::Rang {
            c = c.chiffre();
        }
        if self.cle_possible() {
            c = c.triable();
        }
        c
    }

    fn cle_possible(self) -> bool {
        !matches!(self, Col::Place | Col::Membre | Col::Tendance | Col::Forme | Col::Bilan | Col::Comparer)
    }
}

fn tableau() -> Tableau<'static> {
    COLONNES
        .iter()
        .fold(Tableau::new("valo_classement"), |t, &(col, titre, largeur, priorite)| t.colonne(col.colonne(titre, largeur, priorite)))
        .hauteur_ligne(HAUTEUR_LIGNE)
        .tri_par_defaut(COL_SEPT, false)
}

impl PageValo {
    /// Le classement : une ligne par membre lié, triée par les RR des 7
    /// jours tant qu'on n'a pas cliqué un en-tête — le même tri sur
    /// l'accueil et dans l'onglet. Ceux qui n'ont pas joué sur la période
    /// passent en bas, en pâle. Un clic sur une ligne ouvre la fiche ;
    /// deux lignes cochées se comparent.
    pub(super) fn classement_tableau(
        &mut self,
        ui: &mut Ui,
        lignes: &[Ligne],
        noms: &Annuaire,
        cx: &Contexte<'_>,
        demandes: &mut Vec<Demande>,
    ) {
        let periode = self.periode;
        let maintenant = maintenant_ms();
        let (sept, trente) = (maintenant.saturating_sub(7 * JOUR_MS), maintenant.saturating_sub(30 * JOUR_MS));
        let tableau = tableau();
        let ordre = ordonner(lignes, tableau.tri(ui), periode);

        // Les deux cochés : de quoi les comparer.
        self.a_comparer.retain(|id| lignes.iter().any(|l| l.f.user_id == *id));
        let nom = |id: UserId| lignes.iter().find(|l| l.f.user_id == id).map_or_else(String::new, |l| l.f.username.clone());
        match *self.a_comparer.as_slice() {
            [a, b] => {
                let (na, nb) = (nom(a), nom(b));
                if ui::button(ui, Icon::Target, &format!("Comparer {na} et {nb}")).clicked() {
                    demandes.push(Demande::OuvrirFiche(a, na));
                    demandes.push(Demande::Comparer(b, nb));
                    self.a_comparer.clear();
                }
                ui.add_space(espace::S);
            }
            [_] => {
                ui::hint(ui, "Coche un second membre pour les comparer.");
                ui.add_space(espace::S);
            }
            _ => {}
        }
        let a_comparer = &mut self.a_comparer;

        let clic = tableau.show(ui, ordre.len(), |ui, i, colonne| {
            let l = ordre[i];
            let f = l.f;
            let b = l.b(periode);
            let moi = cx.moi == Some(f.user_id);
            let pale = l.sans_match(periode);
            let texte_ = if pale { TEXT_FAINT } else { TEXT };
            match COLONNES[colonne].0 {
                Col::Place => {
                    ui.label(RichText::new(format!("{}", i + 1)).color(if moi { ACCENT } else { TEXT_FAINT }).size(texte::PETIT));
                }
                Col::Membre => {
                    ui.spacing_mut().item_spacing.x = espace::S;
                    avatar(ui, &f.username, cx.photos.get(&f.user_id).map(|(_, t)| t), AVATAR);
                    let couleur = if pale { noms.couleur(f.user_id).gamma_multiply(0.6) } else { noms.couleur(f.user_id) };
                    let police = if moi { police::moyenne(ui.ctx(), texte::COURANT) } else { FontId::proportional(texte::COURANT) };
                    let mut survol = f.fiche.riot_id.clone();
                    if f.fiche.niveau > 0 {
                        survol.push_str(&format!(" · niveau {}", f.fiche.niveau));
                    }
                    if let Some(pic) = f.fiche.pic.as_ref().filter(|p| p.tier >= 3) {
                        survol.push_str(&format!(" · pic {}", nom_de_rang(pic.tier)));
                    }
                    ui.label(RichText::new(&f.username).font(police).color(couleur)).on_hover_text(survol);
                }
                Col::Rang => {
                    let r = &f.fiche.rang;
                    ui.spacing_mut().item_spacing.x = espace::XS;
                    match cx.rangs.texture(r.tier).filter(|_| r.tier >= 3) {
                        Some(icone) => {
                            ui.add(egui::Image::new(icone).fit_to_exact_size(Vec2::splat(ICONE_RANG)));
                        }
                        None => {
                            ui.allocate_exact_size(Vec2::splat(ICONE_RANG), Sense::hover());
                        }
                    }
                    if r.tier >= 3 {
                        ui.label(RichText::new(nom_de_rang(r.tier)).color(graphes::couleur_de_rang(r.tier)).size(texte::PETIT));
                        ui.label(RichText::new(format!("{} RR", r.rr)).color(TEXT_DIM).size(texte::PETIT));
                    } else {
                        ui::hint(ui, nom_de_rang(0));
                    }
                }
                Col::Tendance => {
                    // Les points reçus, du plus ancien au plus récent ; la
                    // teinte suit la somme des deltas, comme l'infobulle.
                    let mut points: Vec<&PointRR> = f.fiche.historique_rr.iter().collect();
                    points.sort_by_key(|p| p.date);
                    if points.len() >= 2 {
                        let valeurs: Vec<f32> = points.iter().map(|p| graphes::valeur_rr(p.tier, p.rr)).collect();
                        let total = points.iter().fold(0i32, |acc, p| acc.saturating_add(p.delta));
                        let (somme, teinte) = signe(total);
                        graphes::sparkline(ui, &valeurs, Vec2::new(60.0, 16.0), teinte)
                            .on_hover_text(format!("{somme} RR sur {}", pluriel(points.len() as u32, "classé")));
                    } else {
                        cellule(ui, TIRET, TEXT_FAINT);
                    }
                }
                Col::Sept => {
                    let (t, c) = rr_texte(l.bilan.sept_jours.rr, &f.fiche.historique_rr, sept);
                    ui.label(RichText::new(t).font(police::moyenne(ui.ctx(), texte::PETIT)).color(c));
                }
                Col::Trente => {
                    let (t, c) = rr_texte(l.bilan.trente_jours.rr, &f.fiche.historique_rr, trente);
                    cellule(ui, t, c);
                }
                Col::Forme => {
                    if l.bilan.forme.is_empty() {
                        cellule(ui, TIRET, TEXT_FAINT);
                    } else {
                        graphes::bande_forme(ui, &l.bilan.forme, Vec2::new(7.0, 10.0), None)
                            .on_hover_text("les dix derniers classés, le plus récent à droite");
                    }
                }
                Col::Bilan => {
                    cellule(ui, bilan_texte(b), texte_);
                }
                Col::Kd => {
                    cellule(ui, opt_dec(b.kd(), 2), texte_);
                }
                Col::Acs => {
                    cellule(ui, opt_entier(b.acs()), texte_);
                }
                Col::Adr => {
                    cellule(ui, opt_entier(b.adr()), texte_);
                }
                Col::Kast => {
                    cellule(ui, opt_pct(b.kast_pct()), texte_);
                }
                Col::Tete => {
                    cellule(ui, opt_pct(b.tete_pct()), texte_);
                }
                Col::Matchs => {
                    let matchs = if b.matchs > 0 { format!("{}", b.matchs) } else { TIRET.to_string() };
                    cellule(ui, matchs, if pale { TEXT_FAINT } else { TEXT_DIM });
                }
                Col::Comparer => {
                    let mut coche = a_comparer.contains(&f.user_id);
                    if ui.add(egui::Checkbox::without_text(&mut coche)).on_hover_text("le comparer à un autre membre").changed() {
                        cocher(a_comparer, f.user_id, coche);
                    }
                }
            }
        });
        if let Some(i) = clic {
            let f = ordre[i].f;
            demandes.push(Demande::OuvrirFiche(f.user_id, f.username.clone()));
        }
    }
}

/// Coche ou décoche un membre à comparer : deux au plus, le plus ancien
/// coché part quand on en coche un troisième.
fn cocher(liste: &mut Vec<UserId>, id: UserId, oui: bool) {
    liste.retain(|x| *x != id);
    if oui {
        liste.push(id);
        if liste.len() > 2 {
            liste.remove(0);
        }
    }
}

/// Les lignes dans l'ordre du tri choisi : ceux qui ont joué sur la
/// période d'abord, puis par la colonne, les sans-valeur derrière ; à
/// égalité, et pour une colonne qui ne trie pas, l'ordre reçu.
fn ordonner<'l, 'a>(lignes: &'l [Ligne<'a>], tri: Option<ki_ui::tableau::Tri>, periode: Periode) -> Vec<&'l Ligne<'a>> {
    let mut ordre: Vec<&Ligne> = lignes.iter().collect();
    let tri = tri.and_then(|t| COLONNES.get(t.colonne).map(|c| (c.0, !t.croissant)));
    match tri.filter(|(col, _)| col.cle_possible()) {
        Some((col, desc)) => trier(&mut ordre, |l| l.sans_match(periode), |l| col.cle(l, periode).flatten(), desc),
        None => ordre.sort_by_key(|l| l.sans_match(periode)),
    }
    ordre
}

/// L'avatar d'un membre, comme dans la liste des membres : sa photo en
/// rond, ou son initiale sur un disque teinté de sa couleur.
pub(super) fn avatar(ui: &mut Ui, nom: &str, photo: Option<&egui::TextureHandle>, taille: f32) -> Response {
    let (rect, reponse) = ui.allocate_exact_size(Vec2::splat(taille), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return reponse;
    }
    match photo {
        // Détourée par l'image elle-même : le fond d'une ligne rayée ou
        // survolée change, un anneau peint à sa couleur se verrait.
        Some(photo) => {
            egui::Image::new(photo).corner_radius(taille / 2.0).paint_at(ui, rect);
            ui.painter().circle_stroke(
                rect.center(),
                taille / 2.0 - 0.5,
                egui::Stroke::new(1.0, theme::alpha(theme::color_for(nom), 90)),
            );
        }
        None => ui::paint_avatar(ui.painter(), rect, nom, false, None, theme::BG_RAISED),
    }
    reponse
}

#[cfg(test)]
mod tests {
    use super::*;

    fn montrees(largeur: f32) -> Vec<Col> {
        let colonnes: Vec<Colonne> = COLONNES.iter().map(|&(col, titre, l, p)| col.colonne(titre, l, p)).collect();
        ki_ui::tableau::disposer(&colonnes, largeur, espace::M).iter().map(|c| COLONNES[c.0].0).collect()
    }

    /// À 980 px de page (884 px de tableau, une fois les marges de la page
    /// et de la section ôtées), seuls la tête, le KAST et l'ADR partent ; à
    /// 720 px (624), il reste l'essentiel ; ce qui ne part jamais reste.
    #[test]
    fn le_classement_garde_l_essentiel_a_chaque_largeur() {
        let reference = montrees(884.0);
        assert!(reference.contains(&Col::Bilan) && reference.contains(&Col::Matchs), "{reference:?}");
        for partie in [Col::Tete, Col::Kast, Col::Adr] {
            assert!(!reference.contains(&partie), "{partie:?} à 980 px : {reference:?}");
        }
        let etroit = montrees(624.0);
        assert_eq!(
            etroit,
            [Col::Place, Col::Membre, Col::Rang, Col::Tendance, Col::Sept, Col::Forme, Col::Kd, Col::Comparer],
            "à 720 px"
        );
        assert_eq!(montrees(1_400.0).len(), COLONNES.len(), "tout tient sur un grand écran");
        assert_eq!(montrees(200.0), [Col::Place, Col::Membre, Col::Rang, Col::Sept, Col::Comparer], "ce qui ne part jamais");
    }

    /// Chaque colonne qui trie, dans les deux sens : ceux qui ont joué
    /// d'abord, les valeurs dans l'ordre demandé, les sans-valeur derrière.
    #[test]
    fn l_ordre_suit_la_colonne_choisie() {
        use ki_ui::tableau::Tri;
        let maintenant = maintenant_ms();
        let stats = super::super::tests::groupe(maintenant, false);
        let (lignes, _) = lignes_du_groupe(&stats, maintenant);
        let periode = Periode::Trente;
        for (k, &(col, ..)) in COLONNES.iter().enumerate() {
            for croissant in [false, true] {
                let ordre = ordonner(&lignes, Some(Tri { colonne: k, croissant }), periode);
                assert_eq!(ordre.len(), lignes.len());
                let pales: Vec<bool> = ordre.iter().map(|l| l.sans_match(periode)).collect();
                assert!(pales.windows(2).all(|w| !w[0] || w[1]), "{col:?} : les pâles en bas");
                let joues: Vec<&&Ligne> = ordre.iter().filter(|l| !l.sans_match(periode)).collect();
                let cles: Vec<Option<f64>> = joues.iter().map(|l| col.cle(l, periode).flatten()).collect();
                assert!(cles.windows(2).all(|w| w[0].is_some() || w[1].is_none()), "{col:?} : les sans-valeur derrière");
                let valeurs: Vec<f64> = cles.into_iter().flatten().collect();
                let en_ordre = valeurs.windows(2).all(|w| if croissant { w[0] <= w[1] } else { w[0] >= w[1] });
                assert!(!col.cle_possible() || en_ordre, "{col:?} croissant={croissant} : {valeurs:?}");
            }
        }
        // Le rang du plus haut au plus bas : Bob, Immortel, en tête ; Wam,
        // sans match, en bas.
        let ordre = ordonner(&lignes, Some(Tri { colonne: 2, croissant: false }), periode);
        assert_eq!(ordre.first().map(|l| l.f.username.as_str()), Some("Bob"));
        assert_eq!(ordre.last().map(|l| l.f.username.as_str()), Some("Wam"));
        // Sans tri, l'ordre reçu, les pâles en bas.
        let ordre = ordonner(&lignes, None, periode);
        assert_eq!(ordre.iter().map(|l| l.f.user_id).collect::<Vec<_>>(), [1, 2, 4, 3]);
    }

    /// Le 7 jours trie par défaut ; les colonnes de dessin ne trient pas.
    #[test]
    fn seules_les_colonnes_de_chiffres_trient() {
        assert_eq!(COLONNES[COL_SEPT].0, Col::Sept);
        for (col, ..) in COLONNES {
            let attendu = !matches!(col, Col::Place | Col::Membre | Col::Tendance | Col::Forme | Col::Bilan | Col::Comparer);
            assert_eq!(col.cle_possible(), attendu, "{col:?}");
        }
    }

    /// Deux membres cochés au plus : un troisième fait partir le premier.
    #[test]
    fn on_coche_deux_membres_au_plus() {
        let mut liste = Vec::new();
        cocher(&mut liste, 1, true);
        cocher(&mut liste, 2, true);
        cocher(&mut liste, 2, true);
        assert_eq!(liste, [1, 2]);
        cocher(&mut liste, 3, true);
        assert_eq!(liste, [2, 3]);
        cocher(&mut liste, 2, false);
        assert_eq!(liste, [3]);
    }
}
