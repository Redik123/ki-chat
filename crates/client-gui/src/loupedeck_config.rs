//! Ce que fait et montre le Loupedeck Live, au choix dans la page
//! Loupedeck, et son enregistrement dans les réglages (en JSON).
//!
//! - Les huit boutons ronds reçoivent une [`Action`] et, s'il le veut, une
//!   couleur de lumière.
//! - Les six molettes reçoivent une [`ActionMolette`] — qui dit aussi ce
//!   qu'affiche la case de la bande en face d'elle, et ce que fait un appui.
//! - Les douze touches montrent une [`Page`] : douze [`Case`]s, chacune un
//!   bouton (une action, avec son icône, son texte et sa couleur), une place
//!   du salon vocal, un chiffre VALORANT ou un clip récent. Les trois pages
//!   d'origine — Vocal, VALORANT, Clips — sont faites de ces cases, et on en
//!   ajoute autant qu'on veut.

use serde::{Deserialize, Serialize};

use crate::icons::Icon;
use crate::loupedeck::Role;

/// Ce que fait un bouton, rond ou touche.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Action {
    Rien,
    Micro,
    Sourd,
    /// Tenu tant qu'on appuie : un bouton rond seulement.
    Ptt,
    Clip,
    Partage,
    Changeur,
    Enregistreur,
    QuitterVocal,
    /// Montrer la page n (dans l'ordre des pages).
    Page(usize),
    /// Entrer dans le salon vocal de ce nom.
    Salon(String),
    /// Jouer ce son de la soundboard, par son nom.
    Son(String),
    /// La page des stats VALORANT du groupe.
    Stats,
    /// Ma fiche VALORANT.
    Fiche,
    /// La galerie des clips.
    Galerie,
    /// La durée des clips, à la suivante (15, 30, 60, 120 s).
    DureeClip,
    /// Ce que fait un toucher sur un clip : le partager, ou le lire.
    ToucherClip,
}

/// Les genres d'action, dans l'ordre des listes de choix. Ceux qui ont un
/// paramètre (la page, le salon, le son) le reçoivent dans un second choix.
pub(crate) const GENRES: [Action; 17] = [
    Action::Rien,
    Action::Micro,
    Action::Sourd,
    Action::Ptt,
    Action::Clip,
    Action::Partage,
    Action::Changeur,
    Action::Enregistreur,
    Action::QuitterVocal,
    Action::Page(0),
    Action::Salon(String::new()),
    Action::Son(String::new()),
    Action::Stats,
    Action::Fiche,
    Action::Galerie,
    Action::DureeClip,
    Action::ToucherClip,
];

