//! La diffusion sans le processeur : la capture, la conversion et
//! l'encodage restent sur la carte graphique — la chaîne des clips
//! (`clip_gpu.rs`) appliquée au partage d'écran (PLAN-STREAM.md, S4).
//!
//! L'ancienne chaîne (`streamer_pipeline`) rapatriait chaque image vers la
//! mémoire centrale — 20 Mo en 3440×1440, soixante fois par seconde, et
//! pour la lire il fallait attendre que la carte ait fini, derrière le
//! jeu —, la convertissait et la réduisait sur un cœur, la renvoyait à la
//! carte, et l'aperçu redécodait le flux sur un cœur encore. Chez Pandora
//! (GTX 1050 Ti, VALORANT sur un écran de 3440×1440, le 2026-09-27), elle
//! ne tenait ni 60 ni 30 images par seconde, même descendue en 720p. Ici :
//!
//! 1. **Un seul device Direct3D 11**, sur la carte NVIDIA : la capture y
//!    arrive, une image prise à chaque échéance (la réserve d'une image des
//!    clips).
//! 2. **Le processeur vidéo de la carte** convertit la surface capturée en
//!    NV12 BT.601 (ce que le décodeur des spectateurs attend), réduction
//!    comprise, pour la haute — et, quand le serveur la
//!    demande, une seconde fois pour la basse : la même surface, deux
//!    tailles. La taille émise est fixée à l'ouverture ; une fenêtre qui
//!    change de taille est cadrée dedans, sans relancer l'encodeur.
//! 3. **Deux sessions NVENC**, sans option qui passe par CUDA. La haute
//!    que plus personne ne regarde n'est plus encodée du tout.
//! 4. **L'aperçu** : une vignette BGRA réduite par le même processeur,
//!    relue sans jamais attendre la carte (trois textures en rotation,
//!    `DO_NOT_WAIT`), quinze fois par seconde au plus. Il montre l'image
//!    capturée plutôt que le flux décodé : le prix d'un décodage de moins.
//!
//! Si la chaîne ne s'ouvre pas, `StreamerLoop` prend l'ancien chemin ; si
//! elle renonce en route (la carte perdue trois fois en une minute), elle
//! le prend elle-même, sur son fil, sans couper le stream.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::DXGI_ERROR_WAS_STILL_DRAWING;
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;

use crate::clip_gpu::{element, Appareil, Appartement, Capture, Convertisseur, Couleurs, Horloge};
use crate::nvenc::Nvenc;
use crate::{
    journal, qualite_basse, scale, EncodedFrame, FrameEmit, FrameSink, Profil, Qualites, RgbaFrame, StageStats,
    StreamConfig,
};

/// L'aperçu : au plus un par ce laps (15 images par seconde).
const APERCU_PAS: Duration = Duration::from_millis(66);
/// Et dans cette boîte, au rapport de l'image émise.
const APERCU_BOITE: (u32, u32) = (854, 480);

/// La diffusion sur la carte, tant que la poignée vit.
pub(crate) struct DiffusionGpu {
    arret: Arc<AtomicBool>,
    fermee: Arc<AtomicBool>,
    fil: Option<std::thread::JoinHandle<()>>,
}

/// Ce que le fil reçoit : tout ce que la boucle du processeur recevrait,
/// pour pouvoir la prendre à son compte si la carte renonce.
struct Parts {
    config: StreamConfig,
    stats: Arc<StageStats>,
    apercu: FrameSink,
    emit: FrameEmit,
    force_idr: Arc<AtomicBool>,
    origine: Instant,
    qualites: Option<Arc<Qualites>>,
    arret: Arc<AtomicBool>,
    fermee: Arc<AtomicBool>,
}

