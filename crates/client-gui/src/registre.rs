//! Le registre des raccourcis de la fenêtre : chaque action, la touche
//! qu'elle a d'origine, celle qu'on lui a donnée à la place.
//!
//! Ce sont les raccourcis qui ne marchent que dans ki-chat, fenêtre au
//! premier plan. Ceux qui marchent partout, même en jeu — parler, couper le
//! micro, prendre un clip — vivent dans `ptt` et se règlent avec la voix.
//!
//! Tout se lit ici, et rien qu'ici : `clavier` demande « cette action
//! vient-elle d'être faite ? » sans connaître la touche. Toutes n'ont pas de
//! touche d'origine : celles-là se lancent par la palette (Ctrl+Maj+P), et
//! reçoivent une combinaison si on leur en donne une.
//!
//! Une combinaison se compare à la lettre — Alt+Maj+↓ n'est pas Alt+↓ —,
//! sauf Maj sur un signe, qui dépend du clavier : « / » se tape avec Maj sur
//! un clavier français ou suisse, sans sur un clavier américain.

use std::collections::HashMap;

use eframe::egui::{self, Key, Modifiers};

/// Ce que le clavier peut déclencher dans la fenêtre.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Action {
    AllerA,
    Palette,
    Chercher,
    SalonSuivant,
    SalonPrecedent,
    NonLuSuivant,
    NonLuPrecedent,
    Gras,
    Italique,
    Code,
    Barre,
    Reglages,
    Raccourcis,
    Compte,
    Valorant,
    Tableau,
    Claude,
    Micro,
    Sourd,
    QuitterVocal,
}

/// Les familles, dans l'ordre où la fenêtre les montre.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Groupe {
    Aller,
    Ecrire,
    Fenetres,
    Voix,
}

impl Groupe {
    pub(crate) const TOUS: [Groupe; 4] = [Groupe::Aller, Groupe::Ecrire, Groupe::Fenetres, Groupe::Voix];

    pub(crate) fn titre(self) -> &'static str {
        match self {
            Groupe::Aller => "Aller quelque part",
            Groupe::Ecrire => "Écrire",
            Groupe::Fenetres => "Ouvrir",
            Groupe::Voix => "La voix, ki-chat devant",
        }
    }
}

impl Action {
    pub(crate) const TOUTES: [Action; 20] = [
        Action::AllerA,
        Action::Palette,
        Action::Chercher,
        Action::SalonSuivant,
        Action::SalonPrecedent,
        Action::NonLuSuivant,
        Action::NonLuPrecedent,
        Action::Gras,
        Action::Italique,
        Action::Code,
        Action::Barre,
        Action::Reglages,
        Action::Raccourcis,
        Action::Compte,
        Action::Valorant,
        Action::Tableau,
        Action::Claude,
        Action::Micro,
        Action::Sourd,
        Action::QuitterVocal,
    ];