impl Action {
    /// Même genre, quel que soit le paramètre.
    pub fn meme_genre(&self, autre: &Action) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(autre)
    }

    /// Le nom du genre, dans la liste du choix.
    pub fn nom(&self) -> &'static str {
        match self {
            Action::Rien => "Rien",
            Action::Micro => "Couper le micro",
            Action::Sourd => "Me rendre sourd",
            Action::Ptt => "Push-to-talk",
            Action::Clip => "Clip !",
            Action::Partage => "Diffuser mon écran",
            Action::Changeur => "Changeur de voix",
            Action::Enregistreur => "Enregistreur de clips",
            Action::QuitterVocal => "Quitter le vocal",
            Action::Page(_) => "Montrer une page",
            Action::Salon(_) => "Entrer dans un salon vocal",
            Action::Son(_) => "Jouer un son de la soundboard",
            Action::Stats => "Stats VALORANT du groupe",
            Action::Fiche => "Ma fiche VALORANT",
            Action::Galerie => "Galerie des clips",
            Action::DureeClip => "Durée des clips",
            Action::ToucherClip => "Toucher un clip : partager ou lire",
        }
    }

    /// Le nom en un mot, sous un bouton rond dessiné ou sur une touche.
    /// `pages` : pour nommer la page d'une action « Page ».
    pub fn court(&self, pages: &[Page]) -> String {
        match self {
            Action::Rien => "—".into(),
            Action::Micro => "Micro".into(),
            Action::Sourd => "Sourd".into(),
            Action::Ptt => "PTT".into(),
            Action::Clip => "Clip !".into(),
            Action::Partage => "Écran".into(),
            Action::Changeur => "Voix".into(),
            Action::Enregistreur => "REC".into(),
            Action::QuitterVocal => "Quitter".into(),
            Action::Page(n) => pages.get(*n).map_or("Page ?".into(), |p| p.nom.clone()),
            Action::Salon(nom) | Action::Son(nom) if !nom.is_empty() => nom.clone(),
            Action::Salon(_) => "Salon".into(),
            Action::Son(_) => "Son".into(),
            Action::Stats => "Stats".into(),
            Action::Fiche => "Ma fiche".into(),
            Action::Galerie => "Clips".into(),
            Action::DureeClip => "Durée".into(),
            Action::ToucherClip => "Toucher".into(),
        }
    }

    /// Ce que fait l'action, et ce que dit sa lumière.
    pub fn aide(&self) -> &'static str {
        match self {
            Action::Rien => "Ne fait rien, et reste éteint.",
            Action::Micro => "Coupe ou rétablit ton micro, en vocal. Rouge : coupé ; vif : tu parles.",
            Action::Sourd => "Coupe tout ce que tu entends (et ton micro avec). Rouge : sourd.",
            Action::Ptt => {
                "Tu parles tant que tu appuies, en mode push-to-talk, avec le même maintien que la \
                 touche. Vif : tu émets."
            }
            Action::Clip => "Enregistre les dernières secondes, comme le raccourci des clips.",
            Action::Partage => {
                "Diffuse ton écran au salon avec les réglages de la dernière diffusion, ou l'arrête. \
                 Rouge : tu diffuses."
            }
            Action::Changeur => "Allume ou coupe le changeur de voix. Vif : allumé.",
            Action::Enregistreur => "Lance ou arrête l'enregistreur de clips. Vif : il tourne.",
            Action::QuitterVocal => "Quitte le salon vocal.",
            Action::Page(_) => "Les touches montrent cette page. Vif : c'est elle qu'on regarde.",
            Action::Salon(_) => "Entre dans ce salon vocal. Vif : tu y es.",
            Action::Son(_) => "Joue ce son de ta soundboard, entendu par tout le salon vocal.",
            Action::Stats => "Ouvre la page des stats VALORANT du groupe.",
            Action::Fiche => "Ouvre ta fiche VALORANT.",
            Action::Galerie => "Ouvre la galerie de tes clips.",
            Action::DureeClip => "Passe à la durée de clip suivante : 15, 30, 60 ou 120 secondes.",
            Action::ToucherClip => "Choisit ce que fait un toucher sur un clip récent : le partager ou le lire.",
        }
    }

    /// L'icône d'origine d'une touche.
    pub fn icone(&self) -> Icon {
        match self {
            Action::Rien => Icon::Close,
            Action::Micro => Icon::Mic,
            Action::Sourd => Icon::Headphones,
            Action::Ptt => Icon::Mic,
            Action::Clip => Icon::Film,
            Action::Partage => Icon::Screen,
            Action::Changeur => Icon::Sliders,
            Action::Enregistreur => Icon::Play,
            Action::QuitterVocal => Icon::Logout,
            Action::Page(_) => Icon::ChevronRight,
            Action::Salon(_) => Icon::Volume,
            Action::Son(_) => Icon::Play,
            Action::Stats => Icon::Star,
            Action::Fiche => Icon::User,
            Action::Galerie => Icon::Film,
            Action::DureeClip => Icon::Sliders,
            Action::ToucherClip => Icon::Send,
        }
    }

    /// La couleur d'origine : celle de l'action allumée.
    pub fn couleur(&self) -> [u8; 3] {
        const VIOLET: [u8; 3] = [150, 90, 255];
        const ROUGE_VALO: [u8; 3] = [255, 70, 85];
        match self {
            Action::Rien => [90, 90, 90],
            Action::Micro => [0, 230, 110],
            Action::Sourd => [190, 200, 220],
            Action::Ptt => [255, 190, 0],
            Action::Clip | Action::Galerie | Action::DureeClip | Action::ToucherClip => VIOLET,
            Action::Partage => [40, 140, 255],
            Action::Changeur => [230, 40, 255],
            Action::Enregistreur => [230, 30, 30],
            Action::QuitterVocal => [230, 60, 60],
            Action::Page(0) => [0, 200, 90],
            Action::Page(1) => ROUGE_VALO,
            Action::Page(2) => VIOLET,
            Action::Page(_) => [0, 180, 220],
            Action::Salon(_) => [0, 200, 90],
            Action::Son(_) => [255, 140, 0],
            Action::Stats | Action::Fiche => ROUGE_VALO,
        }
    }

    /// Ce que le fil de l'appareil fait lui-même d'un bouton rond.
    fn role(&self) -> Role {
        match self {
            Action::Rien => Role::Rien,
            Action::Ptt => Role::Ptt,
            Action::Clip => Role::Clip,
            _ => Role::Commande,
        }
    }
}

