//! Les partages d'écran sur Android : les regarder, et diffuser le sien.
//!
//! **Regarder.** Le réseau verse les trames dans un fil qui les déchiffre, les
//! remet en ordre ([`ki_core::flux::Lecture`]) et les donne au décodeur matériel,
//! qui peint directement dans la `Surface` de la vue native (`RegardVue.kt`) : ni
//! copie d'image, ni passage par la page. Le son va dans la sortie du moteur vocal.
//!
//! Deux façons de regarder, au choix de la personne. **Fluide** : chaque image
//! attend son heure ([`TAMPON_FLUIDE_US`] après la trame la plus rapide) et s'affiche
//! au rythme où elle a été capturée, malgré les à-coups du réseau ; le son attend
//! autant. **Faible latence** : tout s'affiche dès que c'est décodé.
//!
//! **Diffuser.** `DiffusionService.kt` tient la capture d'écran système et
//! l'encodeur matériel ; chaque trame encodée arrive ici par [`trame`](Java_fun_baws_kichat_KiVideo_trame),
//! y est chiffrée et part dans son flux QUIC. C'est le format du PC : un téléphone
//! regarde un PC, et l'inverse.
//!
//! Rien ne vit ici qui ne se dise à Kotlin que par ces fonctions JNI. Le réglage
//! inverse (une trame clé demandée, le débit à tenir) repart dans la valeur rendue
//! par `trame`, à chaque trame : pas d'appel de Rust vers Java à gérer.

use std::collections::VecDeque;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use jni_sys::{jboolean, jclass, jfloatArray, jint, jlong, jobject, jstring, JNIEnv, JNI_FALSE, JNI_TRUE};
use ki_core::flux::{
    lancer_envoi, parametres_h264, Lecture, Lissage, Ouvreur, Repere, Scelleur, Trame, FILE_SON_MAX, FILE_TRAMES_MAX,
};
use ki_core::net::{Cmd, NetHandle, VoiceLink};
use ki_protocol::{ClientMsg, StreamMeta};
use ndk_sys as ffi;

use crate::Partage;

/// Le son d'un stream qu'on diffuse, en bits/s (le même que sur PC).
const DEBIT_SON: i32 = 96_000;
/// Sans trame clé depuis ce temps, on en redemande une : `Watch` repart à neuf.
const ATTENTE_CLE: Duration = Duration::from_millis(1500);
/// Le temps moyen de passage au décodeur matériel, pour estimer le retard.
const DECODAGE_MS: f64 = 8.0;
/// Mode fluide : l'avance qu'on se donne sur la trame la plus rapide. Une trame peut
/// arriver jusque-là en retard sans que ça se voie.
const TAMPON_FLUIDE_US: i64 = 300_000;
/// Mode fluide : une trame part au décodeur ce temps-là avant son heure. Assez pour
/// la décoder ; et l'image n'attend pas plus de deux balayages à l'entrée de l'écran,
/// comme le demande `MediaCodec`.
const AVANCE_DECODAGE_US: i64 = 40_000;
/// Sur une télé, une image immobile ne doit pas laisser le lecteur sans rien : la
/// capture du PC ne livre plus rien, mais elle renvoie une trame clé si on la demande.
const IMAGE_IMMOBILE: Duration = Duration::from_millis(1500);

/// Une mutex empoisonnée n'est pas une raison d'arrêter le vocal : ces fonctions
/// sont appelées depuis Java, où une panique ferait tomber toute l'appli.
pub(crate) fn verrou<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

static PARTAGE: OnceLock<Partage> = OnceLock::new();

/// L'état de l'appli, pour les fonctions appelées depuis Java.
pub fn installer(partage: Partage) {
    let _ = PARTAGE.set(partage);
}

// ---------------------------------------------------------------------------
// La surface où peindre
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Surface {
    /// Un `ANativeWindow*`, 0 : pas de surface.
    fenetre: usize,
    generation: u64,
    /// La génération que le fil du décodeur a prise en compte.
    vue: u64,
}

static SURFACE: Mutex<Surface> = Mutex::new(Surface { fenetre: 0, generation: 0, vue: 0 });
static SURFACE_VUE: Condvar = Condvar::new();
/// Un fil décodeur tourne : la vue native attend qu'il lâche sa surface.
static REGARD_ACTIF: AtomicBool = AtomicBool::new(false);
/// Largeur et hauteur de la dernière image peinte (0 : aucune encore).
static DIMENSIONS: AtomicU64 = AtomicU64::new(0);
/// Les chiffres du stream regardé, refaits chaque seconde : un toucher sur la vue
/// native les montre (`RegardVue.kt`).
static CHIFFRES: Mutex<String> = Mutex::new(String::new());
/// Ce qu'annonce celui qui diffuse : `(images par seconde << 32) | kbit/s`.
static SOURCE: AtomicU64 = AtomicU64::new(0);
/// Regarder en mode fluide : c'est la vue native qui le dit (`KiVideo.fluide`).
static FLUIDE: AtomicBool = AtomicBool::new(true);

