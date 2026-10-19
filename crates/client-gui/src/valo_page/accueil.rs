//! L'accueil de la page VALORANT — V9.2 de PLAN-VALORANT-2.md, d'après la
//! maquette validée (pistes B et C). On ouvre la page et l'on sait trois
//! choses : comment se passe la soirée, qui joue, où en est chacun.
//!
//! De haut en bas :
//! 1. l'affiche de la soirée : le score du groupe, ses RR, la partie en
//!    direct et le MVP ;
//! 2. le vestiaire : une carte par membre lié, rangée par les RR de la
//!    semaine ;
//! 3. le fil de la soirée, partie par partie, à côté des records et des
//!    duos ;
//! 4. quand le groupe joue — le groupe, moi, ou les deux — et ses repères ;
//! 5. le classement, trié par les 7 jours, et les douze médailles.
//!
//! Les parties du fil sont les cartes de l'onglet Matchs ([`matchs`]) ; le
//! classement est celui de son onglet ([`classement`]).
//!
//! L'affiche et le fil viennent de `ServerMsg::Soiree` ; le vestiaire,
//! les records et le classement des fiches de `StatsValorant` ; « en
//! direct » de la présence. Rien de neuf n'est demandé à HenrikDev.

use ki_ui::jetons::police;

use super::*;
use crate::icons::Icon;

/// La carte de chaleur de l'accueil.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heures {
    Groupe,
    Moi,
    LesDeux,
}

