//! Choix de touche push-to-talk, détectée globalement (même fenêtre non
//! focalisée) via device_query.

use device_query::Keycode;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PttKey {
    LAlt,
    LControl,
    LShift,
    CapsLock,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
}

impl PttKey {
    pub const ALL: [PttKey; 18] = [
        PttKey::LAlt,
        PttKey::LControl,
        PttKey::LShift,
        PttKey::CapsLock,
        PttKey::F1,
        PttKey::F2,
        PttKey::F3,
        PttKey::F4,
        PttKey::F5,
        PttKey::F6,
        PttKey::F7,
        PttKey::F8,
        PttKey::Insert,
        PttKey::Delete,
        PttKey::Home,
        PttKey::End,
        PttKey::PageUp,
        PttKey::PageDown,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PttKey::LAlt => "Alt gauche",
            PttKey::LControl => "Ctrl gauche",
            PttKey::LShift => "Maj gauche",
            PttKey::CapsLock => "Verr. Maj",
            PttKey::F1 => "F1",
            PttKey::F2 => "F2",
            PttKey::F3 => "F3",
            PttKey::F4 => "F4",
            PttKey::F5 => "F5",
            PttKey::F6 => "F6",
            PttKey::F7 => "F7",
            PttKey::F8 => "F8",
            PttKey::Insert => "Inser",
            PttKey::Delete => "Suppr",
            PttKey::Home => "Début",
            PttKey::End => "Fin",
            PttKey::PageUp => "Page haut",
            PttKey::PageDown => "Page bas",
        }
    }

    pub fn keycode(self) -> Keycode {
        match self {
            PttKey::LAlt => Keycode::LAlt,
            PttKey::LControl => Keycode::LControl,
            PttKey::LShift => Keycode::LShift,
            PttKey::CapsLock => Keycode::CapsLock,
            PttKey::F1 => Keycode::F1,
            PttKey::F2 => Keycode::F2,
            PttKey::F3 => Keycode::F3,
            PttKey::F4 => Keycode::F4,
            PttKey::F5 => Keycode::F5,
            PttKey::F6 => Keycode::F6,
            PttKey::F7 => Keycode::F7,
            PttKey::F8 => Keycode::F8,
            PttKey::Insert => Keycode::Insert,
            PttKey::Delete => Keycode::Delete,
            PttKey::Home => Keycode::Home,
            PttKey::End => Keycode::End,
            PttKey::PageUp => Keycode::PageUp,
            PttKey::PageDown => Keycode::PageDown,
        }
    }

    /// Sérialisation stable pour les préférences.
    pub fn id(self) -> &'static str {
        match self {
            PttKey::LAlt => "lalt",
            PttKey::LControl => "lctrl",
            PttKey::LShift => "lshift",
            PttKey::CapsLock => "caps",
            PttKey::F1 => "f1",
            PttKey::F2 => "f2",
            PttKey::F3 => "f3",
            PttKey::F4 => "f4",
            PttKey::F5 => "f5",
            PttKey::F6 => "f6",
            PttKey::F7 => "f7",
            PttKey::F8 => "f8",
            PttKey::Insert => "insert",
            PttKey::Delete => "delete",
            PttKey::Home => "home",
            PttKey::End => "end",
            PttKey::PageUp => "pageup",
            PttKey::PageDown => "pagedown",
        }
    }

    pub fn from_id(id: &str) -> Option<PttKey> {
        Self::ALL.iter().copied().find(|k| k.id() == id)
    }
}

// ---------------------------------------------------------------------------
// Une combinaison de touches (l'enregistreur de clips)
// ---------------------------------------------------------------------------

/// La touche principale d'une combinaison : ce qu'on peut raisonnablement
/// tenir avec un modificateur en pleine partie.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Touche {
    /// F1 à F12.
    F(u8),
    /// A à Z (majuscule ASCII).
    Lettre(u8),
    /// 0 à 9 de la rangée du haut.
    Chiffre(u8),
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Space,
}

