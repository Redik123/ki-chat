//! Le profil d'un membre — V9.1 de PLAN-VALORANT-2.md. La fiche devient
//! une page qui prend la place du chat (allumée dans ⚙ → Bêta, d'abord),
//! au style de la maquette validée : des sections relevées, des tuiles de
//! chiffres, des cartes d'agents et de cartes, un tableau des matchs.
//!
//! Les nombres sont ceux de la fiche d'aujourd'hui (la [`Vue`] et ses
//! bilans) : seule la mise en page change. Module enfant de
//! `valo`, il en reprend les calculs et les petits dessins tels
//! quels.

use ki_ui::typo::{Texte, Ton};
use ki_ui::jetons::{largeur, police};
use ki_ui::tableau::{Colonne, Tableau};

use super::*;
use crate::icons::Icon;

/// Les vues du profil.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VueProfil {
    Ensemble,
    Matchs,
    Agents,
    Cartes,
    Medailles,
    Progression,
    Comparer,
}

impl VueProfil {
    pub(super) const TOUTES: [(VueProfil, &'static str); 7] = [
        (VueProfil::Ensemble, "Vue d'ensemble"),
        (VueProfil::Matchs, "Matchs"),
        (VueProfil::Agents, "Agents"),
        (VueProfil::Cartes, "Cartes"),
        (VueProfil::Medailles, "Médailles"),
        (VueProfil::Progression, "Progression"),
        (VueProfil::Comparer, "Comparer"),
    ];
}

/// En dessous, deux blocs côte à côte passent l'un sous l'autre.
const DEUX_COLONNES: f32 = 860.0;
const HAUTEUR_COURBE_PROFIL: f32 = 220.0;
/// Ce que la vue d'ensemble montre de chaque liste, avant « voir tout ».
const MATCHS_ENSEMBLE: usize = 8;
const AGENTS_ENSEMBLE: usize = 4;
const CARTES_ENSEMBLE: usize = 4;
/// Les matchs de la tendance : les dix derniers, contre les dix d'avant.
const TENDANCE: usize = 10;
/// Assez de matchs de part et d'autre pour qu'une tendance dise quelque
/// chose.
const TENDANCE_MIN: u16 = 3;
/// Le portrait d'une carte d'agent, la hauteur du bandeau d'une carte.
const PORTRAIT_AGENT: f32 = 40.0;
const BANDEAU_CARTE: f32 = 56.0;

impl PageValo {
    /// Le profil d'un membre, sur toute la place qu'on lui donne, seul :
    /// le banc d'essai le photographie ainsi. Rend `false` quand on le
    /// referme : le bouton retour, ou Échap. Ce qu'il demande se perd —
    /// la page au centre, elle, le rend ([`Self::profil_contenu`]).
    #[cfg(debug_assertions)]
    pub fn profil(&mut self, ui: &mut Ui, ouverte: &FicheOuverte, cx: &Contexte<'_>) -> bool {
        let mut ouvert = true;
        ui.horizontal(|ui| {
            if ui::Bouton::new("Retour au chat").icone(Icon::ChevronLeft).show(ui).clicked() {
                ouvert = false;
            }
            ui.add_space(espace::M);
            ui.label(
                RichText::new(format!("VALORANT — {}", ouverte.username))
                    .font(police::moyenne(ui.ctx(), texte::TITRE_PAGE))
                    .color(TEXT),
            );
        });
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) && !ui.ctx().egui_wants_keyboard_input() {
            ouvert = false;
        }
        ui.add_space(espace::M);
        self.profil_contenu(ui, ouverte, cx, &mut Vec::new());
        ouvert
    }

    /// Le profil sans sa barre : dans l'onglet Profil de la page au
    /// centre, comme seul au centre.
    pub(super) fn profil_contenu(&mut self, ui: &mut Ui, ouverte: &FicheOuverte, cx: &Contexte<'_>, demandes: &mut Vec<Demande>) {
        choix_du_membre(ui, Some((ouverte.user_id, &ouverte.username)), cx, demandes);
        match (ouverte.recue, &ouverte.fiche) {
            (false, _) => {
                ui.label(RichText::new("demande au serveur…").color(TEXT_DIM));
            }
            (true, None) => {
                ui.label(RichText::new("pas de compte Riot lié — ou pas encore de fiche.").color(TEXT_DIM));
            }
            (true, Some(fiche)) => self.profil_corps(ui, ouverte.user_id, &ouverte.username, fiche, cx, demandes),
        }
    }

    fn profil_corps(
        &mut self,
        ui: &mut Ui,
        user_id: UserId,
        username: &str,
        fiche: &FicheValorant,
        cx: &Contexte<'_>,
        demandes: &mut Vec<Demande>,
    ) {
        let vue = Vue::new(username, fiche, self.fiche_periode, self.fiche_classe, cx, maintenant_ms());
        egui::ScrollArea::vertical().id_salt("profil_valo").auto_shrink([false, false]).show(ui, |ui| {
            en_tete(ui, &vue);
            ui.add_space(espace::L);
            // Un compte tout neuf : un mot, plutôt que dix sections vides.
            if fiche.matchs.is_empty() && fiche.historique_rr.is_empty() {
                ui::section(ui, Icon::Info, "Pas encore de partie", None, |ui| {
                    ui::hint(
                        ui,
                        "Le serveur n'a encore lu aucun match de ce compte chez HenrikDev : la fiche se remplit au fil \
                         des parties, après chacune.",
                    );
                });
                return;
            }
            self.choix(ui);
            ui.add_space(espace::L);
            match self.vue_profil {
                VueProfil::Ensemble => {
                    chiffres(ui, &vue);
                    self.bloc_courbe(ui, &vue);
                    forme_et_heures(ui, &vue);
                    if agents(ui, &vue, Some(AGENTS_ENSEMBLE)) {
                        self.vue_profil = VueProfil::Agents;
                    }
                    if cartes(ui, &vue, Some(CARTES_ENSEMBLE)) {
                        self.vue_profil = VueProfil::Cartes;
                    }
                    medailles(ui, &vue);
                    if self.matchs(ui, &vue, Some(MATCHS_ENSEMBLE)) {
                        self.vue_profil = VueProfil::Matchs;
                    }
                    les_actes(ui, &vue);
                }
                VueProfil::Matchs => {
                    self.matchs(ui, &vue, None);
                }
                VueProfil::Agents => {
                    agents(ui, &vue, None);
                }
                VueProfil::Cartes => {
                    cartes(ui, &vue, None);
                }
                VueProfil::Medailles => medailles(ui, &vue),
                VueProfil::Progression => self.progression(ui, &vue, user_id, cx, demandes),
                VueProfil::Comparer => self.comparer(ui, &vue, user_id, cx, demandes),
            }
        });
    }