impl Heures {
    const TOUTES: [(Heures, &'static str); 3] =
        [(Heures::Groupe, "Le groupe"), (Heures::Moi, "Moi"), (Heures::LesDeux, "Moi et le groupe")];
}

pub(super) const JOURS_CAPITALES: [&str; 7] = ["LUNDI", "MARDI", "MERCREDI", "JEUDI", "VENDREDI", "SAMEDI", "DIMANCHE"];
pub(super) const MOIS_CAPITALES: [&str; 12] =
    ["JANVIER", "FÉVRIER", "MARS", "AVRIL", "MAI", "JUIN", "JUILLET", "AOÛT", "SEPTEMBRE", "OCTOBRE", "NOVEMBRE", "DÉCEMBRE"];
/// La hauteur de l'affiche.
const HAUTEUR_AFFICHE: f32 = 132.0;
/// En dessous, le fil et les records passent l'un sous l'autre.
const DEUX_COLONNES: f32 = 900.0;
/// Les semaines de la carte de chaleur : trente jours.
const SEMAINES: f32 = 30.0 / 7.0;
/// La largeur d'une carte du vestiaire : cinq tiennent côte à côte dès
/// 900 px.
const CARTE_VESTIAIRE: f32 = 164.0;
/// Les duos de la soirée montrés, au plus.
const DUOS_DU_SOIR: usize = 4;

// ---------------------------------------------------------------------
// Ce que la soirée dit
// ---------------------------------------------------------------------

/// Le bilan de la soirée du groupe : ses victoires et défaites (une
/// partie jouée ensemble compte une fois), les RR gagnés à tous, le temps
/// de jeu, et son MVP.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Soiree {
    pub victoires: u32,
    pub defaites: u32,
    pub rr: i32,
    pub parties: usize,
    pub duree_s: u32,
    pub mvp: Option<UserId>,
}

/// Le bilan de ces parties. Le résultat d'une partie est celui de la
/// plupart des membres qui y étaient ; le MVP de la soirée est celui qui
/// a le plus de MVP, puis la meilleure note moyenne sur deux parties au
/// moins.
pub(super) fn soiree_de(parties: &[PartieSoiree]) -> Soiree {
    let mut s = Soiree { parties: parties.len(), ..Soiree::default() };
    let mut par_membre: BTreeMap<UserId, (u32, u32, u32)> = BTreeMap::new();
    for p in parties {
        s.duree_s = s.duree_s.saturating_add(p.duree_s);
        let gagnes = p.joueurs.iter().filter(|j| j.gagne == Some(true)).count();
        let perdus = p.joueurs.iter().filter(|j| j.gagne == Some(false)).count();
        if gagnes > perdus {
            s.victoires += 1;
        } else if perdus > gagnes {
            s.defaites += 1;
        }
        for j in &p.joueurs {
            s.rr = s.rr.saturating_add(j.delta_rr.unwrap_or(0));
            let e = par_membre.entry(j.user_id).or_default();
            e.0 += u32::from(j.mvp == ki_protocol::MVP_DU_MATCH) * 2 + u32::from(j.mvp == ki_protocol::MVP_D_EQUIPE);
            if j.perf > 0 {
                e.1 += u32::from(j.perf);
                e.2 += 1;
            }
        }
    }
    s.mvp = par_membre
        .iter()
        .filter(|(_, &(mvp, _, n))| mvp > 0 || n >= 2)
        .max_by_key(|(_, &(mvp, total, n))| (mvp, total.checked_div(n).unwrap_or(0)))
        .map(|(&id, _)| id);
    s
}

/// Un duo : deux membres, leurs parties dans le même camp, leurs victoires.
pub(super) type Duo = (UserId, UserId, u32, u32);

/// Les duos de la soirée, le meilleur d'abord : le plus de victoires, puis
/// le moins de défaites. Deux membres sont dans le même camp quand la
/// partie leur donne le même résultat ; un nul ne dit rien.
pub(super) fn duos_de_soiree(parties: &[PartieSoiree]) -> Vec<Duo> {
    let mut paires: BTreeMap<(UserId, UserId), (u32, u32)> = BTreeMap::new();
    for p in parties {
        for (i, a) in p.joueurs.iter().enumerate() {
            for b in &p.joueurs[i + 1..] {
                let (Some(ga), Some(gb)) = (a.gagne, b.gagne) else { continue };
                if ga != gb || a.user_id == b.user_id {
                    continue;
                }
                let e = paires.entry((a.user_id.min(b.user_id), a.user_id.max(b.user_id))).or_default();
                e.0 += 1;
                e.1 += u32::from(ga);
            }
        }
    }
    let mut duos: Vec<Duo> = paires.into_iter().map(|((a, b), (parties, victoires))| (a, b, parties, victoires)).collect();
    duos.sort_by_key(|&(_, _, parties, victoires)| (std::cmp::Reverse(victoires), parties - victoires));
    duos
}

/// Le début de la plus longue fenêtre de deux heures où le groupe joue
/// (jour, heure), d'une heure sur l'autre — 23 h et minuit se suivent, le
/// dimanche soir déborde sur lundi.
pub(super) fn grand_rendez_vous(cases: &[u16]) -> Option<(usize, usize)> {
    let valeur = |i: usize| u32::from(cases.get(i % 168).copied().unwrap_or(0));
    (0..168).map(|i| (i, valeur(i) + valeur(i + 1))).filter(|(_, v)| *v > 0).max_by_key(|(i, v)| (*v, std::cmp::Reverse(*i))).map(|(i, _)| (i / 24, i % 24))
}

/// « 21 h – 23 h », à cheval sur minuit s'il le faut.
fn creneau(heure: usize) -> String {
    format!("{heure} h – {} h", (heure + 2) % 24)
}

// ---------------------------------------------------------------------
// La page
// ---------------------------------------------------------------------

impl PageValo {
    pub(super) fn accueil(&mut self, ui: &mut Ui, cx: &Contexte<'_>, lignes: &[Ligne], noms: &Annuaire, demandes: &mut Vec<Demande>) {
        affiche(ui, cx, noms);
        ui.add_space(espace::L);
        if !cx.recu {
            ui.label(RichText::new("demande au serveur…").color(TEXT_DIM));
            return;
        }
        if lignes.is_empty() {
            ui::section(ui, Icon::User, "Le vestiaire", None, |ui| {
                ui::hint(ui, "Personne n'a encore lié de compte Riot — ⚙ → Jeu.");
            });
        } else {
            vestiaire(ui, cx, lignes, demandes);
        }
        if ui.available_width() >= DEUX_COLONNES {
            let largeur = ui.available_width();
            let gauche = (largeur * 0.58).floor();
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = espace::L;
                ui.allocate_ui_with_layout(Vec2::new(gauche, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_width(gauche);
                    fil(ui, cx, noms, demandes);
                });
                let droite = ui.available_width();
                ui.allocate_ui_with_layout(Vec2::new(droite, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_width(droite);
                    records_et_duos(ui, lignes, noms, cx);
                });
            });
        } else {
            fil(ui, cx, noms, demandes);
            records_et_duos(ui, lignes, noms, cx);
        }
        self.quand_on_joue(ui, cx);
        ui::section(
            ui,
            Icon::Crown,
            "Classement",
            Some("trié par les RR des 7 jours · un clic sur un en-tête change le tri, sur une ligne ouvre la fiche"),
            |ui| {
                self.choix_de_periode(ui);
                ui.add_space(espace::S);
                self.classement_tableau(ui, lignes, noms, cx, demandes);
                medailles_du_groupe(ui, lignes, noms, cx.moi, demandes, &Medaille::TOUTES);
            },
        );
    }

    /// La carte de chaleur, au choix du groupe, de moi ou des deux, et ses
    /// trois repères.
    fn quand_on_joue(&mut self, ui: &mut Ui, cx: &Contexte<'_>) {
        let decalage = chrono::Local::now().offset().local_minus_utc() / 3600;
        let groupe = activite_locale(cx.activite, decalage);
        let trente = maintenant_ms().saturating_sub(30 * JOUR_MS);
        let moi = cx.ma_fiche.map(|f| cases_des_heures(f.matchs.iter().filter(|m| m.date >= trente).map(|m| m.date)));
        ui::section(
            ui,
            Icon::Sliders,
            "Quand le groupe joue",
            Some("les parties commencées sur 30 jours, tous modes, à l'heure locale"),
            |ui| {
                ui::pastilles(ui, &mut self.heures, &Heures::TOUTES);
                ui.add_space(espace::S);
                let (cases, contour) = match (self.heures, &moi) {
                    (Heures::Groupe, _) => (groupe.as_slice(), None),
                    (Heures::Moi, Some(moi)) => (moi.as_slice(), None),
                    (Heures::LesDeux, Some(moi)) => (moi.as_slice(), Some(groupe.as_slice())),
                    (_, None) => {
                        ui::hint(ui, "Ta fiche n'est pas encore arrivée : lie ton compte Riot dans ⚙ → Jeu si ce n'est pas fait.");
                        return;
                    }
                };
                if cases.iter().all(|&v| v == 0) {
                    ui::hint(ui, "Aucune partie sur 30 jours.");
                    return;
                }
                let montrer_reperes = |ui: &mut Ui| reperes(ui, &groupe);
                let info = |jour: usize, heure: usize| {
                    let n = cases.get(jour * 24 + heure).copied().unwrap_or(0);
                    format!(
                        "{} {heure}h–{}h · {} · heure locale, à une heure près au changement d'heure",
                        JOURS_LONGS.get(jour).copied().unwrap_or("?"),
                        heure + 1,
                        pluriel(u32::from(n), "partie")
                    )
                };
                let plus = graphes::EnPlus { encadree: Some(graphes::case_de_maintenant()), contour };
                if ui.available_width() >= DEUX_COLONNES {
                    let carte = (ui.available_width() - 260.0).max(400.0);
                    ui.horizontal_top(|ui| {
                        ui.allocate_ui_with_layout(Vec2::new(carte, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                            ui.set_width(carte);
                            graphes::heatmap_semaine(ui, cases, plus, info);
                        });
                        ui.add_space(espace::L);
                        ui.vertical(|ui| montrer_reperes(ui));
                    });
                } else {
                    graphes::heatmap_semaine(ui, cases, plus, info);
                    ui.add_space(espace::S);
                    montrer_reperes(ui);
                }
                if contour.is_some() {
                    ui::hint(ui, "en vert, mes parties ; en contour bleu, celles du groupe");
                }
            },
        );
    }
}

