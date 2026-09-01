//! Le strict nécessaire de `nvEncodeAPI.h` (SDK vidéo NVIDIA 12.0, tel que
//! redistribué sous licence MIT dans les `nv-codec-headers` de FFmpeg) pour
//! ouvrir une session H.264, y pousser des images I420 et relire le flux.
//!
//! Traduit à la main, champ par champ, à partir de l'en-tête — pas de
//! bindgen, pas de SDK à installer : la DLL vient avec le pilote et se
//! charge au vol (voir `nvenc.rs`). Les groupes de champs de bits deviennent
//! un `u32` chacun (l'en-tête complète toujours à 32 bits), les unions dont
//! on ne touche qu'un membre deviennent ce membre, complété à la taille de
//! l'union. Chaque taille est vérifiée à la compilation contre le calcul
//! fait sur l'en-tête : une erreur de traduction ne passe pas.
//!
//! Version 12.0 volontairement, pas la dernière : un pilote de fin 2022
//! suffit, et les pilotes plus récents servent les vieilles structures.

#![allow(non_camel_case_types, non_snake_case, dead_code, clippy::upper_case_acronyms)]

use std::ffi::{c_char, c_void};

pub const NVENCAPI_MAJOR_VERSION: u32 = 12;
pub const NVENCAPI_MINOR_VERSION: u32 = 0;
pub const NVENCAPI_VERSION: u32 = NVENCAPI_MAJOR_VERSION | (NVENCAPI_MINOR_VERSION << 24);

const fn struct_version(ver: u32) -> u32 {
    NVENCAPI_VERSION | (ver << 16) | (0x7 << 28)
}

