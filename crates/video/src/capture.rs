//! Capture d'écran ou de fenêtre : ce que l'on diffuse, et d'où.
//!
//! Les types que l'interface manipule (source, écrans, fenêtres, trames,
//! drapeaux) vivent ici et sont les mêmes partout. La capture elle-même est
//! une affaire de système : Windows.Graphics.Capture sous Windows
//! (`wgc.rs`) ; ailleurs, rien encore — `absent.rs` le dit au lieu de
//! planter, et le reste du pipeline (décodage, visionnage) reste entier.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::Arc;
use std::time::Duration;

use crate::stats::StageStats;

#[cfg(windows)]
mod wgc;
#[cfg(windows)]
pub use wgc::{list_monitors, list_windows, start_capture, Control, ScreenGrab};

#[cfg(not(windows))]
mod absent;
#[cfg(not(windows))]
pub use absent::{list_monitors, list_windows, start_capture, Control};

/// Ce que l'on diffuse : un écran entier ou une seule fenêtre.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureSource {
    /// Un moniteur par son rang d'énumération (1 = premier) ; 0 = le
    /// principal, quel que soit son rang.
    Monitor(usize),
    /// Une fenêtre, retrouvée par son titre exact au moment du démarrage.
    Window(String),
}

impl Default for CaptureSource {
    fn default() -> Self {
        Self::Monitor(0)
    }
}

/// Un écran tel que le sélecteur le présente.
#[derive(Clone, Debug)]
pub struct MonitorInfo {
    /// Rang d'énumération, à donner à `CaptureSource::Monitor`.
    pub index: usize,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

/// Une fenêtre capturable, telle que le sélecteur la présente.
#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub title: String,
    /// Nom de l'exécutable (« valorant.exe »), pour reconnaître le jeu
    /// derrière un titre de fenêtre obscur.
    pub process: String,
}


/// Une trame BGRA serrée, dimensions comprises.
pub struct CapturedFrame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// Tout ce que le thread de capture reçoit à sa création.
pub struct CaptureFlags {
    pub stats: Arc<StageStats>,
    pub tx: SyncSender<CapturedFrame>,
    /// Tampons rendus par le pipeline, à réutiliser.
    pub recycle: Receiver<Vec<u8>>,
    /// Levé quand la source disparaît (fenêtre fermée) : plus aucune trame
    /// ne viendra, c'est à l'appelant de conclure.
    pub closed: Arc<AtomicBool>,
    /// Écart minimal entre deux trames retenues : la cadence demandée,
    /// tenue ici quel que soit le rythme auquel Windows livre.
    pub interval: Duration,
}