/// Les trois repères du groupe : le grand rendez-vous, le soir le plus
/// joué, et ce qu'on joue d'habitude à cette heure-ci.
fn reperes(ui: &mut Ui, groupe: &[u16]) {
    let repere = |ui: &mut Ui, titre: &str, valeur: String| {
        ui::section_label(ui, titre);
        ui.label(RichText::new(valeur).font(police::moyenne(ui.ctx(), texte::CORPS)).color(TEXT));
        ui.add_space(espace::XS);
    };
    match grand_rendez_vous(groupe) {
        Some((jour, heure)) => repere(ui, "le grand rendez-vous", format!("{}, {}", JOURS_LONGS[jour], creneau(heure))),
        None => repere(ui, "le grand rendez-vous", TIRET.to_string()),
    }
    let par_jour: Vec<u32> = (0..7).map(|j| groupe.iter().skip(j * 24).take(24).map(|&v| u32::from(v)).sum()).collect();
    if let Some((jour, _)) = par_jour.iter().enumerate().filter(|(_, &n)| n > 0).max_by_key(|(_, &n)| n) {
        repere(ui, "le soir le plus joué", JOURS_LONGS[jour].to_string());
    }
    let (jour, heure) = graphes::case_de_maintenant();
    let d_habitude = f32::from(groupe.get(jour * 24 + heure).copied().unwrap_or(0)) / SEMAINES;
    let maintenant = if d_habitude >= 0.5 {
        format!("d'habitude, ≈ {} à cette heure-ci", pluriel(d_habitude.round() as u32, "partie"))
    } else {
        "d'habitude, personne à cette heure-ci".to_string()
    };
    repere(ui, "maintenant", maintenant);
}