impl Touche {
    pub fn keycode(self) -> Keycode {
        match self {
            Touche::F(n) => match n {
                1 => Keycode::F1,
                2 => Keycode::F2,
                3 => Keycode::F3,
                4 => Keycode::F4,
                5 => Keycode::F5,
                6 => Keycode::F6,
                7 => Keycode::F7,
                8 => Keycode::F8,
                9 => Keycode::F9,
                10 => Keycode::F10,
                11 => Keycode::F11,
                _ => Keycode::F12,
            },
            Touche::Lettre(l) => match l {
                b'A' => Keycode::A,
                b'B' => Keycode::B,
                b'C' => Keycode::C,
                b'D' => Keycode::D,
                b'E' => Keycode::E,
                b'F' => Keycode::F,
                b'G' => Keycode::G,
                b'H' => Keycode::H,
                b'I' => Keycode::I,
                b'J' => Keycode::J,
                b'K' => Keycode::K,
                b'L' => Keycode::L,
                b'M' => Keycode::M,
                b'N' => Keycode::N,
                b'O' => Keycode::O,
                b'P' => Keycode::P,
                b'Q' => Keycode::Q,
                b'R' => Keycode::R,
                b'S' => Keycode::S,
                b'T' => Keycode::T,
                b'U' => Keycode::U,
                b'V' => Keycode::V,
                b'W' => Keycode::W,
                b'X' => Keycode::X,
                b'Y' => Keycode::Y,
                _ => Keycode::Z,
            },
            Touche::Chiffre(c) => match c {
                0 => Keycode::Key0,
                1 => Keycode::Key1,
                2 => Keycode::Key2,
                3 => Keycode::Key3,
                4 => Keycode::Key4,
                5 => Keycode::Key5,
                6 => Keycode::Key6,
                7 => Keycode::Key7,
                8 => Keycode::Key8,
                _ => Keycode::Key9,
            },
            Touche::Insert => Keycode::Insert,
            Touche::Delete => Keycode::Delete,
            Touche::Home => Keycode::Home,
            Touche::End => Keycode::End,
            Touche::PageUp => Keycode::PageUp,
            Touche::PageDown => Keycode::PageDown,
            Touche::Space => Keycode::Space,
        }
    }

    /// La touche que représente un code, si c'en est une qu'on accepte.
    pub fn depuis_keycode(k: Keycode) -> Option<Touche> {
        let fs = [
            Keycode::F1,
            Keycode::F2,
            Keycode::F3,
            Keycode::F4,
            Keycode::F5,
            Keycode::F6,
            Keycode::F7,
            Keycode::F8,
            Keycode::F9,
            Keycode::F10,
            Keycode::F11,
            Keycode::F12,
        ];
        if let Some(i) = fs.iter().position(|f| *f == k) {
            return Some(Touche::F(i as u8 + 1));
        }
        for l in b'A'..=b'Z' {
            if Touche::Lettre(l).keycode() == k {
                return Some(Touche::Lettre(l));
            }
        }
        for c in 0..=9u8 {
            if Touche::Chiffre(c).keycode() == k {
                return Some(Touche::Chiffre(c));
            }
        }
        [
            Touche::Insert,
            Touche::Delete,
            Touche::Home,
            Touche::End,
            Touche::PageUp,
            Touche::PageDown,
            Touche::Space,
        ]
        .into_iter()
        .find(|t| t.keycode() == k)
    }

    pub fn label(self) -> String {
        match self {
            Touche::F(n) => format!("F{n}"),
            Touche::Lettre(l) => (l as char).to_string(),
            Touche::Chiffre(c) => c.to_string(),
            Touche::Insert => "Inser".into(),
            Touche::Delete => "Suppr".into(),
            Touche::Home => "Début".into(),
            Touche::End => "Fin".into(),
            Touche::PageUp => "Page haut".into(),
            Touche::PageDown => "Page bas".into(),
            Touche::Space => "Espace".into(),
        }
    }

    fn id(self) -> String {
        match self {
            Touche::F(n) => format!("f{n}"),
            Touche::Lettre(l) => (l as char).to_ascii_lowercase().to_string(),
            Touche::Chiffre(c) => c.to_string(),
            Touche::Insert => "insert".into(),
            Touche::Delete => "delete".into(),
            Touche::Home => "home".into(),
            Touche::End => "end".into(),
            Touche::PageUp => "pageup".into(),
            Touche::PageDown => "pagedown".into(),
            Touche::Space => "space".into(),
        }
    }

    fn depuis_id(id: &str) -> Option<Touche> {
        if let Some(n) = id.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
            return (1..=12).contains(&n).then_some(Touche::F(n));
        }
        if id.len() == 1 {
            let c = id.as_bytes()[0];
            if c.is_ascii_lowercase() {
                return Some(Touche::Lettre(c.to_ascii_uppercase()));
            }
            if c.is_ascii_digit() {
                return Some(Touche::Chiffre(c - b'0'));
            }
        }
        match id {
            "insert" => Some(Touche::Insert),
            "delete" => Some(Touche::Delete),
            "home" => Some(Touche::Home),
            "end" => Some(Touche::End),
            "pageup" => Some(Touche::PageUp),
            "pagedown" => Some(Touche::PageDown),
            "space" => Some(Touche::Space),
            _ => None,
        }
    }
}

