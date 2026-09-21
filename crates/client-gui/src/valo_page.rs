//! La page VALORANT du groupe et la fiche d'un membre : deux fenêtres sur
//! ce que le serveur sait des fiches HenrikDev, sans jamais solliciter
//! HenrikDev elles-mêmes — ouvrir la page ou une fiche ne coûte aucune
//! requête à personne.
//!
//! Sur le patron du soundboard et de la visionneuse, le module tient son
//! état (onglet, période, tri, filtres, ce qui est déplié) et rend des
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

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{Datelike, TimeZone, Timelike};
use eframe::egui::{self, Color32, Pos2, Rect, Response, RichText, Sense, Ui, Vec2};
use ki_protocol::{
    nom_de_rang, Bilan, BilanMembre, FicheMembre, FicheValorant, MatchEsport, MatchResume, Member,
    PointRR, PositionMmr, UserId,
};

use crate::graphes::{self, Barre, PointCourbe};
use crate::icons;
use crate::theme::{self, ACCENT, DANGER, SPEAK, TEXT, TEXT_DIM, TEXT_FAINT};
use crate::ui;
use crate::{boutique, rangs, FicheOuverte};

// ---------------------------------------------------------------------
// Constantes
// ---------------------------------------------------------------------

const JOUR_MS: u64 = 86_400_000;
/// Autant de classés pour concourir à un record ou peser dans une médiane.
const ASSEZ: u16 = 5;
/// Le nom français du mode classé, tel que le serveur le traduit.
const MODE_CLASSE: &str = "Compétitif";
/// Le fil des matchs du groupe s'arrête là, puis « voir plus ».
const LIGNES_FIL: usize = 40;
/// La table des matchs d'une fiche s'arrête là, puis « voir les N autres ».
const LIGNES_TABLE: usize = 20;
/// La largeur d'une tuile de record (page du groupe) et d'une tuile de
/// la fiche ; les marges intérieures des cadres sont dedans.
const TUILE_GROUPE: Vec2 = Vec2::new(150.0, 80.0);
const TUILE_FICHE: f32 = 140.0;
const MARGE_TUILE: f32 = 12.0;
/// La ligne de matchs de la page groupe se déroule sur cette hauteur.
const HAUTEUR_COURBE: f32 = 120.0;
const JOURS_LONGS: [&str; 7] = ["lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche"];
const TIRET: &str = "—";

// ---------------------------------------------------------------------
// Les choix de la page
// ---------------------------------------------------------------------

/// Les quatre onglets de la page du groupe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Onglet {
    Groupe,
    Matchs,
    Esport,
    Boutique,
}

impl Onglet {
    pub const TOUS: [Onglet; 4] = [Onglet::Groupe, Onglet::Matchs, Onglet::Esport, Onglet::Boutique];

    pub fn label(self) -> &'static str {
        match self {
            Onglet::Groupe => "Groupe",
            Onglet::Matchs => "Matchs",
            Onglet::Esport => "Esport",
            Onglet::Boutique => "Boutique",
        }
    }

    /// La clé mémorisée : l'onglet ouvert revient à la session suivante.
    pub fn cle(self) -> &'static str {
        match self {
            Onglet::Groupe => "groupe",
            Onglet::Matchs => "matchs",
            Onglet::Esport => "esport",
            Onglet::Boutique => "boutique",
        }
    }

    pub fn depuis(cle: &str) -> Self {
        Self::TOUS.into_iter().find(|o| o.cle() == cle).unwrap_or(Onglet::Groupe)
    }
}

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

/// La colonne qui trie le classement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tri {
    Rang,
    Rr7,
    Rr30,
    Kd,
    Acs,
    Adr,
    Kast,
    Matchs,
}

/// Ce que la page demande à l'application, qui tient la connexion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Demande {
    Actualiser,
    OuvrirFiche(UserId, String),
}

// ---------------------------------------------------------------------
// L'état
// ---------------------------------------------------------------------

/// L'état des deux fenêtres : ce qui se mémorise d'une session à l'autre
/// (onglet, périodes, mode) et ce qui ne dure que la fenêtre (tri,
/// filtres, ligne dépliée, point survolé).
pub struct PageValo {
    pub ouvert: bool,
    onglet: Onglet,
    periode: Periode,
    tri: Tri,
    tri_desc: bool,
    /// Le fil des matchs : un mode, un membre — ou tout.
    filtre_mode: Option<String>,
    filtre_membre: Option<UserId>,
    /// « voir plus » a été cliqué : tout le fil.
    plus: bool,
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
}

impl PageValo {
    /// Clés mémorisées : `valo_onglet`, `valo_periode`, `fiche_periode`,
    /// `fiche_classe`.
    pub fn load(get: impl Fn(&str, &str) -> String) -> Self {
        Self {
            ouvert: false,
            onglet: Onglet::depuis(&get("valo_onglet", "groupe")),
            periode: Periode::depuis(&get("valo_periode", "30")),
            tri: Tri::Rang,
            tri_desc: true,
            filtre_mode: None,
            filtre_membre: None,
            plus: false,
            fiche_periode: PeriodeFiche::depuis(&get("fiche_periode", "30")),
            fiche_classe: get("fiche_classe", "on") != "off",
            deplie: None,
            survol: None,
            fiche_plus: false,
        }
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        storage.set_string("valo_onglet", self.onglet.cle().into());
        storage.set_string("valo_periode", self.periode.cle().into());
        storage.set_string("fiche_periode", self.fiche_periode.cle().into());
        storage.set_string("fiche_classe", if self.fiche_classe { "on" } else { "off" }.into());
    }