// ---------------------------------------------------------------------
// L'affiche
// ---------------------------------------------------------------------

/// « SAMEDI 18 OCTOBRE · LA SOIRÉE », du jour où la soirée a commencé.
fn titre_de_soiree() -> String {
    let debut = debut_de_soiree(chrono::Local::now());
    match chrono::Local.timestamp_millis_opt(debut as i64).single() {
        Some(t) => format!(
            "{} {} {} · LA SOIRÉE",
            JOURS_CAPITALES[t.weekday().num_days_from_monday() as usize],
            t.day(),
            MOIS_CAPITALES[t.month0() as usize]
        ),
        None => "LA SOIRÉE".to_string(),
    }
}

/// Qui joue en ce moment : la première partie en cours que la présence
/// montre, et ceux qui y sont.
fn en_cours<'a>(cx: &'a Contexte<'_>) -> Option<(Cow<'a, JeuStatut>, Vec<&'a Member>)> {
    let joueurs: Vec<(&Member, &JeuStatut)> = cx
        .membres
        .iter()
        .filter(|m| m.online)
        .filter_map(|m| m.jeu.as_ref().filter(|j| j.est_valorant() && j.etat == JeuEtat::EnJeu).map(|j| (m, j)))
        .collect();
    let statuts: Vec<&JeuStatut> = joueurs.iter().map(|(_, j)| *j).collect();
    let groupe = regrouper_en_direct(&statuts).into_iter().next()?;
    let j = cx.catalogue.nommer(statuts[groupe[0]]);
    Some((j, groupe.iter().map(|&i| joueurs[i].0).collect()))
}