/// Une combinaison : des modificateurs et une touche. Alt+F10 par défaut,
/// comme la relecture instantanée de NVIDIA.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Raccourci {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub touche: Touche,
}

impl Raccourci {
    pub const DEFAUT: Raccourci = Raccourci { ctrl: false, alt: true, shift: false, touche: Touche::F(10) };

    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        if self.shift {
            parts.push("Maj".to_string());
        }
        parts.push(self.touche.label());
        parts.join("+")
    }

    /// Sérialisation stable : « alt+f10 ».
    pub fn id(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("ctrl".to_string());
        }
        if self.alt {
            parts.push("alt".to_string());
        }
        if self.shift {
            parts.push("shift".to_string());
        }
        parts.push(self.touche.id());
        parts.join("+")
    }

    pub fn depuis(id: &str) -> Option<Raccourci> {
        let mut r = Raccourci { ctrl: false, alt: false, shift: false, touche: Touche::F(10) };
        let mut touche = None;
        for part in id.split('+') {
            match part {
                "ctrl" => r.ctrl = true,
                "alt" => r.alt = true,
                "shift" => r.shift = true,
                autre => touche = Touche::depuis_id(autre),
            }
        }
        r.touche = touche?;
        Some(r)
    }

    /// Vrai si la combinaison est enfoncée (les modificateurs exigés le
    /// sont, et pas les autres — Alt+F10 n'est pas Ctrl+Alt+F10).
    pub fn enfonce(&self, touches: &[Keycode]) -> bool {
        let ctrl = touches.contains(&Keycode::LControl) || touches.contains(&Keycode::RControl);
        let alt = touches.contains(&Keycode::LAlt) || touches.contains(&Keycode::RAlt);
        let shift = touches.contains(&Keycode::LShift) || touches.contains(&Keycode::RShift);
        ctrl == self.ctrl && alt == self.alt && shift == self.shift && touches.contains(&self.touche.keycode())
    }

    /// La combinaison que l'on tient à l'instant, s'il y a une touche
    /// principale dedans — pour « appuie sur ta combinaison ».
    pub fn depuis_touches(touches: &[Keycode]) -> Option<Raccourci> {
        let touche = touches.iter().find_map(|k| Touche::depuis_keycode(*k))?;
        Some(Raccourci {
            ctrl: touches.contains(&Keycode::LControl) || touches.contains(&Keycode::RControl),
            alt: touches.contains(&Keycode::LAlt) || touches.contains(&Keycode::RAlt),
            shift: touches.contains(&Keycode::LShift) || touches.contains(&Keycode::RShift),
            touche,
        })
    }
}

// ---------------------------------------------------------------------------
// Surveillance de la touche, hors de la boucle de rendu
// ---------------------------------------------------------------------------

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use device_query::{DeviceQuery, DeviceState};
use eframe::egui;

/// Cadence de sondage du clavier.
///
/// Le sondage vivait dans `update()`, donc à la cadence de rendu : vingt fois
/// par seconde. C'était doublement mauvais. Pour la touche, parce qu'une
/// pression brève de moins de cinquante millisecondes passait entre deux
/// images — sur un push-to-talk, rater une pression c'est rater une phrase.
/// Pour le reste de l'application, parce que cette contrainte imposait de
/// repeindre en permanence, y compris fenêtre réduite pendant une partie.
///
/// Cent hertz coûtent une fraction de pour-cent — `GetAsyncKeyState` lit un
/// tableau que Windows tient déjà à jour — et rendent la touche plus fiable
/// qu'elle ne l'a jamais été.
const PERIODE: Duration = Duration::from_millis(10);

/// Valeur de `key` signifiant « on ne surveille rien » : hors mode
/// push-to-talk, il n'y a aucune raison de lire le clavier.
const AUCUNE: u8 = u8::MAX;