/// L'horloge de `MediaCodec` et de l'écran (`CLOCK_MONOTONIC`), en microsecondes.
pub(crate) fn horloge_us() -> i64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY : `t` vit le temps de l'appel.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    // 32 bits sur les vieux téléphones (armv7), 64 ailleurs : ces conversions ne sont
    // inutiles que sur ceux-là.
    #[allow(clippy::unnecessary_cast)]
    let (secondes, nanos) = (t.tv_sec as i64, t.tv_nsec as i64);
    secondes * 1_000_000 + nanos / 1000
}

fn poser_surface(fenetre: *mut ffi::ANativeWindow) {
    let mut s = verrou(&SURFACE);
    let ancienne = s.fenetre;
    s.fenetre = fenetre as usize;
    s.generation += 1;
    let generation = s.generation;
    // La surface s'en va : le décodeur doit la lâcher avant que la vue ne la
    // rende au système, sinon il écrirait dans une surface morte.
    if fenetre.is_null() && REGARD_ACTIF.load(Ordering::Relaxed) {
        let delai = Duration::from_millis(300);
        let (garde, _) = SURFACE_VUE
            .wait_timeout_while(s, delai, |s| s.vue != generation)
            .unwrap_or_else(|e| e.into_inner());
        s = garde;
    }
    drop(s);
    if ancienne != 0 {
        // SAFETY : la référence prise par `ANativeWindow_fromSurface`, rendue une fois.
        unsafe { ffi::ANativeWindow_release(ancienne as *mut ffi::ANativeWindow) };
    }
}

// ---------------------------------------------------------------------------
// Le décodeur matériel
// ---------------------------------------------------------------------------

/// Ce que mesure le fil vidéo sur une seconde, pour [`CHIFFRES`].
#[derive(Default)]
struct Mesures {
    recues: u32,
    peintes: u32,
    octets: u64,
    /// Le temps passé dans le décodeur : somme, nombre, pire cas.
    decodage_us: u64,
    decodages: u32,
    decodage_max_us: u64,
    /// La plage de (arrivée − horodatage) : sa largeur est la gigue. Les horloges
    /// des deux machines ne se comparent pas, seul l'écart entre les trames compte.
    ecart: Option<(i64, i64)>,
    /// Mode fluide : les images sorties du décodeur après leur heure.
    en_retard: u32,
}

struct Decodeur {
    codec: *mut ffi::AMediaCodec,
    largeur: u16,
    hauteur: u16,
    /// Ce qui est dans le décodeur et n'en est pas ressorti : (horodatage, entrée).
    entrees: VecDeque<(u64, Instant)>,
}

impl Decodeur {
    /// Un décodeur H.264 peignant dans `fenetre`, configuré sur une trame clé.
    fn new(fenetre: *mut ffi::ANativeWindow, largeur: u16, hauteur: u16, cle: &[u8]) -> Result<Self, String> {
        // Les réglages de latence ne passent pas partout : refusés, on retente sans.
        Self::creer(fenetre, largeur, hauteur, cle, true).or_else(|e| {
            tracing::warn!("regard : décodeur sans ses réglages de latence ({e})");
            Self::creer(fenetre, largeur, hauteur, cle, false)
        })
    }

    fn creer(
        fenetre: *mut ffi::ANativeWindow,
        largeur: u16,
        hauteur: u16,
        cle: &[u8],
        faible_latence: bool,
    ) -> Result<Self, String> {
        // SAFETY : suite d'appels de l'API NDK, chaque pointeur vérifié avant usage ;
        // le format est libéré ici, le codec par `Drop`.
        unsafe {
            let codec = ffi::AMediaCodec_createDecoderByType(c"video/avc".as_ptr());
            if codec.is_null() {
                return Err("pas de décodeur H.264 sur ce téléphone".into());
            }
            let decodeur = Self { codec, largeur, hauteur, entrees: VecDeque::new() };
            let format = ffi::AMediaFormat_new();
            ffi::AMediaFormat_setString(format, c"mime".as_ptr(), c"video/avc".as_ptr());
            ffi::AMediaFormat_setInt32(format, c"width".as_ptr(), i32::from(largeur));
            ffi::AMediaFormat_setInt32(format, c"height".as_ptr(), i32::from(hauteur));
            if faible_latence {
                // Temps réel : rendre chaque image dès qu'elle est prête. Sans ça, un
                // décodeur attend d'en avoir plusieurs d'avance, au cas où elles
                // seraient à remettre en ordre (dix images chez Qualcomm : 400 ms). La
                // clé d'Android 11 ne suffit pas toujours, chaque fabricant a la sienne ;
                // celles qui ne le concernent pas sont ignorées.
                for nom in [
                    c"low-latency",
                    c"vendor.qti-ext-dec-low-latency.enable",
                    c"vendor.rtc-ext-dec-low-latency.enable",
                    c"vendor.low-latency.enable",
                    c"vdec-lowlatency",
                ] {
                    ffi::AMediaFormat_setInt32(format, nom.as_ptr(), 1);
                }
                ffi::AMediaFormat_setInt32(format, c"priority".as_ptr(), 0);
            }
            if let Some((sps, pps)) = parametres_h264(cle) {
                ffi::AMediaFormat_setBuffer(format, c"csd-0".as_ptr(), sps.as_ptr().cast(), sps.len());
                ffi::AMediaFormat_setBuffer(format, c"csd-1".as_ptr(), pps.as_ptr().cast(), pps.len());
            }
            let configure = ffi::AMediaCodec_configure(codec, format, fenetre, ptr::null_mut(), 0);
            ffi::AMediaFormat_delete(format);
            if configure != ffi::media_status_t::AMEDIA_OK {
                return Err(format!("configuration du décodeur refusée ({configure:?})"));
            }
            let demarre = ffi::AMediaCodec_start(codec);
            if demarre != ffi::media_status_t::AMEDIA_OK {
                return Err(format!("démarrage du décodeur refusé ({demarre:?})"));
            }
            Ok(decodeur)
        }
    }

