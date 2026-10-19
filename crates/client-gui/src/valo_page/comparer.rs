//! Comparer deux membres — V9.4 de PLAN-VALORANT-2.md (c'était le bonus
//! V5.1), une vue du profil. Le membre du profil à gauche, en bleu ;
//! l'autre à droite, en orange ; la même période et le même mode pour les
//! deux :
//! - face à face, chiffre par chiffre ;
//! - leurs courbes de RR superposées ;
//! - quand ils jouent ensemble, et l'un contre l'autre ;
//! - les agents et les cartes qu'ils ont en commun.
//!
//! L'autre se choisit ici, ou en cochant deux lignes du classement.

use ki_ui::jetons::{largeur, police};

use super::*;
use crate::icons::Icon;

/// Les couleurs des deux membres comparés.
const COULEUR_A: Color32 = theme::INFO;
const COULEUR_B: Color32 = theme::WARN;
const HAUTEUR_COURBES: f32 = 220.0;
/// Assez de parties avec et sans l'autre pour les opposer.
const ASSEZ_ENSEMBLE: u16 = 3;
/// Les colonnes d'une ligne du face-à-face.
const COLONNE_INTITULE: f32 = 128.0;
const COLONNE_VALEUR: f32 = 112.0;
const HAUTEUR_DUEL: f32 = 26.0;

// ---------------------------------------------------------------------
// Ce que deux fiches disent ensemble
// ---------------------------------------------------------------------

/// Ce que deux membres ont joué ensemble, sur la sélection de chacun.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Ensemble {
    /// Dans le même camp : le bilan de A, celui de B.
    pub a_avec: Bilan,
    pub b_avec: Bilan,
    /// Chacun sans l'autre, sur le reste de sa sélection.
    pub a_sans: Bilan,
    pub b_sans: Bilan,
    /// L'un contre l'autre : les victoires de A, celles de B.
    pub contre: (u16, u16),
}

/// Le camp de B dans un match de A : le même, l'autre, ou on ne sait pas.
/// Le match le dit (`avec`, `contre`), sinon le même match chez B : le même
/// score vu du même côté, c'est le même camp.
fn meme_camp(m: &MatchResume, b_id: UserId, chez_b: Option<&MatchResume>) -> Option<bool> {
    if m.avec.contains(&b_id) {
        return Some(true);
    }
    if m.contre.contains(&b_id) {
        return Some(false);
    }
    let mb = chez_b?;
    if m.manches.0 == m.manches.1 {
        // Un nul ne dit pas de quel côté était chacun.
        return None;
    }
    if mb.manches == m.manches {
        Some(true)
    } else if mb.manches == (m.manches.1, m.manches.0) {
        Some(false)
    } else {
        None
    }
}

/// Les matchs d'une fiche par leur id.
fn par_id<'a>(v: &Vue<'a>) -> HashMap<&'a str, &'a MatchResume> {
    v.fiche.matchs.iter().filter(|m| !m.id.is_empty()).map(|m| (m.id.as_str(), m)).collect()
}

pub(super) fn ensemble(a: &Vue, a_id: UserId, b: &Vue, b_id: UserId) -> Ensemble {
    let (chez_a, chez_b) = (par_id(a), par_id(b));
    let mut e = Ensemble::default();
    for m in a.comptes() {
        match meme_camp(m, b_id, chez_b.get(m.id.as_str()).copied()) {
            Some(true) => e.a_avec.ajouter(m),
            Some(false) => match m.gagne {
                Some(true) => e.contre.0 += 1,
                Some(false) => e.contre.1 += 1,
                None => {}
            },
            None => e.a_sans.ajouter(m),
        }
    }
    for m in b.comptes() {
        match meme_camp(m, a_id, chez_a.get(m.id.as_str()).copied()) {
            Some(true) => e.b_avec.ajouter(m),
            Some(false) => {}
            None => e.b_sans.ajouter(m),
        }
    }
    e
}

