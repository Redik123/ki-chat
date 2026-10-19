//! La progression d'un membre — V9.4 de PLAN-VALORANT-2.md, une vue du
//! profil :
//! - toute sa courbe de RR, sur ce que le serveur garde, les actes
//!   marqués ;
//! - le rang de fin de chaque acte, en barres, et le MMR estimé acte par
//!   acte ;
//! - le MMR estimé aujourd'hui, en tuiles ;
//! - pour soi seul, le temps de jeu de son journal des parties (V8.4) :
//!   semaine par semaine, mode par mode, et par compte. Le serveur ne
//!   relit jamais le journal d'un autre.

use ki_protocol::{EstimationMmr, JourDeJeu, StatsSaison};
use ki_ui::jetons::{largeur, police};
use ki_ui::tableau::{Colonne, Tableau};

use super::*;
use crate::icons::Icon;

/// Les semaines du temps de jeu montrées en barres.
const SEMAINES: usize = 12;
/// Les modes qui ont leur couleur ; les autres vont ensemble, en gris.
const MODES_EN_COULEUR: usize = 4;
const HAUTEUR_COURBE_ENTIERE: f32 = 240.0;
const HAUTEUR_ACTES: f32 = 150.0;
const HAUTEUR_SEMAINES: f32 = 140.0;

/// Ce que la page sait du journal des parties de qui la regarde.
#[derive(Clone, Copy, Default)]
pub struct Journal<'a> {
    /// Le serveur le relit (`Welcome.journal_lecture`).
    pub servi: bool,
    /// La case « Garder l'historique de mes parties » est cochée.
    pub coche: bool,
    /// Ses jours, une fois reçus (`ServerMsg::MonJournal`).
    pub jours: Option<&'a [JourDeJeu]>,
}

/// Les minutes à ajouter à l'heure UTC pour tomber sur le jour du
/// journal : l'écart du fuseau, moins six heures — un jour commence à 6 h
/// du matin, comme une soirée.
pub fn decalage_journal() -> i32 {
    chrono::Local::now().offset().local_minus_utc() / 60 - 360
}

/// Le jour du journal d'un instant.
pub(super) fn jour_de_journal(ms: u64, decalage_min: i32) -> i32 {
    let local = i64::try_from(ms).unwrap_or(i64::MAX).saturating_add(i64::from(decalage_min) * 60_000);
    i32::try_from(local.div_euclid(JOUR_MS as i64)).unwrap_or(i32::MAX)
}

/// La semaine d'un jour, à partir du lundi : le 1ᵉʳ janvier 1970 était un
/// jeudi.
fn semaine_de(jour: i32) -> i32 {
    (jour + 3).div_euclid(7)
}

/// « 13/10 », le lundi d'une semaine.
fn lundi_de(semaine: i32) -> String {
    chrono::NaiveDate::from_ymd_opt(1970, 1, 1)
        .and_then(|d| d.checked_add_signed(chrono::Duration::days(i64::from(semaine) * 7 - 3)))
        .map(|d| d.format("%d/%m").to_string())
        .unwrap_or_default()
}

/// « 5 h 20 », « 45 min ».
fn heures_minutes(secondes: u32) -> String {
    let (h, m) = (secondes / 3600, (secondes % 3600) / 60);
    match (h, m) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m:02}"),
    }
}

/// Le nom d'un mode du journal, tel que la présence le dit : « compétitive »,
/// une file inconnue telle quelle, « personnalisée » sans file.
fn nom_du_mode(file: &str) -> String {
    let (file, console) = file.strip_prefix("console_").map_or((file, false), |f| (f, true));
    let nom = match file {
        "" => "personnalisée".to_string(),
        f => ki_protocol::nom_de_file(f).map_or_else(|| f.to_string(), str::to_string),
    };
    if console {
        format!("{nom} (console)")
    } else {
        nom
    }
}

/// Le compte d'une partie du journal, en mots.
fn nom_du_compte(compte: &str) -> &'static str {
    match compte {
        "principal" => "compte principal",
        "second" => "second compte",
        "autre" => "un compte non lié",
        _ => "compte inconnu",
    }
}

