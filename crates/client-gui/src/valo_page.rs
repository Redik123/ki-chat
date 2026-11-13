//! La page VALORANT : le groupe et la fiche de chacun, à la place du chat,
//! sur ce que le serveur sait des fiches HenrikDev — sans jamais solliciter
//! HenrikDev elle-même : l'ouvrir ne coûte aucune requête à personne.
//!
//! La page est dessinée par les modules d'à côté ([`centre`] et ses
//! onglets : [`accueil`], [`classement`], [`matchs`], [`profil`]…) ; ce
//! fichier porte ce qu'ils partagent — l'état, le contexte, les vues, les
//! formateurs, les cellules. Les deux fenêtres flottantes d'avant, que la
//! page a remplacées, sont parties avec la 0.2.0.
//!
//! Sur le patron du soundboard et de la visionneuse, le module tient son
//! état (onglet, période, filtres, ce qui est déplié) et rend des
//! [`Demande`] à l'application, qui garde la connexion : rien ici ne
//! touche à `KiApp`. Les nombres viennent de `ki-protocol` (`Bilan`,
//! `FicheValorant::bilan`, `forme`, `serie`…) — une seule implémentation
//! des formules, la même que le serveur — et les dessins de
//! [`crate::graphes`].
//!
//! Deux règles tiennent toute la page : jamais de division par zéro (tout
//! ratio passe par les `Option` de `Bilan` et s'affiche « — » à `None`),
//! et jamais de page vide — un serveur d'avant 0.1.40, qui n'envoie ni
//! bilan ni activité, donne une page calculée ici sur les cinq matchs et
//! dix points qu'il envoie, avec un mot pour le dire.

use ki_ui::typo::{Texte, Ton};
use ki_ui::jetons::{espace, rayon, texte};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{Datelike, TimeZone, Timelike};
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Response, RichText, Sense, Ui, Vec2};
use ki_protocol::{
    nom_de_rang, Bilan, BilanMembre, FicheMembre, FicheValorant, JeuEtat, JeuStatut, MatchEsport,
    MatchResume, Medaille, MedailleGagnee, Medailles, MedaillesDuMatch, Member, PartieSoiree, PointRR,
    PositionMmr, UserId,
};

use crate::graphes::{self, PointCourbe};
use crate::icons;
use crate::theme::{self, ACCENT, DANGER, SPEAK, TEXT, TEXT_DIM, TEXT_FAINT};
use crate::ui;
use crate::valo_catalogue::Catalogue;
use crate::{boutique, rangs, FicheOuverte};

mod accueil;
#[cfg(debug_assertions)]
pub(crate) mod banc;
mod centre;
mod classement;
mod comparer;
mod esport;
mod matchs;
mod medailles;
mod profil;
mod progression;

pub use progression::{decalage_journal, Journal};

// ---------------------------------------------------------------------
// Constantes
// ---------------------------------------------------------------------

const JOUR_MS: u64 = 86_400_000;
/// Autant de classés pour concourir à un record ou peser dans une médiane.
const ASSEZ: u16 = 5;
/// Le nom français du mode classé, tel que le serveur le traduit.
const MODE_CLASSE: &str = "Compétitif";
/// La largeur d'une tuile de record (page du groupe) et d'une tuile de
/// la fiche ; les marges intérieures des cadres sont dedans.
const TUILE_GROUPE: Vec2 = Vec2::new(150.0, 80.0);
const MARGE_TUILE: f32 = 12.0;
/// Le portrait d'un agent dans une ligne de table.
const PORTRAIT: f32 = 18.0;
/// La pastille d'une carte dans une ligne de table : son nom sur son
/// bandeau.
const HAUTEUR_PASTILLE: f32 = 18.0;
const LARGEUR_PASTILLE_MIN: f32 = 64.0;
/// Le bandeau de la carte de joueur en tête de fiche.
const HAUTEUR_BANDEAU_FICHE: f32 = 76.0;
const JOURS_LONGS: [&str; 7] = ["lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche"];
const TIRET: &str = "—";

// ---------------------------------------------------------------------
// Les choix de la page
// ---------------------------------------------------------------------

/// La fenêtre des agrégats de la page du groupe : sept ou trente jours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Periode {
    Sept,
    Trente,
}

impl Periode {
    pub const TOUTES: [Periode; 2] = [Periode::Sept, Periode::Trente];

    pub fn label(self) -> &'static str {
        match self {
            Periode::Sept => "7 j",
            Periode::Trente => "30 j",
        }
    }

    /// « 7 jours » — pour les phrases.
    pub fn titre(self) -> &'static str {
        match self {
            Periode::Sept => "7 jours",
            Periode::Trente => "30 jours",
        }
    }

    pub fn jours(self) -> u64 {
        match self {
            Periode::Sept => 7,
            Periode::Trente => 30,
        }
    }

    pub fn cle(self) -> &'static str {
        match self {
            Periode::Sept => "7",
            Periode::Trente => "30",
        }
    }

    pub fn depuis(cle: &str) -> Self {
        Self::TOUTES.into_iter().find(|p| p.cle() == cle).unwrap_or(Periode::Trente)
    }

    /// Le bilan d'un membre sur cette période.
    fn bilan(self, b: &BilanMembre) -> &Bilan {
        match self {
            Periode::Sept => &b.sept_jours,
            Periode::Trente => &b.trente_jours,
        }
    }
}

/// La fenêtre de la fiche d'un membre.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodeFiche {
    DixDerniers,
    SeptJours,
    TrenteJours,
    Tout,
}

impl PeriodeFiche {
    pub const TOUTES: [PeriodeFiche; 4] =
        [PeriodeFiche::DixDerniers, PeriodeFiche::SeptJours, PeriodeFiche::TrenteJours, PeriodeFiche::Tout];

    pub fn label(self) -> &'static str {
        match self {
            PeriodeFiche::DixDerniers => "10 derniers",
            PeriodeFiche::SeptJours => "7 j",
            PeriodeFiche::TrenteJours => "30 j",
            PeriodeFiche::Tout => "tout",
        }
    }

    /// « 30 jours » — pour le titre de la courbe.
    pub fn titre(self) -> &'static str {
        match self {
            PeriodeFiche::DixDerniers => "10 derniers",
            PeriodeFiche::SeptJours => "7 jours",
            PeriodeFiche::TrenteJours => "30 jours",
            PeriodeFiche::Tout => "tout",
        }
    }

    pub fn cle(self) -> &'static str {
        match self {
            PeriodeFiche::DixDerniers => "10",
            PeriodeFiche::SeptJours => "7",
            PeriodeFiche::TrenteJours => "30",
            PeriodeFiche::Tout => "tout",
        }
    }

    pub fn depuis(cle: &str) -> Self {
        Self::TOUTES.into_iter().find(|p| p.cle() == cle).unwrap_or(PeriodeFiche::TrenteJours)
    }

    /// La borne basse de la fenêtre (ms Unix) et, pour « 10 derniers »,
    /// combien on garde.
    fn fenetre(self, maintenant: u64) -> (u64, Option<usize>) {
        match self {
            PeriodeFiche::DixDerniers => (0, Some(10)),
            PeriodeFiche::SeptJours => (maintenant.saturating_sub(7 * JOUR_MS), None),
            PeriodeFiche::TrenteJours => (maintenant.saturating_sub(30 * JOUR_MS), None),
            PeriodeFiche::Tout => (0, None),
        }
    }
}

/// En attendant les fiches du groupe : la roue tant que la réponse peut
/// venir, et, passé le délai, ce qui s'est passé avec de quoi réessayer.
pub(crate) fn attente_des_fiches(ui: &mut Ui, cx: &Contexte<'_>, demandes: &mut Vec<Demande>) {
    if cx.chargement == crate::valo_donnees::Chargement::EnRetard {
        ui::bandeau(ui, ui::Ton::Alerte, "le serveur n'a pas rendu les fiches du groupe");
        ui.add_space(espace::S);
        if ui::Bouton::new("Réessayer").icone(crate::icons::Icon::Refresh).principal().show(ui).clicked() {
            demandes.push(Demande::Actualiser);
        }
    } else {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new("demande au serveur…").color(TEXT_DIM));
        });
    }
}

/// Ce que la page demande à l'application, qui tient la connexion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Demande {
    Actualiser,
    OuvrirFiche(UserId, String),
    /// La carte tactique, depuis son onglet de la page au centre.
    OuvrirTactique,
    /// Le profil d'un autre membre à la place de celui qu'on regarde, sur
    /// la même vue : ses agents après ceux du premier.
    AutreFiche(UserId, String),
    /// Comparer la fiche ouverte à celle de ce membre (la vue Comparer du
    /// profil) : la demander au serveur.
    Comparer(UserId, String),
    /// Mon journal des parties, résumé jour par jour, pour la vue
    /// Progression de mon profil.
    MonJournal,
}

// ---------------------------------------------------------------------
// L'état
// ---------------------------------------------------------------------

/// L'état de la page : ce qui se mémorise d'une session à l'autre
/// (périodes, mode) et ce qui ne dure que le temps qu'elle est ouverte
/// (filtres, ligne dépliée, point survolé).
pub struct PageValo {
    pub ouvert: bool,
    periode: Periode,
    /// Le fil des matchs : un mode, un membre — ou tout.
    filtre_mode: Option<String>,
    filtre_membre: Option<UserId>,
    // La fiche.
    fiche_periode: PeriodeFiche,
    fiche_classe: bool,
    /// Le match déplié dans la table, par son id.
    deplie: Option<String>,
    /// Le point de la courbe sous la souris, par son match — la ligne de
    /// la table se surligne.
    survol: Option<String>,
    /// « voir les N autres » a été cliqué : toute la table.
    fiche_plus: bool,
    /// La vue du profil, quand la fiche prend la place du chat.
    vue_profil: profil::VueProfil,
    /// L'onglet de la page au centre.
    onglet_centre: centre::OngletCentre,
    /// L'onglet d'où une fiche a été ouverte : Échap y ramène.
    retour: Option<centre::OngletCentre>,
    /// Le tri du classement, tel qu'on l'a laissé.
    tri_classement: classement::TriChoisi,
    /// La carte de chaleur de l'accueil : le groupe, moi, ou les deux.
    heures: accueil::Heures,
    /// Les cartes de l'onglet Matchs montrées : « voir plus » en ajoute.
    cartes_matchs: usize,
    /// Les membres cochés dans le classement pour les comparer : deux au
    /// plus, le dernier coché remplaçant le plus ancien.
    a_comparer: Vec<UserId>,
}

impl PageValo {
    /// Clés mémorisées : `valo_periode`, `fiche_periode`, `fiche_classe`,
    /// `valo_onglet`, `valo_tri`.
    pub fn load(get: impl Fn(&str, &str) -> String) -> Self {
        Self {
            ouvert: false,
            periode: Periode::depuis(&get("valo_periode", "30")),
            filtre_mode: None,
            filtre_membre: None,
            fiche_periode: PeriodeFiche::depuis(&get("fiche_periode", "30")),
            fiche_classe: get("fiche_classe", "on") != "off",
            deplie: None,
            survol: None,
            fiche_plus: false,
            vue_profil: profil::VueProfil::Ensemble,
            onglet_centre: centre::OngletCentre::depuis(&get("valo_onglet", "accueil")),
            retour: None,
            tri_classement: classement::lire_tri(&get("valo_tri", "")),
            heures: accueil::Heures::Groupe,
            cartes_matchs: matchs::CARTES_PAR_PAGE,
            a_comparer: Vec::new(),
        }
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        storage.set_string("valo_periode", self.periode.cle().into());
        storage.set_string("fiche_periode", self.fiche_periode.cle().into());
        storage.set_string("fiche_classe", if self.fiche_classe { "on" } else { "off" }.into());
        storage.set_string("valo_onglet", self.onglet_a_garder().cle().into());
        storage.set_string("valo_tri", classement::ecrire_tri(self.tri_classement));
    }

    /// La vue Comparer du profil, l'onglet Profil devant.
    pub fn vers_comparer(&mut self) {
        self.vue_profil = profil::VueProfil::Comparer;
        self.vers_profil();
    }

    /// La fiche se ferme : rien de déplié, rien de survolé, table repliée.
    pub fn fermer_fiche(&mut self) {
        self.replier_fiche();
        self.vue_profil = profil::VueProfil::Ensemble;
    }

    /// Un autre membre sur la même vue : ce qui était déplié ou déroulé
    /// s'oublie — un match joué ensemble porte le même id chez les deux —,
    /// la vue reste.
    pub fn replier_fiche(&mut self) {
        self.deplie = None;
        self.survol = None;
        self.fiche_plus = false;
    }
}

// ---------------------------------------------------------------------
// Ce que la page lit
// ---------------------------------------------------------------------

