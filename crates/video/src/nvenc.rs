//! NVENC : l'encodeur matériel des cartes NVIDIA, sans SDK ni CUDA.
//!
//! `nvEncodeAPI64.dll` vient avec le pilote ; on la charge au vol au premier
//! besoin, on vérifie qu'elle parle au moins la version 12.0 de l'API, et
//! l'on ouvre une session sur un device Direct3D 11 créé sur l'adaptateur
//! NVIDIA — pas sur le premier venu : sur un portable, le premier est
//! souvent l'iGPU Intel. Les images I420 sont copiées dans les tampons
//! d'entrée du pilote, le flux H.264 (Annex B, SPS/PPS répétés à chaque
//! IDR) relu en synchrone. Tout ce qui manque — DLL, carte, pilote trop
//! vieux — se dit en clair, et la boucle retombe sur l'encodeur logiciel.

use std::ffi::{c_void, CStr};
use std::ptr::null_mut;
use std::sync::OnceLock;

use anyhow::{anyhow, bail, Context};
use openh264::formats::YUVSource;
use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

use crate::nvenc_ffi as ffi;
use crate::{Paquet, VideoEncoder};

/// La table des fonctions du pilote, chargée une fois pour le processus.
struct Api {
    fl: ffi::NV_ENCODE_API_FUNCTION_LIST,
    /// Version d'API maximale du pilote, `majeur << 4 | mineur`.
    version: u32,
}

// Des pointeurs de fonctions vers une DLL chargée pour la vie du processus :
// partageables entre fils sans autre précaution.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

static API: OnceLock<Result<Api, String>> = OnceLock::new();

fn api() -> anyhow::Result<&'static Api> {
    match API.get_or_init(charger) {
        Ok(a) => Ok(a),
        Err(e) => Err(anyhow!("{e}")),
    }
}

fn charger() -> Result<Api, String> {
    unsafe {
        let module = LoadLibraryW(windows::core::w!("nvEncodeAPI64.dll"))
            .map_err(|_| "nvEncodeAPI64.dll introuvable — pas de pilote NVIDIA".to_string())?;
        let version_max: ffi::PfnGetMaxSupportedVersion = std::mem::transmute(
            GetProcAddress(module, windows::core::s!("NvEncodeAPIGetMaxSupportedVersion"))
                .ok_or("point d'entrée NvEncodeAPIGetMaxSupportedVersion absent")?,
        );
        let mut version = 0u32;
        let st = version_max(&mut version);
        if st != ffi::NV_ENC_SUCCESS {
            return Err(format!("version du pilote illisible : {}", ffi::status_name(st)));
        }
        let requis = (ffi::NVENCAPI_MAJOR_VERSION << 4) | ffi::NVENCAPI_MINOR_VERSION;
        if version < requis {
            return Err(format!(
                "pilote NVIDIA trop ancien : NVENC {}.{} offert, {}.{} requis — mets à jour le pilote",
                version >> 4,
                version & 0xf,
                ffi::NVENCAPI_MAJOR_VERSION,
                ffi::NVENCAPI_MINOR_VERSION
            ));
        }
        let creer: ffi::PfnCreateInstance = std::mem::transmute(
            GetProcAddress(module, windows::core::s!("NvEncodeAPICreateInstance"))
                .ok_or("point d'entrée NvEncodeAPICreateInstance absent")?,
        );
        let mut fl: ffi::NV_ENCODE_API_FUNCTION_LIST = std::mem::zeroed();
        fl.version = ffi::NV_ENCODE_API_FUNCTION_LIST_VER;
        let st = creer(&mut fl);
        if st != ffi::NV_ENC_SUCCESS {
            return Err(format!("NvEncodeAPICreateInstance : {}", ffi::status_name(st)));
        }
        Ok(Api { fl, version })
    }
}