/// « Plat 2 » pour une valeur ronde de palier, « +40 » entre deux : l'axe
/// des courbes de RR, dans sa gouttière de 40 px.
fn etiquette_de_rang(v: f32) -> String {
    const PALIERS: [&str; 8] = ["Fer", "Bron", "Arg", "Or", "Plat", "Dia", "Asc", "Imm"];
    let abrege = |tier: u8| match tier {
        0..=2 => TIRET.to_string(),
        27.. => "Rad".to_string(),
        t => {
            let i = (t - 3) as usize;
            format!("{} {}", PALIERS[i / 3], i % 3 + 1)
        }
    };
    if !v.is_finite() {
        return String::new();
    }
    let tier = (v / 100.0).floor().clamp(0.0, 27.0) as u8;
    match (v - f32::from(tier) * 100.0).round() as i32 {
        0 => abrege(tier),
        100 => abrege(tier.saturating_add(1)),
        rr => format!("+{rr}"),
    }
}

// ---------------------------------------------------------------------
// La vue
// ---------------------------------------------------------------------

impl PageValo {
    /// La vue Comparer du profil de `a_id` : le choix de l'autre, puis tout
    /// ce qui les oppose et les réunit.
    pub(super) fn comparer(&mut self, ui: &mut Ui, a: &Vue, a_id: UserId, cx: &Contexte<'_>, demandes: &mut Vec<Demande>) {
        let autres: Vec<&FicheMembre> = cx.stats.iter().filter(|f| f.user_id != a_id).collect();
        let choisi = cx.comparee.filter(|c| c.user_id != a_id);
        ui::section(ui, Icon::User, "Comparer avec", Some("un autre membre lié · la même période et le même mode pour les deux"), |ui| {
            if autres.is_empty() {
                ui::hint(ui, "Personne d'autre n'a encore lié de compte Riot.");
                return;
            }
            let mut choix = choisi.and_then(|c| autres.iter().position(|f| f.user_id == c.user_id)).unwrap_or(usize::MAX);
            let options: Vec<(usize, &str)> = autres.iter().enumerate().map(|(i, f)| (i, f.username.as_str())).collect();
            if ui::pastilles(ui, &mut choix, &options) {
                if let Some(f) = autres.get(choix) {
                    demandes.push(Demande::Comparer(f.user_id, f.username.clone()));
                }
            }
            if choisi.is_none() {
                ui.add_space(espace::XS);
                ui::hint(ui, "Choisis un membre — ou coche deux lignes du classement.");
            }
        });
        let Some(c) = choisi else { return };
        match (c.recue, &c.fiche) {
            (false, _) => {
                ui.label(RichText::new("demande au serveur…").color(TEXT_DIM));
            }
            (true, None) => ui::hint(ui, format!("{} n'a pas encore de fiche.", c.username)),
            (true, Some(fiche)) => {
                let b = Vue::new(&c.username, fiche, self.fiche_periode, self.fiche_classe, cx, maintenant_ms());
                face_a_face(ui, a, &b);
                courbes_superposees(ui, a, &b);
                ensemble_ui(ui, a, a_id, &b, c.user_id);
                en_commun(ui, a, &b);
            }
        }
    }
}

/// Une ligne du face-à-face : l'intitulé, la valeur de chacun, et entre
/// elles la part de chacun quand elle a un sens.
struct Duel {
    intitule: &'static str,
    a: Option<f32>,
    b: Option<f32>,
    texte_a: String,
    texte_b: String,
    /// Plus haut, c'est mieux.
    plus_haut: bool,
    /// Des parts qui s'additionnent : une barre partagée.
    barre: bool,
}

impl Duel {
    fn new(intitule: &'static str, f: fn(&Bilan) -> Option<f32>, a: &Bilan, b: &Bilan, texte: fn(Option<f32>) -> String) -> Self {
        let (va, vb) = (f(a), f(b));
        Duel { intitule, a: va, b: vb, texte_a: texte(va), texte_b: texte(vb), plus_haut: true, barre: true }
    }

