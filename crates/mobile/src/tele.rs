//! Le stream sur une télé (Chromecast, Google TV), le téléphone restant libre.
//!
//! Un Chromecast ne lit qu'une adresse HTTP. Le téléphone reçoit donc le stream comme
//! d'habitude, le réemballe en MP4 ([`ki_core::fmp4`]) sans décoder l'image, et le
//! sert sur le réseau local à la télé que `TeleCast.kt` a chargée de le lire.
//!
//! Le flux part **déchiffré** sur le Wi-Fi : un Chromecast ne saurait rien faire
//! d'autre. Seul un jeton tiré au hasard, dans l'adresse, en garde l'accès.
//!
//! Un seul lecteur à la fois : un nouveau remplace le précédent, et repart d'une trame
//! clé. Le son doit être continu, sans quoi le lecteur s'arrête pour l'attendre : les
//! silences du stream sont remplis ([`SonTele::silence_jusqu_a`]).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jni_sys::{jclass, jint, jstring, JNIEnv};
use ki_core::flux::Trame;
use ki_core::fmp4::{Flux, Sortie};
use ndk_sys as ffi;

use crate::flux::verrou;

/// Environ 25 s de fragments : au-delà, le lecteur ne suit plus, autant qu'il recharge.
const FILE_FRAGMENTS: usize = 256;
/// Une trame de son de 20 ms, en échantillons stéréo entrelacés.
const BLOC_SILENCE: usize = 960 * 2;
const DUREE_BLOC_US: u64 = 20_000;
/// Le son réel arrive avec ce retard au plus sur l'horloge du flux. Au-delà, on remplit :
/// mieux vaut un trou de son qu'un lecteur qui s'arrête pour l'attendre.
const MARGE_SILENCE_US: i64 = 150_000;
/// Un trou plus court ne vaut pas qu'on le remplisse : la trame suivante le rattrape.
const TROU_TOLERE_US: u64 = 15_000;
/// Un trou de plus de ça : pas la peine de tout remplir d'un coup, on saute.
const TROU_MAX_US: u64 = 5_000_000;
const DEBIT_AAC: i32 = 128_000;
const PAS_DE_DECALAGE: i64 = i64::MIN;

static TELE: Mutex<Option<Arc<Tele>>> = Mutex::new(None);
static NUMERO_LECTEUR: AtomicU64 = AtomicU64::new(0);

/// La diffusion vers la télé, si elle est en cours.
pub fn active() -> Option<Arc<Tele>> {
    verrou(&TELE).clone()
}

struct Lecteur {
    numero: u64,
    tx: SyncSender<Vec<u8>>,
}

struct Etat {
    flux: Flux,
    lecteur: Option<Lecteur>,
}

pub struct Tele {
    jeton: String,
    port: u16,
    stop: AtomicBool,
    /// Change quand le lecteur doit recharger l'adresse.
    generation: AtomicU32,
    /// Ce qu'il faut ajouter à l'horloge du flux pour avoir la nôtre.
    decalage: AtomicI64,
    etat: Mutex<Etat>,
}

impl Tele {
    fn demarrer() -> Result<Arc<Self>, String> {
        let mut alea = [0u8; 16];
        getrandom::fill(&mut alea).map_err(|e| format!("hasard : {e}"))?;
        let ecoute = TcpListener::bind("0.0.0.0:0").map_err(|e| format!("écoute : {e}"))?;
        let port = ecoute.local_addr().map_err(|e| format!("écoute : {e}"))?.port();
        ecoute.set_nonblocking(true).map_err(|e| format!("écoute : {e}"))?;
        let tele = Arc::new(Self {
            jeton: ki_protocol::hex_encode(&alea),
            port,
            stop: AtomicBool::new(false),
            generation: AtomicU32::new(0),
            decalage: AtomicI64::new(PAS_DE_DECALAGE),
            etat: Mutex::new(Etat { flux: Flux::default(), lecteur: None }),
        });
        let t = tele.clone();
        std::thread::Builder::new()
            .name("tele-ecoute".into())
            .spawn(move || ecouter(ecoute, t))
            .map_err(|e| format!("fil d'écoute : {e}"))?;
        tracing::info!("télé : le flux est servi sur le port {port}");
        Ok(tele)
    }