/// Un device Direct3D 11 sur la première carte NVIDIA matérielle, et son nom.
fn device_nvidia() -> anyhow::Result<(ID3D11Device, String)> {
    unsafe {
        let fabrique: IDXGIFactory1 = CreateDXGIFactory1().context("fabrique DXGI")?;
        let mut i = 0;
        while let Ok(adaptateur) = fabrique.EnumAdapters1(i) {
            i += 1;
            let desc = adaptateur.GetDesc1().context("description d'adaptateur")?;
            let logiciel = desc.Flags & (DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0;
            if desc.VendorId != 0x10DE || logiciel {
                continue;
            }
            let fin = desc.Description.iter().position(|&c| c == 0).unwrap_or(128);
            let nom = String::from_utf16_lossy(&desc.Description[..fin]);
            let base: IDXGIAdapter = adaptateur.cast().context("adaptateur DXGI")?;
            let mut device: Option<ID3D11Device> = None;
            D3D11CreateDevice(
                &base,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(0),
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
            .with_context(|| format!("device Direct3D 11 sur {nom}"))?;
            return Ok((device.context("device Direct3D 11 absent")?, nom));
        }
        bail!("aucune carte NVIDIA")
    }
}

/// Une session d'encodage H.264 sur la carte NVIDIA.
pub struct Nvenc {
    api: &'static Api,
    /// Le device sous la session : il doit lui survivre.
    _device: ID3D11Device,
    session: *mut c_void,
    entree: *mut c_void,
    sortie: *mut c_void,
    width: u32,
    height: u32,
    trame: u64,
    /// Le nom de la carte, pour le journal et l'interface.
    pub carte: String,
}

// La session ne sert que depuis le fil vidéo, mais elle y est créée puis
// détruite sur le même fil : rien ne s'oppose à la déplacer.
unsafe impl Send for Nvenc {}

impl Nvenc {
    /// Ouvre une session pour des images `width`×`height`, à `fps` images
    /// par seconde, en débit constant `bitrate_bps`.
    pub fn new(width: u32, height: u32, bitrate_bps: u32, fps: u32) -> anyhow::Result<Self> {
        let api = api()?;
        let (device, carte) = device_nvidia()?;
        unsafe {
            let ouvrir = api.fl.nvEncOpenEncodeSessionEx.context("nvEncOpenEncodeSessionEx absent")?;
            let mut params: ffi::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS = std::mem::zeroed();
            params.version = ffi::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
            params.deviceType = ffi::NV_ENC_DEVICE_TYPE_DIRECTX;
            params.device = device.as_raw();
            params.apiVersion = ffi::NVENCAPI_VERSION;
            let mut session = null_mut();
            let st = ouvrir(&mut params, &mut session);
            if st != ffi::NV_ENC_SUCCESS {
                bail!("ouverture de session NVENC sur {carte} : {}", ffi::status_name(st));
            }
            let mut moi = Self {
                api,
                _device: device,
                session,
                entree: null_mut(),
                sortie: null_mut(),
                width,
                height,
                trame: 0,
                carte,
            };
            moi.initialiser(bitrate_bps, fps.clamp(1, 120))?;
            Ok(moi)
        }
    }

    fn erreur(&self, st: ffi::NVENCSTATUS, quoi: &str) -> anyhow::Error {
        let detail = self
            .api
            .fl
            .nvEncGetLastErrorString
            .map(|f| unsafe {
                let p = f(self.session);
                if p.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(p).to_string_lossy().into_owned()
                }
            })
            .unwrap_or_default();
        anyhow!("NVENC {quoi} : {} {detail}", ffi::status_name(st))
    }

    fn verif(&self, st: ffi::NVENCSTATUS, quoi: &str) -> anyhow::Result<()> {
        if st == ffi::NV_ENC_SUCCESS {
            Ok(())
        } else {
            Err(self.erreur(st, quoi))
        }
    }

    /// Le préréglage P4 accordé « faible latence », retouché pour un stream :
    /// débit constant tenu à la trame près (VBV d'une image, deux passes en
    /// quart de résolution), pas de trame B ni de réordonnancement, GOP de
    /// deux secondes avec SPS/PPS répétés à chaque IDR pour qui arrive en
    /// cours de route, profil Main — celui que tous les décodeurs lisent.
    unsafe fn initialiser(&mut self, bitrate_bps: u32, fps: u32) -> anyhow::Result<()> {
        let fl = &self.api.fl;
        let preregler = fl.nvEncGetEncodePresetConfigEx.context("nvEncGetEncodePresetConfigEx absent")?;
        let mut preset: Box<ffi::NV_ENC_PRESET_CONFIG> = Box::new(std::mem::zeroed());
        preset.version = ffi::NV_ENC_PRESET_CONFIG_VER;
        preset.presetCfg.version = ffi::NV_ENC_CONFIG_VER;
        self.verif(
            preregler(
                self.session,
                ffi::NV_ENC_CODEC_H264_GUID,
                ffi::NV_ENC_PRESET_P4_GUID,
                ffi::NV_ENC_TUNING_INFO_LOW_LATENCY,
                &mut *preset,
            ),
            "préréglage",
        )?;

        let mut config: Box<ffi::NV_ENC_CONFIG> = Box::new(preset.presetCfg);
        config.version = ffi::NV_ENC_CONFIG_VER;
        config.profileGUID = ffi::NV_ENC_H264_PROFILE_MAIN_GUID;
        config.gopLength = 2 * fps;
        config.frameIntervalP = 1;
        let rc = &mut config.rcParams;
        rc.rateControlMode = ffi::NV_ENC_PARAMS_RC_CBR;
        rc.averageBitRate = bitrate_bps;
        rc.maxBitRate = bitrate_bps;
        rc.vbvBufferSize = bitrate_bps / fps;
        rc.vbvInitialDelay = rc.vbvBufferSize;
        rc.multiPass = ffi::NV_ENC_TWO_PASS_QUARTER_RESOLUTION;
        rc.flags |= ffi::RC_ENABLE_AQ | ffi::RC_ZERO_REORDER_DELAY;
        rc.flags &= !ffi::RC_ENABLE_LOOKAHEAD;
        rc.lookaheadDepth = 0;
        let h264 = &mut config.encodeCodecConfig.h264Config;
        h264.idrPeriod = config.gopLength;
        h264.flags |= ffi::H264_REPEAT_SPSPPS;
        h264.entropyCodingMode = ffi::NV_ENC_H264_ENTROPY_CODING_MODE_CABAC;
        h264.level = ffi::NV_ENC_LEVEL_AUTOSELECT;
        h264.chromaFormatIDC = 1;
        h264.sliceMode = 0;
        h264.sliceModeData = 0;

        let initialiser = fl.nvEncInitializeEncoder.context("nvEncInitializeEncoder absent")?;
        let mut init: Box<ffi::NV_ENC_INITIALIZE_PARAMS> = Box::new(std::mem::zeroed());
        init.version = ffi::NV_ENC_INITIALIZE_PARAMS_VER;
        init.encodeGUID = ffi::NV_ENC_CODEC_H264_GUID;
        init.presetGUID = ffi::NV_ENC_PRESET_P4_GUID;
        init.encodeWidth = self.width;
        init.encodeHeight = self.height;
        init.darWidth = self.width;
        init.darHeight = self.height;
        init.frameRateNum = fps;
        init.frameRateDen = 1;
        init.enableEncodeAsync = 0;
        init.enablePTD = 1;
        init.encodeConfig = &mut *config;
        init.tuningInfo = ffi::NV_ENC_TUNING_INFO_LOW_LATENCY;
        self.verif(initialiser(self.session, &mut *init), "initialisation")?;

        let creer_entree = fl.nvEncCreateInputBuffer.context("nvEncCreateInputBuffer absent")?;
        let mut ci: ffi::NV_ENC_CREATE_INPUT_BUFFER = std::mem::zeroed();
        ci.version = ffi::NV_ENC_CREATE_INPUT_BUFFER_VER;
        ci.width = self.width;
        ci.height = self.height;
        ci.bufferFmt = ffi::NV_ENC_BUFFER_FORMAT_IYUV;
        self.verif(creer_entree(self.session, &mut ci), "tampon d'entrée")?;
        self.entree = ci.inputBuffer;

        let creer_sortie =
            fl.nvEncCreateBitstreamBuffer.context("nvEncCreateBitstreamBuffer absent")?;
        let mut cb: ffi::NV_ENC_CREATE_BITSTREAM_BUFFER = std::mem::zeroed();
        cb.version = ffi::NV_ENC_CREATE_BITSTREAM_BUFFER_VER;
        self.verif(creer_sortie(self.session, &mut cb), "tampon de sortie")?;
        self.sortie = cb.bitstreamBuffer;
        Ok(())
    }

    /// Version d'API du pilote, `majeur.mineur`.
    pub fn version_pilote(&self) -> (u32, u32) {
        (self.api.version >> 4, self.api.version & 0xf)
    }
}

impl VideoEncoder for Nvenc {
    fn nom(&self) -> &'static str {
        "NVENC"
    }

    fn encode(&mut self, src: &dyn YUVSource, force_idr: bool) -> anyhow::Result<Option<Paquet>> {
        let (w, h) = src.dimensions();
        if (w as u32, h as u32) != (self.width, self.height) {
            bail!("NVENC : image {w}x{h} pour une session {}x{}", self.width, self.height);
        }
        let fl = &self.api.fl;
        unsafe {
            // 1. L'image dans le tampon d'entrée du pilote : trois plans,
            //    le luma au pitch donné, les chromas à la moitié.
            let verrouiller = fl.nvEncLockInputBuffer.context("nvEncLockInputBuffer absent")?;
            let mut li: ffi::NV_ENC_LOCK_INPUT_BUFFER = std::mem::zeroed();
            li.version = ffi::NV_ENC_LOCK_INPUT_BUFFER_VER;
            li.inputBuffer = self.entree;
            self.verif(verrouiller(self.session, &mut li), "verrou du tampon d'entrée")?;
            let pitch = li.pitch as usize;
            let dst = li.bufferDataPtr as *mut u8;
            let (ys, us, vs) = src.strides();
            let (y, u, v) = (src.y(), src.u(), src.v());
            for r in 0..h {
                std::ptr::copy_nonoverlapping(y.as_ptr().add(r * ys), dst.add(r * pitch), w);
            }
            let (cw, ch, cp) = (w / 2, h / 2, pitch / 2);
            let base_u = dst.add(pitch * h);
            let base_v = base_u.add(cp * ch);
            for r in 0..ch {
                std::ptr::copy_nonoverlapping(u.as_ptr().add(r * us), base_u.add(r * cp), cw);
                std::ptr::copy_nonoverlapping(v.as_ptr().add(r * vs), base_v.add(r * cp), cw);
            }
            let deverrouiller = fl.nvEncUnlockInputBuffer.context("nvEncUnlockInputBuffer absent")?;
            self.verif(deverrouiller(self.session, self.entree), "libération du tampon d'entrée")?;

            // 2. L'encodage, synchrone : le verrou du flux attend la fin.
            let encoder = fl.nvEncEncodePicture.context("nvEncEncodePicture absent")?;
            let mut pp: ffi::NV_ENC_PIC_PARAMS = std::mem::zeroed();
            pp.version = ffi::NV_ENC_PIC_PARAMS_VER;
            pp.inputWidth = self.width;
            pp.inputHeight = self.height;
            pp.inputPitch = li.pitch;
            pp.encodePicFlags = if force_idr { ffi::NV_ENC_PIC_FLAG_FORCEIDR } else { 0 };
            pp.frameIdx = self.trame as u32;
            pp.inputTimeStamp = self.trame;
            pp.inputBuffer = self.entree;
            pp.outputBitstream = self.sortie;
            pp.bufferFmt = ffi::NV_ENC_BUFFER_FORMAT_IYUV;
            pp.pictureStruct = ffi::NV_ENC_PIC_STRUCT_FRAME;
            self.trame += 1;
            let st = encoder(self.session, &mut pp);
            if st == ffi::NV_ENC_ERR_NEED_MORE_INPUT {
                return Ok(None);
            }
            self.verif(st, "encodage")?;

            // 3. Le flux produit.
            let lire = fl.nvEncLockBitstream.context("nvEncLockBitstream absent")?;
            let mut lb: ffi::NV_ENC_LOCK_BITSTREAM = std::mem::zeroed();
            lb.version = ffi::NV_ENC_LOCK_BITSTREAM_VER;
            lb.outputBitstream = self.sortie;
            self.verif(lire(self.session, &mut lb), "lecture du flux")?;
            let data = std::slice::from_raw_parts(
                lb.bitstreamBufferPtr as *const u8,
                lb.bitstreamSizeInBytes as usize,
            )
            .to_vec();
            let idr = lb.pictureType == ffi::NV_ENC_PIC_TYPE_IDR;
            let relacher = fl.nvEncUnlockBitstream.context("nvEncUnlockBitstream absent")?;
            self.verif(relacher(self.session, self.sortie), "libération du flux")?;
            Ok(Some(Paquet { data, idr }))
        }
    }
}