    /// Les vues en pastilles, la période en pastilles, le mode en
    /// segments.
    fn choix(&mut self, ui: &mut Ui) {
        ui::pastilles(ui, &mut self.vue_profil, &VueProfil::TOUTES);
        ui.add_space(espace::S);
        ui.horizontal_wrapped(|ui| {
            let periodes = PeriodeFiche::TOUTES.map(|p| (p, p.label()));
            if ui::pastilles(ui, &mut self.fiche_periode, &periodes) {
                self.fiche_plus = false;
            }
            ui.add_space(espace::L);
            ui.scope(|ui| {
                ui.set_max_width(220.0);
                ui::segmente(ui, &mut self.fiche_classe, &[(true, "Compétitif"), (false, "Tous")]);
            });
        });
    }

    /// La courbe de RR, haute.
    fn bloc_courbe(&mut self, ui: &mut Ui, vue: &Vue) {
        let (rr, _) = signe(vue.b.rr);
        let quoi = match vue.periode {
            PeriodeFiche::DixDerniers => "les 10 derniers classés".to_string(),
            p => format!("{} de classés", p.titre()),
        };
        let aide = if vue.p.is_empty() {
            quoi
        } else {
            format!("{quoi} · {rr} RR · survole un point pour retrouver le match")
        };
        ui::section(ui, Icon::ArrowUp, "Courbe de RR", Some(&aide), |ui| self.courbe_rr(ui, vue, HAUTEUR_COURBE_PROFIL));
    }

    /// Les derniers matchs en tableau ; un clic sur une ligne montre ses
    /// manches et son récit dessous. Rend vrai quand on demande à tout
    /// voir.
    fn matchs(&mut self, ui: &mut Ui, vue: &Vue, n: Option<usize>) -> bool {
        let montres: Vec<&MatchResume> = vue.e.iter().copied().take(n.unwrap_or(usize::MAX)).collect();
        let reste = vue.e.len() - montres.len();
        let mut tout_voir = false;
        let mut basculer = None;
        ui::section(
            ui,
            Icon::Play,
            "Derniers matchs",
            Some("clique une ligne pour voir les manches et le récit du match"),
            |ui| {
                if montres.is_empty() {
                    ui::hint(ui, "Aucun match sur cette période.");
                    return;
                }
                // Le mode n'a de sens que tous modes confondus : en
                // compétitif, toutes les lignes diraient la même chose.
                let mut colonnes = vec![(ColonneMatch::Carte, Colonne::new("Carte", 130.0).grandit())];
                if !vue.classe {
                    colonnes.push((ColonneMatch::Mode, Colonne::new("Mode", 92.0).priorite(2)));
                }
                colonnes.extend([
                    (ColonneMatch::Agent, Colonne::new("Agent", 104.0).priorite(2)),
                    (ColonneMatch::Score, Colonne::new("Score", 52.0)),
                    (ColonneMatch::Kda, Colonne::new("K / D / A", 80.0)),
                    (ColonneMatch::Acs, Colonne::new("ACS", 44.0).droite().priorite(3)),
                    (ColonneMatch::Adr, Colonne::new("ADR", 44.0).droite().priorite(4)),
                    (ColonneMatch::Kast, Colonne::new("KAST", 48.0).droite().priorite(4)),
                    (ColonneMatch::PremiersSangs, Colonne::new("FK / FD", 60.0).droite().priorite(5)),
                    (ColonneMatch::Note, Colonne::new("Note", 46.0).droite().priorite(3)),
                    (ColonneMatch::Medailles, Colonne::new("Médailles", 84.0).priorite(5)),
                    (ColonneMatch::Rr, Colonne::new("RR", 44.0).droite()),
                ]);
                let mut tableau = Tableau::new("profil_matchs").hauteur_ligne(40.0);
                for (quoi, colonne) in colonnes {
                    tableau = tableau.colonne(quoi, colonne);
                }
                // Le point survolé sur la courbe retrouve son match ici.
                let survole = self.survol.as_deref().and_then(|id| montres.iter().position(|m| m.id == id));
                let clic = tableau.surlignee(survole).show(ui, montres.len(), |ui, i, colonne| {
                    cellule_de_match(ui, montres[i], colonne, vue);
                });
                if let Some(i) = clic {
                    basculer = Some(montres[i].id.clone()).filter(|id| !id.is_empty());
                }
                if let Some(m) = montres.iter().find(|m| !m.id.is_empty() && self.deplie.as_deref() == Some(m.id.as_str())) {
                    ui.add_space(espace::S);
                    ui::surface(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let deroule = m.manches_detail.as_ref().map(|d| d.deroule.as_str()).unwrap_or("");
                        if !deroule.is_empty() {
                            graphes::cases_manches(ui, deroule);
                            ui.add_space(espace::XS);
                        }
                        ui.add(egui::Label::new(Texte::Aide.riche_ton(ui, Ton::Discret, recit_du_match(m, &vue.noms))).wrap());
                    });
                }
                if reste > 0 {
                    ui.add_space(espace::S);
                    tout_voir = ui::Bouton::new(&format!("Voir les {reste} autres")).icone(Icon::ChevronRight).show(ui).clicked();
                }
            },
        );
        if let Some(id) = basculer {
            self.deplie = if self.deplie.as_deref() == Some(id.as_str()) { None } else { Some(id) };
        }
        tout_voir
    }
}

// ---------------------------------------------------------------------
// Le choix du membre
// ---------------------------------------------------------------------

/// La largeur de la liste des membres, et la taille de ce qu'une ligne
/// montre.
const LISTE_MEMBRES: f32 = 240.0;
const AVATAR_LISTE: f32 = 20.0;
const RANG_LISTE: f32 = 18.0;

/// Les membres liés, dans l'ordre des pseudos sans regarder la casse :
/// l'ordre de la liste et des deux flèches.
fn membres_lies(stats: &[FicheMembre]) -> Vec<&FicheMembre> {
    let mut lies: Vec<&FicheMembre> = stats.iter().collect();
    lies.sort_by_cached_key(|f| f.username.to_lowercase());
    lies
}

