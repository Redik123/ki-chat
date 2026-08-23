//! Moteur audio Windows natif (WASAPI direct), sans passer par cpal.
//!
//! Pourquoi doubler cpal ? Parce qu'il ouvre les flux comme un lecteur de
//! musique, et qu'un client vocal a besoin de quatre choses que cpal ne
//! demande jamais à Windows — celles-là mêmes qui rendent Discord insensible
//! aux jeux qui rebrassent l'audio :
//!
//! - le périphérique de communication par défaut (rôle eCommunications),
//!   et non celui des jeux (eConsole) ;
//! - la conversion de format automatique (AUTOCONVERTPCM) : un jeu qui fait
//!   basculer le périphérique de 44,1 à 48 kHz ne tue plus le flux ;
//! - la catégorie « Communications » **nulle part par défaut**, leçon chère
//!   du terrain : toute session déclarée communications — capture COMME
//!   lecture — peut déclencher une baisse du volume des autres sons chez
//!   l'utilisateur (l'atténuation « activité de communication » de Windows,
//!   ou le « ChatMix » du DSP du casque, insensible aux réglages Windows).
//!   La capture y bascule uniquement en escalade anti-famine : quand la
//!   voix intégrée d'un jeu tient la voie de traitement du micro et que la
//!   nôtre s'ouvre sans jamais être servie, demander la même voie la
//!   partage. Les rares concernés règlent alors Panneau son →
//!   Communication → « Ne rien faire » ;
//! - le mode brut en option sur le micro : court-circuite les chaînes
//!   d'effets tiers (Sonar, Nahimic, Synapse…), grandes pourvoyeuses de
//!   micros zombies au lancement d'un jeu.
//!
//! S'y ajoutent les notifications de périphériques (IMMNotificationClient) :
//! un changement déclenche la sonde d'empreinte en ~300 ms au lieu de 3 s.
//!
//! Le module ne remplace pas les boucles de surveillance de lib.rs : il
//! fournit des ouvertures de flux au même contrat que celles de cpal, et
//! lib.rs garde ses garde-fous (empreinte, watchdogs, zombie à zéros).
//! Toute erreur d'ouverture fait retomber lib.rs sur cpal : au pire, on se
//! comporte comme avant.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context};
use windows::core::{implement, PCWSTR};
use windows::Win32::Devices::Properties::DEVPKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, HANDLE, PROPERTYKEY, RPC_E_CHANGED_MODE};
use windows::Win32::Media::Audio::{
    eCapture, eCommunications, eRender, AudioCategory_Communications, AudioCategory_Other,
    EDataFlow, ERole, IAudioCaptureClient, IAudioClient2, IAudioRenderClient, IMMDevice,
    IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator,
    AudioClientProperties, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, AUDCLNT_STREAMOPTIONS_NONE,
    AUDCLNT_STREAMOPTIONS_RAW, DEVICE_STATE, DEVICE_STATE_ACTIVE, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE, WAVE_FORMAT_PCM,
};
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::{journal, normalized_device_name, SAMPLE_RATE};

// ---------------------------------------------------------------------------
// COM
// ---------------------------------------------------------------------------

/// COM initialisé (MTA) sur ce fil, une fois pour toutes. Le MTA d'abord :
/// si cpal passe ensuite sur le même fil, son STA échoue en RPC_E_CHANGED_MODE
/// qu'il tolère — l'inverse (STA d'abord) lierait nos objets à un fil qui ne
/// pompe pas de messages.
fn ensure_com() {
    thread_local! {
        static DONE: () = {
            unsafe {
                let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
                if hr.is_err() && hr != RPC_E_CHANGED_MODE {
                    tracing::warn!("CoInitializeEx : {hr:?}");
                }
            }
        };
    }
    DONE.with(|_| {});
}

fn enumerator() -> anyhow::Result<IMMDeviceEnumerator> {
    ensure_com();
    unsafe {
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .context("création de l'énumérateur de périphériques")
    }
}

// ---------------------------------------------------------------------------
// Notifications de périphériques
// ---------------------------------------------------------------------------