/// Surveille la touche push-to-talk sur un fil dédié et réveille l'interface
/// aux seuls changements d'état.
///
/// C'est ce réveil qui rend possible le repeint conditionnel : l'interface
/// n'a plus besoin de tourner en boucle pour savoir si l'on parle.
pub struct Watcher {
    /// Indice dans [`PttKey::ALL`], ou [`AUCUNE`].
    key: Arc<AtomicU8>,
    /// Maintien après relâchement, en millisecondes.
    release_ms: Arc<AtomicU32>,
    /// Touche enfoncée, ou relâchée depuis moins que le maintien.
    active: Arc<AtomicBool>,
    /// Les raccourcis à bascule — couper le micro, se rendre sourd — : leur
    /// touche (ou [`AUCUNE`]) et le nombre de pressions vues, que
    /// l'interface compare à ce qu'elle a déjà traité. Un compteur plutôt
    /// qu'un drapeau : deux pressions entre deux images ne s'annulent pas
    /// l'une l'autre par accident, elles se voient.
    bascules: Arc<[(AtomicU8, AtomicU32); 2]>,
    /// Le raccourci de l'enregistreur de clips (une combinaison), le
    /// nombre d'appuis vus, et le mode « appuie sur ta combinaison » avec
    /// ce qu'il a vu.
    raccourci: Arc<Mutex<Option<Raccourci>>>,
    appuis_raccourci: Arc<AtomicU32>,
    capture: Arc<AtomicBool>,
    capturee: Arc<Mutex<Option<Raccourci>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// Les raccourcis à bascule, dans l'ordre des cases de `bascules`.
#[derive(Clone, Copy)]
pub enum Bascule {
    Micro = 0,
    Sourd = 1,
}

impl Watcher {
    pub fn start(ctx: egui::Context) -> Self {
        let key = Arc::new(AtomicU8::new(AUCUNE));
        let release_ms = Arc::new(AtomicU32::new(0));
        let active = Arc::new(AtomicBool::new(false));
        let bascules = Arc::new([
            (AtomicU8::new(AUCUNE), AtomicU32::new(0)),
            (AtomicU8::new(AUCUNE), AtomicU32::new(0)),
        ]);
        let stop = Arc::new(AtomicBool::new(false));
        let raccourci = Arc::new(Mutex::new(None));
        let appuis_raccourci = Arc::new(AtomicU32::new(0));
        let capture = Arc::new(AtomicBool::new(false));
        let capturee = Arc::new(Mutex::new(None));

        let handle = std::thread::Builder::new()
            .name("ki-ptt".into())
            .spawn({
                let (key, release_ms, active, bascules, stop) = (
                    key.clone(),
                    release_ms.clone(),
                    active.clone(),
                    bascules.clone(),
                    stop.clone(),
                );
                let combo = Combo {
                    raccourci: raccourci.clone(),
                    appuis: appuis_raccourci.clone(),
                    capture: capture.clone(),
                    capturee: capturee.clone(),
                };
                move || boucle(ctx, key, release_ms, active, bascules, combo, stop)
            })
            .ok();

        Self {
            key,
            release_ms,
            active,
            bascules,
            raccourci,
            appuis_raccourci,
            capture,
            capturee,
            stop,
            handle,
        }
    }

    /// Règle la combinaison de l'enregistreur de clips. `None` = aucune.
    pub fn watch_raccourci(&self, r: Option<Raccourci>) {
        *self.raccourci.lock().unwrap() = r;
    }

    /// Nombre d'appuis vus sur la combinaison depuis le démarrage.
    pub fn appuis_raccourci(&self) -> u32 {
        self.appuis_raccourci.load(Ordering::Relaxed)
    }

    /// Mode « appuie sur ta combinaison » : la prochaine combinaison tenue
    /// est retenue, et le mode s'éteint.
    pub fn capturer(&self, on: bool) {
        if on {
            *self.capturee.lock().unwrap() = None;
        }
        self.capture.store(on, Ordering::Relaxed);
    }

    pub fn en_capture(&self) -> bool {
        self.capture.load(Ordering::Relaxed)
    }

    /// La combinaison capturée, une fois.
    pub fn capturee(&self) -> Option<Raccourci> {
        self.capturee.lock().unwrap().take()
    }

    /// Règle la touche surveillée. `None` = ne rien surveiller.
    pub fn watch(&self, key: Option<PttKey>) {
        self.key.store(index_de(key), Ordering::Relaxed);
    }

    /// Règle la touche d'un raccourci à bascule. `None` = pas de raccourci.
    pub fn watch_bascule(&self, quoi: Bascule, key: Option<PttKey>) {
        self.bascules[quoi as usize].0.store(index_de(key), Ordering::Relaxed);
    }