/// Un membre de la page du groupe avec son bilan : celui du serveur, ou
/// celui qu'on refait ici sur ce qu'il a envoyé quand il vient d'avant
/// 0.1.40.
struct Ligne<'a> {
    f: &'a FicheMembre,
    bilan: Cow<'a, BilanMembre>,
}

impl Ligne<'_> {
    /// Le bilan de la période choisie.
    fn b(&self, periode: Periode) -> &Bilan {
        periode.bilan(&self.bilan)
    }

    /// Aucun classé sur la période : la ligne s'estompe et descend.
    fn sans_match(&self, periode: Periode) -> bool {
        self.b(periode).matchs == 0
    }
}

/// Les lignes du groupe, et si les bilans ont dû être refaits ici (un
/// serveur d'avant, qui n'en envoie pas).
fn lignes_du_groupe(stats: &[FicheMembre], maintenant: u64) -> (Vec<Ligne<'_>>, bool) {
    let local = !stats.is_empty() && stats.iter().all(|f| f.bilan.is_none());
    let lignes = stats
        .iter()
        .map(|f| Ligne {
            f,
            bilan: match &f.bilan {
                Some(b) => Cow::Borrowed(b),
                None => Cow::Owned(bilan_local(&f.fiche, maintenant)),
            },
        })
        .collect();
    (lignes, local)
}

/// Le bilan qu'un serveur 0.1.40 aurait envoyé, refait sur la fiche
/// reçue — cinq matchs et dix points d'un serveur d'avant, mais les
/// mêmes formules.
fn bilan_local(f: &FicheValorant, maintenant: u64) -> BilanMembre {
    let sept = maintenant.saturating_sub(7 * JOUR_MS);
    let trente = maintenant.saturating_sub(30 * JOUR_MS);
    let compte = |(nom, b): (String, Bilan)| (nom, b.matchs, b.victoires);
    BilanMembre {
        sept_jours: f.bilan(sept, u64::MAX, true),
        trente_jours: f.bilan(trente, u64::MAX, true),
        serie: f.serie(),
        forme: f.forme(10),
        agents: f.par_agent(trente, u64::MAX, true).into_iter().take(3).map(compte).collect(),
        cartes: f.par_carte(trente, u64::MAX, true).into_iter().take(5).map(compte).collect(),
        duos: f.duos(trente, u64::MAX).into_iter().take(5).collect(),
    }
}

/// Qui est qui : le pseudo et la couleur d'un membre, d'après les fiches
/// reçues puis le roster ; un inconnu se nomme par son numéro.
struct Annuaire<'a> {
    stats: &'a [FicheMembre],
    membres: HashMap<UserId, &'a Member>,
}

impl<'a> Annuaire<'a> {
    fn new(stats: &'a [FicheMembre], membres: &'a [Member]) -> Self {
        Self { stats, membres: membres.iter().map(|m| (m.user_id, m)).collect() }
    }

    fn nom(&self, id: UserId) -> String {
        if let Some(m) = self.membres.get(&id) {
            return m.username.clone();
        }
        self.stats
            .iter()
            .find(|f| f.user_id == id)
            .map(|f| f.username.clone())
            .unwrap_or_else(|| format!("#{id}"))
    }

    fn couleur(&self, id: UserId) -> Color32 {
        if let Some(m) = self.membres.get(&id) {
            return theme::member_color(m.color, &m.username);
        }
        self.stats
            .iter()
            .find(|f| f.user_id == id)
            .map(|f| theme::member_color(None, &f.username))
            .unwrap_or(TEXT_DIM)
    }

    /// « Nono, Wam » — plusieurs membres d'une virgule.
    fn noms(&self, ids: &[UserId]) -> String {
        ids.iter().map(|id| self.nom(*id)).collect::<Vec<_>>().join(", ")
    }
}

/// Le début de la soirée qu'on vit : le dernier 6 h du matin à l'heure
/// locale — une partie à 1 h du matin est encore de la veille.
pub fn debut_de_soiree(maintenant: chrono::DateTime<chrono::Local>) -> u64 {
    let six = |jour: chrono::NaiveDate| {
        jour.and_hms_opt(6, 0, 0)
            .and_then(|t| t.and_local_timezone(chrono::Local).earliest())
            .map_or(0, |t| t.timestamp_millis().max(0) as u64)
    };
    let jour = maintenant.date_naive();
    let ce_matin = six(jour);
    let ms = maintenant.timestamp_millis().max(0) as u64;
    if ms >= ce_matin {
        ce_matin
    } else {
        jour.pred_opt().map_or(ce_matin.saturating_sub(JOUR_MS), six)
    }
}

fn maintenant_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------
// Des nombres en mots
// ---------------------------------------------------------------------

/// « 1,24 » — une décimale à la française.
fn dec(v: f32, decimales: usize) -> String {
    format!("{v:.decimales$}").replace('.', ",")
}

fn opt_dec(v: Option<f32>, decimales: usize) -> String {
    v.filter(|v| v.is_finite()).map(|v| dec(v, decimales)).unwrap_or_else(|| TIRET.to_string())
}

/// « 245 » — un entier arrondi, ou le tiret.
fn opt_entier(v: Option<f32>) -> String {
    v.filter(|v| v.is_finite()).map(|v| format!("{}", v.round() as i64)).unwrap_or_else(|| TIRET.to_string())
}

/// « 61 % » — ou le tiret.
fn opt_pct(v: Option<f32>) -> String {
    v.filter(|v| v.is_finite()).map(|v| format!("{} %", v.round() as i64)).unwrap_or_else(|| TIRET.to_string())
}

/// « +18 » vert, « -12 » rouge, « ±0 » gris.
fn signe(v: i32) -> (String, Color32) {
    if v > 0 {
        (format!("+{v}"), SPEAK)
    } else if v < 0 {
        (v.to_string(), DANGER)
    } else {
        ("±0".to_string(), TEXT_DIM)
    }
}

/// « 4 212 » — des milliers espacés.
fn milliers(n: u32) -> String {
    let brut = n.to_string();
    let mut sortie = String::with_capacity(brut.len() + brut.len() / 3);
    for (i, c) in brut.chars().enumerate() {
        if i > 0 && (brut.len() - i).is_multiple_of(3) {
            sortie.push(' ');
        }
        sortie.push(c);
    }
    sortie
}

/// « 61 h », « 45 min » — une durée jouée.
fn duree_texte(secondes: u32) -> String {
    if secondes >= 3600 {
        format!("{} h", secondes / 3600)
    } else {
        format!("{} min", secondes / 60)
    }
}

/// « 3 parties », « 1 partie ».
fn pluriel(n: u32, mot: &str) -> String {
    if n > 1 {
        format!("{n} {mot}s")
    } else {
        format!("{n} {mot}")
    }
}

/// « 14 V · 8 D · 61 % » — ou le tiret sans match.
fn bilan_texte(b: &Bilan) -> String {
    if b.matchs == 0 {
        return TIRET.to_string();
    }
    format!("{} V · {} D · {}", b.victoires, b.defaites, opt_pct(b.victoires_pct()))
}

/// Les RR d'une fenêtre : le tiret si aucun point n'y tombe, « ±0 » si
/// des points s'y annulent. Les points reçus sont les plus récents : s'ils
/// sont tous plus vieux que la fenêtre, aucun n'y était.
fn rr_texte(rr: i32, points: &[PointRR], depuis: u64) -> (String, Color32) {
    if rr == 0 && !points.iter().any(|p| p.date >= depuis) {
        (TIRET.to_string(), TEXT_FAINT)
    } else {
        signe(rr)
    }
}

/// La couleur d'un score : victoire, défaite, nul.
fn teinte_de_score(gagne: Option<bool>) -> Color32 {
    match gagne {
        Some(true) => SPEAK,
        Some(false) => DANGER,
        None => TEXT_DIM,
    }
}

/// Le score d'un match par manche — « — » quand il n'en a pas.
fn acs_de(m: &MatchResume) -> Option<f32> {
    let manches = u32::from(m.manches.0) + u32::from(m.manches.1);
    (manches > 0).then(|| m.score as f32 / manches as f32)
}

fn adr_de(m: &MatchResume) -> Option<f32> {
    let manches = u32::from(m.manches.0) + u32::from(m.manches.1);
    (manches > 0 && m.degats > 0).then(|| m.degats as f32 / manches as f32)
}

fn kast_de(m: &MatchResume) -> Option<f32> {
    let d = m.manches_detail.as_ref()?;
    (d.manches > 0).then(|| f32::from(d.kast) * 100.0 / f32::from(d.manches))
}

// ---------------------------------------------------------------------
// Des morceaux d'interface
// ---------------------------------------------------------------------

/// Une tuile : un titre en petit, une valeur en grand à sa teinte, une
/// ligne du bas (qui, ou sur combien), une icône devant la valeur s'il y
/// en a une, et ce que dit le survol.
struct Tuile<'a> {
    titre: &'a str,
    valeur: String,
    teinte: Color32,
    sous: String,
    icone: Option<&'a egui::TextureHandle>,
    info: String,
}

impl<'a> Tuile<'a> {
    fn new(titre: &'a str, valeur: impl Into<String>, teinte: Color32) -> Self {
        Self { titre, valeur: valeur.into(), teinte, sous: String::new(), icone: None, info: String::new() }
    }

    /// Une tuile sans candidat : le tiret, et pourquoi.
    fn vide(titre: &'a str, pourquoi: &str) -> Self {
        Self::new(titre, TIRET, TEXT_FAINT).sous(pourquoi)
    }

    fn sous(mut self, sous: impl Into<String>) -> Self {
        self.sous = sous.into();
        self
    }

    fn icone(mut self, icone: Option<&'a egui::TextureHandle>) -> Self {
        self.icone = icone;
        self
    }

    fn info(mut self, info: impl Into<String>) -> Self {
        self.info = info.into();
        self
    }
}

/// Dessine une tuile de `largeur` (marges comprises) et d'au moins
/// `hauteur` ; `bas` ajoute ce qui va sous la ligne du bas — une jauge,
/// une barre. Rend la réponse du cadre.
fn tuile(ui: &mut Ui, largeur: f32, hauteur: f32, t: Tuile, bas: impl FnOnce(&mut Ui)) -> Response {
    let interieur = largeur - 2.0 * MARGE_TUILE;
    let reponse = egui::Frame::new()
        .fill(theme::BG_RAISED)
        .corner_radius(egui::CornerRadius::same(rayon::XL))
        .inner_margin(egui::Margin::same(MARGE_TUILE as i8))
        .show(ui, |ui| {
            // Le cadre hérite de la disposition du parent — une ligne qui
            // replie, quand les tuiles sont côte à côte — et sans ceci le
            // titre, la valeur et la sous-ligne partiraient en escalier :
            // dedans, on empile toujours de haut en bas.
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_min_size(Vec2::new(interieur, hauteur - 2.0 * MARGE_TUILE));
                ui.set_max_width(interieur);
                ui.spacing_mut().item_spacing.y = espace::XXS;
                ui.add(egui::Label::new(Texte::Aide.riche(ui, t.titre)).truncate());
                ui.horizontal(|ui| {
                    if let Some(icone) = t.icone {
                        ui.add(egui::Image::new(icone).fit_to_exact_size(Vec2::splat(18.0)));
                    }
                    ui.add(egui::Label::new(Texte::TitreFenetre.riche(ui, &t.valeur).color(t.teinte)).truncate());
                });
                if !t.sous.is_empty() {
                    ui.add(egui::Label::new(Texte::Aide.riche_ton(ui, Ton::Discret, &t.sous)).truncate());
                }
                bas(ui);
            });
        })
        .response;
    if t.info.is_empty() {
        reponse
    } else {
        reponse.on_hover_text(t.info)
    }
}

/// Des tuiles de même taille rangées en lignes : autant par ligne que la
/// largeur disponible en contient, puis on passe à la suivante.
/// `horizontal_wrapped` ne convient pas ici : un cadre qui impose sa
/// largeur minimale ne replie pas, il déborde — et la fenêtre grandit pour
/// le contenir, jusqu'à sortir de l'écran sans pouvoir être réduite.
/// Ce qui va sous la ligne du bas d'une tuile : une jauge, une barre, rien.
type Bas<'a> = Box<dyn FnOnce(&mut Ui) + 'a>;

struct Grille<'a> {
    largeur: f32,
    hauteur: f32,
    tuiles: Vec<(Tuile<'a>, Bas<'a>)>,
}

impl<'a> Grille<'a> {
    fn new(largeur: f32, hauteur: f32) -> Self {
        Self { largeur, hauteur, tuiles: Vec::new() }
    }

    /// Une tuile de plus, avec ce qui va sous sa ligne du bas.
    fn tuile(&mut self, t: Tuile<'a>, bas: impl FnOnce(&mut Ui) + 'a) {
        self.tuiles.push((t, Box::new(bas)));
    }

