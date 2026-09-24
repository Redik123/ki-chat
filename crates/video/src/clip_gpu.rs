//! Les clips sans le processeur : la capture, la conversion et l'encodage
//! restent sur la carte graphique (PLAN-CLIPS.md, C4 « chemin tout-GPU »).
//!
//! L'ancienne chaîne — celle du partage d'écran, réglée en clip — lisait
//! chaque image de la carte vers la mémoire centrale (8 Mo en 1080p, soixante
//! fois par seconde, dans une texture de transit recréée à chaque image),
//! la convertissait en I420 sur un cœur, la repliait en NV12, puis la
//! renvoyait à la carte pour NVENC ; et NVENC, réglé « haute qualité »,
//! passait par CUDA (deux passes, AQ spatiale et temporelle) — du temps de
//! carte pris au jeu à chaque image. Ici :
//!
//! 1. **Un seul device Direct3D 11**, sur la carte NVIDIA : la capture
//!    (Windows.Graphics.Capture), la conversion et NVENC le partagent, les
//!    textures ne changent jamais de mains.
//! 2. **La capture au rythme voulu** : une réserve d'une seule image, prise
//!    à l'échéance (1/60 s). Tant qu'on ne l'a pas prise, le compositeur
//!    n'en fabrique pas d'autre : un écran à 240 Hz ne coûte pas quatre
//!    copies pour une gardée, et ce sur tous les Windows (l'intervalle
//!    minimal de la capture n'existe que depuis Windows 11 24H2).
//! 3. **La conversion par le processeur vidéo de Direct3D**
//!    (`VideoProcessorBlt`) : BGRA → NV12 BT.709, réduction comprise, en un
//!    seul passage sur la carte, dans une texture qui ne change pas de
//!    taille — une fenêtre qui change de taille est cadrée dedans, le flux
//!    reste d'un seul tenant.
//! 4. **NVENC sur cette texture**, enregistrée une fois, réglé sans aucune
//!    option qui passe par CUDA : le moteur d'encodage seul, indépendant des
//!    cœurs qui dessinent le jeu.
//!
//! Il ne remonte vers le processeur que le flux H.264 : ~25 ko par image.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context};
use windows::core::{Interface, BOOL, HSTRING};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709, DXGI_FORMAT,
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, MonitorFromPoint, MonitorFromWindow, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST,
    MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, FindWindowW, GetWindowTextW, IsWindowVisible};

use crate::nvenc::Nvenc;
use crate::{journal, CaptureSource, ConfigClip, EncodedFrame, FrameEmit, StageStats};

const MARCHE: u8 = 0;
const SOURCE_FERMEE: u8 = 1;
const EN_PANNE: u8 = 2;

/// La chaîne des clips sur la carte, tant que la poignée vit : chaque image
/// encodée part vers `emit`, horodatée depuis `origine`.
pub struct ClipGpu {
    arret: Arc<AtomicBool>,
    etat: Arc<AtomicU8>,
    fil: Option<std::thread::JoinHandle<()>>,
}

impl ClipGpu {
    /// Démarre la chaîne sur son fil. Rend l'erreur si elle ne peut pas
    /// s'ouvrir (pas de carte NVIDIA, capture refusée, NVENC absent…) :
    /// l'appelant repasse alors par le chemin du processeur.
    pub fn demarrer(
        config: ConfigClip,
        stats: Arc<StageStats>,
        emit: FrameEmit,
        origine: Instant,
    ) -> anyhow::Result<Self> {
        stats.mark_started();
        let arret = Arc::new(AtomicBool::new(false));
        let etat = Arc::new(AtomicU8::new(MARCHE));
        let (pret_tx, pret_rx) = mpsc::channel::<anyhow::Result<()>>();
        let fil = {
            let (arret, etat) = (arret.clone(), etat.clone());
            std::thread::Builder::new()
                .name("clips-carte".into())
                .spawn(move || boucle(config, stats, emit, origine, arret, etat, pret_tx))
                .context("fil de la chaîne des clips")?
        };
        match pret_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(Self { arret, etat, fil: Some(fil) }),
            Ok(Err(e)) => {
                let _ = fil.join();
                Err(e)
            }
            Err(_) => {
                arret.store(true, Ordering::Relaxed);
                fil.thread().unpark();
                Err(anyhow!("la chaîne des clips ne répond pas à l'ouverture"))
            }
        }
    }

    /// La source s'est évanouie (fenêtre fermée) : plus rien ne viendra.
    pub fn source_fermee(&self) -> bool {
        self.etat.load(Ordering::Relaxed) == SOURCE_FERMEE
    }

    /// La chaîne a renoncé (carte perdue plusieurs fois de suite…) ; la
    /// raison est dans l'avis des statistiques.
    pub fn en_panne(&self) -> bool {
        self.etat.load(Ordering::Relaxed) == EN_PANNE
    }

    pub fn arreter(mut self) {
        self.fermer();
    }

    fn fermer(&mut self) {
        self.arret.store(true, Ordering::Relaxed);
        if let Some(fil) = self.fil.take() {
            fil.thread().unpark();
            let _ = fil.join();
        }
    }
}

impl Drop for ClipGpu {
    fn drop(&mut self) {
        self.fermer();
    }
}

