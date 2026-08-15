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