    fn montrer(self, ui: &mut Ui) {
        let ecart = ui.spacing().item_spacing.x;
        let au_plus = (((ui.available_width() + ecart) / (self.largeur + ecart)).floor() as usize).max(1);
        // Des lignes équilibrées : six tuiles dans une fenêtre qui en
        // contient cinq font deux lignes de trois, pas cinq et une.
        let n = self.tuiles.len().max(1);
        let lignes = n.div_ceil(au_plus);
        let par_ligne = n.div_ceil(lignes);
        let mut reste = self.tuiles.into_iter().peekable();
        while reste.peek().is_some() {
            ui.horizontal(|ui| {
                for (t, bas) in reste.by_ref().take(par_ligne) {
                    tuile(ui, self.largeur, self.hauteur, t, bas);
                }
            });
        }
    }
}

/// Un texte de cellule, en 11,5 — la taille des tables de la page.
fn cellule(ui: &mut Ui, texte: impl Into<String>, couleur: Color32) -> Response {
    ui.label(RichText::new(texte.into()).color(couleur).size(texte::PETIT))
}

/// Ce que dit le survol d'un point de RR : la carte, le delta, le rang,
/// quand, et ce qui l'explique (bonus, placement, descente protégée).
fn info_de_point(x: &PointRR) -> String {
    let mut texte = format!(
        "{} · {} · {} · {} RR · {}",
        if x.carte.is_empty() { "?" } else { x.carte.as_str() },
        signe(x.delta).0,
        nom_de_rang(x.tier),
        x.rr,
        crate::formats::il_y_a(x.date)
    );
    if x.bonus != 0 {
        texte.push_str(&format!(" · dont {} de bonus de performance", signe(x.bonus).0));
    }
    if x.placement {
        texte.push_str(" · placement");
    }
    if x.protege {
        texte.push_str(" · descente protégée");
    }
    texte
}

/// L'agent d'une ligne de table : son portrait (valorant-api.com) puis son
/// nom — le nom seul tant que l'image n'est pas là.
fn agent_cellule(ui: &mut Ui, agent: &str, catalogue: &Catalogue) -> Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = espace::XS;
        if let Some(portrait) = catalogue.agent(agent).filter(|_| !agent.is_empty()) {
            ui.add(egui::Image::new(portrait).fit_to_exact_size(Vec2::splat(PORTRAIT)));
        }
        cellule(ui, agent, TEXT_DIM);
    })
    .response
}

/// La carte d'une ligne de table : son nom sur son bandeau, assombri pour
/// que le texte se lise — le nom seul tant que l'image n'est pas là.
fn carte_cellule(ui: &mut Ui, carte: &str, catalogue: &Catalogue) -> Response {
    let Some(bandeau) = catalogue.bandeau(carte).filter(|_| !carte.is_empty()) else {
        return cellule(ui, carte, TEXT);
    };
    pastille_carte(ui, carte, bandeau, HAUTEUR_PASTILLE, 11.5)
}

/// Un nom sur le bandeau de sa carte : l'image rognée au centre pour
/// remplir la pastille, un voile sombre, le texte en blanc.
fn pastille_carte(ui: &mut Ui, texte: &str, bandeau: &egui::TextureHandle, hauteur: f32, taille: f32) -> Response {
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(texte.to_string(), FontId::proportional(taille), Color32::WHITE));
    let dimensions = Vec2::new((galley.size().x + 16.0).max(LARGEUR_PASTILLE_MIN), hauteur);
    let (rect, reponse) = ui.allocate_exact_size(dimensions, Sense::hover());
    if ui.is_rect_visible(rect) {
        egui::Image::new(bandeau)
            .uv(graphes::uv_couvrant(bandeau.size_vec2(), rect.size()))
            .corner_radius(4)
            .paint_at(ui, rect);
        ui.painter().rect_filled(rect, egui::CornerRadius::same(rayon::S), Color32::from_black_alpha(120));
        ui.painter().galley(Pos2::new(rect.left() + 8.0, rect.center().y - galley.size().y / 2.0), galley, Color32::WHITE);
    }
    reponse
}

/// L'or pour le MVP et la distinction, le texte pour les autres.
fn teinte_de_medaille(m: Medaille) -> Color32 {
    match m {
        Medaille::Mvp | Medaille::Distinction => theme::WARN,
        _ => TEXT_DIM,
    }
}

/// Le nom court d'une médaille dans une case de table : les mots des
/// joueurs. Le nom entier est au survol.
fn nom_court(m: Medaille) -> &'static str {
    match m {
        Medaille::Mvp => "MVP",
        Medaille::Distinction => "DIST",
        Medaille::TopFrag => "TOP",
        Medaille::Degats => "ADR",
        Medaille::Tetes => "HS",
        Medaille::Kills => "KILLS",
        Medaille::Assists => "ASSISTS",
        Medaille::Poses => "POSES",
        Medaille::PremiersSangs => "FB",
        Medaille::Aces => "ACE",
        Medaille::Clutchs => "CLUTCH",
        Medaille::Echanges => "TRADES",
        Medaille::Inconnue => "?",
    }
}

/// Les médailles d'un match à montrer, dans l'ordre : sans les inconnues,
/// et sans la distinction quand il y a le MVP — c'est la même note.
fn medailles_a_montrer(md: &MedaillesDuMatch) -> Vec<&MedailleGagnee> {
    medailles_triees(&md.medailles)
}

/// [`medailles_a_montrer`], pour une liste de médailles.
fn medailles_triees(medailles: &[MedailleGagnee]) -> Vec<&MedailleGagnee> {
    let mut liste: Vec<&MedailleGagnee> = medailles.iter().filter(|g| g.medaille != Medaille::Inconnue).collect();
    liste.sort_by_key(|g| g.medaille.ordre());
    let mvp = liste.iter().any(|g| g.medaille == Medaille::Mvp);
    liste.retain(|g| !(mvp && g.medaille == Medaille::Distinction));
    liste
}

/// Les médailles d'un match dans une case : trois pastilles au plus (★ :
/// record de l'acte), puis « +N » ; le détail au survol de chacune. Une
/// case vide sans médaille.
fn medailles_cellule(ui: &mut Ui, md: Option<&MedaillesDuMatch>) -> Response {
    let liste = md.map(medailles_a_montrer).unwrap_or_default();
    if liste.is_empty() {
        return ui.label("");
    }
    let detail = liste
        .iter()
        .map(|g| {
            let record = if g.record { " · record de l'acte" } else { "" };
            format!("{} — {}{record}", g.medaille.nom(), g.medaille.valeur(g.valeur))
        })
        .collect::<Vec<_>>()
        .join("\n");
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = espace::XS;
        for g in liste.iter().take(3) {
            let texte = if g.record { format!("{}★", nom_court(g.medaille)) } else { nom_court(g.medaille).to_string() };
            badge(ui, &texte, teinte_de_medaille(g.medaille)).on_hover_text(&detail);
        }
        if liste.len() > 3 {
            ui.label(Texte::Minuscule.riche(ui, format!("+{}", liste.len() - 3))).on_hover_text(&detail);
        }
    })
    .response
}

/// Une étiquette en capitales sur un fond de sa couleur : « EN PARTIE ».
fn badge(ui: &mut Ui, texte: &str, couleur: Color32) -> Response {
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(texte.to_string(), FontId::proportional(texte::MINUSCULE), couleur));
    let (rect, reponse) = ui.allocate_exact_size(Vec2::new(galley.size().x + 10.0, 16.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, egui::CornerRadius::same(rayon::S), theme::alpha(couleur, 36));
        ui.painter().galley(Pos2::new(rect.left() + 5.0, rect.center().y - galley.size().y / 2.0), galley, couleur);
    }
    reponse
}

// ---------------------------------------------------------------------
// En direct
// ---------------------------------------------------------------------

/// Où en est une partie, pour trier « En direct » : les parties d'abord,
/// puis la sélection d'agents, les files, les menus.
fn rang_d_etat(j: &JeuStatut) -> u8 {
    match j.etat {
        JeuEtat::EnJeu => 0,
        JeuEtat::PreGame => 1,
        JeuEtat::Menus if !j.file.is_empty() => 2,
        JeuEtat::Menus => 3,
        // Un état d'une version plus récente : en dernier, faute de savoir.
        JeuEtat::Inconnu => 4,
    }
}

/// Les lignes d'« En direct » : les indices des joueurs réunis quand ils
/// sont dans la même partie de toute évidence — en jeu ou en sélection,
/// même file, même carte et même score vu du même camp — un par ligne
/// sinon, dans l'ordre reçu. Deux membres dans des camps opposés restent
/// sur deux lignes : leurs scores sont inversés.
fn regrouper_en_direct(statuts: &[&JeuStatut]) -> Vec<Vec<usize>> {
    let cle = |j: &JeuStatut| {
        (matches!(j.etat, JeuEtat::EnJeu | JeuEtat::PreGame) && !j.carte.is_empty())
            .then(|| (j.etat == JeuEtat::EnJeu, j.file.clone(), j.carte.clone(), j.score_allie, j.score_adverse))
    };
    let mut lignes: Vec<Vec<usize>> = Vec::new();
    let mut par_partie: HashMap<(bool, String, String, u8, u8), usize> = HashMap::new();
    for (i, j) in statuts.iter().enumerate() {
        match cle(j) {
            Some(k) => match par_partie.get(&k) {
                Some(&ligne) => lignes[ligne].push(i),
                None => {
                    par_partie.insert(k, lignes.len());
                    lignes.push(vec![i]);
                }
            },
            None => lignes.push(vec![i]),
        }
    }
    lignes
}

/// Ce qu'on dit d'une partie en direct, sans la carte (elle est sur le
/// bandeau) : la file, puis où il en est, et sa party.
fn etat_en_direct(j: &JeuStatut) -> (String, Option<(String, Color32)>) {
    let file = j.libelle_file();
    let party = if j.party_taille > 1 {
        format!(" · party {}/{}", j.party_taille, j.party_max.max(j.party_taille))
    } else {
        String::new()
    };
    match j.etat {
        JeuEtat::EnJeu => {
            let score = (!(j.custom && j.score_allie == 0 && j.score_adverse == 0)).then(|| {
                let teinte = teinte_de_score((j.score_allie != j.score_adverse).then_some(j.score_allie > j.score_adverse));
                (format!("{}-{}", j.score_allie, j.score_adverse), teinte)
            });
            (format!("{file}{party}"), score)
        }
        JeuEtat::PreGame if file.is_empty() => (format!("sélection des agents{party}"), None),
        JeuEtat::PreGame => (format!("{file} · sélection des agents{party}"), None),
        JeuEtat::Menus if file.is_empty() => (format!("au menu{party}"), None),
        JeuEtat::Menus => (format!("en file {file}{party}"), None),
        JeuEtat::Inconnu => (format!("en ligne{party}"), None),
    }
}

/// L'étiquette d'une partie en direct, et sa couleur.
fn badge_d_etat(j: &JeuStatut) -> (&'static str, Color32) {
    match rang_d_etat(j) {
        0 => ("EN PARTIE", SPEAK),
        1 => ("SÉLECTION", ACCENT),
        2 => ("EN FILE", ACCENT),
        _ => ("AU MENU", TEXT_FAINT),
    }
}

/// Qui est sur VALORANT en ce moment, d'après la présence que chacun
/// partage : les parties d'abord, puis la sélection, les files, les menus.
fn joueurs_en_direct<'a>(membres: &'a [Member], catalogue: &'a Catalogue) -> Vec<(&'a Member, Cow<'a, JeuStatut>)> {
    let mut joueurs: Vec<(&Member, Cow<JeuStatut>)> = membres
        .iter()
        .filter(|m| m.online)
        .filter_map(|m| m.jeu.as_ref().filter(|j| j.est_valorant()).map(|j| (m, catalogue.nommer(j))))
        .collect();
    joueurs.sort_by_key(|(m, j)| (rang_d_etat(j), m.username.to_lowercase()));
    joueurs
}

/// « 3 membres sur VALORANT, 2 en partie ».
fn resume_en_direct(joueurs: &[(&Member, Cow<JeuStatut>)]) -> String {
    let en_partie = joueurs.iter().filter(|(_, j)| j.etat == JeuEtat::EnJeu).count();
    match (joueurs.len(), en_partie) {
        (n, 0) => format!("{} sur VALORANT", pluriel(n as u32, "membre")),
        (n, p) if p == n => format!("{} en partie", pluriel(n as u32, "membre")),
        (n, p) => format!("{} sur VALORANT, {p} en partie", pluriel(n as u32, "membre")),
    }
}

