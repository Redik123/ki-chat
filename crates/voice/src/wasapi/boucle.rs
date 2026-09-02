//! Son du jeu : la boucle de tout ce que joue le système, **sauf nous**.
//!
//! Windows 10 2004 a apporté la « boucle par processus » : un client de
//! capture qui reçoit le mix de tout le système en excluant un processus et
//! ses enfants. C'est la technique de Discord pour le son du jeu — ce que
//! ki-chat joue (les voix des copains, les notifications) n'y est pas, les
//! spectateurs n'entendent donc pas leurs propres voix en retour. Le client
//! s'obtient par `ActivateAudioInterfaceAsync` sur le périphérique virtuel
//! `VAD\Process_Loopback`, et accepte le format qu'on lui demande : ici du
//! float 48 kHz stéréo, Windows convertit.
//!
//! Deux pièges rencontrés en chemin, pour mémoire. Le `PROPVARIANT` qui
//! porte les paramètres d'activation a un `Drop` dans le crate `windows`
//! (`PropVariantClear`) : avec un BLOB qui pointe sur la pile, ce Drop
//! libère une adresse de pile et abîme le tas — d'où le `ManuallyDrop`.
//! Et toute la mise en route du client se fait dans le rappel
//! d'activation, sur le fil de Windows, comme dans l'exemple de Microsoft.

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use anyhow::{anyhow, Context};
use windows::core::{IUnknown_Vtbl, Interface as _, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, E_NOINTERFACE, E_POINTER, HANDLE, S_OK};
use windows::Win32::Media::Audio::{
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Vtbl,
    IAudioCaptureClient, IAudioClient, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::BLOB;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::Variant::VT_BLOB;

use super::{engine_format, ensure_com, NativeStream, ProAudio};
use crate::journal;

/// Blocs stéréo f32 entrelacés, 48 kHz, tels que la boucle les livre.
pub type LoopTx = mpsc::SyncSender<Vec<f32>>;

/// Le client de boucle et son événement ; arrêtés et fermés avec lui.
struct LoopClient {
    client: IAudioClient,
    event: HANDLE,
}

impl Drop for LoopClient {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

/// Le client prêt à capturer, tel que le rappel d'activation le livre.
type Pret = anyhow::Result<(LoopClient, IAudioCaptureClient)>;

/// Le rappel d'activation, objet COM écrit à la main : Windows l'appelle sur
/// un fil à lui quand le client de boucle est prêt ; on y fait toute la mise
/// en route et on passe le résultat à celui qui attend. À la main plutôt
/// que par `#[implement]` : quatre fonctions, un compte de références
/// qu'on tient nous-mêmes, rien de caché.
#[repr(C)]
struct Rappel {
    vtable: *const IActivateAudioInterfaceCompletionHandler_Vtbl,
    count: AtomicU32,
    tx: mpsc::Sender<Pret>,
}

/// IAgileObject : dire à Windows qu'il peut nous appeler de n'importe quel
/// fil, sans marshaling.
const IID_AGILE: windows::core::GUID =
    windows::core::GUID::from_u128(0x94ea2b94_e9cc_49e0_c0ff_ee64ca8f5b90);

const VTABLE: IActivateAudioInterfaceCompletionHandler_Vtbl =
    IActivateAudioInterfaceCompletionHandler_Vtbl {
        base__: IUnknown_Vtbl {
            QueryInterface: rappel_qi,
            AddRef: rappel_add_ref,
            Release: rappel_release,
        },
        ActivateCompleted: rappel_activate_completed,
    };

unsafe extern "system" fn rappel_qi(
    this: *mut c_void,
    iid: *const windows::core::GUID,
    out: *mut *mut c_void,
) -> windows::core::HRESULT {
    if iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    let iid = unsafe { &*iid };
    if *iid == windows::core::IUnknown::IID
        || *iid == IActivateAudioInterfaceCompletionHandler::IID
        || *iid == IID_AGILE
    {
        unsafe {
            rappel_add_ref(this);
            *out = this;
        }
        S_OK
    } else {
        unsafe { *out = std::ptr::null_mut() };
        E_NOINTERFACE
    }
}

unsafe extern "system" fn rappel_add_ref(this: *mut c_void) -> u32 {
    unsafe { (*(this as *mut Rappel)).count.fetch_add(1, Ordering::AcqRel) + 1 }
}

unsafe extern "system" fn rappel_release(this: *mut c_void) -> u32 {
    let n = unsafe { (*(this as *mut Rappel)).count.fetch_sub(1, Ordering::AcqRel) - 1 };
    if n == 0 {
        drop(unsafe { Box::from_raw(this as *mut Rappel) });
    }
    n
}

unsafe extern "system" fn rappel_activate_completed(
    this: *mut c_void,
    op: *mut c_void,
) -> windows::core::HRESULT {
    // L'opération nous est prêtée le temps de l'appel : pas de Release.
    let op = ManuallyDrop::new(unsafe { IActivateAudioInterfaceAsyncOperation::from_raw(op) });
    let pret = (|| -> Pret {
        let mut hr = windows::core::HRESULT(0);
        let mut iface: Option<windows::core::IUnknown> = None;
        unsafe { op.GetActivateResult(&mut hr, &mut iface) }
            .context("résultat d'activation de la boucle")?;
        hr.ok().context("la boucle audio est refusée")?;
        let client: IAudioClient =
            iface.context("client audio absent")?.cast().context("IAudioClient")?;
        let fmt = engine_format(2);
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                200_000,
                0,
                &fmt,
                None,
            )
        }
        .context("initialisation de la boucle audio")?;
        let event = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }
            .context("événement audio")?;
        if let Err(e) = unsafe { client.SetEventHandle(event) } {
            unsafe {
                let _ = CloseHandle(event);
            }
            return Err(anyhow!(e).context("SetEventHandle"));
        }
        let open = LoopClient { client, event };
        let capture: IAudioCaptureClient =
            unsafe { open.client.GetService() }.context("service de capture")?;
        unsafe { open.client.Start() }.context("démarrage de la boucle")?;
        Ok((open, capture))
    })();
    let _ = unsafe { &(*(this as *mut Rappel)).tx }.send(pret);
    S_OK
}