/// Le score de la soirée en grand sur le bandeau de la carte jouée, la
/// partie en direct et le MVP.
fn affiche(ui: &mut Ui, cx: &Contexte<'_>, noms: &Annuaire) {
    let s = soiree_de(cx.soiree);
    let direct = en_cours(cx);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), HAUTEUR_AFFICHE), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let carte = direct.as_ref().map(|(j, _)| j.carte.clone()).or_else(|| cx.soiree.first().map(|p| p.carte.clone())).unwrap_or_default();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::CornerRadius::same(rayon::XL), theme::BG_RAISED);
    if let Some(bandeau) = cx.catalogue.bandeau(&carte).filter(|_| !carte.is_empty()) {
        egui::Image::new(bandeau)
            .uv(graphes::uv_couvrant(bandeau.size_vec2(), rect.size()))
            .corner_radius(rayon::XL)
            .tint(Color32::from_white_alpha(150))
            .paint_at(ui, rect);
        if let Some(voile) = cx.catalogue.voile() {
            egui::Image::new(voile).corner_radius(rayon::XL).paint_at(ui, rect);
        }
    }
    painter.rect_stroke(rect, egui::CornerRadius::same(rayon::XL), egui::Stroke::new(1.0, theme::BORDER_SOFT), egui::StrokeKind::Inside);

    let dedans = rect.shrink(espace::XL);
    let gauche = dedans.left_top();
    painter.text(gauche, egui::Align2::LEFT_TOP, titre_de_soiree(), FontId::proportional(texte::MINUSCULE), Color32::from_white_alpha(190));
    if s.parties == 0 {
        painter.text(
            gauche + Vec2::new(0.0, 22.0),
            egui::Align2::LEFT_TOP,
            if direct.is_some() { "la soirée commence" } else { "pas encore de partie ce soir" },
            police::moyenne(ui.ctx(), texte::TITRE_PAGE),
            TEXT,
        );
        if !cx.soiree_servie {
            painter.text(
                gauche + Vec2::new(0.0, 56.0),
                egui::Align2::LEFT_TOP,
                "ce serveur ne raconte pas encore la soirée",
                FontId::proportional(texte::PETIT),
                TEXT_DIM,
            );
        }
    } else {
        let couleur = match s.victoires.cmp(&s.defaites) {
            std::cmp::Ordering::Greater => SPEAK,
            std::cmp::Ordering::Less => DANGER,
            std::cmp::Ordering::Equal => TEXT,
        };
        let score = painter.text(
            gauche + Vec2::new(0.0, 16.0),
            egui::Align2::LEFT_TOP,
            format!("{} – {}", s.victoires, s.defaites),
            police::moyenne(ui.ctx(), texte::KPI * 1.5),
            couleur,
        );
        let (rr, teinte) = signe(s.rr);
        let rr_texte = painter.text(
            Pos2::new(score.right() + espace::L, score.center().y - 2.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{rr} RR"),
            police::moyenne(ui.ctx(), texte::TITRE),
            teinte,
        );
        painter.text(
            Pos2::new(rr_texte.left(), score.center().y + 2.0),
            egui::Align2::LEFT_TOP,
            "à nous tous",
            FontId::proportional(texte::PETIT),
            Color32::from_white_alpha(190),
        );
        let heures = s.duree_s / 3600;
        let minutes = (s.duree_s % 3600) / 60;
        let temps = if heures > 0 { format!("{heures} h {minutes:02}") } else { format!("{minutes} min") };
        painter.text(
            Pos2::new(gauche.x, score.bottom() + espace::XS),
            egui::Align2::LEFT_TOP,
            format!("{} · {temps} de jeu", pluriel(s.parties as u32, "partie")),
            FontId::proportional(texte::COURANT),
            Color32::from_white_alpha(210),
        );
    }

    // À droite : la partie en direct, puis le MVP.
    let droite = dedans.right_top();
    let mut y = droite.y;
    if let Some((j, joueurs)) = &direct {
        let (_, score) = etat_en_direct(j);
        let mut texte_ = format!("● en direct · {}", if j.carte.is_empty() { "partie" } else { j.carte.as_str() });
        if let Some((score, _)) = &score {
            texte_.push_str(&format!(" · {score}"));
        }
        let r = painter.text(Pos2::new(droite.x, y), egui::Align2::RIGHT_TOP, texte_, police::moyenne(ui.ctx(), texte::CORPS), SPEAK);
        y = r.bottom() + espace::XXS;
        let qui = joueurs.iter().map(|m| m.username.as_str()).collect::<Vec<_>>().join(", ");
        let r = painter.text(Pos2::new(droite.x, y), egui::Align2::RIGHT_TOP, qui, FontId::proportional(texte::PETIT), Color32::from_white_alpha(200));
        y = r.bottom() + espace::M;
    }
    if let Some(mvp) = s.mvp {
        let r = painter.text(
            Pos2::new(droite.x, y),
            egui::Align2::RIGHT_TOP,
            "MVP DE LA SOIRÉE",
            FontId::proportional(texte::MINUSCULE),
            Color32::from_white_alpha(170),
        );
        painter.text(
            Pos2::new(droite.x, r.bottom() + espace::XXS),
            egui::Align2::RIGHT_TOP,
            noms.nom(mvp),
            police::moyenne(ui.ctx(), texte::TITRE),
            theme::WARN,
        );
    }
}

// ---------------------------------------------------------------------
// Le vestiaire
// ---------------------------------------------------------------------

/// Où en est un membre d'après sa présence, et sa couleur.
fn statut_de(m: Option<&Member>) -> (&'static str, Color32) {
    let Some(m) = m.filter(|m| m.online) else { return ("hors ligne", TEXT_FAINT) };
    match m.jeu.as_ref().filter(|j| j.est_valorant()).map(|j| j.etat) {
        Some(JeuEtat::EnJeu) => ("en partie", SPEAK),
        Some(JeuEtat::PreGame) => ("sélection", ACCENT),
        Some(JeuEtat::Menus) if m.jeu.as_ref().is_some_and(|j| !j.libelle_file().is_empty()) => ("en file", ACCENT),
        Some(_) => ("au menu", theme::INFO),
        None => ("en ligne", TEXT_DIM),
    }
}