/// « En direct », ligne par ligne : rien de plus que ce que la liste des
/// membres dit déjà, rangé et illustré. Ceux qui jouent ensemble tiennent
/// sur une ligne ; toutes les parties en cours y sont, liées ou non à un
/// compte Riot.
fn lignes_en_direct(
    ui: &mut Ui,
    joueurs: &[(&Member, Cow<JeuStatut>)],
    noms: &Annuaire,
    rangs: &rangs::Rangs,
    catalogue: &Catalogue,
    my_id: Option<UserId>,
    demandes: &mut Vec<Demande>,
) {
    let statuts: Vec<&JeuStatut> = joueurs.iter().map(|(_, j)| j.as_ref()).collect();
    for ligne in regrouper_en_direct(&statuts) {
        let j = statuts[ligne[0]];
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = espace::S;
            // Le bandeau de la carte en tête, ou sa place vide : les noms
            // s'alignent d'une ligne à l'autre.
            let carte = if j.etat == JeuEtat::Menus { "" } else { j.carte.as_str() };
            match catalogue.bandeau(carte).filter(|_| !carte.is_empty()) {
                Some(bandeau) => {
                    pastille_carte(ui, carte, bandeau, 24.0, 12.0);
                }
                None if !carte.is_empty() => {
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(LARGEUR_PASTILLE_MIN + 24.0, 24.0), Sense::hover());
                    ui.painter().rect_filled(rect, egui::CornerRadius::same(rayon::S), theme::alpha(TEXT, 10));
                    ui.painter().text(
                        Pos2::new(rect.left() + 8.0, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        carte,
                        FontId::proportional(texte::COURANT),
                        TEXT,
                    );
                }
                None => {
                    ui.allocate_exact_size(Vec2::new(LARGEUR_PASTILLE_MIN + 24.0, 24.0), Sense::hover());
                }
            }
            let (etiquette, couleur) = badge_d_etat(j);
            badge(ui, etiquette, couleur);
            for (n, &i) in ligne.iter().enumerate() {
                let (m, statut) = (&joueurs[i].0, &joueurs[i].1);
                if n > 0 {
                    ui.label(RichText::new("·").color(TEXT_FAINT));
                }
                let mut pseudo = RichText::new(&m.username).color(noms.couleur(m.user_id)).size(texte::COURANT);
                if my_id == Some(m.user_id) {
                    pseudo = pseudo.font(ki_ui::jetons::police::moyenne(ui.ctx(), texte::COURANT));
                }
                let lie = m.riot_id.is_some();
                let reponse = ui.add(egui::Label::new(pseudo).sense(if lie { Sense::click() } else { Sense::hover() }));
                if lie && reponse.on_hover_text("ouvrir sa fiche").clicked() {
                    demandes.push(Demande::OuvrirFiche(m.user_id, m.username.clone()));
                }
                // Sur un autre compte que celui de sa fiche, le palier est
                // celui de la présence.
                let palier = if crate::membres::sur_un_autre_compte(m) { statut.rang } else { m.rang_valorant.unwrap_or(statut.rang) };
                if let Some(icone) = rangs.texture(palier).filter(|_| palier >= 3) {
                    ui.add(egui::Image::new(icone).fit_to_exact_size(Vec2::splat(16.0)))
                        .on_hover_text(nom_de_rang(palier));
                }
            }
            let (texte, score) = etat_en_direct(j);
            if let Some((score, teinte)) = score {
                ui.label(RichText::new(score).color(teinte).font(ki_ui::jetons::police::moyenne(ui.ctx(), texte::COURANT)));
            }
            if !texte.is_empty() {
                ui.label(Texte::Aide.riche_ton(ui, Ton::Discret, texte));
            }
        });
        ui.add_space(espace::XXS);
    }
}

// ---------------------------------------------------------------------
// La page du groupe
// ---------------------------------------------------------------------

/// Ce que la page lit de l'application, prêté le temps d'une image : ce
/// que le serveur a envoyé, et de quoi nommer et illustrer.
#[derive(Clone, Copy)]
pub struct Contexte<'a> {
    /// Les fiches du groupe (`StatsValorant`).
    pub stats: &'a [FicheMembre],
    /// Elles sont arrivées, même vides.
    pub recu: bool,
    /// Où en est leur demande : en vol, sans réponse, ou rien.
    pub chargement: crate::valo_donnees::Chargement,
    pub esports: &'a [MatchEsport],
    /// Les parties du groupe par heure de la semaine, en UTC.
    pub activite: &'a [u16],
    pub moi: Option<UserId>,
    pub membres: &'a [Member],
    pub rangs: &'a rangs::Rangs,
    pub catalogue: &'a Catalogue,
    /// Les parties du groupe depuis 6 h du matin (`ServerMsg::Soiree`).
    pub soiree: &'a [PartieSoiree],
    /// Le serveur raconte la soirée (`Welcome.soiree`) : sans lui, le fil
    /// le dit.
    pub soiree_servie: bool,
    /// Ma propre fiche, entière, si le serveur l'a envoyée : « Moi » sur la
    /// carte de chaleur de l'accueil.
    pub ma_fiche: Option<&'a FicheValorant>,
    /// Les photos des membres, pour leurs avatars : celles que ki-chat a
    /// déjà en mémoire, rien n'est demandé d'ici.
    pub photos: &'a HashMap<UserId, (String, egui::TextureHandle)>,
    /// La fiche à laquelle on compare celle qui est ouverte (la vue
    /// Comparer du profil), demandée ou reçue.
    pub comparee: Option<&'a FicheOuverte>,
    /// Mon journal des parties, pour la vue Progression de mon profil.
    pub journal: Journal<'a>,
}

/// Trie stable : ceux qui ont joué d'abord, puis par la clé dans le sens
/// demandé, les sans-clé derrière ; à égalité, l'ordre reçu.
fn trier<T>(ordre: &mut [T], en_bas: impl Fn(&T) -> bool, cle: impl Fn(&T) -> Option<f64>, desc: bool) {
    ordre.sort_by(|a, b| {
        en_bas(a).cmp(&en_bas(b)).then_with(|| match (cle(a), cle(b)) {
            (Some(x), Some(y)) => {
                if desc {
                    y.total_cmp(&x)
                } else {
                    x.total_cmp(&y)
                }
            }
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        })
    });
}

/// Les six records du groupe.
fn records(ui: &mut Ui, lignes: &[Ligne], periode: Periode, rangs: &rangs::Rangs) {
    let fenetre = format!("sur {}, {ASSEZ} classés au moins", periode.titre());
    let assez = |l: &&Ligne| l.b(periode).assez(ASSEZ);

    let haut = lignes.iter().filter(|l| l.f.fiche.rang.tier >= 3).max_by_key(|l| (l.f.fiche.rang.tier, l.f.fiche.rang.rr));
    let grimpeur = lignes.iter().filter(|l| l.b(periode).rr > 0).max_by_key(|l| l.b(periode).rr);
    let kd = lignes
        .iter()
        .filter(assez)
        .filter_map(|l| l.b(periode).kd().map(|v| (l, v)))
        .max_by(|a, c| a.1.total_cmp(&c.1));
    let acs = lignes
        .iter()
        .filter(assez)
        .filter_map(|l| l.b(periode).acs().map(|v| (l, v)))
        .max_by(|a, c| a.1.total_cmp(&c.1));
    let assidu = lignes.iter().filter(|l| l.b(periode).matchs > 0).max_by_key(|l| l.b(periode).matchs);
    let clutcheur = lignes
        .iter()
        .filter(|l| l.b(periode).clutchs > 0)
        .max_by_key(|l| (l.b(periode).clutchs, l.b(periode).meilleur_clutch));
    let serie = lignes.iter().filter(|l| l.bilan.serie > 0).max_by_key(|l| l.bilan.serie);

    let (w, h) = (TUILE_GROUPE.x, TUILE_GROUPE.y);
    // Une tuile fait 126 px dedans, et sa valeur en 19 px n'a que ~100 px
    // après l'icône : le nom du rang seul en valeur, les RR et le pseudo
    // en dessous — « Ascendant 3 · 99 RR » finirait en « Ascendant 3 ·… ».
    let mut grille = Grille::new(w, h);
    {
        let t = match haut {
            Some(l) => {
                let r = &l.f.fiche.rang;
                Tuile::new("Plus haut rang", nom_de_rang(r.tier), graphes::couleur_de_rang(r.tier))
                    .icone(rangs.texture(r.tier))
                    .sous(format!("{} RR · {}", r.rr, l.f.username))
                    .info("le rang courant le plus élevé du groupe")
            }
            None => Tuile::vide("Plus haut rang", "personne de classé"),
        };
        grille.tuile(t, |_| {});

        let t = match grimpeur {
            Some(l) => Tuile::new("Le grimpeur", format!("+{} RR", l.b(periode).rr), SPEAK)
                .sous(&l.f.username)
                .info(format!("la somme des RR gagnés et perdus sur {}", periode.titre())),
            None => Tuile::vide("Le grimpeur", "aucun RR gagné"),
        };
        grille.tuile(t, |_| {});

        let t = match kd {
            Some((l, v)) => Tuile::new("Meilleur K/D", dec(v, 2), ACCENT)
                .sous(&l.f.username)
                .info(format!("kills / morts, {fenetre}")),
            None => Tuile::vide("Meilleur K/D", "pas assez de parties"),
        };
        grille.tuile(t, |_| {});

        let t = match acs {
            Some((l, v)) => Tuile::new("Meilleur ACS", opt_entier(Some(v)), ACCENT)
                .sous(&l.f.username)
                .info(format!("score moyen par manche, {fenetre}")),
            None => Tuile::vide("Meilleur ACS", "pas assez de parties"),
        };
        grille.tuile(t, |_| {});

        let t = match assidu {
            Some(l) => Tuile::new("Le plus assidu", pluriel(u32::from(l.b(periode).matchs), "classé"), TEXT)
                .sous(&l.f.username)
                .info(format!("le plus de classés sur {}", periode.titre())),
            None => Tuile::vide("Le plus assidu", "aucun classé"),
        };
        grille.tuile(t, |_| {});

        let t = match (clutcheur, serie) {
            (Some(l), _) => {
                let bl = l.b(periode);
                // « 6 clutchs · 1v3 » déborde la valeur : le plus gros X
                // descend sous la ligne, avec le pseudo.
                Tuile::new("Le clutcheur", pluriel(u32::from(bl.clutchs), "clutch"), theme::WARN)
                    .sous(format!("meilleur 1v{} · {}", bl.meilleur_clutch, l.f.username))
                    .info(format!("situations 1 contre X gagnées, et le plus gros X, sur {}", periode.titre()))
            }
            (None, Some(l)) => Tuile::new("Meilleure série", format!("{} V d'affilée", l.bilan.serie), SPEAK)
                .sous(&l.f.username)
                .info("les victoires classées d'affilée en cours, nuls sautés"),
            (None, None) => Tuile::vide("Le clutcheur", "pas assez de parties"),
        };
        grille.tuile(t, |_| {});
    }
    grille.montrer(ui);
}

fn paires_de_duos<'a>(duos: impl Iterator<Item = (UserId, &'a [(UserId, u16, u16)])>) -> Vec<(UserId, UserId, u16, u16)> {
    let mut paires: BTreeMap<(UserId, UserId), (u16, u16)> = BTreeMap::new();
    for (moi, liste) in duos {
        for &(autre, parties, victoires) in liste {
            if autre == moi {
                continue;
            }
            let cle = (moi.min(autre), moi.max(autre));
            let e = paires.entry(cle).or_default();
            if parties > e.0 {
                *e = (parties, victoires);
            }
        }
    }
    let mut v: Vec<(UserId, UserId, u16, u16)> =
        paires.into_iter().filter(|(_, (p, _))| *p >= 2).map(|((a, b), (p, g))| (a, b, p, g)).collect();
    v.sort_by_key(|&(_, _, p, g)| std::cmp::Reverse((p, g)));
    v.truncate(5);
    v
}

/// Les 168 cases UTC tournées à l'heure locale : une heure qui passe
/// minuit change de jour, et le dimanche soir déborde sur lundi.
fn activite_locale(cases: &[u16], decalage_h: i32) -> Vec<u16> {
    let mut locale = vec![0u16; 168];
    for jour in 0..7 {
        for heure in 0..24 {
            let v = cases.get(jour * 24 + heure).copied().unwrap_or(0);
            if v == 0 {
                continue;
            }
            let h = heure as i32 + decalage_h;
            let (report, h) = (h.div_euclid(24), h.rem_euclid(24));
            let j = (jour as i32 + report).rem_euclid(7);
            let case = &mut locale[j as usize * 24 + h as usize];
            *case = case.saturating_add(v);
        }
    }
    locale
}

