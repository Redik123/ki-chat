//! Les matchs du groupe — V9.3 de PLAN-VALORANT-2.md. Une carte par
//! partie, qu'on l'ait jouée seul ou à plusieurs : le bandeau de la carte à
//! gauche, un liseré à la couleur du résultat, les manches, puis une
//! pastille par membre qui y était — le portrait de son agent, son K/D/A,
//! ses RR et ses médailles.
//!
//! La même carte sert au fil de la soirée sur l'accueil : une [`Partie`] se
//! lit d'une `PartieSoiree` ([`Partie::de_la_soiree`]) comme des
//! `MatchResume` des fiches ([`parties_du_groupe`]).

use ki_ui::jetons::police;

use super::*;
use crate::icons::Icon;

/// Les cartes montrées avant « voir plus », et combien il en ajoute.
pub(super) const CARTES_PAR_PAGE: usize = 20;
/// La largeur du bandeau de la carte, et sous quelle largeur de carte il
/// se resserre.
const BANDEAU: f32 = 116.0;
const BANDEAU_ETROIT: f32 = 72.0;
const CARTE_ETROITE: f32 = 600.0;
/// La largeur d'une pastille de joueur, au moins et au plus — une
/// partie jouée seul ne s'étire pas sur toute la carte —, et son portrait.
const PASTILLE_MIN: f32 = 210.0;
const PASTILLE_MAX: f32 = 320.0;
const PORTRAIT_PASTILLE: f32 = 28.0;
/// La hauteur minimale du contenu d'une carte : le bandeau reste lisible.
const HAUTEUR_MIN: f32 = 64.0;
const LISERE: f32 = 3.0;

// ---------------------------------------------------------------------
// Ce qu'une carte montre
// ---------------------------------------------------------------------

/// Une partie, telle qu'une carte la montre.
pub(super) struct Partie<'a> {
    pub id: &'a str,
    pub date: u64,
    pub carte: &'a str,
    pub mode: &'a str,
    pub duree_s: u32,
    /// Les vainqueurs d'abord, puis le meilleur score.
    pub joueurs: Vec<Joueur<'a>>,
}

/// Un membre dans une partie.
pub(super) struct Joueur<'a> {
    pub user_id: UserId,
    pub agent: &'a str,
    pub kda: (u16, u16, u16),
    pub acs: Option<u32>,
    pub manches: (u8, u8),
    pub gagne: Option<bool>,
    pub deroule: &'a str,
    pub mvp: bool,
    pub delta_rr: Option<i32>,
    /// Sans le MVP, qui a son étiquette.
    pub medailles: Vec<&'a MedailleGagnee>,
}

/// Ce qu'une partie a donné au groupe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Issue {
    Victoire,
    Defaite,
    Nul,
    /// Des membres de chaque côté.
    FaceAFace,
}

impl Issue {
    fn texte(self) -> &'static str {
        match self {
            Issue::Victoire => "Victoire",
            Issue::Defaite => "Défaite",
            Issue::Nul => "Nul",
            Issue::FaceAFace => "Face à face",
        }
    }

    fn teinte(self) -> Color32 {
        match self {
            Issue::Victoire => SPEAK,
            Issue::Defaite => DANGER,
            Issue::Nul => TEXT_DIM,
            Issue::FaceAFace => theme::INFO,
        }
    }
}

/// Le MVP du match, d'après la note ou d'après la médaille.
fn est_mvp(mvp: u8, medailles: &[&MedailleGagnee]) -> bool {
    mvp == ki_protocol::MVP_DU_MATCH || medailles.iter().any(|g| g.medaille == Medaille::Mvp)
}

impl<'a> Joueur<'a> {
    fn du_match(user_id: UserId, m: &'a MatchResume, point: Option<&PointRR>, md: Option<&'a MedaillesDuMatch>) -> Self {
        let medailles = md.map(medailles_a_montrer).unwrap_or_default();
        Joueur {
            user_id,
            agent: &m.agent,
            kda: (m.kills, m.deaths, m.assists),
            acs: acs_de(m).map(|v| v.round() as u32),
            manches: m.manches,
            gagne: m.gagne,
            deroule: m.manches_detail.as_ref().map_or("", |d| d.deroule.as_str()),
            mvp: est_mvp(m.mvp, &medailles),
            delta_rr: point.map(|p| p.delta),
            medailles: medailles.into_iter().filter(|g| g.medaille != Medaille::Mvp).collect(),
        }
    }