/// Le temps de jeu du journal, prêt à montrer.
#[derive(Debug, Default, PartialEq)]
pub(super) struct TempsDeJeu {
    /// Les modes, du plus joué au moins joué : (file, secondes, parties).
    pub modes: Vec<(String, u32, u32)>,
    /// Les comptes, de même.
    pub comptes: Vec<(String, u32, u32)>,
    /// Les [`SEMAINES`] dernières, de la plus ancienne à la courante : la
    /// semaine, et ses secondes mode par mode (dans l'ordre de `modes`).
    pub semaines: Vec<(i32, Vec<u32>)>,
    pub cette_semaine: u32,
    pub semaine_passee: u32,
    /// Sur trente jours : secondes, parties, soirs joués.
    pub trente_jours: (u32, u32, u32),
    /// Depuis le début du journal : secondes, parties, et son premier jour.
    pub en_tout: (u32, u32, Option<i32>),
}

/// Le temps de jeu de ces jours, vus d'`aujourd_hui` (un jour du journal).
pub(super) fn temps_de_jeu(jours: &[JourDeJeu], aujourd_hui: i32) -> TempsDeJeu {
    let mut t = TempsDeJeu::default();
    let mut modes: BTreeMap<&str, (u32, u32)> = BTreeMap::new();
    let mut comptes: BTreeMap<&str, (u32, u32)> = BTreeMap::new();
    let mut soirs: BTreeSet<i32> = BTreeSet::new();
    let semaine = semaine_de(aujourd_hui);
    for j in jours.iter().filter(|j| j.jour <= aujourd_hui) {
        let (s, p) = (j.secondes, u32::from(j.parties));
        let m = modes.entry(j.file.as_str()).or_default();
        *m = (m.0.saturating_add(s), m.1.saturating_add(p));
        let c = comptes.entry(j.compte.as_str()).or_default();
        *c = (c.0.saturating_add(s), c.1.saturating_add(p));
        t.en_tout = (t.en_tout.0.saturating_add(s), t.en_tout.1.saturating_add(p), Some(t.en_tout.2.map_or(j.jour, |d| d.min(j.jour))));
        if j.jour > aujourd_hui - 30 {
            t.trente_jours.0 = t.trente_jours.0.saturating_add(s);
            t.trente_jours.1 = t.trente_jours.1.saturating_add(p);
            soirs.insert(j.jour);
        }
        match semaine - semaine_de(j.jour) {
            0 => t.cette_semaine = t.cette_semaine.saturating_add(s),
            1 => t.semaine_passee = t.semaine_passee.saturating_add(s),
            _ => {}
        }
    }
    t.trente_jours.2 = soirs.len() as u32;
    let trier = |m: BTreeMap<&str, (u32, u32)>| {
        let mut v: Vec<(String, u32, u32)> = m.into_iter().map(|(k, (s, p))| (k.to_string(), s, p)).collect();
        v.sort_by_key(|(_, s, _)| std::cmp::Reverse(*s));
        v
    };
    t.modes = trier(modes);
    t.comptes = trier(comptes);
    let premiere = semaine - SEMAINES as i32 + 1;
    t.semaines = (premiere..=semaine)
        .map(|s| {
            let parts = t
                .modes
                .iter()
                .map(|(file, ..)| {
                    jours.iter().filter(|j| semaine_de(j.jour) == s && j.jour <= aujourd_hui && &j.file == file).map(|j| j.secondes).sum()
                })
                .collect();
            (s, parts)
        })
        .collect();
    t
}

/// L'estimation du MMR acte par acte, d'après les points de RR de chacun.
fn mmr_par_acte(fiche: &FicheValorant) -> BTreeMap<&str, EstimationMmr> {
    let mut actes: BTreeMap<&str, Vec<&PointRR>> = BTreeMap::new();
    for p in fiche.historique_rr.iter().filter(|p| !p.saison.is_empty()) {
        actes.entry(p.saison.as_str()).or_default().push(p);
    }
    actes.into_iter().filter_map(|(acte, points)| EstimationMmr::des_points(points).map(|e| (acte, e))).collect()
}