    /// Une trame à envoyer, dans l'ordre de lecture.
    pub fn image(&self, t: &Trame) {
        let mut e = verrou(&self.etat);
        if e.lecteur.is_none() {
            return;
        }
        let sortie = e.flux.image(&t.donnees, t.idr, t.pts_us, t.largeur, t.hauteur);
        self.traiter(&mut e, sortie);
    }

    fn son(&self, aac: Vec<u8>, pts_us: u64) {
        let mut e = verrou(&self.etat);
        if e.lecteur.is_none() {
            return;
        }
        let sortie = e.flux.son(aac, pts_us);
        self.traiter(&mut e, sortie);
    }

    fn traiter(&self, e: &mut Etat, sortie: Sortie) {
        match sortie {
            Sortie::Rien => {}
            Sortie::Recharger => {
                tracing::info!("télé : autre résolution, le lecteur recharge");
                self.faire_recharger(e);
            }
            Sortie::Octets(octets) => {
                let Some(lecteur) = e.lecteur.as_ref() else { return };
                match lecteur.tx.try_send(octets) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => {
                        tracing::warn!("télé : le lecteur ne suit plus, il recharge");
                        self.faire_recharger(e);
                    }
                    // Son fil est parti : il va se désabonner.
                    Err(TrySendError::Disconnected(_)) => {}
                }
            }
        }
    }

    fn faire_recharger(&self, e: &mut Etat) {
        e.flux = Flux::default();
        e.lecteur = None;
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Un autre stream commence : si le lecteur avait déjà reçu le précédent, il doit
    /// repartir de zéro. Sinon il attend sa première trame clé, et c'est très bien.
    pub fn repartir(&self) {
        let mut e = verrou(&self.etat);
        if e.flux.commence() {
            self.faire_recharger(&mut e);
        }
    }

    /// Y a-t-il quelqu'un au bout ?
    pub fn a_un_lecteur(&self) -> bool {
        verrou(&self.etat).lecteur.is_some()
    }

    /// Le fil vidéo dit où en est l'horloge du flux.
    pub fn noter_decalage(&self, decalage: Option<i64>) {
        if let Some(d) = decalage {
            self.decalage.store(d, Ordering::Relaxed);
        }
    }

    /// L'heure qu'il est sur l'horloge du flux, moins la marge laissée au son réel.
    fn temps_a_remplir(&self) -> Option<u64> {
        let d = self.decalage.load(Ordering::Relaxed);
        (d != PAS_DE_DECALAGE).then(|| (crate::flux::horloge_us() - d - MARGE_SILENCE_US).max(0) as u64)
    }

    fn abonner(&self, tx: SyncSender<Vec<u8>>) -> u64 {
        let numero = NUMERO_LECTEUR.fetch_add(1, Ordering::Relaxed);
        let mut e = verrou(&self.etat);
        e.flux = Flux::default();
        e.lecteur = Some(Lecteur { numero, tx });
        numero
    }

    fn desabonner(&self, numero: u64) {
        let mut e = verrou(&self.etat);
        if e.lecteur.as_ref().is_some_and(|l| l.numero == numero) {
            e.lecteur = None;
            e.flux = Flux::default();
        }
    }
}

fn ecouter(ecoute: TcpListener, tele: Arc<Tele>) {
    while !tele.stop.load(Ordering::Relaxed) {
        match ecoute.accept() {
            Ok((flot, _)) => {
                let t = tele.clone();
                let _ = std::thread::Builder::new().name("tele-lecteur".into()).spawn(move || servir(flot, &t));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                tracing::warn!("télé : écoute interrompue : {e}");
                return;
            }
        }
    }
}

/// La première ligne d'une requête HTTP : la méthode et le chemin. Le reste des
/// en-têtes ne nous dit rien : pas de reprise au milieu d'un flux sans fin.
fn lire_requete(flot: &mut TcpStream) -> Option<(String, String)> {
    let mut lu = Vec::with_capacity(1024);
    let mut tampon = [0u8; 1024];
    while !lu.windows(4).any(|f| f == b"\r\n\r\n") {
        if lu.len() > 16 * 1024 {
            return None;
        }
        let n = flot.read(&mut tampon).ok()?;
        if n == 0 {
            return None;
        }
        lu.extend_from_slice(&tampon[..n]);
    }
    let texte = String::from_utf8_lossy(&lu);
    let mut ligne = texte.lines().next()?.split(' ');
    Some((ligne.next()?.to_string(), ligne.next()?.to_string()))
}