    /// Qui fait mieux : `Some(true)` pour A, `Some(false)` pour B.
    fn meilleur(&self) -> Option<bool> {
        let (a, b) = (self.a.filter(|v| v.is_finite())?, self.b.filter(|v| v.is_finite())?);
        if (a - b).abs() < f32::EPSILON {
            return None;
        }
        Some((a > b) == self.plus_haut)
    }
}

/// Le rang, les RR de la période et les taux de chacun, ligne par ligne.
fn face_a_face(ui: &mut Ui, a: &Vue, b: &Vue) {
    let (ra, rb) = (&a.fiche.rang, &b.fiche.rang);
    let rang = |r: &ki_protocol::RangValorant| if r.tier >= 3 { format!("{} · {} RR", nom_de_rang(r.tier), r.rr) } else { nom_de_rang(0) };
    let rr = |v: i32| signe(v).0;
    let mut duels = vec![
        Duel {
            intitule: "Rang",
            a: (ra.tier >= 3).then(|| graphes::valeur_rr(ra.tier, ra.rr)),
            b: (rb.tier >= 3).then(|| graphes::valeur_rr(rb.tier, rb.rr)),
            texte_a: rang(ra),
            texte_b: rang(rb),
            plus_haut: true,
            barre: false,
        },
        Duel {
            intitule: "RR de la période",
            a: (!a.p.is_empty()).then_some(a.b.rr as f32),
            b: (!b.p.is_empty()).then_some(b.b.rr as f32),
            texte_a: if a.p.is_empty() { TIRET.to_string() } else { rr(a.b.rr) },
            texte_b: if b.p.is_empty() { TIRET.to_string() } else { rr(b.b.rr) },
            plus_haut: true,
            barre: false,
        },
    ];
    let (ba, bb) = (&a.b, &b.b);
    duels.push(Duel::new("Victoires", Bilan::victoires_pct, ba, bb, opt_pct));
    duels.push(Duel::new("K/D", Bilan::kd, ba, bb, |v| opt_dec(v, 2)));
    duels.push(Duel::new("ACS", Bilan::acs, ba, bb, opt_entier));
    duels.push(Duel::new("ADR", Bilan::adr, ba, bb, opt_entier));
    duels.push(Duel::new("KAST", Bilan::kast_pct, ba, bb, opt_pct));
    duels.push(Duel::new("Tête", Bilan::tete_pct, ba, bb, opt_pct));
    duels.push(Duel::new("Premiers sangs", Bilan::fk_par_match, ba, bb, |v| opt_dec(v, 1)));
    duels.push(Duel::new("Matchs", |b| Some(f32::from(b.matchs)), ba, bb, |v| v.map_or_else(|| TIRET.to_string(), |v| format!("{v}"))));

    let aide = format!("{}, {}", a.periode.titre(), if a.classe { "compétitif" } else { "tous les modes" });
    ui::section(ui, Icon::Target, "Face à face", Some(&aide), |ui| {
        // Les deux noms, chacun de son côté, à sa couleur.
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::hover());
        let painter = ui.painter_at(rect);
        let gauche = rect.left() + COLONNE_INTITULE + COLONNE_VALEUR;
        let droite = rect.right() - COLONNE_VALEUR;
        let titre = police::moyenne(ui.ctx(), texte::TITRE);
        painter.text(Pos2::new(gauche, rect.center().y), egui::Align2::RIGHT_CENTER, a.username, titre.clone(), COULEUR_A);
        painter.text(Pos2::new((gauche + droite) / 2.0, rect.center().y), egui::Align2::CENTER_CENTER, "contre", FontId::proportional(texte::PETIT), TEXT_FAINT);
        painter.text(Pos2::new(droite, rect.center().y), egui::Align2::LEFT_CENTER, b.username, titre, COULEUR_B);
        ui.add_space(espace::XS);
        for d in &duels {
            ligne_de_duel(ui, d);
        }
        if ba.matchs == 0 || bb.matchs == 0 {
            ui.add_space(espace::XS);
            ui::hint(ui, "L'un des deux n'a aucun match à manches sur cette période : ses taux restent vides.");
        }
    });
}