/// Le fil : ouvre la chaîne, dit à `demarrer` si c'est bon, puis prend une
/// image à chaque échéance jusqu'à l'arrêt.
fn boucle(
    config: ConfigClip,
    stats: Arc<StageStats>,
    emit: FrameEmit,
    origine: Instant,
    arret: Arc<AtomicBool>,
    etat: Arc<AtomicU8>,
    pret: mpsc::Sender<anyhow::Result<()>>,
) {
    // Windows.Graphics.Capture vit dans l'appartement multithread.
    let _ro = Appartement::entrer();
    let reveil = std::thread::current();
    let mut chaine = match Chaine::ouvrir(&config, reveil.clone()) {
        Ok(c) => {
            let _ = pret.send(Ok(()));
            c
        }
        Err(e) => {
            let _ = pret.send(Err(e));
            return;
        }
    };
    stats.materiel.store(true, Ordering::Relaxed);
    stats.set_dims(chaine.sortie.0, chaine.sortie.1);
    journal(format!(
        "clips : chaîne tout-GPU sur {} — {}x{} à {} i/s, {} kbit/s, rien ne passe par le processeur",
        chaine.appareil.carte,
        chaine.sortie.0,
        chaine.sortie.1,
        config.fps,
        config.debit_bps / 1000
    ));
    let horloge = Horloge::new(origine);
    let intervalle = Duration::from_micros(1_000_000 / u64::from(config.fps.clamp(1, 120)));
    let mut prochaine = Instant::now();
    // Les pannes récentes : au-delà de trois en une minute, on renonce.
    let mut pannes: Vec<Instant> = Vec::new();
    while !arret.load(Ordering::Relaxed) {
        if chaine.capture.fermee.load(Ordering::Relaxed) {
            journal("clips : la source filmée s'est fermée");
            etat.store(SOURCE_FERMEE, Ordering::Relaxed);
            return;
        }
        let maintenant = Instant::now();
        if maintenant < prochaine {
            // Jusqu'à l'échéance, un sommeil précis (la minuterie haute
            // résolution de Windows, que `sleep` emploie) : `park_timeout`
            // suit l'horloge du système, 15,6 ms par défaut, et ferait
            // sauter une image sur trois. Une image arrivée entre-temps
            // attend dans la réserve — c'est voulu.
            std::thread::sleep(prochaine - maintenant);
            continue;
        }
        match chaine.une_image(&stats, &horloge, origine) {
            Ok(Some(image)) => {
                // L'échéance suivante : un intervalle après celle-ci, sans
                // rattraper un retard (pas de rafale après un écran figé).
                prochaine = (prochaine + intervalle).max(maintenant + intervalle.mul_f32(0.75));
                emit(image);
            }
            // Rien de neuf (écran immobile, fenêtre réduite) : on attend
            // la prochaine image, que le compositeur signalera.
            Ok(None) => std::thread::park_timeout(Duration::from_millis(100)),
            Err(e) => {
                journal(format!("clips : la chaîne tout-GPU a flanché ({e:#})"));
                pannes.retain(|t| t.elapsed() < Duration::from_secs(60));
                pannes.push(Instant::now());
                if pannes.len() > 3 {
                    stats.poser_avis(format!("la chaîne des clips sur la carte a renoncé : {e:#}"));
                    etat.store(EN_PANNE, Ordering::Relaxed);
                    return;
                }
                // Tout est reconstruit (device compris : une carte qui a
                // redémarré invalide tout ce qui vivait dessus).
                drop(chaine);
                std::thread::park_timeout(Duration::from_millis(500));
                chaine = match Chaine::ouvrir(&config, reveil.clone()) {
                    Ok(c) => c,
                    Err(e) => {
                        stats.poser_avis(format!("la chaîne des clips sur la carte ne repart pas : {e:#}"));
                        etat.store(EN_PANNE, Ordering::Relaxed);
                        return;
                    }
                };
                prochaine = Instant::now();
            }
        }
    }
}

/// `RoInitialize` pour la vie du fil.
struct Appartement(bool);