/// Ce que fait une molette : tourner règle, appuyer remet à 100 %.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActionMolette {
    Rien,
    Volume,
    Micro,
    Choix,
    VolumeChoisi,
    Musique,
    Sons,
}

impl ActionMolette {
    pub const TOUTES: [ActionMolette; 7] = [
        ActionMolette::Rien,
        ActionMolette::Volume,
        ActionMolette::Micro,
        ActionMolette::Choix,
        ActionMolette::VolumeChoisi,
        ActionMolette::Musique,
        ActionMolette::Sons,
    ];

    pub fn nom(self) -> &'static str {
        match self {
            ActionMolette::Rien => "Rien",
            ActionMolette::Volume => "Volume général",
            ActionMolette::Micro => "Gain du micro",
            ActionMolette::Choix => "Choisir quelqu'un du salon",
            ActionMolette::VolumeChoisi => "Volume de la personne choisie",
            ActionMolette::Musique => "Volume de la musique",
            ActionMolette::Sons => "Volume des sons de ki-chat",
        }
    }

    pub fn court(self) -> &'static str {
        match self {
            ActionMolette::Rien => "—",
            ActionMolette::Volume => "Volume",
            ActionMolette::Micro => "Micro",
            ActionMolette::Choix => "Choix",
            ActionMolette::VolumeChoisi => "Son volume",
            ActionMolette::Musique => "Musique",
            ActionMolette::Sons => "Sons",
        }
    }

    pub fn aide(self) -> &'static str {
        match self {
            ActionMolette::Rien => "La molette ne fait rien ; sa case reste vide.",
            ActionMolette::Volume => {
                "Le volume de tout ce que tu entends, de 5 % par cran. Appui : 100 %. Le glissé sur \
                 la bande de gauche le règle aussi."
            }
            ActionMolette::Micro => "Le gain de ton micro, de 5 % par cran. Appui : 100 %.",
            ActionMolette::Choix => {
                "Passe d'une personne du salon à l'autre, pour la molette « Volume de la personne \
                 choisie ». Appui : revient à la première. Toucher quelqu'un sur la grille le \
                 choisit aussi."
            }
            ActionMolette::VolumeChoisi => {
                "Le volume de la personne choisie, rien que pour toi, de 5 % par cran. Appui : \
                 100 %."
            }
            ActionMolette::Musique => "Le volume du bot de musique, rien que pour toi. Appui : 100 %.",
            ActionMolette::Sons => {
                "Le volume des sons de ki-chat : messages, entrées et sorties du vocal. Appui : \
                 100 %."
            }
        }
    }
}