/// Le voisin de `rang` parmi `n`, d'un pas en avant ou en arrière, en
/// bouclant.
fn voisin(rang: usize, n: usize, en_avant: bool) -> usize {
    if en_avant {
        (rang + 1) % n
    } else {
        (rang + n - 1) % n
    }
}

/// De qui on lit le profil, et de quoi en changer sans repasser par le
/// classement : la liste des membres liés, et deux flèches vers le
/// précédent et le suivant. Rien quand il n'y a personne d'autre à
/// choisir.
pub(super) fn choix_du_membre(ui: &mut Ui, ouvert: Option<(UserId, &str)>, cx: &Contexte<'_>, demandes: &mut Vec<Demande>) {
    let lies = membres_lies(cx.stats);
    let devant = |f: &FicheMembre| ouvert.is_some_and(|(id, _)| id == f.user_id);
    if lies.is_empty() || (lies.len() == 1 && devant(lies[0])) {
        return;
    }
    let rang = lies.iter().position(|f| devant(f));
    let mut choisi: Option<&FicheMembre> = None;
    ui.horizontal(|ui| {
        ui.label(Texte::Aide.riche_ton(ui, Ton::Discret, "Profil de"));
        let titre = ouvert.map_or("choisir un membre", |(_, nom)| nom);
        egui::ComboBox::from_id_salt("profil_membre").selected_text(titre).width(LISTE_MEMBRES).height(360.0).show_ui(ui, |ui| {
            for f in &lies {
                if ligne_de_membre(ui, f, cx, devant(f)).clicked() && !devant(f) {
                    choisi = Some(f);
                }
            }
        });
        if let Some(rang) = rang.filter(|_| lies.len() > 1) {
            if ui::Bouton::icone_seule(Icon::ChevronLeft).taille(ui::Taille::S).bulle("le membre d'avant").show(ui).clicked() {
                choisi = Some(lies[voisin(rang, lies.len(), false)]);
            }
            if ui::Bouton::icone_seule(Icon::ChevronRight).taille(ui::Taille::S).bulle("le membre d'après").show(ui).clicked() {
                choisi = Some(lies[voisin(rang, lies.len(), true)]);
            }
            ui::hint(ui, format!("{} sur {}", rang + 1, lies.len()));
        }
    });
    if let Some(f) = choisi {
        demandes.push(Demande::AutreFiche(f.user_id, f.username.clone()));
    }
    ui.add_space(espace::S);
}

/// Une ligne de la liste : l'avatar, le pseudo, et le rang à droite.
fn ligne_de_membre(ui: &mut Ui, f: &FicheMembre, cx: &Contexte<'_>, devant: bool) -> Response {
    let largeur = ui.available_width().max(LISTE_MEMBRES);
    let (rect, reponse) = ui.allocate_exact_size(Vec2::new(largeur, AVATAR_LISTE + 2.0 * espace::XS), Sense::click());
    if !ui.is_rect_visible(rect) {
        return reponse;
    }
    if devant || reponse.hovered() {
        let fond = if devant { theme::alpha(ACCENT, 36) } else { theme::BG_HOVER };
        ui.painter().rect_filled(rect, rayon::S, fond);
    }
    let mut dedans = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(espace::S, espace::XS))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    classement::avatar(&mut dedans, &f.username, cx.photos.get(&f.user_id).map(|(_, t)| t), AVATAR_LISTE);
    dedans.add(egui::Label::new(RichText::new(&f.username).color(if devant { TEXT } else { TEXT_DIM })).selectable(false).truncate());
    let tier = f.fiche.rang.tier;
    if tier >= 3 {
        dedans.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(icone) = cx.rangs.texture(tier) {
                ui.add(egui::Image::new(icone).fit_to_exact_size(Vec2::splat(RANG_LISTE)));
            }
        });
    }
    reponse.on_hover_text(if tier >= 3 { format!("{} · {} RR", nom_de_rang(tier), f.fiche.rang.rr) } else { "non classé".to_string() })
}

// ---------------------------------------------------------------------
// L'en-tête
// ---------------------------------------------------------------------