const CORS: &str = "Access-Control-Allow-Origin: *\r\n\
                    Access-Control-Allow-Methods: GET, HEAD, OPTIONS\r\n\
                    Access-Control-Allow-Headers: *\r\n";

fn servir(mut flot: TcpStream, tele: &Tele) {
    let _ = flot.set_nonblocking(false);
    let _ = flot.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = flot.set_write_timeout(Some(Duration::from_secs(10)));
    let _ = flot.set_nodelay(true);
    let Some((methode, chemin)) = lire_requete(&mut flot) else { return };
    let attendu = format!("/{}/flux.mp4", tele.jeton);
    if chemin.split('?').next() != Some(attendu.as_str()) {
        let _ = flot.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    }
    let entete = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nCache-Control: no-store\r\n\
         Accept-Ranges: none\r\nConnection: close\r\n{CORS}\r\n"
    );
    match methode.as_str() {
        "OPTIONS" => {
            let _ = flot.write_all(format!("HTTP/1.1 204 No Content\r\nConnection: close\r\n{CORS}\r\n").as_bytes());
            return;
        }
        "HEAD" => {
            let _ = flot.write_all(entete.as_bytes());
            return;
        }
        "GET" => {}
        _ => {
            let _ = flot.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            return;
        }
    }
    if flot.write_all(entete.as_bytes()).is_err() {
        return;
    }
    let (tx, rx) = mpsc::sync_channel(FILE_FRAGMENTS);
    let numero = tele.abonner(tx);
    let pair = flot.peer_addr().map_or_else(|_| "?".to_string(), |a| a.ip().to_string());
    tracing::info!("télé : lecteur {numero} connecté depuis {pair}");
    // Le flux part d'une trame clé : autant ne pas attendre la prochaine.
    crate::flux::redemander_cle();
    let (mut total, mut bilan) = (0u64, Instant::now());
    while !tele.stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(octets) => {
                if flot.write_all(&octets).is_err() {
                    break;
                }
                total += octets.len() as u64;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            // Remplacé par un autre lecteur, ou invité à recharger.
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if bilan.elapsed() > Duration::from_secs(10) {
            bilan = Instant::now();
            tracing::info!("télé : lecteur {numero}, {} Mo envoyés", total / 1_000_000);
        }
    }
    tele.desabonner(numero);
    tracing::info!("télé : lecteur {numero} parti, {} Mo envoyés", total / 1_000_000);
}

// ---------------------------------------------------------------------------
// Le son, en AAC
// ---------------------------------------------------------------------------

/// L'encodeur AAC matériel d'Android.
struct EncodeurAac {
    codec: *mut ffi::AMediaCodec,
}

impl EncodeurAac {
    fn new() -> Result<Self, String> {
        // SAFETY : suite d'appels de l'API NDK, chaque pointeur vérifié avant usage ; le
        // format est libéré ici, le codec par `Drop`.
        unsafe {
            let codec = ffi::AMediaCodec_createEncoderByType(c"audio/mp4a-latm".as_ptr());
            if codec.is_null() {
                return Err("pas d'encodeur AAC sur ce téléphone".into());
            }
            let encodeur = Self { codec };
            let format = ffi::AMediaFormat_new();
            ffi::AMediaFormat_setString(format, c"mime".as_ptr(), c"audio/mp4a-latm".as_ptr());
            ffi::AMediaFormat_setInt32(format, c"sample-rate".as_ptr(), 48_000);
            ffi::AMediaFormat_setInt32(format, c"channel-count".as_ptr(), 2);
            ffi::AMediaFormat_setInt32(format, c"bitrate".as_ptr(), DEBIT_AAC);
            // AAC-LC, le seul qu'on annonce dans l'en-tête du MP4.
            ffi::AMediaFormat_setInt32(format, c"aac-profile".as_ptr(), 2);
            let configure = ffi::AMediaCodec_configure(
                codec,
                format,
                ptr::null_mut(),
                ptr::null_mut(),
                ffi::AMEDIACODEC_CONFIGURE_FLAG_ENCODE as u32,
            );
            ffi::AMediaFormat_delete(format);
            if configure != ffi::media_status_t::AMEDIA_OK {
                return Err(format!("configuration de l'encodeur AAC refusée ({configure:?})"));
            }
            let demarre = ffi::AMediaCodec_start(codec);
            if demarre != ffi::media_status_t::AMEDIA_OK {
                return Err(format!("démarrage de l'encodeur AAC refusé ({demarre:?})"));
            }
            Ok(encodeur)
        }
    }

