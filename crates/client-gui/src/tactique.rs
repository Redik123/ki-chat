//! La carte tactique, côté client — V10 de PLAN-VALORANT-2.md : le plan
//! d'une carte de VALORANT (sa minimap, le nom de ses zones), sur lequel on
//! dessine pour préparer la manche suivante. Chaque trait vit quinze
//! secondes à partir du moment où l'on lève le crayon : douze pleines,
//! trois de fondu.
//!
//! Rien ne vient du jeu : le plan et les zones viennent de valorant-api.com
//! (par le catalogue), les traits des mains de qui dessine. La carte vit
//! dans la fenêtre de ki-chat, jamais dans l'overlay en jeu.
//!
//! Dans un salon vocal, face à un serveur qui la partage (V10.2), elle est
//! celle de tout le salon : chaque trait part par morceaux, toutes les
//! 50 ms pendant qu'on le tire, et ceux des autres arrivent de même. Les
//! traits sont rangés dans le repère de la minimap — de 0 à 1 sur chaque
//! axe —, et se reconnaissent à leur auteur et à leur numéro. Ailleurs, on
//! y dessine seul.
//!
//! Le module ne dépend que d'egui, de ki-ui, de serde et du protocole : le
//! banc `examples/banc_tactique.rs` le photographie sans ki-chat autour.

use std::time::{Duration, Instant};

use eframe::egui::{
    self, epaint::Vertex, pos2, vec2, Align2, Color32, FontId, PointerButton, Pos2, Rect, Sense, Shape, Stroke,
    StrokeKind,
};
use ki_protocol::{ActionTactique, CarteTactique, EvenementTactique, OutilTactique, PointTactique, UserId};
use ki_ui::composants as ui;
use ki_ui::icones::Icon;
use ki_ui::jetons::{couleur as c, espace, rayon, texte};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// La vie d'un trait, crayon levé.
pub const VIE: Duration = Duration::from_secs(15);
/// Ses dernières secondes, en fondu.
pub const FONDU: Duration = Duration::from_secs(3);
/// Le zoom va de la carte entière à quatre fois plus près.
const ZOOM_MAX: f32 = 4.0;
/// Les bornes d'un trait et de ceux d'un même auteur : au-delà, le plus
/// ancien de l'auteur s'en va.
const POINTS_MAX: usize = 4000;
const TRAITS_MAX: usize = 30;
/// Un point de plus seulement quand le pointeur a bougé d'autant, à
/// l'écran : un trait lent ne s'alourdit pas.
const ECART_MIN_PX: f32 = 2.0;
/// Le rythme des repeints pendant un fondu.
const REPEINT: Duration = Duration::from_millis(33);
/// Un nom de zone plus long n'est pas un nom de zone.
const NOM_DE_ZONE_MAX: usize = 32;
/// Le rythme des morceaux d'un trait qu'on tire, vers le salon.
const MORCEAU: Duration = Duration::from_millis(50);
/// Le temps qu'un refus du serveur reste affiché.
const REFUS_AFFICHE: Duration = Duration::from_secs(6);

// ---------------------------------------------------------------------
// Les plans
// ---------------------------------------------------------------------

/// Une carte de compétition telle que le catalogue la garde pour la carte
/// tactique (`/v1/maps` de valorant-api.com).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    /// Son nom affiché (« Ascent »).
    pub nom: String,
    /// L'adresse de sa minimap (`displayIcon`, 1024 px).
    pub minimap: String,
    /// Les quarts de tour, dans le sens des aiguilles d'une montre, qui
    /// mettent le spawn d'attaque en bas : les minimaps ne sont pas toutes
    /// tournées pareil.
    #[serde(default)]
    pub quarts: u8,
    /// Les zones nommées (« A Main », « Mid Courtyard »), dans le repère
    /// de la minimap.
    #[serde(default)]
    pub zones: Vec<(String, f32, f32)>,
}

/// Le plan d'une entrée de `/v1/maps`, si c'est une carte de compétition
/// (elle a des sites, une minimap et ses facteurs). Une zone hors de la
/// minimap ou au nom douteux ne passe pas.
///
/// Les facteurs passent du repère du jeu à celui de la minimap, les axes
/// croisés : `u = y·xMultiplier + xScalarToAdd`, `v = x·yMultiplier +
/// yScalarToAdd`.
pub fn plan_depuis(carte: &Value) -> Option<Plan> {
    carte["tacticalDescription"].as_str().filter(|s| !s.trim().is_empty())?;
    let nom = carte["displayName"].as_str().map(str::trim).filter(|s| !s.is_empty())?;
    let minimap = carte["displayIcon"].as_str().filter(|s| !s.is_empty())?;
    let facteur = |cle: &str| carte[cle].as_f64().map(|v| v as f32).filter(|v| v.is_finite());
    let (mx, my) = (facteur("xMultiplier")?, facteur("yMultiplier")?);
    let (ax, ay) = (facteur("xScalarToAdd")?, facteur("yScalarToAdd")?);
    if mx == 0.0 || my == 0.0 {
        return None;
    }
    let vers_plan = |l: &Value| Some(pos2(l["y"].as_f64()? as f32 * mx + ax, l["x"].as_f64()? as f32 * my + ay));
    let (mut attaque, mut defense) = (None, None);
    let mut zones: Vec<(String, f32, f32)> = Vec::new();
    for zone in carte["callouts"].as_array().into_iter().flatten() {
        let (Some(groupe), Some(region)) = (zone["superRegionName"].as_str(), zone["regionName"].as_str()) else {
            continue;
        };
        let Some(p) = vers_plan(&zone["location"]) else { continue };
        let dedans = |t: f32| (-0.05..=1.05).contains(&t);
        if !dedans(p.x) || !dedans(p.y) {
            continue;
        }
        let p = borner_au_plan(p);
        // « Attacker Side » + « Spawn » : « Attacker Spawn ».
        let groupe = groupe.trim();
        let groupe = groupe.strip_suffix(" Side").unwrap_or(groupe);
        let region = region.trim();
        if region == "Spawn" {
            match groupe {
                "Attacker" => attaque = Some(p),
                "Defender" => defense = Some(p),
                _ => {}
            }
        }
        let nom = format!("{groupe} {region}").trim().to_string();
        let lisible = !nom.is_empty() && nom.chars().count() <= NOM_DE_ZONE_MAX && !nom.chars().any(char::is_control);
        if lisible && !zones.iter().any(|(n, ..)| *n == nom) {
            zones.push((nom, p.x, p.y));
        }
    }
    Some(Plan { nom: nom.to_string(), minimap: minimap.to_string(), quarts: quarts_pour(attaque, defense), zones })
}

/// Les quarts de tour qui mettent le spawn d'attaque en bas, d'après la
/// direction qui va du spawn de défense au sien ; sans les deux, la
/// minimap telle quelle.
fn quarts_pour(attaque: Option<Pos2>, defense: Option<Pos2>) -> u8 {
    let (Some(a), Some(d)) = (attaque, defense) else { return 0 };
    let vers = a - d;
    if vers.y.abs() >= vers.x.abs() {
        if vers.y >= 0.0 {
            0
        } else {
            2
        }
    } else if vers.x > 0.0 {
        1
    } else {
        3
    }
}

// ---------------------------------------------------------------------
// Les traits
// ---------------------------------------------------------------------

/// Ce que le crayon trace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outil {
    /// Un trait libre.
    Crayon,
    /// Un chemin, avec sa pointe au bout : une poussée, une rotation.
    Fleche,
    /// Un cercle tiré au glisser : un site, une fumée.
    Zone,
}