/// Les blocs du fil : les indices des matchs qui partagent un `id`
/// (joués ensemble) réunis, dans l'ordre reçu ; un `id` vide reste seul.
fn regrouper(ids: &[&str]) -> Vec<Vec<usize>> {
    let mut par_id: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, id) in ids.iter().enumerate() {
        if !id.is_empty() {
            par_id.entry(id).or_default().push(i);
        }
    }
    let mut vus: HashSet<&str> = HashSet::new();
    let mut blocs = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        if id.is_empty() {
            blocs.push(vec![i]);
        } else if vus.insert(id) {
            if let Some(bloc) = par_id.get(id) {
                blocs.push(bloc.clone());
            }
        }
    }
    blocs
}

// ---------------------------------------------------------------------
// La fiche d'un membre
// ---------------------------------------------------------------------

/// Ce que la fiche montre une fois les filtres appliqués : `e` les matchs
/// retenus (du plus récent au plus ancien), `p` les points de RR de la
/// même fenêtre, `b` le bilan des matchs à manches de `e`.
struct Vue<'a> {
    username: &'a str,
    fiche: &'a FicheValorant,
    e: Vec<&'a MatchResume>,
    p: Vec<&'a PointRR>,
    b: Bilan,
    classe: bool,
    periode: PeriodeFiche,
    noms: Annuaire<'a>,
    rangs: &'a rangs::Rangs,
    catalogue: &'a Catalogue,
    /// Les bilans du groupe s'il a déjà été reçu — pour la médiane.
    groupe: Vec<Ligne<'a>>,
    /// Les parties du groupe par heure, en UTC : le contour de « Mes
    /// heures ».
    activite: &'a [u16],
}

impl<'a> Vue<'a> {
    fn new(username: &'a str, fiche: &'a FicheValorant, periode: PeriodeFiche, classe: bool, cx: &Contexte<'a>, maintenant: u64) -> Self {
        let Contexte { stats, membres, rangs, catalogue, activite, .. } = *cx;
        let (depuis, derniers) = periode.fenetre(maintenant);
        let mut e: Vec<&MatchResume> = fiche
            .matchs
            .iter()
            .filter(|m| m.date >= depuis && (!classe || m.mode == MODE_CLASSE))
            .collect();
        e.sort_by_key(|m| std::cmp::Reverse(m.date));
        let mut p: Vec<&PointRR> = fiche.historique_rr.iter().filter(|x| x.date >= depuis).collect();
        p.sort_by_key(|x| std::cmp::Reverse(x.date));
        if let Some(n) = derniers {
            e.truncate(n);
            p.truncate(n);
        }
        // Les moyennes ne comptent que les matchs à manches ; la table
        // montre aussi les combats à mort.
        let mut b = Bilan::default();
        for m in e.iter().filter(|m| FicheValorant::a_des_manches(m)) {
            b.ajouter(m);
        }
        b.rr = p.iter().fold(0i32, |acc, x| acc.saturating_add(x.delta));
        let (groupe, _) = lignes_du_groupe(stats, maintenant);
        Self { username, fiche, e, p, b, classe, periode, noms: Annuaire::new(stats, membres), rangs, catalogue, groupe, activite }
    }

    /// Les matchs à manches de la sélection, ceux qui pèsent.
    fn comptes(&self) -> impl Iterator<Item = &&'a MatchResume> + '_ {
        self.e.iter().filter(|m| FicheValorant::a_des_manches(m))
    }

    /// (nom, bilan) par la clé donnée sur les matchs comptés, les plus
    /// joués d'abord, l'alphabet à égalité.
    fn ventiler(&self, cle: fn(&MatchResume) -> &str) -> Vec<(String, Bilan)> {
        let mut par: BTreeMap<&str, Bilan> = BTreeMap::new();
        for m in self.comptes() {
            par.entry(cle(m)).or_default().ajouter(m);
        }
        let mut v: Vec<(String, Bilan)> = par.into_iter().map(|(k, b)| (k.to_string(), b)).collect();
        v.sort_by_key(|(_, b)| std::cmp::Reverse(b.matchs));
        v
    }

    /// Les médailles d'un match, pour leur colonne.
    fn medailles_du(&self, id: &str) -> Option<&'a MedaillesDuMatch> {
        self.fiche.medailles.as_ref()?.du_match(id)
    }

    /// Le point de RR d'un match, pour la colonne ΔRR.
    fn point_de(&self, id: &str) -> Option<&'a PointRR> {
        if id.is_empty() {
            return None;
        }
        self.fiche.historique_rr.iter().find(|p| p.match_id == id)
    }

    /// La médiane et le plafond du groupe pour un taux, quand au moins
    /// trois membres ont assez de classés sur 30 jours ; le plafond
    /// prend en compte la valeur du membre, pour que sa jauge tienne.
    fn repere(&self, valeur: fn(&Bilan) -> Option<f32>, moi: f32) -> Option<(f32, f32)> {
        let mut vals: Vec<f32> = self
            .groupe
            .iter()
            .map(|l| &l.bilan.trente_jours)
            .filter(|b| b.assez(ASSEZ))
            .filter_map(valeur)
            .filter(|v| v.is_finite())
            .collect();
        if vals.len() < 3 {
            return None;
        }
        vals.sort_by(|a, b| a.total_cmp(b));
        let n = vals.len();
        let mediane = if n % 2 == 1 { vals[n / 2] } else { (vals[n / 2 - 1] + vals[n / 2]) / 2.0 };
        let max = vals[n - 1].max(moi);
        (max > 0.0).then_some((mediane, max))
    }
}

impl PageValo {

    /// La courbe seule, sur `hauteur` : ou un mot quand il n'y a pas de
    /// quoi la tracer.
    fn courbe_rr(&mut self, ui: &mut Ui, vue: &Vue, hauteur: f32) {
        if vue.p.is_empty() {
            ui::hint(ui, "Aucun match classé connu.");
            self.survol = None;
            return;
        }
        if vue.p.len() < 2 {
            ui::hint(ui, "Pas encore assez de classés pour une courbe.");
            self.survol = None;
            return;
        }
        // Du plus ancien au plus récent pour les frontières d'acte ; la
        // courbe trie elle-même et parle en indices de la tranche reçue.
        let mut dans_l_ordre: Vec<&PointRR> = vue.p.clone();
        dans_l_ordre.sort_by_key(|x| x.date);
        let frontieres = frontieres_d_acte(&dans_l_ordre);
        let points: Vec<PointCourbe> = vue.p.iter().map(|x| PointCourbe::from(*x)).collect();
        let pic = vue.fiche.pic.as_ref().filter(|p| p.tier >= 3).map(|p| graphes::valeur_rr(p.tier, p.rr));
        let info = |i: usize| vue.p.get(i).map_or_else(String::new, |x| info_de_point(x));
        let survole = graphes::courbe_temps(ui, egui::Id::unique("fiche_courbe"), &points, hauteur, true, &frontieres, pic, info);
        self.survol = survole.and_then(|i| vue.p.get(i)).map(|x| x.match_id.clone()).filter(|id| !id.is_empty());
    }

}

/// Les frontières d'acte entre points consécutifs (du plus ancien au plus
/// récent) : la date du premier point du nouvel acte, et « e9a2 → e9a3 ».
fn frontieres_d_acte(chrono: &[&PointRR]) -> Vec<(u64, String)> {
    chrono
        .windows(2)
        .filter(|w| !w[0].saison.is_empty() && !w[1].saison.is_empty() && w[0].saison != w[1].saison)
        .map(|w| (w[1].date, format!("{} → {}", w[0].saison, w[1].saison)))
        .collect()
}

/// L'identité en tête de fiche : le Riot ID, la région, la plateforme, le
/// niveau et le titre — posés sur la carte de joueur qu'il portait à son
/// dernier match quand le serveur la connaît (0.1.49 et après), en une
/// ligne de texte sinon.
fn identite(ui: &mut Ui, fiche: &FicheValorant, catalogue: &Catalogue) {
    // Ce qu'on sait, sans séparateur pendu quand la région manque.
    let mut parts: Vec<String> = [&fiche.region, &fiche.plateforme]
        .iter()
        .map(|s| s.trim().to_uppercase())
        .filter(|s| !s.is_empty())
        .collect();
    if fiche.niveau > 0 {
        parts.push(format!("niveau {}", fiche.niveau));
    }
    let detail = parts.join(" · ");
    let titre = catalogue.titre(&fiche.titre_joueur).map(|t| format!("« {t} »"));
    if fiche.carte_joueur.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label(Texte::TitreSection.riche(ui, &fiche.riot_id));
            ui.label(Texte::Aide.riche_ton(ui, Ton::Discret, &detail));
            if let Some(titre) = &titre {
                ui::hint(ui, titre);
            }
        });
        return;
    }
    // La place du bandeau se prend tout de suite, image ou pas : rien ne
    // saute quand elle arrive.
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), HAUTEUR_BANDEAU_FICHE), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let image = catalogue.carte_joueur(&fiche.carte_joueur);
    match image {
        Some(carte) => {
            egui::Image::new(carte)
                .uv(graphes::uv_couvrant(carte.size_vec2(), rect.size()))
                .corner_radius(6)
                .paint_at(ui, rect);
            if let Some(voile) = catalogue.voile() {
                egui::Image::new(voile).corner_radius(6).paint_at(ui, rect);
            }
        }
        None => {
            ui.painter().rect_filled(rect, egui::CornerRadius::same(rayon::M), theme::alpha(TEXT, 10));
        }
    }
    let (fort, doux) = if image.is_some() {
        (Color32::WHITE, Color32::from_white_alpha(200))
    } else {
        (TEXT, TEXT_DIM)
    };
    let mut lignes = vec![ui.fonts_mut(|f| f.layout_no_wrap(fiche.riot_id.clone(), FontId::proportional(texte::GRAND), fort))];
    lignes.push(ui.fonts_mut(|f| f.layout_no_wrap(detail, FontId::proportional(texte::PETIT), doux)));
    if let Some(titre) = titre {
        lignes.push(ui.fonts_mut(|f| f.layout_no_wrap(titre, FontId::proportional(texte::PETIT), doux)));
    }
    let hauteur: f32 = lignes.iter().map(|g| g.size().y).sum::<f32>() + 2.0 * (lignes.len() as f32 - 1.0);
    let mut y = rect.center().y - hauteur / 2.0;
    let painter = ui.painter().with_clip_rect(rect);
    for galley in lignes {
        let h = galley.size().y;
        painter.galley(Pos2::new(rect.left() + 14.0, y), galley, fort);
        y += h + 2.0;
    }
}

/// Pourquoi on peut dire quelque chose d'un MMR que Riot cache.
const POURQUOI_MMR: &str = "Riot ne montre pas le MMR caché : on le devine à tes variations de RR — gagner plus qu'on ne perd, \
c'est un MMR au-dessus du rang, le jeu pousse à monter ; l'inverse, en dessous. Sur les 20 derniers classés de l'acte, \
sans les descentes protégées.";

/// Les classés à manches du plus récent au plus ancien — l'ordre de
/// `FicheValorant::forme`, refait ici pour avoir les matchs sous la main.
fn classes_recents(fiche: &FicheValorant) -> Vec<&MatchResume> {
    let mut v: Vec<&MatchResume> =
        fiche.matchs.iter().filter(|m| m.mode == MODE_CLASSE && FicheValorant::a_des_manches(m)).collect();
    v.sort_by_key(|m| std::cmp::Reverse(m.date));
    v
}

/// « 3k×12 · 4k×3 » — les multi-kills, sans les zéros : à trois d'un
/// coup, la ligne dépasserait la tuile. Rien à dire : « aucun multi-kill ».
fn multi_kills(triples: u16, quadruples: u16, aces: u16) -> String {
    let parts: Vec<String> = [(triples, "3k"), (quadruples, "4k"), (aces, "ace")]
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, mot)| format!("{mot}×{n}"))
        .collect();
    if parts.is_empty() {
        "aucun multi-kill".to_string()
    } else {
        parts.join(" · ")
    }
}

/// Les 168 cases jour × heure, à l'heure locale exacte, des dates données.
fn cases_des_heures(dates: impl Iterator<Item = u64>) -> Vec<u16> {
    let mut cases = vec![0u16; 168];
    for d in dates {
        let Some(t) = chrono::Local.timestamp_millis_opt(d as i64).single() else { continue };
        let jour = t.weekday().num_days_from_monday() as usize;
        let heure = t.hour() as usize;
        if let Some(case) = cases.get_mut(jour * 24 + heure) {
            *case = case.saturating_add(1);
        }
    }
    cases
}