    /// Le nom sous lequel le choix se range : ne change plus.
    pub(crate) fn cle(self) -> &'static str {
        match self {
            Action::AllerA => "aller-a",
            Action::Palette => "palette",
            Action::Chercher => "chercher",
            Action::SalonSuivant => "salon-suivant",
            Action::SalonPrecedent => "salon-precedent",
            Action::NonLuSuivant => "non-lu-suivant",
            Action::NonLuPrecedent => "non-lu-precedent",
            Action::Gras => "gras",
            Action::Italique => "italique",
            Action::Code => "code",
            Action::Barre => "barre",
            Action::Reglages => "reglages",
            Action::Raccourcis => "raccourcis",
            Action::Compte => "compte",
            Action::Valorant => "valorant",
            Action::Tableau => "tableau",
            Action::Claude => "claude",
            Action::Micro => "micro",
            Action::Sourd => "sourd",
            Action::QuitterVocal => "quitter-vocal",
        }
    }

    /// Ce que ça fait, en quelques mots : la ligne de la fenêtre des
    /// raccourcis, et celle de la palette.
    pub(crate) fn libelle(self) -> &'static str {
        match self {
            Action::AllerA => "Aller à un salon ou à un membre",
            Action::Palette => "Toutes les commandes",
            Action::Chercher => "Chercher dans les messages",
            Action::SalonSuivant => "Salon suivant",
            Action::SalonPrecedent => "Salon précédent",
            Action::NonLuSuivant => "Prochain salon non lu",
            Action::NonLuPrecedent => "Salon non lu d'avant",
            Action::Gras => "Gras",
            Action::Italique => "Italique",
            Action::Code => "Code",
            Action::Barre => "Barré",
            Action::Reglages => "Réglages",
            Action::Raccourcis => "Raccourcis clavier",
            Action::Compte => "Mon compte",
            Action::Valorant => "Page VALORANT",
            Action::Tableau => "Tableau tactique",
            Action::Claude => "Salon de Claude",
            Action::Micro => "Couper ou rouvrir le micro",
            Action::Sourd => "Ne plus entendre, ou entendre",
            Action::QuitterVocal => "Quitter le salon vocal",
        }
    }

    pub(crate) fn groupe(self) -> Groupe {
        match self {
            Action::AllerA
            | Action::Palette
            | Action::Chercher
            | Action::SalonSuivant
            | Action::SalonPrecedent
            | Action::NonLuSuivant
            | Action::NonLuPrecedent => Groupe::Aller,
            Action::Gras | Action::Italique | Action::Code | Action::Barre => Groupe::Ecrire,
            Action::Reglages | Action::Raccourcis | Action::Compte | Action::Valorant | Action::Tableau | Action::Claude => {
                Groupe::Fenetres
            }
            Action::Micro | Action::Sourd | Action::QuitterVocal => Groupe::Voix,
        }
    }

    /// La combinaison d'origine. `None` : l'action n'en a pas — elle se
    /// lance par la palette, ou par celle qu'on lui donne.
    pub(crate) fn origine(self) -> Option<Combinaison> {
        Some(match self {
            Action::AllerA => Combinaison::ctrl(Key::K),
            Action::Palette => Combinaison::ctrl(Key::P).et_maj(),
            Action::Chercher => Combinaison::ctrl(Key::F),
            Action::SalonSuivant => Combinaison::alt(Key::ArrowDown),
            Action::SalonPrecedent => Combinaison::alt(Key::ArrowUp),
            Action::NonLuSuivant => Combinaison::alt(Key::ArrowDown).et_maj(),
            Action::NonLuPrecedent => Combinaison::alt(Key::ArrowUp).et_maj(),
            Action::Gras => Combinaison::ctrl(Key::B),
            Action::Italique => Combinaison::ctrl(Key::I),
            Action::Code => Combinaison::ctrl(Key::E),
            Action::Barre => Combinaison::ctrl(Key::X).et_maj(),
            Action::Reglages => Combinaison::ctrl(Key::Comma),
            Action::Raccourcis => Combinaison::ctrl(Key::Slash),
            Action::Compte
            | Action::Valorant
            | Action::Tableau
            | Action::Claude
            | Action::Micro
            | Action::Sourd
            | Action::QuitterVocal => return None,
        })
    }

    fn depuis_cle(cle: &str) -> Option<Action> {
        Action::TOUTES.into_iter().find(|a| a.cle() == cle)
    }
}

/// Des modificateurs et une touche. « Ctrl » est la touche de commande de
/// la machine : Ctrl sous Windows et Linux, ⌘ sur un Mac.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Combinaison {
    pub(crate) ctrl: bool,
    pub(crate) alt: bool,
    pub(crate) maj: bool,
    pub(crate) touche: Key,
}

/// Un signe : la touche qui le porte, et s'il faut Maj pour l'écrire,
/// changent d'un clavier à l'autre.
fn est_un_signe(touche: Key) -> bool {
    matches!(
        touche,
        Key::Colon
            | Key::Comma
            | Key::Backslash
            | Key::Slash
            | Key::Pipe
            | Key::Questionmark
            | Key::Exclamationmark
            | Key::OpenBracket
            | Key::CloseBracket
            | Key::OpenCurlyBracket
            | Key::CloseCurlyBracket
            | Key::Backtick
            | Key::Minus
            | Key::Period
            | Key::Plus
            | Key::Equals
            | Key::Semicolon
            | Key::Quote
            | Key::IntlBackslash
    )
}