impl DiffusionGpu {
    /// Démarre la chaîne sur son fil. L'erreur, si elle ne peut pas
    /// s'ouvrir (pas de carte NVIDIA, capture refusée, NVENC absent…) :
    /// l'appelant prend alors le chemin du processeur.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn demarrer(
        stats: Arc<StageStats>,
        apercu: FrameSink,
        emit: FrameEmit,
        config: StreamConfig,
        force_idr: Arc<AtomicBool>,
        origine: Instant,
        qualites: Option<Arc<Qualites>>,
    ) -> anyhow::Result<Self> {
        stats.mark_started();
        let arret = Arc::new(AtomicBool::new(false));
        let fermee = Arc::new(AtomicBool::new(false));
        let (pret_tx, pret_rx) = mpsc::channel::<anyhow::Result<()>>();
        let parts = Parts {
            config,
            stats,
            apercu,
            emit,
            force_idr,
            origine,
            qualites,
            arret: arret.clone(),
            fermee: fermee.clone(),
        };
        let fil = std::thread::Builder::new()
            .name("diffusion-carte".into())
            .spawn(move || boucle(parts, pret_tx))
            .context("fil de la diffusion sur la carte")?;
        match pret_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(Self { arret, fermee, fil: Some(fil) }),
            Ok(Err(e)) => {
                let _ = fil.join();
                Err(e)
            }
            Err(_) => {
                arret.store(true, Ordering::Relaxed);
                fil.thread().unpark();
                Err(anyhow!("la diffusion sur la carte ne répond pas à l'ouverture"))
            }
        }
    }

    /// La source s'est évanouie (fenêtre fermée) : plus rien ne viendra.
    pub(crate) fn source_fermee(&self) -> bool {
        self.fermee.load(Ordering::Relaxed)
    }

    pub(crate) fn arreter(mut self) {
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

impl Drop for DiffusionGpu {
    fn drop(&mut self) {
        self.fermer();
    }
}

/// Le fil : ouvre la chaîne, dit à `demarrer` si c'est bon, puis prend une
/// image à chaque échéance jusqu'à l'arrêt.
fn boucle(p: Parts, pret: mpsc::Sender<anyhow::Result<()>>) {
    // Windows.Graphics.Capture vit dans l'appartement multithread.
    let ro = Appartement::entrer();
    let reveil = std::thread::current();
    let mut chaine = match Chaine::ouvrir(&p.config, reveil.clone()) {
        Ok(c) => {
            let _ = pret.send(Ok(()));
            c
        }
        Err(e) => {
            let _ = pret.send(Err(e));
            return;
        }
    };
    chaine.annoncer(&p.stats, &p.config);
    let horloge = Horloge::new(p.origine);
    let intervalle = Duration::from_micros(1_000_000 / u64::from(p.config.fps.clamp(1, 120)));
    let mut prochaine = Instant::now();
    // Les pannes récentes : au-delà de trois en une minute, on renonce.
    let mut pannes: Vec<Instant> = Vec::new();
    while !p.arret.load(Ordering::Relaxed) {
        if chaine.capture.fermee.load(Ordering::Relaxed) {
            journal("diffusion : la source diffusée s'est fermée");
            p.fermee.store(true, Ordering::Relaxed);
            return;
        }
        let maintenant = Instant::now();
        if maintenant < prochaine {
            // Un sommeil précis jusqu'à l'échéance (voir `clip_gpu`).
            std::thread::sleep(prochaine - maintenant);
            continue;
        }
        match chaine.une_image(&p, &horloge) {
            Ok(true) => {
                // Les échéances passées pendant qu'on travaillait : autant
                // d'images que la capture n'a pas pu donner. C'est ce que le
                // régulateur de l'encodeur regarde (« sautées : capture »).
                let retard = maintenant.saturating_duration_since(prochaine);
                let manquees = (retard.as_micros() / intervalle.as_micros().max(1)) as u64;
                if manquees > 0 {
                    p.stats.skipped.fetch_add(manquees, Ordering::Relaxed);
                }
                prochaine = (prochaine + intervalle).max(maintenant + intervalle.mul_f32(0.75));
            }
            // Rien de neuf (écran immobile, fenêtre réduite) : on attend
            // l'image suivante, que le compositeur signalera — sans compter
            // l'attente comme du retard.
            Ok(false) => {
                std::thread::park_timeout(Duration::from_millis(100));
                prochaine = Instant::now();
            }
            Err(e) => {
                journal(format!("diffusion : la chaîne sur la carte a flanché ({e:#})"));
                pannes.retain(|t| t.elapsed() < Duration::from_secs(60));
                pannes.push(Instant::now());
                // Tout est reconstruit, device compris : une carte qui a
                // redémarré invalide tout ce qui vivait dessus.
                drop(chaine);
                let reprise = if pannes.len() > 3 {
                    Err(anyhow!("trois pannes en une minute"))
                } else {
                    std::thread::park_timeout(Duration::from_millis(500));
                    Chaine::ouvrir(&p.config, reveil.clone())
                };
                match reprise {
                    Ok(c) => {
                        chaine = c;
                        chaine.annoncer(&p.stats, &p.config);
                        prochaine = Instant::now();
                    }
                    Err(e) => {
                        journal(format!("diffusion : la carte renonce ({e:#}) — chemin du processeur, le stream continue"));
                        drop(ro);
                        repli(p);
                        return;
                    }
                }
            }
        }
    }
}

/// La chaîne a renoncé : l'ancien chemin, sur ce fil, jusqu'à l'arrêt.
fn repli(p: Parts) {
    p.stats.materiel.store(false, Ordering::Relaxed);
    p.stats.set_basse_dims(0, 0);
    p.force_idr.store(true, Ordering::Relaxed);
    if let Err(e) = crate::diffuser_par_le_processeur(
        p.stats.clone(),
        p.apercu,
        p.emit,
        p.config,
        p.force_idr,
        p.origine,
        p.qualites,
        p.arret,
        p.fermee,
    ) {
        journal(format!("diffusion : le chemin du processeur ne démarre pas non plus ({e:#})"));
        p.stats.poser_avis(format!("diffusion interrompue : {e:#}"));
    }
}

/// Une texture de la carte, de taille et de format donnés, que le
/// processeur vidéo peut écrire.
fn texture(appareil: &Appareil, taille: (u32, u32), format: DXGI_FORMAT) -> anyhow::Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: taille.0,
        Height: taille.1,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut t = None;
    unsafe { appareil.device.CreateTexture2D(&desc, None, Some(&mut t)) }.context("texture de la chaîne")?;
    t.context("texture absente")
}