    fn de_la_soiree(j: &'a ki_protocol::JoueurSoiree) -> Self {
        let medailles = medailles_triees(&j.medailles);
        let manches = u32::from(j.manches.0) + u32::from(j.manches.1);
        Joueur {
            user_id: j.user_id,
            agent: &j.agent,
            kda: (j.kills, j.deaths, j.assists),
            acs: (manches > 0 && j.score > 0).then(|| j.score / manches),
            manches: j.manches,
            gagne: j.gagne,
            deroule: &j.deroule,
            mvp: est_mvp(j.mvp, &medailles),
            delta_rr: j.delta_rr,
            medailles: medailles.into_iter().filter(|g| g.medaille != Medaille::Mvp).collect(),
        }
    }
}

impl<'a> Partie<'a> {
    pub fn de_la_soiree(p: &'a PartieSoiree) -> Self {
        Partie::nouvelle(&p.id, p.date, &p.carte, &p.mode, p.duree_s, p.joueurs.iter().map(Joueur::de_la_soiree).collect())
    }

    fn nouvelle(id: &'a str, date: u64, carte: &'a str, mode: &'a str, duree_s: u32, mut joueurs: Vec<Joueur<'a>>) -> Self {
        joueurs.sort_by_key(|j| (j.gagne != Some(true), std::cmp::Reverse(j.acs.unwrap_or(0))));
        Partie { id, date, carte, mode, duree_s, joueurs }
    }

    /// Le résultat pour le groupe, et le score lu du côté des vainqueurs.
    pub fn issue(&self) -> (Issue, Option<(u8, u8)>) {
        let gagnes = self.joueurs.iter().any(|j| j.gagne == Some(true));
        let perdus = self.joueurs.iter().any(|j| j.gagne == Some(false));
        let issue = match (gagnes, perdus) {
            (true, false) => Issue::Victoire,
            (false, true) => Issue::Defaite,
            (true, true) => Issue::FaceAFace,
            (false, false) => Issue::Nul,
        };
        let score = self.joueurs.first().map(|j| j.manches).filter(|m| *m != (0, 0));
        (issue, score)
    }

    fn a(&self, qui: UserId) -> bool {
        self.joueurs.iter().any(|j| j.user_id == qui)
    }
}

/// Les parties du groupe depuis `depuis`, de la plus récente à la plus
/// ancienne : les matchs de chacun, réunis par leur id — une partie jouée
/// à trois fait une seule carte.
pub(super) fn parties_du_groupe<'a>(lignes: &[Ligne<'a>], depuis: u64) -> Vec<Partie<'a>> {
    let mut tous: Vec<(UserId, &'a MatchResume)> = lignes
        .iter()
        .flat_map(|l| l.f.fiche.matchs.iter().map(move |m| (l.f.user_id, m)))
        .filter(|(_, m)| m.date >= depuis)
        .collect();
    tous.sort_by_key(|(_, m)| std::cmp::Reverse(m.date));
    // Le point de RR et les médailles de chacun pour chaque match.
    let points: HashMap<(UserId, &str), &PointRR> = lignes
        .iter()
        .flat_map(|l| l.f.fiche.historique_rr.iter().map(move |p| ((l.f.user_id, p.match_id.as_str()), p)))
        .filter(|((_, id), _)| !id.is_empty())
        .collect();
    let medailles: HashMap<UserId, &Medailles> =
        lignes.iter().filter_map(|l| l.f.fiche.medailles.as_ref().map(|m| (l.f.user_id, m))).collect();
    let ids: Vec<&str> = tous.iter().map(|(_, m)| m.id.as_str()).collect();
    regrouper(&ids)
        .into_iter()
        .map(|bloc| {
            let (_, m) = tous[bloc[0]];
            let joueurs = bloc
                .iter()
                .map(|&i| {
                    let (qui, m) = tous[i];
                    let point = points.get(&(qui, m.id.as_str())).copied();
                    let md = medailles.get(&qui).and_then(|x| x.du_match(&m.id));
                    Joueur::du_match(qui, m, point, md)
                })
                .collect();
            Partie::nouvelle(&m.id, m.date, &m.carte, &m.mode, m.duree_s, joueurs)
        })
        .collect()
}