/// Génération du parc de périphériques : incrémentée à chaque événement
/// Windows (branchement, retrait, changement de défaut ou de propriété).
/// Les boucles de lib.rs la comparent pour avancer leur sonde d'empreinte.
static GENERATION: AtomicU64 = AtomicU64::new(1);

pub fn device_generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

fn bump() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

#[implement(IMMNotificationClient)]
struct DeviceEvents;

/// Chaque événement se contente d'incrémenter la génération : le rappel
/// arrive sur un fil COM, on n'y fait rien de plus qu'un atomique.
impl IMMNotificationClient_Impl for DeviceEvents_Impl {
    fn OnDeviceStateChanged(
        &self,
        _id: &PCWSTR,
        _state: DEVICE_STATE,
    ) -> windows::core::Result<()> {
        bump();
        Ok(())
    }
    fn OnDeviceAdded(&self, _id: &PCWSTR) -> windows::core::Result<()> {
        bump();
        Ok(())
    }
    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> windows::core::Result<()> {
        bump();
        Ok(())
    }
    fn OnDefaultDeviceChanged(
        &self,
        _flow: EDataFlow,
        _role: ERole,
        _id: &PCWSTR,
    ) -> windows::core::Result<()> {
        bump();
        Ok(())
    }
    fn OnPropertyValueChanged(
        &self,
        _id: &PCWSTR,
        _key: &PROPERTYKEY,
    ) -> windows::core::Result<()> {
        bump();
        Ok(())
    }
}

static NOTIFICATIONS: OnceLock<()> = OnceLock::new();

/// Branche l'écoute des événements de périphériques, une fois par processus.
///
/// Sur un fil dédié, initialisé MTA et parké pour toujours : l'enregistrement
/// depuis un fil STA sans pompe de messages ne livrerait jamais de rappels,
/// et l'apartment doit survivre à l'enregistreur.
pub fn ensure_notifications() {
    NOTIFICATIONS.get_or_init(|| {
        let spawned = std::thread::Builder::new()
            .name("audio-events".into())
            .spawn(|| unsafe {
                let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
                if hr.is_err() && hr != RPC_E_CHANGED_MODE {
                    tracing::warn!("notifications audio : CoInitializeEx {hr:?}");
                    return;
                }
                let Ok(enu) =
                    CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
                else {
                    tracing::warn!("notifications audio : énumérateur indisponible");
                    return;
                };
                let events: IMMNotificationClient = DeviceEvents.into();
                if let Err(e) = enu.RegisterEndpointNotificationCallback(&events) {
                    tracing::warn!("notifications audio : enregistrement refusé ({e})");
                    return;
                }
                tracing::debug!("notifications de périphériques audio actives");
                // L'énumérateur et le client doivent rester vivants : le fil
                // les garde et dort. La sonde périodique reste le filet si ce
                // fil venait à mourir.
                loop {
                    std::thread::park();
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("fil de notifications audio impossible à créer : {e}");
        }
    });
}

// ---------------------------------------------------------------------------
// Périphériques
// ---------------------------------------------------------------------------

/// Nom convivial d'un point de terminaison (le même que celui de cpal, les
/// réglages enregistrés restent donc valables d'un moteur à l'autre).
fn friendly_name(device: &IMMDevice) -> anyhow::Result<String> {
    unsafe {
        let store = device
            .OpenPropertyStore(STGM_READ)
            .context("ouverture du magasin de propriétés")?;
        let value = store
            .GetValue(&DEVPKEY_Device_FriendlyName as *const _ as *const PROPERTYKEY)
            .context("lecture du nom du périphérique")?;
        // L'affichage du PROPVARIANT fait la conversion (VT_LPWSTR -> texte) ;
        // un nom vide n'est d'aucune utilité pour la correspondance.
        let name = value.to_string();
        if name.is_empty() {
            bail!("nom de périphérique vide ou illisible");
        }
        Ok(name)
    }
}

/// Identifiant de point de terminaison Windows : change quand l'USB est
/// ré-énuméré — exactement ce qu'on veut voir dans l'empreinte.
fn endpoint_id(device: &IMMDevice) -> anyhow::Result<String> {
    unsafe {
        let pw = device.GetId().context("GetId")?;
        let id = pw.to_string().unwrap_or_default();
        CoTaskMemFree(Some(pw.as_ptr() as *const _));
        Ok(id)
    }
}

/// Tous les périphériques actifs d'un sens, avec leur nom convivial.
fn active_devices(
    enu: &IMMDeviceEnumerator,
    input: bool,
) -> anyhow::Result<Vec<(IMMDevice, String)>> {
    unsafe {
        let flow = if input { eCapture } else { eRender };
        let collection = enu
            .EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)
            .context("énumération des périphériques")?;
        let count = collection.GetCount().context("GetCount")?;
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let Ok(dev) = collection.Item(i) else { continue };
            let Ok(name) = friendly_name(&dev) else { continue };
            out.push((dev, name));
        }
        Ok(out)
    }
}

