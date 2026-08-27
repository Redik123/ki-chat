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
// Surveillance de la touche, hors de la boucle de rendu
// ---------------------------------------------------------------------------

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;
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
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Watcher {
    pub fn start(ctx: egui::Context) -> Self {
        let key = Arc::new(AtomicU8::new(AUCUNE));
        let release_ms = Arc::new(AtomicU32::new(0));
        let active = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));

        let handle = std::thread::Builder::new()
            .name("ki-ptt".into())
            .spawn({
                let (key, release_ms, active, stop) =
                    (key.clone(), release_ms.clone(), active.clone(), stop.clone());
                move || boucle(ctx, key, release_ms, active, stop)
            })
            .ok();

        Self { key, release_ms, active, stop, handle }
    }

    /// Règle la touche surveillée. `None` = ne rien surveiller.
    pub fn watch(&self, key: Option<PttKey>) {
        let index = key
            .and_then(|k| PttKey::ALL.iter().position(|c| *c == k))
            .map(|i| i as u8)
            .unwrap_or(AUCUNE);
        self.key.store(index, Ordering::Relaxed);
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

fn boucle(
    ctx: egui::Context,
    key: Arc<AtomicU8>,
    release_ms: Arc<AtomicU32>,
    active: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) {
    let device = DeviceState::new();
    let mut dernier_appui: Option<Instant> = None;

    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(PERIODE);

        let index = key.load(Ordering::Relaxed);
        let voulu = if index == AUCUNE {
            // Hors push-to-talk : on ne lit même pas le clavier. Une
            // application qui interroge le clavier en permanence sans en
            // avoir l'usage n'a rien à faire sur la machine de quelqu'un.
            dernier_appui = None;
            false
        } else {
            let touche = PttKey::ALL[index as usize].keycode();
            let enfoncee = device.get_keys().contains(&touche);
            if enfoncee {
                dernier_appui = Some(Instant::now());
            }
            // Le maintien évite de couper la dernière syllabe.
            let maintien = Duration::from_millis(release_ms.load(Ordering::Relaxed) as u64);
            enfoncee || dernier_appui.is_some_and(|t| t.elapsed() < maintien)
        };

        // On ne réveille l'interface qu'aux changements : c'est ce qui permet
        // à l'application de dormir le reste du temps.
        if active.swap(voulu, Ordering::Relaxed) != voulu {
            ctx.request_repaint();
        }
    }
}