impl Outil {
    const TOUS: [(Outil, &'static str, &'static str); 3] =
        [(Outil::Crayon, "Crayon", "1"), (Outil::Fleche, "Flèche", "2"), (Outil::Zone, "Zone", "3")];

    fn icone(self) -> Icon {
        match self {
            Outil::Crayon => Icon::Pencil,
            Outil::Fleche => Icon::Send,
            Outil::Zone => Icon::Target,
        }
    }

    fn vers_fil(self) -> OutilTactique {
        match self {
            Outil::Crayon => OutilTactique::Crayon,
            Outil::Fleche => OutilTactique::Fleche,
            Outil::Zone => OutilTactique::Zone,
        }
    }

    /// `None` : un outil d'une version plus récente, qui ne se dessine pas
    /// ici.
    fn du_fil(outil: OutilTactique) -> Option<Outil> {
        match outil {
            OutilTactique::Crayon => Some(Outil::Crayon),
            OutilTactique::Fleche => Some(Outil::Fleche),
            OutilTactique::Zone => Some(Outil::Zone),
            OutilTactique::Inconnu => None,
        }
    }
}

/// Un point de la carte sur le fil : de 0 à `u16::MAX` sur chaque axe.
fn vers_fil(p: Pos2) -> PointTactique {
    let axe = |t: f32| (t.clamp(0.0, 1.0) * u16::MAX as f32).round() as u16;
    [axe(p.x), axe(p.y)]
}

fn du_fil(p: PointTactique) -> Pos2 {
    pos2(p[0] as f32 / u16::MAX as f32, p[1] as f32 / u16::MAX as f32)
}

/// Avec qui la carte se partage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lien {
    /// Rien que pour soi : ce serveur ne partage pas la carte.
    #[default]
    SansServeur,
    /// Rien que pour soi : on n'est pas dans un salon vocal.
    HorsVocal,
    /// Celle de son salon vocal.
    Salon,
}

/// D'où l'on regarde : en attaque, son spawn en bas ; en défense, la carte
/// fait un demi-tour. Chacun chez soi — les traits restent à leur place sur
/// la carte.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Camp {
    #[default]
    Attaque,
    Defense,
}

/// Un trait sur la carte.
#[derive(Clone, Debug)]
pub struct Trait {
    /// Un trait se reconnaît à son auteur et à son numéro.
    pub auteur: UserId,
    pub id: u32,
    pub couleur: Color32,
    pub outil: Outil,
    /// Dans le repère de la minimap : (0, 0) en haut à gauche, (1, 1) en
    /// bas à droite de l'image. Une zone en a deux : son centre et un point
    /// du bord.
    pub points: Vec<Pos2>,
    /// Le crayon levé : le compte à rebours part de là. `None` pendant le
    /// tracé, où le trait ne vieillit pas.
    pub fini_le: Option<Instant>,
    /// Son dernier morceau : le trait d'un autre laissé en plan, sans
    /// crayon levé, s'en va après 15 s de silence.
    pub maj: Instant,
}

impl Trait {
    /// Son opacité maintenant.
    fn opacite(&self, maintenant: Instant) -> f32 {
        self.fini_le.map_or(1.0, |fini| opacite(maintenant.saturating_duration_since(fini)))
    }

    fn mort(&self, maintenant: Instant) -> bool {
        self.fini_le.is_some_and(|fini| maintenant.saturating_duration_since(fini) >= VIE)
    }
}

/// L'opacité d'un trait d'après son âge, crayon levé : pleine, puis un
/// fondu adouci les trois dernières secondes, puis rien.
pub fn opacite(age: Duration) -> f32 {
    if age >= VIE {
        return 0.0;
    }
    let reste = VIE - age;
    if reste >= FONDU {
        return 1.0;
    }
    let t = reste.as_secs_f32() / FONDU.as_secs_f32();
    t * t * (3.0 - 2.0 * t)
}

// ---------------------------------------------------------------------
// La vue
// ---------------------------------------------------------------------

/// La carte posée à l'écran : le carré de la minimap dans `cadre`, au zoom
/// donné, avec `centre` (un point de la carte) au milieu du cadre, tournée
/// de `quarts` quarts de tour. Elle passe du repère de la carte à l'écran
/// et retour.
#[derive(Clone, Copy, Debug)]
pub struct Vue {
    pub cadre: Rect,
    pub zoom: f32,
    pub centre: Pos2,
    pub quarts: u8,
}

impl Vue {
    /// Le côté de la carte à l'écran.
    pub fn cote(&self) -> f32 {
        self.cadre.width().min(self.cadre.height()) * self.zoom
    }

    pub fn vers_ecran(&self, p: Pos2) -> Pos2 {
        let k = self.quarts;
        self.cadre.center() + (tourner(p, k) - tourner(self.centre, k)) * self.cote()
    }

    pub fn vers_carte(&self, e: Pos2) -> Pos2 {
        let k = self.quarts;
        tourner(tourner(self.centre, k) + (e - self.cadre.center()) / self.cote(), 4 - k % 4)
    }

    /// Un centre qui garde la carte dans le cadre : au zoom 1 elle est
    /// centrée, plus près on ne la pousse pas au-delà de ses bords.
    fn borner(&mut self) {
        let demi = (0.5 / self.zoom).min(0.5);
        self.centre = pos2(self.centre.x.clamp(demi, 1.0 - demi), self.centre.y.clamp(demi, 1.0 - demi));
    }
}

/// `k` quarts de tour du carré de la carte autour de son centre, dans le
/// sens des aiguilles d'une montre à l'écran (l'axe des y descend).
fn tourner(p: Pos2, k: u8) -> Pos2 {
    match k % 4 {
        0 => p,
        1 => pos2(1.0 - p.y, p.x),
        2 => pos2(1.0 - p.x, 1.0 - p.y),
        _ => pos2(p.y, 1.0 - p.x),
    }
}

// ---------------------------------------------------------------------
// La carte
// ---------------------------------------------------------------------

/// La carte tactique : le plan choisi, les traits vivants, et la vue.
pub struct Tactique {
    pub ouvert: bool,
    pub outil: Outil,
    pub camp: Camp,
    /// Les noms des zones, montrés ou cachés.
    pub noms: bool,
    /// Le plan choisi, par son nom ; vide : le premier proposé.
    pub carte: String,
    traits: Vec<Trait>,
    zoom: f32,
    centre: Pos2,
    /// Le numéro de mon trait en cours de tracé.
    en_cours: Option<u32>,
    prochain_id: u32,
    /// Avec qui la carte se partage, dit à chaque image par l'appli.
    lien: Lien,
    /// Un modérateur m'a rendu muet : je regarde, sans dessiner.
    pub muet: bool,
    /// Le serveur a dit la carte de mon salon (`TactiqueEtat`) : le plan
    /// est le sien.
    du_salon: bool,
    /// L'ouverture est demandée au serveur, la réponse en route.
    ouverture_demandee: bool,
    /// Qui a ouvert la carte du salon, ou choisi son plan.
    par: Option<UserId>,
    /// Les gestes à envoyer au serveur, pris par l'appli à chaque image.
    envois: Vec<ActionTactique>,
    /// Les points de mon trait en cours déjà partis — ceux de ma zone, qui
    /// repartent entiers —, et quand.
    envoyes: usize,
    zone_envoyee: Vec<PointTactique>,
    envoye_le: Option<Instant>,
    /// Le dernier refus du serveur, et quand il est arrivé.
    refus: Option<(String, Instant)>,
}

impl Default for Tactique {
    fn default() -> Self {
        Self {
            ouvert: false,
            outil: Outil::Fleche,
            camp: Camp::Attaque,
            noms: true,
            carte: String::new(),
            traits: Vec::new(),
            zoom: 1.0,
            centre: pos2(0.5, 0.5),
            en_cours: None,
            prochain_id: 1,
            lien: Lien::SansServeur,
            muet: false,
            du_salon: false,
            ouverture_demandee: false,
            par: None,
            envois: Vec::new(),
            envoyes: 0,
            zone_envoyee: Vec::new(),
            envoye_le: None,
            refus: None,
        }
    }
}