    /// La fiche se ferme : rien de déplié, rien de survolé, table repliée.
    pub fn fermer_fiche(&mut self) {
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

/// La taille au-delà de laquelle une fenêtre ne va pas : l'écran, moins
/// une marge pour garder la poignée et la croix à portée.
fn bornes_de_fenetre(ctx: &egui::Context) -> (f32, f32) {
    let ecran = ctx.screen_rect();
    ((ecran.width() - 40.0).max(360.0), (ecran.height() - 40.0).max(300.0))
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
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::same(MARGE_TUILE as i8))
        .show(ui, |ui| {
            // Le cadre hérite de la disposition du parent — une ligne qui
            // replie, quand les tuiles sont côte à côte — et sans ceci le
            // titre, la valeur et la sous-ligne partiraient en escalier :
            // dedans, on empile toujours de haut en bas.
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_min_size(Vec2::new(interieur, hauteur - 2.0 * MARGE_TUILE));
                ui.set_max_width(interieur);
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add(egui::Label::new(RichText::new(t.titre).color(TEXT_FAINT).size(11.0)).truncate());
                ui.horizontal(|ui| {
                    if let Some(icone) = t.icone {
                        ui.add(egui::Image::new(icone).fit_to_exact_size(Vec2::splat(18.0)));
                    }
                    ui.add(egui::Label::new(RichText::new(&t.valeur).color(t.teinte).strong().size(19.0)).truncate());
                });
                if !t.sous.is_empty() {
                    ui.add(egui::Label::new(RichText::new(&t.sous).color(TEXT_DIM).size(11.5)).truncate());
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

/// Un petit triangle : vers le bas (déplié, ou tri décroissant) ou vers
/// la droite / le haut. Peint, pour ne pas dépendre d'une police.
fn triangle(ui: &mut Ui, sens: Sens, couleur: Color32) -> Response {
    let (rect, reponse) = ui.allocate_exact_size(Vec2::splat(9.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        let r = rect.shrink(1.5);
        let points = match sens {
            Sens::Bas => vec![r.left_top(), r.right_top(), r.center_bottom()],
            Sens::Haut => vec![r.left_bottom(), r.right_bottom(), r.center_top()],
            Sens::Droite => vec![r.left_top(), r.left_bottom(), r.right_center()],
        };
        ui.painter().add(egui::Shape::convex_polygon(points, couleur, egui::Stroke::NONE));
    }
    reponse
}

#[derive(Clone, Copy)]
enum Sens {
    Bas,
    Haut,
    Droite,
}

/// Les pastilles des co-membres d'un match : un point à la couleur de
/// chacun dans son camp, cerclé de rouge pour ceux d'en face ; leurs noms
/// au survol. Rien n'est alloué sans personne.
fn pastilles(ui: &mut Ui, avec: &[UserId], contre: &[UserId], noms: &Annuaire) -> Option<Response> {
    let n = avec.len() + contre.len();
    if n == 0 {
        return None;
    }
    let pas = 11.0;
    let (rect, reponse) = ui.allocate_exact_size(Vec2::new(pas * n as f32, 12.0), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return Some(reponse);
    }
    let painter = ui.painter();
    for (i, id) in avec.iter().chain(contre.iter()).enumerate() {
        let centre = Pos2::new(rect.left() + pas * i as f32 + 5.0, rect.center().y);
        icons::dot(painter, centre, 4.0, noms.couleur(*id));
        if i >= avec.len() {
            painter.circle_stroke(centre, 5.0, egui::Stroke::new(1.0_f32, DANGER));
        }
    }
    let mut texte = String::new();
    if !avec.is_empty() {
        texte.push_str(&format!("avec {}", noms.noms(avec)));
    }
    if !contre.is_empty() {
        if !texte.is_empty() {
            texte.push_str(" · ");
        }
        texte.push_str(&format!("contre {}", noms.noms(contre)));
    }
    Some(reponse.on_hover_text(texte))
}

/// Un texte de cellule, en 11,5 — la taille des tables de la page.
fn cellule(ui: &mut Ui, texte: impl Into<String>, couleur: Color32) -> Response {
    ui.label(RichText::new(texte.into()).color(couleur).size(11.5))
}

/// L'en-tête d'une colonne, en petit et pâle.
fn en_tete(ui: &mut Ui, titre: &str) {
    ui.label(RichText::new(titre).color(TEXT_FAINT).size(11.0));
}

/// Le rang en une ligne : l'icône du palier si elle est là, puis le nom à
/// sa couleur — « Non classé » en pâle.
fn rang_court(ui: &mut Ui, tier: u8, rr: u16, taille: f32, rangs: &rangs::Rangs) {
    if let Some(icone) = rangs.texture(tier).filter(|_| tier >= 3) {
        ui.add(egui::Image::new(icone).fit_to_exact_size(Vec2::splat(taille)));
    }
    if tier >= 3 {
        ui.label(RichText::new(nom_de_rang(tier)).color(graphes::couleur_de_rang(tier)).strong().size(11.5));
        ui.label(RichText::new(format!("{rr} RR")).color(TEXT_DIM).size(11.0));
    } else {
        ui.label(RichText::new(nom_de_rang(0)).color(TEXT_FAINT).size(11.5));
    }
}

// ---------------------------------------------------------------------
// La page du groupe
// ---------------------------------------------------------------------

impl PageValo {
    /// La fenêtre du groupe. `stats`, `recu`, `esports` et `activite`
    /// viennent de l'application, telles que le serveur les a envoyées ;
    /// la boutique est prêtée le temps de l'onglet. Rend ce que la page
    /// demande : actualiser, ouvrir une fiche.
    #[allow(clippy::too_many_arguments)]
    pub fn fenetre(
        &mut self,
        ctx: &egui::Context,
        stats: &[FicheMembre],
        recu: bool,
        esports: &[MatchEsport],
        activite: &[u16],
        my_id: Option<UserId>,
        membres: &[Member],
        rangs: &rangs::Rangs,
        boutique: &mut boutique::Lecteur,
    ) -> Vec<Demande> {
        let mut demandes = Vec::new();
        if !self.ouvert {
            return demandes;
        }
        let mut open = true;
        let roomy = (ctx.screen_rect().height() - 120.0).clamp(360.0, 780.0);
        let maintenant = maintenant_ms();
        let (lignes, local) = lignes_du_groupe(stats, maintenant);
        let noms = Annuaire::new(stats, membres);
        // La taille se mémorise d'une session à l'autre ; l'identifiant
        // change avec la mise en page pour oublier ce qu'une version
        // d'avant avait retenu. Et jamais plus large que l'écran : une
        // fenêtre egui ne rétrécit pas d'elle-même, et sa poignée hors
        // écran ne se rattrape plus.
        let (max_l, max_h) = bornes_de_fenetre(ctx);
        egui::Window::new("VALORANT")
            .id(egui::Id::new("valo_page_v5"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(860.0)
            .default_height(roomy)
            .min_width(620.0_f32.min(max_l))
            .max_width(max_l)
            .max_height(max_h)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Le groupe sur VALORANT").strong().size(17.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui::button(ui, icons::Icon::Refresh, "Actualiser").clicked() {
                            demandes.push(Demande::Actualiser);
                        }
                        let n = stats.len();
                        let s = if n > 1 { "s" } else { "" };
                        ui.label(RichText::new(format!("{n} joueur{s} lié{s}")).color(TEXT_FAINT).size(11.5));
                    });
                });
                ui::hint(
                    ui,
                    "d'après les fiches que le serveur tient à jour par HenrikDev — la ligne de \
                     chacun dans ses matchs, jamais celles des adversaires",
                );
                ui.add_space(6.0);
                // Les onglets à gauche, la période à droite — elle vaut
                // pour les agrégats du groupe et l'en-tête du fil.
                ui.horizontal(|ui| {
                    for o in Onglet::TOUS {
                        if ui.selectable_label(self.onglet == o, o.label()).clicked() {
                            self.onglet = o;
                        }
                    }
                    if matches!(self.onglet, Onglet::Groupe | Onglet::Matchs) {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            // De droite à gauche : « 30 j » d'abord, « 7 j » à sa gauche.
                            for p in Periode::TOUTES.into_iter().rev() {
                                if ui.selectable_label(self.periode == p, p.label()).clicked() {
                                    self.periode = p;
                                }
                            }
                        });
                    }
                });
                ui.add_space(8.0);
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.onglet {
                    Onglet::Groupe | Onglet::Matchs => {
                        if !recu {
                            ui.label(RichText::new("demande au serveur…").color(TEXT_DIM));
                            return;
                        }
                        if stats.is_empty() {
                            ui.label(
                                RichText::new("Personne n'a encore lié de compte Riot — ⚙ → Jeu.").color(TEXT_DIM),
                            );
                            return;
                        }
                        if local {
                            ui::hint(ui, "serveur d'avant : statistiques sur les cinq derniers matchs");
                            ui.add_space(4.0);
                        }
                        if self.onglet == Onglet::Groupe {
                            self.onglet_groupe(ui, &lignes, &noms, activite, my_id, rangs, &mut demandes);
                        } else {
                            self.onglet_matchs(ui, &lignes, &noms, my_id, maintenant, &mut demandes);
                        }
                    }
                    Onglet::Esport => esports_ui(ui, esports),
                    Onglet::Boutique => {
                        ui.label(RichText::new("Ma boutique du jour").strong().size(13.5));
                        ui::hint(
                            ui,
                            "lue dans ton client Riot, sur ce PC, pour toi seul — rien ne part vers le \
                             serveur ; il faut VALORANT ouvert",
                        );
                        ui.add_space(4.0);
                        boutique.ui(ui);
                    }
                });
            });
        if !open {
            self.ouvert = false;
            self.plus = false;
        }
        demandes
    }

    /// L'onglet Groupe : les records, le classement, les duos, et quand
    /// le groupe joue.
    #[allow(clippy::too_many_arguments)]
    fn onglet_groupe(
        &mut self,
        ui: &mut Ui,
        lignes: &[Ligne],
        noms: &Annuaire,
        activite: &[u16],
        my_id: Option<UserId>,
        rangs: &rangs::Rangs,
        demandes: &mut Vec<Demande>,
    ) {
        records(ui, lignes, self.periode, rangs);
        ui.add_space(14.0);
        self.classement(ui, lignes, noms, my_id, rangs, demandes);
        duos(ui, lignes, noms);
        quand_le_groupe_joue(ui, activite);
    }

    /// Le classement : une ligne par membre lié, triable par ses en-têtes.
    fn classement(
        &mut self,
        ui: &mut Ui,
        lignes: &[Ligne],
        noms: &Annuaire,
        my_id: Option<UserId>,
        rangs: &rangs::Rangs,
        demandes: &mut Vec<Demande>,
    ) {
        let periode = self.periode;
        let maintenant = maintenant_ms();
        let (sept, trente) = (maintenant.saturating_sub(7 * JOUR_MS), maintenant.saturating_sub(30 * JOUR_MS));
        // Sous 700 px, trois colonnes s'effacent ; la grille défile de
        // toute façon.
        let etroit = ui.available_width() < 700.0;
        let mut ordre: Vec<&Ligne> = lignes.iter().collect();
        trier(&mut ordre, |l| l.sans_match(periode), |l| cle_de_tri(l, self.tri, periode), self.tri_desc);
        let (tri, desc) = (self.tri, self.tri_desc);
        let mut nouveau_tri: Option<Tri> = None;

        ui.label(RichText::new("Classement").strong().size(13.5));
        ui.add_space(4.0);
        egui::ScrollArea::horizontal().id_salt("valo_classement").show(ui, |ui| {
            egui::Grid::new("stats_classement").striped(true).spacing([14.0, 6.0]).show(ui, |ui| {
                for (titre, cle, masquable) in COLONNES {
                    if masquable && etroit {
                        continue;
                    }
                    if en_tete_triable(ui, titre, cle, tri, desc) {
                        nouveau_tri = cle;
                    }
                }
                ui.end_row();
                for (i, l) in ordre.iter().enumerate() {
                    let f = l.f;
                    let b = l.b(periode);
                    let moi = my_id == Some(f.user_id);
                    let pale = l.sans_match(periode);
                    let texte = if pale { TEXT_FAINT } else { TEXT };
                    let dim = if pale { TEXT_FAINT } else { TEXT_DIM };

                    // #
                    ui.label(RichText::new(format!("{}", i + 1)).color(if moi { ACCENT } else { TEXT_FAINT }));
                    // Joueur — cliquable, sa couleur, en gras pour soi.
                    let couleur = if pale { noms.couleur(f.user_id).gamma_multiply(0.6) } else { noms.couleur(f.user_id) };
                    let mut pseudo = RichText::new(&f.username).color(couleur);
                    if moi {
                        pseudo = pseudo.strong();
                    }
                    let mut survol = f.fiche.riot_id.clone();
                    if f.fiche.niveau > 0 {
                        survol.push_str(&format!(" · niveau {}", f.fiche.niveau));
                    }
                    if let Some(pic) = f.fiche.pic.as_ref().filter(|p| p.tier >= 3) {
                        survol.push_str(&format!(" · pic {}", nom_de_rang(pic.tier)));
                    }
                    if ui.add(egui::Label::new(pseudo).sense(Sense::click())).on_hover_text(survol).clicked() {
                        demandes.push(Demande::OuvrirFiche(f.user_id, f.username.clone()));
                    }
                    // Rang : icône, nom, RR, et la jauge vers le palier suivant.
                    let r = &f.fiche.rang;
                    ui.horizontal(|ui| {
                        rang_court(ui, r.tier, r.rr, 18.0, rangs);
                        if (3..24).contains(&r.tier) {
                            graphes::jauge(ui, f32::from(r.rr) / 100.0, None, 40.0, graphes::couleur_de_rang(r.tier));
                        }
                    });
                    // Tendance : les points reçus, du plus ancien au plus récent.
                    let mut points: Vec<&PointRR> = f.fiche.historique_rr.iter().collect();
                    points.sort_by_key(|p| p.date);
                    if points.len() >= 2 {
                        let valeurs: Vec<f32> = points.iter().map(|p| graphes::valeur_rr(p.tier, p.rr)).collect();
                        // La teinte suit la somme des deltas, comme
                        // l'infobulle — pas la pente entre le premier et
                        // le dernier point, qui ignore le gain du premier.
                        let total = points.iter().fold(0i32, |acc, p| acc.saturating_add(p.delta));
                        let (somme, teinte) = signe(total);
                        graphes::sparkline(ui, &valeurs, Vec2::new(64.0, 16.0), teinte).on_hover_text(format!(
                            "{somme} RR sur {}",
                            pluriel(points.len() as u32, "classé")
                        ));
                    } else {
                        cellule(ui, TIRET, TEXT_FAINT);
                    }
                    // 7 j et 30 j.
                    let (t, c) = rr_texte(l.bilan.sept_jours.rr, &f.fiche.historique_rr, sept);
                    cellule(ui, t, c);
                    let (t, c) = rr_texte(l.bilan.trente_jours.rr, &f.fiche.historique_rr, trente);
                    cellule(ui, t, c);
                    // Forme.
                    if l.bilan.forme.is_empty() {
                        cellule(ui, TIRET, TEXT_FAINT);
                    } else {
                        graphes::bande_forme(ui, &l.bilan.forme, Vec2::new(7.0, 10.0), None)
                            .on_hover_text("les dix derniers classés, le plus récent à droite");
                    }
                    // Bilan, K/D, ACS, ADR, KAST, Tête, Matchs.
                    cellule(ui, bilan_texte(b), texte);
                    cellule(ui, opt_dec(b.kd(), 2), texte);
                    cellule(ui, opt_entier(b.acs()), texte);
                    if !etroit {
                        cellule(ui, opt_entier(b.adr()), texte);
                        cellule(ui, opt_pct(b.kast_pct()), texte);
                        cellule(ui, opt_pct(b.tete_pct()), texte);
                    }
                    let matchs = if b.matchs > 0 { format!("{}", b.matchs) } else { TIRET.to_string() };
                    cellule(ui, matchs, dim);
                    if ui.add(egui::Button::new(RichText::new("fiche").size(11.0)).small()).clicked() {
                        demandes.push(Demande::OuvrirFiche(f.user_id, f.username.clone()));
                    }
                    ui.end_row();
                }
            });
        });
        if let Some(t) = nouveau_tri {
            if t == self.tri {
                self.tri_desc = !self.tri_desc;
            } else {
                self.tri = t;
                self.tri_desc = true;
            }
        }
    }

    /// L'onglet Matchs : la somme de la période, deux filtres, et le fil
    /// de tout le groupe — les matchs joués ensemble regroupés.
    fn onglet_matchs(
        &mut self,
        ui: &mut Ui,
        lignes: &[Ligne],
        noms: &Annuaire,
        my_id: Option<UserId>,
        maintenant: u64,
        demandes: &mut Vec<Demande>,
    ) {
        let periode = self.periode;
        let depuis = maintenant.saturating_sub(periode.jours() * JOUR_MS);

        // La somme des classés de tout le monde sur la période.
        let mut total = Bilan::default();
        for l in lignes {
            let b = l.b(periode);
            total.matchs = total.matchs.saturating_add(b.matchs);
            total.victoires = total.victoires.saturating_add(b.victoires);
            total.defaites = total.defaites.saturating_add(b.defaites);
            total.duree_s = total.duree_s.saturating_add(b.duree_s);
        }
        if total.matchs > 0 {
            let nuls = total.matchs.saturating_sub(total.victoires).saturating_sub(total.defaites);
            let mut texte = format!(
                "{} · {} V / {} D / {}",
                pluriel(u32::from(total.matchs), "classé"),
                total.victoires,
                total.defaites,
                pluriel(u32::from(nuls), "nul")
            );
            if total.duree_s > 0 {
                texte.push_str(&format!(" · {}", duree_texte(total.duree_s)));
            }
            ui.label(RichText::new(texte).strong().size(13.0));
            graphes::barre_vd(
                ui,
                u32::from(total.victoires),
                u32::from(total.defaites),
                u32::from(nuls),
                ui.available_width(),
            )
            .on_hover_text(format!("le groupe sur {}, en classé", periode.titre()));
            ui.add_space(8.0);
        }

        // Tout ce qu'on sait, sur la période, du plus récent au plus ancien.
        let modes: BTreeSet<&str> = lignes
            .iter()
            .flat_map(|l| l.f.fiche.matchs.iter())
            .filter(|m| m.date >= depuis && !m.mode.is_empty())
            .map(|m| m.mode.as_str())
            .collect();
        ui.horizontal(|ui| {
            let mode = self.filtre_mode.clone().unwrap_or_else(|| "tous".to_string());
            egui::ComboBox::from_id_salt("valo_fil_mode").selected_text(mode).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.filtre_mode, None, "tous");
                for m in &modes {
                    ui.selectable_value(&mut self.filtre_mode, Some(m.to_string()), *m);
                }
            });
            let membre = self.filtre_membre.map(|id| noms.nom(id)).unwrap_or_else(|| "tout le monde".to_string());
            egui::ComboBox::from_id_salt("valo_fil_membre").selected_text(membre).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.filtre_membre, None, "tout le monde");
                for l in lignes {
                    ui.selectable_value(&mut self.filtre_membre, Some(l.f.user_id), &l.f.username);
                }
            });
        });
        ui.add_space(6.0);

        let mut tous: Vec<(UserId, &MatchResume)> = lignes
            .iter()
            .flat_map(|l| l.f.fiche.matchs.iter().map(move |m| (l.f.user_id, m)))
            .filter(|(id, m)| {
                m.date >= depuis
                    && self.filtre_mode.as_ref().is_none_or(|mode| &m.mode == mode)
                    && self.filtre_membre.is_none_or(|qui| qui == *id)
            })
            .collect();
        tous.sort_by_key(|(_, m)| std::cmp::Reverse(m.date));
        let ids: Vec<&str> = tous.iter().map(|(_, m)| m.id.as_str()).collect();
        let blocs = regrouper(&ids);
        if blocs.is_empty() {
            ui.label(RichText::new("Aucun match connu sur cette période.").color(TEXT_DIM));
            return;
        }
        // Le point de RR de chaque membre pour chaque match, pour la
        // colonne ΔRR.
        let points: HashMap<(UserId, &str), &PointRR> = lignes
            .iter()
            .flat_map(|l| l.f.fiche.historique_rr.iter().map(move |p| ((l.f.user_id, p.match_id.as_str()), p)))
            .filter(|((_, id), _)| !id.is_empty())
            .collect();
        let limite = if self.plus { usize::MAX } else { LIGNES_FIL };
        let mut lignes_montrees = 0usize;
        let mut tronque = false;

        egui::ScrollArea::horizontal().id_salt("valo_fil").show(ui, |ui| {
            egui::Grid::new("stats_matchs").striped(true).spacing([12.0, 5.0]).show(ui, |ui| {
                for titre in
                    ["Quand", "Joueur", "Mode", "Carte", "Agent", "K / D / A", "ACS", "ADR", "KAST", "FK", "Score", "ΔRR", "Avec"]
                {
                    en_tete(ui, titre);
                }
                ui.end_row();
                for bloc in &blocs {
                    if lignes_montrees >= limite {
                        tronque = true;
                        break;
                    }
                    lignes_montrees += bloc.len();
                    let ensemble = bloc.len() > 1;
                    if ensemble {
                        // L'en-tête du bloc : quand, ENSEMBLE, le mode, la
                        // carte et le score du premier — chacun a le sien
                        // sur sa ligne.
                        let (_, m) = tous[bloc[0]];
                        cellule(ui, crate::il_y_a(m.date), TEXT_FAINT);
                        ui.label(RichText::new(format!("ENSEMBLE ×{}", bloc.len())).color(ACCENT).strong().size(11.0));
                        cellule(ui, &m.mode, TEXT);
                        cellule(ui, &m.carte, TEXT);
                        for _ in 0..6 {
                            ui.label("");
                        }
                        let score = RichText::new(format!("{}-{}", m.manches.0, m.manches.1))
                            .color(teinte_de_score(m.gagne))
                            .strong()
                            .size(11.5);
                        ui.label(score);
                        ui.end_row();
                    }
                    for &i in bloc {
                        let (qui, m) = tous[i];
                        let point = points.get(&(qui, m.id.as_str())).copied();
                        ligne_du_fil(ui, qui, m, ensemble, point, noms, my_id, demandes);
                    }
                }
            });
        });
        if tronque && ui.button("voir plus").clicked() {
            self.plus = true;
        }
    }
}