/// L'identité sur la carte de joueur, puis le rang en grand : la jauge
/// vers le palier suivant, ce qu'il reste à gagner, le pic, le MMR.
fn en_tete(ui: &mut Ui, vue: &Vue) {
    identite(ui, vue.fiche, vue.catalogue);
    if vue.fiche.maj > 0 {
        ui::hint(ui, format!("mise à jour {} · HenrikDev", crate::formats::il_y_a(vue.fiche.maj)));
    }
    ui.add_space(espace::S);
    let r = &vue.fiche.rang;
    ui::surface(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            if let Some(icone) = vue.rangs.texture(r.tier).filter(|_| r.tier >= 3) {
                ui.add(egui::Image::new(icone).fit_to_exact_size(Vec2::splat(64.0)));
                ui.add_space(espace::S);
            }
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    if r.tier >= 3 {
                        let couleur = graphes::couleur_de_rang(r.tier);
                        ui.label(RichText::new(nom_de_rang(r.tier)).font(police::moyenne(ui.ctx(), texte::TITRE_PAGE)).color(couleur));
                        ui.label(RichText::new(format!("· {} RR", r.rr)).color(TEXT_DIM).size(texte::TITRE));
                        if r.delta != 0 {
                            let (texte_, teinte) = signe(r.delta);
                            ui.label(RichText::new(texte_).font(police::moyenne(ui.ctx(), texte::CORPS)).color(teinte))
                                .on_hover_text("au dernier match classé");
                        }
                    } else {
                        ui.label(RichText::new("Non classé").font(police::moyenne(ui.ctx(), texte::TITRE_PAGE)).color(TEXT_FAINT));
                    }
                });
                if (3..24).contains(&r.tier) {
                    graphes::jauge(ui, f32::from(r.rr) / 100.0, None, 260.0, graphes::couleur_de_rang(r.tier));
                } else if r.tier >= 24 && r.classement > 0 {
                    ui::hint(ui, format!("#{} du classement", milliers(r.classement)));
                }
                let mut ligne: Vec<String> = Vec::new();
                let estimation = vue.fiche.mmr_estime();
                if let Some(e) = estimation.as_ref().filter(|e| e.gain_moyen > 0.0 && (3..24).contains(&r.tier)) {
                    let restant = f32::from(100u16.saturating_sub(r.rr));
                    let n = (restant / e.gain_moyen).ceil().max(1.0) as u32;
                    ligne.push(format!("≈ {} pour {}", pluriel(n, "victoire"), nom_de_rang(r.tier + 1)));
                }
                if let Some(pic) = vue.fiche.pic.as_ref().filter(|p| p.tier >= 3) {
                    let acte = if pic.saison.is_empty() { String::new() } else { format!(" ({})", pic.saison.to_uppercase()) };
                    ligne.push(format!("pic {} · {} RR{acte}", nom_de_rang(pic.tier), pic.rr));
                }
                if r.placements_restants > 0 {
                    ligne.push(format!("encore {}", pluriel(u32::from(r.placements_restants), "placement")));
                }
                if r.boucliers > 0 {
                    ligne.push(pluriel(u32::from(r.boucliers), "bouclier"));
                }
                if !ligne.is_empty() {
                    ui.label(Texte::Aide.riche_ton(ui, Ton::Discret, ligne.join(" · ")));
                }
                // L'acte en cours : son bilan, à part de la période choisie
                // plus bas.
                if let Some(s) = vue.fiche.saisons.last().filter(|s| s.parties > 0) {
                    ui.label(
                        Texte::Aide.riche_ton(ui, Ton::Discret, format!(
                            "acte {} : {} V sur {} · {} %",
                            s.saison.to_uppercase(),
                            s.victoires,
                            pluriel(u32::from(s.parties), "partie"),
                            u32::from(s.victoires) * 100 / u32::from(s.parties)
                        )),
                    );
                }
                if let Some(e) = estimation {
                    let (ou, teinte, phrase) = match e.position {
                        PositionMmr::AuDessus => ("au-dessus du rang", SPEAK, "tu gagnes plus que tu ne perds"),
                        PositionMmr::AuNiveau => ("au niveau du rang", TEXT_DIM, "tes gains et tes pertes se tiennent"),
                        PositionMmr::EnDessous => ("en dessous du rang", DANGER, "tu perds plus que tu ne gagnes"),
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = espace::XS;
                        ui.label(RichText::new(format!("MMR estimé : {ou}")).color(teinte).size(texte::PETIT))
                            .on_hover_text(POURQUOI_MMR);
                        ui.label(
                            Texte::Aide.riche(ui, format!(
                                "— {phrase} (+{} / −{} sur {})",
                                opt_entier(Some(e.gain_moyen)),
                                opt_entier(Some(e.perte_moyenne)),
                                pluriel(u32::from(e.points), "classé")
                            )),
                        )
                        .on_hover_text(POURQUOI_MMR);
                    });
                }
            });
        });
    });
}

// ---------------------------------------------------------------------
// Mes chiffres
// ---------------------------------------------------------------------

/// Une tuile à venir : ce qu'elle dit, prêt à prêter à [`ui::Kpi`].
struct Chiffre {
    intitule: &'static str,
    valeur: String,
    ecart: Option<ui::Ecart>,
    precision: String,
    jauge: Option<(f32, Option<f32>, Color32)>,
    /// Comment le chiffre se calcule, avec ses nombres : au survol.
    info: String,
}

/// Le taux de clutchs gagnés, en pourcentage.
fn taux_de_clutch(b: &Bilan) -> Option<f32> {
    (b.clutchs_tentes > 0).then(|| f32::from(b.clutchs) * 100.0 / f32::from(b.clutchs_tentes))
}

/// Les dix derniers matchs à manches de la sélection, et les dix d'avant
/// eux — pris dans toute la fiche, au même mode : de quoi dire si ça
/// monte.
fn tendance(vue: &Vue) -> (Bilan, Bilan) {
    let mut tous: Vec<&MatchResume> = vue
        .fiche
        .matchs
        .iter()
        .filter(|m| FicheValorant::a_des_manches(m) && (!vue.classe || m.mode == MODE_CLASSE))
        .collect();
    tous.sort_by_key(|m| std::cmp::Reverse(m.date));
    let recents: Vec<&&MatchResume> = vue.comptes().take(TENDANCE).collect();
    let (mut avant, mut maintenant) = (Bilan::default(), Bilan::default());
    for m in &recents {
        maintenant.ajouter(m);
    }
    if let Some(plus_ancien) = recents.last() {
        for m in tous.iter().filter(|m| m.date < plus_ancien.date).take(TENDANCE) {
            avant.ajouter(m);
        }
    }
    (maintenant, avant)
}

/// L'écart d'un chiffre entre les dix derniers matchs et les dix d'avant,
/// s'il y en a assez des deux côtés.
fn ecart_de(f: fn(&Bilan) -> Option<f32>, recents: &Bilan, avant: &Bilan, decimales: usize, hausse_bonne: bool) -> Option<ui::Ecart> {
    if recents.matchs < TENDANCE_MIN || avant.matchs < TENDANCE_MIN {
        return None;
    }
    let delta = f(recents)? - f(avant)?;
    if !delta.is_finite() {
        return None;
    }
    let pas = 10f32.powi(decimales as i32);
    let arrondi = (delta * pas).round() / pas;
    let texte_ = if arrondi == 0.0 {
        "= 0".to_string()
    } else {
        format!("{}{}", if arrondi > 0.0 { "+" } else { "−" }, dec(arrondi.abs(), decimales))
    };
    let e = ui::Ecart::new(arrondi, texte_);
    Some(if hausse_bonne { e } else { e.a_l_envers() })
}