/// Un chiffre du tableau de bord VALORANT.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Widget {
    /// Mon rang et mes RR.
    Rang,
    /// La partie en cours : score et carte, en direct.
    Partie,
    /// Ma journée : RR, victoires et défaites.
    Jour,
    /// Les dix derniers classés.
    Forme,
    Kd,
    Tete,
    Victoires,
    Adr,
    /// Le n-ième de mes derniers matchs (0 : le dernier).
    Match(u8),
}

impl Widget {
    pub const TOUS: [Widget; 12] = [
        Widget::Rang,
        Widget::Partie,
        Widget::Jour,
        Widget::Forme,
        Widget::Kd,
        Widget::Tete,
        Widget::Victoires,
        Widget::Adr,
        Widget::Match(0),
        Widget::Match(1),
        Widget::Match(2),
        Widget::Match(3),
    ];

    pub fn nom(self) -> String {
        match self {
            Widget::Rang => "Mon rang".into(),
            Widget::Partie => "La partie en direct".into(),
            Widget::Jour => "Ma journée".into(),
            Widget::Forme => "Ma forme".into(),
            Widget::Kd => "K/D sur 7 jours".into(),
            Widget::Tete => "Tirs à la tête sur 7 jours".into(),
            Widget::Victoires => "Victoires sur 7 jours".into(),
            Widget::Adr => "ADR sur 7 jours".into(),
            Widget::Match(0) => "Mon dernier match".into(),
            Widget::Match(n) => format!("Mon match d'avant (n° {})", n + 1),
        }
    }
}

/// Une touche.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Case {
    #[default]
    Vide,
    /// Une place du salon : en vocal, la n-ième personne (moi d'abord) ;
    /// hors vocal, le n-ième salon vocal où entrer.
    Vocal(u8),
    /// Un chiffre VALORANT.
    Valo(Widget),
    /// Le n-ième clip le plus récent (0 : le dernier).
    Clip(u8),
    Bouton(Bouton),
}

/// Une touche-bouton : une action, et l'apparence qu'on lui a choisie —
/// sinon celle de l'action.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Bouton {
    pub action: Action,
    #[serde(default)]
    pub icone: Option<Icon>,
    #[serde(default)]
    pub texte: String,
    #[serde(default)]
    pub couleur: Option<[u8; 3]>,
}

impl Bouton {
    pub fn nu(action: Action) -> Self {
        Self { action, icone: None, texte: String::new(), couleur: None }
    }
}

/// Une page de la grille.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Page {
    pub nom: String,
    pub cases: [Case; 12],
}

impl Page {
    pub fn vide(nom: &str) -> Self {
        Self { nom: nom.into(), cases: Default::default() }
    }

    /// La page Vocal d'origine : les douze places du salon.
    fn vocal() -> Self {
        Self { nom: "Vocal".into(), cases: std::array::from_fn(|i| Case::Vocal(i as u8)) }
    }

    /// Le tableau de bord VALORANT d'origine.
    fn valorant() -> Self {
        Self { nom: "VALORANT".into(), cases: Widget::TOUS.map(Case::Valo) }
    }

    /// La page Clips d'origine : quatre boutons, huit clips.
    fn clips() -> Self {
        let mut cases: [Case; 12] = std::array::from_fn(|i| Case::Clip(i.saturating_sub(4) as u8));
        cases[0] = Case::Bouton(Bouton::nu(Action::Clip));
        cases[1] = Case::Bouton(Bouton::nu(Action::Enregistreur));
        cases[2] = Case::Bouton(Bouton::nu(Action::DureeClip));
        cases[3] = Case::Bouton(Bouton::nu(Action::ToucherClip));
        Self { nom: "Clips".into(), cases }
    }

    pub fn a_du_valorant(&self) -> bool {
        self.cases.iter().any(|c| matches!(c, Case::Valo(_)))
    }