fn est_une_lettre(touche: Key) -> bool {
    let nom = touche.name();
    nom.len() == 1 && nom.as_bytes()[0].is_ascii_uppercase()
}

fn est_un_chiffre(touche: Key) -> bool {
    matches!(
        touche,
        Key::Num0 | Key::Num1 | Key::Num2 | Key::Num3 | Key::Num4 | Key::Num5 | Key::Num6 | Key::Num7 | Key::Num8 | Key::Num9
    )
}

fn est_une_fonction(touche: Key) -> bool {
    touche.name().strip_prefix('F').is_some_and(|n| n.parse::<u8>().is_ok())
}

/// Pourquoi une combinaison ne peut pas servir.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Refus {
    /// Une touche seule, qui écrirait au lieu d'agir.
    SansModificateur,
    /// Entrée, Échap, Tab… : elles ont déjà leur rôle partout.
    ToucheReservee,
    /// Copier, coller, annuler, la taille de l'interface, fermer la fenêtre.
    DejaAuSysteme(&'static str),
    /// Ctrl+Alt+lettre, c'est AltGr : la touche du € et du @.
    AltGr,
    /// Une autre action l'a déjà.
    Prise(Action),
}

impl Refus {
    pub(crate) fn phrase(self) -> String {
        match self {
            Refus::SansModificateur => "il faut Ctrl ou Alt avec la touche — ou une touche F1 à F12".into(),
            Refus::ToucheReservee => "cette touche a déjà son rôle partout".into(),
            Refus::DejaAuSysteme(quoi) => format!("déjà prise : {quoi}"),
            Refus::AltGr => "Ctrl+Alt avec une lettre, c'est AltGr : elle écrit déjà quelque chose".into(),
            Refus::Prise(action) => format!("déjà prise par « {} »", action.libelle()),
        }
    }
}

impl Combinaison {
    pub(crate) const fn ctrl(touche: Key) -> Self {
        Self { ctrl: true, alt: false, maj: false, touche }
    }

    pub(crate) const fn alt(touche: Key) -> Self {
        Self { ctrl: false, alt: true, maj: false, touche }
    }

    pub(crate) const fn et_maj(mut self) -> Self {
        self.maj = true;
        self
    }

    /// La combinaison que fait une touche enfoncée avec ces modificateurs.
    /// Maj sur un signe ne compte pas (voir [`est_un_signe`]).
    pub(crate) fn tenue(touche: Key, m: Modifiers) -> Self {
        Self { ctrl: m.command, alt: m.alt, maj: m.shift && !est_un_signe(touche), touche }
    }

    /// Cette touche, avec ces modificateurs, est-elle la combinaison ?
    pub(crate) fn convient(self, touche: Key, m: Modifiers) -> bool {
        Self::tenue(touche, m) == Self { maj: self.maj && !est_un_signe(self.touche), ..self }
    }

    /// Ce qu'on en montre : « Ctrl+Maj+P », « Alt+↓ ».
    pub(crate) fn libelle(self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push(if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" }.to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        if self.maj {
            parts.push("Maj".to_string());
        }
        parts.push(nom_de_touche(self.touche));
        parts.join("+")
    }