/// Le périphérique demandé (exact, puis sans compteur de ré-énumération),
/// ou le défaut **de communication** — c'est le rôle que suivent les apps
/// d'appel, Discord en tête ; cpal suivait celui des jeux.
fn pick(
    enu: &IMMDeviceEnumerator,
    name: Option<&str>,
    input: bool,
) -> anyhow::Result<(IMMDevice, bool)> {
    let flow = if input { eCapture } else { eRender };
    let default = || unsafe {
        enu.GetDefaultAudioEndpoint(flow, eCommunications)
            .context("aucun périphérique par défaut")
    };
    let Some(wanted) = name else {
        return Ok((default()?, false));
    };
    let devices = active_devices(enu, input)?;
    if let Some((d, _)) = devices.iter().find(|(_, n)| n == wanted) {
        return Ok((d.clone(), false));
    }
    let want = normalized_device_name(wanted);
    if let Some((d, _)) = devices
        .iter()
        .find(|(_, n)| normalized_device_name(n) == want)
    {
        return Ok((d.clone(), false));
    }
    Ok((default()?, true))
}

/// Empreinte native : identifiant d'endpoint + format de mixage + repli.
/// L'identifiant capte les ré-énumérations USB que le nom seul raterait.
pub fn device_signature(name: Option<&str>, input: bool) -> Option<String> {
    let enu = enumerator().ok()?;
    let (device, fallback) = pick(&enu, name, input).ok()?;
    let id = endpoint_id(&device).ok()?;
    unsafe {
        let client: IAudioClient2 = device.Activate(CLSCTX_ALL, None).ok()?;
        let fmt = client.GetMixFormat().ok()?;
        let (rate, channels) = ((*fmt).nSamplesPerSec, (*fmt).nChannels);
        CoTaskMemFree(Some(fmt as *const _));
        Some(format!("{id}|{rate}|{channels}|{fallback}"))
    }
}

// ---------------------------------------------------------------------------
// Formats
// ---------------------------------------------------------------------------

/// Ce qu'on sait lire dans un tampon WASAPI.
#[derive(Clone, Copy, PartialEq)]
enum SampleKind {
    F32,
    I16,
}

/// Le format qu'on demande d'abord : celui du moteur (48 kHz mono f32 pour la
/// capture, stéréo pour la lecture). AUTOCONVERTPCM laisse Windows faire la
/// conversion — c'est toute la manœuvre : le format du périphérique peut
/// changer sous nos pieds sans nous concerner.
fn engine_format(channels: u16) -> WAVEFORMATEX {
    let block = channels * 4;
    WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_IEEE_FLOAT as u16,
        nChannels: channels,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * block as u32,
        nBlockAlign: block,
        wBitsPerSample: 32,
        cbSize: 0,
    }
}