/// Le bilan de ces parties pour le groupe : victoires, défaites, nuls (un
/// face-à-face n'est ni l'un ni l'autre), le temps de jeu, les RR à tous.
fn bilan_des_parties(parties: &[&Partie]) -> (u32, u32, u32, u32, i32) {
    let mut b = (0, 0, 0, 0u32, 0i32);
    for p in parties {
        match p.issue().0 {
            Issue::Victoire => b.0 += 1,
            Issue::Defaite => b.1 += 1,
            Issue::Nul => b.2 += 1,
            Issue::FaceAFace => {}
        }
        b.3 = b.3.saturating_add(p.duree_s);
        b.4 = p.joueurs.iter().fold(b.4, |rr, j| rr.saturating_add(j.delta_rr.unwrap_or(0)));
    }
    b
}

/// « aujourd'hui », « hier », « samedi 17 octobre » : la soirée d'une
/// partie, qui commence à 6 h du matin — une partie à 1 h est celle de la
/// veille.
fn jour_de(date: u64, aujourd_hui: u64) -> String {
    let Some(t) = chrono::Local.timestamp_millis_opt(date as i64).single() else { return String::new() };
    let soiree = debut_de_soiree(t);
    if soiree == aujourd_hui {
        return "aujourd'hui".to_string();
    }
    let veille = chrono::Local
        .timestamp_millis_opt(aujourd_hui as i64)
        .single()
        .map(|t| debut_de_soiree(t - chrono::Duration::hours(12)));
    if veille == Some(soiree) {
        return "hier".to_string();
    }
    match chrono::Local.timestamp_millis_opt(soiree as i64).single() {
        Some(s) => format!(
            "{} {} {}",
            accueil::JOURS_CAPITALES[s.weekday().num_days_from_monday() as usize],
            s.day(),
            accueil::MOIS_CAPITALES[s.month0() as usize]
        ),
        None => String::new(),
    }
}

// ---------------------------------------------------------------------
// La carte
// ---------------------------------------------------------------------

/// Une partie en carte : le bandeau de la carte à gauche et le liseré du
/// résultat, l'heure, l'issue et le score, les manches, puis une pastille
/// par membre. Un clic sur une pastille ouvre la fiche.
pub(super) fn carte_de_partie(ui: &mut Ui, p: &Partie, cx: &Contexte<'_>, noms: &Annuaire, demandes: &mut Vec<Demande>) {
    let bandeau = if ui.available_width() >= CARTE_ETROITE { BANDEAU } else { BANDEAU_ETROIT };
    let (issue, score) = p.issue();
    let marge = egui::Margin { left: 0, right: espace::L as i8, top: espace::M as i8, bottom: espace::M as i8 };
    let cadre = egui::Frame::NONE
        .fill(theme::BG_RAISED)
        .stroke(egui::Stroke::new(1.0, theme::BORDER_SOFT))
        .corner_radius(egui::CornerRadius::same(rayon::XL))
        .inner_margin(marge)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.add_space(bandeau + LISERE + espace::L - ui.spacing().item_spacing.x);
                ui.vertical(|ui| {
                    ui.set_min_height(HAUTEUR_MIN);
                    ui.set_width(ui.available_width());
                    contenu(ui, p, issue, score, cx, noms, demandes);
                });
            });
        });

    // Le bandeau, puis le liseré qui le sépare du contenu.
    let rect = cadre.response.rect;
    if !ui.is_rect_visible(rect) {
        return;
    }
    let zone = Rect::from_min_max(rect.min, Pos2::new(rect.left() + bandeau, rect.bottom()));
    let coins = egui::CornerRadius { nw: rayon::XL, sw: rayon::XL, ne: 0, se: 0 };
    let painter = ui.painter();
    match cx.catalogue.bandeau(p.carte).filter(|_| !p.carte.is_empty()) {
        Some(image) => {
            egui::Image::new(image).uv(graphes::uv_couvrant(image.size_vec2(), zone.size())).corner_radius(coins).paint_at(ui, zone);
            painter.rect_filled(zone, coins, Color32::from_black_alpha(80));
        }
        None => {
            painter.rect_filled(zone, coins, theme::BG_DEEP);
        }
    }
    let nom = if p.carte.is_empty() { "?".to_string() } else { p.carte.to_uppercase() };
    let police_nom = police::moyenne(ui.ctx(), if bandeau >= BANDEAU { texte::COURANT } else { texte::MINUSCULE });
    painter.with_clip_rect(zone).text(
        Pos2::new(zone.left() + espace::M, zone.bottom() - espace::M),
        egui::Align2::LEFT_BOTTOM,
        nom,
        police_nom,
        Color32::WHITE,
    );
    let lisere = Rect::from_min_max(Pos2::new(zone.right(), rect.top()), Pos2::new(zone.right() + LISERE, rect.bottom()));
    painter.rect_filled(lisere, 0.0, issue.teinte());
}