/// « au-dessus », et sa couleur : la place du MMR estimé par rapport au
/// rang affiché.
fn position_de(e: &EstimationMmr) -> (&'static str, Color32) {
    match e.position {
        PositionMmr::AuDessus => ("au-dessus", SPEAK),
        PositionMmr::AuNiveau => ("au niveau", TEXT_DIM),
        PositionMmr::EnDessous => ("en dessous", DANGER),
    }
}

impl PageValo {
    /// La vue Progression du profil de `user_id`.
    pub(super) fn progression(&mut self, ui: &mut Ui, vue: &Vue, user_id: UserId, cx: &Contexte<'_>, demandes: &mut Vec<Demande>) {
        courbe_entiere(ui, vue);
        les_actes_en_detail(ui, vue);
        mmr_aujourd_hui(ui, vue);
        if cx.moi == Some(user_id) {
            mon_temps_de_jeu(ui, &cx.journal, demandes);
        } else {
            ui::hint(ui, "Le temps de jeu du journal des parties ne se montre qu'à son propriétaire, sur son propre profil.");
        }
    }
}

/// Tous les points de RR que le serveur garde, les actes marqués, le pic
/// en pointillé.
fn courbe_entiere(ui: &mut Ui, vue: &Vue) {
    let mut points: Vec<&PointRR> = vue.fiche.historique_rr.iter().collect();
    points.sort_by_key(|p| p.date);
    let aide = match points.first() {
        Some(premier) => format!(
            "{} gardés, depuis {} · les traits marquent les actes",
            pluriel(points.len() as u32, "classé"),
            crate::il_y_a(premier.date)
        ),
        None => "les classés que le serveur garde".to_string(),
    };
    ui::section(ui, Icon::ArrowUp, "Toute la courbe de RR", Some(&aide), |ui| {
        if points.len() < 2 {
            ui::hint(ui, "Pas encore assez de classés pour une courbe.");
            return;
        }
        let frontieres = frontieres_d_acte(&points);
        let courbe: Vec<PointCourbe> = points.iter().map(|p| PointCourbe::from(*p)).collect();
        let pic = vue.fiche.pic.as_ref().filter(|p| p.tier >= 3).map(|p| graphes::valeur_rr(p.tier, p.rr));
        let info = |i: usize| points.get(i).map_or_else(String::new, |p| info_de_point(p));
        graphes::courbe_temps(ui, egui::Id::new("progression_courbe"), &courbe, HAUTEUR_COURBE_ENTIERE, true, &frontieres, pic, info);
    });
}

/// Le rang de fin de chaque acte en barres, puis acte par acte : le rang,
/// les victoires, et le MMR estimé sur ses classés.
fn les_actes_en_detail(ui: &mut Ui, vue: &Vue) {
    let saisons: &[StatsSaison] = &vue.fiche.saisons;
    ui::section(ui, Icon::Crown, "Les actes", Some("le rang en fin d'acte, et le MMR estimé sur les classés de chacun"), |ui| {
        if saisons.is_empty() {
            ui::hint(ui, "Pas encore d'acte connu pour ce compte.");
            return;
        }
        let barres = barres_d_actes(saisons);
        let haut = barres.iter().map(|b| b.valeur).fold(0.0, f32::max) + 2.0;
        graphes::barres_verticales(ui, egui::Id::new("progression_actes"), &barres, HAUTEUR_ACTES, Some(haut.min(26.0)));
        ui.add_space(espace::M);
        let mmr = mmr_par_acte(vue.fiche);
        let lignes: Vec<&StatsSaison> = saisons.iter().rev().collect();
        let tableau = Tableau::new("progression_actes_tableau")
            .colonne(Colonne::new("Acte", 70.0))
            .colonne(Colonne::new("Fin d'acte", 150.0).grandit())
            .colonne(Colonne::new("Victoires", 110.0).priorite(2))
            .colonne(Colonne::new("MMR estimé", 200.0).priorite(1));
        tableau.show(ui, lignes.len(), |ui, i, colonne| {
            let s = lignes[i];
            match colonne {
                0 => {
                    cellule(ui, &s.saison, TEXT_DIM);
                }
                1 => {
                    ui.spacing_mut().item_spacing.x = espace::XS;
                    if let Some(icone) = vue.rangs.texture(s.tier_fin).filter(|_| s.tier_fin >= 3) {
                        ui.add(egui::Image::new(icone).fit_to_exact_size(Vec2::splat(20.0)));
                    }
                    ui.label(RichText::new(nom_de_rang(s.tier_fin)).color(graphes::couleur_de_rang(s.tier_fin)).size(texte::PETIT));
                    if s.tier_fin >= 3 {
                        cellule(ui, format!("{} RR", s.rr_fin), TEXT_DIM);
                    }
                }
                2 => {
                    let pct = (s.parties > 0).then(|| f32::from(s.victoires) * 100.0 / f32::from(s.parties));
                    cellule(ui, format!("{} / {} · {}", s.victoires, s.parties, opt_pct(pct)), TEXT);
                }
                _ => match mmr.get(s.saison.as_str()) {
                    Some(e) => {
                        let (ou, teinte) = position_de(e);
                        cellule(ui, format!("+{} / −{} · {ou} du rang", opt_entier(Some(e.gain_moyen)), opt_entier(Some(e.perte_moyenne))), teinte)
                            .on_hover_text(format!("sur {} de l'acte", pluriel(u32::from(e.points), "classé")));
                    }
                    None => {
                        cellule(ui, TIRET, TEXT_FAINT).on_hover_text("trois victoires et trois défaites classées de l'acte au moins");
                    }
                },
            }
        });
    });
}