    pub fn a_des_clips(&self) -> bool {
        self.cases.iter().any(|c| matches!(c, Case::Clip(_)))
    }
}

/// Toute la configuration de l'appareil.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Config {
    /// Du rond le plus à gauche (0) au plus à droite (7).
    pub ronds: [Action; 8],
    /// La couleur de la lumière de chaque rond ; `None` : celle de l'action.
    pub couleurs_ronds: [Option<[u8; 3]>; 8],
    /// 0 à 2 à gauche de haut en bas, 3 à 5 à droite.
    pub molettes: [ActionMolette; 6],
    pub pages: Vec<Page>,
    /// La première page VALORANT s'affiche toute seule quand une partie
    /// commence.
    pub valo_auto: bool,
    /// La luminosité des écrans, de 0 à 10.
    pub luminosite: u8,
    /// Un toucher sur un clip le partage (sinon il le lit).
    pub toucher_partage: bool,
    /// Le fond des touches et des bandes, et la couleur des jauges.
    pub fond: [u8; 3],
    pub accent: [u8; 3],
}

/// Le fond d'origine des écrans.
pub(crate) const FOND: [u8; 3] = [0x0b, 0x0e, 0x12];
/// L'accent d'origine : le vert de ki-chat.
pub(crate) const ACCENT: [u8; 3] = [0x00, 0xd2, 0x6a];

impl Default for Config {
    fn default() -> Self {
        Self {
            ronds: [
                Action::Micro,
                Action::Sourd,
                Action::Ptt,
                Action::Clip,
                Action::Partage,
                Action::Page(0),
                Action::Page(1),
                Action::Page(2),
            ],
            couleurs_ronds: [None; 8],
            molettes: [
                ActionMolette::Volume,
                ActionMolette::Micro,
                ActionMolette::Rien,
                ActionMolette::Choix,
                ActionMolette::VolumeChoisi,
                ActionMolette::Musique,
            ],
            pages: vec![Page::vocal(), Page::valorant(), Page::clips()],
            valo_auto: true,
            luminosite: 10,
            toucher_partage: true,
            fond: FOND,
            accent: ACCENT,
        }
    }
}

impl Config {
    /// La configuration enregistrée : en JSON, ou dans le format d'une
    /// ligne d'avant les pages (`r=micro,…;m=volume,…;v=1;l=10;t=1`). Ce qui
    /// ne se lit pas garde sa valeur d'origine.
    pub fn lire(texte: &str) -> Self {
        let mut c = serde_json::from_str::<Config>(texte).unwrap_or_else(|_| Self::lire_ligne(texte));
        if c.pages.is_empty() {
            c.pages = Config::default().pages;
        }
        c.luminosite = c.luminosite.min(10);
        c
    }

    fn lire_ligne(texte: &str) -> Self {
        let mut c = Config::default();
        for champ in texte.split(';') {
            let Some((cle, valeur)) = champ.split_once('=') else { continue };
            match cle.trim() {
                "r" => {
                    for (i, id) in valeur.split(',').take(8).enumerate() {
                        let action = match id.trim() {
                            "rien" => Action::Rien,
                            "micro" => Action::Micro,
                            "sourd" => Action::Sourd,
                            "ptt" => Action::Ptt,
                            "clip" => Action::Clip,
                            "partage" => Action::Partage,
                            "page_vocal" => Action::Page(0),
                            "page_valorant" => Action::Page(1),
                            "page_clips" => Action::Page(2),
                            "changeur" => Action::Changeur,
                            "enregistreur" => Action::Enregistreur,
                            "quitter" => Action::QuitterVocal,
                            _ => continue,
                        };
                        c.ronds[i] = action;
                    }
                }
                "m" => {
                    for (i, id) in valeur.split(',').take(6).enumerate() {
                        let action = match id.trim() {
                            "rien" => ActionMolette::Rien,
                            "volume" => ActionMolette::Volume,
                            "micro" => ActionMolette::Micro,
                            "choix" => ActionMolette::Choix,
                            "volume_choisi" => ActionMolette::VolumeChoisi,
                            "musique" => ActionMolette::Musique,
                            "sons" => ActionMolette::Sons,
                            _ => continue,
                        };
                        c.molettes[i] = action;
                    }
                }
                "v" => c.valo_auto = valeur.trim() != "0",
                "l" => {
                    if let Ok(l) = valeur.trim().parse::<u8>() {
                        c.luminosite = l.min(10);
                    }
                }
                "t" => c.toucher_partage = valeur.trim() != "0",
                _ => {}
            }
        }
        c
    }