    /// Donne une trame au décodeur. `false` : pas de place pour l'instant.
    fn envoyer(&mut self, trame: &Trame) -> bool {
        // SAFETY : l'index vient de `dequeueInputBuffer`, le tampon rendu par le
        // codec fait `capacite` octets, et l'on n'y écrit pas au-delà.
        unsafe {
            let index = ffi::AMediaCodec_dequeueInputBuffer(self.codec, 0);
            if index < 0 {
                return false;
            }
            let mut capacite = 0usize;
            let tampon = ffi::AMediaCodec_getInputBuffer(self.codec, index as usize, &mut capacite);
            let n = trame.donnees.len();
            if tampon.is_null() || n > capacite {
                // Rendu vide, pour ne pas perdre le tampon. La trame est perdue :
                // le décodeur repartira à la prochaine trame clé.
                ffi::AMediaCodec_queueInputBuffer(self.codec, index as usize, 0, 0, trame.pts_us, 0);
                return true;
            }
            ptr::copy_nonoverlapping(trame.donnees.as_ptr(), tampon, n);
            ffi::AMediaCodec_queueInputBuffer(self.codec, index as usize, 0, n, trame.pts_us, 0);
            self.entrees.push_back((trame.pts_us, Instant::now()));
            true
        }
    }

    /// Peint tout ce qui est prêt ; rend le nombre d'images peintes. `decalage` : ce
    /// qu'il faut ajouter à un horodatage pour avoir son heure d'affichage (fluide),
    /// ou rien pour afficher tout de suite.
    fn peindre(&mut self, m: &mut Mesures, decalage: Option<i64>) -> u32 {
        let mut peintes = 0;
        loop {
            let mut info = ffi::AMediaCodecBufferInfo { offset: 0, size: 0, presentationTimeUs: 0, flags: 0 };
            // SAFETY : `info` vit le temps de l'appel ; l'index rendu n'est relâché
            // qu'une fois.
            let index = unsafe { ffi::AMediaCodec_dequeueOutputBuffer(self.codec, &mut info, 0) };
            if index >= 0 {
                let heure = decalage.filter(|_| info.size > 0).map(|d| info.presentationTimeUs + d);
                match heure {
                    Some(h) if h > horloge_us() => {
                        // SAFETY : voir ci-dessus.
                        unsafe { ffi::AMediaCodec_releaseOutputBufferAtTime(self.codec, index as usize, h * 1000) };
                    }
                    _ => {
                        if heure.is_some() {
                            m.en_retard += 1;
                        }
                        // SAFETY : voir ci-dessus.
                        unsafe { ffi::AMediaCodec_releaseOutputBuffer(self.codec, index as usize, info.size > 0) };
                    }
                }
                if info.size > 0 {
                    peintes += 1;
                    let pts = info.presentationTimeUs as u64;
                    if let Some(i) = self.entrees.iter().position(|(p, _)| *p == pts) {
                        let us = self.entrees[i].1.elapsed().as_micros() as u64;
                        self.entrees.drain(..=i);
                        m.decodage_us += us;
                        m.decodages += 1;
                        m.decodage_max_us = m.decodage_max_us.max(us);
                    }
                }
            } else if index == ffi::AMEDIACODEC_INFO_TRY_AGAIN_LATER as isize {
                return peintes;
            }
            // Le reste (format ou tampons changés) : on continue de vider.
        }
    }
}

impl Drop for Decodeur {
    fn drop(&mut self) {
        // SAFETY : le codec vient de `createDecoderByType`, libéré une seule fois.
        unsafe {
            ffi::AMediaCodec_stop(self.codec);
            ffi::AMediaCodec_delete(self.codec);
        }
    }
}

// ---------------------------------------------------------------------------
// Regarder un stream
// ---------------------------------------------------------------------------

/// Un stream qu'on regarde, tant que la poignée vit.
pub struct Regard {
    stream_id: u32,
    stop: Arc<AtomicBool>,
    video: Option<std::thread::JoinHandle<()>>,
    son: Option<std::thread::JoinHandle<()>>,
}