/// Décode un WAVEFORMATEX (éventuellement EXTENSIBLE) en (échantillon, Hz,
/// canaux). Le format de mixage partagé est en pratique du f32 ; le PCM 16
/// bits est accepté par acquit de conscience.
unsafe fn parse_format(fmt: *const WAVEFORMATEX) -> anyhow::Result<(SampleKind, u32, u16)> {
    let tag = (*fmt).wFormatTag as u32;
    let bits = (*fmt).wBitsPerSample;
    let rate = (*fmt).nSamplesPerSec;
    let channels = (*fmt).nChannels;
    let kind = if tag == WAVE_FORMAT_IEEE_FLOAT && bits == 32 {
        SampleKind::F32
    } else if tag == WAVE_FORMAT_PCM as u32 && bits == 16 {
        SampleKind::I16
    } else if tag == WAVE_FORMAT_EXTENSIBLE {
        let ext = fmt as *const WAVEFORMATEXTENSIBLE;
        let sub = (*ext).SubFormat;
        if sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT && bits == 32 {
            SampleKind::F32
        } else if sub == KSDATAFORMAT_SUBTYPE_PCM && bits == 16 {
            SampleKind::I16
        } else {
            bail!("format de mixage non géré (extensible, {bits} bits)");
        }
    } else {
        bail!("format de mixage non géré (tag {tag}, {bits} bits)");
    };
    Ok((kind, rate, channels))
}

// ---------------------------------------------------------------------------
// Ouverture d'un client
// ---------------------------------------------------------------------------

/// Client initialisé, prêt à démarrer, et le format effectivement obtenu.
struct OpenClient {
    client: IAudioClient2,
    kind: SampleKind,
    rate: u32,
    channels: u16,
    event: HANDLE,
}

/// L'arrêt et la fermeture de l'événement suivent la poignée : aucun chemin
/// de sortie — erreur d'ouverture comprise — ne peut les oublier.
impl Drop for OpenClient {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

/// Active et initialise un client sur `device`. Essaie d'abord le format du
/// moteur (Windows convertit), puis le format de mixage du périphérique ;
/// et si `raw` était demandé mais refusé, recommence sans.
fn open_client(
    device: &IMMDevice,
    input: bool,
    raw: bool,
    comms: bool,
) -> anyhow::Result<OpenClient> {
    // Chaque tentative repart d'un client neuf : un Initialize raté ne
    // laisse pas le client dans un état garanti réutilisable.
    fn attempt(
        device: &IMMDevice,
        input: bool,
        raw: bool,
        comms: bool,
        mix: bool,
    ) -> anyhow::Result<OpenClient> {
        unsafe {
            let client: IAudioClient2 = device
                .Activate(CLSCTX_ALL, None)
                .context("activation du client audio")?;
            // Catégorie « Communications » : nulle part, sauf sur la capture
            // à la demande (`comms`, l'escalade anti-famine). Le terrain l'a
            // retirée pièce par pièce : sur la capture, elle déclenchait
            // l'« appel permanent » de Windows (volume des jeux -80 %) ; et
            // une fois la capture repassée en standard, le volume baissait
            // *encore* — la session de **lecture** déclarée communications
            // suffit à déclencher l'atténuation chez certains (Windows ou le
            // « ChatMix » du pilote du casque, qui baisse le jeu dès qu'une
            // session d'appel existe sur l'endpoint, réglages Windows ou
            // pas). Le mode brut ne concerne que le micro : sur la sortie,
            // les effets du casque restent un choix de l'usager.
            let props = AudioClientProperties {
                cbSize: std::mem::size_of::<AudioClientProperties>() as u32,
                bIsOffload: false.into(),
                eCategory: if input && comms {
                    AudioCategory_Communications
                } else {
                    AudioCategory_Other
                },
                Options: if raw && input {
                    AUDCLNT_STREAMOPTIONS_RAW
                } else {
                    AUDCLNT_STREAMOPTIONS_NONE
                },
            };
            client
                .SetClientProperties(&props)
                .context("propriétés du client audio")?;
            let engine = engine_format(if input { 1 } else { 2 });
            let mix_fmt;
            let (fmt_ptr, owned): (*const WAVEFORMATEX, bool) = if mix {
                mix_fmt = client.GetMixFormat().context("format de mixage")?;
                (mix_fmt as *const _, true)
            } else {
                (&engine as *const _, false)
            };
            let parsed = parse_format(fmt_ptr);
            // Tampons explicites (unités de 100 ns), au lieu du minimum que
            // choisit Windows (~20 ms) : un pic CPU — un jeu qui se lance,
            // précisément — creusait un trou plus grand que le tampon, d'où
            // craquements en sortie et échantillons perdus au micro. Large au
            // micro (simple mémoire, la latence ne dépend que du rythme de
            // lecture) ; en sortie l'allocation est une réserve — la latence
            // réelle est bornée par la cible de remplissage du render_worker.
            let buffer_100ns: i64 = if input { 2_000_000 } else { 600_000 };
            let init = client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                buffer_100ns,
                0,
                fmt_ptr,
                None,
            );
            if owned {
                CoTaskMemFree(Some(fmt_ptr as *const _));
            }
            init.context("initialisation du flux")?;
            let (kind, rate, channels) = parsed?;
            let event = CreateEventW(None, false, false, PCWSTR::null())
                .context("événement audio")?;
            if let Err(e) = client.SetEventHandle(event) {
                let _ = CloseHandle(event);
                return Err(anyhow!(e).context("SetEventHandle"));
            }
            Ok(OpenClient { client, kind, rate, channels, event })
        }
    }