impl Appartement {
    fn entrer() -> Self {
        Self(unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for Appartement {
    fn drop(&mut self) {
        if self.0 {
            unsafe { RoUninitialize() };
        }
    }
}

/// Les instants de la capture (horloge des performances, en centaines de
/// nanosecondes) rapportés à l'origine des horodatages, en µs.
struct Horloge {
    origine_100ns: i64,
}

impl Horloge {
    fn new(origine: Instant) -> Self {
        let (mut qpc, mut freq) = (0i64, 1i64);
        unsafe {
            let _ = QueryPerformanceCounter(&mut qpc);
            let _ = QueryPerformanceFrequency(&mut freq);
        }
        let maintenant = (i128::from(qpc) * 10_000_000 / i128::from(freq.max(1))) as i64;
        let ecoule = (origine.elapsed().as_nanos() / 100) as i64;
        Self { origine_100ns: maintenant - ecoule }
    }

    /// L'horodatage d'une image composée à `systeme` (100 ns), s'il est
    /// plausible — sinon l'instant présent.
    fn pts_us(&self, systeme: i64, origine: Instant) -> u64 {
        let present = origine.elapsed().as_micros() as u64;
        let d = systeme - self.origine_100ns;
        if d <= 0 {
            return present;
        }
        let pts = (d / 10) as u64;
        // Une image « du futur », ou vieille de plus d'une seconde : pas la
        // même horloge, on se fie à l'arrivée.
        if pts > present + 5_000 || pts + 1_000_000 < present {
            present
        } else {
            pts
        }
    }
}

/// Le device de la chaîne : sur la carte NVIDIA, processeur vidéo et BGRA
/// compris, protégé pour plusieurs fils (la capture y alloue ses surfaces
/// depuis les siens).
struct Appareil {
    device: ID3D11Device,
    contexte: ID3D11DeviceContext,
    video: ID3D11VideoDevice,
    video_ctx: ID3D11VideoContext,
    winrt: IDirect3DDevice,
    carte: String,
}

impl Appareil {
    fn nvidia() -> anyhow::Result<Self> {
        let (device, carte) = crate::nvenc::device_nvidia_avec(
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
        )?;
        unsafe {
            let contexte = device.GetImmediateContext().context("contexte Direct3D 11")?;
            if let Ok(mt) = contexte.cast::<ID3D11Multithread>() {
                let _ = mt.SetMultithreadProtected(true);
            }
            let video: ID3D11VideoDevice = device.cast().context("processeur vidéo Direct3D absent")?;
            let video_ctx: ID3D11VideoContext = contexte.cast().context("contexte vidéo Direct3D absent")?;
            let dxgi: IDXGIDevice = device.cast().context("device DXGI")?;
            let winrt: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)
                .context("device WinRT de la capture")?
                .cast()
                .context("device WinRT de la capture")?;
            Ok(Self { device, contexte, video, video_ctx, winrt, carte })
        }
    }
}

/// Windows.Graphics.Capture sur le device de la chaîne : une réserve d'une
/// image, prise à la demande (voir le point 2 en tête du module).
struct Capture {
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    /// La taille des surfaces de la réserve.
    taille: SizeInt32,
    fermee: Arc<AtomicBool>,
    jeton_image: i64,
    jeton_fermee: i64,
}

const FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;

impl Capture {
    /// La capture de `item` sur le device `winrt` — pas forcément celui de
    /// la carte qui affiche : sur un portable, l'écran est sur la puce
    /// Intel et la chaîne sur la carte NVIDIA, Windows fait passer chaque
    /// image de l'une à l'autre.
    fn ouvrir(
        winrt: &IDirect3DDevice,
        item: GraphicsCaptureItem,
        curseur: bool,
        fps: u32,
        reveil: std::thread::Thread,
    ) -> anyhow::Result<Self> {
        let taille = item.Size().context("taille de la source")?;
        if taille.Width <= 0 || taille.Height <= 0 {
            bail!("la source n'a pas de taille (fenêtre réduite ?)");
        }
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(winrt, FORMAT, 1, taille)
            .context("réserve d'images de la capture")?;
        let fermee = Arc::new(AtomicBool::new(false));
        // Une image arrivée réveille le fil (sans rien faire d'autre ici :
        // c'est lui qui la prendra, à son échéance).
        let r = reveil.clone();
        let jeton_image = pool.FrameArrived(&TypedEventHandler::<Direct3D11CaptureFramePool, windows::core::IInspectable>::new(
            move |_, _| {
                r.unpark();
                Ok(())
            },
        ))?;
        let (f, r) = (fermee.clone(), reveil);
        let jeton_fermee = item.Closed(&TypedEventHandler::<GraphicsCaptureItem, windows::core::IInspectable>::new(
            move |_, _| {
                f.store(true, Ordering::Relaxed);
                r.unpark();
                Ok(())
            },
        ))?;
        let session = pool.CreateCaptureSession(&item).context("session de capture")?;
        // Ce que ce Windows sait faire ; le reste garde le défaut (bordure
        // jaune avant Windows 11, pas d'intervalle minimal avant 24H2 —
        // la réserve d'une image tient la cadence de toute façon).
        let mut refus = Vec::new();
        if session.SetIsCursorCaptureEnabled(curseur).is_err() {
            refus.push("curseur");
        }
        if session.SetIsBorderRequired(false).is_err() {
            refus.push("sans bordure");
        }
        let pas = windows::Foundation::TimeSpan { Duration: 10_000_000 / i64::from(fps.clamp(1, 120)) };
        if session.SetMinUpdateInterval(pas).is_err() {
            refus.push("intervalle minimal");
        }
        if !refus.is_empty() {
            journal(format!("clips : ce Windows ignore les options de capture : {}", refus.join(", ")));
        }
        session.StartCapture().context("démarrage de la capture")?;
        Ok(Self { item, pool, session, taille, fermee, jeton_image, jeton_fermee })
    }

    /// La dernière image arrivée, s'il y en a une ; les plus anciennes sont
    /// rendues au passage.
    fn prendre(&self) -> anyhow::Result<Option<Direct3D11CaptureFrame>> {
        let mut derniere: Option<Direct3D11CaptureFrame> = None;
        loop {
            match self.pool.TryGetNextFrame() {
                Ok(image) => {
                    if let Some(vieille) = derniere.replace(image) {
                        let _ = vieille.Close();
                    }
                }
                // Pas d'image : l'API rend un objet nul, que windows-rs
                // traduit en « erreur » de code S_OK.
                Err(e) if e.code().is_ok() => break,
                Err(e) => return Err(anyhow!(e).context("image de la capture")),
            }
        }
        Ok(derniere)
    }

    /// La source a changé de taille : la réserve suit.
    fn recreer(&mut self, winrt: &IDirect3DDevice, taille: SizeInt32) -> anyhow::Result<()> {
        self.pool.Recreate(winrt, FORMAT, 1, taille).context("réserve de la capture")?;
        self.taille = taille;
        Ok(())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.pool.RemoveFrameArrived(self.jeton_image);
        let _ = self.item.RemoveClosed(self.jeton_fermee);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

/// L'élément de capture de la source, et l'écran sur lequel elle se
/// trouve — c'est lui qui donne la taille de l'image enregistrée.
fn element(source: &CaptureSource) -> anyhow::Result<(GraphicsCaptureItem, GraphicsCaptureItem)> {
    if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
        bail!("la capture d'écran de Windows n'est pas disponible (Windows 10 1903 ou plus récent requis)");
    }
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
        .context("fabrique de la capture")?;
    unsafe {
        match source {
            CaptureSource::Monitor(i) => {
                let ecran = moniteur(*i)?;
                let item: GraphicsCaptureItem = interop.CreateForMonitor(ecran).context("capture de l'écran")?;
                Ok((item.clone(), item))
            }
            CaptureSource::Window(titre) => {
                let fenetre = fenetre_par_titre(titre)
                    .ok_or_else(|| anyhow!("fenêtre « {titre} » introuvable — fermée ?"))?;
                let item: GraphicsCaptureItem =
                    interop.CreateForWindow(fenetre).context("capture de la fenêtre")?;
                let ecran = MonitorFromWindow(fenetre, MONITOR_DEFAULTTONEAREST);
                let item_ecran: GraphicsCaptureItem =
                    interop.CreateForMonitor(ecran).context("écran de la fenêtre")?;
                Ok((item, item_ecran))
            }
        }
    }
}

/// Un écran par son rang d'énumération (1 = premier), 0 = le principal —
/// la même convention que la capture du partage d'écran.
fn moniteur(rang: usize) -> anyhow::Result<HMONITOR> {
    if rang == 0 {
        return Ok(unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) });
    }
    unsafe extern "system" fn rappel(m: HMONITOR, _: HDC, _: *mut RECT, l: LPARAM) -> BOOL {
        let v = unsafe { &mut *(l.0 as *mut Vec<HMONITOR>) };
        v.push(m);
        true.into()
    }
    let mut ecrans: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(rappel), LPARAM(&mut ecrans as *mut _ as isize));
    }
    ecrans.get(rang - 1).copied().ok_or_else(|| anyhow!("écran {rang} introuvable"))
}