/// Un rappel neuf, une référence (la nôtre) ; l'interface la porte.
fn rappel_neuf(tx: mpsc::Sender<Pret>) -> IActivateAudioInterfaceCompletionHandler {
    let obj = Box::into_raw(Box::new(Rappel { vtable: &VTABLE, count: AtomicU32::new(1), tx }));
    unsafe { IActivateAudioInterfaceCompletionHandler::from_raw(obj as *mut c_void) }
}

/// Ouvre la boucle « tout ce que joue le système, sauf le processus
/// `exclure` et ses enfants », en float 48 kHz stéréo. Les blocs partent
/// dans `tx` ; `alive` tombe si le flux meurt.
pub fn open_loopback(
    exclure: u32,
    tx: LoopTx,
    alive: Arc<AtomicBool>,
) -> anyhow::Result<NativeStream> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let (ready_tx, ready_rx) = mpsc::channel::<anyhow::Result<()>>();
    let thread = std::thread::Builder::new()
        .name("wasapi-boucle".into())
        .spawn(move || {
            let opened = (|| -> Pret {
                ensure_com();
                let params = AUDIOCLIENT_ACTIVATION_PARAMS {
                    ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
                    Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                        ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                            TargetProcessId: exclure,
                            ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
                        },
                    },
                };
                // Les paramètres voyagent dans un PROPVARIANT de type BLOB qui
                // pointe sur `params`, sur la pile. ManuallyDrop : le Drop du
                // crate windows appellerait PropVariantClear, qui libérerait
                // cette adresse de pile comme si elle venait de CoTaskMemAlloc.
                let mut prop: ManuallyDrop<PROPVARIANT> = ManuallyDrop::new(PROPVARIANT::default());
                unsafe {
                    let inner = &mut *prop.Anonymous.Anonymous;
                    inner.vt = VT_BLOB;
                    inner.Anonymous.blob = BLOB {
                        cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: &params as *const _ as *mut u8,
                    };
                }
                let (fait_tx, fait_rx) = mpsc::channel::<Pret>();
                let handler = rappel_neuf(fait_tx);
                let op = unsafe {
                    ActivateAudioInterfaceAsync(
                        VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
                        &IAudioClient::IID,
                        Some(&*prop as *const PROPVARIANT),
                        &handler,
                    )
                }
                .context("activation de la boucle audio (Windows 10 2004 ou plus récent requis)")?;
                // Notre référence sur l'opération part tout de suite, comme
                // dans l'exemple de Microsoft : la dernière sera celle de
                // Windows, quand il en aura fini.
                drop(op);
                fait_rx
                    .recv_timeout(Duration::from_secs(3))
                    .map_err(|_| anyhow!("activation de la boucle audio : Windows ne répond pas"))?
            })();
            let (open, capture) = match opened {
                Ok(parts) => parts,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(()));
            let _priorite = ProAudio::claim("boucle");
            loopback_worker(open, capture, &stop_thread, &tx, &alive);
        })
        .context("création du fil de boucle audio")?;
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(NativeStream { stop, thread: Some(thread) }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            stop.store(true, Ordering::Relaxed);
            let _ = thread.join();
            Err(anyhow!("le fil de boucle audio s'est arrêté avant d'ouvrir"))
        }
    }
}

/// La boucle de lecture des paquets : stéréo f32 tel quel vers `tx`. Le
/// consommateur (l'encodeur) est plus lent que nous ? On jette le bloc : du
/// son de jeu en retard ne vaut rien.
fn loopback_worker(
    open: LoopClient,
    capture: IAudioCaptureClient,
    stop: &AtomicBool,
    tx: &LoopTx,
    alive: &AtomicBool,
) {
    // Le format demandé à l'initialisation : stéréo f32.
    let channels = 2usize;
    let fail = |what: &str, e: windows::core::Error| {
        tracing::warn!("boucle audio : {what} : {e}");
        journal(format!("erreur de la boucle audio (son du jeu) : {e}"));
        alive.store(false, Ordering::Relaxed);
    };
    while !stop.load(Ordering::Relaxed) {
        unsafe {
            let _ = WaitForSingleObject(open.event, 50);
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
                if let Err(e) = capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None) {
                    return fail("GetBuffer", e);
                }
                if frames > 0 {
                    let n = frames as usize * channels;
                    let bloc: Vec<f32> = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                        vec![0.0; n]
                    } else {
                        std::slice::from_raw_parts(data as *const f32, n).to_vec()
                    };
                    let _ = tx.try_send(bloc);
                }
                if let Err(e) = capture.ReleaseBuffer(frames) {
                    return fail("ReleaseBuffer", e);
                }
            }
        }
    }
}
