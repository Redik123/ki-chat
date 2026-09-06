//! Là où l'on ne sait pas encore capturer l'écran : macOS et Linux.
//!
//! Le partage d'écran a été écrit contre Windows.Graphics.Capture, et sa
//! capture n'a pas encore d'équivalent ici (ce serait ScreenCaptureKit sur
//! macOS, PipeWire sous Linux). On ne fait pas semblant : aucun écran ni
//! fenêtre à proposer, et démarrer une diffusion dit pourquoi ça ne se peut
//! pas. **Regarder** la diffusion d'un autre, elle, marche partout — le
//! décodeur H.264 est le même sur toutes les plateformes.

use super::{CaptureFlags, CaptureSource, MonitorInfo, WindowInfo};

/// Aucun écran à proposer : le sélecteur reste vide, et le dit.
pub fn list_monitors() -> Vec<MonitorInfo> {
    Vec::new()
}

/// Aucune fenêtre non plus.
pub fn list_windows() -> Vec<WindowInfo> {
    Vec::new()
}

/// Le contrôle d'une capture — qui n'existe jamais ici, mais le type doit
/// exister pour que le pipeline compile tel quel.
pub struct Control;

impl Control {
    pub fn stop(self) -> Result<(), std::convert::Infallible> {
        Ok(())
    }
}

/// Refuse, en expliquant. Le message remonte tel quel dans l'interface.
pub fn start_capture(
    _source: &CaptureSource,
    _cursor: bool,
    _fps: u32,
    _flags: CaptureFlags,
) -> anyhow::Result<Control> {
    anyhow::bail!(
        "la diffusion d'écran n'est pas encore disponible sur {} — regarder celle des \
         autres, si",
        std::env::consts::OS
    )
}