fn contenu(
    ui: &mut Ui,
    p: &Partie,
    issue: Issue,
    score: Option<(u8, u8)>,
    cx: &Contexte<'_>,
    noms: &Annuaire,
    demandes: &mut Vec<Demande>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = espace::S;
        ui.label(RichText::new(heure_de(p.date)).color(TEXT_FAINT).size(texte::PETIT));
        let mut titre = issue.texte().to_string();
        if let Some((a, b)) = score {
            titre.push_str(&format!(" {a}-{b}"));
        }
        ui.label(RichText::new(titre).font(police::moyenne(ui.ctx(), texte::CORPS)).color(issue.teinte()));
        if p.mode != MODE_CLASSE && !p.mode.is_empty() {
            ui::hint(ui, p.mode);
        }
        if p.duree_s > 0 {
            ui::hint(ui, duree_texte(p.duree_s));
        }
        if p.joueurs.len() > 1 {
            ui::hint(ui, format!("{} du groupe", p.joueurs.len()));
        }
    });
    if let Some(deroule) = p.joueurs.first().map(|j| j.deroule).filter(|d| !d.is_empty()) {
        ui.add_space(espace::XXS);
        graphes::cases_manches(ui, deroule);
    }
    ui.add_space(espace::S);
    let n = p.joueurs.len();
    let largeur = ui.available_width().min(n as f32 * PASTILLE_MAX + n.saturating_sub(1) as f32 * espace::S);
    ui.allocate_ui_with_layout(Vec2::new(largeur, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_width(largeur);
        ui::grille(ui, n, PASTILLE_MIN, espace::S, |ui, k| {
            let j = &p.joueurs[k];
            if pastille_de_joueur(ui, j, cx, noms, issue == Issue::FaceAFace).clicked() {
                demandes.push(Demande::OuvrirFiche(j.user_id, noms.nom(j.user_id)));
            }
        });
    });
}

/// « 21:47 », à l'heure locale.
pub(super) fn heure_de(ms: u64) -> String {
    chrono::Local.timestamp_millis_opt(ms as i64).single().map(|t| t.format("%H:%M").to_string()).unwrap_or_default()
}