/// Les huit tuiles : chacune avec sa tendance et la médiane du groupe.
fn chiffres(ui: &mut Ui, vue: &Vue) {
    let b = &vue.b;
    let (recents, avant) = tendance(vue);
    // La jauge va jusqu'au meilleur du groupe, et un quart au-delà : le
    // meilleur ne la remplit pas, et l'on voit encore où est la médiane.
    let mediane = |f: fn(&Bilan) -> Option<f32>, moi: Option<f32>, decimales: usize, unite: &str| {
        let moi = moi?;
        let (med, max) = vue.repere(f, moi)?;
        let echelle = max * 1.25;
        let couleur = if moi >= med { SPEAK } else { DANGER };
        Some(((moi / echelle, Some(med / echelle), couleur), format!("médiane {}{unite}", dec(med, decimales))))
    };
    // Pourquoi une valeur manque, quand elle manque : des dégâts à zéro ne
    // viennent que d'un match résumé avant 0.1.40, un détail absent d'un
    // match d'avant ou d'un match archivé. Sans ça, un tiret ne dit rien.
    let sur = |n: u16| format!("sur {}", pluriel(u32::from(n), "match"));
    let sous_degats = if b.matchs_degats == 0 {
        "matchs d'avant 0.1.40".to_string()
    } else if b.matchs_degats < b.matchs {
        sur(b.matchs_degats)
    } else {
        String::new()
    };
    let sous_detail = if b.matchs_detailles == 0 {
        "manches non lues".to_string()
    } else if b.matchs_detailles < b.matchs {
        sur(b.matchs_detailles)
    } else {
        String::new()
    };
    let sous_tirs = if b.tirs > 0 { format!("{} tirs", milliers(b.tirs)) } else { "tirs non comptés".to_string() };

    let mut tuiles: Vec<Chiffre> = Vec::new();
    let mut ajouter =
        |intitule, valeur: String, f: fn(&Bilan) -> Option<f32>, decimales, unite: &str, sinon: String, info: String| {
            let repere = mediane(f, f(b), decimales, unite);
            tuiles.push(Chiffre {
                intitule,
                valeur,
                ecart: ecart_de(f, &recents, &avant, decimales, true),
                precision: repere.as_ref().map(|(_, t)| t.clone()).unwrap_or(sinon),
                jauge: repere.map(|(j, _)| j),
                info,
            });
        };
    ajouter(
        "K/D",
        opt_dec(b.kd(), 2),
        Bilan::kd,
        2,
        "",
        format!("KDA {}", opt_dec(b.kda(), 2)),
        format!(
            "kills / morts = {} / {} ; KDA = (kills + assists) / morts = ({} + {}) / {}",
            b.kills, b.deaths, b.kills, b.assists, b.deaths
        ),
    );
    ajouter(
        "ACS",
        opt_entier(b.acs()),
        Bilan::acs,
        0,
        "",
        format!("sur {}", pluriel(u32::from(b.manches), "manche")),
        format!("score / manches = {} / {}", milliers(b.score), b.manches),
    );
    ajouter(
        "ADR",
        opt_entier(b.adr()),
        Bilan::adr,
        0,
        "",
        sous_degats,
        format!("dégâts / manches des matchs qui les ont = {} / {}", milliers(b.degats), b.manches_degats),
    );
    ajouter(
        "KAST",
        opt_pct(b.kast_pct()),
        Bilan::kast_pct,
        0,
        " %",
        sous_detail.clone(),
        format!("manches avec kill, assist, survie ou échange / manches lues = {} / {}", b.kast, b.manches_detaillees),
    );
    ajouter(
        "Tête",
        opt_pct(b.tete_pct()),
        Bilan::tete_pct,
        0,
        " %",
        sous_tirs,
        format!("tirs à la tête / tirs = {} / {}", milliers(b.tetes), milliers(b.tirs)),
    );
    let bilan = format!("{} V – {} D", b.victoires, b.defaites);
    ajouter(
        "Victoires",
        opt_pct(b.victoires_pct()),
        Bilan::victoires_pct,
        0,
        " %",
        bilan.clone(),
        format!(
            "victoires / (victoires + défaites) = {} / {} sur {} à manches",
            b.victoires,
            u32::from(b.victoires) + u32::from(b.defaites),
            pluriel(u32::from(b.matchs), "match")
        ),
    );
    let morts = format!("{} premières morts", b.premieres_morts);
    ajouter(
        "Premiers sangs",
        opt_dec(b.fk_par_match(), 1),
        Bilan::fk_par_match,
        1,
        "",
        if b.matchs_detailles > 0 { format!("par match · {morts}") } else { sous_detail.clone() },
        format!(
            "premiers sangs / matchs lus = {} / {} ; en face, {morts}",
            b.premiers_sangs, b.matchs_detailles
        ),
    );
    let clutchs = if b.clutchs_tentes > 0 { format!("{} / {}", b.clutchs, b.clutchs_tentes) } else { TIRET.to_string() };
    let meilleur = if b.meilleur_clutch > 0 { format!(" · meilleur 1 v {}", b.meilleur_clutch) } else { String::new() };
    ajouter(
        "Clutchs",
        clutchs,
        taux_de_clutch,
        0,
        " %",
        if b.matchs_detailles > 0 { format!("{} gagnés{meilleur}", opt_pct(taux_de_clutch(b))) } else { sous_detail },
        format!(
            "situations 1 contre X gagnées / tentées{meilleur} ; manches à 3, 4, 5 kills ou plus : {} triple(s), {} \
             quadruple(s), {} ace(s)",
            b.triples, b.quadruples, b.aces
        ),
    );
    // Avec une médiane, la précision ne dit plus que la médiane : ces trois
    // tuiles gardent devant ce qu'elles disaient sans elle.
    let devant = [
        ("Victoires", bilan),
        ("Premiers sangs", format!("{} FD", b.premieres_morts)),
        ("Clutchs", if b.triples + b.quadruples + b.aces > 0 { multi_kills(b.triples, b.quadruples, b.aces) } else { String::new() }),
    ];
    for (intitule, texte_) in devant {
        if let Some(t) = tuiles.iter_mut().find(|t| t.intitule == intitule && t.jauge.is_some()) {
            if !texte_.is_empty() {
                t.precision = format!("{texte_} · {}", t.precision);
            }
        }
    }

    let aide = format!(
        "{}, {} · la flèche compare les dix derniers matchs aux dix d'avant, le trait blanc est la médiane du groupe",
        vue.periode.titre(),
        if vue.classe { "compétitif" } else { "tous les modes" }
    );
    ui::section(ui, Icon::Target, "Mes chiffres", Some(&aide), |ui| {
        if b.matchs == 0 {
            ui::hint(ui, "Aucun match à manches sur cette période.");
            return;
        }
        ui::grille(ui, tuiles.len(), largeur::TUILE_MIN + 30.0, espace::L, |ui, i| {
            let t = &tuiles[i];
            let mut kpi = ui::Kpi::new(t.intitule, t.valeur.clone()).precision(&t.precision);
            if let Some(e) = &t.ecart {
                kpi = kpi.ecart(e.clone());
            }
            if let Some((part, repere, couleur)) = t.jauge {
                kpi = kpi.jauge(part, repere, couleur);
            }
            kpi.show(ui).on_hover_text(&t.info);
        });
    });
}