impl Tactique {
    /// Le plan choisi parmi ceux proposés ; s'il n'y en a pas encore, le
    /// premier. Celui du salon ne se remplace pas, même absent du
    /// catalogue : la fenêtre le dit.
    pub fn choisi<'a>(&mut self, plans: &'a [Plan]) -> Option<&'a Plan> {
        let connu = plans.iter().any(|p| p.nom == self.carte);
        if !connu && !(self.du_salon && !self.carte.is_empty()) {
            if let Some(p) = plans.first() {
                self.choisir(&p.nom);
            }
        }
        plans.iter().find(|p| p.nom == self.carte)
    }

    /// Avec qui la carte se partage, à chaque image. Quitter le partage
    /// rend la carte à soi seul, vide.
    pub fn relier(&mut self, lien: Lien) {
        if self.lien == Lien::Salon && lien != Lien::Salon {
            self.recevoir_etat(None);
        }
        self.lien = lien;
    }

    /// La carte est celle du salon : ce qu'on y fait part au serveur.
    fn partagee(&self) -> bool {
        self.lien == Lien::Salon
    }

    /// Les gestes à envoyer au serveur depuis la dernière fois.
    pub fn a_envoyer(&mut self) -> Vec<ActionTactique> {
        std::mem::take(&mut self.envois)
    }

    /// Un geste refusé par le serveur, à montrer quelques secondes.
    pub fn refuser(&mut self, message: String) {
        self.refus = Some((message, Instant::now()));
    }

    /// La carte du salon, tout entière (`TactiqueEtat`) : son plan, puis
    /// ses traits, qui suivent un par un. `None` : plus de carte du salon.
    pub fn recevoir_etat(&mut self, carte: Option<CarteTactique>) {
        self.traits.clear();
        self.en_cours = None;
        self.ouverture_demandee = false;
        match carte {
            Some(c) => {
                self.du_salon = true;
                self.par = Some(c.par);
                if self.carte != c.carte {
                    self.carte = c.carte;
                    self.recadrer();
                }
            }
            None => {
                self.du_salon = false;
                self.par = None;
            }
        }
    }

    /// Ce qui change sur la carte du salon. Mes propres morceaux ne me
    /// reviennent pas ; ceux qui arrivent quand même (mes traits rejoués à
    /// une reconnexion) prennent la place qu'ils disent.
    pub fn recevoir(&mut self, evenement: EvenementTactique, moi: UserId, couleur_de: &dyn Fn(UserId) -> Color32) {
        let maintenant = Instant::now();
        match evenement {
            EvenementTactique::Plan { carte, par } => {
                self.par = Some(par);
                // Mon propre choix, déjà fait ici : rien à effacer de ce
                // que j'ai tracé depuis.
                if par == moi && carte == self.carte {
                    return;
                }
                self.choisir(&carte);
                self.traits.clear();
                self.en_cours = None;
            }
            EvenementTactique::Trait { auteur, id, outil, depuis, points, fin, reste_ms } => {
                if auteur == moi && self.en_cours == Some(id) {
                    return;
                }
                let Some(outil) = Outil::du_fil(outil) else { return };
                if auteur == moi {
                    self.prochain_id = self.prochain_id.max(id.wrapping_add(1)).max(1);
                }
                let i = match self.traits.iter().position(|t| t.auteur == auteur && t.id == id) {
                    Some(i) => i,
                    None => {
                        let t = Trait {
                            auteur,
                            id,
                            couleur: couleur_de(auteur),
                            outil,
                            points: Vec::new(),
                            fini_le: None,
                            maj: maintenant,
                        };
                        self.traits.push(t);
                        self.limiter(auteur);
                        match self.traits.iter().position(|t| t.auteur == auteur && t.id == id) {
                            Some(i) => i,
                            None => return,
                        }
                    }
                };
                let t = &mut self.traits[i];
                t.points.truncate(depuis as usize);
                let place = POINTS_MAX.saturating_sub(t.points.len());
                t.points.extend(points.into_iter().take(place).map(du_fil));
                t.maj = maintenant;
                if fin {
                    let reste = reste_ms.map_or(VIE, |ms| Duration::from_millis(ms as u64).min(VIE));
                    t.fini_le = Some(maintenant.checked_sub(VIE - reste).unwrap_or(maintenant));
                }
            }
            EvenementTactique::Retire { auteur, id } => {
                self.traits.retain(|t| (t.auteur, t.id) != (auteur, id));
                if auteur == moi && self.en_cours == Some(id) {
                    self.en_cours = None;
                }
            }
            EvenementTactique::RetireTout { auteur } => {
                self.traits.retain(|t| t.auteur != auteur);
                if auteur == moi {
                    self.en_cours = None;
                }
            }
            EvenementTactique::Inconnu => {}
        }
    }

    /// Le plan choisi dans la liste : chez tout le salon si la carte est la
    /// sienne.
    fn changer_de_plan(&mut self, carte: &str) {
        self.choisir(carte);
        if self.partagee() && self.du_salon {
            self.envois.push(ActionTactique::Choisir { carte: carte.to_string() });
        }
    }

    /// Ce qui est neuf dans mon trait en cours part au salon : tout de
    /// suite au premier morceau et au crayon levé, sinon toutes les 50 ms.
    /// Une zone repart entière, ses deux points, à chaque changement.
    fn pousser_mon_trait(&mut self, moi: UserId, maintenant: Instant, fin: bool) {
        if !self.partagee() || !self.du_salon {
            return;
        }
        let Some(id) = self.en_cours else { return };
        let Some(t) = self.traits.iter().find(|t| t.auteur == moi && t.id == id) else { return };
        let outil = t.outil;
        let fil: Vec<PointTactique> = t.points.iter().map(|p| vers_fil(*p)).collect();
        let neufs = match outil {
            Outil::Zone if fin || fil != self.zone_envoyee => fil.clone(),
            Outil::Zone => Vec::new(),
            Outil::Crayon | Outil::Fleche => fil[self.envoyes.min(fil.len())..].to_vec(),
        };
        let du = self.envoye_le.is_none_or(|e| maintenant.saturating_duration_since(e) >= MORCEAU);
        if !(fin || (du && !neufs.is_empty())) {
            return;
        }
        self.envoyes = fil.len();
        self.zone_envoyee = if outil == Outil::Zone { fil } else { Vec::new() };
        self.envoye_le = Some(maintenant);
        let mut morceaux: Vec<Vec<PointTactique>> =
            neufs.chunks(ki_protocol::TACTIQUE_POINTS_PAR_MORCEAU).map(<[PointTactique]>::to_vec).collect();
        if morceaux.is_empty() {
            morceaux.push(Vec::new());
        }
        let dernier = morceaux.len() - 1;
        for (k, points) in morceaux.into_iter().enumerate() {
            self.envois.push(ActionTactique::Trait { id, outil: outil.vers_fil(), points, fin: fin && k == dernier });
        }
    }

    /// Des points de mon trait attendent leur morceau.
    fn reste_a_pousser(&self, moi: UserId) -> bool {
        self.partagee()
            && self.du_salon
            && self.en_cours.is_some_and(|id| {
                self.traits.iter().any(|t| t.auteur == moi && t.id == id && t.outil != Outil::Zone && t.points.len() > self.envoyes)
            })
    }

    /// Un trait qui arrive tout fait — d'un banc d'essai aujourd'hui, du
    /// salon demain. Ses points sont bornés à la carte ; un trait du même
    /// auteur et du même numéro est remplacé.
    pub fn ajouter(&mut self, mut t: Trait) {
        t.points.truncate(POINTS_MAX);
        for p in &mut t.points {
            *p = borner_au_plan(*p);
        }
        let auteur = t.auteur;
        self.traits.retain(|u| (u.auteur, u.id) != (t.auteur, t.id));
        self.traits.push(t);
        self.limiter(auteur);
    }

    /// Change de plan : les traits de l'ancien n'y ont plus de sens.
    pub fn choisir(&mut self, carte: &str) {
        if self.carte != carte {
            self.carte = carte.to_string();
            self.traits.clear();
            self.en_cours = None;
            self.recadrer();
        }
    }

    /// Retire le dernier trait de `moi`, chez tout le salon si la carte
    /// est la sienne.
    pub fn annuler(&mut self, moi: UserId) {
        if let Some(i) = self.traits.iter().rposition(|t| t.auteur == moi) {
            let t = self.traits.remove(i);
            if self.en_cours == Some(t.id) {
                self.en_cours = None;
            }
            if self.partagee() && self.du_salon {
                self.envois.push(ActionTactique::Annuler { id: t.id });
            }
        }
    }

    /// Retire tous les traits de `moi`, de même.
    pub fn effacer_les_miens(&mut self, moi: UserId) {
        let avant = self.traits.len();
        self.traits.retain(|t| t.auteur != moi);
        self.en_cours = None;
        if self.partagee() && self.du_salon && self.traits.len() < avant {
            self.envois.push(ActionTactique::EffacerLesMiens);
        }
    }

    /// La carte entière, centrée.
    pub fn recadrer(&mut self) {
        self.zoom = 1.0;
        self.centre = pos2(0.5, 0.5);
    }

    /// Les traits morts s'en vont, et ceux des autres laissés en plan —
    /// sans crayon levé ni morceau depuis 15 s.
    fn purger(&mut self, maintenant: Instant, moi: UserId) {
        self.traits.retain(|t| {
            let en_plan = t.auteur != moi && t.fini_le.is_none() && maintenant.saturating_duration_since(t.maj) >= VIE;
            !t.mort(maintenant) && !en_plan
        });
    }

    /// Au-delà de [`TRAITS_MAX`] traits d'un auteur, les plus anciens des
    /// siens partent.
    fn limiter(&mut self, auteur: UserId) {
        let siens = self.traits.iter().filter(|t| t.auteur == auteur).count();
        let mut trop = siens.saturating_sub(TRAITS_MAX);
        self.traits.retain(|t| {
            if trop > 0 && t.auteur == auteur {
                trop -= 1;
                false
            } else {
                true
            }
        });
    }

    /// Mon trait en cours de tracé.
    fn mon_trait(&mut self, moi: UserId) -> Option<&mut Trait> {
        let id = self.en_cours?;
        self.traits.iter_mut().find(|t| t.auteur == moi && t.id == id)
    }

    /// Quand redessiner sans que rien ne bouge : au début du prochain
    /// fondu, puis à chaque image pendant qu'il dure. Une carte sans trait
    /// ne coûte rien.
    fn prochain_repeint(&self, maintenant: Instant) -> Option<Duration> {
        self.traits
            .iter()
            .filter_map(|t| t.fini_le)
            .map(|fini| {
                let age = maintenant.saturating_duration_since(fini);
                (VIE - FONDU).saturating_sub(age).max(REPEINT)
            })
            .min()
    }

    /// La fenêtre, si elle est ouverte.
    pub fn fenetre(
        &mut self,
        ctx: &egui::Context,
        plans: &[Plan],
        minimap: Option<&egui::TextureHandle>,
        moi: UserId,
        ma_couleur: Color32,
    ) {
        if !self.ouvert {
            return;
        }
        let ecran = ctx.content_rect();
        let (max_l, max_h) = ((ecran.width() - 40.0).max(380.0), (ecran.height() - 40.0).max(360.0));
        let mut ouvert = true;
        egui::Window::new("Carte tactique")
            .id(egui::Id::new("carte_tactique"))
            .open(&mut ouvert)
            .collapsible(false)
            .resizable(true)
            .default_size([760.0_f32.min(max_l), 820.0_f32.min(max_h)])
            .min_size([420.0_f32.min(max_l), 460.0_f32.min(max_h)])
            .max_size([max_l, max_h])
            .show(ctx, |ui| self.contenu(ui, plans, minimap, moi, ma_couleur));
        self.ouvert = ouvert;
    }

    /// Le contenu de la fenêtre : la barre d'outils, puis la carte dans
    /// toute la place qui reste. `minimap` est celle du plan choisi
    /// ([`Tactique::choisi`]), si elle est arrivée.
    pub fn contenu(
        &mut self,
        ui: &mut egui::Ui,
        plans: &[Plan],
        minimap: Option<&egui::TextureHandle>,
        moi: UserId,
        ma_couleur: Color32,
    ) {
        let maintenant = Instant::now();
        self.purger(maintenant, moi);
        let plan = self.choisi(plans);
        // La carte du salon s'ouvre au premier regard, sur le plan choisi
        // ici ; déjà ouverte, le serveur l'aurait dite à l'arrivée.
        if self.partagee() && !self.du_salon && !self.ouverture_demandee && !self.carte.is_empty() {
            self.envois.push(ActionTactique::Ouvrir { carte: self.carte.clone() });
            self.ouverture_demandee = true;
        }
        self.barre(ui, plans, moi);
        ui.add_space(espace::S);
        let (zone, reponse) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let quarts = plan.map_or(0, |p| p.quarts) + if self.camp == Camp::Defense { 2 } else { 0 };
        let mut vue = Vue { cadre: zone.shrink(espace::S), zoom: self.zoom, centre: self.centre, quarts };
        self.gestes(ui, &reponse, &mut vue, moi, ma_couleur, maintenant);
        self.zoom = vue.zoom;
        self.centre = vue.centre;
        let p = ui.painter_at(zone);
        p.rect_filled(zone, rayon::XL, c::melanger(c::BG_DEEP, c::BG_RAISED, 0.35));
        match plan {
            Some(plan) => self.dessiner(&p, &vue, plan, minimap, maintenant),
            None => {
                let attente = if plans.is_empty() {
                    "le catalogue des cartes arrive…".to_string()
                } else {
                    format!("« {} » n'est pas dans ton catalogue : mets ki-chat à jour", self.carte)
                };
                p.text(zone.center(), Align2::CENTER_CENTER, attente, FontId::proportional(texte::COURANT), c::TEXT_FAINT);
            }
        }
        p.rect_stroke(zone, rayon::XL, Stroke::new(1.0, c::BORDER), StrokeKind::Inside);
        let morceau = self.reste_a_pousser(moi).then_some(MORCEAU);
        if let Some(dans) = self.prochain_repeint(maintenant).into_iter().chain(morceau).min() {
            ui.ctx().request_repaint_after(dans);
        }
    }

    /// Le plan, les camps, les noms, les outils, et ce qu'on efface.
    fn barre(&mut self, ui: &mut egui::Ui, plans: &[Plan], moi: UserId) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = espace::M;
            let mut choisie = self.carte.clone();
            egui::ComboBox::from_id_salt("tactique_plan")
                .width(130.0)
                .selected_text(if choisie.is_empty() { "Choisir une carte" } else { choisie.as_str() })
                .show_ui(ui, |ui| {
                    for p in plans {
                        ui.selectable_value(&mut choisie, p.nom.clone(), p.nom.as_str());
                    }
                });
            if choisie != self.carte {
                self.changer_de_plan(&choisie);
            }
            ui.scope(|ui| {
                ui.set_max_width(210.0);
                ui::segmente(ui, &mut self.camp, &[(Camp::Attaque, "Attaque"), (Camp::Defense, "Défense")]);
            });
            ui::interrupteur(ui, &mut self.noms, "Noms des zones");
        });
        ui.add_space(espace::XS);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = espace::S;
            for (outil, nom, touche) in Outil::TOUS {
                let libelle = format!("{nom}  {touche}");
                let bouton = if self.outil == outil {
                    ui::tinted_button(ui, Some(outil.icone()), &libelle, ui::Tone::Accent)
                } else {
                    ui::button(ui, outil.icone(), &libelle)
                };
                if bouton.clicked() {
                    self.outil = outil;
                }
            }
            ui.add_space(espace::M);
            if ui::icon_button(ui, Icon::Repeat, "Annuler mon dernier trait (Ctrl+Z)").clicked() {
                self.annuler(moi);
            }
            if ui::icon_button(ui, Icon::Trash, "Effacer mes traits").clicked() {
                self.effacer_les_miens(moi);
            }
        });
        let refus = self.refus.as_ref().filter(|(_, quand)| quand.elapsed() < REFUS_AFFICHE).map(|(m, _)| m.as_str());
        let partage = match (self.lien, self.muet) {
            _ if refus.is_some() => refus.unwrap_or_default(),
            (Lien::Salon, true) => "un modérateur t'a rendu muet : tu regardes la carte du salon, sans y dessiner",
            (Lien::Salon, false) => "partagée avec ton salon vocal",
            (Lien::HorsVocal, _) => "rien que pour toi : rejoins un salon vocal pour la partager",
            (Lien::SansServeur, _) => "rien que pour toi : ce serveur ne partage pas encore la carte",
        };
        ui::hint(
            ui,
            format!(
                "{partage} · chaque trait s'efface 15 s après qu'on lève le crayon · molette : zoom · \
                 clic du milieu : déplacer · double-clic : recadrer · N : noms"
            ),
        );
    }

    /// Le zoom à la molette (autour du pointeur), le déplacement au clic du
    /// milieu, le recadrage au double-clic, les touches, et le tracé au
    /// clic principal.
    fn gestes(&mut self, ui: &egui::Ui, r: &egui::Response, vue: &mut Vue, moi: UserId, couleur: Color32, maintenant: Instant) {
        if let Some(sous) = r.hover_pos() {
            let molette = ui.input(|i| i.smooth_scroll_delta.y);
            if molette != 0.0 {
                let avant = vue.vers_carte(sous);
                vue.zoom = (vue.zoom * (molette * 0.0025).exp()).clamp(1.0, ZOOM_MAX);
                // Le point sous le pointeur reste sous le pointeur.
                let apres = vue.vers_carte(sous);
                vue.centre += avant - apres;
                vue.borner();
            }
            if !ui.ctx().egui_wants_keyboard_input() {
                let (un, deux, trois, noms, annuler) = ui.input(|i| {
                    (
                        i.key_pressed(egui::Key::Num1),
                        i.key_pressed(egui::Key::Num2),
                        i.key_pressed(egui::Key::Num3),
                        i.key_pressed(egui::Key::N),
                        i.modifiers.command && i.key_pressed(egui::Key::Z),
                    )
                });
                if un {
                    self.outil = Outil::Crayon;
                } else if deux {
                    self.outil = Outil::Fleche;
                } else if trois {
                    self.outil = Outil::Zone;
                }
                if noms {
                    self.noms = !self.noms;
                }
                if annuler {
                    self.annuler(moi);
                }
            }
        }
        if r.dragged_by(PointerButton::Middle) {
            vue.centre = vue.vers_carte(vue.cadre.center() - r.drag_delta());
            vue.borner();
        }
        if r.double_clicked() {
            vue.zoom = 1.0;
            vue.centre = pos2(0.5, 0.5);
            // Le double-clic a commencé par un clic, qui a posé un point :
            // il s'en va avec.
            self.en_cours = None;
            if let Some(i) = self.traits.iter().rposition(|t| t.auteur == moi) {
                if self.traits[i].points.len() <= 1 {
                    self.traits.remove(i);
                }
            }
            return;
        }
        // Muet sur la carte du salon : on regarde, on zoome, on ne dessine
        // pas.
        let dessine = !(self.muet && self.partagee());
        if let Some(ecran) = r.interact_pointer_pos().filter(|_| dessine) {
            let commence = r.drag_started_by(PointerButton::Primary) || r.clicked_by(PointerButton::Primary);
            if commence && self.en_cours.is_none() {
                // Le premier point là où le bouton s'est enfoncé, pas là où
                // egui a reconnu le glisser : le centre d'une zone tombe juste.
                let depart = ui.input(|i| i.pointer.press_origin()).unwrap_or(ecran);
                let id = self.prochain_id;
                self.prochain_id = self.prochain_id.wrapping_add(1).max(1);
                let points = vec![borner_au_plan(vue.vers_carte(depart))];
                self.ajouter(Trait { auteur: moi, id, couleur, outil: self.outil, points, fini_le: None, maj: maintenant });
                self.en_cours = Some(id);
                self.envoyes = 0;
                self.zone_envoyee.clear();
                self.envoye_le = None;
            }
            if r.dragged_by(PointerButton::Primary) {
                let ici = borner_au_plan(vue.vers_carte(ecran));
                if let Some(t) = self.mon_trait(moi) {
                    match t.outil {
                        Outil::Zone => {
                            t.points.truncate(1);
                            t.points.push(ici);
                        }
                        Outil::Crayon | Outil::Fleche => {
                            let loin = t.points.last().is_none_or(|&d| vue.vers_ecran(d).distance(ecran) >= ECART_MIN_PX);
                            if loin && t.points.len() < POINTS_MAX {
                                t.points.push(ici);
                            }
                        }
                    }
                }
            }
        }
        // Le crayon se lève au relâcher, où qu'il ait lieu : un trait ne
        // reste jamais « en cours » sans bouton enfoncé.
        let leve = self.en_cours.is_some() && !ui.input(|i| i.pointer.primary_down());
        if leve {
            if let Some(t) = self.mon_trait(moi) {
                t.fini_le = Some(maintenant);
            }
        }
        self.pousser_mon_trait(moi, maintenant, leve);
        if leve {
            self.en_cours = None;
        }
    }

    /// La minimap, les noms des zones, puis les traits.
    fn dessiner(&self, p: &egui::Painter, vue: &Vue, plan: &Plan, minimap: Option<&egui::TextureHandle>, maintenant: Instant) {
        match minimap {
            Some(minimap) => {
                // Les coins de l'image suivent ceux de la carte, quelle que
                // soit la vue : deux triangles plutôt qu'un rectangle.
                let mut maillage = egui::Mesh::with_texture(minimap.id());
                let teinte = Color32::from_white_alpha(235);
                for coin in [pos2(0.0, 0.0), pos2(1.0, 0.0), pos2(1.0, 1.0), pos2(0.0, 1.0)] {
                    maillage.vertices.push(Vertex { pos: vue.vers_ecran(coin), uv: coin, color: teinte });
                }
                maillage.add_triangle(0, 1, 2);
                maillage.add_triangle(0, 2, 3);
                p.add(Shape::mesh(maillage));
            }
            None => {
                // En attendant l'image : un quadrillage, la carte se lit
                // quand même par ses zones.
                let ligne = Stroke::new(1.0, c::BORDER_SOFT);
                for k in 0..=8 {
                    let t = k as f32 / 8.0;
                    p.line_segment([vue.vers_ecran(pos2(t, 0.0)), vue.vers_ecran(pos2(t, 1.0))], ligne);
                    p.line_segment([vue.vers_ecran(pos2(0.0, t)), vue.vers_ecran(pos2(1.0, t))], ligne);
                }
            }
        }
        if self.noms {
            // À la taille de la carte à l'écran : une petite fenêtre ne
            // noie pas son plan sous les noms, le zoom les grossit un peu.
            let police = FontId::proportional((vue.cote() / 58.0).clamp(8.5, texte::COURANT));
            for (nom, u, v) in &plan.zones {
                let ou = vue.vers_ecran(pos2(*u, *v));
                p.text(ou + vec2(1.0, 1.0), Align2::CENTER_CENTER, nom, police.clone(), Color32::from_black_alpha(200));
                p.text(ou, Align2::CENTER_CENTER, nom, police.clone(), c::melanger(c::TEXT_DIM, c::TEXT, 0.4));
            }
        }
        let epaisseur = (2.5 + 1.5 * vue.zoom).min(9.0);
        for t in &self.traits {
            let a = t.opacite(maintenant);
            if a <= 0.0 {
                continue;
            }
            let couleur = t.couleur.gamma_multiply(a);
            let points: Vec<Pos2> = t.points.iter().map(|&q| vue.vers_ecran(q)).collect();
            match t.outil {
                Outil::Zone => {
                    let Some(&centre) = points.first() else { continue };
                    let r = points.get(1).map_or(0.0, |b| centre.distance(*b)).max(epaisseur);
                    p.circle(centre, r, couleur.gamma_multiply(0.14), Stroke::new(epaisseur * 0.8, couleur));
                }
                Outil::Crayon | Outil::Fleche => {
                    if points.len() == 1 {
                        p.circle_filled(points[0], epaisseur * 0.7, couleur);
                        continue;
                    }
                    let lisse = lisser(&points);
                    // Des bouts ronds : egui trace des bouts carrés.
                    p.circle_filled(lisse[0], epaisseur / 2.0, couleur);
                    if t.outil == Outil::Fleche {
                        pointe(p, &lisse, epaisseur, couleur);
                    } else if let Some(&fin) = lisse.last() {
                        p.circle_filled(fin, epaisseur / 2.0, couleur);
                    }
                    p.add(Shape::line(lisse, Stroke::new(epaisseur, couleur)));
                }
            }
        }
    }
}