/// Une carte par membre lié, de celui qui a gagné le plus de RR cette
/// semaine à celui qui en a perdu le plus. La sienne est entourée.
fn vestiaire(ui: &mut Ui, cx: &Contexte<'_>, lignes: &[Ligne], demandes: &mut Vec<Demande>) {
    let mut ordre: Vec<&Ligne> = lignes.iter().collect();
    ordre.sort_by_key(|l| (std::cmp::Reverse(l.bilan.sept_jours.rr), std::cmp::Reverse((l.f.fiche.rang.tier, l.f.fiche.rang.rr))));
    ui::section(
        ui,
        Icon::User,
        "Le vestiaire",
        Some("rangé par les RR gagnés sur 7 jours · clique une carte pour ouvrir la fiche"),
        |ui| {
            ui::grille(ui, ordre.len(), CARTE_VESTIAIRE, espace::L, |ui, i| {
                if carte_de_membre(ui, i + 1, ordre[i], cx).clicked() {
                    demandes.push(Demande::OuvrirFiche(ordre[i].f.user_id, ordre[i].f.username.clone()));
                }
            });
        },
    );
}

fn carte_de_membre(ui: &mut Ui, place: usize, l: &Ligne, cx: &Contexte<'_>) -> Response {
    let f = &l.f.fiche;
    let membre = cx.membres.iter().find(|m| m.user_id == l.f.user_id);
    let mien = cx.moi == Some(l.f.user_id);
    let couleur = membre.map_or_else(|| theme::color_for(&l.f.username), crate::couleur_de_membre);
    let cadre = egui::Frame::NONE
        .fill(theme::BG_RAISED)
        .stroke(egui::Stroke::new(if mien { 1.5 } else { 1.0 }, if mien { ACCENT } else { theme::BORDER_SOFT }))
        .corner_radius(egui::CornerRadius::same(rayon::XL))
        .inner_margin(ki_ui::jetons::marge::egale(espace::L))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("#{place}")).font(police::moyenne(ui.ctx(), texte::TITRE)).color(TEXT_FAINT));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (texte_, teinte) = statut_de(membre);
                    ui.label(RichText::new(texte_).color(teinte).size(texte::PETIT));
                    let (point, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                    icons::dot(ui.painter(), point.center(), 3.0, teinte);
                });
            });
            ui.horizontal(|ui| {
                match cx.rangs.texture(f.rang.tier).filter(|_| f.rang.tier >= 3) {
                    Some(icone) => {
                        ui.add(egui::Image::new(icone).fit_to_exact_size(Vec2::splat(34.0)));
                    }
                    None => {
                        ui.allocate_exact_size(Vec2::splat(34.0), Sense::hover());
                    }
                }
                ui.vertical(|ui| {
                    ui.label(RichText::new(&l.f.username).font(police::moyenne(ui.ctx(), texte::TITRE)).color(couleur));
                    let rang = if f.rang.tier >= 3 { format!("{} · {} RR", nom_de_rang(f.rang.tier), f.rang.rr) } else { "non classé".to_string() };
                    ui.label(RichText::new(rang).color(graphes::couleur_de_rang(f.rang.tier)).size(texte::PETIT));
                });
            });
            ui.add_space(espace::XS);
            let (rr, teinte) = signe(l.bilan.sept_jours.rr);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{rr} RR")).font(police::moyenne(ui.ctx(), texte::CORPS)).color(teinte));
                ui::hint(ui, "sur 7 jours");
            });
            ui.horizontal(|ui| {
                let forme: Vec<i8> = l.bilan.forme.iter().take(5).rev().copied().collect();
                if !forme.is_empty() {
                    graphes::bande_forme(ui, &forme, Vec2::new(9.0, 12.0), None);
                }
                if let Some((agent, _, _)) = l.bilan.agents.first() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(agent).color(TEXT_DIM).size(texte::PETIT));
                        if let Some(portrait) = cx.catalogue.agent(agent) {
                            ui.add(egui::Image::new(portrait).fit_to_exact_size(Vec2::splat(20.0)));
                        }
                    });
                }
            });
        });
    cadre.response.interact(Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("ouvrir sa fiche")
}