/// Les colonnes du classement : le titre, la clé de tri s'il y en a une,
/// et si la colonne s'efface dans une fenêtre étroite.
const COLONNES: [(&str, Option<Tri>, bool); 15] = [
    ("#", None, false),
    ("Joueur", None, false),
    ("Rang", Some(Tri::Rang), false),
    ("Tendance", None, false),
    ("7 j", Some(Tri::Rr7), false),
    ("30 j", Some(Tri::Rr30), false),
    ("Forme", None, false),
    ("Bilan", None, false),
    ("K/D", Some(Tri::Kd), false),
    ("ACS", Some(Tri::Acs), false),
    ("ADR", Some(Tri::Adr), true),
    ("KAST", Some(Tri::Kast), true),
    ("Tête", None, true),
    ("Matchs", Some(Tri::Matchs), false),
    ("", None, false),
];

/// L'en-tête d'une colonne du classement : cliquable quand elle trie, la
/// flèche du sens sur la colonne active. Rend `true` au clic.
fn en_tete_triable(ui: &mut Ui, titre: &str, cle: Option<Tri>, actif: Tri, desc: bool) -> bool {
    let Some(t) = cle else {
        en_tete(ui, titre);
        return false;
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let couleur = if actif == t { TEXT } else { TEXT_FAINT };
        let r = ui
            .add(egui::Label::new(RichText::new(titre).color(couleur).size(11.0)).sense(Sense::click()))
            .on_hover_text("trier par cette colonne");
        if actif == t {
            triangle(ui, if desc { Sens::Bas } else { Sens::Haut }, TEXT_DIM);
        }
        r.clicked()
    })
    .inner
}