impl Regard {
    /// Branche le réseau sur deux fils : l'image et le son.
    pub fn demarrer(stream_id: u32, cle: [u8; 32], meta: &StreamMeta, net: &NetHandle, lien: &VoiceLink) -> Option<Self> {
        let (tx, rx) = mpsc::sync_channel(FILE_TRAMES_MAX);
        let (tx_son, rx_son) = mpsc::sync_channel(FILE_SON_MAX);
        let stop = Arc::new(AtomicBool::new(false));
        DIMENSIONS.store(0, Ordering::Relaxed);
        verrou(&CHIFFRES).clear();
        SOURCE.store(source_de(meta), Ordering::Relaxed);
        if let Some(tele) = crate::tele::active() {
            tele.repartir();
        }
        REGARD_ACTIF.store(true, Ordering::Relaxed);
        let video = {
            let (stop, commandes, conn) = (stop.clone(), net.commandes(), net.connexion());
            let ping = move || conn.as_ref().map(|c| c.rtt().as_millis() as u32);
            std::thread::Builder::new()
                .name("regard-video".into())
                .spawn(move || fil_video(stream_id, cle, rx, stop, commandes, ping))
                .ok()?
        };
        let son = {
            let (stop, moteur) = (stop.clone(), lien.engine.clone());
            std::thread::Builder::new()
                .name("regard-son".into())
                .spawn(move || fil_son(stream_id, cle, rx_son, moteur, stop))
                .ok()
        };
        net.set_video_feed(Some(tx));
        net.set_game_audio_feed(Some(tx_son));
        Some(Self { stream_id, stop, video: Some(video), son })
    }

    pub fn stream_id(&self) -> u32 {
        self.stream_id
    }