// ---------------------------------------------------------------------
// Le fil de la soirée
// ---------------------------------------------------------------------

/// La partie en cours en tête, puis chaque partie de la soirée, la plus
/// récente d'abord, avec les passages de palier qu'elle a valus.
fn fil(ui: &mut Ui, cx: &Contexte<'_>, noms: &Annuaire, demandes: &mut Vec<Demande>) {
    ui::section(ui, Icon::Play, "Le fil de la soirée", Some("depuis 6 h du matin · une carte par partie, jouée seul ou ensemble"), |ui| {
        if let Some((j, joueurs)) = en_cours(cx) {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = espace::S;
                badge(ui, "EN COURS", SPEAK);
                if let Some(bandeau) = cx.catalogue.bandeau(&j.carte).filter(|_| !j.carte.is_empty()) {
                    pastille_carte(ui, &j.carte, bandeau, HAUTEUR_PASTILLE, texte::PETIT);
                } else if !j.carte.is_empty() {
                    ui.label(RichText::new(&j.carte).color(TEXT).size(texte::COURANT));
                }
                let (texte_, score) = etat_en_direct(&j);
                if let Some((score, teinte)) = score {
                    ui.label(RichText::new(score).font(police::moyenne(ui.ctx(), texte::CORPS)).color(teinte));
                }
                ui.label(RichText::new(texte_).color(TEXT_DIM).size(texte::PETIT));
                let qui = joueurs.iter().map(|m| m.username.as_str()).collect::<Vec<_>>().join(", ");
                ui.label(RichText::new(qui).color(TEXT_DIM).size(texte::PETIT));
            });
            ui.add_space(espace::S);
        }
        if !cx.soiree_servie {
            ui::hint(ui, "Ce serveur ne raconte pas encore la soirée : il faut le mettre à jour.");
            return;
        }
        if cx.soiree.is_empty() {
            ui::hint(ui, "Pas encore de partie depuis 6 h du matin.");
            return;
        }
        for p in cx.soiree {
            for j in p.joueurs.iter().filter(|j| j.tier_avant >= 3 && j.tier >= 3 && j.tier != j.tier_avant) {
                let (fleche, verbe, teinte) =
                    if j.tier > j.tier_avant { ("▲", "passe", SPEAK) } else { ("▼", "redescend en", DANGER) };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(fleche).color(teinte).size(texte::PETIT));
                    ui.label(RichText::new(noms.nom(j.user_id)).color(noms.couleur(j.user_id)).size(texte::COURANT));
                    ui.label(RichText::new(format!("{verbe} {}", nom_de_rang(j.tier))).color(graphes::couleur_de_rang(j.tier)).size(texte::COURANT));
                });
            }
            ui.push_id(("partie_du_soir", &p.id, p.date), |ui| {
                matchs::carte_de_partie(ui, &matchs::Partie::de_la_soiree(p), cx, noms, demandes);
            });
            ui.add_space(espace::S);
        }
        if let Some(premiere) = cx.soiree.last() {
            ui::hint(ui, format!("la soirée commence · {}", matchs::heure_de(premiere.date)));
        }
    });
}

/// Les records de la semaine, puis les duos.
fn records_et_duos(ui: &mut Ui, lignes: &[Ligne], noms: &Annuaire, cx: &Contexte<'_>) {
    ui::section(ui, Icon::Star, "Les records", Some("sur 7 jours, cinq classés au moins"), |ui| {
        records(ui, lignes, Periode::Sept, cx.rangs);
    });
    let ce_soir = duos_de_soiree(cx.soiree);
    let trente = paires_de_duos(lignes.iter().map(|l| (l.f.user_id, l.bilan.duos.as_slice())));
    if ce_soir.is_empty() && trente.is_empty() {
        return;
    }
    ui.add_space(espace::L);
    ui::section(ui, Icon::Target, "Les duos", Some("les parties jouées dans le même camp"), |ui| {
        if !ce_soir.is_empty() {
            ui::section_label(ui, "ce soir");
            for &(a, b, parties, victoires) in ce_soir.iter().take(DUOS_DU_SOIR) {
                ligne_de_duo(ui, noms, a, b, parties, victoires);
            }
        }
        if !trente.is_empty() {
            if !ce_soir.is_empty() {
                ui.add_space(espace::S);
            }
            ui::section_label(ui, "sur 30 jours");
            for (a, b, parties, victoires) in trente {
                ligne_de_duo(ui, noms, a, b, u32::from(parties), u32::from(victoires));
            }
        }
    });
}