    /// Ce qu'on en range : « ctrl+alt+maj+Slash », avec le nom qu'egui donne
    /// à la touche.
    fn ecrire(self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("ctrl");
        }
        if self.alt {
            parts.push("alt");
        }
        if self.maj {
            parts.push("maj");
        }
        parts.push(self.touche.name());
        parts.join("+")
    }

    fn lire(texte: &str) -> Option<Self> {
        let mut c = Self { ctrl: false, alt: false, maj: false, touche: Key::Escape };
        let mut touche = None;
        for part in texte.split('+') {
            match part {
                "ctrl" => c.ctrl = true,
                "alt" => c.alt = true,
                "maj" => c.maj = true,
                autre => touche = Some(Key::from_name(autre)?),
            }
        }
        c.touche = touche?;
        Some(c)
    }

    /// Peut-elle servir de raccourci, sans regarder qui l'a déjà ?
    pub(crate) fn acceptable(self) -> Result<(), Refus> {
        let t = self.touche;
        if matches!(t, Key::Escape | Key::Enter | Key::Tab | Key::Backspace | Key::Delete | Key::Space) {
            return Err(Refus::ToucheReservee);
        }
        if !(self.ctrl || self.alt || est_une_fonction(t)) {
            return Err(Refus::SansModificateur);
        }
        let ecrit = est_une_lettre(t) || est_un_chiffre(t) || est_un_signe(t);
        if self.ctrl && self.alt && ecrit {
            return Err(Refus::AltGr);
        }
        if self.alt && !self.ctrl && t == Key::F4 {
            return Err(Refus::DejaAuSysteme("fermer la fenêtre"));
        }
        if self.ctrl && !self.alt {
            let quoi = match t {
                // Avec Maj, ce ne sont plus elles : Ctrl+Maj+X reste libre.
                Key::C | Key::V | Key::X if !self.maj => Some("copier, couper, coller"),
                Key::A => Some("tout sélectionner"),
                Key::Z | Key::Y => Some("annuler, refaire"),
                Key::Plus | Key::Minus | Key::Equals | Key::Num0 => Some("la taille de l'interface"),
                Key::ArrowLeft | Key::ArrowRight | Key::Home | Key::End => Some("se déplacer dans le texte"),
                _ => None,
            };
            if let Some(quoi) = quoi {
                return Err(Refus::DejaAuSysteme(quoi));
            }
        }
        Ok(())
    }
}

/// Le nom d'une touche, comme il est écrit dessus sur un clavier français.
pub(crate) fn nom_de_touche(touche: Key) -> String {
    match touche {
        Key::ArrowUp => "↑".into(),
        Key::ArrowDown => "↓".into(),
        Key::ArrowLeft => "←".into(),
        Key::ArrowRight => "→".into(),
        Key::PageUp => "Page haut".into(),
        Key::PageDown => "Page bas".into(),
        Key::Home => "Début".into(),
        Key::End => "Fin".into(),
        Key::Insert => "Inser".into(),
        Key::Delete => "Suppr".into(),
        Key::Backspace => "Retour".into(),
        Key::Enter => "Entrée".into(),
        Key::Escape => "Échap".into(),
        Key::Space => "Espace".into(),
        Key::Tab => "Tab".into(),
        t if est_un_chiffre(t) => t.name().trim_start_matches("Num").chars().last().map(String::from).unwrap_or_default(),
        t if est_un_signe(t) && t != Key::IntlBackslash => t.symbol_or_name().into(),
        t => t.name().into(),
    }
}

/// Le registre : pour chaque action, la combinaison choisie quand ce n'est
/// pas celle d'origine — `None` quand on la lui a retirée.
#[derive(Default, Debug, PartialEq)]
pub(crate) struct Registre {
    choisies: HashMap<Action, Option<Combinaison>>,
}

impl Registre {
    /// Relit le registre : « action=combinaison », séparés par des virgules ;
    /// une action sans rien après le signe égal n'a plus de raccourci. Ce
    /// qui ne se lit pas, ne peut pas servir ou est déjà pris garde sa
    /// combinaison d'origine.
    pub(crate) fn load(get: impl Fn(&str, &str) -> String) -> Self {
        let mut registre = Self::default();
        for entree in get("raccourcis", "").split(',') {
            let Some((cle, valeur)) = entree.split_once('=') else { continue };
            let Some(action) = Action::depuis_cle(cle) else { continue };
            if valeur.is_empty() {
                registre.choisies.insert(action, None);
            } else if let Some(c) = Combinaison::lire(valeur) {
                let _ = registre.donner(action, Some(c));
            }
        }
        registre
    }

