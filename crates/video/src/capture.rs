//! Capture d'écran ou de fenêtre via Windows.Graphics.Capture.
//!
//! Le rappel `on_frame_arrived` tourne sur le thread de capture de
//! windows-capture : il doit rester léger. Ici il ne fait que dé-padder le
//! BGRA et le pousser (sans bloquer) vers le thread pipeline ; si celui-ci
//! est occupé, la trame est sautée — c'est le comportement voulu (mieux vaut
//! sauter que retarder).
//!
//! Les tampons circulent en boucle fermée (canal de recyclage) : pas
//! d'allocation de 8 Mo à 30 Hz en régime établi.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::Duration;

use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    GraphicsCaptureItemType, MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};
use windows_capture::window::Window;

use crate::stats::StageStats;

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

/// Les écrans, dans l'ordre d'énumération de Windows.
pub fn list_monitors() -> Vec<MonitorInfo> {
    let primary = Monitor::primary().ok();
    Monitor::enumerate()
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(i, m)| MonitorInfo {
            index: i + 1,
            name: m.name().unwrap_or_else(|_| format!("Écran {}", i + 1)),
            width: m.width().unwrap_or(0),
            height: m.height().unwrap_or(0),
            primary: primary.map(|p| p == m).unwrap_or(i == 0),
        })
        .collect()
}

/// Les fenêtres visibles qui ont un titre — sans le bureau ni les fenêtres
/// utilitaires du système, qui ne sont pas des choses que l'on diffuse.
pub fn list_windows() -> Vec<WindowInfo> {
    Window::enumerate()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|w| {
            let title = w.title().ok()?;
            let title = title.trim().to_string();
            if title.is_empty()
                || title == "Program Manager"
                || title == "Windows Input Experience"
                || title == "Paramètres"
                || title == "Settings"
            {
                return None;
            }
            let process = w.process_name().unwrap_or_default();
            Some(WindowInfo { title, process })
        })
        .collect()
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
}

pub struct ScreenGrab {
    flags: CaptureFlags,
    scratch: Vec<u8>,
}

impl GraphicsCaptureApiHandler for ScreenGrab {
    type Flags = CaptureFlags;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { flags: ctx.flags, scratch: Vec::new() })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let (w, h) = (frame.width(), frame.height());
        let buffer = frame.buffer()?;
        let bgra = buffer.as_nopadding_buffer(&mut self.scratch);

        self.flags.stats.captured.fetch_add(1, Ordering::Relaxed);

        // Tampon recyclé si disponible, sinon nouveau (démarrage).
        let mut owned = self.flags.recycle.try_recv().unwrap_or_default();
        owned.clear();
        owned.extend_from_slice(bgra);

        match self.flags.tx.try_send(CapturedFrame { width: w, height: h, bgra: owned }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                // Pipeline occupé : on saute la trame (le tampon repartira
                // au recyclage via le drop, on en réallouera un — rare).
                self.flags.stats.skipped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {
                // Le pipeline est arrêté : la capture va être stoppée par
                // le handle, rien à faire ici.
            }
        }
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        self.flags.closed.store(true, Ordering::Relaxed);
        Ok(())
    }
}

/// Le contrôle d'une capture en cours — à `stop()` pour l'arrêter.
pub type Control = CaptureControl<ScreenGrab, <ScreenGrab as GraphicsCaptureApiHandler>::Error>;

/// Démarre la capture de `source`, à `fps` images/s au plus, curseur ou non,
/// sans bordure jaune quand l'OS le permet.
pub fn start_capture(
    source: &CaptureSource,
    cursor: bool,
    fps: u32,
    flags: CaptureFlags,
) -> anyhow::Result<Control> {
    match source {
        CaptureSource::Monitor(0) => {
            let m = Monitor::primary().map_err(|e| anyhow::anyhow!("moniteur principal : {e}"))?;
            lancer(m, cursor, fps, flags)
        }
        CaptureSource::Monitor(i) => {
            let m = Monitor::from_index(*i).map_err(|e| anyhow::anyhow!("écran {i} : {e}"))?;
            lancer(m, cursor, fps, flags)
        }
        CaptureSource::Window(title) => {
            let w = Window::from_name(title)
                .map_err(|_| anyhow::anyhow!("fenêtre « {title} » introuvable — fermée ?"))?;
            lancer(w, cursor, fps, flags)
        }
    }
}

fn lancer<T>(item: T, cursor: bool, fps: u32, flags: CaptureFlags) -> anyhow::Result<Control>
where
    T: TryInto<GraphicsCaptureItemType> + Send + 'static,
{
    let settings = Settings::new(
        item,
        if cursor { CursorCaptureSettings::WithCursor } else { CursorCaptureSettings::WithoutCursor },
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        // Limite la cadence de livraison ; l'OS ne livre de toute façon que
        // quand l'image change (écran statique = peu de trames, c'est normal).
        MinimumUpdateIntervalSettings::Custom(Duration::from_micros(
            1_000_000 / u64::from(fps.clamp(1, 120)),
        )),
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        flags,
    );
    ScreenGrab::start_free_threaded(settings)
        .map_err(|e| anyhow::anyhow!("démarrage de la capture : {e}"))
}