/// Un point gardé sur la carte.
fn borner_au_plan(p: Pos2) -> Pos2 {
    pos2(p.x.clamp(0.0, 1.0), p.y.clamp(0.0, 1.0))
}

/// Un tracé à main levée, lissé à l'affichage seulement — les points
/// gardés restent ceux du pointeur : une spline de Catmull-Rom, quatre
/// pas entre deux points.
pub fn lisser(points: &[Pos2]) -> Vec<Pos2> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let n = points.len();
    let mut sortie = Vec::with_capacity(n * 4);
    for i in 0..n - 1 {
        let p0 = points[i.saturating_sub(1)];
        let (p1, p2) = (points[i], points[i + 1]);
        let p3 = points[(i + 2).min(n - 1)];
        for k in 0..4 {
            let t = k as f32 / 4.0;
            let (t2, t3) = (t * t, t * t * t);
            let x = 0.5
                * (2.0 * p1.x
                    + (p2.x - p0.x) * t
                    + (2.0 * p0.x - 5.0 * p1.x + 4.0 * p2.x - p3.x) * t2
                    + (3.0 * p1.x - p0.x - 3.0 * p2.x + p3.x) * t3);
            let y = 0.5
                * (2.0 * p1.y
                    + (p2.y - p0.y) * t
                    + (2.0 * p0.y - 5.0 * p1.y + 4.0 * p2.y - p3.y) * t2
                    + (3.0 * p1.y - p0.y - 3.0 * p2.y + p3.y) * t3);
            sortie.push(pos2(x, y));
        }
    }
    sortie.push(points[n - 1]);
    sortie
}