fn ligne_de_duel(ui: &mut Ui, d: &Duel) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), HAUTEUR_DUEL), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect);
    let cy = rect.center().y;
    let meilleur = d.meilleur();
    let style = |gagne: bool, couleur: Color32| {
        if gagne {
            (police::moyenne(ui.ctx(), texte::COURANT), couleur)
        } else {
            (FontId::proportional(texte::COURANT), TEXT_DIM)
        }
    };
    painter.text(Pos2::new(rect.left(), cy), egui::Align2::LEFT_CENTER, d.intitule, FontId::proportional(texte::PETIT), TEXT_FAINT);
    let gauche = rect.left() + COLONNE_INTITULE + COLONNE_VALEUR;
    let droite = rect.right() - COLONNE_VALEUR;
    let (police_a, couleur_a) = style(meilleur == Some(true), COULEUR_A);
    let (police_b, couleur_b) = style(meilleur == Some(false), COULEUR_B);
    painter.text(Pos2::new(gauche - espace::M, cy), egui::Align2::RIGHT_CENTER, &d.texte_a, police_a, couleur_a);
    painter.text(Pos2::new(droite + espace::M, cy), egui::Align2::LEFT_CENTER, &d.texte_b, police_b, couleur_b);

    // La part de chacun, de part et d'autre du milieu.
    let barre = Rect::from_min_max(Pos2::new(gauche, cy - 4.0), Pos2::new(droite.max(gauche + 1.0), cy + 4.0));
    let arrondi = egui::CornerRadius::same(rayon::S);
    painter.rect_filled(barre, arrondi, theme::BG_DEEP);
    let parts = match (d.a, d.b) {
        (Some(a), Some(b)) if d.barre && a.is_finite() && b.is_finite() && a >= 0.0 && b >= 0.0 && a + b > 0.0 => Some(a / (a + b)),
        _ => None,
    };
    match parts {
        Some(part) => {
            let x = barre.left() + barre.width() * part;
            let teinte = |gagne: bool, c: Color32| if gagne { c } else { theme::alpha(c, 110) };
            painter.rect_filled(
                Rect::from_min_max(barre.min, Pos2::new(x, barre.bottom())),
                egui::CornerRadius { nw: rayon::S, sw: rayon::S, ne: 0, se: 0 },
                teinte(meilleur == Some(true), COULEUR_A),
            );
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(x, barre.top()), barre.max),
                egui::CornerRadius { nw: 0, sw: 0, ne: rayon::S, se: rayon::S },
                teinte(meilleur == Some(false), COULEUR_B),
            );
        }
        None => {
            // Sans parts, une flèche dit qui fait mieux.
            if let Some(gagne_a) = meilleur {
                let (pointe, couleur) = if gagne_a { (barre.left() + 6.0, COULEUR_A) } else { (barre.right() - 6.0, COULEUR_B) };
                painter.circle_filled(Pos2::new(pointe, cy), 4.0, couleur);
            }
        }
    }
}