pub const NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER: u32 = struct_version(1);
pub const NV_ENC_INITIALIZE_PARAMS_VER: u32 = struct_version(5) | (1 << 31);
pub const NV_ENC_CONFIG_VER: u32 = struct_version(8) | (1 << 31);
pub const NV_ENC_PRESET_CONFIG_VER: u32 = struct_version(4) | (1 << 31);
pub const NV_ENC_PIC_PARAMS_VER: u32 = struct_version(6) | (1 << 31);
pub const NV_ENC_RC_PARAMS_VER: u32 = struct_version(1);
pub const NV_ENC_CREATE_INPUT_BUFFER_VER: u32 = struct_version(1);
pub const NV_ENC_CREATE_BITSTREAM_BUFFER_VER: u32 = struct_version(1);
pub const NV_ENC_LOCK_INPUT_BUFFER_VER: u32 = struct_version(1);
pub const NV_ENC_LOCK_BITSTREAM_VER: u32 = struct_version(2);
pub const NV_ENCODE_API_FUNCTION_LIST_VER: u32 = struct_version(2);

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GUID {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

const fn guid(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> GUID {
    GUID { data1, data2, data3, data4 }
}

pub const NV_ENC_CODEC_H264_GUID: GUID =
    guid(0x6bc82762, 0x4e63, 0x4ca4, [0xaa, 0x85, 0x1e, 0x50, 0xf3, 0x21, 0xf6, 0xbf]);
pub const NV_ENC_H264_PROFILE_MAIN_GUID: GUID =
    guid(0x60b5c1d4, 0x67fe, 0x4790, [0x94, 0xd5, 0xc4, 0x72, 0x6d, 0x7b, 0x6e, 0x6d]);
pub const NV_ENC_H264_PROFILE_HIGH_GUID: GUID =
    guid(0xe7cbc309, 0x4f7a, 0x4b89, [0xaf, 0x2a, 0xd5, 0x37, 0xc9, 0x2b, 0xe3, 0x10]);
pub const NV_ENC_PRESET_P3_GUID: GUID =
    guid(0x36850110, 0x3a07, 0x441f, [0x94, 0xd5, 0x36, 0x70, 0x63, 0x1f, 0x91, 0xf6]);
pub const NV_ENC_PRESET_P4_GUID: GUID =
    guid(0x90a7b826, 0xdf06, 0x4862, [0xb9, 0xd2, 0xcd, 0x6d, 0x73, 0xa0, 0x86, 0x81]);

pub type NVENCSTATUS = u32;
pub const NV_ENC_SUCCESS: NVENCSTATUS = 0;
pub const NV_ENC_ERR_NEED_MORE_INPUT: NVENCSTATUS = 17;

/// Le nom d'un code de retour, pour les journaux.
pub fn status_name(st: NVENCSTATUS) -> &'static str {
    const NOMS: [&str; 26] = [
        "NV_ENC_SUCCESS",
        "NV_ENC_ERR_NO_ENCODE_DEVICE",
        "NV_ENC_ERR_UNSUPPORTED_DEVICE",
        "NV_ENC_ERR_INVALID_ENCODERDEVICE",
        "NV_ENC_ERR_INVALID_DEVICE",
        "NV_ENC_ERR_DEVICE_NOT_EXIST",
        "NV_ENC_ERR_INVALID_PTR",
        "NV_ENC_ERR_INVALID_EVENT",
        "NV_ENC_ERR_INVALID_PARAM",
        "NV_ENC_ERR_INVALID_CALL",
        "NV_ENC_ERR_OUT_OF_MEMORY",
        "NV_ENC_ERR_ENCODER_NOT_INITIALIZED",
        "NV_ENC_ERR_UNSUPPORTED_PARAM",
        "NV_ENC_ERR_LOCK_BUSY",
        "NV_ENC_ERR_NOT_ENOUGH_BUFFER",
        "NV_ENC_ERR_INVALID_VERSION",
        "NV_ENC_ERR_MAP_FAILED",
        "NV_ENC_ERR_NEED_MORE_INPUT",
        "NV_ENC_ERR_ENCODER_BUSY",
        "NV_ENC_ERR_EVENT_NOT_REGISTERD",
        "NV_ENC_ERR_GENERIC",
        "NV_ENC_ERR_INCOMPATIBLE_CLIENT_KEY",
        "NV_ENC_ERR_UNIMPLEMENTED",
        "NV_ENC_ERR_RESOURCE_REGISTER_FAILED",
        "NV_ENC_ERR_RESOURCE_NOT_REGISTERED",
        "NV_ENC_ERR_RESOURCE_NOT_MAPPED",
    ];
    NOMS.get(st as usize).copied().unwrap_or("NV_ENC_ERR_?")
}

pub const NV_ENC_DEVICE_TYPE_DIRECTX: u32 = 0;
pub const NV_ENC_BUFFER_FORMAT_IYUV: u32 = 0x100;
pub const NV_ENC_PIC_STRUCT_FRAME: u32 = 1;
pub const NV_ENC_PIC_TYPE_I: u32 = 2;
pub const NV_ENC_PIC_TYPE_IDR: u32 = 3;
pub const NV_ENC_PARAMS_RC_CBR: u32 = 2;
pub const NV_ENC_TWO_PASS_QUARTER_RESOLUTION: u32 = 1;
pub const NV_ENC_TUNING_INFO_LOW_LATENCY: u32 = 2;
pub const NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY: u32 = 3;
pub const NV_ENC_PIC_FLAG_FORCEIDR: u32 = 2;
pub const NV_ENC_H264_ENTROPY_CODING_MODE_CABAC: u32 = 1;
pub const NV_ENC_LEVEL_AUTOSELECT: u32 = 0;

// Champs de bits de NV_ENC_RC_PARAMS::flags.
pub const RC_ENABLE_AQ: u32 = 1 << 3;
pub const RC_ENABLE_LOOKAHEAD: u32 = 1 << 5;
pub const RC_ZERO_REORDER_DELAY: u32 = 1 << 9;
// Champs de bits de NV_ENC_CONFIG_H264::flags.
pub const H264_REPEAT_SPSPPS: u32 = 1 << 12;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_QP {
    pub qpInterP: u32,
    pub qpInterB: u32,
    pub qpIntra: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_RC_PARAMS {
    pub version: u32,
    pub rateControlMode: u32,
    pub constQP: NV_ENC_QP,
    pub averageBitRate: u32,
    pub maxBitRate: u32,
    pub vbvBufferSize: u32,
    pub vbvInitialDelay: u32,
    /// enableMinQP:1, enableMaxQP:1, enableInitialRCQP:1, enableAQ:1,
    /// réservé:1, enableLookahead:1, disableIadapt:1, disableBadapt:1,
    /// enableTemporalAQ:1, zeroReorderDelay:1, enableNonRefP:1,
    /// strictGOPTarget:1, aqStrength:4, réservé:16.
    pub flags: u32,
    pub minQP: NV_ENC_QP,
    pub maxQP: NV_ENC_QP,
    pub initialRCQP: NV_ENC_QP,
    pub temporallayerIdxMask: u32,
    pub temporalLayerQP: [u8; 8],
    pub targetQuality: u8,
    pub targetQualityLSB: u8,
    pub lookaheadDepth: u16,
    pub lowDelayKeyFrameScale: u8,
    pub yDcQPIndexOffset: i8,
    pub uDcQPIndexOffset: i8,
    pub vDcQPIndexOffset: i8,
    pub qpMapMode: u32,
    pub multiPass: u32,
    pub alphaLayerBitrateRatio: u32,
    pub cbQPIndexOffset: i8,
    pub crQPIndexOffset: i8,
    pub reserved2: u16,
    pub reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_CONFIG_H264_VUI_PARAMETERS {
    pub overscanInfoPresentFlag: u32,
    pub overscanInfo: u32,
    pub videoSignalTypePresentFlag: u32,
    pub videoFormat: u32,
    pub videoFullRangeFlag: u32,
    pub colourDescriptionPresentFlag: u32,
    pub colourPrimaries: u32,
    pub transferCharacteristics: u32,
    pub colourMatrix: u32,
    pub chromaSampleLocationFlag: u32,
    pub chromaSampleLocationTop: u32,
    pub chromaSampleLocationBot: u32,
    pub bitstreamRestrictionFlag: u32,
    pub timingInfoPresentFlag: u32,
    pub numUnitInTicks: u32,
    pub timeScale: u32,
    pub reserved: [u32; 12],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_CONFIG_H264 {
    /// enableTemporalSVC:1, enableStereoMVC:1, hierarchicalPFrames:1,
    /// hierarchicalBFrames:1, outputBufferingPeriodSEI:1,
    /// outputPictureTimingSEI:1, outputAUD:1, disableSPSPPS:1,
    /// outputFramePackingSEI:1, outputRecoveryPointSEI:1,
    /// enableIntraRefresh:1, enableConstrainedEncoding:1, repeatSPSPPS:1,
    /// enableVFR:1, enableLTR:1, qpPrimeYZeroTransformBypassFlag:1,
    /// useConstrainedIntraPred:1, enableFillerDataInsertion:1,
    /// disableSVCPrefixNalu:1, enableScalabilityInfoSEI:1,
    /// singleSliceIntraRefresh:1, enableTimeCode:1, réservé:10.
    pub flags: u32,
    pub level: u32,
    pub idrPeriod: u32,
    pub separateColourPlaneFlag: u32,
    pub disableDeblockingFilterIDC: u32,
    pub numTemporalLayers: u32,
    pub spsId: u32,
    pub ppsId: u32,
    pub adaptiveTransformMode: u32,
    pub fmoMode: u32,
    pub bdirectMode: u32,
    pub entropyCodingMode: u32,
    pub stereoMode: u32,
    pub intraRefreshPeriod: u32,
    pub intraRefreshCnt: u32,
    pub maxNumRefFrames: u32,
    pub sliceMode: u32,
    pub sliceModeData: u32,
    pub h264VUIParameters: NV_ENC_CONFIG_H264_VUI_PARAMETERS,
    pub ltrNumFrames: u32,
    pub ltrTrustMode: u32,
    pub chromaFormatIDC: u32,
    pub maxTemporalLayers: u32,
    pub useBFramesAsRef: u32,
    pub numRefL0: u32,
    pub numRefL1: u32,
    pub reserved1: [u32; 267],
    pub reserved2: [*mut c_void; 64],
}

/// L'union des configurations par codec : H.264 en est le plus grand
/// membre (1792 octets), la taille est donc la sienne.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_CODEC_CONFIG {
    pub h264Config: NV_ENC_CONFIG_H264,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_CONFIG {
    pub version: u32,
    pub profileGUID: GUID,
    pub gopLength: u32,
    pub frameIntervalP: i32,
    pub monoChromeEncoding: u32,
    pub frameFieldMode: u32,
    pub mvPrecision: u32,
    pub rcParams: NV_ENC_RC_PARAMS,
    pub encodeCodecConfig: NV_ENC_CODEC_CONFIG,
    pub reserved: [u32; 278],
    pub reserved2: [*mut c_void; 64],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_PRESET_CONFIG {
    pub version: u32,
    pub presetCfg: NV_ENC_CONFIG,
    pub reserved1: [u32; 255],
    pub reserved2: [*mut c_void; 64],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NVENC_EXTERNAL_ME_HINT_COUNTS_PER_BLOCKTYPE {
    pub flags: u32,
    pub reserved1: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_INITIALIZE_PARAMS {
    pub version: u32,
    pub encodeGUID: GUID,
    pub presetGUID: GUID,
    pub encodeWidth: u32,
    pub encodeHeight: u32,
    pub darWidth: u32,
    pub darHeight: u32,
    pub frameRateNum: u32,
    pub frameRateDen: u32,
    pub enableEncodeAsync: u32,
    pub enablePTD: u32,
    /// reportSliceOffsets:1, enableSubFrameWrite:1, enableExternalMEHints:1,
    /// enableMEOnlyMode:1, enableWeightedPrediction:1,
    /// enableOutputInVidmem:1, réservé:26.
    pub flags: u32,
    pub privDataSize: u32,
    pub privData: *mut c_void,
    pub encodeConfig: *mut NV_ENC_CONFIG,
    pub maxEncodeWidth: u32,
    pub maxEncodeHeight: u32,
    pub maxMEHintCountsPerBlock: [NVENC_EXTERNAL_ME_HINT_COUNTS_PER_BLOCKTYPE; 2],
    pub tuningInfo: u32,
    pub bufferFormat: u32,
    pub reserved: [u32; 287],
    pub reserved2: [*mut c_void; 64],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_CREATE_INPUT_BUFFER {
    pub version: u32,
    pub width: u32,
    pub height: u32,
    pub memoryHeap: u32,
    pub bufferFmt: u32,
    pub reserved: u32,
    pub inputBuffer: *mut c_void,
    pub pSysMemBuffer: *mut c_void,
    pub reserved1: [u32; 57],
    pub reserved2: [*mut c_void; 63],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_CREATE_BITSTREAM_BUFFER {
    pub version: u32,
    pub size: u32,
    pub memoryHeap: u32,
    pub reserved: u32,
    pub bitstreamBuffer: *mut c_void,
    pub bitstreamBufferPtr: *mut c_void,
    pub reserved1: [u32; 58],
    pub reserved2: [*mut c_void; 64],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_LOCK_INPUT_BUFFER {
    pub version: u32,
    /// doNotWait:1, réservé:31.
    pub flags: u32,
    pub inputBuffer: *mut c_void,
    pub bufferDataPtr: *mut c_void,
    pub pitch: u32,
    pub reserved1: [u32; 251],
    pub reserved2: [*mut c_void; 64],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_LOCK_BITSTREAM {
    pub version: u32,
    /// doNotWait:1, ltrFrame:1, getRCStats:1, réservé:29.
    pub flags: u32,
    pub outputBitstream: *mut c_void,
    pub sliceOffsets: *mut u32,
    pub frameIdx: u32,
    pub hwEncodeStatus: u32,
    pub numSlices: u32,
    pub bitstreamSizeInBytes: u32,
    pub outputTimeStamp: u64,
    pub outputDuration: u64,
    pub bitstreamBufferPtr: *mut c_void,
    pub pictureType: u32,
    pub pictureStruct: u32,
    pub frameAvgQP: u32,
    pub frameSatd: u32,
    pub ltrFrameIdx: u32,
    pub ltrFrameBitmap: u32,
    pub temporalId: u32,
    pub reserved: [u32; 12],
    pub intraMBCount: u32,
    pub interMBCount: u32,
    pub averageMVX: i32,
    pub averageMVY: i32,
    pub alphaLayerSizeInBytes: u32,
    pub reserved1: [u32; 218],
    pub reserved2: [*mut c_void; 64],
}

/// L'union des paramètres d'image par codec — on n'y touche pas (la
/// décision de type d'image est laissée à l'encodeur), seule sa taille
/// compte : 1552 octets, celle du membre AV1, alignée sur 8.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_CODEC_PIC_PARAMS {
    pub reserved: [u64; 194],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_PIC_PARAMS {
    pub version: u32,
    pub inputWidth: u32,
    pub inputHeight: u32,
    pub inputPitch: u32,
    pub encodePicFlags: u32,
    pub frameIdx: u32,
    pub inputTimeStamp: u64,
    pub inputDuration: u64,
    pub inputBuffer: *mut c_void,
    pub outputBitstream: *mut c_void,
    pub completionEvent: *mut c_void,
    pub bufferFmt: u32,
    pub pictureStruct: u32,
    pub pictureType: u32,
    pub codecPicParams: NV_ENC_CODEC_PIC_PARAMS,
    pub meHintCountsPerBlock: [NVENC_EXTERNAL_ME_HINT_COUNTS_PER_BLOCKTYPE; 2],
    pub meExternalHints: *mut c_void,
    pub reserved1: [u32; 6],
    pub reserved2: [*mut c_void; 2],
    pub qpDeltaMap: *mut i8,
    pub qpDeltaMapSize: u32,
    pub reservedBitFields: u32,
    pub meHintRefPicDist: [u16; 2],
    pub alphaBuffer: *mut c_void,
    pub meExternalSbHints: *mut c_void,
    pub meSbHintsCount: u32,
    pub reserved3: [u32; 285],
    pub reserved4: [*mut c_void; 58],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS {
    pub version: u32,
    pub deviceType: u32,
    pub device: *mut c_void,
    pub reserved: *mut c_void,
    pub apiVersion: u32,
    pub reserved1: [u32; 253],
    pub reserved2: [*mut c_void; 64],
}

pub type PfnOpenEncodeSessionEx = unsafe extern "system" fn(
    *mut NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS,
    *mut *mut c_void,
) -> NVENCSTATUS;
pub type PfnGetEncodePresetConfigEx =
    unsafe extern "system" fn(*mut c_void, GUID, GUID, u32, *mut NV_ENC_PRESET_CONFIG) -> NVENCSTATUS;
pub type PfnInitializeEncoder =
    unsafe extern "system" fn(*mut c_void, *mut NV_ENC_INITIALIZE_PARAMS) -> NVENCSTATUS;
pub type PfnCreateInputBuffer =
    unsafe extern "system" fn(*mut c_void, *mut NV_ENC_CREATE_INPUT_BUFFER) -> NVENCSTATUS;
pub type PfnCreateBitstreamBuffer =
    unsafe extern "system" fn(*mut c_void, *mut NV_ENC_CREATE_BITSTREAM_BUFFER) -> NVENCSTATUS;
pub type PfnEncodePicture =
    unsafe extern "system" fn(*mut c_void, *mut NV_ENC_PIC_PARAMS) -> NVENCSTATUS;
pub type PfnLockBitstream =
    unsafe extern "system" fn(*mut c_void, *mut NV_ENC_LOCK_BITSTREAM) -> NVENCSTATUS;
pub type PfnLockInputBuffer =
    unsafe extern "system" fn(*mut c_void, *mut NV_ENC_LOCK_INPUT_BUFFER) -> NVENCSTATUS;
/// Détruire un tampon, déverrouiller un tampon : (session, tampon).
pub type PfnSessionPtr = unsafe extern "system" fn(*mut c_void, *mut c_void) -> NVENCSTATUS;
pub type PfnDestroyEncoder = unsafe extern "system" fn(*mut c_void) -> NVENCSTATUS;
pub type PfnGetLastErrorString = unsafe extern "system" fn(*mut c_void) -> *const c_char;

/// La table des fonctions, remplie par `NvEncodeAPICreateInstance`. Seules
/// celles que l'on appelle sont typées ; les autres restent des pointeurs
/// opaques, à la bonne place.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENCODE_API_FUNCTION_LIST {
    pub version: u32,
    pub reserved: u32,
    pub nvEncOpenEncodeSession: *const c_void,
    pub nvEncGetEncodeGUIDCount: *const c_void,
    pub nvEncGetEncodeProfileGUIDCount: *const c_void,
    pub nvEncGetEncodeProfileGUIDs: *const c_void,
    pub nvEncGetEncodeGUIDs: *const c_void,
    pub nvEncGetInputFormatCount: *const c_void,
    pub nvEncGetInputFormats: *const c_void,
    pub nvEncGetEncodeCaps: *const c_void,
    pub nvEncGetEncodePresetCount: *const c_void,
    pub nvEncGetEncodePresetGUIDs: *const c_void,
    pub nvEncGetEncodePresetConfig: *const c_void,
    pub nvEncInitializeEncoder: Option<PfnInitializeEncoder>,
    pub nvEncCreateInputBuffer: Option<PfnCreateInputBuffer>,
    pub nvEncDestroyInputBuffer: Option<PfnSessionPtr>,
    pub nvEncCreateBitstreamBuffer: Option<PfnCreateBitstreamBuffer>,
    pub nvEncDestroyBitstreamBuffer: Option<PfnSessionPtr>,
    pub nvEncEncodePicture: Option<PfnEncodePicture>,
    pub nvEncLockBitstream: Option<PfnLockBitstream>,
    pub nvEncUnlockBitstream: Option<PfnSessionPtr>,
    pub nvEncLockInputBuffer: Option<PfnLockInputBuffer>,
    pub nvEncUnlockInputBuffer: Option<PfnSessionPtr>,
    pub nvEncGetEncodeStats: *const c_void,
    pub nvEncGetSequenceParams: *const c_void,
    pub nvEncRegisterAsyncEvent: *const c_void,
    pub nvEncUnregisterAsyncEvent: *const c_void,
    pub nvEncMapInputResource: *const c_void,
    pub nvEncUnmapInputResource: *const c_void,
    pub nvEncDestroyEncoder: Option<PfnDestroyEncoder>,
    pub nvEncInvalidateRefFrames: *const c_void,
    pub nvEncOpenEncodeSessionEx: Option<PfnOpenEncodeSessionEx>,
    pub nvEncRegisterResource: *const c_void,
    pub nvEncUnregisterResource: *const c_void,
    pub nvEncReconfigureEncoder: *const c_void,
    pub reserved1: *const c_void,
    pub nvEncCreateMVBuffer: *const c_void,
    pub nvEncDestroyMVBuffer: *const c_void,
    pub nvEncRunMotionEstimationOnly: *const c_void,
    pub nvEncGetLastErrorString: Option<PfnGetLastErrorString>,
    pub nvEncSetIOCudaStreams: *const c_void,
    pub nvEncGetEncodePresetConfigEx: Option<PfnGetEncodePresetConfigEx>,
    pub nvEncGetSequenceParamEx: *const c_void,
    pub reserved2: [*const c_void; 277],
}

pub type PfnCreateInstance =
    unsafe extern "system" fn(*mut NV_ENCODE_API_FUNCTION_LIST) -> NVENCSTATUS;
pub type PfnGetMaxSupportedVersion = unsafe extern "system" fn(*mut u32) -> NVENCSTATUS;

// Les tailles, calculées sur l'en-tête (x86-64 : u32 = 4, pointeurs et u64
// = 8, alignés sur leur taille). Une traduction fausse d'un seul champ
// décale tout ce qui suit — d'où ces gardes.
const _: () = assert!(std::mem::size_of::<GUID>() == 16);
const _: () = assert!(std::mem::size_of::<NV_ENC_RC_PARAMS>() == 128);
const _: () = assert!(std::mem::size_of::<NV_ENC_CONFIG_H264_VUI_PARAMETERS>() == 112);
const _: () = assert!(std::mem::size_of::<NV_ENC_CONFIG_H264>() == 1792);
const _: () = assert!(std::mem::size_of::<NV_ENC_CONFIG>() == 3584);
const _: () = assert!(std::mem::size_of::<NV_ENC_PRESET_CONFIG>() == 5128);
const _: () = assert!(std::mem::size_of::<NV_ENC_INITIALIZE_PARAMS>() == 1808);
const _: () = assert!(std::mem::size_of::<NV_ENC_CREATE_INPUT_BUFFER>() == 776);
const _: () = assert!(std::mem::size_of::<NV_ENC_CREATE_BITSTREAM_BUFFER>() == 776);
const _: () = assert!(std::mem::size_of::<NV_ENC_LOCK_INPUT_BUFFER>() == 1544);
const _: () = assert!(std::mem::size_of::<NV_ENC_LOCK_BITSTREAM>() == 1544);
const _: () = assert!(std::mem::size_of::<NV_ENC_PIC_PARAMS>() == 3360);
const _: () = assert!(std::mem::size_of::<NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS>() == 1552);
const _: () = assert!(std::mem::size_of::<NV_ENCODE_API_FUNCTION_LIST>() == 2552);