    /// Du PCM 16 bits stéréo entrelacé, daté de `pts_us`. `false` : pas de place, rien
    /// n'est entré. Un tampon d'entrée trop petit, et le bloc passe en plusieurs fois.
    fn pousser(&mut self, pcm: &[i16], pts_us: u64) -> bool {
        let mut fait = 0;
        while fait < pcm.len() {
            // SAFETY : l'index vient de `dequeueInputBuffer`, le tampon rendu par le codec
            // fait `capacite` octets, et l'on n'y écrit pas au-delà.
            unsafe {
                let index = ffi::AMediaCodec_dequeueInputBuffer(self.codec, 5_000);
                if index < 0 {
                    return fait > 0;
                }
                let mut capacite = 0usize;
                let tampon = ffi::AMediaCodec_getInputBuffer(self.codec, index as usize, &mut capacite);
                // Une paire d'échantillons fait 4 octets : on n'en coupe jamais une.
                let n = (pcm.len() - fait).min(capacite / 4 * 2);
                let quand = pts_us + (fait as u64 / 2) * 1_000_000 / 48_000;
                if tampon.is_null() || n == 0 {
                    ffi::AMediaCodec_queueInputBuffer(self.codec, index as usize, 0, 0, quand, 0);
                    return fait > 0;
                }
                ptr::copy_nonoverlapping(pcm[fait..].as_ptr().cast::<u8>(), tampon, n * 2);
                ffi::AMediaCodec_queueInputBuffer(self.codec, index as usize, 0, n * 2, quand, 0);
                fait += n;
            }
        }
        true
    }

    /// Ce qui est sorti de l'encodeur.
    fn vider(&mut self, mut sortie: impl FnMut(Vec<u8>, u64)) {
        loop {
            let mut info = ffi::AMediaCodecBufferInfo { offset: 0, size: 0, presentationTimeUs: 0, flags: 0 };
            // SAFETY : `info` vit le temps de l'appel ; l'index rendu n'est relâché qu'une
            // fois, et l'on ne lit que ce que `info` annonce du tampon.
            unsafe {
                let index = ffi::AMediaCodec_dequeueOutputBuffer(self.codec, &mut info, 0);
                if index >= 0 {
                    // La configuration du codec : elle est déjà dans l'en-tête du MP4.
                    let configuration = info.flags & ffi::AMEDIACODEC_BUFFER_FLAG_CODEC_CONFIG != 0;
                    if !configuration && info.size > 0 {
                        let mut taille = 0usize;
                        let tampon = ffi::AMediaCodec_getOutputBuffer(self.codec, index as usize, &mut taille);
                        let (debut, n) = (info.offset as usize, info.size as usize);
                        if !tampon.is_null() && debut + n <= taille {
                            let trame = std::slice::from_raw_parts(tampon.add(debut), n).to_vec();
                            sortie(trame, info.presentationTimeUs.max(0) as u64);
                        }
                    }
                    ffi::AMediaCodec_releaseOutputBuffer(self.codec, index as usize, false);
                } else if index == ffi::AMEDIACODEC_INFO_TRY_AGAIN_LATER as isize {
                    return;
                }
            }
        }
    }
}

impl Drop for EncodeurAac {
    fn drop(&mut self) {
        // SAFETY : le codec vient de `createEncoderByType`, libéré une seule fois.
        unsafe {
            ffi::AMediaCodec_stop(self.codec);
            ffi::AMediaCodec_delete(self.codec);
        }
    }
}

/// Le son du stream, de l'Opus reçu à l'AAC de la télé, sur un fil.
pub struct SonTele {
    opus: ki_voice::jeu::DecodeurSon,
    aac: EncodeurAac,
    pcm: Vec<i16>,
    /// L'heure, sur l'horloge du flux, du prochain échantillon à encoder.
    suivant: Option<u64>,
    /// Un bloc de silence tout prêt : l'encodeur regroupe en trames AAC tout seul.
    silence: Vec<i16>,
}