/// Une qualité émise : sa texture NV12, la conversion qui la remplit, sa
/// session NVENC.
struct Sortie {
    // L'ordre des champs est celui de la destruction : NVENC (qui tient la
    // texture enregistrée) avant la texture.
    nvenc: Nvenc,
    conv: Option<Convertisseur>,
    nv12: ID3D11Texture2D,
    taille: (u32, u32),
    fps: u32,
    kbps: u32,
}

impl Sortie {
    fn ouvrir(appareil: &Appareil, taille: (u32, u32), kbps: u32, fps: u32, gop_s: u32) -> anyhow::Result<Self> {
        let nv12 = texture(appareil, taille, DXGI_FORMAT_NV12)?;
        let nvenc = Nvenc::sur_texture(
            &appareil.device,
            &appareil.carte,
            &nv12,
            taille.0,
            taille.1,
            kbps.saturating_mul(1000),
            fps,
            gop_s,
            Profil::Diffusion,
        )?;
        Ok(Self { nvenc, conv: None, nv12, taille, fps, kbps })
    }

    /// La surface capturée, cadrée et convertie dans sa texture — soumis à
    /// la carte, pas attendu.
    fn convertir(
        &mut self,
        appareil: &Appareil,
        surface: &ID3D11Texture2D,
        entree: (u32, u32),
        contenu: (u32, u32),
    ) -> anyhow::Result<()> {
        if self.conv.as_ref().is_none_or(|c| c.entree != entree) {
            self.conv = Some(Convertisseur::new(
                appareil,
                entree,
                self.taille,
                &self.nv12,
                Couleurs::Nv12Bt601,
                self.fps,
                "diffusion",
            )?);
        }
        let conv = self.conv.as_mut().context("conversion absente")?;
        conv.convertir(appareil, surface, contenu, self.taille)
    }
}

/// La taille de l'aperçu : l'image émise réduite dans `APERCU_BOITE`, au
/// même rapport, aux dimensions paires.
fn taille_apercu(emise: (u32, u32)) -> (u32, u32) {
    let (w, h) = (u64::from(emise.0.max(2)), u64::from(emise.1.max(2)));
    let (bw, bh) = (u64::from(APERCU_BOITE.0), u64::from(APERCU_BOITE.1));
    let (w, h) = if w <= bw && h <= bh {
        (w, h)
    } else if w * bh >= h * bw {
        (bw, (h * bw / w).max(2))
    } else {
        ((w * bh / h).max(2), bh)
    };
    ((w as u32) & !1, (h as u32) & !1)
}