// ---------------------------------------------------------------------
// Forme et heures
// ---------------------------------------------------------------------

fn forme_et_heures(ui: &mut Ui, vue: &Vue) {
    if ui.available_width() >= DEUX_COLONNES {
        ui.columns(2, |colonnes| {
            bloc_forme(&mut colonnes[0], vue);
            bloc_heures(&mut colonnes[1], vue);
        });
    } else {
        bloc_forme(ui, vue);
        bloc_heures(ui, vue);
    }
}

/// Les vingt derniers classés en cases, le plus récent à droite ; la
/// série, le bilan, les gains et les pertes moyens.
fn bloc_forme(ui: &mut Ui, vue: &Vue) {
    ui::section(ui, Icon::Check, "Forme", Some("les 20 derniers classés, le plus récent à droite"), |ui| {
        let mut recents: Vec<&MatchResume> = classes_recents(vue.fiche).into_iter().take(20).collect();
        if recents.is_empty() {
            ui::hint(ui, "Aucun match classé connu.");
            return;
        }
        recents.reverse();
        let resultats: Vec<i8> = recents
            .iter()
            .map(|m| match m.gagne {
                Some(true) => 1,
                Some(false) => -1,
                None => 0,
            })
            .collect();
        let info = |i: usize| -> String {
            recents
                .get(i)
                .map(|m| format!("{} · {}-{} · {} · {}", m.carte, m.manches.0, m.manches.1, m.agent, crate::formats::il_y_a(m.date)))
                .unwrap_or_default()
        };
        graphes::bande_forme(ui, &resultats, Vec2::new(12.0, 18.0), Some(&info));
        ui.add_space(espace::M);
        let (v, d) = resultats.iter().fold((0u32, 0u32), |(v, d), &r| match r {
            1 => (v + 1, d),
            -1 => (v, d + 1),
            _ => (v, d),
        });
        let serie = vue.fiche.serie();
        let serie_texte = match serie {
            s if s > 0 => pluriel(s as u32, "victoire"),
            s if s < 0 => pluriel(u32::from(s.unsigned_abs()), "défaite"),
            _ => TIRET.to_string(),
        };
        let estimation = vue.fiche.mmr_estime();
        let lignes = [
            ("Série en cours", serie_texte, if serie > 0 { SPEAK } else if serie < 0 { DANGER } else { TEXT_DIM }),
            (&*format!("{} derniers", resultats.len()), format!("{v} V – {d} D"), TEXT),
            ("Gain moyen", estimation.as_ref().map_or(TIRET.to_string(), |e| format!("+{}", dec(e.gain_moyen, 1))), SPEAK),
            ("Perte moyenne", estimation.as_ref().map_or(TIRET.to_string(), |e| format!("−{}", dec(e.perte_moyenne, 1))), DANGER),
        ]
        .map(|(a, b, c)| (a.to_string(), b, c));
        ui.horizontal(|ui| {
            if v + d > 0 {
                let part = v as f32 / (v + d) as f32;
                let couleur = if part >= 0.5 { SPEAK } else { DANGER };
                graphes::anneau(ui, egui::Id::unique("profil_anneau"), part, 64.0, couleur, &format!("{:.0} %", part * 100.0))
                    .on_hover_text("la part des victoires sur ces classés");
                ui.add_space(espace::L);
            }
            egui::Grid::new("profil_forme").num_columns(2).spacing([espace::L, espace::XS]).show(ui, |ui| {
                for (libelle, valeur, couleur) in &lignes {
                    ui.label(Texte::Aide.riche(ui, libelle));
                    ui.label(RichText::new(valeur).font(police::moyenne(ui.ctx(), texte::CORPS)).color(*couleur));
                    ui.end_row();
                }
            });
        });
    });
}

/// Ses heures sur la sélection, le groupe en contour, l'heure qu'il est
/// encadrée, et son grand rendez-vous.
fn bloc_heures(ui: &mut Ui, vue: &Vue) {
    ui::section(
        ui,
        Icon::Sliders,
        "Mes heures",
        Some("mes parties de la sélection, à l'heure locale · en contour, le groupe sur 30 jours"),
        |ui| {
            if vue.e.len() < 5 {
                ui::hint(ui, "Pas encore assez de parties pour dessiner ses heures.");
                return;
            }
            let cases = cases_des_heures(vue.e.iter().map(|m| m.date));
            let decalage = chrono::Local::now().offset().local_minus_utc() / 3600;
            let groupe = activite_locale(vue.activite, decalage);
            let avec_groupe = groupe.iter().any(|&v| v > 0);
            if let Some((i, _)) = cases.iter().enumerate().filter(|(_, &n)| n > 0).max_by_key(|(_, &n)| n) {
                ui::hint(ui, format!("pic : {} {} h", JOURS_LONGS[i / 24], i % 24));
            }
            let plus = graphes::EnPlus {
                encadree: Some(graphes::case_de_maintenant()),
                contour: avec_groupe.then_some(groupe.as_slice()),
            };
            graphes::heatmap_semaine(ui, &cases, plus, |jour, heure| {
                let n = cases.get(jour * 24 + heure).copied().unwrap_or(0);
                let g = groupe.get(jour * 24 + heure).copied().unwrap_or(0);
                let du_groupe = if avec_groupe { format!(" · le groupe : {}", pluriel(u32::from(g), "partie")) } else { String::new() };
                format!(
                    "{} {heure}h–{}h · {}{du_groupe}",
                    JOURS_LONGS.get(jour).copied().unwrap_or("?"),
                    heure + 1,
                    pluriel(u32::from(n), "partie")
                )
            });
        },
    );
}

// ---------------------------------------------------------------------
// Agents, cartes, médailles, actes
// ---------------------------------------------------------------------

/// Le bilan en une barre, sous une carte.
fn barre_du_bilan(ui: &mut Ui, b: &Bilan) {
    let nuls = b.matchs.saturating_sub(b.victoires).saturating_sub(b.defaites);
    graphes::barre_vd(ui, u32::from(b.victoires), u32::from(b.defaites), u32::from(nuls), ui.available_width());
}

