//! Capture via Windows.Graphics.Capture.
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

use std::sync::atomic::Ordering;
use std::sync::mpsc::TrySendError;
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

use windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;

use super::{CaptureFlags, CaptureSource, CapturedFrame, MonitorInfo, WindowInfo};


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


pub struct ScreenGrab {
    flags: CaptureFlags,
    /// La texture de transit, lisible par le processeur, gardée d'une image
    /// à l'autre. `Frame::buffer()` de windows-capture en crée une neuve à
    /// chaque image : 8 Mo (en 1080p) alloués, épinglés pour la carte puis
    /// rendus, soixante fois par seconde — et une copie de plus vers son
    /// tampon sans remplissage, par rayon quand les lignes en ont.
    transit: Option<(ID3D11Texture2D, D3D11_TEXTURE2D_DESC)>,
    /// Un tampon que le pipeline n'a pas pu prendre (il était occupé) :
    /// gardé pour l'image suivante plutôt que rendu au système.
    reserve: Option<Vec<u8>>,
    /// Les tampons en circulation : deux au plus, l'un chez le pipeline,
    /// l'autre qu'on remplit.
    tampons: usize,
    /// Instant de la dernière trame retenue.
    last: Option<Instant>,
}

impl ScreenGrab {
    /// Copie l'image de la carte vers `sortie` (BGRA serré), par la texture
    /// de transit.
    fn lire(&mut self, frame: &mut Frame, sortie: &mut Vec<u8>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let desc = *frame.desc();
        let a_refaire = self
            .transit
            .as_ref()
            .is_none_or(|(_, d)| (d.Width, d.Height, d.Format) != (desc.Width, desc.Height, desc.Format));
        if a_refaire {
            let d = D3D11_TEXTURE2D_DESC {
                Width: desc.Width,
                Height: desc.Height,
                MipLevels: 1,
                ArraySize: 1,
                Format: desc.Format,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut t: Option<ID3D11Texture2D> = None;
            unsafe { frame.device().CreateTexture2D(&d, None, Some(&mut t))? };
            self.transit = Some((t.ok_or("texture de transit absente")?, d));
        }
        let Some((transit, _)) = self.transit.as_ref() else { return Err("texture de transit absente".into()) };
        let ctx = frame.device_context();
        let (w, h) = (desc.Width as usize, desc.Height as usize);
        let ligne = w * 4;
        sortie.resize(ligne * h, 0);
        unsafe {
            ctx.CopyResource(transit, frame.as_raw_texture());
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(transit, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
            let src = m.pData as *const u8;
            let pas = m.RowPitch as usize;
            if pas == ligne {
                std::ptr::copy_nonoverlapping(src, sortie.as_mut_ptr(), ligne * h);
            } else {
                for y in 0..h {
                    std::ptr::copy_nonoverlapping(src.add(y * pas), sortie.as_mut_ptr().add(y * ligne), ligne);
                }
            }
            ctx.Unmap(transit, 0);
        }
        Ok(())
    }
}

impl GraphicsCaptureApiHandler for ScreenGrab {
    type Flags = CaptureFlags;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { flags: ctx.flags, transit: None, reserve: None, tampons: 0, last: None })
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
        // Un tampon à remplir : gardé, recyclé, ou neuf tant qu'il n'y en a
        // pas deux. Sinon le pipeline tient encore les deux : la trame est
        // sautée AVANT d'être lue de la carte — elle ne coûte rien.
        let mut owned = match self.reserve.take().or_else(|| self.flags.recycle.try_recv().ok()) {
            Some(b) => b,
            None if self.tampons < 2 => {
                self.tampons += 1;
                Vec::new()
            }
            None => {
                self.flags.stats.skipped.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
        };
        self.last = Some(now);

        let (w, h) = (frame.width(), frame.height());
        if let Err(e) = self.lire(frame, &mut owned) {
            self.reserve = Some(owned);
            return Err(e);
        }
        self.flags.stats.captured.fetch_add(1, Ordering::Relaxed);

        match self.flags.tx.try_send(CapturedFrame { width: w, height: h, bgra: owned }) {
            Ok(()) => {}
            Err(TrySendError::Full(f)) => {
                // Pipeline occupé : on saute la trame, et l'on garde son
                // tampon pour la suivante.
                self.flags.stats.skipped.fetch_add(1, Ordering::Relaxed);
                self.reserve = Some(f.bgra);
            }
            Err(TrySendError::Disconnected(f)) => {
                // Le pipeline est arrêté : la capture va être stoppée par
                // le handle, rien à faire ici.
                self.reserve = Some(f.bgra);
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
            // Le titre exact d'abord, puis aux espaces de bord près :
            // `list_windows` rend les titres nettoyés, et celui de VALORANT
            // finit par deux espaces — la recherche exacte ne le trouvait
            // jamais.
            let w = Window::from_name(title)
                .ok()
                .or_else(|| {
                    Window::enumerate()
                        .ok()?
                        .into_iter()
                        .find(|w| w.title().is_ok_and(|t| t.trim() == title.trim()))
                })
                .ok_or_else(|| anyhow::anyhow!("fenêtre « {title} » introuvable — fermée ?"))?;
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
