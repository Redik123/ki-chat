//! Le décodeur H.264 des trames brutes — celles d'un stream regardé —, par
//! Media Foundation : le décodeur de Microsoft, présent dans chaque Windows,
//! sur la carte graphique (DXVA) quand elle sait. openh264, celui d'avant,
//! décode sur un seul cœur : une image 3440×1440 lui coûtait 20 à 30 ms sur
//! un i7-11700K, pour 33 ms de budget à 30 images par seconde — au moindre
//! à-côté (une compilation, un jeu), l'image sautait.
//!
//! Mode faible latence : chaque trame entrée ressort aussitôt décodée, sans
//! attendre les suivantes (les streams n'ont pas d'images B). Sans lui, le
//! décodeur de Microsoft décode sur tous les cœurs, mais en gardant une
//! vingtaine d'images d'avance — 800 ms de retard, le son en avance sur
//! l'image ; avec lui, sur le processeur, il ne prend plus qu'un cœur par
//! image (une seule tranche par image, comme NVENC les fait), à peine plus
//! vite qu'openh264. D'où la carte graphique : un moteur à part, qui ne
//! prend rien au jeu ni au processeur. La sortie est
//! du NV12, converti en RGBA sur tous les cœurs (`pixels`), comme pour les
//! fichiers de la visionneuse — mais en BT.601 à toutes les tailles : c'est
//! ce que le streamer encode, parce que c'est ce qu'openh264 suppose (voir
//! `Couleurs` dans ki-video).
//!
//! Tout ce qui parle à COM est ici ; hors Windows, `new` refuse en le
//! disant, et le spectateur garde openh264.

use crate::Image;

/// Le décodeur, prêt à recevoir des trames Annex B (une image chacune).
pub struct DecodeurH264 {
    #[cfg(windows)]
    mft: mft::Mft,
}

impl DecodeurH264 {
    /// Le décodeur de Microsoft, en mode faible latence : sur la carte
    /// graphique si elle sait, sinon sur le processeur. Échoue sur un
    /// Windows sans Media Foundation (éditions « N » sans le Media Feature
    /// Pack), ou hors Windows.
    pub fn new() -> anyhow::Result<Self> {
        #[cfg(windows)]
        {
            match mft::Mft::new(true) {
                Ok(mft) => Ok(Self { mft }),
                Err(e) => {
                    tracing::info!("décodeur H.264 : pas sur la carte ({e:#}), sur le processeur");
                    Self::logiciel()
                }
            }
        }
        #[cfg(not(windows))]
        {
            anyhow::bail!("le décodeur de Media Foundation n'existe que sous Windows")
        }
    }

    /// Le même, sur le processeur seulement.
    pub fn logiciel() -> anyhow::Result<Self> {
        #[cfg(windows)]
        {
            Ok(Self { mft: mft::Mft::new(false)? })
        }
        #[cfg(not(windows))]
        {
            anyhow::bail!("le décodeur de Media Foundation n'existe que sous Windows")
        }
    }