    /// Nombre de pressions vues sur ce raccourci depuis le démarrage :
    /// l'interface bascule autant de fois que la valeur a avancé.
    pub fn pressions(&self, quoi: Bascule) -> u32 {
        self.bascules[quoi as usize].1.load(Ordering::Relaxed)
    }

    pub fn set_release_ms(&self, ms: u32) {
        self.release_ms.store(ms, Ordering::Relaxed);
    }

    /// Vrai si l'on doit émettre : touche enfoncée, ou relâchée depuis moins
    /// que le maintien.
    pub fn active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Ouvre le clavier global. Windows le lit d'office (`GetAsyncKeyState`).
///
/// macOS ne laisse lire les touches destinées aux autres applications qu'aux
/// applications que l'utilisateur a inscrites dans *Accessibilité*. Deux
/// pièges, appris sur le terrain :
///
/// - `device_query::DeviceState::new()` **panique** sans l'autorisation ;
/// - sa version vérifiée, `checked_new()`, **affiche la demande système à
///   chaque appel** tant que l'autorisation manque. Appelée toutes les
///   secondes, elle empilait des dizaines de fenêtres « Accès
///   d'accessibilité » que rien ne fermait.
///
/// D'où la règle : la demande (avec sa fenêtre et son bouton vers les
/// Réglages) part **une fois**, au premier besoin ; ensuite on interroge le
/// système en silence (`AXIsProcessTrusted`, sans fenêtre) et l'on n'ouvre
/// le clavier que le jour où il dit oui — sans redémarrer.
#[cfg(target_os = "macos")]
fn ouvrir_clavier(deja_demande: bool) -> Option<DeviceState> {
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        /// `Boolean` en C : un octet, 0 ou 1.
        fn AXIsProcessTrusted() -> u8;
    }
    // SAFETY : aucun argument, aucun état — une simple question au système.
    let accorde = unsafe { AXIsProcessTrusted() } != 0;
    if accorde || !deja_demande {
        DeviceState::checked_new()
    } else {
        None
    }
}

#[cfg(not(target_os = "macos"))]
fn ouvrir_clavier(_deja_demande: bool) -> Option<DeviceState> {
    Some(DeviceState::new())
}

/// Le clavier est refusé, et il y a une touche à surveiller : on le dit au
/// journal, qui voyage avec les diagnostics. Sur macOS, la fenêtre du
/// système vient d'être montrée par `ouvrir_clavier` — rien à ajouter.
fn clavier_refuse() {
    ki_voice::journal(
        "push-to-talk : le clavier n'est pas lisible — sur macOS, autoriser ki-chat dans \
         Réglages Système → Confidentialité et sécurité → Accessibilité"
            .to_string(),
    );
}

/// L'indice d'une touche dans [`PttKey::ALL`], ou [`AUCUNE`].
fn index_de(key: Option<PttKey>) -> u8 {
    key.and_then(|k| PttKey::ALL.iter().position(|c| *c == k))
        .map(|i| i as u8)
        .unwrap_or(AUCUNE)
}

/// Ce que la boucle tient pour la combinaison de l'enregistreur.
struct Combo {
    raccourci: Arc<Mutex<Option<Raccourci>>>,
    appuis: Arc<AtomicU32>,
    capture: Arc<AtomicBool>,
    capturee: Arc<Mutex<Option<Raccourci>>>,
}

fn boucle(
    ctx: egui::Context,
    key: Arc<AtomicU8>,
    release_ms: Arc<AtomicU32>,
    active: Arc<AtomicBool>,
    bascules: Arc<[(AtomicU8, AtomicU32); 2]>,
    combo: Combo,
    stop: Arc<AtomicBool>,
) {
    // Le clavier n'est pas toujours lisible : voir `ouvrir_clavier`. On
    // l'ouvre au premier besoin, et on réessaie tant qu'il refuse — sur
    // macOS, l'autorisation peut être accordée pendant que l'on tourne.
    let mut device: Option<DeviceState> = None;
    let mut prevenu = false;
    let mut dernier_appui: Option<Instant> = None;
    // Les bascules réagissent au front : une touche tenue enfoncée ne
    // bascule qu'une fois.
    let mut enfoncees_avant = [false; 2];
    let mut combo_avant = false;

    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(PERIODE);

        let index = key.load(Ordering::Relaxed);
        let indices_bascules = [
            bascules[0].0.load(Ordering::Relaxed),
            bascules[1].0.load(Ordering::Relaxed),
        ];
        let raccourci = *combo.raccourci.lock().unwrap();
        let en_capture = combo.capture.load(Ordering::Relaxed);
        // Rien à surveiller : on ne lit même pas le clavier. Une
        // application qui interroge le clavier en permanence sans en avoir
        // l'usage n'a rien à faire sur la machine de quelqu'un.
        if index == AUCUNE
            && indices_bascules.iter().all(|i| *i == AUCUNE)
            && raccourci.is_none()
            && !en_capture
        {
            dernier_appui = None;
            enfoncees_avant = [false; 2];
            combo_avant = false;
            if active.swap(false, Ordering::Relaxed) {
                ctx.request_repaint();
            }
            continue;
        }
        if device.is_none() {
            device = ouvrir_clavier(prevenu);
            if device.is_none() {
                if !prevenu {
                    prevenu = true;
                    clavier_refuse();
                }
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        }
        let touches = device.as_ref().expect("clavier ouvert à l'instant").get_keys();

        let voulu = if index == AUCUNE {
            dernier_appui = None;
            false
        } else {
            let touche = PttKey::ALL[index as usize].keycode();
            let enfoncee = touches.contains(&touche);
            if enfoncee {
                dernier_appui = Some(Instant::now());
            }
            // Le maintien évite de couper la dernière syllabe.
            let maintien = Duration::from_millis(release_ms.load(Ordering::Relaxed) as u64);
            enfoncee || dernier_appui.is_some_and(|t| t.elapsed() < maintien)
        };

        let mut reveil = false;
        for (i, idx) in indices_bascules.iter().enumerate() {
            let enfoncee =
                *idx != AUCUNE && touches.contains(&PttKey::ALL[*idx as usize].keycode());
            if enfoncee && !enfoncees_avant[i] {
                bascules[i].1.fetch_add(1, Ordering::Relaxed);
                reveil = true;
            }
            enfoncees_avant[i] = enfoncee;
        }

        // La combinaison de l'enregistreur : au front, comme les bascules.
        let combo_enfonce = raccourci.is_some_and(|r| r.enfonce(&touches));
        if combo_enfonce && !combo_avant {
            combo.appuis.fetch_add(1, Ordering::Relaxed);
            reveil = true;
        }
        combo_avant = combo_enfonce;
        // « Appuie sur ta combinaison » : la première tenue est retenue.
        if en_capture {
            if let Some(r) = Raccourci::depuis_touches(&touches) {
                *combo.capturee.lock().unwrap() = Some(r);
                combo.capture.store(false, Ordering::Relaxed);
                reveil = true;
            }
        }

        // On ne réveille l'interface qu'aux changements : c'est ce qui permet
        // à l'application de dormir le reste du temps.
        if active.swap(voulu, Ordering::Relaxed) != voulu || reveil {
            ctx.request_repaint();
        }
    }
}

#[cfg(test)]
mod tests_raccourci {
    use super::*;