/// Une fenêtre visible par son titre : exact d'abord, puis aux espaces de
/// bord près — l'inventaire des fenêtres rend les titres nettoyés, et
/// celui de VALORANT finit par deux espaces.
fn fenetre_par_titre(titre: &str) -> Option<HWND> {
    if let Ok(h) = unsafe { FindWindowW(None, &HSTRING::from(titre)) } {
        if !h.is_invalid() {
            return Some(h);
        }
    }
    struct Recherche {
        titre: String,
        trouvee: Option<HWND>,
    }
    unsafe extern "system" fn rappel(h: HWND, l: LPARAM) -> BOOL {
        let r = unsafe { &mut *(l.0 as *mut Recherche) };
        if !unsafe { IsWindowVisible(h) }.as_bool() {
            return true.into();
        }
        let mut tampon = [0u16; 512];
        let n = unsafe { GetWindowTextW(h, &mut tampon) };
        if n > 0 && String::from_utf16_lossy(&tampon[..n as usize]).trim() == r.titre {
            r.trouvee = Some(h);
            return false.into();
        }
        true.into()
    }
    let mut r = Recherche { titre: titre.trim().to_string(), trouvee: None };
    unsafe {
        let _ = EnumWindows(Some(rappel), LPARAM(&mut r as *mut _ as isize));
    }
    r.trouvee
}

/// La conversion BGRA → NV12 (et la réduction) par le processeur vidéo de
/// Direct3D, vers la texture que NVENC lit.
struct Convertisseur {
    /// La taille des surfaces d'entrée pour laquelle il a été bâti.
    entree: (u32, u32),
    enumerateur: ID3D11VideoProcessorEnumerator,
    processeur: ID3D11VideoProcessor,
    vue_sortie: ID3D11VideoProcessorOutputView,
    /// Les vues d'entrée, par surface de la capture (la réserve en
    /// recycle une ou deux) — la texture est gardée avec sa vue, pour que
    /// son adresse ne puisse pas resservir à une autre.
    vues: Vec<(ID3D11Texture2D, ID3D11VideoProcessorInputView)>,
    /// Le pilote refuse une vue sur les surfaces de la capture : on passe
    /// alors par une copie (sur la carte) dans une texture à nous.
    copie: Option<(ID3D11Texture2D, ID3D11VideoProcessorInputView)>,
    par_copie: bool,
    /// Le cadre de la dernière image, pour ne le redire que s'il change.
    cadre: Option<(u32, u32)>,
}