/// La valeur qui trie une ligne ; `None` quand elle n'en a pas (un taux
/// sans match), et la ligne passe derrière celles qui en ont.
fn cle_de_tri(l: &Ligne, tri: Tri, periode: Periode) -> Option<f64> {
    let b = periode.bilan(&l.bilan);
    let r = &l.f.fiche.rang;
    match tri {
        Tri::Rang => Some(f64::from(r.tier) * 1000.0 + f64::from(r.rr)),
        Tri::Rr7 => Some(f64::from(l.bilan.sept_jours.rr)),
        Tri::Rr30 => Some(f64::from(l.bilan.trente_jours.rr)),
        Tri::Kd => b.kd().map(f64::from),
        Tri::Acs => b.acs().map(f64::from),
        Tri::Adr => b.adr().map(f64::from),
        Tri::Kast => b.kast_pct().map(f64::from),
        Tri::Matchs => Some(f64::from(b.matchs)),
    }
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

/// Les paires qui jouent ensemble, d'après les duos de chacun : clé
/// ordonnée, le plus grand des deux sens gagne, deux parties au moins,
/// les plus assidues d'abord. Rend `(a, b, parties, victoires)`.
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

/// La section des duos — absente sans paire.
fn duos(ui: &mut Ui, lignes: &[Ligne], noms: &Annuaire) {
    let paires = paires_de_duos(lignes.iter().map(|l| (l.f.user_id, l.bilan.duos.as_slice())));
    if paires.is_empty() {
        return;
    }
    ui.add_space(14.0);
    ui::section_label(ui, "Duos");
    ui::hint(ui, "les parties jouées dans le même camp sur 30 jours");
    for (a, b, parties, victoires) in paires {
        let pct = if parties > 0 { format!(" ({} %)", u32::from(victoires) * 100 / u32::from(parties)) } else { String::new() };
        ui.horizontal(|ui| {
            ui.label(RichText::new(noms.nom(a)).color(noms.couleur(a)).strong().size(12.5));
            ui.label(RichText::new("&").color(TEXT_FAINT).size(12.5));
            ui.label(RichText::new(noms.nom(b)).color(noms.couleur(b)).strong().size(12.5));
            ui.label(
                RichText::new(format!("— {} · {} V{pct}", pluriel(u32::from(parties), "partie"), victoires))
                    .color(TEXT_DIM)
                    .size(12.0),
            );
        });
    }
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

/// La heatmap des parties du groupe — absente sans activité.
fn quand_le_groupe_joue(ui: &mut Ui, activite: &[u16]) {
    if activite.is_empty() || activite.iter().all(|&v| v == 0) {
        return;
    }
    let decalage = chrono::Local::now().offset().local_minus_utc() / 3600;
    let locale = activite_locale(activite, decalage);
    ui.add_space(14.0);
    ui::section_label(ui, "Quand le groupe joue");
    ui::hint(ui, "les parties commencées sur 30 jours, tous modes, à l'heure locale");
    graphes::heatmap_semaine(ui, &locale, |jour, heure| {
        let n = locale.get(jour * 24 + heure).copied().unwrap_or(0);
        format!(
            "{} {heure}h–{}h · {} · heure locale, à une heure près au changement d'heure",
            JOURS_LONGS.get(jour).copied().unwrap_or("?"),
            heure + 1,
            pluriel(u32::from(n), "partie")
        )
    });
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

/// Une ligne du fil des matchs du groupe.
#[allow(clippy::too_many_arguments)]
fn ligne_du_fil(
    ui: &mut Ui,
    qui: UserId,
    m: &MatchResume,
    dans_bloc: bool,
    point: Option<&PointRR>,
    noms: &Annuaire,
    my_id: Option<UserId>,
    demandes: &mut Vec<Demande>,
) {
    // Dans un bloc, le quand, le mode et la carte sont sur l'en-tête.
    if dans_bloc {
        ui.label("");
    } else {
        cellule(ui, crate::il_y_a(m.date), TEXT_FAINT);
    }
    let mut pseudo = RichText::new(format!("{}{}", if dans_bloc { "    " } else { "" }, noms.nom(qui)))
        .color(noms.couleur(qui))
        .size(11.5);
    if my_id == Some(qui) {
        pseudo = pseudo.strong();
    }
    if ui.add(egui::Label::new(pseudo).sense(Sense::click())).on_hover_text("ouvrir sa fiche").clicked() {
        demandes.push(Demande::OuvrirFiche(qui, noms.nom(qui)));
    }
    if dans_bloc {
        ui.label("");
        ui.label("");
    } else {
        cellule(ui, &m.mode, TEXT);
        cellule(ui, &m.carte, TEXT);
    }
    cellule(ui, &m.agent, TEXT_DIM);
    cellule(ui, format!("{} / {} / {}", m.kills, m.deaths, m.assists), TEXT);
    cellule(ui, opt_entier(acs_de(m)), TEXT_DIM);
    cellule(ui, opt_entier(adr_de(m)), TEXT_DIM);
    cellule(ui, opt_pct(kast_de(m)), TEXT_DIM);
    let fk = m.manches_detail.as_ref().map(|d| d.premiers_sangs.to_string()).unwrap_or_else(|| TIRET.to_string());
    cellule(ui, fk, TEXT_DIM);
    ui.label(
        RichText::new(format!("{}-{}", m.manches.0, m.manches.1)).color(teinte_de_score(m.gagne)).strong().size(11.5),
    );
    match point {
        Some(p) => {
            let (t, c) = signe(p.delta);
            cellule(ui, t, c);
        }
        None => {
            ui.label("");
        }
    }
    if pastilles(ui, &m.avec, &m.contre, noms).is_none() {
        ui.label("");
    }
    ui.end_row();
}

/// Les prochains matchs d'esport, tels que le serveur les a lus chez
/// HenrikDev — un mot si le serveur n'en a pas.
fn esports_ui(ui: &mut Ui, matchs: &[MatchEsport]) {
    ui.label(RichText::new("Esports — prochains matchs").strong().size(13.5));
    ui.add_space(4.0);
    if matchs.is_empty() {
        ui.label(RichText::new("rien de prévu chez HenrikDev pour l'instant").color(TEXT_DIM));
        return;
    }
    egui::Grid::new("stats_esports").striped(true).spacing([16.0, 5.0]).show(ui, |ui| {
        for titre in ["Quand", "Affiche", "Ligue", "Tournoi", "Format"] {
            en_tete(ui, titre);
        }
        ui.end_row();
        for m in matchs {
            let quand = if m.etat == "inProgress" {
                RichText::new("en cours").color(SPEAK).strong().size(11.5)
            } else {
                RichText::new(format!("{} · {}", crate::day_label(m.date), crate::format_time(m.date)))
                    .color(TEXT_DIM)
                    .size(11.5)
            };
            ui.label(quand);
            ui.label(RichText::new(m.equipes.join("  vs  ")).strong());
            let ligue = if m.region.is_empty() { m.ligue.clone() } else { format!("{} · {}", m.ligue, m.region) };
            ui.label(RichText::new(ligue).color(TEXT_DIM));
            ui.label(RichText::new(&m.tournoi).color(TEXT_FAINT).size(11.5));
            ui.label(RichText::new(&m.format).color(TEXT_FAINT).size(11.5));
            ui.end_row();
        }
    });
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
    /// Les bilans du groupe s'il a déjà été reçu — pour la médiane.
    groupe: Vec<Ligne<'a>>,
}

impl<'a> Vue<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        username: &'a str,
        fiche: &'a FicheValorant,
        periode: PeriodeFiche,
        classe: bool,
        stats: &'a [FicheMembre],
        membres: &'a [Member],
        rangs: &'a rangs::Rangs,
        maintenant: u64,
    ) -> Self {
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
        Self { username, fiche, e, p, b, classe, periode, noms: Annuaire::new(stats, membres), rangs, groupe }
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
    /// La fiche d'un membre : une page déroulante à deux filtres. Rend
    /// `false` quand elle se ferme.
    pub fn fiche(
        &mut self,
        ctx: &egui::Context,
        ouverte: &FicheOuverte,
        stats: &[FicheMembre],
        membres: &[Member],
        rangs: &rangs::Rangs,
    ) -> bool {
        let mut open = true;
        let roomy = (ctx.screen_rect().height() - 120.0).clamp(360.0, 780.0);
        let titre = format!("VALORANT — {}", ouverte.username);
        let (max_l, max_h) = bornes_de_fenetre(ctx);
        egui::Window::new(titre)
            .id(egui::Id::new("fiche_valorant_v5"))
            .collapsible(false)
            .resizable(true)
            .default_width(640.0)
            .min_width(520.0_f32.min(max_l))
            .max_width(max_l)
            .max_height(max_h)
            .default_height(roomy)
            .open(&mut open)
            .show(ctx, |ui| match (ouverte.recue, &ouverte.fiche) {
                (false, _) => {
                    ui.label(RichText::new("demande au serveur…").color(TEXT_DIM));
                }
                (true, None) => {
                    ui.label(RichText::new("pas de compte Riot lié — ou pas encore de fiche.").color(TEXT_DIM));
                }
                (true, Some(fiche)) => {
                    self.fiche_corps(ui, &ouverte.username, fiche, stats, membres, rangs);
                }
            });
        open
    }

    /// Le corps de la fiche : l'identité et les filtres en tête, puis les
    /// sections qui se déroulent.
    fn fiche_corps(
        &mut self,
        ui: &mut Ui,
        username: &str,
        fiche: &FicheValorant,
        stats: &[FicheMembre],
        membres: &[Member],
        rangs: &rangs::Rangs,
    ) {
        // L'identité sur sa ligne, les filtres sur la leur — comme la
        // ligne d'onglets de la page du groupe. Six boutons et un Riot ID
        // sur une seule ligne dépassent les 640 px de la fenêtre, et un
        // `right_to_left` ne se replie pas : il se dessinait par-dessus
        // « EU · PC · niveau … ».
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(&fiche.riot_id).strong().size(16.0));
            let mut detail = format!("{} · {}", fiche.region.to_uppercase(), fiche.plateforme.to_uppercase());
            if fiche.niveau > 0 {
                detail.push_str(&format!(" · niveau {}", fiche.niveau));
            }
            ui.label(RichText::new(detail).color(TEXT_DIM).size(11.5));
        });
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            for p in PeriodeFiche::TOUTES {
                if ui.selectable_label(self.fiche_periode == p, p.label()).clicked() {
                    self.fiche_periode = p;
                    self.fiche_plus = false;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // De droite à gauche : « Tous », puis « Compétitif ».
                if ui.selectable_label(!self.fiche_classe, "Tous").clicked() {
                    self.fiche_classe = false;
                }
                if ui.selectable_label(self.fiche_classe, "Compétitif").clicked() {
                    self.fiche_classe = true;
                }
            });
        });
        ui.add_space(6.0);

        let vue = Vue::new(username, fiche, self.fiche_periode, self.fiche_classe, stats, membres, rangs, maintenant_ms());
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            en_tete_fiche(ui, &vue);
            ui.add_space(10.0);
            self.courbe(ui, &vue);
            forme(ui, &vue);
            ui.add_space(10.0);
            tuiles(ui, &vue);
            ui.add_space(10.0);
            agents_et_cartes(ui, &vue);
            mes_heures(ui, &vue);
            ui.add_space(10.0);
            self.table(ui, &vue);
            actes(ui, &vue);
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!("mis à jour {} · HenrikDev", crate::il_y_a(fiche.maj))).color(TEXT_FAINT).size(10.5),
            );
        });
    }

    /// La courbe de RR sur les points de la fenêtre ; le point survolé
    /// surligne sa ligne dans la table.
    fn courbe(&mut self, ui: &mut Ui, vue: &Vue) {
        let (rr, teinte) = signe(vue.b.rr);
        let quoi = match vue.periode {
            PeriodeFiche::DixDerniers => "10 derniers classés".to_string(),
            p => p.titre().to_string(),
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("RR — {quoi}")).color(TEXT_DIM).size(11.5));
            if !vue.p.is_empty() {
                ui.label(RichText::new(format!("· {rr}")).color(teinte).size(11.5).strong());
            }
        });
        if vue.p.is_empty() {
            ui.label(RichText::new("Aucun match classé connu.").color(TEXT_FAINT).size(11.5));
            self.survol = None;
            return;
        }
        if vue.p.len() < 2 {
            ui.label(RichText::new("Pas encore assez de classés pour une courbe.").color(TEXT_FAINT).size(11.5));
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
        let info = |i: usize| -> String {
            let Some(x) = vue.p.get(i) else { return String::new() };
            let mut texte = format!(
                "{} · {} · {} · {} RR · {}",
                if x.carte.is_empty() { "?" } else { x.carte.as_str() },
                signe(x.delta).0,
                nom_de_rang(x.tier),
                x.rr,
                crate::il_y_a(x.date)
            );
            if x.protege {
                texte.push_str(" · descente protégée");
            }
            texte
        };
        let survole = graphes::courbe_temps(ui, egui::Id::new("fiche_courbe"), &points, HAUTEUR_COURBE, true, &frontieres, pic, info);
        self.survol = survole.and_then(|i| vue.p.get(i)).map(|x| x.match_id.clone()).filter(|id| !id.is_empty());
    }

    /// La table des matchs retenus, une ligne dépliable par match.
    fn table(&mut self, ui: &mut Ui, vue: &Vue) {
        ui.label(RichText::new("Matchs").color(TEXT_DIM).size(11.5));
        if vue.e.is_empty() {
            ui.label(RichText::new("Aucun match sur cette période.").color(TEXT_FAINT).size(11.5));
            return;
        }
        let largeur_page = ui.available_width();
        let limite = if self.fiche_plus { usize::MAX } else { LIGNES_TABLE };
        let restants = vue.e.len().saturating_sub(limite);
        let mut basculer: Option<String> = None;

        egui::ScrollArea::horizontal().id_salt("fiche_table").show(ui, |ui| {
            egui::Grid::new("fiche_matchs").striped(true).spacing([10.0, 4.0]).show(ui, |ui| {
                // Les textes de la table ne se sélectionnent pas : un label
                // sélectionnable sent le clic, et passerait devant le cadre
                // de la ligne — c'est toute la ligne qui se clique ici.
                ui.style_mut().interaction.selectable_labels = false;
                en_tete(ui, "Quand");
                if !vue.classe {
                    en_tete(ui, "Mode");
                }
                for titre in ["Carte", "Agent", "K / D / A", "ACS", "ADR", "KAST", "FK / FD", "Score", "ΔRR", "Rang", "Avec", ""] {
                    en_tete(ui, titre);
                }
                ui.end_row();
                for (i, m) in vue.e.iter().enumerate().take(limite) {
                    let deplie = !m.id.is_empty() && self.deplie.as_deref() == Some(m.id.as_str());
                    let survole = !m.id.is_empty() && self.survol.as_deref() == Some(m.id.as_str());

                    // Toute la ligne se clique et se surligne. Son cadre
                    // s'enregistre AVANT les cellules : egui ne « survole »
                    // un widget muet — les pastilles, l'icône de rang —
                    // que s'il est au-dessus du dernier widget interactif
                    // sous la souris ; posé après elles, le cadre les
                    // éteignait, et leurs infobulles avec. Sa hauteur est
                    // celle mesurée à l'image précédente (la première
                    // image se contente d'une ligne de texte) ; sa position
                    // vient du curseur de cette image, exacte même en
                    // plein défilement. Le fond se réserve ici aussi, pour
                    // passer sous le texte.
                    let id_ligne = ui.id().with(("ligne", i));
                    let haut_ligne = ui.cursor().min.y;
                    let (decalage, taille): (f32, Vec2) =
                        ui.data(|d| d.get_temp(id_ligne)).unwrap_or((-2.0, Vec2::new(largeur_page, 20.0)));
                    let rect_estime = Rect::from_min_size(Pos2::new(ui.cursor().min.x, haut_ligne + decalage), taille);
                    let reponse = ui.interact(rect_estime, id_ligne, Sense::click());
                    let fond = ui.painter().add(egui::Shape::Noop);

                    let premier = cellule(ui, crate::il_y_a(m.date), TEXT_FAINT);
                    if !vue.classe {
                        cellule(ui, &m.mode, TEXT);
                    }
                    cellule(ui, &m.carte, TEXT);
                    cellule(ui, &m.agent, TEXT_DIM);
                    cellule(ui, format!("{} / {} / {}", m.kills, m.deaths, m.assists), TEXT)
                        .on_hover_text(format!("éliminations / morts / assistances — {} points", milliers(m.score)));
                    cellule(ui, opt_entier(acs_de(m)), TEXT_DIM);
                    cellule(ui, opt_entier(adr_de(m)), TEXT_DIM);
                    cellule(ui, opt_pct(kast_de(m)), TEXT_DIM);
                    let fk = m
                        .manches_detail
                        .as_ref()
                        .map(|d| format!("{} / {}", d.premiers_sangs, d.premieres_morts))
                        .unwrap_or_else(|| TIRET.to_string());
                    cellule(ui, fk, TEXT_DIM);
                    ui.label(
                        RichText::new(format!("{}-{}", m.manches.0, m.manches.1))
                            .color(teinte_de_score(m.gagne))
                            .strong()
                            .size(11.5),
                    );
                    match vue.point_de(&m.id) {
                        Some(p) => {
                            let (t, c) = signe(p.delta);
                            cellule(ui, t, c);
                        }
                        None => {
                            ui.label("");
                        }
                    }
                    match vue.rangs.texture(m.tier).filter(|_| m.tier >= 3) {
                        Some(icone) => {
                            ui.add(egui::Image::new(icone).fit_to_exact_size(Vec2::splat(16.0)))
                                .on_hover_text(nom_de_rang(m.tier));
                        }
                        None => {
                            let rang = if m.tier >= 3 { nom_de_rang(m.tier) } else { String::new() };
                            ui.label(RichText::new(rang).color(graphes::couleur_de_rang(m.tier)).size(11.0));
                        }
                    }
                    if pastilles(ui, &m.avec, &m.contre, &vue.noms).is_none() {
                        ui.label("");
                    }
                    let dernier = triangle(ui, if deplie { Sens::Bas } else { Sens::Droite }, TEXT_DIM);

                    // Le cadre tel qu'il est vraiment : de la première à la
                    // dernière cellule, sur la largeur de la grille — pour
                    // le fond de cette image et le clic de la suivante.
                    let gauche = premier.rect.left();
                    let droite = ui.min_rect().right().max(gauche + largeur_page);
                    let rect = Rect::from_min_max(
                        Pos2::new(gauche, premier.rect.top() - 2.0),
                        Pos2::new(droite, dernier.rect.bottom() + 2.0),
                    );
                    ui.data_mut(|d| d.insert_temp(id_ligne, (rect.top() - haut_ligne, rect.size())));
                    if survole {
                        ui.painter().set(fond, egui::Shape::rect_filled(rect, egui::CornerRadius::same(4), theme::alpha(ACCENT, 24)));
                    } else if reponse.hovered() {
                        ui.painter().set(fond, egui::Shape::rect_filled(rect, egui::CornerRadius::same(4), theme::alpha(TEXT, 10)));
                    }
                    if reponse.clicked() && !m.id.is_empty() {
                        basculer = Some(m.id.clone());
                    }
                    ui.end_row();

                    if deplie {
                        detail_du_match(ui, m, vue, largeur_page);
                        ui.end_row();
                    }
                }
            });
        });
        if let Some(id) = basculer {
            self.deplie = if self.deplie.as_deref() == Some(id.as_str()) { None } else { Some(id) };
        }
        if restants > 0 && ui.button(format!("voir les {} autres", restants)).clicked() {
            self.fiche_plus = true;
        }
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

/// L'en-tête de la fiche : le rang en grand, la jauge vers le palier
/// suivant, l'acte en cours, le pic.
fn en_tete_fiche(ui: &mut Ui, vue: &Vue) {
    let fiche = vue.fiche;
    let r = &fiche.rang;
    let couleur = graphes::couleur_de_rang(r.tier);
    ui.horizontal(|ui| {
        if let Some(icone) = vue.rangs.texture(r.tier).filter(|_| r.tier >= 3) {
            ui.add(egui::Image::new(icone).fit_to_exact_size(Vec2::splat(40.0)));
        }
        if r.tier >= 3 {
            ui.label(RichText::new(nom_de_rang(r.tier)).color(couleur).strong().size(20.0));
            ui.label(RichText::new(format!("{} RR", r.rr)).color(TEXT_DIM).size(15.0));
            if r.delta != 0 {
                let (texte, teinte) = signe(r.delta);
                ui.label(RichText::new(format!("{texte} au dernier match")).color(teinte).size(11.5));
            }
        } else {
            ui.label(RichText::new("Non classé").color(TEXT_FAINT).strong().size(20.0));
        }
    });
    if (3..24).contains(&r.tier) {
        ui.horizontal(|ui| {
            graphes::jauge(ui, f32::from(r.rr) / 100.0, None, 200.0, couleur);
            ui.label(RichText::new(format!("{} / 100 vers {}", r.rr, nom_de_rang(r.tier + 1))).color(TEXT_FAINT).size(11.0));
        });
    } else if r.tier >= 24 && r.classement > 0 {
        ui.label(RichText::new(format!("#{} du classement", milliers(r.classement))).color(TEXT_DIM).size(11.5));
    }
    ligne_mmr(ui, fiche);
    // L'acte en cours, les placements, les boucliers.
    let mut parts: Vec<String> = Vec::new();
    if let Some(s) = fiche.saisons.last() {
        let pct = if s.parties > 0 { format!(" · {} %", u32::from(s.victoires) * 100 / u32::from(s.parties)) } else { String::new() };
        parts.push(format!("acte {} · {} V / {}{pct}", s.saison.to_uppercase(), s.victoires, pluriel(u32::from(s.parties), "partie")));
    }
    if r.placements_restants > 0 {
        parts.push(format!("encore {}", pluriel(u32::from(r.placements_restants), "placement")));
    }
    if r.boucliers > 0 {
        parts.push(pluriel(u32::from(r.boucliers), "bouclier"));
    }
    if !parts.is_empty() {
        ui.label(RichText::new(parts.join(" · ")).color(TEXT_DIM).size(11.5));
    }
    if let Some(pic) = fiche.pic.as_ref().filter(|p| p.tier >= 3) {
        let mut texte = format!("pic : {} · {} RR", nom_de_rang(pic.tier), pic.rr);
        if !pic.saison.is_empty() {
            texte.push_str(&format!(" · {}", pic.saison.to_uppercase()));
        }
        ui.label(RichText::new(texte).color(TEXT_FAINT).size(11.5));
    }
}

/// Pourquoi on peut dire quelque chose d'un MMR que Riot cache.
const POURQUOI_MMR: &str = "Riot ne montre pas le MMR caché : on le devine à tes variations de RR — gagner plus qu'on ne perd, \
c'est un MMR au-dessus du rang, le jeu pousse à monter ; l'inverse, en dessous. Sur les 20 derniers classés de l'acte, \
sans les descentes protégées.";

/// « MMR caché : au-dessus du rang · +22 par victoire, −13 par défaite
/// sur 14 classés · ~2 victoires vers Or 3 » — l'estimation du protocole
/// ([`FicheValorant::mmr_estime`]), ou pourquoi il n'y en a pas. Rien
/// pour un non-classé. La ligne replie : elle ne pousse jamais la
/// fenêtre.
fn ligne_mmr(ui: &mut Ui, fiche: &FicheValorant) {
    let r = &fiche.rang;
    if r.tier < 3 {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let Some(e) = fiche.mmr_estime() else {
            let texte = if r.placements_restants > 0 {
                "MMR caché : après les placements"
            } else {
                "MMR caché : pas assez de classés"
            };
            ui.label(RichText::new(texte).color(TEXT_FAINT).size(11.5)).on_hover_text(POURQUOI_MMR);
            return;
        };
        let (ou, teinte) = match e.position {
            PositionMmr::AuDessus => ("au-dessus du rang", SPEAK),
            PositionMmr::AuNiveau => ("au niveau du rang", TEXT_DIM),
            PositionMmr::EnDessous => ("en dessous du rang", DANGER),
        };
        ui.label(RichText::new(format!("MMR caché : {ou}")).color(teinte).strong().size(11.5))
            .on_hover_text(POURQUOI_MMR);
        let mut detail = format!(
            "· +{} par victoire, −{} par défaite sur {}",
            opt_entier(Some(e.gain_moyen)),
            opt_entier(Some(e.perte_moyenne)),
            pluriel(u32::from(e.points), "classé")
        );
        // Combien de victoires au rythme actuel jusqu'au palier suivant —
        // pas pour Immortel et plus, qui n'ont pas de palier à 100 RR.
        if (3..24).contains(&r.tier) && e.gain_moyen > 0.0 {
            let restant = f32::from(100u16.saturating_sub(r.rr));
            let n = (restant / e.gain_moyen).ceil().max(1.0) as u32;
            detail.push_str(&format!(" · ~{} vers {}", pluriel(n, "victoire"), nom_de_rang(r.tier + 1)));
        }
        ui.label(RichText::new(detail).color(TEXT_FAINT).size(11.5)).on_hover_text(POURQUOI_MMR);
    });
}