/// La part des victoires, grande et colorée, puis « de victoires ».
fn victoires_en_grand(ui: &mut Ui, b: &Bilan) {
    ui.horizontal(|ui| {
        let couleur = match b.victoires_pct() {
            Some(p) if p >= 50.0 => SPEAK,
            Some(_) => DANGER,
            None => TEXT_FAINT,
        };
        ui.label(RichText::new(opt_pct(b.victoires_pct())).font(police::moyenne(ui.ctx(), texte::GRAND)).color(couleur));
        ui::hint(ui, "de victoires");
    });
}

/// Les agents joués, en cartes. Rend vrai quand on demande à tout voir.
fn agents(ui: &mut Ui, vue: &Vue, n: Option<usize>) -> bool {
    let liste = vue.ventiler(|m| m.agent.as_str());
    let montres = n.map_or(liste.len(), |n| n.min(liste.len()));
    let mut tout_voir = false;
    ui::section(ui, Icon::User, "Agents", Some("les plus joués de la sélection"), |ui| {
        if liste.is_empty() {
            ui::hint(ui, "Aucun agent sur cette période.");
            return;
        }
        ui::grille(ui, montres, largeur::CARTE_MIN, espace::L, |ui, i| {
            let (nom, b) = &liste[i];
            ui::surface(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    match vue.catalogue.agent(nom) {
                        Some(portrait) => {
                            ui.add(egui::Image::new(portrait).fit_to_exact_size(Vec2::splat(PORTRAIT_AGENT)));
                        }
                        None => {
                            let (rect, _) = ui.allocate_exact_size(Vec2::splat(PORTRAIT_AGENT), Sense::hover());
                            ui.painter().rect_filled(rect, egui::CornerRadius::same(rayon::M), theme::BG_ACTIVE);
                        }
                    }
                    ui.vertical(|ui| {
                        ui.label(RichText::new(nom).font(police::moyenne(ui.ctx(), texte::TITRE)).color(TEXT));
                        ui::hint(ui, vue.catalogue.role(nom).unwrap_or(""));
                    });
                });
                ui.add_space(espace::S);
                ui.label(
                    Texte::Aide.riche_ton(ui, Ton::Discret, format!("{} · K/D {}", pluriel(u32::from(b.matchs), "partie"), opt_dec(b.kd(), 2))),
                );
                victoires_en_grand(ui, b);
                barre_du_bilan(ui, b);
            });
        });
        if montres < liste.len() {
            ui.add_space(espace::S);
            tout_voir = ui::Bouton::new(&format!("Voir les {} agents", liste.len())).icone(Icon::ChevronRight).show(ui).clicked();
        }
    });
    tout_voir
}

/// Les cartes jouées, chacune sur son bandeau. Rend vrai quand on demande
/// à tout voir.
fn cartes(ui: &mut Ui, vue: &Vue, n: Option<usize>) -> bool {
    let liste = vue.ventiler(|m| m.carte.as_str());
    let montres = n.map_or(liste.len(), |n| n.min(liste.len()));
    let mut tout_voir = false;
    ui::section(ui, Icon::Screen, "Cartes", Some("la part des victoires sur chaque carte"), |ui| {
        if liste.is_empty() {
            ui::hint(ui, "Aucune carte sur cette période.");
            return;
        }
        ui::grille(ui, montres, largeur::CARTE_MIN, espace::L, |ui, i| {
            let (nom, b) = &liste[i];
            ui::surface(ui, |ui| {
                ui.set_width(ui.available_width());
                let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), BANDEAU_CARTE), Sense::hover());
                let image = vue.catalogue.bandeau(nom);
                match image {
                    Some(bandeau) => {
                        egui::Image::new(bandeau)
                            .uv(graphes::uv_couvrant(bandeau.size_vec2(), rect.size()))
                            .corner_radius(rayon::M)
                            .paint_at(ui, rect);
                        if let Some(voile) = vue.catalogue.voile() {
                            egui::Image::new(voile).corner_radius(rayon::M).paint_at(ui, rect);
                        }
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
                    if image.is_some() { Color32::WHITE } else { TEXT },
                );
                ui.add_space(espace::S);
                ui.label(Texte::Aide.riche_ton(ui, Ton::Discret, pluriel(u32::from(b.matchs), "partie")));
                victoires_en_grand(ui, b);
                barre_du_bilan(ui, b);
            });
        });
        if montres < liste.len() {
            ui.add_space(espace::S);
            tout_voir = ui::Bouton::new(&format!("Voir les {} cartes", liste.len())).icone(Icon::ChevronRight).show(ui).clicked();
        }
    });
    tout_voir
}

/// Les douze médailles de l'acte en cours (de la carrière, faute
/// d'acte), même celles qu'on n'a pas encore, et la carrière dessous.
fn medailles(ui: &mut Ui, vue: &Vue) {
    let Some(md) = vue.fiche.medailles.as_ref() else {
        ui::section(ui, Icon::Star, "Médailles", Some("celles de l'écran de fin de partie"), |ui| {
            ui::hint(ui, "Pas encore de médailles lues pour ce compte.");
        });
        return;
    };
    let (liste, quand) = if !md.cet_acte.is_empty() {
        (&md.cet_acte, if md.acte.is_empty() { "l'acte en cours".to_string() } else { md.acte.clone() })
    } else {
        (&md.carriere, "la carrière".to_string())
    };
    let tuiles: Vec<(&'static str, String, String, Color32)> = Medaille::TOUTES
        .iter()
        .map(|&m| {
            let compte = liste.iter().find(|c| c.medaille == m && c.fois > 0);
            let couleur = match compte {
                None => TEXT_FAINT,
                Some(_) if matches!(m, Medaille::Mvp | Medaille::Distinction) => theme::WARN,
                Some(_) => TEXT,
            };
            let fois = compte.map_or(0, |c| c.fois);
            let record = compte.map_or_else(|| "pas encore".to_string(), |c| format!("record : {}", m.valeur(c.meilleur)));
            (m.nom(), format!("×{fois}"), record, couleur)
        })
        .collect();
    let aide = format!("{quand} · mises à jour {}", crate::formats::il_y_a(md.maj));
    ui::section(ui, Icon::Star, "Médailles", Some(&aide), |ui| {
        ui::grille(ui, tuiles.len(), largeur::TUILE_MIN, espace::L, |ui, i| {
            let (nom, fois, record, couleur) = &tuiles[i];
            ui::Kpi::new(nom, fois.clone()).couleur(*couleur).precision(record).show(ui);
        });
        if !md.cet_acte.is_empty() && md.carriere != md.cet_acte {
            let mut carriere: Vec<_> = md.carriere.iter().filter(|c| c.medaille != Medaille::Inconnue && c.fois > 0).collect();
            carriere.sort_by_key(|c| c.medaille.ordre());
            if !carriere.is_empty() {
                ui.add_space(espace::S);
                let texte_ = carriere.iter().map(|c| format!("{} ×{}", c.medaille.nom(), c.fois)).collect::<Vec<_>>().join(" · ");
                ui::hint(ui, format!("en carrière : {texte_}"));
            }
        }
    });
}

/// Le rang de fin de chaque acte, en barres.
fn les_actes(ui: &mut Ui, vue: &Vue) {
    let saisons = &vue.fiche.saisons;
    if saisons.is_empty() {
        return;
    }
    ui::section(
        ui,
        Icon::Crown,
        "Les actes",
        Some("le rang en fin d'acte, du plus ancien au plus récent ; survole une barre pour ses victoires"),
        |ui| {
            // Jusqu'au meilleur acte et deux paliers au-dessus : les marches
            // d'un acte à l'autre se voient.
            let barres = barres_d_actes(saisons);
            let haut = barres.iter().map(|b| b.valeur).fold(0.0, f32::max) + 2.0;
            graphes::barres_verticales(ui, egui::Id::unique("profil_actes"), &barres, 110.0, Some(haut.min(26.0)));
        },
    );
}

// ---------------------------------------------------------------------
// Une case du tableau des matchs
// ---------------------------------------------------------------------

/// Les colonnes de la table des matchs. Par leur nom, pas par leur rang :
/// la colonne Mode va et vient avec le filtre, et un numéro aurait glissé.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ColonneMatch {
    Carte,
    Mode,
    Agent,
    Score,
    Kda,
    Acs,
    Adr,
    Kast,
    PremiersSangs,
    Note,
    Medailles,
    Rr,
}