    pub fn arreter(mut self, net: Option<&NetHandle>) {
        if let Some(net) = net {
            net.set_video_feed(None);
            net.set_game_audio_feed(None);
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(fil) = self.video.take() {
            let _ = fil.join();
        }
        if let Some(fil) = self.son.take() {
            let _ = fil.join();
        }
        REGARD_ACTIF.store(false, Ordering::Relaxed);
    }
}

fn source_de(meta: &StreamMeta) -> u64 {
    (u64::from(meta.fps) << 32) | u64::from(meta.kbps)
}

/// Redemande une trame clé au stream regardé, s'il y en a un.
pub(crate) fn redemander_cle() {
    let Some(partage) = PARTAGE.get() else { return };
    let a = verrou(partage);
    if let Some(stream_id) = a.regard.as_ref().map(Regard::stream_id) {
        a.envoyer(ClientMsg::Watch { stream_id, couches: true });
    }
}

/// Celui qui diffuse a changé ses réglages : ce sont ceux qu'on affiche.
pub fn source_changee(stream_id: u32, meta: &StreamMeta, regard: Option<u32>) {
    if regard == Some(stream_id) {
        SOURCE.store(source_de(meta), Ordering::Relaxed);
    }
}

/// Les chiffres d'une seconde (`duree`), à lire d'un coup d'œil sur le téléphone.
#[allow(clippy::too_many_arguments)]
fn chiffres_du_stream(
    m: &Mesures,
    duree: f32,
    dimensions: Option<(u16, u16)>,
    basse: bool,
    dans_le_decodeur: usize,
    en_attente: usize,
    ping: Option<u32>,
    perdues: u64,
    reprises: u64,
) -> String {
    let source = SOURCE.load(Ordering::Relaxed);
    let (source_fps, source_kbps) = (source >> 32, source & 0xffff_ffff);
    let taille = dimensions.map_or("…".to_string(), |(l, h)| format!("{l}×{h}"));
    let debit = format!("{:.1}", m.octets as f32 * 8.0 / duree / 1_000_000.0).replace('.', ",");
    let annonce = format!("{:.1}", source_kbps as f32 / 1000.0).replace('.', ",");
    let decodage = (m.decodage_us / u64::from(m.decodages.max(1))) / 1000;
    let gigue = m.ecart.map_or(0, |(a, b)| (b - a) / 1000);
    let ping = ping.map_or("?".to_string(), |p| p.to_string());
    let mode = if FLUIDE.load(Ordering::Relaxed) {
        format!("fluide, {} ms de tampon · {en_attente} images · {} en retard", TAMPON_FLUIDE_US / 1000, m.en_retard)
    } else {
        "faible latence".to_string()
    };
    format!(
        "{taille} · qualité {}\n\
         Mode : {mode}\n\
         Images : {:.0} reçues/s · {:.0} peintes/s (source {source_fps})\n\
         Débit : {debit} Mbit/s (source {annonce})\n\
         Décodeur : {decodage} ms, max {} ms · {dans_le_decodeur} dedans\n\
         Réseau : ping {ping} ms · gigue {gigue} ms\n\
         Depuis le début : {perdues} perdues · {reprises} reprises",
        if basse { "basse" } else { "haute" },
        m.recues as f32 / duree,
        m.peintes as f32 / duree,
        m.decodage_max_us / 1000,
    )
}

fn fil_video(
    stream_id: u32,
    cle: [u8; 32],
    rx: mpsc::Receiver<Vec<u8>>,
    stop: Arc<AtomicBool>,
    commandes: tokio::sync::mpsc::UnboundedSender<Cmd>,
    ping: impl Fn() -> Option<u32>,
) {
    let ouvreur = Ouvreur::new(stream_id, cle);
    let mut lecture = Lecture::default();
    let mut decodeur: Option<Decodeur> = None;
    let mut en_attente: VecDeque<Trame> = VecDeque::new();
    let mut generation = 0u64;
    let mut derniere_demande = Instant::now();
    let depart = Instant::now();
    let mut m = Mesures::default();
    let (mut periode, mut bilan) = (Instant::now(), Instant::now());
    // Depuis le début, eux : un incident doit rester lisible après coup.
    let (mut perdues, mut reprises) = (0u64, 0u64);
    let mut derniere: Option<(u64, bool)> = None;
    let mut repere = Repere::default();
    let mut lissage = Lissage::default();
    let mut derniere_pour_la_tele = Instant::now();
    let mut premiere = true;
    while !stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(4)) {
            Ok(octets) => {
                let mut lot = vec![octets];
                while let Ok(octets) = rx.try_recv() {
                    lot.push(octets);
                }
                for octets in lot {
                    if let Some((trame, basse)) = ouvreur.ouvrir(&octets) {
                        m.recues += 1;
                        m.octets += octets.len() as u64;
                        let ecart = horloge_us() - trame.pts_us as i64;
                        m.ecart = Some(m.ecart.map_or((ecart, ecart), |(a, b)| (a.min(ecart), b.max(ecart))));
                        repere.observer(ecart);
                        lecture.recevoir(trame, basse);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }

        // La surface a changé : le décodeur qui peignait dedans s'en va, et le
        // prochain repartira d'une trame clé (`Watch`, au retour de la vue).
        let fenetre = {
            let mut s = verrou(&SURFACE);
            if s.generation != generation {
                generation = s.generation;
                decodeur = None;
                en_attente.clear();
                lecture.repartir();
                derniere = None;
                s.vue = generation;
                SURFACE_VUE.notify_all();
            }
            s.fenetre as *mut ffi::ANativeWindow
        };
        let tele = crate::tele::active();
        if fenetre.is_null() && tele.is_none() {
            lecture.repartir();
            continue;
        }

        let mut prets = lecture.a_decoder(DECODAGE_MS);
        let couche = lecture.basse();
        for t in &mut prets {
            t.pts_us = lissage.lisser(t.pts_us as i64) as u64;
            if let Some((s, _)) = derniere.filter(|(_, c)| *c == couche) {
                perdues += t.seq.saturating_sub(s + 1);
            }
            derniere = Some((t.seq, couche));
        }
        en_attente.extend(prets);
        // L'heure d'affichage d'une trame, en mode fluide : son horodatage plus ce
        // décalage. Sans repère encore, ou en faible latence : tout de suite.
        let repere_courant = repere.decalage();
        let decalage = repere_courant.filter(|_| FLUIDE.load(Ordering::Relaxed)).map(|b| b + TAMPON_FLUIDE_US);
        // Sur la télé : pas de décodeur ici, les trames partent telles quelles.
        if let Some(tele) = &tele {
            decodeur = None;
            tele.noter_decalage(repere_courant);
            if en_attente.is_empty() {
                if derniere_pour_la_tele.elapsed() > IMAGE_IMMOBILE && tele.a_un_lecteur() {
                    derniere_pour_la_tele = Instant::now();
                    let _ = commandes.send(Cmd::Send(ClientMsg::Watch { stream_id, couches: true }));
                }
            } else {
                derniere_pour_la_tele = Instant::now();
                for trame in en_attente.drain(..) {
                    tele.image(&trame);
                }
            }
        }
        while let Some(trame) = en_attente.front() {
            // Pas encore son heure : elle attend ici, compressée, plutôt que d'occuper
            // les quelques tampons de sortie du décodeur.
            if decalage.is_some_and(|d| trame.pts_us as i64 + d - AVANCE_DECODAGE_US > horloge_us()) {
                break;
            }
            // Le décodeur se crée sur une trame clé, et change avec les dimensions :
            // le streamer a tourné son écran, ou le serveur a changé notre qualité.
            let a_changer = decodeur.as_ref().is_none_or(|d| d.largeur != trame.largeur || d.hauteur != trame.hauteur);
            if trame.idr && a_changer {
                decodeur = None;
                tracing::info!("regard : décodeur {}x{}, qualité {}", trame.largeur, trame.hauteur, if couche { "basse" } else { "haute" });
                match Decodeur::new(fenetre, trame.largeur, trame.hauteur, &trame.donnees) {
                    Ok(d) => decodeur = Some(d),
                    Err(e) => tracing::warn!("regard : {e}"),
                }
            }
            let Some(d) = decodeur.as_mut() else {
                en_attente.pop_front();
                continue;
            };
            if !d.envoyer(trame) {
                break;
            }
            en_attente.pop_front();
        }
        if let Some(d) = decodeur.as_mut() {
            let n = d.peindre(&mut m, decalage);
            if n > 0 {
                m.peintes += n;
                let dimensions = (u64::from(d.largeur) << 32) | u64::from(d.hauteur);
                DIMENSIONS.store(dimensions, Ordering::Relaxed);
                if premiere {
                    premiere = false;
                    tracing::info!(
                        "regard : première image {}x{} après {} ms",
                        d.largeur,
                        d.hauteur,
                        depart.elapsed().as_millis()
                    );
                }
            }
        }
        // Le décodeur ne suit plus : on repart de la prochaine trame clé.
        if en_attente.len() > FILE_TRAMES_MAX / 2 {
            reprises += 1;
            tracing::info!("regard : le décodeur ne suit plus ({} en attente)", en_attente.len());
            en_attente.clear();
            lecture.repartir();
            decodeur = None;
        }
        // Rien à lire depuis trop longtemps : on redemande une trame clé.
        if lecture.attend_une_cle() && decodeur.is_none() && derniere_demande.elapsed() > ATTENTE_CLE {
            derniere_demande = Instant::now();
            let _ = commandes.send(Cmd::Send(ClientMsg::Watch { stream_id, couches: true }));
        }
        if periode.elapsed() >= Duration::from_secs(1) {
            let texte = chiffres_du_stream(
                &m,
                periode.elapsed().as_secs_f32(),
                decodeur.as_ref().map(|d| (d.largeur, d.hauteur)),
                lecture.basse(),
                decodeur.as_ref().map_or(0, |d| d.entrees.len()),
                en_attente.len(),
                ping(),
                perdues,
                reprises,
            );
            if bilan.elapsed() >= Duration::from_secs(5) {
                bilan = Instant::now();
                tracing::info!("regard : {}", texte.replace('\n', " | "));
            }
            *verrou(&CHIFFRES) = texte;
            m = Mesures::default();
            periode = Instant::now();
        }
    }
}

fn fil_son(
    stream_id: u32,
    cle: [u8; 32],
    rx: mpsc::Receiver<bytes::Bytes>,
    moteur: Arc<Mutex<Option<ki_voice::VoiceEngine>>>,
    stop: Arc<AtomicBool>,
) {
    let ouvreur = Ouvreur::new(stream_id, cle);
    let mut lecteur = match ki_voice::jeu::Lecteur::new() {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("regard : son indisponible : {e:#}");
            return;
        }
    };
    // Le son reste calé sur l'image : en mode fluide, il attend autant qu'elle.
    let mut retenus: VecDeque<(Instant, u64, Vec<u8>)> = VecDeque::new();
    let mut pour_la_tele: Option<crate::tele::SonTele> = None;
    let attente = Duration::from_micros(TAMPON_FLUIDE_US as u64);
    let mut tele = crate::tele::active();
    while !stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(5)) {
            Ok(datagramme) => {
                if let Some((seq, pts_us, opus)) = ouvreur.ouvrir_son(&datagramme) {
                    match (&tele, pour_la_tele.as_mut()) {
                        (Some(t), Some(son)) => son.paquet(pts_us, &opus, t),
                        _ => retenus.push_back((Instant::now(), seq, opus)),
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        // Sur la télé, le téléphone ne joue rien. Le son y est continu : même sans
        // personne qui parle ni son de jeu, le lecteur ne doit pas l'attendre.
        tele = crate::tele::active();
        if let Some(t) = &tele {
            retenus.clear();
            if pour_la_tele.is_none() {
                pour_la_tele = match crate::tele::SonTele::new() {
                    Ok(son) => Some(son),
                    Err(e) => {
                        tracing::warn!("télé : pas de son : {e}");
                        None
                    }
                };
            }
            if let Some(son) = pour_la_tele.as_mut() {
                son.remplir(t);
            }
            continue;
        }
        pour_la_tele = None;
        let fluide = FLUIDE.load(Ordering::Relaxed);
        while retenus.front().is_some_and(|(quand, _, _)| !fluide || quand.elapsed() >= attente) {
            let Some((_, seq, opus)) = retenus.pop_front() else { break };
            if let Some(moteur) = verrou(&moteur).as_ref() {
                lecteur.jouer(seq, &opus, moteur);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Diffuser son écran
// ---------------------------------------------------------------------------

/// Notre diffusion. `stream_id` et le reste arrivent avec `StreamGranted` : d'ici
/// là, les trames de l'encodeur sont perdues sans dommage.
struct Emission {
    cle: [u8; 32],
    meta: StreamMeta,
    accordee: Option<Accordee>,
    force_idr: bool,
    /// Le débit à tenir (kbit/s), redescendu par le serveur quand ça ne suit pas.
    kbps: u32,
    /// Le serveur l'a arrêtée, ou la connexion est partie : Kotlin l'apprend à la
    /// prochaine trame.
    finie: bool,
}

struct Accordee {
    stream_id: u32,
    scelleur: Scelleur,
    envoi: tokio::sync::mpsc::Sender<(u64, Vec<u8>)>,
    son: Option<ki_voice::jeu::EncodeurSon>,
    commandes: tokio::sync::mpsc::UnboundedSender<Cmd>,
}

static EMISSION: Mutex<Option<Emission>> = Mutex::new(None);

/// Le stream qu'on diffuse, une fois accordé.
pub fn diffusion() -> Option<u32> {
    verrou(&EMISSION).as_ref().and_then(|e| e.accordee.as_ref().map(|a| a.stream_id))
}

/// Une diffusion est en cours, ou attend sa réponse.
pub fn diffusion_demandee() -> bool {
    verrou(&EMISSION).is_some()
}

/// `StreamGranted` : le serveur a donné son numéro à notre diffusion.
pub fn accordee(stream_id: u32, net: &NetHandle) {
    let mut garde = verrou(&EMISSION);
    let Some(e) = garde.as_mut() else { return };
    let (Some(conn), Some(rt)) = (net.connexion(), net.runtime()) else {
        e.finie = true;
        return;
    };
    let seq_son = Arc::new(AtomicU64::new(0));
    let emettre = net.game_audio_emit(stream_id, e.cle, seq_son);
    let son = match ki_voice::jeu::EncodeurSon::new(DEBIT_SON, emettre) {
        Ok(s) => Some(s),
        Err(err) => {
            tracing::warn!("diffusion sans son : {err:#}");
            None
        }
    };
    e.accordee = Some(Accordee {
        stream_id,
        scelleur: Scelleur::new(stream_id, e.cle),
        envoi: lancer_envoi(&rt, conn),
        son,
        commandes: net.commandes(),
    });
    e.force_idr = true;
    tracing::info!("diffusion accordée (stream {stream_id})");
}

/// `KeyframeNeeded` : un spectateur a besoin d'une trame décodable.
pub fn trame_cle_demandee(stream_id: u32) {
    if let Some(e) = verrou(&EMISSION).as_mut().filter(|e| e.accordee.as_ref().is_some_and(|a| a.stream_id == stream_id)) {
        e.force_idr = true;
    }
}

/// `StreamBudget` : le débit que les spectateurs avalent.
pub fn budget(stream_id: u32, kbps: u32) {
    if let Some(e) = verrou(&EMISSION).as_mut().filter(|e| e.accordee.as_ref().is_some_and(|a| a.stream_id == stream_id)) {
        e.kbps = kbps;
    }
}

/// Le serveur arrête ce stream. Vrai : c'était le nôtre.
pub fn terminee(stream_id: u32) -> bool {
    match verrou(&EMISSION).as_mut().filter(|e| e.accordee.as_ref().is_some_and(|a| a.stream_id == stream_id)) {
        Some(e) => {
            e.finie = true;
            true
        }
        None => false,
    }
}

/// La connexion est partie : le serveur a oublié la diffusion, et la clé et la
/// séquence ne resserviront pas. Kotlin arrête tout à la prochaine trame.
pub fn coupure() {
    if let Some(e) = verrou(&EMISSION).as_mut() {
        e.finie = true;
    }
}

// ---------------------------------------------------------------------------
// Le pont avec Kotlin : KiVideo.kt
// ---------------------------------------------------------------------------

/// La surface de la vue native, ou rien quand elle disparaît (appli en arrière-plan,
/// vue fermée). Au retour, `Watch` redemande une trame clé ; en arrière-plan,
/// `Unwatch` : pas de données mobiles pour une image que personne ne voit.
///
/// # Safety
/// Appelée par la JVM, `env` et `surface` valides le temps de l'appel.
#[no_mangle]
pub unsafe extern "system" fn Java_fun_baws_kichat_KiVideo_surface(env: *mut JNIEnv, _classe: jclass, surface: jobject) {
    let fenetre = if surface.is_null() { ptr::null_mut() } else { ffi::ANativeWindow_fromSurface(env, surface) };
    poser_surface(fenetre);
    let Some(partage) = PARTAGE.get() else { return };
    let a = verrou(partage);
    let Some(stream_id) = a.regard.as_ref().map(Regard::stream_id) else { return };
    // Sur la télé, le stream continue : la vue du téléphone n'y est pour rien.
    if fenetre.is_null() && crate::tele::active().is_some() {
        return;
    }
    a.envoyer(if fenetre.is_null() {
        ClientMsg::Unwatch { stream_id }
    } else {
        ClientMsg::Watch { stream_id, couches: true }
    });
}

/// Largeur et hauteur de ce qui est peint : `(largeur << 32) | hauteur`, 0 tant
/// qu'aucune image n'est arrivée. La vue garde ainsi les bonnes proportions.
#[no_mangle]
pub extern "system" fn Java_fun_baws_kichat_KiVideo_dimensions(_env: *mut JNIEnv, _classe: jclass) -> jlong {
    DIMENSIONS.load(Ordering::Relaxed) as jlong
}

/// Fluide (un peu de retard, image régulière) ou faible latence : le choix de la
/// personne, retenu par la vue native.
#[no_mangle]
pub extern "system" fn Java_fun_baws_kichat_KiVideo_fluide(_env: *mut JNIEnv, _classe: jclass, oui: jboolean) {
    FLUIDE.store(oui != JNI_FALSE, Ordering::Relaxed);
}

/// Les chiffres du stream regardé, pour les montrer à qui touche l'écran.
///
/// # Safety
/// Appelée par la JVM, `env` valide le temps de l'appel.
#[no_mangle]
pub unsafe extern "system" fn Java_fun_baws_kichat_KiVideo_chiffres(env: *mut JNIEnv, _classe: jclass) -> jstring {
    let Ok(texte) = std::ffi::CString::new(verrou(&CHIFFRES).as_str()) else { return ptr::null_mut() };
    let Some(creer) = (**env).NewStringUTF else { return ptr::null_mut() };
    creer(env, texte.as_ptr())
}

/// L'utilisateur a accepté la capture : on annonce la diffusion au serveur. Faux :
/// pas connecté, ou pas en vocal, et rien ne doit démarrer.
#[no_mangle]
pub extern "system" fn Java_fun_baws_kichat_KiVideo_demarrerDiffusion(
    _env: *mut JNIEnv,
    _classe: jclass,
    largeur: jint,
    hauteur: jint,
    fps: jint,
    kbps: jint,
) -> jboolean {
    let Some(partage) = PARTAGE.get() else { return JNI_FALSE };
    let mut a = verrou(partage);
    if a.net.is_none() || a.etat.vocal.is_none() {
        return JNI_FALSE;
    }
    let mut cle = [0u8; 32];
    if getrandom::fill(&mut cle).is_err() {
        return JNI_FALSE;
    }
    let meta = StreamMeta {
        width: largeur as u16,
        height: hauteur as u16,
        fps: fps as u8,
        kbps: kbps as u32,
        machine: 0,
        reglage: kbps as u32,
    };
    *verrou(&EMISSION) = Some(Emission { cle, meta, accordee: None, force_idr: true, kbps: kbps as u32, finie: false });
    a.envoyer(ClientMsg::StreamStart { meta, stream_key: ki_protocol::hex_encode(&cle), couches: false });
    a.vue_sale = true;
    JNI_TRUE
}

/// Une trame encodée, dans un `ByteBuffer` direct. Rend -1 si la diffusion est
/// finie, sinon `(kbps << 1) | trame_cle` : le débit à tenir, et s'il faut une trame
/// clé tout de suite.
///
/// # Safety
/// Appelée par la JVM, `env` et `tampon` valides le temps de l'appel.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "system" fn Java_fun_baws_kichat_KiVideo_trame(
    env: *mut JNIEnv,
    _classe: jclass,
    tampon: jobject,
    debut: jint,
    taille: jint,
    pts_us: jlong,
    idr: jboolean,
    largeur: jint,
    hauteur: jint,
) -> jint {
    let mut garde = verrou(&EMISSION);
    let Some(e) = garde.as_mut() else { return -1 };
    if e.finie {
        return -1;
    }
    let Some(lire) = (**env).GetDirectBufferAddress else { return -1 };
    let adresse = lire(env, tampon).cast::<u8>();
    if adresse.is_null() || debut < 0 || taille <= 0 {
        return ((e.kbps as jint) << 1) | jint::from(e.force_idr);
    }
    let Some(a) = e.accordee.as_mut() else {
        return (e.kbps as jint) << 1;
    };
    // SAFETY : Kotlin donne `debut` et `taille` depuis le tampon lui-même.
    let donnees = std::slice::from_raw_parts(adresse.add(debut as usize), taille as usize);
    let cle = idr != JNI_FALSE;
    if cle {
        e.force_idr = false;
    }
    let (l, h) = (largeur as u16, hauteur as u16);
    if (l, h) != (e.meta.width, e.meta.height) {
        e.meta.width = l;
        e.meta.height = h;
        let _ = a.commandes.send(Cmd::Send(ClientMsg::StreamMetaUpdate { meta: e.meta }));
    }
    if let Some((seq, octets)) = a.scelleur.sceller(donnees, cle, pts_us.max(0) as u64, l, h) {
        if a.envoi.try_send((seq, octets)).is_err() {
            // Le réseau ne suit pas : la prochaine trame décodable sera une trame clé.
            e.force_idr = true;
        }
    }
    ((e.kbps as jint) << 1) | jint::from(e.force_idr)
}

/// Du son capturé : flottants stéréo entrelacés à 48 kHz, dont la fin vient d'être
/// capturée à `fin_us`, sur la même horloge que les trames.
///
/// # Safety
/// Appelée par la JVM, `env` et `pcm` valides le temps de l'appel.
#[no_mangle]
pub unsafe extern "system" fn Java_fun_baws_kichat_KiVideo_son(
    env: *mut JNIEnv,
    _classe: jclass,
    pcm: jfloatArray,
    n: jint,
    fin_us: jlong,
) {
    if n <= 0 {
        return;
    }
    let mut copie = vec![0f32; n as usize];
    let Some(lire) = (**env).GetFloatArrayRegion else { return };
    lire(env, pcm, 0, n, copie.as_mut_ptr());
    if let Some(son) = verrou(&EMISSION).as_mut().and_then(|e| e.accordee.as_mut()).and_then(|a| a.son.as_mut()) {
        son.pousser(&copie, fin_us.max(0) as u64);
    }
}

/// La capture s'arrête, pour quelque raison que ce soit.
#[no_mangle]
pub extern "system" fn Java_fun_baws_kichat_KiVideo_arreterDiffusion(_env: *mut JNIEnv, _classe: jclass) {
    let Some(ancienne) = verrou(&EMISSION).take() else { return };
    let Some(partage) = PARTAGE.get() else { return };
    let mut a = verrou(partage);
    // Déjà finie côté serveur : il n'y a plus rien à arrêter.
    if !ancienne.finie {
        a.envoyer(ClientMsg::StreamStop);
    }
    a.spectateurs.clear();
    a.vue_sale = true;
}