/// Le MMR estimé aujourd'hui, en quatre tuiles.
fn mmr_aujourd_hui(ui: &mut Ui, vue: &Vue) {
    ui::section(ui, Icon::Sliders, "Le MMR estimé", Some("deviné aux variations de RR des 20 derniers classés de l'acte"), |ui| {
        let Some(e) = vue.fiche.mmr_estime() else {
            ui::hint(ui, "Pas d'estimation : il faut un rang, et trois victoires et trois défaites classées dans l'acte en cours.");
            return;
        };
        let (ou, teinte) = position_de(&e);
        let tuiles = [
            TuileDeChiffre("Position", ou.to_string(), "du rang affiché".to_string(), Some(teinte)),
            TuileDeChiffre("Par victoire", format!("+{}", opt_entier(Some(e.gain_moyen))), pluriel(u32::from(e.victoires), "victoire"), Some(SPEAK)),
            TuileDeChiffre("Par défaite", format!("−{}", opt_entier(Some(e.perte_moyenne))), pluriel(u32::from(e.defaites), "défaite"), Some(DANGER)),
            TuileDeChiffre("Écart", format!("{:+}", (e.gain_moyen - e.perte_moyenne).round() as i32), "gain moins perte, en RR".to_string(), None),
        ];
        let reponse = ui.scope(|ui| tuiles_ui(ui, &tuiles)).response;
        reponse.on_hover_text(POURQUOI_MMR);
    });
}

/// Mon temps de jeu, d'après mon journal des parties : ou ce qui manque
/// pour l'avoir.
fn mon_temps_de_jeu(ui: &mut Ui, journal: &Journal<'_>, demandes: &mut Vec<Demande>) {
    ui::section(
        ui,
        Icon::Film,
        "Mon temps de jeu",
        Some("d'après mon journal des parties, que ki-chat tient à chaque fin de partie · visible de moi seul"),
        |ui| {
            if !journal.servi {
                ui::hint(ui, "Ce serveur ne relit pas encore le journal des parties : il faut le mettre à jour.");
                return;
            }
            let Some(jours) = journal.jours else {
                demandes.push(Demande::MonJournal);
                ui.label(RichText::new("demande au serveur…").color(TEXT_DIM));
                return;
            };
            if jours.is_empty() {
                let pourquoi = if journal.coche {
                    "Pas encore de partie au journal : il se remplit à chaque fin de partie de VALORANT."
                } else {
                    "Coche « Garder l'historique de mes parties » dans ⚙ → Jeu : ki-chat comptera ton temps de jeu, partie par partie."
                };
                ui::hint(ui, pourquoi);
                return;
            }
            let aujourd_hui = jour_de_journal(maintenant_ms(), decalage_journal());
            temps_de_jeu_ui(ui, &temps_de_jeu(jours, aujourd_hui));
        },
    );
}