impl Convertisseur {
    fn new(appareil: &Appareil, entree: (u32, u32), sortie: (u32, u32), nv12: &ID3D11Texture2D, fps: u32) -> anyhow::Result<Self> {
        unsafe {
            let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL { Numerator: fps, Denominator: 1 },
                InputWidth: entree.0,
                InputHeight: entree.1,
                OutputFrameRate: DXGI_RATIONAL { Numerator: fps, Denominator: 1 },
                OutputWidth: sortie.0,
                OutputHeight: sortie.1,
                Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
            };
            let enumerateur = appareil
                .video
                .CreateVideoProcessorEnumerator(&desc)
                .context("processeur vidéo : description refusée")?;
            let accepte = |format: DXGI_FORMAT, sens: D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT| {
                enumerateur.CheckVideoProcessorFormat(format).is_ok_and(|f| f & sens.0 as u32 != 0)
            };
            if !accepte(DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT) {
                bail!("le processeur vidéo de la carte ne lit pas le BGRA");
            }
            if !accepte(DXGI_FORMAT_NV12, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT) {
                bail!("le processeur vidéo de la carte n'écrit pas le NV12");
            }
            let processeur = appareil
                .video
                .CreateVideoProcessor(&enumerateur, 0)
                .context("processeur vidéo")?;
            let ctx = &appareil.video_ctx;
            // Les couleurs : un écran en RGB pleine plage, un flux vidéo en
            // BT.709 plage limitée — ce que NVENC déclare, et ce que les
            // lecteurs attendent d'une image HD.
            if let Ok(ctx1) = ctx.cast::<ID3D11VideoContext1>() {
                ctx1.VideoProcessorSetStreamColorSpace1(&processeur, 0, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
                ctx1.VideoProcessorSetOutputColorSpace1(&processeur, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
            } else {
                // Champs de bits : Usage:1, RGB_Range:1 (0 = pleine),
                // YCbCr_Matrix:1 (1 = BT.709), xvYCC:1, Nominal_Range:2
                // (1 = 16-235).
                let entree_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 };
                let sortie_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: (1 << 2) | (1 << 4) };
                ctx.VideoProcessorSetStreamColorSpace(&processeur, 0, &entree_cs);
                ctx.VideoProcessorSetOutputColorSpace(&processeur, &sortie_cs);
            }
            ctx.VideoProcessorSetStreamFrameFormat(&processeur, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            // Aucune « amélioration » automatique du pilote : l'image telle
            // qu'elle est.
            ctx.VideoProcessorSetStreamAutoProcessingMode(&processeur, 0, false);
            ctx.VideoProcessorSetOutputTargetRect(
                &processeur,
                true,
                Some(&RECT { left: 0, top: 0, right: sortie.0 as i32, bottom: sortie.1 as i32 }),
            );
            // Le fond (bandes d'une fenêtre cadrée) : noir, en YCbCr limité.
            let noir = D3D11_VIDEO_COLOR {
                Anonymous: D3D11_VIDEO_COLOR_0 {
                    YCbCr: D3D11_VIDEO_COLOR_YCbCrA { Y: 16.0 / 255.0, Cb: 0.5, Cr: 0.5, A: 1.0 },
                },
            };
            ctx.VideoProcessorSetOutputBackgroundColor(&processeur, true, &noir);
            let desc_sortie = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
            };
            let mut vue_sortie = None;
            appareil
                .video
                .CreateVideoProcessorOutputView(nv12, &enumerateur, &desc_sortie, Some(&mut vue_sortie))
                .context("vue de sortie du processeur vidéo")?;
            Ok(Self {
                entree,
                enumerateur,
                processeur,
                vue_sortie: vue_sortie.context("vue de sortie absente")?,
                vues: Vec::new(),
                copie: None,
                par_copie: false,
                cadre: None,
            })
        }
    }

    fn vue_entree(&self, appareil: &Appareil, texture: &ID3D11Texture2D) -> anyhow::Result<ID3D11VideoProcessorInputView> {
        let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
            },
        };
        let mut vue = None;
        unsafe { appareil.video.CreateVideoProcessorInputView(texture, &self.enumerateur, &desc, Some(&mut vue)) }
            .context("vue d'entrée du processeur vidéo")?;
        vue.context("vue d'entrée absente")
    }

    /// La vue d'entrée pour cette surface : directe si le pilote l'accepte,
    /// sinon par une copie sur la carte dans une texture à nous.
    fn entree_pour(
        &mut self,
        appareil: &Appareil,
        surface: &ID3D11Texture2D,
    ) -> anyhow::Result<ID3D11VideoProcessorInputView> {
        if !self.par_copie {
            if let Some((_, v)) = self.vues.iter().find(|(t, _)| t.as_raw() == surface.as_raw()) {
                return Ok(v.clone());
            }
            match self.vue_entree(appareil, surface) {
                Ok(v) => {
                    if self.vues.len() >= 4 {
                        self.vues.remove(0);
                    }
                    self.vues.push((surface.clone(), v.clone()));
                    return Ok(v);
                }
                Err(e) => {
                    journal(format!("clips : vue directe sur la capture refusée ({e:#}) — copie sur la carte"));
                    self.par_copie = true;
                    self.vues.clear();
                }
            }
        }
        if self.copie.is_none() {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: self.entree.0,
                Height: self.entree.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut t = None;
            unsafe { appareil.device.CreateTexture2D(&desc, None, Some(&mut t)) }.context("texture de copie")?;
            let t = t.context("texture de copie absente")?;
            let v = self.vue_entree(appareil, &t)?;
            self.copie = Some((t, v));
        }
        let (t, v) = self.copie.as_ref().context("texture de copie")?;
        unsafe { appareil.contexte.CopyResource(t, surface) };
        Ok(v.clone())
    }

    /// Une image : la surface de la capture (dont `contenu` est la partie
    /// utile) cadrée dans la sortie, convertie en NV12.
    fn convertir(
        &mut self,
        appareil: &Appareil,
        surface: &ID3D11Texture2D,
        contenu: (u32, u32),
        sortie: (u32, u32),
    ) -> anyhow::Result<()> {
        let vue = self.entree_pour(appareil, surface)?;
        let ctx = &appareil.video_ctx;
        unsafe {
            if self.cadre != Some(contenu) {
                let (cw, ch) = (contenu.0.min(self.entree.0), contenu.1.min(self.entree.1));
                ctx.VideoProcessorSetStreamSourceRect(
                    &self.processeur,
                    0,
                    true,
                    Some(&RECT { left: 0, top: 0, right: cw as i32, bottom: ch as i32 }),
                );
                let dest = cadrer((cw, ch), sortie);
                ctx.VideoProcessorSetStreamDestRect(&self.processeur, 0, true, Some(&dest));
                self.cadre = Some(contenu);
            }
            let flux = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                OutputIndex: 0,
                InputFrameOrField: 0,
                PastFrames: 0,
                FutureFrames: 0,
                ppPastSurfaces: std::ptr::null_mut(),
                pInputSurface: std::mem::ManuallyDrop::new(Some(vue)),
                ppFutureSurfaces: std::ptr::null_mut(),
                ppPastSurfacesRight: std::ptr::null_mut(),
                pInputSurfaceRight: std::mem::ManuallyDrop::new(None),
                ppFutureSurfacesRight: std::ptr::null_mut(),
            };
            let mut flux = [flux];
            let resultat = ctx.VideoProcessorBlt(&self.processeur, &self.vue_sortie, 0, &flux);
            // La vue prêtée au flux : rendue ici, sinon sa référence fuit.
            std::mem::ManuallyDrop::drop(&mut flux[0].pInputSurface);
            resultat.context("conversion par le processeur vidéo")?;
        }
        Ok(())
    }
}