    /// Où il décode, une fois la première image sortie : `Some(true)` sur
    /// la carte graphique, `Some(false)` sur le processeur.
    pub fn sur_la_carte(&self) -> Option<bool> {
        #[cfg(windows)]
        {
            self.mft.sur_la_carte
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    /// Une trame (Annex B, une image) ; l'image décodée, s'il en sort une.
    pub fn decoder(&mut self, trame: &[u8]) -> anyhow::Result<Option<Image>> {
        #[cfg(windows)]
        {
            self.mft.decoder(trame)
        }
        #[cfg(not(windows))]
        {
            let _ = trame;
            Ok(None)
        }
    }
}

#[cfg(windows)]
mod mft {
    use std::mem::ManuallyDrop;
    use std::ptr::null_mut;

    use anyhow::Context;
    use windows::core::Interface;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Device, ID3D11Multithread, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
        D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
        D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    };
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
    use windows::Win32::Graphics::Dxgi::IDXGIAdapter;
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

    use crate::mf::{convertir, format_du_type, image_nv12, preparer, FormatVideo};
    use crate::pixels::Matrice;
    use crate::Image;

    /// Une image toutes les 33 ms, en centaines de nanosecondes : le
    /// décodeur veut des horodatages croissants, leur valeur ne sert pas.
    const PAS_TEMPS: i64 = 333_333;

    pub(super) struct Mft {
        transform: IMFTransform,
        format: Option<FormatVideo>,
        /// La taille d'un tampon de sortie, quand c'est à nous de le fournir.
        taille_sortie: u32,
        /// Le décodeur fournit ses propres échantillons de sortie.
        fournit: bool,
        temps: i64,
        pub(super) sur_la_carte: Option<bool>,
        /// L'appareil Direct3D 11 prêté au décodeur, gardé en vie avec lui.
        carte: Option<(ID3D11Device, IMFDXGIDeviceManager)>,
        /// La texture où l'on recopie chaque image pour la lire.
        lecture: Option<ID3D11Texture2D>,
    }

    impl Mft {
        /// `carte` : décoder sur la carte graphique, ou échouer.
        pub(super) fn new(carte: bool) -> anyhow::Result<Self> {
            preparer()?;
            let transform: IMFTransform =
                unsafe { CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER) }
                    .context("décodeur H.264 de Windows")?;
            let attributs = unsafe { transform.GetAttributes() }.context("attributs du décodeur")?;
            // Faible latence : une image sort dès sa trame entrée.
            unsafe { attributs.SetUINT32(&MF_LOW_LATENCY, 1) }.context("mode faible latence")?;
            let carte = if carte {
                if unsafe { attributs.GetUINT32(&MF_SA_D3D11_AWARE) }.unwrap_or(0) == 0 {
                    anyhow::bail!("le décodeur de Windows ne sait pas décoder sur la carte");
                }
                let (appareil, gestionnaire) = appareil_video()?;
                unsafe { transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, gestionnaire.as_raw() as usize) }
                    .context("carte graphique refusée par le décodeur")?;
                Some((appareil, gestionnaire))
            } else {
                None
            };
            let entree = unsafe { MFCreateMediaType() }?;
            unsafe {
                entree.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                entree.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
                transform.SetInputType(0, &entree, 0).context("entrée H.264")?;
            }
            let mut m = Self {
                transform,
                format: None,
                taille_sortie: 0,
                fournit: false,
                temps: 0,
                sur_la_carte: None,
                carte,
                lecture: None,
            };
            m.choisir_sortie()?;
            unsafe {
                m.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
                m.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            }
            Ok(m)
        }

        /// La sortie NV12 parmi celles que propose le décodeur — au départ,
        /// puis à chaque changement de format (une autre taille d'image).
        fn choisir_sortie(&mut self) -> anyhow::Result<()> {
            let mut i = 0;
            loop {
                let t = unsafe { self.transform.GetOutputAvailableType(0, i) }
                    .context("pas de sortie NV12 au décodeur H.264")?;
                if unsafe { t.GetGUID(&MF_MT_SUBTYPE) }.ok() == Some(MFVideoFormat_NV12) {
                    unsafe { self.transform.SetOutputType(0, &t, 0) }.context("sortie NV12")?;
                    break;
                }
                i += 1;
            }
            let info = unsafe { self.transform.GetOutputStreamInfo(0) }?;
            self.taille_sortie = info.cbSize;
            let fournit = (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
            self.fournit = info.dwFlags & fournit != 0;
            // La taille n'est connue qu'après la première trame clé : le
            // format se relira alors.
            self.format = self.format_de_sortie().ok();
            Ok(())
        }

        /// Le format des images qui sortent, en BT.601 quelle que soit leur
        /// taille.
        fn format_de_sortie(&self) -> anyhow::Result<FormatVideo> {
            let t = unsafe { self.transform.GetOutputCurrentType(0) }?;
            let mut format = format_du_type(&t)?;
            format.matrice = Matrice::Bt601;
            Ok(format)
        }

        pub(super) fn decoder(&mut self, trame: &[u8]) -> anyhow::Result<Option<Image>> {
            let echantillon = echantillon_de(trame, self.temps)?;
            self.temps += PAS_TEMPS;
            let mut image = None;
            match unsafe { self.transform.ProcessInput(0, &echantillon, 0) } {
                Ok(()) => {}
                // Une image attendait encore d'être sortie : on la sort, puis
                // on redonne la trame.
                Err(e) if e.code() == MF_E_NOTACCEPTING => {
                    image = self.sortir()?;
                    unsafe { self.transform.ProcessInput(0, &echantillon, 0) }.context("entrée refusée")?;
                }
                Err(e) => return Err(e).context("trame refusée"),
            }
            Ok(self.sortir()?.or(image))
        }

        /// Tout ce que le décodeur a de prêt ; la dernière image.
        fn sortir(&mut self) -> anyhow::Result<Option<Image>> {
            let mut image = None;
            loop {
                let fourni = if self.fournit { None } else { Some(echantillon_vide(self.taille_sortie)?) };
                let mut tampons = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(fourni),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                }];
                let mut statut = 0u32;
                let resultat = unsafe { self.transform.ProcessOutput(0, &mut tampons, &mut statut) };
                // Ce que l'appel nous rend est à nous : on le reprend pour le
                // libérer.
                let rendu = unsafe { ManuallyDrop::take(&mut tampons[0].pSample) };
                drop(unsafe { ManuallyDrop::take(&mut tampons[0].pEvents) });
                match resultat {
                    Ok(()) => {
                        let Some(rendu) = rendu else { return Ok(image) };
                        if self.format.is_none() {
                            self.format = Some(self.format_de_sortie()?);
                        }
                        // Une image restée sur la carte est une texture : le
                        // décodeur a bien pris la carte (il peut s'en passer
                        // en silence, pour un flux qu'elle ne sait pas lire).
                        let texture = unsafe { rendu.GetBufferByIndex(0) }?.cast::<IMFDXGIBuffer>().ok();
                        self.sur_la_carte.get_or_insert(texture.is_some());
                        let format = self.format.clone().context("format de sortie inconnu")?;
                        image = Some(match texture {
                            Some(texture) => self.lire_texture(&texture, &format)?,
                            None => image_nv12(&format, &rendu, 0)?,
                        });
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(image),
                    // Une nouvelle taille (la première trame clé, ou le
                    // streamer qui change de palier) : on renégocie.
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.choisir_sortie()?,
                    Err(e) => return Err(e).context("sortie du décodeur"),
                }
            }
        }