/// Un membre dans une partie : le portrait de son agent, son nom, ses RR,
/// puis son K/D/A, son ACS, le MVP et ses médailles. La sienne est
/// entourée ; dans un face-à-face, un point dit de quel côté il était.
fn pastille_de_joueur(ui: &mut Ui, j: &Joueur, cx: &Contexte<'_>, noms: &Annuaire, face_a_face: bool) -> Response {
    let moi = cx.moi == Some(j.user_id);
    let bord = if moi { egui::Stroke::new(1.0, theme::alpha(ACCENT, 110)) } else { egui::Stroke::NONE };
    let cadre = egui::Frame::NONE
        .fill(theme::BG_DEEP)
        .stroke(bord)
        .corner_radius(egui::CornerRadius::same(rayon::L))
        .inner_margin(ki_ui::jetons::marge::symetrique(espace::S, espace::XS))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = espace::S;
                match cx.catalogue.agent(j.agent).filter(|_| !j.agent.is_empty()) {
                    Some(portrait) => {
                        ui.add(egui::Image::new(portrait).fit_to_exact_size(Vec2::splat(PORTRAIT_PASTILLE)).corner_radius(rayon::M));
                    }
                    None => {
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(PORTRAIT_PASTILLE), Sense::hover());
                        ui.painter().rect_filled(rect, egui::CornerRadius::same(rayon::M), theme::BG_HOVER);
                    }
                }
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = espace::XS;
                        if face_a_face {
                            let (point, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                            icons::dot(ui.painter(), point.center(), 3.0, teinte_de_score(j.gagne));
                        }
                        let police_nom = if moi { police::moyenne(ui.ctx(), texte::COURANT) } else { FontId::proportional(texte::COURANT) };
                        ui.label(RichText::new(noms.nom(j.user_id)).font(police_nom).color(noms.couleur(j.user_id)));
                        if let Some(delta) = j.delta_rr {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let (t, c) = signe(delta);
                                ui.label(RichText::new(format!("{t} RR")).font(police::moyenne(ui.ctx(), texte::PETIT)).color(c));
                            });
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = espace::XS;
                        ui.label(RichText::new(format!("{} / {} / {}", j.kda.0, j.kda.1, j.kda.2)).color(TEXT).size(texte::PETIT));
                        if let Some(acs) = j.acs {
                            ui.label(RichText::new(format!("{acs} ACS")).color(TEXT_DIM).size(texte::PETIT));
                        }
                        if j.mvp {
                            badge(ui, "MVP", theme::WARN);
                        }
                        for g in j.medailles.iter().take(2) {
                            let texte_ = if g.record { format!("{}★", nom_court(g.medaille)) } else { nom_court(g.medaille).to_string() };
                            badge(ui, &texte_, teinte_de_medaille(g.medaille));
                        }
                        if j.medailles.len() > 2 {
                            ui.label(RichText::new(format!("+{}", j.medailles.len() - 2)).color(TEXT_FAINT).size(texte::MINUSCULE));
                        }
                    });
                });
            });
        });
    let mut survol = if j.agent.is_empty() { "ouvrir sa fiche".to_string() } else { format!("{} · ouvrir sa fiche", j.agent) };
    for g in &j.medailles {
        let record = if g.record { " · record de l'acte" } else { "" };
        survol.push_str(&format!("\n{} — {}{record}", g.medaille.nom(), g.medaille.valeur(g.valeur)));
    }
    cadre.response.interact(Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(survol)
}

// ---------------------------------------------------------------------
// L'onglet
// ---------------------------------------------------------------------