    pub(crate) fn save(&self, storage: &mut dyn eframe::Storage) {
        // Dans l'ordre des actions : le fichier ne bouge pas sans raison.
        let entrees: Vec<String> = Action::TOUTES
            .into_iter()
            .filter_map(|a| self.choisies.get(&a).map(|c| format!("{}={}", a.cle(), c.map(Combinaison::ecrire).unwrap_or_default())))
            .collect();
        storage.set_string("raccourcis", entrees.join(","));
    }

    /// La combinaison d'une action, à l'instant. `None` : elle n'en a plus.
    pub(crate) fn de(&self, action: Action) -> Option<Combinaison> {
        self.choisies.get(&action).copied().unwrap_or(action.origine())
    }

    /// Ce qu'on en montre dans une bulle ou un menu : « Ctrl+K », ou rien.
    pub(crate) fn libelle(&self, action: Action) -> Option<String> {
        self.de(action).map(Combinaison::libelle)
    }

    /// L'action n'a plus sa combinaison d'origine.
    pub(crate) fn changee(&self, action: Action) -> bool {
        self.choisies.contains_key(&action)
    }

    pub(crate) fn rien_de_change(&self) -> bool {
        self.choisies.is_empty()
    }

    /// Qui d'autre que `sauf` a déjà cette combinaison ?
    fn prise_par(&self, c: Combinaison, sauf: Action) -> Option<Action> {
        Action::TOUTES.into_iter().find(|a| *a != sauf && self.de(*a).is_some_and(|autre| autre.convient(c.touche, c.modificateurs())))
    }

    /// Donne une combinaison à une action, ou la lui retire (`None`).
    pub(crate) fn donner(&mut self, action: Action, c: Option<Combinaison>) -> Result<(), Refus> {
        if let Some(c) = c {
            c.acceptable()?;
            if let Some(autre) = self.prise_par(c, action) {
                return Err(Refus::Prise(autre));
            }
        }
        if c == action.origine() {
            self.choisies.remove(&action);
        } else {
            self.choisies.insert(action, c);
        }
        Ok(())
    }

    /// Rend sa combinaison d'origine à une action — si personne ne l'a
    /// prise entre-temps. Celle qui n'en avait pas n'en a plus.
    pub(crate) fn remettre(&mut self, action: Action) -> Result<(), Refus> {
        self.donner(action, action.origine())
    }

    /// Tout comme au premier jour.
    pub(crate) fn tout_remettre(&mut self) {
        self.choisies.clear();
    }

    /// L'action vient-elle d'être demandée au clavier ? La touche est prise :
    /// personne d'autre ne la lira pendant cette image.
    pub(crate) fn faite(&self, entree: &mut egui::InputState, action: Action) -> bool {
        let Some(c) = self.de(action) else { return false };
        let avant = entree.events.len();
        entree.events.retain(|e| !matches!(e, egui::Event::Key { key, pressed: true, modifiers, .. } if c.convient(*key, *modifiers)));
        entree.events.len() != avant
    }
}

impl Combinaison {
    fn modificateurs(self) -> Modifiers {
        Modifiers { alt: self.alt, ctrl: self.ctrl, shift: self.maj, mac_cmd: false, command: self.ctrl }
    }
}