/// Les courbes de RR des deux sur la période, sur le même axe.
fn courbes_superposees(ui: &mut Ui, a: &Vue, b: &Vue) {
    let pa: Vec<PointCourbe> = a.p.iter().map(|p| PointCourbe::from(*p)).collect();
    let pb: Vec<PointCourbe> = b.p.iter().map(|p| PointCourbe::from(*p)).collect();
    ui::section(ui, Icon::ArrowUp, "Les courbes de RR", Some("les classés de la période, l'un sur l'autre · survole un point"), |ui| {
        let series = [
            graphes::Serie { nom: a.username, couleur: COULEUR_A, points: &pa },
            graphes::Serie { nom: b.username, couleur: COULEUR_B, points: &pb },
        ];
        if pa.len() + pb.len() < 2 {
            ui::hint(ui, "Pas assez de classés sur cette période pour une courbe.");
            return;
        }
        let points = [&a.p, &b.p];
        let noms = [a.username, b.username];
        graphes::courbes(ui, egui::Id::new("comparer_courbes"), &series, HAUTEUR_COURBES, etiquette_de_rang, |k, i| {
            points[k].get(i).map_or_else(String::new, |p| format!("{} · {}", noms[k], info_de_point(p)))
        });
    });
}

/// Leurs parties ensemble et l'un contre l'autre, et ce que chacun y fait
/// de mieux ou de moins bien que sans l'autre.
fn ensemble_ui(ui: &mut Ui, a: &Vue, a_id: UserId, b: &Vue, b_id: UserId) {
    let e = ensemble(a, a_id, b, b_id);
    ui::section(ui, Icon::Play, "Ensemble", Some("leurs parties dans le même camp, et l'un contre l'autre, sur la sélection"), |ui| {
        if e.a_avec.matchs == 0 && e.contre == (0, 0) {
            ui::hint(ui, "Aucune partie ensemble sur cette période.");
            return;
        }
        if e.a_avec.matchs > 0 {
            let v = &e.a_avec;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = espace::S;
                ui.label(RichText::new(pluriel(u32::from(v.matchs), "partie")).font(police::moyenne(ui.ctx(), texte::TITRE)).color(TEXT));
                ui.label(RichText::new("ensemble").color(TEXT_DIM).size(texte::CORPS));
                ui.label(
                    RichText::new(format!("{} V – {} D · {}", v.victoires, v.defaites, opt_pct(v.victoires_pct())))
                        .font(police::moyenne(ui.ctx(), texte::CORPS))
                        .color(match v.victoires.cmp(&v.defaites) {
                            std::cmp::Ordering::Greater => SPEAK,
                            std::cmp::Ordering::Less => DANGER,
                            std::cmp::Ordering::Equal => TEXT,
                        }),
                );
            });
            let nuls = v.matchs.saturating_sub(v.victoires).saturating_sub(v.defaites);
            graphes::barre_vd(ui, u32::from(v.victoires), u32::from(v.defaites), u32::from(nuls), ui.available_width());
            ui.add_space(espace::S);
            for (nom, couleur, avec, sans, autre) in
                [(a.username, COULEUR_A, &e.a_avec, &e.a_sans, b.username), (b.username, COULEUR_B, &e.b_avec, &e.b_sans, a.username)]
            {
                if !(avec.assez(ASSEZ_ENSEMBLE) && sans.assez(ASSEZ_ENSEMBLE)) {
                    continue;
                }
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = espace::XS;
                    ui.label(RichText::new(nom).font(police::moyenne(ui.ctx(), texte::COURANT)).color(couleur));
                    ui.label(
                        RichText::new(format!(
                            ": K/D {} avec {autre}, {} sans · victoires {} avec, {} sans",
                            opt_dec(avec.kd(), 2),
                            opt_dec(sans.kd(), 2),
                            opt_pct(avec.victoires_pct()),
                            opt_pct(sans.victoires_pct())
                        ))
                        .color(TEXT_DIM)
                        .size(texte::COURANT),
                    );
                });
            }
        }
        if e.contre != (0, 0) {
            ui.add_space(espace::S);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = espace::XS;
                ui.label(RichText::new("L'un contre l'autre :").color(TEXT_DIM).size(texte::COURANT));
                ui.label(RichText::new(a.username).color(COULEUR_A).size(texte::COURANT));
                ui.label(RichText::new(format!("{} – {}", e.contre.0, e.contre.1)).font(police::moyenne(ui.ctx(), texte::CORPS)).color(TEXT));
                ui.label(RichText::new(b.username).color(COULEUR_B).size(texte::COURANT));
            });
        }
    });
}