    pub fn ecrire(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Ce que le fil de l'appareil fait lui-même de chaque rond.
    pub fn roles(&self) -> [Role; 8] {
        std::array::from_fn(|i| self.ronds[i].role())
    }

    /// Une page de plus, vide, et son numéro.
    pub fn ajouter_page(&mut self) -> usize {
        let n = self.pages.len();
        self.pages.push(Page::vide(&format!("Page {}", n + 1)));
        n
    }

    /// Retire la page n — jamais la dernière qui reste — et recolle les
    /// actions « Page » qui visaient les suivantes ; celles qui la visaient
    /// ne font plus rien.
    pub fn retirer_page(&mut self, n: usize) {
        if self.pages.len() <= 1 || n >= self.pages.len() {
            return;
        }
        self.pages.remove(n);
        let recoller = |a: &mut Action| {
            if let Action::Page(p) = a {
                if *p == n {
                    *a = Action::Rien;
                } else if *p > n {
                    *p -= 1;
                }
            }
        };
        for a in &mut self.ronds {
            recoller(a);
        }
        for page in &mut self.pages {
            for case in &mut page.cases {
                if let Case::Bouton(b) = case {
                    recoller(&mut b.action);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aller_retour() {
        let mut c = Config::default();
        c.ronds[2] = Action::Son("airhorn".into());
        c.couleurs_ronds[2] = Some([1, 2, 3]);
        c.molettes[2] = ActionMolette::Sons;
        c.pages[0].cases[3] = Case::Bouton(Bouton {
            action: Action::Salon("Gaming".into()),
            icone: Some(Icon::Star),
            texte: "Go".into(),
            couleur: Some([9, 8, 7]),
        });
        c.valo_auto = false;
        c.luminosite = 6;
        assert_eq!(Config::lire(&c.ecrire()), c);
    }

    #[test]
    fn ancien_format() {
        let c = Config::lire("r=micro,sourd,changeur,clip,partage,page_vocal,page_valorant,page_clips;m=volume,micro,sons;v=0;l=7;t=0");
        assert_eq!(c.ronds[2], Action::Changeur);
        assert_eq!(c.ronds[6], Action::Page(1));
        assert_eq!(c.molettes[2], ActionMolette::Sons);
        assert!(!c.valo_auto && !c.toucher_partage);
        assert_eq!(c.luminosite, 7);
        assert_eq!(c.pages.len(), 3);
        assert_eq!(Config::lire(""), Config::default());
    }

    #[test]
    fn retirer_une_page() {
        let mut c = Config::default();
        c.retirer_page(1);
        assert_eq!(c.pages.len(), 2);
        assert_eq!(c.ronds[5], Action::Page(0));
        assert_eq!(c.ronds[6], Action::Rien);
        assert_eq!(c.ronds[7], Action::Page(1));
        // La dernière page ne part jamais.
        c.retirer_page(0);
        c.retirer_page(0);
        assert_eq!(c.pages.len(), 1);
    }

    #[test]
    fn roles() {
        let r = Config::default().roles();
        assert_eq!(r[0], Role::Commande);
        assert_eq!(r[2], Role::Ptt);
        assert_eq!(r[3], Role::Clip);
    }
}