/// Ce que dit une ligne dépliée : les cases des manches puis le récit.
fn recit_du_match(m: &MatchResume, noms: &Annuaire) -> String {
    let mut parts: Vec<String> = Vec::new();
    match m.mvp {
        ki_protocol::MVP_DU_MATCH => parts.push("MVP du match".to_string()),
        ki_protocol::MVP_D_EQUIPE => parts.push("MVP de son équipe".to_string()),
        _ => {}
    }
    if m.perf > 0 {
        parts.push(format!("note de performance {}", m.perf));
    }
    match &m.manches_detail {
        Some(d) => {
            if d.manches > 0 {
                parts.push(format!("KAST {} %", u32::from(d.kast) * 100 / u32::from(d.manches)));
            }
            if d.clutchs_tentes > 0 {
                let mut c = format!("{} sur {}", pluriel(u32::from(d.clutchs), "clutch"), d.clutchs_tentes);
                if d.meilleur_clutch > 0 {
                    c.push_str(&format!(" (1v{})", d.meilleur_clutch));
                }
                parts.push(c);
            }
            if d.poses > 0 {
                parts.push(pluriel(u32::from(d.poses), "pose"));
            }
            if d.desamorcages > 0 {
                parts.push(pluriel(u32::from(d.desamorcages), "désamorçage"));
            }
            if d.premiers_sangs > 0 {
                parts.push(pluriel(u32::from(d.premiers_sangs), "premier sang"));
            }
            if d.premieres_morts > 0 {
                parts.push(pluriel(u32::from(d.premieres_morts), "première mort"));
            }
            let multi: Vec<String> = [(d.triples, "3k"), (d.quadruples, "4k"), (d.aces, "ace")]
                .iter()
                .filter(|(n, _)| *n > 0)
                .map(|(n, mot)| format!("{mot}×{n}"))
                .collect();
            if !multi.is_empty() {
                parts.push(multi.join(" · "));
            }
        }
        // Sans détail, dire pourquoi sans se tromper : un combat à mort
        // n'a pas de manches (le serveur le sait et n'en calcule pas) ; un
        // match à manches sans détail date d'avant 0.1.40 ou vient de
        // l'archive de HenrikDev, rattrapée à la liaison sans le détail.
        None if !FicheValorant::a_des_manches(m) => {
            if m.mode.starts_with("Combat à mort") {
                parts.push(format!("{TIRET} combat à mort : pas de manches"));
            } else {
                parts.push(format!("{TIRET} pas de manches connues"));
            }
        }
        None => parts.push(format!("{TIRET} manches non lues : match d'avant 0.1.40, ou rattrapé de l'archive")),
    }
    if m.party > 0 {
        parts.push(format!("party de {}", m.party));
    }
    if m.duree_s > 0 {
        parts.push(format!("{} min", m.duree_s / 60));
    }
    if m.degats > 0 || m.degats_recus > 0 {
        parts.push(format!("dégâts {} reçus {}", milliers(m.degats), milliers(m.degats_recus)));
    }
    if m.tirs > 0 {
        parts.push(format!("{} % à la tête sur {} tirs", u32::from(m.tetes) * 100 / u32::from(m.tirs), m.tirs));
    }
    if !m.avec.is_empty() {
        parts.push(format!("avec {}", noms.noms(&m.avec)));
    }
    if !m.contre.is_empty() {
        parts.push(format!("contre {}", noms.noms(&m.contre)));
    }
    parts.join(" · ")
}