/// « drion & Jerem ······ 2 V – 1 D », les victoires à droite.
fn ligne_de_duo(ui: &mut Ui, noms: &Annuaire, a: UserId, b: UserId, parties: u32, victoires: u32) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = espace::XS;
        ui.label(RichText::new(noms.nom(a)).color(noms.couleur(a)).size(texte::COURANT));
        ui.label(RichText::new("&").color(TEXT_FAINT).size(texte::COURANT));
        ui.label(RichText::new(noms.nom(b)).color(noms.couleur(b)).size(texte::COURANT));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let defaites = parties - victoires;
            let teinte = match victoires.cmp(&defaites) {
                std::cmp::Ordering::Greater => SPEAK,
                std::cmp::Ordering::Less => DANGER,
                std::cmp::Ordering::Equal => TEXT,
            };
            ui.label(RichText::new(format!("{victoires} V – {defaites} D")).font(police::moyenne(ui.ctx(), texte::COURANT)).color(teinte));
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ki_protocol::JoueurSoiree;

    fn joueur(user_id: UserId, gagne: bool, delta: i32, perf: u16, mvp: u8) -> JoueurSoiree {
        JoueurSoiree { user_id, gagne: Some(gagne), delta_rr: Some(delta), perf, mvp, ..Default::default() }
    }

    /// Une partie à deux compte une fois ; les RR s'additionnent pour tous ;
    /// le MVP de la soirée est celui qui en a le plus.
    #[test]
    fn la_soiree_se_compte_partie_par_partie() {
        let parties = vec![
            PartieSoiree { duree_s: 2_000, joueurs: vec![joueur(1, true, 20, 400, 2), joueur(2, true, 18, 300, 0)], ..Default::default() },
            PartieSoiree { duree_s: 1_800, joueurs: vec![joueur(2, false, -15, 350, 1)], ..Default::default() },
            PartieSoiree { duree_s: 1_900, joueurs: vec![joueur(1, true, 22, 310, 0)], ..Default::default() },
        ];
        let s = soiree_de(&parties);
        assert_eq!((s.victoires, s.defaites, s.rr, s.parties, s.duree_s), (2, 1, 45, 3, 5_700));
        assert_eq!(duos_de_soiree(&parties), vec![(1, 2, 1, 1)]);
        assert_eq!(s.mvp, Some(1));
        assert_eq!(soiree_de(&[]), Soiree::default());
    }

    /// Le grand rendez-vous : les deux heures qui se suivent les plus
    /// jouées, à cheval sur minuit s'il le faut.
    #[test]
    fn le_grand_rendez_vous_passe_minuit() {
        let mut cases = vec![0u16; 168];
        cases[6 * 24 + 23] = 5; // dimanche 23 h
        cases[0] = 6; // lundi 0 h
        cases[2 * 24 + 21] = 7; // mercredi 21 h, seul
        assert_eq!(grand_rendez_vous(&cases), Some((6, 23)));
        assert_eq!(creneau(23), "23 h – 1 h");
        assert_eq!(grand_rendez_vous(&[0; 168]), None);
    }

    /// La soirée commence à 6 h du matin : à 1 h, c'est encore celle de la
    /// veille.
    #[test]
    fn la_soiree_commence_a_six_heures() {
        use chrono::TimeZone as _;
        let a = |j, h| chrono::Local.with_ymd_and_hms(2026, 10, j, h, 30, 0).single().unwrap();
        let six = |j| chrono::Local.with_ymd_and_hms(2026, 10, j, 6, 0, 0).single().unwrap().timestamp_millis() as u64;
        assert_eq!(debut_de_soiree(a(18, 22)), six(18));
        assert_eq!(debut_de_soiree(a(19, 1)), six(18), "à 1 h, la soirée de la veille");
        assert_eq!(debut_de_soiree(a(19, 7)), six(19));
    }
}