impl PageValo {
    /// L'onglet Matchs : la période, le mode, le membre, le bilan du
    /// groupe, puis les parties en cartes, soirée par soirée.
    pub(super) fn matchs_du_groupe(
        &mut self,
        ui: &mut Ui,
        lignes: &[Ligne],
        noms: &Annuaire,
        cx: &Contexte<'_>,
        maintenant: u64,
        demandes: &mut Vec<Demande>,
    ) {
        let periode = self.periode;
        let depuis = maintenant.saturating_sub(periode.jours() * JOUR_MS);
        let toutes = parties_du_groupe(lignes, depuis);
        let modes: BTreeSet<&str> = toutes.iter().map(|p| p.mode).filter(|m| !m.is_empty()).collect();
        let modes: Vec<&str> = modes.into_iter().collect();
        // Un mode qui n'est plus dans la période ne filtre plus rien.
        if self.filtre_mode.as_deref().is_some_and(|m| !modes.contains(&m)) {
            self.filtre_mode = None;
        }
        let parties: Vec<&Partie> = toutes
            .iter()
            .filter(|p| self.filtre_mode.as_deref().is_none_or(|m| p.mode == m))
            .filter(|p| self.filtre_membre.is_none_or(|qui| p.a(qui)))
            .collect();

        let aide = format!("sur {} · une carte par partie, jouée seul ou ensemble · clique un joueur pour sa fiche", periode.titre());
        ui::section(ui, Icon::Play, "Les matchs du groupe", Some(&aide), |ui| {
            self.choix_de_periode(ui);
            if modes.len() > 1 {
                ui.add_space(espace::XS);
                let mut choix = self.filtre_mode.as_deref().and_then(|m| modes.iter().position(|x| *x == m)).map_or(0, |i| i + 1);
                let options: Vec<(usize, &str)> =
                    std::iter::once((0, "Tous les modes")).chain(modes.iter().enumerate().map(|(i, m)| (i + 1, *m))).collect();
                if ui::pastilles(ui, &mut choix, &options) {
                    self.filtre_mode = choix.checked_sub(1).map(|i| modes[i].to_string());
                    self.cartes_matchs = CARTES_PAR_PAGE;
                }
            }
            if lignes.len() > 1 {
                ui.add_space(espace::XS);
                let mut choix = self.filtre_membre.and_then(|qui| lignes.iter().position(|l| l.f.user_id == qui)).map_or(0, |i| i + 1);
                let options: Vec<(usize, &str)> = std::iter::once((0, "Tout le monde"))
                    .chain(lignes.iter().enumerate().map(|(i, l)| (i + 1, l.f.username.as_str())))
                    .collect();
                if ui::pastilles(ui, &mut choix, &options) {
                    self.filtre_membre = choix.checked_sub(1).map(|i| lignes[i].f.user_id);
                    self.cartes_matchs = CARTES_PAR_PAGE;
                }
            }
            ui.add_space(espace::M);
            let (victoires, defaites, nuls, duree, rr) = bilan_des_parties(&parties);
            if parties.is_empty() {
                ui::hint(ui, "Aucune partie connue sur cette période.");
                return;
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = espace::M;
                ui.label(
                    RichText::new(format!("{victoires} – {defaites}"))
                        .font(police::moyenne(ui.ctx(), texte::KPI))
                        .color(match victoires.cmp(&defaites) {
                            std::cmp::Ordering::Greater => SPEAK,
                            std::cmp::Ordering::Less => DANGER,
                            std::cmp::Ordering::Equal => TEXT,
                        }),
                );
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let (t, c) = signe(rr);
                    ui.label(RichText::new(format!("{t} RR")).font(police::moyenne(ui.ctx(), texte::CORPS)).color(c));
                    let mut detail = pluriel(parties.len() as u32, "partie");
                    if nuls > 0 {
                        detail.push_str(&format!(" · {}", pluriel(nuls, "nul")));
                    }
                    if duree > 0 {
                        detail.push_str(&format!(" · {} de jeu", duree_texte(duree)));
                    }
                    ui::hint(ui, detail);
                });
            });
            ui.add_space(espace::XS);
            graphes::barre_vd(ui, victoires, defaites, nuls, ui.available_width())
                .on_hover_text("une partie jouée ensemble compte une fois ; un face-à-face, pour personne");
        });
        if parties.is_empty() {
            return;
        }

        let aujourd_hui = debut_de_soiree(chrono::Local::now());
        let mut jour_courant = None;
        for p in parties.iter().take(self.cartes_matchs) {
            let jour = jour_de(p.date, aujourd_hui);
            if jour_courant.as_ref() != Some(&jour) {
                if jour_courant.is_some() {
                    ui.add_space(espace::S);
                }
                ui::section_label(ui, &jour);
                jour_courant = Some(jour);
            }
            ui.push_id(("carte_de_partie", p.id, p.date), |ui| carte_de_partie(ui, p, cx, noms, demandes));
            ui.add_space(espace::S);
        }
        let reste = parties.len().saturating_sub(self.cartes_matchs);
        if reste > 0 {
            ui.add_space(espace::XS);
            let libelle = match reste {
                1 => "voir l'autre".to_string(),
                n if n <= CARTES_PAR_PAGE => format!("voir les {n} autres"),
                n => format!("voir {CARTES_PAR_PAGE} de plus, sur {n}"),
            };
            if ui::button(ui, Icon::Plus, &libelle).clicked() {
                self.cartes_matchs += CARTES_PAR_PAGE;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ki_protocol::FicheMembre;

    fn joue(id: &str, date: u64, gagne: bool, score: u32) -> MatchResume {
        MatchResume { score, ..super::super::tests::match_de_test(id, date, MODE_CLASSE, gagne, &[]) }
    }

    fn membre(user_id: UserId, nom: &str, matchs: Vec<MatchResume>, points: Vec<PointRR>) -> FicheMembre {
        let fiche = FicheValorant { riot_id: format!("{nom}#EUW"), matchs, historique_rr: points, ..Default::default() };
        FicheMembre { user_id, username: nom.into(), bilan: None, fiche }
    }

    /// Une partie jouée ensemble fait une seule carte, les vainqueurs puis
    /// le meilleur score d'abord ; deux membres de part et d'autre font un
    /// face-à-face ; un match sans id reste seul.
    #[test]
    fn une_partie_jouee_ensemble_fait_une_seule_carte() {
        let point = |id: &str, delta| PointRR { match_id: id.into(), date: 5, tier: 15, delta, ..Default::default() };
        let stats = vec![
            membre(1, "A", vec![joue("x", 5_000, true, 4_000), joue("y", 4_000, false, 4_000), joue("", 1_000, true, 4_000)], vec![point("x", 20)]),
            membre(2, "B", vec![joue("x", 5_000, true, 6_000)], Vec::new()),
            membre(3, "C", vec![joue("y", 4_000, true, 4_000), joue("z", 3_000, false, 4_000), joue("", 2_000, false, 4_000)], vec![point("z", -15)]),
        ];
        let (lignes, _) = lignes_du_groupe(&stats, 10_000);
        let parties = parties_du_groupe(&lignes, 0);
        let resume: Vec<(&str, Vec<UserId>, Issue)> = parties.iter().map(|p| (p.id, p.joueurs.iter().map(|j| j.user_id).collect(), p.issue().0)).collect();
        assert_eq!(
            resume,
            [
                ("x", vec![2, 1], Issue::Victoire),
                ("y", vec![3, 1], Issue::FaceAFace),
                ("z", vec![3], Issue::Defaite),
                ("", vec![3], Issue::Defaite),
                ("", vec![1], Issue::Victoire),
            ]
        );
        assert_eq!(parties[0].joueurs[1].delta_rr, Some(20), "le point de RR de A sur x");
        assert_eq!(parties[1].issue().1, Some((13, 9)), "le score lu du côté des vainqueurs");
        let toutes: Vec<&Partie> = parties.iter().collect();
        assert_eq!(bilan_des_parties(&toutes), (2, 2, 0, 0, 5), "un face-à-face ne compte pour personne");
        assert!(parties_du_groupe(&lignes, 4_500).iter().all(|p| p.date >= 4_500));
    }

    /// La soirée d'une partie commence à 6 h : à 5 h, c'est encore celle de
    /// la veille.
    #[test]
    fn les_parties_se_rangent_par_soiree() {
        let aujourd_hui = debut_de_soiree(chrono::Local::now());
        let heure = 3_600_000;
        assert_eq!(jour_de(aujourd_hui + heure, aujourd_hui), "aujourd'hui");
        assert_eq!(jour_de(aujourd_hui - heure, aujourd_hui), "hier", "5 h du matin");
        let avant = jour_de(aujourd_hui - 3 * JOUR_MS + 15 * heure, aujourd_hui);
        assert!(accueil::JOURS_CAPITALES.iter().any(|j| avant.starts_with(j)), "{avant}");
    }
}