/// L'aperçu de la personne qui diffuse : une vignette BGRA faite par la
/// carte, relue sans l'attendre.
struct Apercu {
    conv: Option<Convertisseur>,
    cible: ID3D11Texture2D,
    /// Trois textures de relecture en rotation : on relit celle écrite deux
    /// tours plus tôt, que la carte a eu le temps de finir.
    relues: Vec<ID3D11Texture2D>,
    ecrites: [bool; 3],
    tour: usize,
    taille: (u32, u32),
    derniere: Option<Instant>,
    rgba: Vec<u8>,
}

impl Apercu {
    fn ouvrir(appareil: &Appareil, emise: (u32, u32)) -> anyhow::Result<Self> {
        let taille = taille_apercu(emise);
        let cible = texture(appareil, taille, DXGI_FORMAT_B8G8R8A8_UNORM)?;
        let mut relues = Vec::with_capacity(3);
        for _ in 0..3 {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: taille.0,
                Height: taille.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut t = None;
            unsafe { appareil.device.CreateTexture2D(&desc, None, Some(&mut t)) }.context("texture de relecture")?;
            relues.push(t.context("texture de relecture absente")?);
        }
        Ok(Self {
            conv: None,
            cible,
            relues,
            ecrites: [false; 3],
            tour: 0,
            taille,
            derniere: None,
            rgba: Vec::new(),
        })
    }

    /// C'est l'heure d'une vignette.
    fn due(&self) -> bool {
        self.derniere.is_none_or(|d| d.elapsed() >= APERCU_PAS)
    }

    /// La vignette de cette surface : convertie, copiée pour la relecture.
    fn preparer(
        &mut self,
        appareil: &Appareil,
        surface: &ID3D11Texture2D,
        entree: (u32, u32),
        contenu: (u32, u32),
        fps: u32,
    ) -> anyhow::Result<()> {
        if self.conv.as_ref().is_none_or(|c| c.entree != entree) {
            self.conv = Some(Convertisseur::new(
                appareil,
                entree,
                self.taille,
                &self.cible,
                Couleurs::Bgra,
                fps,
                "diffusion",
            )?);
        }
        let conv = self.conv.as_mut().context("conversion absente")?;
        conv.convertir(appareil, surface, contenu, self.taille)?;
        unsafe { appareil.contexte.CopyResource(&self.relues[self.tour], &self.cible) };
        self.ecrites[self.tour] = true;
        self.derniere = Some(Instant::now());
        Ok(())
    }

    /// La plus ancienne vignette copiée, si la carte l'a finie — sinon
    /// rien, et l'on repassera. Puis le tour suivant.
    fn relire(&mut self, appareil: &Appareil) -> Option<RgbaFrame> {
        let ancienne = (self.tour + 1) % 3;
        self.tour = ancienne;
        if !self.ecrites[ancienne] {
            return None;
        }
        let t = &self.relues[ancienne];
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        let lue = unsafe {
            appareil.contexte.Map(t, 0, D3D11_MAP_READ, D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32, Some(&mut m))
        };
        match lue {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAS_STILL_DRAWING => return None,
            Err(_) => return None,
        }
        self.ecrites[ancienne] = false;
        let (w, h) = (self.taille.0 as usize, self.taille.1 as usize);
        self.rgba.clear();
        self.rgba.reserve(w * h * 4);
        for y in 0..h {
            let ligne = unsafe { std::slice::from_raw_parts((m.pData as *const u8).add(y * m.RowPitch as usize), w * 4) };
            // BGRA → RGBA, opaque.
            for px in ligne.as_chunks::<4>().0 {
                self.rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
            }
        }
        unsafe { appareil.contexte.Unmap(t, 0) };
        Some(RgbaFrame { width: w, height: h, rgba: self.rgba.clone() })
    }
}

/// Tout ce qui vit sur la carte pour une diffusion.
struct Chaine {
    // L'ordre des champs est celui de la destruction : les sessions et
    // textures avant la capture, la capture avant le device.
    haute: Sortie,
    basse: Option<Sortie>,
    /// La basse a été refusée pour cette chaîne (session NVENC de trop…) :
    /// le serveur s'en apercevra et s'en passera.
    basse_hs: bool,
    basse_pts: Option<u64>,
    apercu: Option<Apercu>,
    capture: Capture,
    fps: u32,
    gop_s: u32,
    appareil: Appareil,
    /// La prochaine image haute doit être une trame clé.
    idr: bool,
    /// Le dernier horodatage émis : les suivants montent toujours.
    dernier_pts: Option<u64>,
}