/// Les classés à manches du plus récent au plus ancien — l'ordre de
/// `FicheValorant::forme`, refait ici pour avoir les matchs sous la main.
fn classes_recents(fiche: &FicheValorant) -> Vec<&MatchResume> {
    let mut v: Vec<&MatchResume> =
        fiche.matchs.iter().filter(|m| m.mode == MODE_CLASSE && FicheValorant::a_des_manches(m)).collect();
    v.sort_by_key(|m| std::cmp::Reverse(m.date));
    v
}

/// La forme : les vingt derniers classés en cases, le bilan et la série
/// en cours — sur les classés seulement, quelle que soit la période.
fn forme(ui: &mut Ui, vue: &Vue) {
    let classes = classes_recents(vue.fiche);
    if classes.is_empty() {
        return;
    }
    ui.add_space(8.0);
    let recents: Vec<&MatchResume> = classes.iter().copied().take(20).collect();
    let resultats: Vec<i8> = recents
        .iter()
        .map(|m| match m.gagne {
            Some(true) => 1,
            Some(false) => -1,
            None => 0,
        })
        .collect();
    let (v, d) = classes.iter().fold((0u32, 0u32), |(v, d), m| match m.gagne {
        Some(true) => (v + 1, d),
        Some(false) => (v, d + 1),
        None => (v, d),
    });
    let serie = vue.fiche.serie();
    let serie_texte = if serie > 0 {
        format!("série en cours : {serie} V")
    } else if serie < 0 {
        format!("série en cours : {} D", -i16::from(serie))
    } else {
        format!("série en cours : {TIRET}")
    };
    let info = |i: usize| -> String {
        recents
            .get(i)
            .map(|m| format!("{} · {}-{} · {} · {}", m.carte, m.manches.0, m.manches.1, m.agent, crate::il_y_a(m.date)))
            .unwrap_or_default()
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new("Forme").color(TEXT_DIM).size(11.5));
        graphes::bande_forme(ui, &resultats, Vec2::new(8.0, 12.0), Some(&info));
        ui.label(RichText::new(format!("{v} V · {d} D · {serie_texte}")).color(TEXT_DIM).size(11.5));
    });
}