/// Les agents puis les cartes qu'ils ont joués tous les deux sur la
/// sélection, les plus joués à eux deux d'abord.
fn en_commun(ui: &mut Ui, a: &Vue, b: &Vue) {
    let agent: fn(&MatchResume) -> &str = |m| m.agent.as_str();
    let carte: fn(&MatchResume) -> &str = |m| m.carte.as_str();
    for (titre, icone, aide, cle, est_carte) in [
        ("Agents en commun", Icon::User, "ceux qu'ils ont joués tous les deux · en gras, les meilleures victoires", agent, false),
        ("Cartes en commun", Icon::Screen, "la part des victoires de chacun sur chaque carte", carte, true),
    ] {
        let (la, lb) = (a.ventiler(cle), b.ventiler(cle));
        let mut communs: Vec<(&str, &Bilan, &Bilan)> = la
            .iter()
            .filter(|(nom, _)| !nom.is_empty())
            .filter_map(|(nom, ba)| lb.iter().find(|(autre, _)| autre == nom).map(|(_, bb)| (nom.as_str(), ba, bb)))
            .collect();
        communs.sort_by_key(|(_, ba, bb)| std::cmp::Reverse(ba.matchs.saturating_add(bb.matchs)));
        ui::section(ui, icone, titre, Some(aide), |ui| {
            if communs.is_empty() {
                ui::hint(ui, "Rien en commun sur cette période.");
                return;
            }
            ui::grille(ui, communs.len(), largeur::CARTE_MIN, espace::L, |ui, i| {
                let (nom, ba, bb) = communs[i];
                ui::surface(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if est_carte {
                        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), Sense::hover());
                        match a.catalogue.bandeau(nom) {
                            Some(image) => {
                                egui::Image::new(image).uv(graphes::uv_couvrant(image.size_vec2(), rect.size())).corner_radius(rayon::M).paint_at(ui, rect);
                                ui.painter().rect_filled(rect, egui::CornerRadius::same(rayon::M), Color32::from_black_alpha(90));
                            }
                            None => {
                                ui.painter().rect_filled(rect, egui::CornerRadius::same(rayon::M), theme::BG_ACTIVE);
                            }
                        }
                        ui.painter().text(
                            Pos2::new(rect.left() + espace::M, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            nom,
                            police::moyenne(ui.ctx(), texte::TITRE),
                            Color32::WHITE,
                        );
                    } else {
                        ui.horizontal(|ui| {
                            if let Some(portrait) = a.catalogue.agent(nom) {
                                ui.add(egui::Image::new(portrait).fit_to_exact_size(Vec2::splat(32.0)));
                            }
                            ui.label(RichText::new(nom).font(police::moyenne(ui.ctx(), texte::TITRE)).color(TEXT));
                        });
                    }
                    ui.add_space(espace::XS);
                    let duel = Duel::new("", Bilan::victoires_pct, ba, bb, opt_pct);
                    let meilleur = duel.meilleur();
                    ligne_en_commun(ui, a.username, COULEUR_A, ba, meilleur == Some(true));
                    ligne_en_commun(ui, b.username, COULEUR_B, bb, meilleur == Some(false));
                });
            });
        });
    }
}