impl SonTele {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            opus: ki_voice::jeu::DecodeurSon::new().map_err(|e| format!("{e:#}"))?,
            aac: EncodeurAac::new()?,
            pcm: Vec::new(),
            suivant: None,
            silence: vec![0; BLOC_SILENCE],
        })
    }

    /// Un paquet Opus reçu, daté sur l'horloge du flux.
    pub fn paquet(&mut self, pts_us: u64, opus: &[u8], tele: &Tele) {
        let Some(pcm) = self.opus.decoder(opus) else { return };
        self.pcm.clear();
        self.pcm.extend(pcm.iter().map(|e| (e.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16));
        let duree = self.pcm.len() as u64 / 2 * 1_000_000 / 48_000;
        let suivant = *self.suivant.get_or_insert(pts_us);
        if pts_us.saturating_add(duree) <= suivant {
            return; // En retard : le silence a déjà pris sa place.
        }
        if pts_us > suivant + TROU_MAX_US {
            self.suivant = Some(pts_us);
        } else if pts_us > suivant + TROU_TOLERE_US {
            self.silence_jusqu_a(pts_us, tele);
        }
        let quand = self.suivant.unwrap_or(pts_us);
        if self.aac.pousser(&self.pcm, quand) {
            self.suivant = Some(quand + duree);
        }
        self.sortir(tele);
    }

    /// Le temps passe sans que le son arrive : le lecteur ne doit pas l'attendre.
    pub fn remplir(&mut self, tele: &Tele) {
        if let Some(cible) = tele.temps_a_remplir() {
            self.silence_jusqu_a(cible, tele);
        }
    }

    fn silence_jusqu_a(&mut self, cible: u64, tele: &Tele) {
        let mut quand = *self.suivant.get_or_insert(cible);
        if cible > quand + TROU_MAX_US {
            quand = cible;
        }
        while quand < cible {
            if !self.aac.pousser(&self.silence, quand) {
                break;
            }
            quand += DUREE_BLOC_US;
            self.sortir(tele);
        }
        self.suivant = Some(quand);
    }

    fn sortir(&mut self, tele: &Tele) {
        self.aac.vider(|trame, pts| tele.son(trame, pts));
    }
}

// ---------------------------------------------------------------------------
// Le pont avec Kotlin : KiVideo.kt, TeleCast.kt
// ---------------------------------------------------------------------------

/// Démarre le service du flux, s'il ne tourne pas : rend `port/jeton/flux.mp4`, la fin
/// de l'adresse à donner à la télé, ou rien si c'est impossible.
///
/// # Safety
/// Appelée par la JVM, `env` valide le temps de l'appel.
#[no_mangle]
pub unsafe extern "system" fn Java_fun_baws_kichat_KiVideo_teleDemarrer(env: *mut JNIEnv, _classe: jclass) -> jstring {
    let tele = {
        let mut garde = verrou(&TELE);
        match garde.as_ref() {
            Some(t) => t.clone(),
            None => match Tele::demarrer() {
                Ok(t) => {
                    *garde = Some(t.clone());
                    t
                }
                Err(e) => {
                    tracing::warn!("télé : {e}");
                    return ptr::null_mut();
                }
            },
        }
    };
    let Ok(texte) = std::ffi::CString::new(format!("{}/{}/flux.mp4", tele.port, tele.jeton)) else {
        return ptr::null_mut();
    };
    let Some(creer) = (**env).NewStringUTF else { return ptr::null_mut() };
    creer(env, texte.as_ptr())
}

/// Plus de télé : le service s'arrête, et le stream ne part plus vers elle.
#[no_mangle]
pub extern "system" fn Java_fun_baws_kichat_KiVideo_teleArreter(_env: *mut JNIEnv, _classe: jclass) {
    if let Some(t) = verrou(&TELE).take() {
        t.stop.store(true, Ordering::Relaxed);
        verrou(&t.etat).lecteur = None;
        tracing::info!("télé : le flux n'est plus servi");
    }
}

/// Change quand le lecteur doit recharger l'adresse : autre résolution, autre stream.
#[no_mangle]
pub extern "system" fn Java_fun_baws_kichat_KiVideo_teleGeneration(_env: *mut JNIEnv, _classe: jclass) -> jint {
    active().map_or(0, |t| t.generation.load(Ordering::Relaxed) as jint)
}