/// Les huit tuiles de la fiche, avec le repère du groupe sous les taux.
fn tuiles(ui: &mut Ui, vue: &Vue) {
    let b = &vue.b;
    let (w, h) = (TUILE_FICHE, 88.0);
    let nuls = b.matchs.saturating_sub(b.victoires).saturating_sub(b.defaites);
    let bilan_de = |ui: &mut Ui| {
        ui.add_space(4.0);
        graphes::barre_vd(ui, u32::from(b.victoires), u32::from(b.defaites), u32::from(nuls), w - 2.0 * MARGE_TUILE);
    };
    // Sous un taux : la jauge contre la médiane du groupe, s'il y en a.
    let repere = |ui: &mut Ui, valeur: fn(&Bilan) -> Option<f32>, moi: Option<f32>, decimales: usize, unite: &str| {
        let Some(moi) = moi else { return };
        let Some((mediane, max)) = vue.repere(valeur, moi) else { return };
        ui.add_space(4.0);
        let teinte = if moi >= mediane { SPEAK } else { DANGER };
        graphes::jauge(ui, moi / max, Some(mediane / max), w - 2.0 * MARGE_TUILE, teinte).on_hover_text(format!(
            "médiane du groupe sur 30 jours : {}{unite} · {} : {}{unite}",
            dec(mediane, decimales),
            vue.username,
            dec(moi, decimales)
        ));
    };

    // Les sous-titres des tuiles disent pourquoi une valeur manque — et
    // ne prêtent rien à la sélection qu'elle n'a pas : sans aucun match à
    // manches, la seule raison est « aucun match », pas des manches non
    // lues. Un dégât à zéro ne vient que d'un match résumé avant 0.1.40
    // (l'archive de HenrikDev les donne) ; un détail absent vient d'un
    // match d'avant, ou d'un match archivé. Courts : 116 px les tronquent.
    let aucun = b.matchs == 0;
    let sous_degats = if aucun {
        "aucun match".to_string()
    } else if b.matchs_degats == 0 {
        "avant 0.1.40".to_string()
    } else if b.matchs_degats < b.matchs {
        format!("sur {}", pluriel(u32::from(b.matchs_degats), "match"))
    } else {
        String::new()
    };
    let sous_detail = if aucun {
        "aucun match".to_string()
    } else if b.matchs_detailles == 0 {
        "manches non lues".to_string()
    } else if b.matchs_detailles < b.matchs {
        format!("sur {}", pluriel(u32::from(b.matchs_detailles), "match"))
    } else {
        String::new()
    };
    let sous_tirs = if aucun {
        "aucun match".to_string()
    } else if b.tirs > 0 {
        format!("{} tirs", milliers(b.tirs))
    } else {
        "tirs non comptés".to_string()
    };

    let mut grille = Grille::new(w, h);
    {
        let valeur = if b.matchs > 0 { format!("{} V · {} D", b.victoires, b.defaites) } else { TIRET.to_string() };
        let t = Tuile::new("Bilan", valeur, TEXT)
            .sous(if b.matchs > 0 { opt_pct(b.victoires_pct()) } else { "aucun match".to_string() })
            .info(format!(
                "victoires / (victoires + défaites) = {} / {} sur {} à manches ; {} nul(s)",
                b.victoires,
                u32::from(b.victoires) + u32::from(b.defaites),
                pluriel(u32::from(b.matchs), "match"),
                nuls
            ));
        grille.tuile(t, bilan_de);

        let t = Tuile::new("K/D", opt_dec(b.kd(), 2), ACCENT)
            .sous(format!("KDA {}", opt_dec(b.kda(), 2)))
            .info(format!(
                "kills / morts = {} / {} ; KDA = (kills + assists) / morts = ({} + {}) / {}",
                b.kills, b.deaths, b.kills, b.assists, b.deaths
            ));
        grille.tuile(t, |ui| repere(ui, Bilan::kd, b.kd(), 2, ""));

        let t = Tuile::new("ACS", opt_entier(b.acs()), ACCENT)
            .sous(format!("sur {}", pluriel(u32::from(b.manches), "manche")))
            .info(format!("score / manches = {} / {}", milliers(b.score), b.manches));
        grille.tuile(t, |ui| repere(ui, Bilan::acs, b.acs(), 0, ""));

        let t = Tuile::new("ADR", opt_entier(b.adr()), ACCENT)
            .sous(sous_degats)
            .info(format!("dégâts / manches des matchs qui les ont = {} / {}", milliers(b.degats), b.manches_degats));
        grille.tuile(t, |ui| repere(ui, Bilan::adr, b.adr(), 0, ""));

        let t = Tuile::new("KAST", opt_pct(b.kast_pct()), ACCENT)
            .sous(sous_detail.clone())
            .info(format!(
                "manches avec kill, assist, survie ou échange / manches lues = {} / {}",
                b.kast, b.manches_detaillees
            ));
        grille.tuile(t, |ui| repere(ui, Bilan::kast_pct, b.kast_pct(), 0, " %"));

        let t = Tuile::new("Tête", opt_pct(b.tete_pct()), ACCENT)
            .sous(sous_tirs)
            .info(format!("tirs à la tête / tirs = {} / {}", milliers(b.tetes), milliers(b.tirs)));
        grille.tuile(t, |ui| repere(ui, Bilan::tete_pct, b.tete_pct(), 0, " %"));

        let valeur = if b.matchs_detailles > 0 {
            format!("{} · {} FD", b.premiers_sangs, b.premieres_morts)
        } else {
            TIRET.to_string()
        };
        let t = Tuile::new("Premiers sangs", valeur, ACCENT)
            .sous(if b.matchs_detailles > 0 { format!("{} par match", opt_dec(b.fk_par_match(), 1)) } else { sous_detail.clone() })
            .info(format!(
                "premiers sangs et premières morts sur {} lus ; {} / {} par match",
                pluriel(u32::from(b.matchs_detailles), "match"),
                b.premiers_sangs,
                b.matchs_detailles
            ));
        grille.tuile(t, |_| {});

        let valeur = if b.matchs_detailles > 0 {
            let mut v = format!("{} / {}", b.clutchs, b.clutchs_tentes);
            if b.meilleur_clutch > 0 {
                v.push_str(&format!(" · 1v{}", b.meilleur_clutch));
            }
            v
        } else {
            TIRET.to_string()
        };
        let t = Tuile::new("Clutchs", valeur, theme::WARN)
            .sous(if b.matchs_detailles > 0 { multi_kills(b.triples, b.quadruples, b.aces) } else { sous_detail })
            .info(format!(
                "situations 1 contre X gagnées / tentées, le plus gros X gagné ; manches à 3, 4, 5 kills ou plus : \
                 {} triple(s), {} quadruple(s), {} ace(s)",
                b.triples, b.quadruples, b.aces
            ));
        grille.tuile(t, |_| {});
    }
    grille.montrer(ui);
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

/// Agents et cartes en barres, côte à côte.
fn agents_et_cartes(ui: &mut Ui, vue: &Vue) {
    let agents = vue.ventiler(|m| m.agent.as_str());
    let cartes = vue.ventiler(|m| m.carte.as_str());
    ui.columns(2, |cols| {
        cols[0].label(RichText::new("Agents").color(TEXT_DIM).size(11.5));
        if agents.is_empty() {
            cols[0].label(RichText::new(TIRET).color(TEXT_FAINT));
        } else {
            let max = agents.iter().map(|(_, b)| b.matchs).max().unwrap_or(0).max(1);
            let lignes: Vec<Barre> = agents
                .iter()
                .take(6)
                .map(|(nom, b)| Barre {
                    label: nom.as_str(),
                    part: f32::from(b.matchs) / f32::from(max),
                    fond: 0.0,
                    texte: format!("{} · {} · K/D {}", b.matchs, opt_pct(b.victoires_pct()), opt_dec(b.kd(), 1)),
                    couleur: ACCENT,
                    info: format!(
                        "{} · {} V / {} D · {} kills / {} morts",
                        pluriel(u32::from(b.matchs), "partie"),
                        b.victoires,
                        b.defaites,
                        b.kills,
                        b.deaths
                    ),
                })
                .collect();
            graphes::barres(&mut cols[0], &lignes, 70.0, 20.0);
        }
        cols[1].label(RichText::new("Cartes").color(TEXT_DIM).size(11.5));
        if cartes.is_empty() {
            cols[1].label(RichText::new(TIRET).color(TEXT_FAINT));
        } else {
            let max = cartes.iter().map(|(_, b)| b.matchs).max().unwrap_or(0).max(1);
            let lignes: Vec<Barre> = cartes
                .iter()
                .map(|(nom, b)| {
                    let taux = b.victoires_pct().map(|p| p / 100.0).unwrap_or(0.0);
                    Barre {
                        label: nom.as_str(),
                        part: taux,
                        fond: f32::from(b.matchs) / f32::from(max),
                        texte: format!("{} · {}/{}", opt_pct(b.victoires_pct()), b.victoires, b.matchs),
                        couleur: if taux >= 0.5 { SPEAK } else { DANGER },
                        info: format!(
                            "{} · {} V / {} D · la part colorée est le taux de victoire, le gris la part des parties",
                            pluriel(u32::from(b.matchs), "partie"),
                            b.victoires,
                            b.defaites
                        ),
                    }
                })
                .collect();
            graphes::barres(&mut cols[1], &lignes, 70.0, 20.0);
        }
    });
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

/// Quand il joue — cinq matchs au moins pour que ça dise quelque chose.
fn mes_heures(ui: &mut Ui, vue: &Vue) {
    if vue.e.len() < 5 {
        return;
    }
    let cases = cases_des_heures(vue.e.iter().map(|m| m.date));
    ui.add_space(10.0);
    ui.label(RichText::new("Mes heures").color(TEXT_DIM).size(11.5));
    graphes::heatmap_semaine(ui, &cases, |jour, heure| {
        let n = cases.get(jour * 24 + heure).copied().unwrap_or(0);
        format!("{} {heure}h–{}h · {}", JOURS_LONGS.get(jour).copied().unwrap_or("?"), heure + 1, pluriel(u32::from(n), "partie"))
    });
}

/// Ce que dit une ligne dépliée : les cases des manches puis le récit.
fn recit_du_match(m: &MatchResume, noms: &Annuaire) -> String {
    let mut parts: Vec<String> = Vec::new();
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

/// La ligne dépliée d'un match, sur toute la largeur de la grille : la
/// cellule n'occupe qu'un pixel de large, le reste est peint à côté dans
/// un enfant qui ne pèse pas sur les colonnes.
fn detail_du_match(ui: &mut Ui, m: &MatchResume, vue: &Vue, largeur_page: f32) {
    let texte = recit_du_match(m, &vue.noms);
    let police = egui::FontId::proportional(11.5);
    let largeur = (largeur_page - 16.0).max(80.0);
    let galley = ui.fonts(|f| f.layout(texte, police, TEXT_DIM, largeur));
    let deroule = m.manches_detail.as_ref().map(|d| d.deroule.as_str()).unwrap_or("");
    let hauteur_cases = if deroule.is_empty() { 0.0 } else { 14.0 + 4.0 };
    let hauteur = hauteur_cases + galley.size().y + 6.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, hauteur), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let zone = Rect::from_min_size(Pos2::new(rect.left() + 8.0, rect.top() + 2.0), Vec2::new(largeur, hauteur));
    let mut enfant = ui.new_child(egui::UiBuilder::new().max_rect(zone).layout(egui::Layout::top_down(egui::Align::Min)));
    if !deroule.is_empty() {
        graphes::cases_manches(&mut enfant, deroule);
        enfant.add_space(4.0);
    }
    let pos = enfant.cursor().min;
    enfant.painter().galley(pos, galley, TEXT_DIM);
}

/// Les actes joués, en pastilles teintées du rang de fin.
fn actes(ui: &mut Ui, vue: &Vue) {
    let saisons = &vue.fiche.saisons;
    if saisons.is_empty() {
        return;
    }
    ui.add_space(10.0);
    ui.label(RichText::new("Actes").color(TEXT_DIM).size(11.5));
    let dernier = saisons.len() - 1;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().button_padding = Vec2::new(6.0, 2.0);
        for (i, s) in saisons.iter().enumerate() {
            let couleur = graphes::couleur_de_rang(s.tier_fin);
            let mut texte = RichText::new(format!("{} · {} · {} V / {}", s.saison, nom_de_rang(s.tier_fin), s.victoires, s.parties))
                .color(couleur)
                .size(11.0);
            if i == dernier {
                texte = texte.strong();
            }
            // Un bouton qu'on ne clique pas, plutôt qu'un cadre : dans une
            // ligne qui replie, un cadre ne passe pas à la ligne, il
            // déborde — et dix-sept actes poussaient la fenêtre hors de
            // l'écran. Un widget d'un bloc, lui, replie.
            ui.add(
                egui::Button::new(texte)
                    .sense(Sense::hover())
                    .fill(theme::alpha(couleur, 28))
                    .stroke(egui::Stroke::NONE)
                    .corner_radius(egui::CornerRadius::same(6))
                    .min_size(Vec2::ZERO),
            )
            .on_hover_text(format!(
                    "fin d'acte : {} · {} RR · {} sur {}",
                    nom_de_rang(s.tier_fin),
                    s.rr_fin,
                    pluriel(u32::from(s.victoires), "victoire"),
                    pluriel(u32::from(s.parties), "partie")
                ));
        }
    });
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ki_protocol::{DetailManches, RangValorant, StatsSaison};

    fn point(match_id: &str, date: u64, saison: &str, delta: i32) -> PointRR {
        PointRR { match_id: match_id.into(), date, tier: 16, rr: 50, delta, saison: saison.into(), ..Default::default() }
    }

    fn match_de_test(id: &str, date: u64, mode: &str, gagne: bool, avec: &[UserId]) -> MatchResume {
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
        for o in Onglet::TOUS {
            assert_eq!(Onglet::depuis(o.cle()), o);
        }
        assert_eq!(Onglet::depuis("n'importe quoi"), Onglet::Groupe);
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
                "valo_onglet" => "matchs",
                "fiche_classe" => "off",
                _ => defaut,
            }
            .to_string()
        });
        assert_eq!(page.onglet, Onglet::Matchs);
        assert!(!page.fiche_classe);
        assert_eq!(page.fiche_periode, PeriodeFiche::TrenteJours);
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
        let vue = Vue::new("moi", &fiche, PeriodeFiche::Tout, false, &[], &[], &rangs, maintenant);
        assert_eq!(vue.e.len(), 5);
        assert_eq!(vue.b.matchs, 5, "un Spike Rush a des manches, il compte");
        assert_eq!(vue.e[0].id, "a", "du plus récent au plus ancien");
        let vue = Vue::new("moi", &fiche, PeriodeFiche::SeptJours, true, &[], &[], &rangs, maintenant);
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
        }
    }

    /// Quatre membres : un riche, un qui a joué trois de ses matchs, un
    /// sans rien, un Immortel classé qui a joué seul.
    fn groupe(maintenant: u64, vieux_serveur: bool) -> Vec<FicheMembre> {
        let riche = fiche_riche(maintenant);
        let membre = |id: UserId, nom: &str, fiche: FicheValorant| FicheMembre {
            user_id: id,
            username: nom.into(),
            bilan: (!vieux_serveur).then(|| bilan_local(&fiche, maintenant)),
            fiche: fiche.resume(5, 10),
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

    fn membres() -> Vec<Member> {
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
                roles: Vec::new(),
                online: true,
                color: None,
                rank: 0,
                invite: false,
            })
            .collect()
    }

    /// Une image egui sans écran, la souris posée quelque part : ce que
    /// la page dessine ne doit jamais paniquer, quelles que soient les
    /// données — pleines, pauvres, ou d'un serveur d'avant.
    fn dessiner(ctx: &egui::Context, souris: Option<Pos2>, mut corps: impl FnMut(&egui::Context)) {
        let mut entree = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1_400.0, 900.0))),
            ..Default::default()
        };
        if let Some(pos) = souris {
            entree.events.push(egui::Event::PointerMoved(pos));
        }
        // Deux passes : la seconde voit les tailles que la première a mesurées.
        for _ in 0..2 {
            let _ = ctx.run(entree.clone(), |ctx| corps(ctx));
        }
    }

    #[test]
    fn les_deux_fenetres_se_dessinent_sans_paniquer() {
        let maintenant = maintenant_ms();
        let ctx = egui::Context::default();
        let rangs = rangs::Rangs::new();
        let membres = membres();
        let mut boutique = boutique::Lecteur::new();
        let activite: Vec<u16> = (0..168u16).map(|i| i % 5).collect();

        for vieux_serveur in [false, true] {
            let stats = groupe(maintenant, vieux_serveur);
            let activite = if vieux_serveur { Vec::new() } else { activite.clone() };
            let mut page = PageValo::load(|_, d| d.to_string());
            page.ouvert = true;
            for onglet in [Onglet::Groupe, Onglet::Matchs, Onglet::Esport] {
                page.onglet = onglet;
                for periode in Periode::TOUTES {
                    page.periode = periode;
                    for souris in [None, Some(Pos2::new(300.0, 300.0)), Some(Pos2::new(700.0, 500.0))] {
                        dessiner(&ctx, souris, |ctx| {
                            let demandes =
                                page.fenetre(ctx, &stats, true, &[], &activite, Some(1), &membres, &rangs, &mut boutique);
                            assert!(demandes.is_empty());
                        });
                    }
                }
            }
            // Le classement dans tous ses tris, le fil dans ses filtres.
            page.onglet = Onglet::Groupe;
            for tri in [Tri::Rang, Tri::Rr7, Tri::Rr30, Tri::Kd, Tri::Acs, Tri::Adr, Tri::Kast, Tri::Matchs] {
                page.tri = tri;
                page.tri_desc = !page.tri_desc;
                dessiner(&ctx, None, |ctx| {
                    page.fenetre(ctx, &stats, true, &[], &activite, None, &membres, &rangs, &mut boutique);
                });
            }
            page.onglet = Onglet::Matchs;
            page.filtre_membre = Some(2);
            page.filtre_mode = Some(MODE_CLASSE.into());
            page.plus = true;
            dessiner(&ctx, None, |ctx| {
                page.fenetre(ctx, &stats, true, &[], &activite, None, &membres, &rangs, &mut boutique);
            });
            // Avant la réponse du serveur, et sans personne de lié.
            dessiner(&ctx, None, |ctx| {
                page.fenetre(ctx, &stats, false, &[], &[], None, &membres, &rangs, &mut boutique);
                page.fenetre(ctx, &[], true, &[], &[], None, &[], &rangs, &mut boutique);
            });
            assert!(page.ouvert);

            // La fiche : pas reçue, sans compte, vide, pauvre, riche.
            let riche = FicheOuverte { user_id: 1, username: "redik".into(), recue: true, fiche: Some(fiche_riche(maintenant)) };
            let pauvre = FicheOuverte { user_id: 4, username: "Bob".into(), recue: true, fiche: Some(stats[3].fiche.clone()) };
            let vide = FicheOuverte { user_id: 3, username: "Wam".into(), recue: true, fiche: Some(FicheValorant::default()) };
            let attente = FicheOuverte { user_id: 9, username: "?".into(), recue: false, fiche: None };
            let sans = FicheOuverte { user_id: 9, username: "?".into(), recue: true, fiche: None };
            for ouverte in [&attente, &sans, &vide, &pauvre, &riche] {
                for periode in PeriodeFiche::TOUTES {
                    page.fiche_periode = periode;
                    for classe in [true, false] {
                        page.fiche_classe = classe;
                        page.deplie = Some("m1".into());
                        page.fiche_plus = periode == PeriodeFiche::Tout;
                        for souris in [None, Some(Pos2::new(200.0, 250.0)), Some(Pos2::new(320.0, 600.0))] {
                            dessiner(&ctx, souris, |ctx| {
                                assert!(page.fiche(ctx, ouverte, &stats, &membres, &rangs));
                            });
                        }
                    }
                }
            }
            page.fermer_fiche();
            assert!(page.deplie.is_none() && page.survol.is_none() && !page.fiche_plus);
        }
    }

    /// Ce sur quoi la table compte : un widget muet posé au-dessus du
    /// cadre cliquable d'une ligne reste survolé — ses infobulles vivent.
    /// Posé en dessous (le cadre enregistré après lui), egui l'éteint.
    #[test]
    fn un_widget_muet_reste_survole_au_dessus_du_cadre_de_ligne() {
        let ctx = egui::Context::default();
        let souris = Pos2::new(60.0, 60.0);
        for cadre_avant in [true, false] {
            let mut survols = (false, false);
            dessiner(&ctx, Some(souris), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let rect = Rect::from_min_size(Pos2::new(20.0, 40.0), Vec2::new(400.0, 40.0));
                    let id = egui::Id::new("ligne_test");
                    let cadre = cadre_avant.then(|| ui.interact(rect, id, Sense::click()));
                    let dedans = Rect::from_min_size(Pos2::new(40.0, 50.0), Vec2::new(200.0, 20.0));
                    let muet = ui.interact(dedans, egui::Id::new("pastille_test"), Sense::hover());
                    let cadre = cadre.unwrap_or_else(|| ui.interact(rect, id, Sense::click()));
                    survols = (cadre.hovered(), muet.hovered());
                });
            });
            assert!(survols.0, "le cadre est survolé dans les deux ordres");
            assert_eq!(survols.1, cadre_avant, "le widget muet ne l'est qu'au-dessus du cadre");
        }
        // Et le clic arrive bien au cadre, même posé avant les cellules,
        // même sur le fond glissable d'une zone de défilement (enregistré
        // avant lui, comme `ScrollArea` le fait), même sous un label muet.
        let mut clique = false;
        let ecran = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_400.0, 900.0));
        let appuyer = |pressed: bool| egui::Event::PointerButton {
            pos: souris,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let entrees = [
            vec![egui::Event::PointerMoved(souris)],
            vec![egui::Event::PointerMoved(souris)],
            vec![appuyer(true)],
            vec![appuyer(false)],
        ];
        for events in entrees {
            let entree = egui::RawInput { screen_rect: Some(ecran), events, ..Default::default() };
            let _ = ctx.run(entree, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let fond = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(1_000.0, 800.0));
                    ui.interact(fond, egui::Id::new("defilement_test"), Sense::drag());
                    let rect = Rect::from_min_size(Pos2::new(20.0, 40.0), Vec2::new(400.0, 40.0));
                    let cadre = ui.interact(rect, egui::Id::new("ligne_test"), Sense::click());
                    let dedans = Rect::from_min_size(Pos2::new(40.0, 50.0), Vec2::new(200.0, 20.0));
                    ui.interact(dedans, egui::Id::new("pastille_test"), Sense::hover());
                    clique |= cadre.clicked();
                });
            });
        }
        assert!(clique, "le clic traverse le label muet et ignore le fond glissable");
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
        let deux = [membre(1, 10), membre(2, 20)];
        let vue = Vue::new("moi", &fiche, PeriodeFiche::Tout, true, &deux, &[], &rangs, maintenant);
        assert!(vue.repere(Bilan::kd, 1.0).is_none());
        let trois = [membre(1, 10), membre(2, 20), membre(3, 30)];
        let vue = Vue::new("moi", &fiche, PeriodeFiche::Tout, true, &trois, &[], &rangs, maintenant);
        let (mediane, max) = vue.repere(Bilan::kd, 5.0).expect("trois membres assez classés");
        assert_eq!(mediane, 2.0);
        assert_eq!(max, 5.0, "le plafond prend en compte la valeur du membre");
        // Le bilan vient d'être refait ici : la page le dit.
        let (_, local) = lignes_du_groupe(&trois, maintenant);
        assert!(local);
    }
    /// Une fiche pleine — soixante matchs détaillés, cent points, des
    /// actes, des co-membres — et la page du groupe à cinq membres se
    /// dessinent dans leur largeur par défaut : rien ne fait grandir la
    /// fenêtre (un cadre qui déborde d'une ligne repliée, une grille hors
    /// d'une zone de défilement…), sinon elle sortirait de l'écran et ne
    /// reviendrait plus, sa taille étant mémorisée.
    #[test]
    fn les_fenetres_gardent_leur_largeur_par_defaut() {
        let mut fiche = FicheValorant { riot_id: "Redik#6162".into(), region: "eu".into(), plateforme: "pc".into(), niveau: 214, ..Default::default() };
        fiche.rang = RangValorant { tier: 16, rr: 57, delta: 18, boucliers: 1, ..Default::default() };
        fiche.pic = Some(RangValorant { tier: 18, rr: 12, saison: "e10a3".into(), ..Default::default() });
        let base = 1_789_500_000_000u64;
        for i in 0..60u64 {
            let mut m = match_de_test(&format!("m{i}"), base - i * 5 * 3_600_000, if i % 7 == 3 { "Non classé" } else { "Compétitif" }, i % 3 != 0, &[2, 3]);
            m.contre = vec![4];
            m.degats = 3_000 + i as u32;
            m.saison = if i < 30 { "e11a2".into() } else { "e11a1".into() };
            m.manches_detail = Some(DetailManches { manches: 22, kast: 15, premiers_sangs: 3, premieres_morts: 2, triples: 1, clutchs_tentes: 2, clutchs: 1, meilleur_clutch: 2, poses: 2, desamorcages: 1, deroule: "VDVVDVDVVVDDVDVVDVVDVV".into(), ..Default::default() });
            fiche.matchs.push(m);
        }
        for i in 0..100u64 {
            fiche.historique_rr.push(point(&format!("m{i}"), base - i * 5 * 3_600_000, if i < 30 { "e11a2" } else { "e11a1" }, if i % 3 == 0 { -17 } else { 19 }));
        }
        // Dix-sept actes, comme un compte qui joue depuis l'épisode 5 :
        // leurs pastilles doivent replier, pas pousser la fenêtre.
        fiche.saisons = (0..17u8)
            .map(|i| StatsSaison {
                saison: format!("e{}a{}", 5 + i / 3, 1 + i % 3),
                victoires: 20 + u16::from(i),
                parties: 40 + 2 * u16::from(i),
                tier_fin: 6 + i,
                rr_fin: 40,
            })
            .collect();
        let stats: Vec<FicheMembre> = (1..=5)
            .map(|id| FicheMembre { user_id: id, username: format!("membre{id}"), fiche: fiche.resume(5, 10), bilan: None })
            .collect();
        let membres: Vec<Member> = Vec::new();
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let rangs = rangs::Rangs::new();
        let mut boutique = boutique::Lecteur::default();
        let mut page = PageValo::load(|_, d| d.to_string());
        page.ouvert = true;
        let ouverte = FicheOuverte { user_id: 1, username: "redik".into(), recue: true, fiche: Some(fiche) };
        let entree = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1900.0, 1000.0))),
            ..Default::default()
        };
        // Quelques images : les grilles et les zones de défilement se
        // stabilisent à la deuxième.
        for _ in 0..4 {
            let _ = ctx.run(entree(), |ctx| {
                page.fenetre(ctx, &stats, true, &[], &[], Some(1), &membres, &rangs, &mut boutique);
                page.fiche(ctx, &ouverte, &stats, &membres, &rangs);
            });
        }
        let groupe = ctx.memory(|m| m.area_rect(egui::Id::new("valo_page_v5"))).expect("la page est ouverte");
        let fiche = ctx.memory(|m| m.area_rect(egui::Id::new("fiche_valorant_v5"))).expect("la fiche est ouverte");
        // 860 et 640 de contenu, plus les marges de la fenêtre.
        assert!((850.0..=900.0).contains(&groupe.width()), "page du groupe : {}", groupe.width());
        assert!((630.0..=680.0).contains(&fiche.width()), "fiche : {}", fiche.width());
        assert!(fiche.height() <= 1000.0 && groupe.height() <= 1000.0);
    }
}