    let plans: &[(bool, bool)] = if raw {
        // (raw, format de mixage plutôt que celui du moteur)
        &[(true, false), (true, true), (false, false), (false, true)]
    } else {
        &[(false, false), (false, true)]
    };
    let mut last = None;
    for &(try_raw, mix) in plans {
        match attempt(device, input, try_raw, comms, mix) {
            Ok(c) => {
                if raw && !try_raw {
                    tracing::warn!("mode brut refusé par le périphérique — ouvert sans");
                    journal("mode brut refusé par le micro — ouvert sans".into());
                }
                return Ok(c);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("aucune tentative d'ouverture")))
}

// ---------------------------------------------------------------------------
// Flux
// ---------------------------------------------------------------------------

/// Poignée d'un flux natif : lâcher la poignée arrête le fil de travail.
/// L'arrêt attend au plus ~200 ms (le pas d'attente du fil).
pub struct NativeStream {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for NativeStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Ouvre le micro en natif. Livre des blocs mono f32 dans `tx`, à la
/// fréquence annoncée en retour (48 kHz sauf repli sur le format de mixage).
/// `alive` est abaissé si le flux meurt — mêmes conventions que cpal.
pub fn open_input(
    device_name: Option<&str>,
    raw: bool,
    comms: bool,
    tx: mpsc::Sender<Vec<f32>>,
    alive: Arc<AtomicBool>,
) -> anyhow::Result<(NativeStream, u32, bool)> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let name = device_name.map(str::to_owned);
    // L'ouverture se fait dans le fil de travail (les objets COM y restent
    // confinés) ; le résultat revient par ce canal.
    let (ready_tx, ready_rx) = mpsc::channel::<anyhow::Result<(u32, bool)>>();