/// La pointe d'une flèche, dans la direction de ses derniers pixels.
fn pointe(p: &egui::Painter, ligne: &[Pos2], epaisseur: f32, couleur: Color32) {
    let Some(&bout) = ligne.last() else { return };
    let recul = epaisseur * 3.0;
    let depuis = ligne.iter().rev().find(|q| q.distance(bout) >= recul).copied().unwrap_or(ligne[0]);
    let dir = (bout - depuis).normalized();
    if !dir.x.is_finite() || !dir.y.is_finite() || dir == egui::Vec2::ZERO {
        return;
    }
    let long = epaisseur * 3.2;
    let large = epaisseur * 2.2;
    let base = bout - dir * long * 0.6;
    let cote = vec2(-dir.y, dir.x) * large;
    p.add(Shape::convex_polygon(vec![bout + dir * long * 0.4, base + cote, base - cote], couleur, Stroke::NONE));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Douze secondes pleines, trois de fondu, puis plus rien.
    #[test]
    fn un_trait_vit_quinze_secondes() {
        assert_eq!(opacite(Duration::ZERO), 1.0);
        assert_eq!(opacite(Duration::from_secs(12)), 1.0);
        let milieu = opacite(Duration::from_millis(13_500));
        assert!(milieu > 0.4 && milieu < 0.6, "{milieu}");
        assert!(opacite(Duration::from_millis(14_900)) < 0.05);
        assert_eq!(opacite(VIE), 0.0);
        assert_eq!(opacite(Duration::from_secs(60)), 0.0);
        let t0 = Instant::now();
        let t = Trait {
            auteur: 1,
            id: 1,
            couleur: Color32::WHITE,
            outil: Outil::Crayon,
            points: vec![pos2(0.5, 0.5)],
            fini_le: None,
            maj: t0,
        };
        assert_eq!(t.opacite(t0 + Duration::from_secs(100)), 1.0, "en cours de tracé, il ne vieillit pas");
        let t = Trait { fini_le: Some(t0), ..t };
        assert!(!t.mort(t0 + Duration::from_secs(14)));
        assert!(t.mort(t0 + VIE));
    }

    /// La carte va à l'écran et revient, quelle que soit la vue ; un quart
    /// de tour met la droite en bas ; le zoom garde le centre au milieu.
    #[test]
    fn la_vue_va_et_revient() {
        let cadre = Rect::from_min_size(pos2(100.0, 50.0), vec2(600.0, 400.0));
        for quarts in 0..6 {
            for (zoom, centre) in [(1.0, pos2(0.5, 0.5)), (2.5, pos2(0.3, 0.7))] {
                let vue = Vue { cadre, zoom, centre, quarts };
                for q in [pos2(0.0, 0.0), pos2(0.25, 0.8), pos2(1.0, 1.0)] {
                    let retour = vue.vers_carte(vue.vers_ecran(q));
                    assert!(retour.distance(q) < 1e-4, "{quarts} {zoom} {q:?} → {retour:?}");
                }
                assert!(vue.vers_ecran(centre).distance(cadre.center()) < 1e-3);
            }
        }
        let droit = Vue { cadre, zoom: 1.0, centre: pos2(0.5, 0.5), quarts: 0 };
        assert_eq!(droit.cote(), 400.0, "le carré tient dans le petit côté");
        assert!(droit.vers_ecran(pos2(0.0, 0.0)).x < droit.vers_ecran(pos2(1.0, 0.0)).x);
        let quart = Vue { quarts: 1, ..droit };
        let bas = droit.vers_ecran(pos2(0.5, 1.0));
        assert!(quart.vers_ecran(pos2(1.0, 0.5)).distance(bas) < 1e-3, "la droite passe en bas");
        let demi = Vue { quarts: 2, ..droit };
        assert!(demi.vers_ecran(pos2(0.0, 0.0)).distance(droit.vers_ecran(pos2(1.0, 1.0))) < 1e-3, "un demi-tour");
        let mut loin = Vue { zoom: 2.0, centre: pos2(0.0, 1.0), ..droit };
        loin.borner();
        assert_eq!(loin.centre, pos2(0.25, 0.75), "on ne pousse pas la carte au-delà de ses bords");
    }

    /// Les traits d'un auteur sont bornés, ses gestes ne touchent que les
    /// siens, un même numéro remplace, un autre plan efface tout, et les
    /// morts s'en vont.
    #[test]
    fn les_traits_se_tiennent() {
        let t0 = Instant::now();
        let trait_de = |auteur, id, fini_le| Trait {
            auteur,
            id,
            couleur: Color32::WHITE,
            outil: Outil::Fleche,
            points: vec![pos2(2.0, -1.0)],
            fini_le,
            maj: t0,
        };
        let mut tac = Tactique::default();
        tac.ajouter(trait_de(1, 1, Some(t0)));
        assert_eq!(tac.traits[0].points[0], pos2(1.0, 0.0), "borné à la carte");
        for id in 0..TRAITS_MAX as u32 + 5 {
            tac.ajouter(trait_de(2, id, Some(t0)));
        }
        assert_eq!(tac.traits.iter().filter(|t| t.auteur == 2).count(), TRAITS_MAX);
        assert_eq!(tac.traits.iter().filter(|t| t.auteur == 2).map(|t| t.id).min(), Some(5), "les plus anciens partent");
        assert_eq!(tac.traits.iter().filter(|t| t.auteur == 1).count(), 1, "les miens ne paient pas pour les siens");
        tac.ajouter(trait_de(1, 1, None));
        assert_eq!(tac.traits.iter().filter(|t| t.auteur == 1).count(), 1, "le même numéro remplace");
        tac.annuler(1);
        assert_eq!(tac.traits.iter().filter(|t| t.auteur == 1).count(), 0);
        tac.ajouter(trait_de(1, 2, None));
        tac.effacer_les_miens(2);
        assert_eq!(tac.traits.len(), 1);
        tac.ajouter(trait_de(3, 1, Some(t0)));
        tac.purger(t0 + VIE, 1);
        assert_eq!(tac.traits.len(), 1, "le mort part, celui qu'on trace reste");
        tac.carte = "Ascent".into();
        tac.choisir("Lotus");
        assert!(tac.traits.is_empty());
        assert_eq!(tac.carte, "Lotus");
    }

    /// On ne redessine qu'au début du fondu, puis à chaque image pendant.
    #[test]
    fn la_carte_ne_se_repeint_que_pour_un_fondu() {
        let t0 = Instant::now();
        let mut tac = Tactique::default();
        assert_eq!(tac.prochain_repeint(t0), None, "une carte vide ne coûte rien");
        let t = Trait {
            auteur: 1,
            id: 1,
            couleur: Color32::WHITE,
            outil: Outil::Crayon,
            points: vec![],
            fini_le: None,
            maj: t0,
        };
        tac.ajouter(t.clone());
        assert_eq!(tac.prochain_repeint(t0), None, "en cours de tracé, le pointeur suffit");
        tac.ajouter(Trait { id: 2, fini_le: Some(t0), ..t });
        assert_eq!(tac.prochain_repeint(t0 + Duration::from_secs(2)), Some(Duration::from_secs(10)));
        assert_eq!(tac.prochain_repeint(t0 + Duration::from_secs(13)), Some(REPEINT));
    }

    /// Le lissage passe par les points du pointeur et ne touche pas à un
    /// tracé trop court pour une courbe.
    #[test]
    fn le_lissage_passe_par_les_points() {
        let points = [pos2(0.0, 0.0), pos2(10.0, 0.0), pos2(10.0, 10.0), pos2(20.0, 10.0)];
        let lisse = lisser(&points);
        assert_eq!(lisse.len(), 3 * 4 + 1);
        for p in points {
            assert!(lisse.iter().any(|q| q.distance(p) < 1e-4), "{p:?}");
        }
        assert_eq!(lisser(&points[..2]), points[..2].to_vec());
    }

    /// Une carte de `/v1/maps` (Ascent, quelques zones) devient un plan :
    /// les facteurs croisés, les noms de zones, et le quart de tour qui met
    /// l'attaque en bas. Un mode sans sites n'en est pas un.
    #[test]
    fn un_plan_se_lit_dans_le_catalogue() {
        let ascent = serde_json::json!({
            "displayName": "Ascent",
            "tacticalDescription": "A/B Sites",
            "displayIcon": "https://media.valorant-api.com/maps/7eaecc1b/displayicon.png",
            "xMultiplier": 7e-05, "yMultiplier": -7e-05, "xScalarToAdd": 0.813895, "yScalarToAdd": 0.573242,
            "callouts": [
                {"regionName": "Site", "superRegionName": "A", "location": {"x": 6153.585, "y": -6626.2114, "z": 500.0}},
                {"regionName": "Spawn", "superRegionName": "Attacker Side", "location": {"x": 60.0, "y": 50.0, "z": 190.0}},
                {"regionName": "Spawn", "superRegionName": "Defender Side", "location": {"x": 1995.2354, "y": -9744.923, "z": 300.0}},
                {"regionName": "Site", "superRegionName": "A", "location": {"x": 6153.0, "y": -6626.0, "z": 500.0}},
                {"regionName": "Ailleurs", "superRegionName": "Mid", "location": {"x": 90000.0, "y": 0.0, "z": 0.0}},
                {"regionName": "Courtyard", "superRegionName": "Mid"}
            ]
        });
        let plan = plan_depuis(&ascent).expect("un plan");
        assert_eq!(plan.nom, "Ascent");
        assert_eq!(plan.quarts, 1, "l'attaque est à droite de la minimap");
        let noms: Vec<&str> = plan.zones.iter().map(|(n, ..)| n.as_str()).collect();
        assert_eq!(noms, ["A Site", "Attacker Spawn", "Defender Spawn"], "sans doublon, ni zone hors carte ou sans place");
        let (_, u, v) = plan.zones[0];
        assert!((u - 0.350).abs() < 0.01 && (v - 0.142).abs() < 0.01, "{u} {v}");
        // Et le disque le relit tel quel.
        let relu: Plan = serde_json::from_slice(&serde_json::to_vec(&plan).unwrap()).unwrap();
        assert_eq!(relu, plan);

        let mut match_a_mort = ascent.clone();
        match_a_mort["tacticalDescription"] = Value::Null;
        assert_eq!(plan_depuis(&match_a_mort), None);
        let mut sans_facteur = ascent;
        sans_facteur["xMultiplier"] = serde_json::json!(0.0);
        assert_eq!(plan_depuis(&sans_facteur), None);
    }

    /// Le spawn d'attaque finit en bas, d'où qu'il parte.
    #[test]
    fn l_attaque_se_met_en_bas() {
        let milieu = pos2(0.5, 0.5);
        for (attaque, quarts) in [(pos2(0.5, 0.9), 0), (pos2(0.9, 0.5), 1), (pos2(0.5, 0.1), 2), (pos2(0.1, 0.5), 3)] {
            let defense = milieu + (milieu - attaque);
            assert_eq!(quarts_pour(Some(attaque), Some(defense)), quarts, "{attaque:?}");
            let bas = tourner(attaque, quarts);
            assert!(bas.y > 0.85 && (bas.x - 0.5).abs() < 1e-4, "{attaque:?} → {bas:?}");
        }
        assert_eq!(quarts_pour(None, Some(milieu)), 0);
    }

    fn carte_du_salon(tac: &mut Tactique) {
        tac.relier(Lien::Salon);
        tac.recevoir_etat(Some(CarteTactique { salon: 4, carte: "Ascent".into(), par: 2 }));
    }

    fn trait_recu(auteur: UserId, id: u32, depuis: u32, points: Vec<PointTactique>, fin: bool, reste_ms: Option<u32>) -> EvenementTactique {
        EvenementTactique::Trait { auteur, id, outil: OutilTactique::Fleche, depuis, points, fin, reste_ms }
    }

    /// Les traits des autres se montent morceau par morceau, chacun à sa
    /// place : un morceau reçu deux fois ne double rien, le temps qui reste
    /// se reprend, un outil inconnu ne se dessine pas, et les retraits
    /// retirent.
    #[test]
    fn les_traits_du_salon_se_montent() {
        let mut tac = Tactique::default();
        carte_du_salon(&mut tac);
        let couleur = |_| Color32::RED;
        tac.recevoir(trait_recu(2, 1, 0, vec![[0, 0], [100, 100]], false, None), 1, &couleur);
        tac.recevoir(trait_recu(2, 1, 2, vec![[200, 200]], false, None), 1, &couleur);
        tac.recevoir(trait_recu(2, 1, 2, vec![[200, 200]], false, None), 1, &couleur);
        assert_eq!(tac.traits.len(), 1);
        assert_eq!(tac.traits[0].points.len(), 3, "le morceau en double ne double rien");
        assert_eq!(tac.traits[0].couleur, Color32::RED);
        assert!(tac.traits[0].fini_le.is_none());
        tac.recevoir(trait_recu(2, 1, 3, vec![], true, None), 1, &couleur);
        assert!(tac.traits[0].fini_le.is_some_and(|f| f.elapsed() < Duration::from_secs(1)), "15 s entières");

        tac.recevoir(trait_recu(3, 1, 0, vec![[1, 1]], true, Some(4_000)), 1, &couleur);
        let reste = VIE.saturating_sub(tac.traits[1].fini_le.unwrap().elapsed());
        assert!(reste <= Duration::from_secs(4) && reste > Duration::from_millis(3_500), "{reste:?}");
        let futur = EvenementTactique::Trait {
            auteur: 3,
            id: 2,
            outil: OutilTactique::Inconnu,
            depuis: 0,
            points: vec![[1, 1]],
            fin: true,
            reste_ms: None,
        };
        tac.recevoir(futur, 1, &couleur);
        assert_eq!(tac.traits.len(), 2, "un outil d'après ne se dessine pas");

        tac.recevoir(EvenementTactique::Retire { auteur: 3, id: 1 }, 1, &couleur);
        assert_eq!(tac.traits.len(), 1);
        tac.recevoir(trait_recu(2, 5, 0, vec![[9, 9]], true, None), 1, &couleur);
        tac.recevoir(EvenementTactique::RetireTout { auteur: 2 }, 1, &couleur);
        assert!(tac.traits.is_empty());
    }

    /// Un plan choisi par un autre efface tout ; le mien, revenu du
    /// serveur, n'efface pas ce que j'ai tracé depuis ; mes traits rejoués
    /// à une reconnexion repoussent mon prochain numéro ; quitter le salon
    /// rend une carte à soi, vide.
    #[test]
    fn le_salon_mene_le_plan() {
        let mut tac = Tactique::default();
        carte_du_salon(&mut tac);
        let couleur = |_| Color32::RED;
        tac.recevoir(trait_recu(1, 41, 0, vec![[5, 5]], true, Some(9_000)), 1, &couleur);
        assert_eq!(tac.prochain_id, 42, "mes numéros ne retombent pas sur un trait vivant");
        tac.recevoir(trait_recu(2, 1, 0, vec![[5, 5]], true, None), 1, &couleur);
        tac.recevoir(EvenementTactique::Plan { carte: "Ascent".into(), par: 1 }, 1, &couleur);
        assert_eq!(tac.traits.len(), 2, "mon propre choix, déjà fait ici");
        tac.recevoir(EvenementTactique::Plan { carte: "Lotus".into(), par: 2 }, 1, &couleur);
        assert!(tac.traits.is_empty());
        assert_eq!((tac.carte.as_str(), tac.par), ("Lotus", Some(2)));
        let plans = [Plan { nom: "Ascent".into(), ..Plan::default() }];
        assert!(tac.choisi(&plans).is_none(), "le plan du salon ne se remplace pas par le premier venu");

        tac.relier(Lien::HorsVocal);
        assert!(!tac.du_salon);
        assert_eq!(tac.choisi(&plans).map(|p| p.nom.as_str()), Some("Ascent"), "seul, on reprend un plan connu");
    }

    /// Une image de la carte, sans écran : `evenements` pour seule entrée.
    fn image(ctx: &egui::Context, tac: &mut Tactique, plans: &[Plan], evenements: Vec<egui::Event>) {
        let entree = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 800.0))),
            events: evenements,
            ..Default::default()
        };
        ctx.run_ui(entree, |ui| tac.contenu(ui, plans, None, 1, Color32::GREEN)).drop_without_applying_deltas();
    }

    fn bouton(pos: Pos2, enfonce: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button: PointerButton::Primary, pressed: enfonce, modifiers: Default::default() }
    }

    /// Un trait tiré à la souris sur la carte du salon part par morceaux :
    /// le premier tout de suite, le dernier au crayon levé, et bout à bout
    /// ils refont le trait. Muet, on ne dessine rien. Sans carte du salon,
    /// la première image la demande, une fois.
    #[test]
    fn un_trait_tire_part_au_salon() {
        let plans = [Plan { nom: "Ascent".into(), ..Plan::default() }];
        let ctx = egui::Context::default();
        let mut tac = Tactique::default();
        tac.relier(Lien::Salon);
        image(&ctx, &mut tac, &plans, vec![]);
        image(&ctx, &mut tac, &plans, vec![]);
        assert_eq!(tac.a_envoyer(), vec![ActionTactique::Ouvrir { carte: "Ascent".into() }]);
        tac.recevoir_etat(Some(CarteTactique { salon: 4, carte: "Ascent".into(), par: 1 }));

        let tirer = |tac: &mut Tactique| {
            let depart = pos2(300.0, 500.0);
            image(&ctx, tac, &plans, vec![egui::Event::PointerMoved(depart)]);
            image(&ctx, tac, &plans, vec![bouton(depart, true)]);
            for k in 1..=12 {
                image(&ctx, tac, &plans, vec![egui::Event::PointerMoved(depart + vec2(15.0 * k as f32, -10.0 * k as f32))]);
            }
            image(&ctx, tac, &plans, vec![bouton(depart + vec2(180.0, -120.0), false)]);
            image(&ctx, tac, &plans, vec![]);
        };
        tirer(&mut tac);
        let envois = tac.a_envoyer();
        assert!(envois.len() >= 2, "{envois:?}");
        let mut bout_a_bout = Vec::new();
        for (k, action) in envois.iter().enumerate() {
            let ActionTactique::Trait { id, outil, points, fin } = action else { panic!("{action:?}") };
            assert_eq!((*id, *outil), (1, OutilTactique::Fleche));
            assert_eq!(*fin, k == envois.len() - 1, "seul le dernier lève le crayon");
            bout_a_bout.extend_from_slice(points);
        }
        let local: Vec<PointTactique> = tac.traits[0].points.iter().map(|p| vers_fil(*p)).collect();
        assert!(local.len() > 5, "{local:?}");
        assert_eq!(bout_a_bout, local);
        assert!(tac.traits[0].fini_le.is_some());

        tac.muet = true;
        tirer(&mut tac);
        assert!(tac.a_envoyer().is_empty());
        assert_eq!(tac.traits.len(), 1, "muet, on regarde");
    }
}
