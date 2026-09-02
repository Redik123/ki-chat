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
//!
//! Les options de capture ne sont pas toutes de tous les Windows : la
//! bordure jaune ne se retire qu'à partir de Windows 11, l'intervalle minimal
//! entre images n'existe que depuis Windows 11 24H2 — et windows-capture
//! **refuse de démarrer** si l'on demande ce que l'OS n'a pas. Vu sur le
//! terrain : la diffusion ne marchait que sur la machine du développeur.
//! Chaque option se demande donc seulement si Windows la connaît, et la
//! cadence se tient de toute façon ici, en sautant les trames trop tôt.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::{GraphicsCaptureApi, InternalCaptureControl};
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
    /// Écart minimal entre deux trames retenues : la cadence demandée,
    /// tenue ici quel que soit le rythme auquel Windows livre.
    pub interval: Duration,
}

pub struct ScreenGrab {
    flags: CaptureFlags,
    scratch: Vec<u8>,
    /// Instant de la dernière trame retenue.
    last: Option<Instant>,
}

impl GraphicsCaptureApiHandler for ScreenGrab {
    type Flags = CaptureFlags;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { flags: ctx.flags, scratch: Vec::new(), last: None })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        // La cadence, avant de toucher au tampon : une trame trop tôt ne
        // coûte rien, pas même sa copie. Un dixième de tolérance, sinon un
        // écran à 60 Hz demandé à 30 i/s en donnerait 20.
        let now = Instant::now();
        if let Some(last) = self.last {
            if now.duration_since(last) < self.flags.interval.mul_f32(0.9) {
                return Ok(());
            }
        }
        self.last = Some(now);

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
    if !GraphicsCaptureApi::is_supported().unwrap_or(false) {
        anyhow::bail!(
            "la capture d'écran de Windows (Windows.Graphics.Capture) n'est pas disponible \
             sur cette machine — il faut Windows 10 version 1903 ou plus récent"
        );
    }
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

/// Les options que ce Windows accepte. Ce qu'il n'a pas reste au défaut —
/// et se dit une fois au journal, pour qu'un « pourquoi la bordure jaune »
/// trouve sa réponse sans qu'on la cherche.
fn options(cursor: bool, fps: u32) -> (CursorCaptureSettings, DrawBorderSettings, MinimumUpdateIntervalSettings) {
    let sait = |f: fn() -> Result<bool, windows_capture::graphics_capture_api::Error>| {
        f().unwrap_or(false)
    };
    let curseur = if sait(GraphicsCaptureApi::is_cursor_settings_supported) {
        if cursor { CursorCaptureSettings::WithCursor } else { CursorCaptureSettings::WithoutCursor }
    } else {
        CursorCaptureSettings::Default
    };
    let bordure = if sait(GraphicsCaptureApi::is_border_settings_supported) {
        DrawBorderSettings::WithoutBorder
    } else {
        crate::journal("capture : ce Windows ne sait pas retirer la bordure jaune (Windows 11 requis)");
        DrawBorderSettings::Default
    };
    let cadence = if sait(GraphicsCaptureApi::is_minimum_update_interval_supported) {
        MinimumUpdateIntervalSettings::Custom(intervalle(fps))
    } else {
        MinimumUpdateIntervalSettings::Default
    };
    (curseur, bordure, cadence)
}

fn intervalle(fps: u32) -> Duration {
    Duration::from_micros(1_000_000 / u64::from(fps.clamp(1, 120)))
}

fn lancer<T>(item: T, cursor: bool, fps: u32, mut flags: CaptureFlags) -> anyhow::Result<Control>
where
    T: TryInto<GraphicsCaptureItemType> + Send + 'static,
{
    let (curseur, bordure, cadence) = options(cursor, fps);
    flags.interval = intervalle(fps);
    let settings = Settings::new(
        item,
        curseur,
        bordure,
        SecondaryWindowSettings::Default,
        cadence,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        flags,
    );
    ScreenGrab::start_free_threaded(settings)
        .map_err(|e| anyhow::anyhow!("démarrage de la capture : {e}"))
}