        /// Une image restée sur la carte : recopiée dans une texture que le
        /// processeur peut lire, puis convertie à même elle. La copie que
        /// Media Foundation fait d'elle-même (verrouiller le tampon) prenait
        /// 8 ms par image 3440×1440 ; celle-ci, le temps que la carte finisse
        /// de décoder.
        fn lire_texture(&mut self, tampon: &IMFDXGIBuffer, format: &FormatVideo) -> anyhow::Result<Image> {
            let (appareil, _) = self.carte.as_ref().context("image sur la carte sans appareil")?;
            let mut source: Option<ID3D11Texture2D> = None;
            unsafe { tampon.GetResource(&ID3D11Texture2D::IID, &mut source as *mut _ as *mut _) }
                .context("texture de l'image")?;
            let source = source.context("texture de l'image absente")?;
            let index = unsafe { tampon.GetSubresourceIndex() }?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { source.GetDesc(&mut desc) };
            if desc.Format != DXGI_FORMAT_NV12 {
                anyhow::bail!("image de la carte au format {:?}, pas en NV12", desc.Format);
            }
            let lecture = match &self.lecture {
                Some(t) if {
                    let mut d = D3D11_TEXTURE2D_DESC::default();
                    unsafe { t.GetDesc(&mut d) };
                    (d.Width, d.Height) == (desc.Width, desc.Height)
                } => t.clone(),
                _ => {
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
                    let mut t = None;
                    unsafe { appareil.CreateTexture2D(&d, None, Some(&mut t)) }.context("texture de lecture")?;
                    let t = t.context("texture de lecture absente")?;
                    self.lecture = Some(t.clone());
                    t
                }
            };
            let contexte = unsafe { appareil.GetImmediateContext() }?;
            let mut lu = D3D11_MAPPED_SUBRESOURCE::default();
            unsafe {
                contexte.CopySubresourceRegion(&lecture, 0, 0, 0, 0, &source, index, None);
                // Attend la carte : le décodage, puis la copie.
                contexte.Map(&lecture, 0, D3D11_MAP_READ, 0, Some(&mut lu))
            }
            .context("lecture de l'image")?;
            let pas = lu.RowPitch as usize;
            let lignes = desc.Height as usize;
            // NV12 : le plan de chrominance suit celui de luminance, au même
            // pas — la taille que la carte annonce (`DepthPitch`) le confirme
            // quand elle la donne.
            let longueur = pas * (lignes + lignes.div_ceil(2));
            let mut rgba = Vec::new();
            let resultat = if lu.pData.is_null() || (lu.DepthPitch != 0 && (lu.DepthPitch as usize) < longueur) {
                Err(anyhow::anyhow!(
                    "image de la carte illisible ({} octets annoncés, {longueur} attendus)",
                    lu.DepthPitch
                ))
            } else {
                let octets = unsafe { std::slice::from_raw_parts(lu.pData as *const u8, longueur) };
                convertir(format, octets, pas, lignes, &mut rgba)
            };
            unsafe { contexte.Unmap(&lecture, 0) };
            resultat?;
            Ok(Image { pts_ms: 0, largeur: format.largeur as u32, hauteur: format.hauteur as u32, rgba })
        }
    }

    /// Un appareil Direct3D 11 de la carte principale, avec la vidéo, et le
    /// gestionnaire qui le prête au décodeur.
    fn appareil_video() -> anyhow::Result<(ID3D11Device, IMFDXGIDeviceManager)> {
        let mut appareil = None;
        unsafe {
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut appareil),
                None,
                None,
            )
        }
        .context("appareil Direct3D 11 avec la vidéo")?;
        let appareil: ID3D11Device = appareil.context("appareil Direct3D 11 absent")?;
        // Le décodeur s'en sert depuis ses propres fils.
        if let Ok(mt) = appareil.cast::<ID3D11Multithread>() {
            let _ = unsafe { mt.SetMultithreadProtected(true) };
        }
        let mut jeton = 0u32;
        let mut gestionnaire = None;
        unsafe { MFCreateDXGIDeviceManager(&mut jeton, &mut gestionnaire) }.context("gestionnaire DXGI")?;
        let gestionnaire: IMFDXGIDeviceManager = gestionnaire.context("gestionnaire DXGI absent")?;
        unsafe { gestionnaire.ResetDevice(&appareil, jeton) }.context("appareil du gestionnaire DXGI")?;
        Ok((appareil, gestionnaire))
    }

    /// Un échantillon qui porte une copie de la trame.
    fn echantillon_de(trame: &[u8], temps: i64) -> anyhow::Result<IMFSample> {
        unsafe {
            let tampon = MFCreateMemoryBuffer(trame.len() as u32)?;
            let mut ptr: *mut u8 = null_mut();
            tampon.Lock(&mut ptr, None, None)?;
            if !ptr.is_null() {
                std::ptr::copy_nonoverlapping(trame.as_ptr(), ptr, trame.len());
            }
            tampon.Unlock()?;
            tampon.SetCurrentLength(trame.len() as u32)?;
            let echantillon = MFCreateSample()?;
            echantillon.AddBuffer(&tampon)?;
            echantillon.SetSampleTime(temps)?;
            echantillon.SetSampleDuration(PAS_TEMPS)?;
            Ok(echantillon)
        }
    }

    /// Un échantillon vide où le décodeur écrira son image.
    fn echantillon_vide(taille: u32) -> anyhow::Result<IMFSample> {
        unsafe {
            let tampon = MFCreateMemoryBuffer(taille.max(1))?;
            let echantillon = MFCreateSample()?;
            echantillon.AddBuffer(&tampon)?;
            Ok(echantillon)
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::annexb;

    /// Un flux H.264 brut de la taille voulue, par ffmpeg s'il est là.
    fn flux(largeur: u32, hauteur: u32, secondes: u32) -> Option<Vec<u8>> {
        let chemin = std::env::temp_dir()
            .join("ki-media-essais")
            .join(format!("regard-{largeur}x{hauteur}-{secondes}s.h264"));
        if !chemin.exists() {
            std::fs::create_dir_all(chemin.parent()?).ok()?;
            let source = format!("testsrc2=size={largeur}x{hauteur}:rate=30");
            let ok = std::process::Command::new("ffmpeg")
                .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", &source, "-t", &secondes.to_string()])
                .args(["-c:v", "libx264", "-preset", "ultrafast", "-bf", "0", "-g", "30"])
                .args(["-x264-params", "aud=1:repeat-headers=1", "-pix_fmt", "yuv420p"])
                .arg(&chemin)
                .status()
                .ok()?
                .success();
            if !ok {
                return None;
            }
        }
        std::fs::read(&chemin).ok()
    }

    /// Les deux décodeurs : sur la carte graphique (sur une machine sans
    /// carte, comme celles de la CI, `new` retombe sur le processeur), et
    /// sur le processeur.
    fn les_deux() -> [(&'static str, DecodeurH264); 2] {
        [
            ("carte", DecodeurH264::new().expect("décodeur de Windows")),
            ("processeur", DecodeurH264::logiciel().expect("décodeur de Windows")),
        ]
    }

    #[test]
    fn chaque_trame_ressort_en_image() {
        let Some(octets) = flux(640, 360, 2) else {
            eprintln!("ffmpeg absent : test sauté");
            return;
        };
        let unites = annexb::unites_d_acces(&octets);
        assert!(unites.len() >= 55, "{} trames", unites.len());
        for (nom, mut d) in les_deux() {
            let mut images = 0;
            for u in &unites {
                if let Some(image) = d.decoder(u).expect("décodage") {
                    assert_eq!((image.largeur, image.hauteur), (640, 360), "{nom}");
                    assert_eq!(image.rgba.len(), 640 * 360 * 4, "{nom}");
                    images += 1;
                }
            }
            // Faible latence : une image par trame, la toute première
            // comprise, à une ou deux près.
            assert!(images + 2 >= unites.len(), "{nom} : {images} images pour {} trames", unites.len());
            eprintln!("{nom} : sur la carte {:?}", d.sur_la_carte());
        }
    }

    #[test]
    fn une_hauteur_non_multiple_de_16_est_rognee() {
        // 1080 lignes : le décodeur rend des tampons de 1088.
        let Some(octets) = flux(1920, 1080, 1) else {
            eprintln!("ffmpeg absent : test sauté");
            return;
        };
        for (nom, mut d) in les_deux() {
            let image = annexb::unites_d_acces(&octets)
                .iter()
                .find_map(|u| d.decoder(u).expect("décodage"))
                .expect("une image");
            assert_eq!((image.largeur, image.hauteur), (1920, 1080), "{nom}");
        }
    }

    #[test]
    fn un_changement_de_taille_passe() {
        // Le streamer change de palier : 640×360, puis 1280×720.
        let (Some(petit), Some(grand)) = (flux(640, 360, 1), flux(1280, 720, 1)) else {
            eprintln!("ffmpeg absent : test sauté");
            return;
        };
        for (nom, mut d) in les_deux() {
            let mut tailles = Vec::new();
            for u in annexb::unites_d_acces(&petit).iter().chain(annexb::unites_d_acces(&grand).iter()) {
                if let Some(image) = d.decoder(u).expect("décodage") {
                    tailles.push((image.largeur, image.hauteur));
                }
            }
            assert_eq!(tailles.first(), Some(&(640, 360)), "{nom}");
            assert_eq!(tailles.last(), Some(&(1280, 720)), "{nom}");
        }
    }

    /// Les mêmes pixels qu'openh264, le décodeur d'avant. En 720p, une
    /// matrice BT.709 (celle des fichiers à cette hauteur) ferait virer
    /// rouges et verts : le streamer encode en BT.601. En 360p, la carte
    /// rend une texture de 368 lignes : le plan de chrominance commence
    /// plus bas qu'à la 360e.
    #[test]
    fn les_couleurs_sont_celles_d_openh264() {
        for (largeur, hauteur) in [(640, 360), (1280, 720)] {
            meme_couleurs(largeur, hauteur);
        }
    }

    fn meme_couleurs(largeur: u32, hauteur: u32) {
        let Some(octets) = flux(largeur, hauteur, 1) else {
            eprintln!("ffmpeg absent : test sauté");
            return;
        };
        let unites = annexb::unites_d_acces(&octets);
        let mut reference = Vec::new();
        let mut ancien = openh264::decoder::Decoder::new().expect("openh264");
        for u in &unites {
            if let Some(image) = ancien.decode(u).expect("openh264") {
                use openh264::formats::YUVSource;
                let (l, h) = image.dimensions();
                let mut rgba = vec![0u8; l * h * 4];
                image.write_rgba8(&mut rgba);
                reference.push(rgba);
            }
        }
        for (nom, mut d) in les_deux() {
            let images: Vec<_> = unites.iter().filter_map(|u| d.decoder(u).expect("décodage")).collect();
            assert!(images.len() + 2 >= reference.len(), "{nom} : {} images", images.len());
            let (mut somme, mut compte) = (0u64, 0u64);
            for (image, attendu) in images.iter().zip(&reference) {
                assert_eq!(image.rgba.len(), attendu.len(), "{nom}");
                somme += image.rgba.iter().zip(attendu).map(|(a, b)| u64::from(a.abs_diff(*b))).sum::<u64>();
                compte += image.rgba.len() as u64;
            }
            // 0,001 mesuré : quelques arrondis, pas une couleur de travers.
            let ecart = somme as f64 / compte as f64;
            assert!(ecart < 0.1, "{nom}, {largeur}×{hauteur} : écart moyen {ecart:.3} sur 255");
        }
    }
}