    let thread = std::thread::Builder::new()
        .name("wasapi-capture".into())
        .spawn(move || {
            let opened = (|| -> anyhow::Result<(OpenClient, IAudioCaptureClient, bool)> {
                let enu = enumerator()?;
                let (device, fallback) = pick(&enu, name.as_deref(), true)?;
                let dev_name = friendly_name(&device).unwrap_or_default();
                let open = open_client(&device, true, raw, comms)?;
                let capture: IAudioCaptureClient =
                    unsafe { open.client.GetService() }.context("service de capture")?;
                unsafe { open.client.Start() }.context("démarrage de la capture")?;
                tracing::info!(
                    "micro (natif) : {dev_name} ({} Hz, {} canaux{}{})",
                    open.rate,
                    open.channels,
                    if raw { ", mode brut" } else { "" },
                    if comms { ", catégorie communications" } else { "" }
                );
                journal(format!(
                    "micro ouvert (natif) : {dev_name} ({} Hz, {} canaux{}{}){}",
                    open.rate,
                    open.channels,
                    if raw { ", mode brut" } else { "" },
                    if comms { ", catégorie communications" } else { "" },
                    if fallback { " — repli, le périphérique réglé est introuvable" } else { "" },
                ));
                Ok((open, capture, fallback))
            })();
            let (open, capture, fallback) = match opened {
                Ok(parts) => parts,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let _ = ready_tx.send(Ok((open.rate, fallback)));
            capture_worker(open, capture, &stop_thread, &tx, &alive);
        })
        .context("création du fil de capture natif")?;

    match ready_rx.recv() {
        Ok(Ok((rate, fallback))) => Ok((
            NativeStream { stop, thread: Some(thread) },
            rate,
            fallback,
        )),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            stop.store(true, Ordering::Relaxed);
            let _ = thread.join();
            Err(anyhow!("le fil de capture natif s'est arrêté avant d'ouvrir"))
        }
    }
}

/// Boucle de capture : vide les paquets WASAPI vers `tx` en mono f32.
/// Le corps vit dans une fermeture pour que le nettoyage (arrêt du client,
/// fermeture de l'événement) coure sur **toutes** les sorties, erreurs
/// comprises.
fn capture_worker(
    open: OpenClient,
    capture: IAudioCaptureClient,
    stop: &AtomicBool,
    tx: &mpsc::Sender<Vec<f32>>,
    alive: &AtomicBool,
) {
    let channels = open.channels as usize;
    let fail = |what: &str, e: windows::core::Error| {
        tracing::warn!("flux micro natif : {what} : {e}");
        journal(format!("erreur du flux micro (natif) : {e}"));
        alive.store(false, Ordering::Relaxed);
    };
    (|| {
        while !stop.load(Ordering::Relaxed) {
            unsafe {
                let _ = WaitForSingleObject(open.event, 200);
                loop {
                    let pending = match capture.GetNextPacketSize() {
                        Ok(n) => n,
                        Err(e) => return fail("GetNextPacketSize", e),
                    };
                    if pending == 0 {
                        break;
                    }
                    let mut data: *mut u8 = std::ptr::null_mut();
                    let mut frames: u32 = 0;
                    let mut flags: u32 = 0;
                    if let Err(e) =
                        capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                    {
                        return fail("GetBuffer", e);
                    }
                    if frames > 0 {
                        let n = frames as usize;
                        let mono = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                            // Silence déclaré : livrer des zéros garde la
                            // cadence (les garde-fous de lib.rs comptent
                            // dessus).
                            vec![0f32; n]
                        } else {
                            match open.kind {
                                SampleKind::F32 => {
                                    let s = std::slice::from_raw_parts(
                                        data as *const f32,
                                        n * channels,
                                    );
                                    fold_mono_f32(s, channels)
                                }
                                SampleKind::I16 => {
                                    let s = std::slice::from_raw_parts(
                                        data as *const i16,
                                        n * channels,
                                    );
                                    fold_mono_i16(s, channels)
                                }
                            }
                        };
                        if tx.send(mono).is_err() {
                            // Plus personne n'écoute : le moteur est parti.
                            let _ = capture.ReleaseBuffer(frames);
                            return;
                        }
                    }
                    if let Err(e) = capture.ReleaseBuffer(frames) {
                        return fail("ReleaseBuffer", e);
                    }
                }
            }
        }
    })();
}