/// Le rectangle, dans une sortie `sortie`, où tient `contenu` sans être
/// déformé : centré, aux dimensions paires.
fn cadrer(contenu: (u32, u32), sortie: (u32, u32)) -> RECT {
    let (cw, ch) = (u64::from(contenu.0.max(1)), u64::from(contenu.1.max(1)));
    let (ow, oh) = (u64::from(sortie.0), u64::from(sortie.1));
    // La plus grande taille au même rapport qui tienne dans la sortie.
    let (w, h) = if cw * oh >= ch * ow { (ow, (ch * ow / cw).max(2)) } else { ((cw * oh / ch).max(2), oh) };
    let (w, h) = (w & !1, h & !1);
    let x = ((ow - w) / 2) & !1;
    let y = ((oh - h) / 2) & !1;
    RECT { left: x as i32, top: y as i32, right: (x + w) as i32, bottom: (y + h) as i32 }
}

/// Tout ce qui vit sur la carte pour une source : device, capture,
/// conversion, texture NV12 et NVENC.
struct Chaine {
    // L'ordre des champs est celui de la destruction : NVENC (qui tient la
    // texture enregistrée) avant la texture, la capture avant le device.
    nvenc: Nvenc,
    conv: Option<Convertisseur>,
    nv12: ID3D11Texture2D,
    capture: Capture,
    /// La taille de l'image enregistrée : celle de l'écran de la source,
    /// réduite au plafond — fixe pour toute la chaîne.
    sortie: (u32, u32),
    fps: u32,
    appareil: Appareil,
    /// La prochaine image doit être une trame clé.
    idr: bool,
    /// L'horodatage de la dernière image émise : les suivants montent
    /// toujours (l'écrivain du MP4 jette ce qui recule).
    dernier_pts: Option<u64>,
}