impl Chaine {
    fn ouvrir(config: &StreamConfig, reveil: std::thread::Thread) -> anyhow::Result<Self> {
        let appareil = Appareil::nvidia()?;
        let (item, _) = element(&config.source)?;
        let source = item.Size().context("taille de la source")?;
        if source.Width <= 0 || source.Height <= 0 {
            anyhow::bail!("la source n'a pas de taille (fenêtre réduite ?)");
        }
        let (w, h) = ((source.Width as u32) & !1, (source.Height as u32) & !1);
        let emise = scale::target_dims(w.max(2), h.max(2), config.max_height);
        let fps = config.fps.clamp(1, 120);
        let kbps = (config.bitrate_bps / 1000).max(100);
        let haute = Sortie::ouvrir(&appareil, emise, kbps, fps, config.gop_s)?;
        let apercu = if config.preview {
            match Apercu::ouvrir(&appareil, emise) {
                Ok(a) => Some(a),
                Err(e) => {
                    journal(format!("diffusion : pas d'aperçu sur la carte ({e:#})"));
                    None
                }
            }
        } else {
            None
        };
        let capture = Capture::ouvrir(&appareil.winrt, item, config.cursor, fps, reveil, "diffusion")?;
        if emise == (w, h) {
            journal(format!("diffusion : source {w}x{h}"));
        } else {
            journal(format!("diffusion : source {w}x{h}, émise en {}x{}", emise.0, emise.1));
        }
        Ok(Self {
            haute,
            basse: None,
            basse_hs: false,
            basse_pts: None,
            apercu,
            capture,
            fps,
            gop_s: config.gop_s,
            appareil,
            idr: true,
            dernier_pts: None,
        })
    }

    /// Ce que la chaîne dit d'elle-même, à l'ouverture et à chaque reprise.
    fn annoncer(&self, stats: &StageStats, config: &StreamConfig) {
        stats.materiel.store(true, Ordering::Relaxed);
        stats.set_dims(self.haute.taille.0, self.haute.taille.1);
        journal(format!(
            "encodeur : NVENC sur {}, {}x{} à {} i/s, {} kbit/s, tout sur la carte (capture, conversion, encodage{})",
            self.appareil.carte,
            self.haute.taille.0,
            self.haute.taille.1,
            self.fps,
            config.bitrate_bps / 1000,
            if self.apercu.is_some() { ", aperçu" } else { "" }
        ));
    }