/// Une barre par acte, à la hauteur de son rang de fin : de Fer 1 (3) à
/// Radiant (27).
fn barres_d_actes(saisons: &[ki_protocol::StatsSaison]) -> Vec<graphes::BarreV> {
    saisons
        .iter()
        .map(|s| graphes::BarreV {
            valeur: f32::from(s.tier_fin.saturating_sub(2)) + f32::from(s.rr_fin.min(100)) / 100.0,
            couleur: graphes::couleur_de_rang(s.tier_fin),
            etiquette: s.saison.clone(),
            info: format!(
                "{} · fin d'acte : {} · {} RR · {} sur {}",
                s.saison,
                nom_de_rang(s.tier_fin),
                s.rr_fin,
                pluriel(u32::from(s.victoires), "victoire"),
                pluriel(u32::from(s.parties), "partie")
            ),
        })
        .collect()
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn point(match_id: &str, date: u64, saison: &str, delta: i32) -> PointRR {
        PointRR { match_id: match_id.into(), date, tier: 16, rr: 50, delta, saison: saison.into(), ..Default::default() }
    }

    pub(super) fn match_de_test(id: &str, date: u64, mode: &str, gagne: bool, avec: &[UserId]) -> MatchResume {
        MatchResume {
            id: id.into(),
            date,
            carte: "Ascent".into(),
            mode: mode.into(),
            agent: "Jett".into(),
            kills: 20,
            deaths: 10,
            assists: 4,
            score: 5_500,
            manches: if gagne { (13, 9) } else { (9, 13) },
            gagne: Some(gagne),
            avec: avec.to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn les_choix_se_relisent_par_leur_cle() {
        for p in Periode::TOUTES {
            assert_eq!(Periode::depuis(p.cle()), p);
        }
        assert_eq!(Periode::depuis(""), Periode::Trente);
        for p in PeriodeFiche::TOUTES {
            assert_eq!(PeriodeFiche::depuis(p.cle()), p);
        }
        assert_eq!(PeriodeFiche::depuis("?"), PeriodeFiche::TrenteJours);
        let page = PageValo::load(|cle, defaut| {
            match cle {
                "fiche_classe" => "off",
                _ => defaut,
            }
            .to_string()
        });
        assert!(!page.fiche_classe);
        assert_eq!(page.fiche_periode, PeriodeFiche::TrenteJours);
    }

    /// L'onglet et le tri du classement se retrouvent au lancement suivant ;
    /// la carte tactique, qui est une fenêtre, et une clé inconnue rendent
    /// l'accueil.
    #[test]
    fn l_onglet_et_le_tri_se_retrouvent() {
        use centre::OngletCentre;
        for (onglet, _) in OngletCentre::TOUS {
            let attendu = if onglet == OngletCentre::Tactique { OngletCentre::Accueil } else { onglet };
            assert_eq!(OngletCentre::depuis(onglet.cle()), attendu);
        }
        assert_eq!(OngletCentre::depuis("?"), OngletCentre::Accueil);
        let page = PageValo::load(|cle, defaut| {
            match cle {
                "valo_onglet" => "classement",
                "valo_tri" => "kd:desc",
                _ => defaut,
            }
            .to_string()
        });
        assert_eq!(page.onglet_centre, OngletCentre::Classement);
        assert_eq!(classement::ecrire_tri(page.tri_classement), "kd:desc");
    }

    /// Échap sur la page. Une fiche ouverte depuis le classement : on y
    /// retourne, la page reste. Sur l'onglet Profil choisi à la main, ou
    /// sans la page (la fiche ouverte d'un clic droit) : tout se ferme, et
    /// l'onglet d'avant attend la prochaine ouverture.
    #[test]
    fn echap_ramene_a_l_onglet_d_ou_la_fiche_vient() {
        use centre::OngletCentre;
        let mut page = PageValo::load(|_, d| d.to_string());
        page.ouvert = true;
        page.onglet_centre = OngletCentre::Classement;
        page.vers_profil();
        page.vue_profil = profil::VueProfil::Agents;
        // Une seconde fiche par-dessus la première : l'origine ne bouge pas.
        page.vers_profil();
        assert_eq!(page.onglet_a_garder(), OngletCentre::Classement, "le profil ouvert en passant ne se retient pas");
        assert!(page.echap(), "la page reste");
        assert_eq!(page.onglet_centre, OngletCentre::Classement);
        assert_eq!(page.vue_profil, profil::VueProfil::Ensemble);
        assert!(!page.echap(), "plus de fiche : Échap ferme la page");

        // L'onglet Profil cliqué : aucun onglet où revenir.
        page.onglet_centre = OngletCentre::Profil;
        page.retour = None;
        assert!(!page.echap());
        assert_eq!(page.onglet_a_garder(), OngletCentre::Profil);

        // La fiche seule, page fermée : Échap ferme, et l'onglet d'avant
        // revient pour la prochaine ouverture.
        let mut page = PageValo::load(|_, d| d.to_string());
        page.onglet_centre = OngletCentre::Matchs;
        page.vers_profil();
        assert!(!page.echap());
        page.quitter_la_fiche();
        assert_eq!(page.onglet_centre, OngletCentre::Matchs);

        // Le bouton de la barre, la page déjà ouverte : l'accueil, sans
        // retour en attente.
        page.vers_profil();
        page.vers_accueil();
        assert_eq!(page.onglet_centre, OngletCentre::Accueil);
        assert!(page.retour.is_none());
    }

    #[test]
    fn les_nombres_se_disent_sans_diviser_par_zero() {
        assert_eq!(dec(1.236, 2), "1,24");
        assert_eq!(opt_dec(None, 2), TIRET);
        assert_eq!(opt_dec(Some(f32::NAN), 2), TIRET);
        assert_eq!(opt_entier(Some(244.6)), "245");
        assert_eq!(opt_pct(Some(60.4)), "60 %");
        assert_eq!(opt_pct(None), TIRET);
        assert_eq!(signe(18).0, "+18");
        assert_eq!(signe(-3).0, "-3");
        assert_eq!(signe(0).0, "±0");
        assert_eq!(milliers(4_212), "4 212");
        assert_eq!(milliers(999), "999");
        assert_eq!(milliers(1_000_000), "1 000 000");
        assert_eq!(duree_texte(3_700), "1 h");
        assert_eq!(duree_texte(2_700), "45 min");
        assert_eq!(pluriel(1, "partie"), "1 partie");
        assert_eq!(pluriel(3, "partie"), "3 parties");
        assert_eq!(bilan_texte(&Bilan::default()), TIRET);
        // Un match sans manche ni dégât ni détail : que des tirets.
        let m = MatchResume::default();
        assert!(acs_de(&m).is_none());
        assert!(adr_de(&m).is_none());
        assert!(kast_de(&m).is_none());
        let vide = Annuaire::new(&[], &[]);
        assert_eq!(recit_du_match(&m, &vide), "— pas de manches connues");
        assert_eq!(vide.nom(42), "#42");
        assert_eq!(multi_kills(0, 0, 0), "aucun multi-kill");
        assert_eq!(multi_kills(12, 3, 0), "3k×12 · 4k×3");
        assert_eq!(multi_kills(0, 0, 1), "ace×1");
    }

    /// La ligne dépliée dit pourquoi les manches manquent, sans prétendre
    /// qu'un combat à mort tout frais date d'avant 0.1.40.
    #[test]
    fn le_recit_ne_ment_pas_sur_les_manches_absentes() {
        let vide = Annuaire::new(&[], &[]);
        let mut dm = match_de_test("dm", 1_000, "Combat à mort", true, &[]);
        dm.manches = (0, 0);
        dm.duree_s = 540;
        assert_eq!(recit_du_match(&dm, &vide), "— combat à mort : pas de manches · 9 min");
        let mut tdm = match_de_test("tdm", 1_000, "Combat à mort par équipe", true, &[]);
        tdm.manches = (0, 0);
        assert_eq!(recit_du_match(&tdm, &vide), "— combat à mort : pas de manches");
        // Un match à manches sans détail : d'avant 0.1.40, ou rattrapé.
        let sans = match_de_test("a", 1_000, MODE_CLASSE, true, &[]);
        assert!(recit_du_match(&sans, &vide).starts_with("— manches non lues"));
        assert!(!recit_du_match(&sans, &vide).contains("combat"));
        // Avec le détail, plus un mot là-dessus.
        let mut avec = match_de_test("b", 1_000, MODE_CLASSE, true, &[]);
        avec.manches_detail = Some(ki_protocol::DetailManches { manches: 22, kast: 11, aces: 1, ..Default::default() });
        assert_eq!(recit_du_match(&avec, &vide), "KAST 50 % · ace×1");
    }

    #[test]
    fn les_rr_d_une_fenetre_distinguent_rien_de_zero() {
        let points = [point("a", 1_000, "e9a2", 5), point("b", 2_000, "e9a2", -5)];
        // Des points dans la fenêtre qui s'annulent : ±0.
        assert_eq!(rr_texte(0, &points, 500).0, "±0");
        // Aucun point dans la fenêtre : le tiret.
        assert_eq!(rr_texte(0, &points, 5_000).0, TIRET);
        // Un total non nul se montre toujours.
        assert_eq!(rr_texte(12, &points, 5_000).0, "+12");
    }

    #[test]
    fn le_classement_se_trie_sans_perdre_l_ordre_recu() {
        // (a joué ?, clé) ; les sans-clé derrière, les sans-match tout en bas.
        let mut v = vec![("a", true, Some(1.0)), ("b", true, Some(3.0)), ("c", false, Some(9.0)), ("d", true, None), ("e", true, Some(3.0))];
        trier(&mut v, |x| !x.1, |x| x.2, true);
        let noms: Vec<&str> = v.iter().map(|x| x.0).collect();
        assert_eq!(noms, vec!["b", "e", "a", "d", "c"], "décroissant, b avant e à égalité, d sans clé, c sans match");
        trier(&mut v, |x| !x.1, |x| x.2, false);
        let noms: Vec<&str> = v.iter().map(|x| x.0).collect();
        assert_eq!(noms, vec!["a", "b", "e", "d", "c"]);
    }

    #[test]
    fn les_duos_se_fusionnent_par_paire_ordonnee() {
        // 1 dit 11 parties avec 2 ; 2 n'en compte que 9 avec 1 ; 3 et 4 une
        // seule (écartée) ; 5 se cite lui-même (ignoré).
        let d1: Vec<(UserId, u16, u16)> = vec![(2, 11, 8), (3, 1, 1)];
        let d2: Vec<(UserId, u16, u16)> = vec![(1, 9, 7)];
        let d3: Vec<(UserId, u16, u16)> = vec![(4, 1, 0)];
        let d5: Vec<(UserId, u16, u16)> = vec![(5, 4, 4), (2, 3, 1)];
        let paires = paires_de_duos([(1, d1.as_slice()), (2, d2.as_slice()), (3, d3.as_slice()), (5, d5.as_slice())].into_iter());
        assert_eq!(paires, vec![(1, 2, 11, 8), (2, 5, 3, 1)]);
    }

    #[test]
    fn l_activite_tourne_a_l_heure_locale_sans_perdre_de_partie() {
        let mut utc = vec![0u16; 168];
        utc[23] = 2; // lundi 23 h UTC
        utc[6 * 24 + 22] = 3; // dimanche 22 h UTC
        let locale = activite_locale(&utc, 2);
        assert_eq!(locale[24 + 1], 2, "lundi 23 h UTC, c'est mardi 1 h à Paris l'été");
        assert_eq!(locale[0], 3, "dimanche 22 h UTC déborde sur lundi 0 h");
        assert_eq!(locale.iter().map(|&v| u32::from(v)).sum::<u32>(), 5);
        let ouest = activite_locale(&utc, -5);
        assert_eq!(ouest[18], 2);
        assert_eq!(ouest[6 * 24 + 17], 3);
        // Une activité tronquée ou vide ne fait pas tomber la rotation.
        assert_eq!(activite_locale(&[], 1).len(), 168);
        assert_eq!(activite_locale(&[7u16; 10], 0)[5], 7);
    }

    #[test]
    fn le_fil_regroupe_les_matchs_joues_ensemble() {
        let ids = ["m1", "m2", "m1", "", "m3", "", "m1"];
        let blocs = regrouper(&ids);
        assert_eq!(blocs, vec![vec![0, 2, 6], vec![1], vec![3], vec![4], vec![5]]);
        assert!(regrouper(&[]).is_empty());
    }

    #[test]
    fn les_frontieres_d_acte_tombent_entre_deux_points() {
        let a = point("a", 1_000, "e9a2", 5);
        let b = point("b", 2_000, "e9a2", 5);
        let c = point("c", 3_000, "e9a3", 5);
        let d = point("d", 4_000, "", 5);
        let e = point("e", 5_000, "e9a3", 5);
        let f = frontieres_d_acte(&[&a, &b, &c, &d, &e]);
        assert_eq!(f, vec![(3_000, "e9a2 → e9a3".to_string())]);
        assert!(frontieres_d_acte(&[]).is_empty());
    }

    #[test]
    fn le_bilan_local_refait_celui_du_serveur() {
        let maintenant = 1_756_000_000_000_u64;
        let fiche = FicheValorant {
            matchs: vec![
                match_de_test("a", maintenant - JOUR_MS, MODE_CLASSE, true, &[7]),
                match_de_test("b", maintenant - 2 * JOUR_MS, MODE_CLASSE, false, &[7]),
                match_de_test("c", maintenant - 10 * JOUR_MS, MODE_CLASSE, true, &[]),
                match_de_test("d", maintenant - 3 * JOUR_MS, "Spike Rush", true, &[]),
                match_de_test("e", maintenant - 40 * JOUR_MS, MODE_CLASSE, true, &[]),
            ],
            historique_rr: vec![point("a", maintenant - JOUR_MS, "e9a2", 20), point("c", maintenant - 10 * JOUR_MS, "e9a2", -7)],
            ..Default::default()
        };
        let b = bilan_local(&fiche, maintenant);
        assert_eq!(b.sept_jours.matchs, 2);
        assert_eq!(b.sept_jours.rr, 20);
        assert_eq!(b.trente_jours.matchs, 3);
        assert_eq!(b.trente_jours.rr, 13);
        assert_eq!(b.forme, vec![1, -1, 1, 1], "la forme ne connaît pas de fenêtre");
        assert_eq!(b.serie, 1);
        assert_eq!(b.agents, vec![("Jett".to_string(), 3, 2)]);
        assert_eq!(b.duos, vec![(7, 2, 1)]);

        // La fiche, avec ses filtres : « 10 derniers » de tous les modes
        // garde le Spike Rush dans la table, « Compétitif » l'écarte.
        let rangs = rangs::Rangs::new();
        let catalogue = Catalogue::new();
        let photos = HashMap::new();
        let vue = Vue::new("moi", &fiche, PeriodeFiche::Tout, false, &Contexte { stats: &[], recu: true, chargement: crate::valo_donnees::Chargement::Repos, esports: &[], activite: &[], moi: None, membres: &[], rangs: &rangs, catalogue: &catalogue, soiree: &[], soiree_servie: true, ma_fiche: None, photos: &photos, comparee: None, journal: Journal::default() }, maintenant);
        assert_eq!(vue.e.len(), 5);
        assert_eq!(vue.b.matchs, 5, "un Spike Rush a des manches, il compte");
        assert_eq!(vue.e[0].id, "a", "du plus récent au plus ancien");
        let photos = HashMap::new();
        let vue = Vue::new("moi", &fiche, PeriodeFiche::SeptJours, true, &Contexte { stats: &[], recu: true, chargement: crate::valo_donnees::Chargement::Repos, esports: &[], activite: &[], moi: None, membres: &[], rangs: &rangs, catalogue: &catalogue, soiree: &[], soiree_servie: true, ma_fiche: None, photos: &photos, comparee: None, journal: Journal::default() }, maintenant);
        assert_eq!(vue.e.len(), 2);
        assert_eq!(vue.b.rr, 20);
        assert!(vue.point_de("a").is_some());
        assert!(vue.point_de("").is_none());
        assert!(vue.repere(Bilan::kd, 2.0).is_none(), "sans groupe, pas de médiane");
        let agents = vue.ventiler(|m| m.agent.as_str());
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].1.matchs, 2);
        // Les heures : autant de cases que de matchs, jamais plus de 168 cases.
        let cases = cases_des_heures(fiche.matchs.iter().map(|m| m.date));
        assert_eq!(cases.len(), 168);
        assert_eq!(cases.iter().map(|&v| u32::from(v)).sum::<u32>(), 5);
    }

    /// Une fiche pleine : soixante classés détaillés sur deux actes, des
    /// combats à mort, cent points, un pic, des saisons, des co-membres.
    fn fiche_riche(maintenant: u64) -> FicheValorant {
        let mut matchs: Vec<MatchResume> = (0..60u64)
            .map(|i| {
                let mut m = match_de_test(&format!("m{i}"), maintenant - i * 6 * 3_600_000, MODE_CLASSE, i % 3 != 0, &[2]);
                m.contre = vec![3];
                m.degats = 3_000 + i as u32;
                m.degats_recus = 2_800;
                m.tetes = 20;
                m.tirs = 90;
                m.party = 3;
                m.tier = 16;
                m.duree_s = 2_400;
                m.saison = if i < 30 { "e9a3".into() } else { "e9a2".into() };
                m.manches_detail = Some(ki_protocol::DetailManches {
                    manches: 22,
                    kast: 16,
                    premiers_sangs: 3,
                    premieres_morts: 1,
                    triples: 2,
                    quadruples: 1,
                    aces: 0,
                    clutchs_tentes: 2,
                    clutchs: 1,
                    meilleur_clutch: 2,
                    poses: 2,
                    desamorcages: 1,
                    deroule: "VVDVDDVVVVDVVDDVVDVVDV".into(),
                });
                m
            })
            .collect();
        matchs.push(match_de_test("dm", maintenant - 1_000, "Combat à mort", true, &[]));
        matchs.push(MatchResume { manches: (0, 0), gagne: None, ..match_de_test("", maintenant - 2_000, "Spike Rush", true, &[]) });
        let historique_rr: Vec<PointRR> = (0..100u64)
            .map(|i| PointRR {
                carte: "Ascent".into(),
                protege: i == 4,
                ..point(
                    &format!("m{i}"),
                    maintenant - i * 6 * 3_600_000,
                    if i < 30 { "e9a3" } else { "e9a2" },
                    if i % 3 == 0 { -17 } else { 19 },
                )
            })
            .collect();
        FicheValorant {
            riot_id: "redik#EUW".into(),
            region: "eu".into(),
            plateforme: "pc".into(),
            niveau: 212,
            rang: ki_protocol::RangValorant { tier: 16, rr: 57, delta: 18, boucliers: 2, ..Default::default() },
            pic: Some(ki_protocol::RangValorant { tier: 18, rr: 12, saison: "e9a1".into(), ..Default::default() }),
            historique_rr,
            matchs,
            maj: maintenant - 60_000,
            saisons: vec![
                ki_protocol::StatsSaison { saison: "e9a2".into(), victoires: 30, parties: 55, tier_fin: 15, rr_fin: 80 },
                ki_protocol::StatsSaison { saison: "e9a3".into(), victoires: 14, parties: 25, tier_fin: 16, rr_fin: 57 },
            ],
            // Une carte connue du serveur : le bandeau garde sa place même
            // sans image (pas de réseau dans les tests).
            carte_joueur: "9fb348bc-41a0-91ad-8a3e-818035c4e561".into(),
            titre_joueur: String::new(),
            medailles: Some(medailles_de_test(maintenant)),
        }
    }

    /// Les médailles d'un membre : deux MVP cet acte, quatre en carrière,
    /// et celles de ses deux derniers matchs (« m0 », « m1 »).
    fn medailles_de_test(maintenant: u64) -> ki_protocol::Medailles {
        use ki_protocol::{CompteMedaille, Medaille, MedailleGagnee, MedaillesDuMatch};
        let compte = |medaille, fois, meilleur| CompteMedaille { medaille, fois, meilleur };
        let gagnee = |medaille, valeur, record| MedailleGagnee { medaille, valeur, record };
        ki_protocol::Medailles {
            maj: maintenant - 3_600_000,
            acte: "V26 · ACTE V".into(),
            cet_acte: vec![
                compte(Medaille::Mvp, 2, 482.8),
                compte(Medaille::Distinction, 1, 482.8),
                compte(Medaille::PremiersSangs, 5, 6.0),
                compte(Medaille::Degats, 4, 254.19),
            ],
            carriere: vec![compte(Medaille::Mvp, 4, 482.8), compte(Medaille::Aces, 1, 1.0)],
            matchs: vec![
                MedaillesDuMatch {
                    id: "m0".into(),
                    debut: maintenant,
                    medailles: vec![gagnee(Medaille::Mvp, 412.0, true), gagnee(Medaille::TopFrag, 26.0, false)],
                },
                MedaillesDuMatch {
                    id: "m1".into(),
                    debut: maintenant - 6 * 3_600_000,
                    medailles: vec![gagnee(Medaille::PremiersSangs, 4.0, true)],
                },
            ],
        }
    }

    /// Quatre membres : un riche, un qui a joué trois de ses matchs, un
    /// sans rien, un Immortel classé qui a joué seul.
    pub(super) fn groupe(maintenant: u64, vieux_serveur: bool) -> Vec<FicheMembre> {
        let riche = fiche_riche(maintenant);
        let membre = |id: UserId, nom: &str, fiche: FicheValorant| FicheMembre {
            user_id: id,
            username: nom.into(),
            bilan: (!vieux_serveur).then(|| bilan_local(&fiche, maintenant)),
            fiche: fiche.resume(30, 30),
        };
        vec![
            membre(1, "redik", riche.clone()),
            membre(2, "Nono", FicheValorant { riot_id: "Nono#EUW".into(), matchs: riche.matchs[..3].to_vec(), ..riche.clone() }),
            membre(3, "Wam", FicheValorant { riot_id: "Wam#EUW".into(), ..Default::default() }),
            membre(
                4,
                "Bob",
                FicheValorant {
                    riot_id: "Bob#EUW".into(),
                    rang: ki_protocol::RangValorant { tier: 25, rr: 140, classement: 1_284, ..Default::default() },
                    matchs: vec![match_de_test("seul", maintenant - 3_000, MODE_CLASSE, true, &[])],
                    ..Default::default()
                },
            ),
        ]
    }

    pub(super) fn membres() -> Vec<Member> {
        [(1, "redik"), (2, "Nono"), (3, "Wam"), (4, "Bob")]
            .into_iter()
            .map(|(id, nom)| Member {
                user_id: id,
                username: nom.into(),
                speaking: false,
                muted: false,
                streaming: None,
                force_muted: false,
                force_deafened: false,
                admin: false,
                avatar: None,
                voice: None,
                jeu: None,
                riot_id: None,
                rang_valorant: None,
                compte_joue: None,
                roles: Vec::new(),
                online: true,
                color: None,
                rank: 0,
                invite: false,
                mobile: false,
            })
            .collect()
    }

    /// Une image egui sans écran, la souris posée quelque part : ce que
    /// la page dessine ne doit jamais paniquer, quelles que soient les
    /// données — pleines, pauvres, ou d'un serveur d'avant.
    pub(super) fn dessiner(ctx: &egui::Context, souris: Option<Pos2>, mut corps: impl FnMut(&mut egui::Ui)) {
        let mut entree = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1_400.0, 900.0))),
            ..Default::default()
        };
        if let Some(pos) = souris {
            entree.events.push(egui::Event::PointerMoved(pos));
        }
        // Deux passes : la seconde voit les tailles que la première a mesurées.
        for _ in 0..2 {
            ctx.run_ui(entree.clone(), |ui| corps(ui)).drop_without_applying_deltas();
        }
    }

    /// La page au centre, onglet par onglet : sur un serveur d'aujourd'hui
    /// et d'avant, sur les deux périodes, avec ou sans soirée, la souris ici
    /// ou là. Elle reste ouverte et ne demande rien d'elle-même — sauf le
    /// profil sans fiche ouverte, qui réclame la mienne.
    #[test]
    fn la_page_au_centre_se_dessine_onglet_par_onglet() {
        use ki_protocol::JoueurSoiree;
        let maintenant = maintenant_ms();
        let ctx = egui::Context::default();
        let rangs = rangs::Rangs::new();
        let catalogue = Catalogue::new();
        let membres = membres();
        let photos = HashMap::new();
        let mut boutique = boutique::Lecteur::new();
        let activite: Vec<u16> = (0..168u16).map(|i| i % 5).collect();
        let joueur = |user_id, gagne: bool| JoueurSoiree {
            user_id,
            agent: "Jett".into(),
            kills: 18,
            deaths: 12,
            assists: 4,
            score: 5_000,
            manches: if gagne { (13, 7) } else { (7, 13) },
            gagne: Some(gagne),
            deroule: "VVDVVVDVVVVDVVVDVVVV".into(),
            delta_rr: Some(if gagne { 20 } else { -15 }),
            tier_avant: 15,
            tier: if gagne { 16 } else { 15 },
            ..Default::default()
        };
        let soiree = vec![
            PartieSoiree { id: "s2".into(), date: maintenant - 600_000, carte: "Bind".into(), mode: MODE_CLASSE.into(), duree_s: 1_800, joueurs: vec![joueur(1, true), joueur(2, false)] },
            PartieSoiree { id: "s1".into(), date: maintenant - 3_600_000, carte: "".into(), mode: "Vélocité".into(), duree_s: 0, joueurs: vec![joueur(4, true)] },
        ];
        for vieux_serveur in [false, true] {
            let stats = groupe(maintenant, vieux_serveur);
            let mut page = PageValo::load(|_, d| d.to_string());
            for onglet in [
                centre::OngletCentre::Accueil,
                centre::OngletCentre::Classement,
                centre::OngletCentre::Matchs,
                centre::OngletCentre::Esport,
                centre::OngletCentre::Boutique,
            ] {
                page.onglet_centre = onglet;
                for periode in Periode::TOUTES {
                    page.periode = periode;
                    for (soiree, servie) in [(&soiree[..], true), (&[][..], false)] {
                        let cx = Contexte { stats: &stats, recu: true, chargement: crate::valo_donnees::Chargement::Repos, esports: &[], activite: &activite, moi: Some(1), membres: &membres, rangs: &rangs, catalogue: &catalogue, soiree, soiree_servie: servie, ma_fiche: stats.first().map(|f| &f.fiche), photos: &photos, comparee: None, journal: Journal::default() };
                        for souris in [None, Some(Pos2::new(300.0, 300.0)), Some(Pos2::new(600.0, 800.0))] {
                            dessiner(&ctx, souris, |ui| {
                                let (ouvert, demandes) = page.centre(ui, &cx, None, &mut boutique);
                                assert!(ouvert && demandes.is_empty(), "{onglet:?} : {demandes:?}");
                            });
                        }
                    }
                }
            }
            let cx = Contexte { stats: &stats, recu: true, chargement: crate::valo_donnees::Chargement::Repos, esports: &[], activite: &activite, moi: Some(1), membres: &membres, rangs: &rangs, catalogue: &catalogue, soiree: &soiree, soiree_servie: true, ma_fiche: None, photos: &photos, comparee: None, journal: Journal::default() };
            // Les filtres des matchs : un membre, et un mode qui n'est plus
            // dans la période, oublié.
            page.onglet_centre = centre::OngletCentre::Matchs;
            page.filtre_membre = Some(2);
            page.filtre_mode = Some("Inexistant".into());
            page.cartes_matchs = usize::MAX;
            dessiner(&ctx, None, |ui| {
                let _ = page.centre(ui, &cx, None, &mut boutique);
            });
            assert!(page.filtre_mode.is_none(), "un mode absent ne filtre plus rien");
            // Le profil sans fiche ouverte réclame la mienne.
            page.onglet_centre = centre::OngletCentre::Profil;
            dessiner(&ctx, None, |ui| {
                let (_, demandes) = page.centre(ui, &cx, None, &mut boutique);
                assert_eq!(demandes, [Demande::OuvrirFiche(1, "redik".into())]);
            });
            // Avant la réponse du serveur, et sans personne de lié.
            for (stats, recu) in [(&stats[..], false), (&[][..], true)] {
                let cx = Contexte { stats, recu, chargement: crate::valo_donnees::Chargement::Repos, esports: &[], activite: &[], moi: None, membres: &membres, rangs: &rangs, catalogue: &catalogue, soiree: &[], soiree_servie: true, ma_fiche: None, photos: &photos, comparee: None, journal: Journal::default() };
                for onglet in [centre::OngletCentre::Accueil, centre::OngletCentre::Classement, centre::OngletCentre::Matchs] {
                    page.onglet_centre = onglet;
                    dessiner(&ctx, None, |ui| {
                        let (ouvert, _) = page.centre(ui, &cx, None, &mut boutique);
                        assert!(ouvert);
                    });
                }
            }
        }
    }

    #[test]
    fn la_mediane_du_groupe_demande_trois_membres() {
        let maintenant = 1_756_000_000_000_u64;
        let membre = |id: UserId, kills: u16| {
            let matchs: Vec<MatchResume> = (0..6)
                .map(|i| MatchResume { kills, deaths: 10, ..match_de_test(&format!("{id}-{i}"), maintenant - i * JOUR_MS, MODE_CLASSE, true, &[]) })
                .collect();
            FicheMembre {
                user_id: id,
                username: format!("j{id}"),
                fiche: FicheValorant { matchs, ..Default::default() },
                bilan: None,
            }
        };
        let fiche = FicheValorant::default();
        let rangs = rangs::Rangs::new();
        let catalogue = Catalogue::new();
        let deux = [membre(1, 10), membre(2, 20)];
        let photos = HashMap::new();
        let vue = Vue::new("moi", &fiche, PeriodeFiche::Tout, true, &Contexte { stats: &deux, recu: true, chargement: crate::valo_donnees::Chargement::Repos, esports: &[], activite: &[], moi: None, membres: &[], rangs: &rangs, catalogue: &catalogue, soiree: &[], soiree_servie: true, ma_fiche: None, photos: &photos, comparee: None, journal: Journal::default() }, maintenant);
        assert!(vue.repere(Bilan::kd, 1.0).is_none());
        let trois = [membre(1, 10), membre(2, 20), membre(3, 30)];
        let photos = HashMap::new();
        let vue = Vue::new("moi", &fiche, PeriodeFiche::Tout, true, &Contexte { stats: &trois, recu: true, chargement: crate::valo_donnees::Chargement::Repos, esports: &[], activite: &[], moi: None, membres: &[], rangs: &rangs, catalogue: &catalogue, soiree: &[], soiree_servie: true, ma_fiche: None, photos: &photos, comparee: None, journal: Journal::default() }, maintenant);
        let (mediane, max) = vue.repere(Bilan::kd, 5.0).expect("trois membres assez classés");
        assert_eq!(mediane, 2.0);
        assert_eq!(max, 5.0, "le plafond prend en compte la valeur du membre");
        // Le bilan vient d'être refait ici : la page le dit.
        let (_, local) = lignes_du_groupe(&trois, maintenant);
        assert!(local);
    }

    fn partie(file: &str, carte: &str, allie: u8, adverse: u8) -> JeuStatut {
        JeuStatut {
            etat: JeuEtat::EnJeu,
            file: file.into(),
            carte: carte.into(),
            score_allie: allie,
            score_adverse: adverse,
            party_taille: 2,
            party_max: 5,
            rang: 15,
            ..JeuStatut::default()
        }
    }

    /// Ceux qui sont dans la même partie, du même camp, tiennent sur une
    /// ligne ; un adversaire (le score inversé), une file et un menu ont
    /// chacun la leur. Et chaque ligne dit où il en est, sans la carte.
    #[test]
    fn en_direct_regroupe_ceux_qui_jouent_ensemble() {
        let a = partie("competitive", "Ascent", 7, 5);
        let b = partie("competitive", "Ascent", 7, 5);
        let adversaire = partie("competitive", "Ascent", 5, 7);
        let file = JeuStatut { etat: JeuEtat::Menus, file: "competitive".into(), ..JeuStatut::default() };
        let menu = JeuStatut::default();
        let choix = JeuStatut { etat: JeuEtat::PreGame, ..partie("unrated", "Bind", 0, 0) };
        let lignes = regrouper_en_direct(&[&a, &b, &adversaire, &file, &menu, &choix]);
        assert_eq!(lignes, vec![vec![0, 1], vec![2], vec![3], vec![4], vec![5]]);

        let (texte, score) = etat_en_direct(&a);
        assert_eq!(texte, "compétitive · party 2/5");
        assert_eq!(score, Some(("7-5".to_string(), SPEAK)));
        assert_eq!(etat_en_direct(&adversaire).1.map(|(_, c)| c), Some(DANGER));
        assert_eq!(etat_en_direct(&file).0, "en file compétitive");
        assert_eq!(etat_en_direct(&menu).0, "au menu");
        assert_eq!(etat_en_direct(&choix).0, "non classée · sélection des agents · party 2/5");
        let perso = JeuStatut { custom: true, ..partie("", "Ascent", 0, 0) };
        assert!(etat_en_direct(&perso).1.is_none(), "une personnalisée à 0-0 ne dit pas de score");
        assert_eq!([&a, &choix, &file, &menu].map(|j| badge_d_etat(j).0), ["EN PARTIE", "SÉLECTION", "EN FILE", "AU MENU"]);
    }

    /// La page se dessine avec des membres en partie — ensemble, seul, en
    /// file, un autre jeu — et sans catalogue ni image.
    #[test]
    fn en_direct_se_dessine() {
        let maintenant = maintenant_ms();
        let ctx = egui::Context::default();
        let rangs = rangs::Rangs::new();
        let catalogue = Catalogue::new();
        let mut boutique = boutique::Lecteur::new();
        let mut membres = membres();
        membres[0].jeu = Some(partie("competitive", "Ascent", 7, 5));
        membres[0].riot_id = Some("redik#EUW".into());
        membres[1].jeu = Some(partie("competitive", "Ascent", 7, 5));
        membres[2].jeu = Some(JeuStatut { etat: JeuEtat::Menus, file: "abilitydraftarena".into(), ..JeuStatut::default() });
        membres[3].jeu = Some(JeuStatut::autre_jeu("Rocket League"));
        let stats = groupe(maintenant, false);
        let mut page = PageValo::load(|_, d| d.to_string());
        page.ouvert = true;
        // L'accueil de la page : c'est lui qui porte « En direct ».
        for recu in [false, true] {
            for souris in [None, Some(Pos2::new(200.0, 160.0))] {
                dessiner(&ctx, souris, |ui| {
                    page.centre(ui, &Contexte { stats: &stats, recu, chargement: crate::valo_donnees::Chargement::Repos, esports: &[], activite: &[], moi: Some(1), membres: &membres, rangs: &rangs, catalogue: &catalogue, soiree: &[], soiree_servie: true, ma_fiche: None, photos: &HashMap::new(), comparee: None, journal: Journal::default() }, None, &mut boutique);
                });
            }
        }
    }
}