    #[test]
    fn un_raccourci_fait_l_aller_retour_par_son_identifiant() {
        let r = Raccourci::DEFAUT;
        assert_eq!(r.id(), "alt+f10");
        assert_eq!(r.label(), "Alt+F10");
        assert_eq!(Raccourci::depuis("alt+f10"), Some(r));
        let c = Raccourci { ctrl: true, alt: false, shift: true, touche: Touche::Lettre(b'K') };
        assert_eq!(Raccourci::depuis(&c.id()), Some(c));
        assert_eq!(c.label(), "Ctrl+Maj+K");
        assert_eq!(Raccourci::depuis("alt+"), None);
        assert_eq!(Raccourci::depuis("f13"), None);
    }

    #[test]
    fn la_combinaison_exige_ses_modificateurs_et_pas_les_autres() {
        let r = Raccourci::DEFAUT;
        assert!(r.enfonce(&[Keycode::LAlt, Keycode::F10]));
        assert!(r.enfonce(&[Keycode::RAlt, Keycode::F10, Keycode::A]));
        assert!(!r.enfonce(&[Keycode::F10]));
        assert!(!r.enfonce(&[Keycode::LAlt, Keycode::LControl, Keycode::F10]));
        assert_eq!(
            Raccourci::depuis_touches(&[Keycode::LShift, Keycode::Key5]),
            Some(Raccourci { ctrl: false, alt: false, shift: true, touche: Touche::Chiffre(5) })
        );
        assert_eq!(Raccourci::depuis_touches(&[Keycode::LShift]), None);
    }
}