    /// Prend l'image arrivée, la convertit pour chaque qualité et l'encode.
    /// `false` : rien de neuf depuis la dernière.
    fn une_image(&mut self, p: &Parts, horloge: &Horloge) -> anyhow::Result<bool> {
        let Some(image) = self.capture.prendre()? else { return Ok(false) };
        let contenu = image.ContentSize().context("taille de l'image")?;
        if contenu.Width <= 0 || contenu.Height <= 0 {
            let _ = image.Close();
            return Ok(false);
        }
        if contenu.Width != self.capture.taille.Width || contenu.Height != self.capture.taille.Height {
            // La source a changé de taille : la réserve suit, l'image (encore
            // à l'ancienne taille) est laissée. La sortie, elle, ne bouge pas.
            let _ = image.Close();
            self.capture.recreer(&self.appareil.winrt, contenu)?;
            return Ok(false);
        }
        let stats = &p.stats;
        stats.captured.fetch_add(1, Ordering::Relaxed);
        let compose = image.SystemRelativeTime().map(|t| t.Duration).unwrap_or(0);
        let mut pts_us = horloge.pts_us(compose, p.origine);
        if let Some(d) = self.dernier_pts {
            pts_us = pts_us.max(d + 1);
        }
        self.dernier_pts = Some(pts_us);

        // Ce que cette image doit nourrir : la haute (sauf si personne ne la
        // regarde), la basse (à sa cadence, si le serveur la veut), l'aperçu.
        let haute = !p.qualites.as_ref().is_some_and(|q| q.haute_suspendue());
        let basse = self.preparer_basse(p, pts_us);
        let apercu = self.apercu.as_ref().is_some_and(Apercu::due);

        // 1. Les conversions, soumises à la carte depuis la même surface.
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
        let contenu = (contenu.Width as u32, contenu.Height as u32);
        if haute {
            self.haute.convertir(&self.appareil, &surface, entree, contenu)?;
        }
        if basse {
            if let Some(b) = self.basse.as_mut() {
                b.convertir(&self.appareil, &surface, entree, contenu)?;
            }
        }
        if apercu {
            if let Some(a) = self.apercu.as_mut() {
                if let Err(e) = a.preparer(&self.appareil, &surface, entree, contenu, self.fps) {
                    journal(format!("diffusion : aperçu arrêté ({e:#})"));
                    self.apercu = None;
                }
            }
        }
        // Soumis avant de rendre la surface à la capture : le compositeur
        // n'y récrira qu'après nos lectures.
        unsafe { self.appareil.contexte.Flush() };
        drop(surface);
        let _ = image.Close();
        stats.convert_ms.record(t0.elapsed().as_secs_f32() * 1000.0);
        stats.converted.fetch_add(1, Ordering::Relaxed);

        // 2. La haute ; seul le flux remonte de la carte.
        if haute {
            let t1 = Instant::now();
            let force = p.force_idr.swap(false, Ordering::Relaxed) | std::mem::take(&mut self.idr);
            match self.haute.nvenc.encoder_texture(force)? {
                Some(paquet) => {
                    stats.encode_ms.record(t1.elapsed().as_secs_f32() * 1000.0);
                    stats.encoded.fetch_add(1, Ordering::Relaxed);
                    stats.encoded_bytes.fetch_add(paquet.data.len() as u64, Ordering::Relaxed);
                    if paquet.idr {
                        stats.keyframes.fetch_add(1, Ordering::Relaxed);
                    }
                    (p.emit)(EncodedFrame {
                        data: paquet.data,
                        idr: paquet.idr,
                        pts_us,
                        width: self.haute.taille.0 as u16,
                        height: self.haute.taille.1 as u16,
                        basse: false,
                    });
                }
                None => {
                    // Sautée par l'encodeur ; la trame clé demandée, elle,
                    // ne se perd pas.
                    stats.enc_skipped.fetch_add(1, Ordering::Relaxed);
                    if force {
                        p.force_idr.store(true, Ordering::Relaxed);
                    }
                }
            }
        }

        // 3. La basse, pour les connexions qui ne suivent pas la haute.
        if basse {
            if let (Some(b), Some(q)) = (self.basse.as_mut(), p.qualites.as_ref()) {
                let t2 = Instant::now();
                let force = q.prendre_idr_basse();
                match b.nvenc.encoder_texture(force) {
                    Ok(Some(paquet)) => {
                        stats.basse_ms.record(t2.elapsed().as_secs_f32() * 1000.0);
                        stats.basse_encoded.fetch_add(1, Ordering::Relaxed);
                        stats.basse_bytes.fetch_add(paquet.data.len() as u64, Ordering::Relaxed);
                        (p.emit)(EncodedFrame {
                            data: paquet.data,
                            idr: paquet.idr,
                            pts_us,
                            width: b.taille.0 as u16,
                            height: b.taille.1 as u16,
                            basse: true,
                        });
                    }
                    Ok(None) => {
                        if force {
                            q.force_keyframe_basse();
                        }
                    }
                    Err(e) => {
                        // Recréée à l'image suivante ; son premier paquet
                        // sera une trame clé.
                        journal(format!("diffusion : qualité basse, encodage raté ({e:#}) — encodeur recréé"));
                        self.basse = None;
                    }
                }
            }
        }

        // 4. L'aperçu : la vignette que la carte a finie, s'il y en a une.
        if apercu {
            let t3 = Instant::now();
            if let Some(image) = self.apercu.as_mut().and_then(|a| a.relire(&self.appareil)) {
                stats.decode_ms.record(t3.elapsed().as_secs_f32() * 1000.0);
                stats.decoded.fetch_add(1, Ordering::Relaxed);
                (p.apercu)(image);
            }
        }
        Ok(true)
    }