fn temps_de_jeu_ui(ui: &mut Ui, t: &TempsDeJeu) {
    let (secondes_30, parties_30, soirs_30) = t.trente_jours;
    let par_soir = secondes_30.checked_div(soirs_30).unwrap_or(0);
    let depuis = t.en_tout.2.map(|j| lundi_de(semaine_de(j))).unwrap_or_default();
    let tuiles = [
        TuileDeChiffre("Cette semaine", heures_minutes(t.cette_semaine), format!("la semaine dernière : {}", heures_minutes(t.semaine_passee)), None),
        TuileDeChiffre("Sur 30 jours", heures_minutes(secondes_30), pluriel(parties_30, "partie"), None),
        TuileDeChiffre("Par soir joué", heures_minutes(par_soir), format!("{} sur 30", pluriel(soirs_30, "soir")), None),
        TuileDeChiffre("En tout", heures_minutes(t.en_tout.0), format!("{} depuis la semaine du {depuis}", pluriel(t.en_tout.1, "partie")), None),
    ];
    tuiles_ui(ui, &tuiles);
    ui.add_space(espace::L);

    // Les semaines en barres, un mode par couleur ; au-delà des quatre
    // plus joués, les autres ensemble.
    let couleur = |k: usize| if k < MODES_EN_COULEUR { theme::SERIES[k] } else { TEXT_FAINT };
    let barres: Vec<graphes::BarreEmpilee> = t
        .semaines
        .iter()
        .map(|(semaine, parts)| {
            let total: u32 = parts.iter().sum();
            let detail: Vec<String> = parts
                .iter()
                .enumerate()
                .filter(|(_, s)| **s > 0)
                .map(|(k, s)| format!("{} {}", nom_du_mode(&t.modes[k].0), heures_minutes(*s)))
                .collect();
            let mut info = format!("semaine du {} · {}", lundi_de(*semaine), heures_minutes(total));
            if !detail.is_empty() {
                info.push_str(&format!("\n{}", detail.join(" · ")));
            }
            graphes::BarreEmpilee {
                parts: parts.iter().enumerate().map(|(k, s)| (*s as f32, couleur(k))).collect(),
                etiquette: lundi_de(*semaine),
                info,
            }
        })
        .collect();
    ui::section_label(ui, &format!("les {SEMAINES} dernières semaines, du lundi"));
    graphes::barres_empilees(ui, &barres, HAUTEUR_SEMAINES);
    ui.add_space(espace::XS);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = espace::M;
        let autres: u32 = t.modes.iter().skip(MODES_EN_COULEUR).map(|(_, s, _)| *s).sum();
        for (k, (file, secondes, _)) in t.modes.iter().enumerate().take(MODES_EN_COULEUR) {
            legende(ui, couleur(k), &nom_du_mode(file), *secondes);
        }
        if autres > 0 {
            legende(ui, TEXT_FAINT, "les autres modes", autres);
        }
    });

    // Par compte : la part de chacun.
    ui.add_space(espace::L);
    ui::section_label(ui, "par compte");
    let total = t.en_tout.0.max(1);
    for (compte, secondes, parties) in &t.comptes {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = espace::M;
            ui.allocate_ui_with_layout(Vec2::new(150.0, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.label(RichText::new(nom_du_compte(compte)).color(TEXT).size(texte::COURANT));
            });
            graphes::jauge(ui, *secondes as f32 / total as f32, None, 160.0, ACCENT);
            ui.label(RichText::new(heures_minutes(*secondes)).font(police::moyenne(ui.ctx(), texte::COURANT)).color(TEXT));
            ui::hint(ui, pluriel(*parties, "partie"));
        });
    }
}