/// « ● drion  8 parties · K/D 1,23 · 63 % », la part des victoires en gras
/// pour qui fait mieux.
fn ligne_en_commun(ui: &mut Ui, nom: &str, couleur: Color32, b: &Bilan, mieux: bool) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = espace::XS;
        let (point, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
        icons::dot(ui.painter(), point.center(), 3.0, couleur);
        ui.label(RichText::new(nom).color(couleur).size(texte::PETIT));
        ui.label(
            RichText::new(format!("{} · K/D {}", pluriel(u32::from(b.matchs), "partie"), opt_dec(b.kd(), 2))).color(TEXT_DIM).size(texte::PETIT),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let police_ = if mieux { police::moyenne(ui.ctx(), texte::COURANT) } else { FontId::proportional(texte::COURANT) };
            ui.label(RichText::new(opt_pct(b.victoires_pct())).font(police_).color(if mieux { couleur } else { TEXT_DIM }));
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joue(id: &str, date: u64, gagne: bool, avec: &[UserId], contre: &[UserId]) -> MatchResume {
        let mut m = super::super::tests::match_de_test(id, date, MODE_CLASSE, gagne, avec);
        m.contre = contre.to_vec();
        m
    }

    /// Le même camp se lit au match (`avec`, `contre`), sinon au même
    /// match chez l'autre ; chacun a son bilan avec et sans l'autre.
    #[test]
    fn deux_membres_se_comparent_ensemble_et_l_un_contre_l_autre() {
        let maintenant = maintenant_ms();
        let heure = 3_600_000;
        let fiche = |matchs| FicheValorant { rang: ki_protocol::RangValorant { tier: 15, ..Default::default() }, matchs, ..Default::default() };
        // A : x avec B (dit par `avec`), y en face (dit par `contre`), z
        // avec B (vu chez B, même score), w seul.
        let fa = fiche(vec![
            joue("x", maintenant - heure, true, &[2], &[]),
            joue("y", maintenant - 2 * heure, true, &[], &[2]),
            joue("z", maintenant - 3 * heure, false, &[], &[]),
            joue("w", maintenant - 4 * heure, true, &[], &[]),
        ]);
        let fb = fiche(vec![
            joue("x", maintenant - heure, true, &[], &[]),
            joue("y", maintenant - 2 * heure, false, &[], &[]),
            joue("z", maintenant - 3 * heure, false, &[], &[]),
            joue("v", maintenant - 5 * heure, false, &[], &[]),
        ]);
        let rangs = rangs::Rangs::new();
        let catalogue = Catalogue::new();
        let photos = HashMap::new();
        let cx = Contexte {
            stats: &[],
            recu: true,
            esports: &[],
            activite: &[],
            moi: None,
            membres: &[],
            rangs: &rangs,
            catalogue: &catalogue,
            soiree: &[],
            soiree_servie: true,
            ma_fiche: None,
            photos: &photos,
            comparee: None,
            journal: Journal::default(),
        };
        let a = Vue::new("A", &fa, PeriodeFiche::TrenteJours, true, &cx, maintenant);
        let b = Vue::new("B", &fb, PeriodeFiche::TrenteJours, true, &cx, maintenant);
        let e = ensemble(&a, 1, &b, 2);
        assert_eq!((e.a_avec.matchs, e.a_avec.victoires), (2, 1), "x et z");
        assert_eq!(e.contre, (1, 0), "y, gagné par A");
        assert_eq!(e.a_sans.matchs, 1, "w");
        assert_eq!((e.b_avec.matchs, e.b_sans.matchs), (2, 1), "x et z ; v");
    }

    /// L'axe des courbes : un palier rond par son nom court, l'entre-deux
    /// par ses RR.
    #[test]
    fn l_axe_des_rangs_se_lit_dans_sa_gouttiere() {
        assert_eq!(etiquette_de_rang(1_600.0), "Plat 2");
        assert_eq!(etiquette_de_rang(1_640.0), "+40");
        assert_eq!(etiquette_de_rang(1_699.7), "Plat 3");
        assert_eq!(etiquette_de_rang(2_700.0), "Rad");
        assert_eq!(etiquette_de_rang(f32::NAN), "");
        let duel = Duel { intitule: "", a: Some(1.0), b: Some(2.0), texte_a: String::new(), texte_b: String::new(), plus_haut: true, barre: true };
        assert_eq!(duel.meilleur(), Some(false));
        assert_eq!(Duel { b: None, ..duel }.meilleur(), None);
    }
}