impl Chaine {
    fn ouvrir(config: &ConfigClip, reveil: std::thread::Thread) -> anyhow::Result<Self> {
        let appareil = Appareil::nvidia()?;
        let (item, item_ecran) = element(&config.source)?;
        let ecran = item_ecran.Size().context("taille de l'écran")?;
        let sortie = crate::scale::target_dims(
            ecran.Width.max(2) as u32,
            ecran.Height.max(2) as u32,
            config.hauteur_max,
        );
        let nv12 = unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: sortie.0,
                Height: sortie.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut t = None;
            appareil.device.CreateTexture2D(&desc, None, Some(&mut t)).context("texture NV12 de l'encodeur")?;
            t.context("texture NV12 absente")?
        };
        let nvenc = Nvenc::sur_texture(
            &appareil.device,
            &appareil.carte,
            &nv12,
            sortie.0,
            sortie.1,
            config.debit_bps,
            config.fps,
            config.gop_s,
        )?;
        let capture = Capture::ouvrir(&appareil.winrt, item, config.curseur, config.fps, reveil)?;
        Ok(Self { nvenc, conv: None, nv12, capture, sortie, fps: config.fps, appareil, idr: true, dernier_pts: None })
    }

    /// Prend l'image arrivée, la convertit et l'encode. `None` : rien de
    /// neuf depuis la dernière.
    fn une_image(
        &mut self,
        stats: &StageStats,
        horloge: &Horloge,
        origine: Instant,
    ) -> anyhow::Result<Option<EncodedFrame>> {
        let Some(image) = self.capture.prendre()? else { return Ok(None) };
        let contenu = image.ContentSize().context("taille de l'image")?;
        if contenu.Width <= 0 || contenu.Height <= 0 {
            let _ = image.Close();
            return Ok(None);
        }
        if contenu.Width != self.capture.taille.Width || contenu.Height != self.capture.taille.Height {
            // La source a changé de taille : la réserve suit, et l'image
            // (encore à l'ancienne taille) est laissée.
            let _ = image.Close();
            self.capture.recreer(&self.appareil.winrt, contenu)?;
            return Ok(None);
        }
        stats.captured.fetch_add(1, Ordering::Relaxed);
        let compose = image.SystemRelativeTime().map(|t| t.Duration).unwrap_or(0);
        let mut pts_us = horloge.pts_us(compose, origine);
        if self.dernier_pts.is_none() {
            // Une fois par chaîne, pour les diagnostics : l'image date de
            // sa composition, pas de son arrivée chez nous.
            let present = origine.elapsed().as_micros() as u64;
            journal(format!(
                "clips : première image composée {:.1} ms avant d'être prise",
                present.saturating_sub(pts_us) as f32 / 1000.0
            ));
        }
        if let Some(d) = self.dernier_pts {
            pts_us = pts_us.max(d + 1);
        }
        self.dernier_pts = Some(pts_us);

        // 1. La conversion, soumise à la carte.
        let t0 = Instant::now();
        let surface: ID3D11Texture2D = unsafe {
            image
                .Surface()
                .context("surface de l'image")?
                .cast::<IDirect3DDxgiInterfaceAccess>()
                .context("accès DXGI de la surface")?
                .GetInterface()
                .context("texture de la surface")?
        };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { surface.GetDesc(&mut desc) };
        let entree = (desc.Width, desc.Height);
        if self.conv.as_ref().is_none_or(|c| c.entree != entree) {
            self.conv = Some(Convertisseur::new(&self.appareil, entree, self.sortie, &self.nv12, self.fps)?);
        }
        let contenu = (contenu.Width as u32, contenu.Height as u32);
        if let Some(conv) = self.conv.as_mut() {
            conv.convertir(&self.appareil, &surface, contenu, self.sortie)?;
        }
        // Soumise avant de rendre la surface à la capture : le compositeur
        // n'y récrira qu'après notre lecture.
        unsafe { self.appareil.contexte.Flush() };
        drop(surface);
        let _ = image.Close();
        stats.convert_ms.record(t0.elapsed().as_secs_f32() * 1000.0);
        stats.converted.fetch_add(1, Ordering::Relaxed);

        // 2. L'encodage, sur la même texture ; seul le flux remonte.
        let t1 = Instant::now();
        let force = std::mem::take(&mut self.idr);
        let paquet = match self.nvenc.encoder_texture(force)? {
            Some(p) => p,
            None => {
                self.idr |= force;
                stats.enc_skipped.fetch_add(1, Ordering::Relaxed);
                return Ok(None);
            }
        };
        stats.encode_ms.record(t1.elapsed().as_secs_f32() * 1000.0);
        stats.encoded.fetch_add(1, Ordering::Relaxed);
        stats.encoded_bytes.fetch_add(paquet.data.len() as u64, Ordering::Relaxed);
        if paquet.idr {
            stats.keyframes.fetch_add(1, Ordering::Relaxed);
        }
        Ok(Some(EncodedFrame {
            data: paquet.data,
            idr: paquet.idr,
            pts_us,
            width: self.sortie.0 as u16,
            height: self.sortie.1 as u16,
            basse: false,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn une_fenetre_se_cadre_sans_deformation() {
        // Même rapport : toute la sortie.
        let r = cadrer((2560, 1440), (1920, 1080));
        assert_eq!((r.left, r.top, r.right, r.bottom), (0, 0, 1920, 1080));
        // Plus étroite (4:3) : bandes à gauche et à droite.
        let r = cadrer((1024, 768), (1920, 1080));
        assert_eq!((r.top, r.bottom), (0, 1080));
        assert_eq!(r.right - r.left, 1440);
        assert_eq!(r.left, 240);
        // Plus large (21:9) : bandes en haut et en bas, dimensions paires.
        let r = cadrer((3440, 1440), (1920, 1080));
        assert_eq!((r.left, r.right), (0, 1920));
        assert_eq!((r.bottom - r.top) % 2, 0);
        assert!(r.top > 0 && r.bottom < 1080);
    }

    #[test]
    fn l_horloge_de_la_capture_se_rapporte_a_l_origine() {
        let origine = Instant::now() - Duration::from_secs(2);
        let h = Horloge::new(origine);
        let (mut qpc, mut freq) = (0i64, 1i64);
        unsafe {
            let _ = QueryPerformanceCounter(&mut qpc);
            let _ = QueryPerformanceFrequency(&mut freq);
        }
        let maintenant = (i128::from(qpc) * 10_000_000 / i128::from(freq)) as i64;
        // Une image composée il y a 10 ms : ~1,99 s après l'origine.
        let pts = h.pts_us(maintenant - 100_000, origine);
        assert!((1_985_000..=1_995_000).contains(&pts), "{pts}");
        // Une horloge étrangère (zéro, ou l'an prochain) : l'arrivée.
        let present = origine.elapsed().as_micros() as u64;
        assert!(h.pts_us(0, origine) >= present);
        assert!(h.pts_us(i64::MAX / 2, origine) >= present);
    }

    /// Le cas des portables — l'écran câblé sur la puce Intel, la chaîne
    /// sur la carte NVIDIA — joué à l'envers sur une machine qui a les
    /// deux : la capture sur une carte qui n'affiche rien. Les images
    /// doivent traverser d'une carte à l'autre, et arriver pleines, pas
    /// noires. Ignoré par défaut (il faut deux cartes, dont une sans
    /// écran) : `cargo test -p ki-video d_une_carte_a_l_autre -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn la_capture_passe_d_une_carte_a_l_autre() {
        use windows::Win32::Foundation::HMODULE;
        use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
        use windows::Win32::Graphics::Dxgi::{
            CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
        };

        let _ro = Appartement::entrer();
        // Une carte matérielle sans écran branché.
        let trouvee = unsafe {
            let fabrique: IDXGIFactory1 = CreateDXGIFactory1().unwrap();
            let mut i = 0;
            let mut trouvee = None;
            while let Ok(a) = fabrique.EnumAdapters1(i) {
                i += 1;
                let d = a.GetDesc1().unwrap();
                if d.Flags & (DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0 || a.EnumOutputs(0).is_ok() {
                    continue;
                }
                let fin = d.Description.iter().position(|&c| c == 0).unwrap_or(128);
                trouvee = Some((a, String::from_utf16_lossy(&d.Description[..fin])));
                break;
            }
            trouvee
        };
        let Some((adaptateur, nom)) = trouvee else {
            eprintln!("pas de carte sans écran sur cette machine : test sauté");
            return;
        };
        let base: IDXGIAdapter = adaptateur.cast().unwrap();
        let (mut device, mut contexte): (Option<ID3D11Device>, Option<ID3D11DeviceContext>) = (None, None);
        unsafe {
            D3D11CreateDevice(
                &base,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut contexte),
            )
            .unwrap();
        }
        let (device, contexte) = (device.unwrap(), contexte.unwrap());
        let winrt: IDirect3DDevice = unsafe {
            CreateDirect3D11DeviceFromDXGIDevice(&device.cast::<IDXGIDevice>().unwrap()).unwrap().cast().unwrap()
        };
        let (item, _) = element(&CaptureSource::Monitor(0)).unwrap();
        let capture = Capture::ouvrir(&winrt, item, true, 60, std::thread::current()).unwrap();
        let debut = Instant::now();
        let image = loop {
            if let Some(i) = capture.prendre().unwrap() {
                break i;
            }
            assert!(debut.elapsed() < Duration::from_secs(3), "aucune image n'est arrivée jusqu'à {nom}");
            std::thread::park_timeout(Duration::from_millis(50));
        };
        let surface: ID3D11Texture2D = unsafe {
            image.Surface().unwrap().cast::<IDirect3DDxgiInterfaceAccess>().unwrap().GetInterface().unwrap()
        };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { surface.GetDesc(&mut desc) };
        // Relue sur cette carte-là : combien de pixels ne sont pas noirs ?
        let lecture = D3D11_TEXTURE2D_DESC {
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
            ..desc
        };
        let mut t = None;
        unsafe { device.CreateTexture2D(&lecture, None, Some(&mut t)).unwrap() };
        let t = t.unwrap();
        let (mut allumes, mut total) = (0usize, 0usize);
        unsafe {
            contexte.CopyResource(&t, &surface);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            contexte.Map(&t, 0, D3D11_MAP_READ, 0, Some(&mut m)).unwrap();
            for y in (0..desc.Height as usize).step_by(8) {
                let ligne = std::slice::from_raw_parts(
                    (m.pData as *const u8).add(y * m.RowPitch as usize),
                    desc.Width as usize * 4,
                );
                for px in ligne.as_chunks::<4>().0.iter().step_by(8) {
                    total += 1;
                    if u32::from(px[0]) + u32::from(px[1]) + u32::from(px[2]) > 30 {
                        allumes += 1;
                    }
                }
            }
            contexte.Unmap(&t, 0);
        }
        let _ = image.Close();
        eprintln!(
            "capture reçue sur {nom} (écran sur une autre carte) : {}x{}, {allumes}/{total} pixels allumés, \
             première image en {:.0} ms",
            desc.Width,
            desc.Height,
            debut.elapsed().as_secs_f32() * 1000.0
        );
        assert!(allumes * 20 > total, "image noire : la capture n'a pas traversé");
    }

    /// Le vrai circuit sur cette machine : l'écran principal quelques
    /// secondes par la carte, des trames H.264 qui sortent, une trame clé
    /// en tête. Ignoré par défaut (il faut un écran et une carte NVIDIA) :
    /// `cargo test -p ki-video chaine_tout_gpu -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn chaine_tout_gpu_filme_l_ecran() {
        let stats = Arc::new(StageStats::default());
        let trames = Arc::new(std::sync::Mutex::new(Vec::<(bool, usize, u64, u16, u16)>::new()));
        let emit: FrameEmit = {
            let t = trames.clone();
            Arc::new(move |f: EncodedFrame| t.lock().unwrap().push((f.idr, f.data.len(), f.pts_us, f.width, f.height)))
        };
        let config = ConfigClip {
            source: CaptureSource::Monitor(0),
            hauteur_max: 1080,
            fps: 60,
            debit_bps: 12_000_000,
            curseur: true,
            gop_s: 1,
        };
        let chaine = match ClipGpu::demarrer(config, stats.clone(), emit, Instant::now()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("chaîne tout-GPU indisponible ici : {e:#}");
                return;
            }
        };
        std::thread::sleep(Duration::from_secs(3));
        chaine.arreter();
        let t = trames.lock().unwrap();
        eprintln!("{} trames, {}", t.len(), stats.summary());
        assert!(!t.is_empty(), "aucune trame");
        assert!(t[0].0, "la première trame est une trame clé");
        assert!(t.windows(2).all(|w| w[1].2 > w[0].2), "horodatages croissants");
        assert!(t.iter().all(|x| x.3 == t[0].3 && x.4 == t[0].4), "taille constante");
    }
}
