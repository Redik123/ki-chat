//! Capture de l'écran principal via Windows.Graphics.Capture.
//!
//! Le rappel `on_frame_arrived` tourne sur le thread de capture de
//! windows-capture : il doit rester léger. Ici il ne fait que dé-padder le
//! BGRA et le pousser (sans bloquer) vers le thread pipeline ; si celui-ci
//! est occupé, la trame est sautée — c'est le comportement voulu (mieux vaut
//! sauter que retarder).
//!
//! Les tampons circulent en boucle fermée (canal de recyclage) : pas
//! d'allocation de 8 Mo à 30 Hz en régime établi.

use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::Duration;

use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

use crate::stats::StageStats;

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
        use std::sync::atomic::Ordering;

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
}

/// Démarre la capture du moniteur principal (~30 images/s max, curseur
/// inclus, sans bordure jaune quand l'OS le permet).
pub fn start_primary_monitor(
    flags: CaptureFlags,
) -> anyhow::Result<windows_capture::capture::CaptureControl<ScreenGrab, <ScreenGrab as GraphicsCaptureApiHandler>::Error>> {
    let monitor = Monitor::primary().map_err(|e| anyhow::anyhow!("moniteur principal : {e}"))?;
    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::WithCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        // Limite la cadence de livraison ; l'OS ne livre de toute façon que
        // quand l'écran change (écran statique = peu de trames, c'est normal).
        MinimumUpdateIntervalSettings::Custom(Duration::from_millis(33)),
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        flags,
    );
    ScreenGrab::start_free_threaded(settings)
        .map_err(|e| anyhow::anyhow!("démarrage de la capture : {e}"))
}