impl Drop for Nvenc {
    fn drop(&mut self) {
        let fl = &self.api.fl;
        unsafe {
            if !self.entree.is_null() {
                if let Some(f) = fl.nvEncDestroyInputBuffer {
                    f(self.session, self.entree);
                }
            }
            if !self.sortie.is_null() {
                if let Some(f) = fl.nvEncDestroyBitstreamBuffer {
                    f(self.session, self.sortie);
                }
            }
            if let Some(f) = fl.nvEncDestroyEncoder {
                f(self.session);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sur une machine avec une carte NVIDIA, une session s'ouvre, encode
    /// une image et rend une trame clé qui commence par un code de départ
    /// Annex B. Ailleurs, le test constate l'absence et s'en contente —
    /// c'est le comportement voulu en production aussi.
    #[test]
    fn une_session_encode_une_trame_cle_ou_dit_pourquoi_elle_ne_peut_pas() {
        let (w, h) = (640u32, 360u32);
        let mut enc = match Nvenc::new(w, h, 2_000_000, 30) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("NVENC indisponible ici : {e:#}");
                return;
            }
        };
        let (maj, min) = enc.version_pilote();
        eprintln!("NVENC sur {} (API {maj}.{min})", enc.carte);
        let mut img = crate::scale::I420::new(w as usize, h as usize);
        for (i, p) in img.y.iter_mut().enumerate() {
            *p = (i % 251) as u8;
        }
        img.u.fill(100);
        img.v.fill(160);
        let p = enc.encode(&img, true).unwrap().expect("une trame");
        assert!(p.idr, "la première trame est une IDR");
        assert!(p.data.len() > 100, "{} octets", p.data.len());
        assert!(p.data.starts_with(&[0, 0, 0, 1]) || p.data.starts_with(&[0, 0, 1]));
        // Une deuxième, en P cette fois.
        let p2 = enc.encode(&img, false).unwrap().expect("une trame");
        assert!(!p2.idr);
        assert!(p2.data.len() < p.data.len(), "une P d'image fixe est bien plus petite");
    }

    /// Ce que NVENC produit, le décodeur des spectateurs (openh264) doit le
    /// lire — profil Main, CABAC, SPS/PPS en tête de chaque IDR. Et l'on
    /// mesure au passage ce que coûte une image 1080p, aller et retour.
    #[test]
    fn le_flux_nvenc_se_decode_avec_openh264() {
        let (w, h) = (1920u32, 1080u32);
        let Ok(mut enc) = Nvenc::new(w, h, 6_000_000, 30) else { return };
        let mut dec = openh264::decoder::Decoder::new().unwrap();
        let mut img = crate::scale::I420::new(w as usize, h as usize);
        img.u.fill(128);
        img.v.fill(128);
        let debut = std::time::Instant::now();
        let mut encodage = std::time::Duration::ZERO;
        let mut decodees = 0;
        let mut octets = 0usize;
        for i in 0..30u8 {
            // Du contenu qui bouge : un dégradé qui glisse d'une image à
            // l'autre, pour que les P aient quelque chose à prédire.
            for (x, p) in img.y.iter_mut().enumerate() {
                *p = ((x % 1920) as u32 / 8 + i as u32 * 3) as u8;
            }
            let t = std::time::Instant::now();
            let p = enc.encode(&img, i == 0).unwrap().expect("une trame");
            encodage += t.elapsed();
            octets += p.data.len();
            if let Ok(Some(image)) = dec.decode(&p.data) {
                assert_eq!(image.dimensions(), (w as usize, h as usize));
                decodees += 1;
            }
        }
        eprintln!(
            "NVENC 1080p : encodage {:.2} ms/image, aller-retour avec openh264 {:.2} ms/image, \
             {} kbit/s à 30 i/s",
            encodage.as_secs_f32() * 1000.0 / 30.0,
            debut.elapsed().as_secs_f32() * 1000.0 / 30.0,
            octets * 8 / 1000
        );
        assert!(decodees >= 28, "{decodees} images décodées sur 30");
    }
}