/// Ouvre la sortie en natif. `make_writer` reçoit la fréquence réelle du flux
/// et rend la fonction qui fournit `n` échantillons mono — le même contrat
/// que la sortie cpal, la fréquence n'étant connue qu'une fois le flux ouvert.
pub fn open_output<F, W>(
    device_name: Option<&str>,
    make_writer: F,
    alive: Arc<AtomicBool>,
) -> anyhow::Result<(NativeStream, bool)>
where
    F: FnOnce(u32) -> W + Send + 'static,
    W: FnMut(usize) -> Vec<f32> + Send,
{
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let name = device_name.map(str::to_owned);
    let (ready_tx, ready_rx) = mpsc::channel::<anyhow::Result<bool>>();

    let thread = std::thread::Builder::new()
        .name("wasapi-render".into())
        .spawn(move || {
            let opened = (|| -> anyhow::Result<(OpenClient, IAudioRenderClient, u32, bool)> {
                let enu = enumerator()?;
                let (device, fallback) = pick(&enu, name.as_deref(), false)?;
                let dev_name = friendly_name(&device).unwrap_or_default();
                let open = open_client(&device, false, false, false)?;
                let render: IAudioRenderClient =
                    unsafe { open.client.GetService() }.context("service de lecture")?;
                let buffer_frames =
                    unsafe { open.client.GetBufferSize() }.context("taille du tampon")?;
                tracing::info!(
                    "sortie (natif) : {dev_name} ({} Hz, {} canaux)",
                    open.rate,
                    open.channels
                );
                journal(format!(
                    "sortie ouverte (natif) : {dev_name} ({} Hz, {} canaux){}",
                    open.rate,
                    open.channels,
                    if fallback { " — repli, le périphérique réglé est introuvable" } else { "" },
                ));
                Ok((open, render, buffer_frames, fallback))
            })();
            let (open, render, buffer_frames, fallback) = match opened {
                Ok(parts) => parts,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let mut write = make_writer(open.rate);
            // Amorcer à la cible avant de démarrer : le moteur part sans
            // blanc initial, et sans non plus charger tout le tampon — la
            // réserve au-delà de la cible n'est pas de la latence.
            let target = target_padding(open.rate).min(buffer_frames);
            let ok = unsafe {
                fill_render(&open, &render, target, &mut write).is_ok()
                    && open.client.Start().is_ok()
            };
            if !ok {
                let _ = ready_tx.send(Err(anyhow!("démarrage de la lecture impossible")));
                return;
            }
            let _ = ready_tx.send(Ok(fallback));
            render_worker(open, render, buffer_frames, &stop_thread, &mut write, &alive);
        })
        .context("création du fil de lecture natif")?;

    match ready_rx.recv() {
        Ok(Ok(fallback)) => Ok((NativeStream { stop, thread: Some(thread) }, fallback)),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            stop.store(true, Ordering::Relaxed);
            let _ = thread.join();
            Err(anyhow!("le fil de lecture natif s'est arrêté avant d'ouvrir"))
        }
    }
}

/// Niveau de remplissage visé du tampon de sortie : ~30 ms. C'est lui qui
/// fixe la latence de lecture ET la marge avant craquement — la réserve
/// allouée au-delà (60 ms) ne sert qu'aux réveils tardifs du fil.
fn target_padding(rate: u32) -> u32 {
    rate * 3 / 100
}

/// Boucle de lecture : à chaque réveil, complète le tampon jusqu'à la cible.
///
/// Un silence prolongé **arrête** le flux au lieu de le nourrir de zéros.
/// Vu sur le terrain dès la première soirée : certains pilotes (égalisation
/// de sonie, DSP « voix » des casques) montent leur gain sur un silence
/// continu jusqu'à rendre le souffle audible — un canal calme finissait par
/// siffler chez tout le monde. À l'arrêt, le mix est sondé toutes les 20 ms ;
/// au premier échantillon non nul, l'amorce sondée est pré-chargée puis le
/// flux repart — rien n'est perdu, au prix de ~20 ms de latence au réveil.
fn render_worker<W: FnMut(usize) -> Vec<f32>>(
    open: OpenClient,
    render: IAudioRenderClient,
    buffer_frames: u32,
    stop: &AtomicBool,
    write: &mut W,
    alive: &AtomicBool,
) {
    /// Silence continu au-delà duquel la sortie est mise en veille.
    const IDLE_STOP: Duration = Duration::from_secs(5);
    /// Sonde à l'arrêt : une trame moteur, 20 ms.
    const PROBE: usize = (SAMPLE_RATE / 50) as usize;

    let target = target_padding(open.rate).min(buffer_frames);
    let mut last_audio = Instant::now();
    let mut running = true;
    while !stop.load(Ordering::Relaxed) {
        unsafe {
            if running {
                let _ = WaitForSingleObject(open.event, 200);
                let padding = match open.client.GetCurrentPadding() {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!("flux sortie natif : GetCurrentPadding : {e}");
                        journal(format!("erreur du flux de sortie (natif) : {e}"));
                        alive.store(false, Ordering::Relaxed);
                        break;
                    }
                };
                // Compléter jusqu'à la cible seulement : remplir tout le
                // tampon transformerait la réserve anti-craquement en
                // latence pure.
                let want = target.saturating_sub(padding);
                if want == 0 {
                    continue;
                }
                match fill_render(&open, &render, want, write) {
                    Ok(silent) => {
                        if !silent {
                            last_audio = Instant::now();
                        } else if last_audio.elapsed() > IDLE_STOP {
                            // Plus rien à jouer : veille. Stop fige, Reset
                            // vide — le réveil repartira d'un tampon propre.
                            let _ = open.client.Stop();
                            let _ = open.client.Reset();
                            running = false;
                            tracing::debug!("sortie en veille (silence prolongé)");
                        }
                    }
                    Err(e) => {
                        tracing::warn!("flux sortie natif : {e}");
                        journal(format!("erreur du flux de sortie (natif) : {e}"));
                        alive.store(false, Ordering::Relaxed);
                        break;
                    }
                }
            } else {
                // En veille : l'événement ne bat plus, on cadence soi-même.
                // La sonde consomme le mix — les compteurs (watchdog) vivent,
                // et l'amorce du réveil est justement ce qu'elle a tiré.
                std::thread::sleep(Duration::from_millis(20));
                let probe = write(PROBE);
                if probe.iter().any(|&s| s != 0.0) {
                    // L'amorce ne peut pas dépasser le tampon : un GetBuffer
                    // trop gourmand échouerait et, la veille revenant après
                    // chaque réouverture, condamnerait la sortie à mourir en
                    // boucle. Les trames en trop (quelques ms au pire) sont
                    // sacrifiées à l'instant du réveil — inaudible.
                    let n = (probe.len()).min(buffer_frames as usize);
                    let ok = write_block(&open, &render, &probe[..n]).is_ok()
                        && open.client.Start().is_ok();
                    if !ok {
                        tracing::warn!("flux sortie natif : réveil impossible");
                        journal("erreur du flux de sortie (natif) : réveil impossible".into());
                        alive.store(false, Ordering::Relaxed);
                        break;
                    }
                    running = true;
                    last_audio = Instant::now();
                    tracing::debug!("sortie réveillée");
                }
            }
        }
    }
}