/// Une tuile à venir : son intitulé, sa valeur, sa précision, sa couleur.
struct TuileDeChiffre(&'static str, String, String, Option<Color32>);

/// Des tuiles en grille, comme les chiffres du profil.
fn tuiles_ui(ui: &mut Ui, tuiles: &[TuileDeChiffre]) {
    ui::grille(ui, tuiles.len(), largeur::TUILE_MIN + 30.0, espace::L, |ui, i| {
        let TuileDeChiffre(intitule, valeur, precision, couleur) = &tuiles[i];
        let mut kpi = ui::Kpi::new(intitule, valeur.clone()).precision(precision);
        if let Some(c) = couleur {
            kpi = kpi.couleur(*c);
        }
        kpi.show(ui);
    });
}

/// Un point de couleur, un mode, son temps.
fn legende(ui: &mut Ui, couleur: Color32, nom: &str, secondes: u32) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = espace::XS;
        let (point, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
        ui.painter().rect_filled(point.shrink(1.0), egui::CornerRadius::same(rayon::S), couleur);
        ui.label(RichText::new(nom).color(TEXT_DIM).size(texte::PETIT));
        ui.label(RichText::new(heures_minutes(secondes)).color(TEXT).size(texte::PETIT));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jour(jour: i32, file: &str, compte: &str, secondes: u32) -> JourDeJeu {
        JourDeJeu { jour, file: file.into(), compte: compte.into(), parties: 1, secondes }
    }

    /// Le temps de jeu se range par semaine du lundi, par mode et par
    /// compte ; un jour d'après aujourd'hui (une horloge en avance) ne
    /// compte pas.
    #[test]
    fn le_temps_de_jeu_se_range_par_semaine() {
        // Le jour 20 381 est un lundi (le 20 octobre 2025).
        assert_eq!(semaine_de(20_381), semaine_de(20_387), "du lundi au dimanche");
        assert_eq!(semaine_de(20_380) + 1, semaine_de(20_381), "le dimanche d'avant");
        assert_eq!(lundi_de(semaine_de(20_385)), "20/10");
        let aujourd_hui = 20_385;
        let jours = vec![
            jour(20_200, "competitive", "principal", 3_600),
            jour(20_380, "competitive", "principal", 1_800),
            jour(20_382, "competitive", "principal", 3_000),
            jour(20_383, "swiftplay", "autre", 1_200),
            jour(20_383, "", "inconnu", 600),
            jour(20_400, "competitive", "principal", 99_999),
        ];
        let t = temps_de_jeu(&jours, aujourd_hui);
        assert_eq!((t.cette_semaine, t.semaine_passee), (4_800, 1_800));
        assert_eq!(t.trente_jours, (6_600, 4, 3));
        assert_eq!(t.en_tout, (10_200, 5, Some(20_200)));
        assert_eq!(t.modes.iter().map(|(m, s, _)| (m.as_str(), *s)).collect::<Vec<_>>(), [("competitive", 8_400), ("swiftplay", 1_200), ("", 600)]);
        assert_eq!(t.comptes[0].0, "principal");
        assert_eq!(t.semaines.len(), SEMAINES);
        assert_eq!(t.semaines.last(), Some(&(semaine_de(aujourd_hui), vec![3_000, 1_200, 600])));
        assert_eq!(t.semaines[SEMAINES - 2].1, vec![1_800, 0, 0]);
        assert_eq!(temps_de_jeu(&[], aujourd_hui).en_tout, (0, 0, None));
    }

    /// Les jours du journal se coupent à 6 h du matin, à l'heure locale.
    #[test]
    fn le_jour_du_journal_commence_a_six_heures() {
        let heure = 3_600_000u64;
        let lundi = 20_381u64 * JOUR_MS;
        // À Paris l'été (UTC+2) : 5 h locales le mardi, c'est encore lundi.
        assert_eq!(jour_de_journal(lundi + 24 * heure + 3 * heure, 120 - 360), 20_381);
        assert_eq!(jour_de_journal(lundi + 24 * heure + 5 * heure, 120 - 360), 20_382);
        assert_eq!(heures_minutes(19_200), "5 h 20");
        assert_eq!(heures_minutes(7_200), "2 h");
        assert_eq!(heures_minutes(2_700), "45 min");
        assert_eq!(nom_du_mode("competitive"), "compétitive");
        assert_eq!(nom_du_mode("console_swiftplay"), "swiftplay (console)");
        assert_eq!(nom_du_mode("un_mode_de_demain"), "un_mode_de_demain");
        assert_eq!(nom_du_mode(""), "personnalisée");
    }
}