    /// La basse que le serveur veut, ouverte ou refermée selon lui ; vrai
    /// si cette image doit la nourrir (à sa cadence, 30 i/s au plus).
    fn preparer_basse(&mut self, p: &Parts, pts_us: u64) -> bool {
        let Some(q) = p.qualites.as_ref() else { return false };
        let Some(kbps) = q.basse().filter(|_| !self.basse_hs) else {
            if self.basse.take().is_some() {
                p.stats.set_basse_dims(0, 0);
                journal("diffusion : qualité basse arrêtée");
            }
            return false;
        };
        let (hb, fb) = qualite_basse(kbps, self.haute.taille.1, self.fps);
        let taille = scale::target_dims(self.haute.taille.0, self.haute.taille.1, hb);
        if self.basse.as_ref().map(|b| (b.taille, b.kbps, b.fps)) != Some((taille, kbps, fb)) {
            // L'ancienne session fermée d'abord : les cartes grand public
            // comptent leurs sessions NVENC.
            self.basse = None;
            match Sortie::ouvrir(&self.appareil, taille, kbps, fb, self.gop_s) {
                Ok(s) => {
                    journal(format!("diffusion : qualité basse {}x{} à {fb} i/s, {kbps} kbit/s", taille.0, taille.1));
                    p.stats.set_basse_dims(taille.0, taille.1);
                    self.basse = Some(s);
                    self.basse_pts = None;
                }
                Err(e) => {
                    journal(format!("diffusion : qualité basse impossible ({e:#})"));
                    p.stats.set_basse_dims(0, 0);
                    self.basse_hs = true;
                    return false;
                }
            }
        }
        // Sa cadence : une image sur deux quand la capture va deux fois plus
        // vite — à un quart d'intervalle près, la gigue de la capture.
        let pas_us = 1_000_000 / u64::from(fb.max(1));
        if self.basse_pts.is_some_and(|d| pts_us + pas_us / 4 < d + pas_us) {
            return false;
        }
        self.basse_pts = Some(pts_us);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l_apercu_tient_dans_sa_boite_au_rapport_de_l_image() {
        // La boîte est un rien plus large que 16:9 : pleine hauteur.
        assert_eq!(taille_apercu((1920, 1080)), (852, 480));
        // L'ultra-large de Pandora : pleine largeur, moins haut.
        assert_eq!(taille_apercu((3440, 1440)), (854, 356));
        // Plus étroit que 16:9 : pleine hauteur.
        assert_eq!(taille_apercu((1440, 1080)), (640, 480));
        // Déjà petit : tel quel (pair).
        assert_eq!(taille_apercu((641, 361)), (640, 360));
    }

    /// Les couleurs, mesurées sur la carte : un rouge pur converti pour la
    /// diffusion doit sortir en BT.601 plage limitée — Y 81, Cb 90, Cr 240,
    /// ce que le décodeur des spectateurs (openh264) attend — et pour un
    /// clip en BT.709 (Y 63, Cb 102). Ignoré par défaut (carte NVIDIA) :
    /// `cargo test -p ki-video couleurs_de_la_carte -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn les_couleurs_de_la_carte_suivent_le_decodeur() {
        let appareil = match Appareil::nvidia() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("pas de carte NVIDIA ici : {e:#}");
                return;
            }
        };
        let (w, h) = (64u32, 64u32);
        // Un carré BGRA rouge pur.
        let rouge: Vec<u8> = [0u8, 0, 255, 255].repeat((w * h) as usize);
        let source = unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let donnees = D3D11_SUBRESOURCE_DATA { pSysMem: rouge.as_ptr().cast(), SysMemPitch: w * 4, SysMemSlicePitch: 0 };
            let mut t = None;
            appareil.device.CreateTexture2D(&desc, Some(&donnees), Some(&mut t)).unwrap();
            t.unwrap()
        };
        let mesurer = |couleurs: Couleurs| -> (u8, u8, u8) {
            let nv12 = texture(&appareil, (w, h), DXGI_FORMAT_NV12).unwrap();
            let mut conv = Convertisseur::new(&appareil, (w, h), (w, h), &nv12, couleurs, 30, "test").unwrap();
            conv.convertir(&appareil, &source, (w, h), (w, h)).unwrap();
            unsafe {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: w,
                    Height: h,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_NV12,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    MiscFlags: 0,
                };
                let mut t = None;
                appareil.device.CreateTexture2D(&desc, None, Some(&mut t)).unwrap();
                let t = t.unwrap();
                appareil.contexte.CopyResource(&t, &nv12);
                let mut m = D3D11_MAPPED_SUBRESOURCE::default();
                appareil.contexte.Map(&t, 0, D3D11_MAP_READ, 0, Some(&mut m)).unwrap();
                let octets = m.pData as *const u8;
                let pas = m.RowPitch as usize;
                // Le milieu : luma, puis le couple Cb/Cr du plan entrelacé.
                let y = *octets.add(32 * pas + 32);
                let uv = octets.add((h as usize + 16) * pas + 32);
                let (cb, cr) = (*uv, *uv.add(1));
                appareil.contexte.Unmap(&t, 0);
                (y, cb, cr)
            }
        };
        let proche = |a: u8, b: u8| a.abs_diff(b) <= 3;
        let (y, cb, cr) = mesurer(Couleurs::Nv12Bt601);
        eprintln!("diffusion : rouge → Y {y}, Cb {cb}, Cr {cr}");
        assert!(proche(y, 81) && proche(cb, 90) && proche(cr, 240), "BT.601 attendu (81, 90, 240)");
        let (y, cb, cr) = mesurer(Couleurs::Nv12Bt709);
        eprintln!("clip : rouge → Y {y}, Cb {cb}, Cr {cr}");
        assert!(proche(y, 63) && proche(cb, 102) && proche(cr, 240), "BT.709 attendu (63, 102, 240)");
    }

    /// Le vrai circuit sur cette machine : l'écran principal quelques
    /// secondes par la carte, une haute qui sort (trame clé en tête), une
    /// basse demandée en route, des vignettes d'aperçu. Ignoré par défaut
    /// (il faut un écran et une carte NVIDIA) :
    /// `cargo test -p ki-video diffusion_tout_gpu -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn diffusion_tout_gpu_filme_l_ecran() {
        let stats = Arc::new(StageStats::default());
        let trames = Arc::new(std::sync::Mutex::new(Vec::<(bool, bool, usize, u64, u16, u16)>::new()));
        let emit: FrameEmit = {
            let t = trames.clone();
            Arc::new(move |f: EncodedFrame| t.lock().unwrap().push((f.basse, f.idr, f.data.len(), f.pts_us, f.width, f.height)))
        };
        let vignettes = Arc::new(std::sync::Mutex::new(Vec::<(usize, usize, u8)>::new()));
        let apercu: FrameSink = {
            let v = vignettes.clone();
            Arc::new(move |f: RgbaFrame| {
                // La luminosité moyenne d'une ligne sur deux : une vignette
                // noire serait une relecture ratée.
                let moyenne = (f.rgba.iter().step_by(4 * 7).map(|&x| u64::from(x)).sum::<u64>()
                    / (f.rgba.len() / (4 * 7)).max(1) as u64) as u8;
                v.lock().unwrap().push((f.width, f.height, moyenne));
            })
        };
        let qualites = Arc::new(Qualites::default());
        let config = StreamConfig { fps: 60, bitrate_bps: 20_000_000, ..StreamConfig::default() };
        let diffusion = match DiffusionGpu::demarrer(
            stats.clone(),
            apercu,
            emit,
            config,
            Arc::new(AtomicBool::new(false)),
            Instant::now(),
            Some(qualites.clone()),
        ) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("diffusion tout-GPU indisponible ici : {e:#}");
                return;
            }
        };
        std::thread::sleep(Duration::from_secs(2));
        qualites.regler_basse(Some(2500));
        std::thread::sleep(Duration::from_secs(2));
        diffusion.arreter();
        let t = trames.lock().unwrap();
        let hautes: Vec<_> = t.iter().filter(|x| !x.0).collect();
        let basses: Vec<_> = t.iter().filter(|x| x.0).collect();
        let v = vignettes.lock().unwrap();
        eprintln!(
            "{} hautes, {} basses, {} vignettes ({:?}), {}",
            hautes.len(),
            basses.len(),
            v.len(),
            v.first(),
            stats.summary()
        );
        assert!(!hautes.is_empty() && hautes[0].1, "la haute commence par une trame clé");
        assert!(hautes.windows(2).all(|w| w[1].3 > w[0].3), "horodatages croissants");
        assert!(hautes.iter().all(|x| (x.4, x.5) == (hautes[0].4, hautes[0].5)), "taille constante");
        assert!(!basses.is_empty() && basses[0].1, "la basse demandée sort, trame clé en tête");
        assert!(basses[0].5 <= 720, "la basse est plus petite : {}p", basses[0].5);
        assert!(!v.is_empty(), "des vignettes d'aperçu");
    }
}