/// La première touche enfoncée de cette image, avec ses modificateurs — pour
/// « appuie sur ta combinaison ». Tout le clavier de l'image est pris :
/// pendant qu'on choisit, rien d'autre ne réagit.
pub(crate) fn touche_enfoncee(entree: &mut egui::InputState) -> Option<(Key, Modifiers)> {
    let vue = entree.events.iter().find_map(|e| match e {
        egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } => Some((*key, *modifiers)),
        _ => None,
    });
    entree.events.retain(|e| !matches!(e, egui::Event::Key { .. } | egui::Event::Text(_)));
    vue
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    const CTRL: Modifiers = Modifiers::COMMAND;
    const ALT: Modifiers = Modifiers::ALT;

    fn avec(m: &[Modifiers]) -> Modifiers {
        m.iter().fold(Modifiers::NONE, |a, b| a | *b)
    }

    #[derive(Default)]
    struct EnMemoire(BTreeMap<String, String>);

    impl eframe::Storage for EnMemoire {
        fn get_string(&self, cle: &str) -> Option<String> {
            self.0.get(cle).cloned()
        }
        fn set_string(&mut self, cle: &str, valeur: String) {
            self.0.insert(cle.to_string(), valeur);
        }
        fn remove_string(&mut self, cle: &str) {
            self.0.remove(cle);
        }
        fn flush(&mut self) {}
    }

    fn relu(r: &Registre) -> (Registre, String) {
        let mut rangement = EnMemoire::default();
        r.save(&mut rangement);
        let texte = rangement.0["raccourcis"].clone();
        (Registre::load(|cle, defaut| rangement.0.get(cle).cloned().unwrap_or_else(|| defaut.to_string())), texte)
    }

    fn lu(texte: &str) -> Registre {
        Registre::load(|_, _| texte.to_string())
    }

    /// Les combinaisons d'origine sont toutes bonnes, toutes différentes, et
    /// chaque action a sa clé à elle.
    #[test]
    fn les_origines_tiennent_debout() {
        let r = Registre::default();
        for (rang, a) in Action::TOUTES.into_iter().enumerate() {
            assert_eq!(Action::depuis_cle(a.cle()), Some(a));
            assert_eq!(Action::TOUTES.iter().position(|b| b.cle() == a.cle()), Some(rang), "{a:?} partage sa clé");
            assert!(Groupe::TOUS.contains(&a.groupe()));
            let Some(c) = a.origine() else { continue };
            assert_eq!(c.acceptable(), Ok(()), "{a:?}");
            assert_eq!(r.prise_par(c, a), None, "{a:?} partage sa combinaison");
            assert_eq!(Combinaison::lire(&c.ecrire()), Some(c), "{a:?} ne se relit pas");
        }
        assert!(r.rien_de_change());
        assert_eq!(relu(&r), (Registre::default(), String::new()));
    }

    /// À la lettre : Alt+Maj+↓ n'est pas Alt+↓, et AltGr+E (Ctrl+Alt pour
    /// Windows) n'est pas Ctrl+E.
    #[test]
    fn une_combinaison_se_compare_a_la_lettre() {
        let origine = |a: Action| a.origine().unwrap();
        let suivant = origine(Action::SalonSuivant);
        assert!(suivant.convient(Key::ArrowDown, ALT));
        assert!(!suivant.convient(Key::ArrowDown, avec(&[ALT, Modifiers::SHIFT])));
        assert!(!suivant.convient(Key::ArrowDown, Modifiers::NONE));
        assert!(!suivant.convient(Key::ArrowUp, ALT));
        assert!(origine(Action::NonLuSuivant).convient(Key::ArrowDown, avec(&[ALT, Modifiers::SHIFT])));
        let code = origine(Action::Code);
        assert!(code.convient(Key::E, CTRL));
        assert!(!code.convient(Key::E, avec(&[CTRL, ALT])), "AltGr+E écrit €");
        assert!(!code.convient(Key::E, avec(&[CTRL, Modifiers::SHIFT])));
    }

    /// Un signe se moque de Maj : « / » est Maj+7 sur un clavier suisse,
    /// Maj+: sur un clavier français.
    #[test]
    fn maj_ne_compte_pas_sur_un_signe() {
        let liste = Action::Raccourcis.origine().unwrap();
        assert!(liste.convient(Key::Slash, CTRL));
        assert!(liste.convient(Key::Slash, avec(&[CTRL, Modifiers::SHIFT])));
        assert!(!liste.convient(Key::Slash, Modifiers::NONE));
        // Prise au vol avec Maj, elle se range sans.
        let prise = Combinaison::tenue(Key::Slash, avec(&[CTRL, Modifiers::SHIFT]));
        assert_eq!(prise, liste);
        assert_eq!(prise.libelle(), "Ctrl+/");
    }

    #[test]
    fn les_libelles_se_lisent() {
        let r = Registre::default();
        assert_eq!(r.libelle(Action::AllerA).as_deref(), Some("Ctrl+K"));
        assert_eq!(r.libelle(Action::Palette).as_deref(), Some("Ctrl+Maj+P"));
        assert_eq!(r.libelle(Action::NonLuPrecedent).as_deref(), Some("Alt+Maj+↑"));
        assert_eq!(r.libelle(Action::Reglages).as_deref(), Some("Ctrl+,"));
        assert_eq!(r.libelle(Action::Micro), None, "pas de touche d'origine");
        assert_eq!(Combinaison::ctrl(Key::Num3).et_maj().libelle(), "Ctrl+Maj+3");
        assert_eq!(Combinaison { ctrl: false, alt: false, maj: false, touche: Key::F2 }.libelle(), "F2");
        assert_eq!(Combinaison::alt(Key::PageDown).libelle(), "Alt+Page bas");
    }

    #[test]
    fn ce_qui_ne_peut_pas_servir_est_refuse() {
        let seule = |touche| Combinaison { ctrl: false, alt: false, maj: false, touche };
        assert_eq!(seule(Key::K).acceptable(), Err(Refus::SansModificateur));
        assert_eq!(seule(Key::K).et_maj().acceptable(), Err(Refus::SansModificateur), "Maj+K écrit un K");
        assert_eq!(seule(Key::F2).acceptable(), Ok(()));
        assert_eq!(Combinaison::ctrl(Key::Enter).acceptable(), Err(Refus::ToucheReservee));
        assert_eq!(seule(Key::Escape).acceptable(), Err(Refus::ToucheReservee));
        assert!(matches!(Combinaison::ctrl(Key::C).acceptable(), Err(Refus::DejaAuSysteme(_))));
        assert_eq!(Combinaison::ctrl(Key::X).et_maj().acceptable(), Ok(()), "avec Maj, ce n'est plus couper");
        assert!(matches!(Combinaison::ctrl(Key::Z).et_maj().acceptable(), Err(Refus::DejaAuSysteme(_))));
        assert!(matches!(Combinaison::ctrl(Key::Num0).acceptable(), Err(Refus::DejaAuSysteme(_))));
        assert!(matches!(Combinaison::alt(Key::F4).acceptable(), Err(Refus::DejaAuSysteme(_))));
        assert_eq!(Combinaison { alt: true, ..Combinaison::ctrl(Key::E) }.acceptable(), Err(Refus::AltGr));
        assert_eq!(Combinaison { alt: true, ..Combinaison::ctrl(Key::F5) }.acceptable(), Ok(()));
        assert_eq!(Combinaison::ctrl(Key::J).et_maj().acceptable(), Ok(()));
    }

    #[test]
    fn on_change_on_retire_on_remet() {
        let mut r = Registre::default();
        // Changer.
        assert_eq!(r.donner(Action::AllerA, Some(Combinaison::ctrl(Key::J))), Ok(()));
        assert_eq!(r.libelle(Action::AllerA).as_deref(), Some("Ctrl+J"));
        assert!(r.changee(Action::AllerA) && !r.changee(Action::Chercher));
        // Une combinaison prise ne se donne pas deux fois.
        assert_eq!(r.donner(Action::Gras, Some(Combinaison::ctrl(Key::J))), Err(Refus::Prise(Action::AllerA)));
        assert_eq!(r.donner(Action::Gras, Some(Combinaison::ctrl(Key::F))), Err(Refus::Prise(Action::Chercher)));
        assert_eq!(r.de(Action::Gras), Action::Gras.origine());
        // Ctrl+K est libre maintenant : une autre peut la prendre…
        assert_eq!(r.donner(Action::Chercher, Some(Combinaison::ctrl(Key::K))), Ok(()));
        // … et « aller à » ne la retrouve plus tant qu'elle est prise.
        assert_eq!(r.remettre(Action::AllerA), Err(Refus::Prise(Action::Chercher)));
        // Retirer : plus de raccourci, et sa combinaison se libère.
        assert_eq!(r.donner(Action::Chercher, None), Ok(()));
        assert_eq!(r.de(Action::Chercher), None);
        assert_eq!(r.remettre(Action::AllerA), Ok(()));
        assert!(!r.changee(Action::AllerA));
        // Redonner celle d'origine, c'est ne plus rien avoir changé.
        assert_eq!(r.donner(Action::Chercher, Action::Chercher.origine()), Ok(()));
        assert!(r.rien_de_change());
    }

    /// Une action sans touche d'origine en reçoit une, et la rend.
    #[test]
    fn une_action_sans_touche_en_recoit_une() {
        let mut r = Registre::default();
        assert_eq!(r.de(Action::Micro), None);
        assert!(!r.changee(Action::Micro));
        let m = Combinaison::ctrl(Key::M).et_maj();
        assert_eq!(r.donner(Action::Micro, Some(m)), Ok(()));
        assert!(r.changee(Action::Micro));
        assert_eq!(r.donner(Action::Sourd, Some(m)), Err(Refus::Prise(Action::Micro)));
        let (copie, texte) = relu(&r);
        assert_eq!((copie.de(Action::Micro), texte.as_str()), (Some(m), "micro=ctrl+maj+M"));
        assert_eq!(r.remettre(Action::Micro), Ok(()));
        assert!(r.rien_de_change() && r.de(Action::Micro).is_none());
        // La lui retirer alors qu'elle n'en a pas ne change rien non plus.
        assert_eq!(r.donner(Action::Micro, None), Ok(()));
        assert!(r.rien_de_change());
    }

    #[test]
    fn le_registre_se_range_et_se_relit() {
        let mut r = Registre::default();
        r.donner(Action::Raccourcis, Some(Combinaison { ctrl: false, alt: false, maj: false, touche: Key::F1 })).unwrap();
        r.donner(Action::SalonSuivant, None).unwrap();
        r.donner(Action::Gras, Some(Combinaison::ctrl(Key::G).et_maj())).unwrap();
        let (copie, texte) = relu(&r);
        assert_eq!(texte, "salon-suivant=,gras=ctrl+maj+G,raccourcis=F1");
        assert_eq!(copie, r);
        r.tout_remettre();
        assert_eq!(relu(&r).1, "");
    }

    /// Un fichier retouché à la main, ou venu d'une version qui avait
    /// d'autres actions : ce qui ne tient pas garde son origine.
    #[test]
    fn un_rangement_abime_retombe_sur_l_origine() {
        let r = lu("aller-a=ctrl+Zorglub,inconnue=ctrl+J,gras=K,code=ctrl+F,chercher,,italique=ctrl+maj+I");
        assert_eq!(r.de(Action::AllerA), Action::AllerA.origine(), "touche inconnue");
        assert_eq!(r.de(Action::Gras), Action::Gras.origine(), "sans modificateur");
        assert_eq!(r.de(Action::Code), Action::Code.origine(), "déjà prise par chercher");
        assert_eq!(r.de(Action::Chercher), Action::Chercher.origine());
        assert_eq!(r.libelle(Action::Italique).as_deref(), Some("Ctrl+Maj+I"));
    }

    /// Lue, la touche est prise ; une action sans raccourci ne se fait pas.
    #[test]
    fn une_touche_lue_est_prise() {
        let frappe = |key, modifiers| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        let mut entree = egui::InputState::default();
        entree.events = vec![frappe(Key::K, CTRL), frappe(Key::ArrowDown, avec(&[ALT, Modifiers::SHIFT])), egui::Event::Text("k".into())];
        let mut r = Registre::default();
        assert!(!r.faite(&mut entree, Action::SalonSuivant), "Alt+Maj+↓ n'est pas Alt+↓");
        assert!(r.faite(&mut entree, Action::AllerA));
        assert!(!r.faite(&mut entree, Action::AllerA), "déjà prise");
        assert_eq!(entree.events.len(), 2);
        r.donner(Action::NonLuSuivant, None).unwrap();
        assert!(!r.faite(&mut entree, Action::NonLuSuivant));
        // « Appuie sur ta combinaison » : la première touche, et tout le
        // clavier de l'image avec elle.
        assert_eq!(touche_enfoncee(&mut entree), Some((Key::ArrowDown, avec(&[ALT, Modifiers::SHIFT]))));
        assert!(entree.events.is_empty());
        assert_eq!(touche_enfoncee(&mut entree), None);
    }
}