fn cellule_de_match(ui: &mut Ui, m: &MatchResume, colonne: ColonneMatch, vue: &Vue) {
    match colonne {
        ColonneMatch::Carte => {
            ui.vertical(|ui| {
                ui.add_space(espace::XXS);
                carte_cellule(ui, &m.carte, vue.catalogue);
                ui.label(Texte::Minuscule.riche(ui, crate::formats::il_y_a(m.date)));
            });
        }
        ColonneMatch::Mode => {
            cellule(ui, &m.mode, TEXT_DIM);
        }
        ColonneMatch::Agent => {
            agent_cellule(ui, &m.agent, vue.catalogue);
        }
        ColonneMatch::Score => {
            ui.label(
                RichText::new(format!("{}-{}", m.manches.0, m.manches.1))
                    .font(police::moyenne(ui.ctx(), texte::COURANT))
                    .color(teinte_de_score(m.gagne)),
            );
        }
        ColonneMatch::Kda => {
            cellule(ui, format!("{} / {} / {}", m.kills, m.deaths, m.assists), TEXT)
                .on_hover_text(format!("éliminations / morts / assistances — {} points", milliers(m.score)));
        }
        ColonneMatch::Acs => {
            cellule(ui, opt_entier(acs_de(m)), TEXT_DIM);
        }
        ColonneMatch::Adr => {
            cellule(ui, opt_entier(adr_de(m)), TEXT_DIM);
        }
        ColonneMatch::Kast => {
            cellule(ui, opt_pct(kast_de(m)), TEXT_DIM);
        }
        ColonneMatch::PremiersSangs => {
            let fk = m
                .manches_detail
                .as_ref()
                .map_or_else(|| TIRET.to_string(), |d| format!("{} / {}", d.premiers_sangs, d.premieres_morts));
            cellule(ui, fk, TEXT_DIM).on_hover_text("premiers sangs / premières morts");
        }
        ColonneMatch::Note => {
            let (note, couleur) = if m.perf > 0 { (m.perf.to_string(), TEXT_DIM) } else { (TIRET.to_string(), TEXT_FAINT) };
            let mvp = match m.mvp {
                ki_protocol::MVP_DU_MATCH => Some("MVP du match"),
                ki_protocol::MVP_D_EQUIPE => Some("MVP de son équipe"),
                _ => None,
            };
            let reponse = cellule(ui, note, if mvp.is_some() { theme::WARN } else { couleur });
            if let Some(mvp) = mvp {
                reponse.on_hover_text(mvp);
            }
        }
        ColonneMatch::Medailles => {
            medailles_cellule(ui, vue.medailles_du(&m.id));
        }
        ColonneMatch::Rr => match vue.point_de(&m.id) {
            Some(p) => {
                let (t, c) = signe(p.delta);
                let reponse = ui.label(RichText::new(t).font(police::moyenne(ui.ctx(), texte::COURANT)).color(c));
                // Ce que ce delta a de particulier, quand il en a.
                let mut details = Vec::new();
                if p.bonus != 0 {
                    details.push(format!("dont {} de bonus de performance", signe(p.bonus).0));
                }
                if p.placement {
                    details.push("match de placement".to_string());
                }
                if p.protege {
                    details.push("descente protégée".to_string());
                }
                if !details.is_empty() {
                    reponse.on_hover_text(details.join(" · "));
                }
            }
            None => {
                cellule(ui, TIRET, TEXT_FAINT);
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Les flèches du choix du membre bouclent, dans les deux sens.
    #[test]
    fn les_fleches_du_profil_bouclent() {
        assert_eq!(voisin(0, 3, true), 1);
        assert_eq!(voisin(2, 3, true), 0);
        assert_eq!(voisin(0, 3, false), 2);
        assert_eq!(voisin(1, 3, false), 0);
        assert_eq!(voisin(0, 1, true), 0);
        assert_eq!(voisin(0, 1, false), 0);
    }

    /// La liste suit les pseudos sans regarder la casse.
    #[test]
    fn les_membres_se_rangent_par_pseudo() {
        let membre = |user_id, nom: &str| FicheMembre { user_id, username: nom.to_string(), ..Default::default() };
        let stats = [membre(1, "zed"), membre(2, "Amnesyak"), membre(3, "mg4230"), membre(4, "BS")];
        let noms: Vec<&str> = membres_lies(&stats).iter().map(|f| f.username.as_str()).collect();
        assert_eq!(noms, ["Amnesyak", "BS", "mg4230", "zed"]);
    }
}