/// Tire `frames` échantillons mono du mix et les écrit dans le tampon.
/// Rend vrai si tout était à zéro strict — le signal de mise en veille.
unsafe fn fill_render<W: FnMut(usize) -> Vec<f32>>(
    open: &OpenClient,
    render: &IAudioRenderClient,
    frames: u32,
    write: &mut W,
) -> anyhow::Result<bool> {
    let mono = write(frames as usize);
    let silent = mono.iter().all(|&s| s == 0.0);
    write_block(open, render, &mono)?;
    Ok(silent)
}

/// Écrit un bloc mono dans le tampon de rendu, dupliqué sur tous les canaux —
/// la sortie voix est mono par nature.
unsafe fn write_block(
    open: &OpenClient,
    render: &IAudioRenderClient,
    mono: &[f32],
) -> anyhow::Result<()> {
    let frames = mono.len() as u32;
    let channels = open.channels as usize;
    let data = render.GetBuffer(frames).context("GetBuffer")?;
    match open.kind {
        SampleKind::F32 => {
            let out = std::slice::from_raw_parts_mut(data as *mut f32, mono.len() * channels);
            for (frame, &s) in out.chunks_exact_mut(channels).zip(mono.iter()) {
                frame.fill(s.clamp(-1.0, 1.0));
            }
        }
        SampleKind::I16 => {
            let out = std::slice::from_raw_parts_mut(data as *mut i16, mono.len() * channels);
            for (frame, &s) in out.chunks_exact_mut(channels).zip(mono.iter()) {
                frame.fill((s.clamp(-1.0, 1.0) * 32767.0) as i16);
            }
        }
    }
    render.ReleaseBuffer(frames, 0).context("ReleaseBuffer")?;
    Ok(())
}

fn fold_mono_f32(data: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return data.to_vec();
    }
    data.chunks_exact(channels)
        .map(|c| c.iter().sum::<f32>() / channels as f32)
        .collect()
}

fn fold_mono_i16(data: &[i16], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return data.iter().map(|&s| s as f32 / 32768.0).collect();
    }
    data.chunks_exact(channels)
        .map(|c| c.iter().map(|&s| s as f32 / 32768.0).sum::<f32>() / channels as f32)
        .collect()
}

