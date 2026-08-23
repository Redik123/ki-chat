//! Moteur audio client ki-chat.
//!
//! Pipeline émission : micro (cpal) -> mono -> rééchantillonnage 48 kHz
//!   -> trames 20 ms -> encodage Opus -> UDP vers le serveur.
//! Pipeline réception : UDP -> jitter buffer par émetteur -> décodage Opus
//!   (avec dissimulation de perte) -> mixage -> rééchantillonnage -> sortie.
//!
//! Tout est en threads std : aucun runtime async requis, le moteur est
//! indépendant du reste du client.

pub mod effects;
mod jitter;
mod resample;
#[cfg(windows)]
mod wasapi;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use chacha20poly1305::aead::{Aead, KeyInit};
use ki_opus as opus;
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ki_protocol::{parse_voice_packet, write_voice_header, VOICE_HEADER_LEN, VOICE_MAX_PACKET};

use jitter::Receiver;
use resample::CubicResampler;

/// Fréquence interne du moteur (celle d'Opus).
pub const SAMPLE_RATE: u32 = 48_000;
/// Trames de 20 ms.
pub const FRAME_SAMPLES: usize = 960;

// ---------------------------------------------------------------------------
// Journal audio
// ---------------------------------------------------------------------------

/// Nombre d'événements conservés dans le journal audio.
const JOURNAL_CAP: usize = 200;

/// Journal des événements audio, en mémoire, hors de tout moteur : les
/// symptômes « le micro a bugué quand le jeu s'est lancé » varient d'un
/// utilisateur à l'autre, et le moteur est recréé à chaque changement de
/// périphérique — un journal porté par le moteur ne raconterait que la fin
/// de l'histoire. Celui-ci couvre la session entière, et les réglages
/// l'affichent : on se fait envoyer le journal au lieu de deviner.
static JOURNAL: Mutex<std::collections::VecDeque<(u64, String)>> =
    Mutex::new(std::collections::VecDeque::new());

/// Consigne un événement audio, horodaté (epoch millis). Les appels doublent
/// une trace `tracing` : le journal parle à l'utilisateur, la trace au
/// développeur.
fn journal(msg: String) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut j = JOURNAL.lock().unwrap();
    j.push_back((now, msg));
    while j.len() > JOURNAL_CAP {
        j.pop_front();
    }
}

/// Les derniers événements audio : (epoch millis, message), du plus ancien au
/// plus récent.
pub fn journal_snapshot() -> Vec<(u64, String)> {
    JOURNAL.lock().unwrap().iter().cloned().collect()
}

/// Émetteur de datagrammes voix : le moteur est indépendant du transport
/// (QUIC aujourd'hui, autre chose demain). L'appel ne doit jamais bloquer.
pub type DatagramSend = std::sync::Arc<dyn Fn(&[u8]) + Send + Sync>;

pub struct VoiceConfig {
    /// Notre identité (en-tête des paquets + nonce de chiffrement).
    pub user_id: u64,
    /// Clé de session XChaCha20-Poly1305 remise par le serveur dans Welcome.
    pub key: [u8; 32],
    /// Débit Opus en bits/s (64 000 = très bonne qualité voix).
    pub bitrate: i32,
    /// Périphérique d'entrée à utiliser (nom cpal), None = défaut système.
    pub input_device: Option<String>,
    /// Périphérique de sortie à utiliser (nom cpal), None = défaut système.
    pub output_device: Option<String>,
    /// Moteur audio Windows natif (WASAPI direct) : rôle communication,
    /// conversion de format par Windows, notifications. Sans effet hors
    /// Windows, et retombe sur cpal à la moindre erreur d'ouverture.
    pub native_audio: bool,
    /// Mode brut du micro : demande à Windows de court-circuiter les effets
    /// tiers (Sonar, Nahimic…). Moteur natif seulement.
    pub raw_mic: bool,
    /// Micro en catégorie « communications » : partage la voie de traitement
    /// avec la voix intégrée des jeux (nécessaire quand elle affame notre
    /// capture), au prix de l'« appel permanent » côté Windows — volume des
    /// autres sons réduit chez qui n'a pas réglé « Ne rien faire ». Faux par
    /// défaut ; le moteur y bascule de lui-même s'il détecte la famine.
    pub comms_mic: bool,
    /// Mode de suppression de bruit (NOISE_OFF / NOISE_RNNOISE).
    pub noise_mode: u8,
    /// Volumes par émetteur (user_id -> gain, 1.0 = 100 %), appliqués au mixage.
    pub volumes: HashMap<u64, f32>,
    /// Gain d'entrée micro (1.0 = 100 %).
    pub input_gain: f32,
    /// Volume de sortie global (1.0 = 100 %).
    pub output_gain: f32,
    /// Seuil d'activation vocale (0.0 = désactivé, sinon niveau crête 0..1).
    pub vad_threshold: f32,
    /// Maintien d'émission après la dernière voix détectée (VAD), en ms.
    pub vad_hangover_ms: u32,
    /// Gain automatique (AGC) : normalise le niveau micro tout seul.
    pub agc: bool,
    /// Niveau crête cible de l'AGC (0..1).
    pub agc_target: f32,
    /// Porte de bruit : en-dessous de ce niveau, le micro est atténué
    /// progressivement (0.0 = désactivée).
    pub gate_threshold: f32,
    /// Taille de tampon de gigue imposée en trames (0 = adaptatif).
    pub jitter_frames: usize,
    /// Mode test : émet une sinusoïde 440 Hz au lieu du micro.
    pub tone: bool,
    /// Mode test : ne lit pas l'audio reçu (compte seulement les paquets).
    pub no_playback: bool,
}

impl VoiceConfig {
    pub fn new(user_id: u64, key: [u8; 32]) -> Self {
        Self {
            user_id,
            key,
            bitrate: 64_000,
            input_device: None,
            output_device: None,
            native_audio: cfg!(windows),
            raw_mic: false,
            comms_mic: false,
            noise_mode: NOISE_RNNOISE,
            volumes: HashMap::new(),
            input_gain: 1.0,
            output_gain: 1.0,
            vad_threshold: 0.0,
            vad_hangover_ms: 400,
            agc: true,
            agc_target: 0.30,
            gate_threshold: 0.0,
            jitter_frames: 0,
            tone: false,
            no_playback: false,
        }
    }
}

/// Valeur DRED « activé » à passer à set_dred : ~1 s de passé re-transmis
/// dans chaque paquet (unités du CTL opus).
pub const DRED_DEFAULT: i32 = 100;

/// Modes de suppression de bruit.
pub const NOISE_OFF: u8 = 0;
pub const NOISE_RNNOISE: u8 = 1;
/// DeepFilterNet3 : qualité studio, ajoute ~30 ms de latence (lookahead).
pub const NOISE_DEEP: u8 = 2;

/// Noms des périphériques audio disponibles : (entrées, sorties).
pub fn list_devices() -> (Vec<String>, Vec<String>) {
    fn names(devs: Option<impl Iterator<Item = cpal::Device>>) -> Vec<String> {
        devs.map(|d| d.filter_map(|dev| dev.name().ok()).collect())
            .unwrap_or_default()
    }
    let host = cpal::default_host();
    (
        names(host.input_devices().ok()),
        names(host.output_devices().ok()),
    )
}

/// Nom « stable » d'un périphérique : Windows préfixe d'un compteur le nom
/// d'un périphérique ré-énuméré — « Microphone (2- HyperX QuadCast) » —
/// typiquement quand un jeu au lancement secoue l'USB. Même matériel, nouveau
/// nom : sans cette normalisation, le nom réglé ne correspondait plus et l'on
/// se repliait en silence sur le défaut (souvent le micro de la webcam),
/// jusqu'à la fin de la session.
fn normalized_device_name(name: &str) -> String {
    /// « 2- Casque » -> « Casque », en début de nom ou juste après « ( ».
    fn strip_counter(s: &str) -> &str {
        let digits = s.bytes().take_while(u8::is_ascii_digit).count();
        match s[digits..].strip_prefix("- ") {
            Some(rest) if digits > 0 => rest,
            _ => s,
        }
    }
    let mut out = String::with_capacity(name.len());
    let mut rest = strip_counter(name.trim());
    while let Some(i) = rest.find('(') {
        out.push_str(&rest[..=i]);
        rest = strip_counter(&rest[i + 1..]);
    }
    out.push_str(rest);
    out
}

/// Trouve un périphérique par nom : exact d'abord — deux exemplaires du même
/// casque réellement branchés restent distincts — puis débarrassé du compteur
/// de ré-énumération.
fn find_by_name(host: &cpal::Host, name: &str, input: bool) -> Option<cpal::Device> {
    let devices = |host: &cpal::Host| {
        if input { host.input_devices().ok() } else { host.output_devices().ok() }
    };
    if let Some(d) = devices(host)?.find(|d| d.name().is_ok_and(|n| n == name)) {
        return Some(d);
    }
    let want = normalized_device_name(name);
    devices(host)?.find(|d| d.name().is_ok_and(|n| normalized_device_name(&n) == want))
}

/// Le périphérique demandé est-il présent ?
///
/// Sert à savoir quand un casque rebranché est de retour : tant qu'on tourne
/// sur un repli, il faut bien reposer la question de temps en temps.
fn device_present(host: &cpal::Host, name: &str, input: bool) -> bool {
    find_by_name(host, name, input).is_some()
}

/// Le périphérique demandé, ou le défaut à défaut.
///
/// Le second membre dit s'il a fallu se rabattre : l'appelant s'en sert pour
/// prévenir l'utilisateur, et surtout pour reposer la question plus tard.
/// Aucune trace ici : la sonde périodique passe aussi par là, et
/// journaliserait la même absence toutes les 3 secondes.
fn pick_device(
    host: &cpal::Host,
    name: Option<&str>,
    input: bool,
) -> (Option<cpal::Device>, bool) {
    if let Some(name) = name {
        if let Some(d) = find_by_name(host, name, input) {
            return (Some(d), false);
        }
        let fallback =
            if input { host.default_input_device() } else { host.default_output_device() };
        return (fallback, true);
    }
    let d = if input { host.default_input_device() } else { host.default_output_device() };
    (d, false)
}

/// Nonce XChaCha20 (24 octets) dérivé de l'émetteur et du compteur de paquet.
/// Unique par clé : les user_id sont uniques et le compteur ne se répète pas.
fn nonce_for(user_id: u64, counter: u64) -> XNonce {
    let mut n = [0u8; 24];
    n[..8].copy_from_slice(&user_id.to_le_bytes());
    n[8..16].copy_from_slice(&counter.to_le_bytes());
    XNonce::from(n)
}

#[derive(Debug, Default, Clone)]
pub struct VoiceStats {
    pub packets_sent: u64,
    pub packets_received: u64,
    pub packets_lost: u64,
    /// Trames perdues reconstruites grâce au FEC.
    pub packets_recovered: u64,
    /// Paquets dont le déchiffrement a échoué (corruption ou intrusion).
    pub packets_rejected: u64,
    pub active_senders: usize,
    /// Niveau crête du micro sur le dernier bloc capturé (0.0 = silence).
    pub mic_peak: f32,
    /// Échantillons de voix distante réellement mixés vers la sortie.
    pub samples_played: u64,
    /// Pire gigue réseau mesurée parmi les locuteurs actifs, en ms.
    pub worst_jitter_ms: f32,
}

#[derive(Default)]
struct Counters {
    sent: AtomicU64,
    received: AtomicU64,
    lost: AtomicU64,
    recovered: AtomicU64,
    rejected: AtomicU64,
    mic_peak_bits: std::sync::atomic::AtomicU32,
    played: AtomicU64,
}

use std::sync::atomic::{AtomicI32, AtomicU32};

/// Atomic pour un f32 (stocké en bits).
fn store_f32(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Ordering::Relaxed);
}
fn load_f32(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

struct Shared {
    cipher: XChaCha20Poly1305,
    /// Micro « armé » par l'application (mute/PTT/mode).
    transmitting: AtomicBool,
    /// Émission réellement en cours (armé ET seuil vocal franchi).
    sending: AtomicBool,
    /// Mode de suppression de bruit (NOISE_*).
    noise_mode: std::sync::atomic::AtomicU8,
    shutdown: AtomicBool,
    /// Gain d'entrée micro (bits f32).
    input_gain: AtomicU32,
    /// Volume de sortie global (bits f32).
    output_gain: AtomicU32,
    /// Seuil d'activation vocale (bits f32, 0.0 = désactivé).
    vad_threshold: AtomicU32,
    /// Gain automatique actif.
    agc: AtomicBool,
    /// Niveau crête cible de l'AGC (bits f32).
    agc_target: AtomicU32,
    /// Porte de bruit (bits f32, 0.0 = désactivée).
    gate_threshold: AtomicU32,
    /// Maintien VAD en ms.
    vad_hangover_ms: AtomicU32,
    /// Tampon de gigue imposé en trames (0 = adaptatif).
    jitter_override: std::sync::atomic::AtomicUsize,
    /// Débit Opus demandé (appliqué à chaud par le thread de capture).
    bitrate: AtomicI32,
    /// Durée de redondance neuronale DRED demandée (0 = désactivé),
    /// dans les unités du CTL opus. Appliquée à chaud.
    dred: AtomicI32,
    /// Retour local du micro (« s'écouter ») actif.
    loopback: AtomicBool,
    /// Échantillons locaux à mixer dans la sortie (test micro / son de test).
    loopback_buf: Mutex<std::collections::VecDeque<f32>>,
    /// Effets sonores en attente de lecture. File séparée du loopback :
    /// celui-ci est vidé dès qu'on coupe le retour local, et le fil de
    /// capture le rogne à 250 ms tant qu'il est actif — une notification
    /// d'une seconde y serait tronquée ou effacée.
    effects_buf: Mutex<std::collections::VecDeque<f32>>,
    /// Volume des effets sonores (bits f32), indépendant des voix.
    effects_gain: AtomicU32,
    /// Le périphérique d'entrée a disparu et l'on tente de le rouvrir.
    /// Débrancher un micro USB, ou un casque sans fil qui s'endort, coupe le
    /// flux cpal sans que rien ne le relance : ces drapeaux permettent de le
    /// dire à l'utilisateur au lieu de le laisser parler dans le vide.
    /// Génération de réinitialisation audio : quand elle change, les boucles
    /// capture et lecture ferment et rouvrent leur périphérique — l'effet
    /// d'un débranchement/rebranchement, sans toucher au câble.
    audio_reset: std::sync::atomic::AtomicU64,
    input_lost: AtomicBool,
    /// On capte, mais sur un périphérique de repli : celui qui est réglé n'a
    /// pas été trouvé. C'est un état distinct de la perte — le son passe.
    input_fallback: AtomicBool,
    /// Idem pour la sortie.
    output_lost: AtomicBool,
    /// Idem pour la sortie.
    output_fallback: AtomicBool,
    /// État de décodage par locuteur : fil réseau, et lectures d'information
    /// depuis l'interface. **Jamais** le rappel de sortie.
    receivers: Mutex<HashMap<u64, Receiver>>,
    /// Tampons de lecture par locuteur, tenus à part de `receivers`.
    ///
    /// C'est la carte que consulte le rappel de sortie, et lui seul la partage
    /// avec le fil réseau — qui n'y dépose que des échantillons déjà décodés.
    /// Les faire cohabiter dans `receivers` mettait le décodage (masquage de
    /// perte neuronal, reconstruction DRED) sur le chemin du fil temps réel.
    playouts: Mutex<HashMap<u64, Arc<Mutex<crate::jitter::Playout>>>>,
    /// user_id -> gain de mixage (1.0 = 100 %). Absent = 1.0.
    volumes: Mutex<HashMap<u64, f32>>,
    counters: Counters,
}

pub struct VoiceEngine {
    shared: Arc<Shared>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl VoiceEngine {
    /// Démarre le moteur. `send` émet un datagramme voix (sans bloquer),
    /// `incoming` reçoit les datagrammes voix entrants — le transport
    /// (QUIC) vit ailleurs, le moteur n'en sait rien.
    pub fn start(
        cfg: VoiceConfig,
        send: DatagramSend,
        incoming: std::sync::mpsc::Receiver<Vec<u8>>,
    ) -> anyhow::Result<Self> {
        let shared = Arc::new(Shared {
            cipher: XChaCha20Poly1305::new(&cfg.key.into()),
            transmitting: AtomicBool::new(false),
            sending: AtomicBool::new(false),
            noise_mode: std::sync::atomic::AtomicU8::new(cfg.noise_mode),
            shutdown: AtomicBool::new(false),
            input_gain: AtomicU32::new(cfg.input_gain.to_bits()),
            output_gain: AtomicU32::new(cfg.output_gain.to_bits()),
            vad_threshold: AtomicU32::new(cfg.vad_threshold.to_bits()),
            agc: AtomicBool::new(cfg.agc),
            agc_target: AtomicU32::new(cfg.agc_target.to_bits()),
            gate_threshold: AtomicU32::new(cfg.gate_threshold.to_bits()),
            vad_hangover_ms: AtomicU32::new(cfg.vad_hangover_ms),
            jitter_override: std::sync::atomic::AtomicUsize::new(cfg.jitter_frames),
            bitrate: AtomicI32::new(cfg.bitrate),
            dred: AtomicI32::new(0),
            loopback: AtomicBool::new(false),
            loopback_buf: Mutex::new(std::collections::VecDeque::new()),
            effects_buf: Mutex::new(std::collections::VecDeque::new()),
            effects_gain: AtomicU32::new(1.0f32.to_bits()),
            audio_reset: std::sync::atomic::AtomicU64::new(0),
            input_lost: AtomicBool::new(false),
            input_fallback: AtomicBool::new(false),
            output_lost: AtomicBool::new(false),
            output_fallback: AtomicBool::new(false),
            receivers: Mutex::new(HashMap::new()),
            playouts: Mutex::new(HashMap::new()),
            volumes: Mutex::new(cfg.volumes.clone()),
            counters: Counters::default(),
        });

        journal("moteur vocal démarré".into());
        let mut threads = Vec::new();

        // --- Réception réseau (datagrammes remontés par le transport) ---
        {
            let sh = shared.clone();
            threads.push(std::thread::spawn(move || recv_loop(sh, incoming)));
        }

        // --- Émission : micro réel ou sinusoïde de test ---
        {
            let sh = shared.clone();
            let user_id = cfg.user_id;
            let bitrate = cfg.bitrate;
            let input_device = cfg.input_device.clone();
            let native = cfg.native_audio;
            let raw = cfg.raw_mic;
            let comms = cfg.comms_mic;
            let send = send.clone();
            if cfg.tone {
                threads.push(std::thread::spawn(move || tone_loop(sh, user_id, bitrate, send)));
            } else {
                threads.push(std::thread::spawn(move || {
                    if let Err(e) =
                        capture_loop(sh, user_id, bitrate, input_device, native, raw, comms, send)
                    {
                        tracing::error!("capture micro : {e:#}");
                    }
                }));
            }
        }

        // --- Lecture ---
        if !cfg.no_playback {
            let sh = shared.clone();
            let output_device = cfg.output_device.clone();
            let native = cfg.native_audio;
            threads.push(std::thread::spawn(move || {
                if let Err(e) = playback_loop(sh, output_device, native) {
                    tracing::error!("sortie audio : {e:#}");
                }
            }));
        }

        Ok(Self { shared, threads })
    }

    /// Active/coupe l'émission micro (l'équivalent du push-to-talk).
    pub fn set_transmit(&self, on: bool) {
        self.shared.transmitting.store(on, Ordering::Relaxed);
    }

    /// Change le mode de suppression de bruit (NOISE_*), à chaud.
    pub fn set_noise_mode(&self, mode: u8) {
        self.shared.noise_mode.store(mode, Ordering::Relaxed);
    }

    /// Niveau crête cible de l'AGC, à chaud.
    pub fn set_agc_target(&self, target: f32) {
        store_f32(&self.shared.agc_target, target.clamp(0.05, 0.8));
    }

    /// Porte de bruit (0.0 = désactivée), à chaud.
    pub fn set_gate_threshold(&self, threshold: f32) {
        store_f32(&self.shared.gate_threshold, threshold.clamp(0.0, 0.5));
    }

    /// Maintien d'émission VAD en ms, à chaud.
    pub fn set_vad_hangover_ms(&self, ms: u32) {
        self.shared.vad_hangover_ms.store(ms.clamp(50, 2000), Ordering::Relaxed);
    }

    /// Impose une taille de tampon de gigue en trames (0 = adaptatif), à chaud.
    pub fn set_jitter_frames(&self, frames: usize) {
        self.shared.jitter_override.store(frames, Ordering::Relaxed);
        let over = if frames == 0 { None } else { Some(frames) };
        let mut receivers = self.shared.receivers.lock().unwrap();
        for rx in receivers.values_mut() {
            rx.set_jitter_override(over);
        }
    }

    /// Règle le volume d'un émetteur (1.0 = 100 %), appliqué au mixage.
    pub fn set_user_volume(&self, user_id: u64, gain: f32) {
        let mut volumes = self.shared.volumes.lock().unwrap();
        if (gain - 1.0).abs() < 0.001 {
            volumes.remove(&user_id);
        } else {
            volumes.insert(user_id, gain.clamp(0.0, 4.0));
        }
    }

    /// Gain d'entrée micro (1.0 = 100 %), à chaud.
    pub fn set_input_gain(&self, gain: f32) {
        store_f32(&self.shared.input_gain, gain.clamp(0.0, 4.0));
    }

    /// Volume de sortie global (1.0 = 100 %), à chaud.
    pub fn set_output_gain(&self, gain: f32) {
        store_f32(&self.shared.output_gain, gain.clamp(0.0, 4.0));
    }

    /// Seuil d'activation vocale (0.0 = désactivé), à chaud.
    pub fn set_vad_threshold(&self, threshold: f32) {
        store_f32(&self.shared.vad_threshold, threshold.clamp(0.0, 1.0));
    }

    /// Débit Opus en bits/s, appliqué à chaud.
    pub fn set_bitrate(&self, bitrate: i32) {
        self.shared.bitrate.store(bitrate.clamp(8_000, 256_000), Ordering::Relaxed);
    }

    /// Redondance neuronale DRED : durée de passé re-transmis dans chaque
    /// paquet (unités du CTL opus, 0 = désactivé), à chaud.
    pub fn set_dred(&self, value: i32) {
        self.shared.dred.store(value.max(0), Ordering::Relaxed);
    }

    /// Active/coupe le retour local du micro (« s'écouter »).
    /// Ferme et rouvre le micro ET la sortie — l'effet d'un débranchement/
    /// rebranchement du casque, sans toucher au câble. Pour les pilotes
    /// (Razer & co) qui laissent un flux zombie après le passage d'un jeu.
    pub fn reset_audio_devices(&self) {
        self.shared.audio_reset.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_loopback(&self, on: bool) {
        self.shared.loopback.store(on, Ordering::Relaxed);
        if !on {
            self.shared.loopback_buf.lock().unwrap().clear();
        }
    }

    /// Joue un effet sonore (PCM mono 48 kHz, cf. `effects::load_wav`).
    /// Le son est mixé à ce qui reste en file : deux événements rapprochés
    /// se superposent au lieu que le second coupe le premier.
    pub fn play_effect(&self, pcm: &[f32], gain: f32) {
        let mut buf = self.shared.effects_buf.lock().unwrap();
        effects::queue(&mut buf, pcm, gain.clamp(0.0, 2.0));
    }

    /// Volume des effets sonores (1.0 = 100 %), à chaud. S'applique à la
    /// lecture, donc aussi aux sons déjà en file.
    pub fn set_effects_gain(&self, gain: f32) {
        store_f32(&self.shared.effects_gain, gain.clamp(0.0, 2.0));
    }

    /// Joue une tonalité de test (~0,5 s) sur la sortie.
    pub fn play_test_tone(&self) {
        let step = 2.0 * std::f32::consts::PI * 440.0 / SAMPLE_RATE as f32;
        let n = SAMPLE_RATE as usize / 2;
        let tone: Vec<f32> = (0..n)
            .map(|i| {
                // Petite enveloppe pour éviter les clics au début/à la fin.
                let env = (i.min(n - i) as f32 / 2400.0).min(1.0);
                (i as f32 * step).sin() * 0.25 * env
            })
            .collect();
        self.play_effect(&tone, 1.0);
    }

    /// Périphériques perdus : (micro, sortie). Vrai tant que la réouverture
    /// n'a pas abouti — un casque débranché, ou qui sort de veille.
    pub fn device_trouble(&self) -> (bool, bool) {
        (
            self.shared.input_lost.load(Ordering::Relaxed),
            self.shared.output_lost.load(Ordering::Relaxed),
        )
    }

    /// Périphériques de repli en service : (micro, sortie). Le son passe, mais
    /// pas par celui qui est réglé — typiquement un casque resté à la maison.
    pub fn device_fallback(&self) -> (bool, bool) {
        (
            self.shared.input_fallback.load(Ordering::Relaxed),
            self.shared.output_fallback.load(Ordering::Relaxed),
        )
    }

    /// Vrai quand de la voix part réellement sur le réseau (après VAD).
    pub fn is_sending(&self) -> bool {
        self.shared.sending.load(Ordering::Relaxed)
    }

    /// Gain automatique (AGC), à chaud.
    pub fn set_agc(&self, on: bool) {
        self.shared.agc.store(on, Ordering::Relaxed);
    }

    /// Niveau crête récent de chaque locuteur distant (pour les vumètres).
    pub fn user_levels(&self) -> Vec<(u64, f32)> {
        // Le niveau est mesuré au mixage : il vit avec le tampon de lecture.
        //
        // Les poignées sont copiées puis le verrou de la carte est relâché
        // avant de lire les tampons un à un. Tenir la carte pendant ces
        // lectures, depuis le fil de l'interface appelé à chaque image,
        // ferait attendre le rappel de sortie derrière elle.
        let handles: Vec<(u64, Arc<Mutex<crate::jitter::Playout>>)> = {
            let playouts = self.shared.playouts.lock().unwrap();
            playouts.iter().map(|(id, p)| (*id, p.clone())).collect()
        };
        handles
            .into_iter()
            .map(|(id, p)| (id, p.lock().unwrap().level()))
            .collect()
    }

    pub fn transmitting(&self) -> bool {
        self.shared.transmitting.load(Ordering::Relaxed)
    }

    pub fn stats(&self) -> VoiceStats {
        let (active_senders, worst_jitter_ms) = {
            let receivers = self.shared.receivers.lock().unwrap();
            let worst = receivers.values().map(|r| r.jitter_ms()).fold(0f32, f32::max);
            (receivers.len(), worst)
        };
        VoiceStats {
            packets_sent: self.shared.counters.sent.load(Ordering::Relaxed),
            packets_received: self.shared.counters.received.load(Ordering::Relaxed),
            packets_lost: self.shared.counters.lost.load(Ordering::Relaxed),
            packets_recovered: self.shared.counters.recovered.load(Ordering::Relaxed),
            packets_rejected: self.shared.counters.rejected.load(Ordering::Relaxed),
            active_senders,
            mic_peak: f32::from_bits(self.shared.counters.mic_peak_bits.load(Ordering::Relaxed)),
            samples_played: self.shared.counters.played.load(Ordering::Relaxed),
            worst_jitter_ms,
        }
    }

    pub fn shutdown(mut self) {
        journal("moteur vocal arrêté".into());
        self.shared.shutdown.store(true, Ordering::Relaxed);
        // Les fils sont retirés plutôt que déplacés : le `Drop` ci-dessous
        // les voit alors déjà pris en charge, et n'a plus rien à faire.
        for t in std::mem::take(&mut self.threads) {
            let _ = t.join();
        }
    }
}

/// Un moteur abandonné s'arrête, au lieu de continuer à tourner.
///
/// Ses fils de capture et de lecture détiennent chacun un `Arc<Shared>` : rien
/// ne les arrête tant que le drapeau d'arrêt n'est pas levé, et lâcher le
/// `VoiceEngine` ne suffisait pas à le lever. Le moteur remplacé — un second
/// `Welcome`, deux changements de périphérique coup sur coup — gardait donc
/// son micro ouvert et continuait d'encoder et d'émettre indéfiniment : on
/// s'entendait en double, et la charge ne redescendait plus.
///
/// `shutdown` reste la voie normale, qui attend la fin des fils ; ici on lève
/// seulement le drapeau, sans joindre — un `Drop` ne doit pas bloquer, et les
/// fils s'arrêtent d'eux-mêmes au tour suivant.
impl Drop for VoiceEngine {
    fn drop(&mut self) {
        if !self.threads.is_empty() {
            tracing::debug!("moteur vocal abandonné sans arrêt explicite : on l'arrête");
            self.shared.shutdown.store(true, Ordering::Relaxed);
        }
    }
}

fn is_shutdown(sh: &Shared) -> bool {
    sh.shutdown.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Réseau
// ---------------------------------------------------------------------------

fn recv_loop(sh: Arc<Shared>, incoming: std::sync::mpsc::Receiver<Vec<u8>>) {
    let mut last_prune = Instant::now();
    loop {
        let dat = match incoming.recv_timeout(Duration::from_millis(200)) {
            Ok(d) => d,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if is_shutdown(&sh) {
                    return;
                }
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        };
        if is_shutdown(&sh) {
            return;
        }
        let Some(pkt) = parse_voice_packet(&dat) else { continue };
        if pkt.payload.is_empty() {
            continue;
        }
        // Déchiffrement : le nonce dérive de (émetteur, compteur), donc un
        // paquet forgé ou altéré échoue ici et est simplement compté.
        let plain = match sh.cipher.decrypt(&nonce_for(pkt.id, pkt.counter), pkt.payload) {
            Ok(p) => p,
            Err(_) => {
                sh.counters.rejected.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        sh.counters.received.fetch_add(1, Ordering::Relaxed);

        let mut receivers = sh.receivers.lock().unwrap();
        let mut fresh = None;
        let rx = receivers.entry(pkt.id).or_insert_with(|| {
            let mut r = Receiver::new();
            let over = sh.jitter_override.load(Ordering::Relaxed);
            r.set_jitter_override(if over == 0 { None } else { Some(over) });
            // Un locuteur qui apparaît doit être audible : son tampon de
            // lecture rejoint la carte que consulte le rappel de sortie.
            fresh = Some(r.playout());
            r
        });
        let (lost, recovered) = rx.push(pkt.counter as u16, &plain);
        if lost > 0 {
            sh.counters.lost.fetch_add(lost, Ordering::Relaxed);
        }
        if recovered > 0 {
            sh.counters.recovered.fetch_add(recovered, Ordering::Relaxed);
        }
        if let Some(playout) = fresh {
            sh.playouts.lock().unwrap().insert(pkt.id, playout);
        }

        // Purge les émetteurs silencieux depuis > 5 s.
        if last_prune.elapsed() > Duration::from_secs(5) {
            receivers.retain(|_, r| r.last_activity().elapsed() < Duration::from_secs(5));
            // Les deux cartes restent alignées : un tampon oublié ici
            // continuerait d'être mixé alors que son locuteur est parti.
            let mut playouts = sh.playouts.lock().unwrap();
            playouts.retain(|id, _| receivers.contains_key(id));
            last_prune = Instant::now();
        }
    }
}

// ---------------------------------------------------------------------------
// Émission
// ---------------------------------------------------------------------------

/// Encode, chiffre et émet une trame de FRAME_SAMPLES échantillons
/// 48 kHz mono via le transport fourni.
struct Sender {
    encoder: opus::Encoder,
    /// Compteur strictement croissant : numéro de séquence ET nonce.
    counter: u64,
    user_id: u64,
    bitrate: i32,
    dred: i32,
    send: DatagramSend,
    opus_buf: [u8; VOICE_MAX_PACKET],
    out: [u8; VOICE_MAX_PACKET],
}

impl Sender {
    /// Applique un nouveau débit Opus si différent de l'actuel.
    fn apply_bitrate(&mut self, bitrate: i32) {
        if bitrate != self.bitrate {
            if self.encoder.set_bitrate(opus::Bitrate::Bits(bitrate)).is_ok() {
                tracing::info!("débit Opus : {} kbps", bitrate / 1000);
                self.bitrate = bitrate;
            }
        }
    }

    /// Applique une nouvelle durée de redondance DRED si différente.
    /// Le budget DRED (en VBR) suit la perte annoncée à l'encodeur : quand
    /// la protection s'engage — donc quand des pertes sont réellement
    /// mesurées — on annonce une perte élevée pour un budget généreux.
    fn apply_dred(&mut self, dred: i32) {
        if dred != self.dred {
            match self.encoder.set_dred_duration(dred) {
                Ok(()) => {
                    let _ = self
                        .encoder
                        .set_packet_loss_perc(if dred > 0 { 30 } else { 10 });
                    tracing::info!(
                        "DRED {}",
                        if dred > 0 { "activé (protection contre les pertes)" } else { "désactivé" }
                    );
                    self.dred = dred;
                }
                Err(e) => {
                    tracing::warn!("DRED indisponible : {e}");
                    self.dred = dred; // ne pas réessayer à chaque trame
                }
            }
        }
    }

    fn new(user_id: u64, bitrate: i32, send: DatagramSend) -> anyhow::Result<Self> {
        let mut encoder =
            opus::Encoder::new(SAMPLE_RATE, opus::Channels::Mono, opus::Application::Voip)?;
        encoder.set_bitrate(opus::Bitrate::Bits(bitrate))?;
        // FEC intra-bande : le décodeur peut reconstruire une trame perdue
        // à partir de la suivante. Précieux en Wi-Fi.
        encoder.set_inband_fec(true)?;
        encoder.set_packet_loss_perc(10)?;
        Ok(Self {
            encoder,
            // Départ **aléatoire**, et non 1.
            //
            // Le compteur est le nonce : la paire (émetteur, compteur) doit
            // rester unique pour une clé donnée. Or la clé ne change qu'au
            // redémarrage du serveur, tandis que ce moteur est recréé bien
            // plus souvent — à chaque changement de micro ou de casque, à
            // chaque reconnexion, à chaque relance de l'application. Repartir
            // de 1 réutilisait donc des nonces déjà employés sous la même
            // clé, ce qui casse le chiffrement : deux flux se retrouvent
            // chiffrés avec le même flot, et la clé d'authentification à usage
            // unique se retrouve exposée.
            //
            // Le tirage est borné à 48 bits pour laisser au compteur toute la
            // place de monter sans jamais déborder — une session de mille ans
            // n'en consommerait pas le millième. Deux sessions se recouvrent
            // avec une probabilité de l'ordre de 10⁻⁸, là où la collision
            // était certaine.
            counter: {
                use rand::Rng as _;
                rand::rng().random::<u64>() >> 16
            },
            user_id,
            bitrate,
            dred: 0,
            send,
            opus_buf: [0; VOICE_MAX_PACKET],
            out: [0; VOICE_MAX_PACKET],
        })
    }

    fn send_frame(&mut self, sh: &Shared, frame: &[f32]) {
        // L'encodeur reçoit le budget **réellement transmissible**, et non la
        // taille du tampon : il reste l'en-tête et le tag d'authentification à
        // loger dans le paquet. Lui annoncer plus le laissait produire des
        // trames qu'on ne pouvait plus envoyer — jetées plus bas, sans trou de
        // séquence, donc sans que le récepteur ne dissimule quoi que ce soit :
        // 20 ms de voix disparaissaient dans un silence que rien ne signalait.
        const OVERHEAD: usize = VOICE_HEADER_LEN + 16;
        let budget = VOICE_MAX_PACKET - OVERHEAD;
        // Toute sortie prématurée fait quand même avancer le compteur : une
        // trame escamotée sans trou de séquence ne serait pas dissimulée à
        // l'autre bout, et produirait une soudure audible plutôt qu'une perte
        // traitée comme telle.
        let Ok(n) = self.encoder.encode_float(frame, &mut self.opus_buf[..budget]) else {
            self.counter += 1;
            return;
        };
        // Chiffrement de la charge : le transport (QUIC/TLS) protège déjà le
        // trajet, et cette couche fait que le relais ne manipule que des
        // octets opaques. Ce n'est pas du bout en bout au sens strict — la
        // clé est distribuée par le serveur, qui pourrait donc déchiffrer —
        // mais elle borne ce qu'un relais compromis après coup peut relire.
        let Ok(sealed) = sh
            .cipher
            .encrypt(&nonce_for(self.user_id, self.counter), &self.opus_buf[..n])
        else {
            self.counter += 1;
            return;
        };
        let total = VOICE_HEADER_LEN + sealed.len();
        if total > VOICE_MAX_PACKET {
            // Ne devrait plus arriver, le budget étant annoncé à l'encodeur.
            // Le compteur avance quand même : un trou de séquence fait au
            // moins dissimuler la perte à l'autre bout, là où une trame
            // escamotée en silence produit une soudure audible.
            self.counter += 1;
            return;
        }
        write_voice_header(&mut self.out, self.user_id, self.counter);
        self.out[VOICE_HEADER_LEN..total].copy_from_slice(&sealed);
        self.counter += 1;
        (self.send)(&self.out[..total]);
        sh.counters.sent.fetch_add(1, Ordering::Relaxed);
    }
}

/// Durée de zéros stricts avant de rouvrir un micro qui a déjà porté du
/// signal. Large : quinze secondes de zéros *exacts* ne ressemblent à aucun
/// silence réel — un micro capte toujours un fond — mais un pilote qui
///« gate » numériquement sa sortie pourrait les produire ; on ne veut pas
/// rouvrir à chaque blanc de conversation.
const ZERO_WEDGE: Duration = Duration::from_secs(15);

/// Capture micro réelle via cpal.
#[allow(clippy::too_many_arguments)]
fn capture_loop(
    sh: Arc<Shared>,
    user_id: u64,
    bitrate: i32,
    device_name: Option<String>,
    native: bool,
    raw: bool,
    comms_pref: bool,
    send: DatagramSend,
) -> anyhow::Result<()> {
    // Les notifications de périphériques accélèrent la sonde d'empreinte.
    // Une seule écoute pour le processus ; l'appel est idempotent.
    #[cfg(windows)]
    wasapi::ensure_notifications();
    // Le compteur de paquets sert de nonce : il doit vivre plus longtemps
    // que le périphérique. Le recréer à chaque rebranchement réutiliserait
    // des nonces déjà employés avec la même clé — ce qui casserait le
    // chiffrement. Il est donc construit ici, une seule fois.
    let mut sender = Sender::new(user_id, bitrate, send)?;
    let mut frame = [0f32; FRAME_SAMPLES];
    let mut denoiser = Denoiser::new();
    let mut deep = LazyDeep::default();
    let mut gate = NoiseGate::new();
    let mut agc = Agc::new();
    let mut monitor: Option<Monitor> = None;
    // Activation vocale : on continue d'émettre un court instant après le
    // dernier franchissement du seuil, pour ne pas hacher les fins de mots.
    let mut last_voice = Instant::now() - Duration::from_secs(60);
    // Zombie à zéros : armé dès qu'un échantillon non nul passe, désarmé par
    // la réouverture qu'il déclenche. Une seule réouverture par épisode : un
    // casque coupé par son bouton mute matériel livre aussi des zéros
    // stricts, et rouvrir en boucle ne lui rendrait pas la parole.
    let mut zero_reopen_armed = false;
    // Ouvertures consécutives où pas un seul bloc n'est arrivé : la signature
    // du micro affamé — il s'ouvre sans erreur, mais un autre logiciel tient
    // la voie de capture et la nôtre n'est jamais servie. Compté pour le
    // nommer dans le journal, et surtout pour escalader.
    let mut starved_opens = 0u32;
    // Catégorie « communications » du micro : celle du réglage, puis escaladée
    // d'elle-même à la famine — la voix intégrée d'un jeu qui tient la voie
    // de traitement ne la partage qu'avec un flux de la même catégorie.
    let mut comms = comms_pref;

    // Boucle de surveillance : le périphérique peut disparaître à tout
    // moment (débranchement, casque sans fil qui s'endort). On le rouvre
    // alors sans relancer le moteur, donc sans perdre l'encodeur ni le
    // compteur de nonces.
    'device: while !is_shutdown(&sh) {
        let opened = open_input(device_name.as_deref(), native, raw, comms);
        let (stream, rx, in_rate, alive, fallback) = match opened {
            Ok(parts) => {
                // Le repli est signalé — sans quoi débrancher son micro pour
                // le rebrancher laissait capter celui de la webcam en silence,
                // pour toute la session — mais comme un repli, pas comme une
                // perte : on capte, et annoncer « micro perdu » serait faux.
                sh.input_lost.store(false, Ordering::Relaxed);
                sh.input_fallback.store(parts.4, Ordering::Relaxed);
                parts
            }
            Err(e) => {
                // Le micro n'est pas là : on le signale et on réessaie.
                // Rien d'autre à faire — il reviendra peut-être.
                if !sh.input_lost.swap(true, Ordering::Relaxed) {
                    tracing::warn!("micro indisponible : {e:#} — nouvelle tentative en cours");
                    journal(format!("micro indisponible : {e:#} — tentatives en cours"));
                }
                sh.sending.store(false, Ordering::Relaxed);
                sleep_unless_shutdown(&sh, Duration::from_millis(1000));
                continue 'device;
            }
        };
        let mut resampler = CubicResampler::new(in_rate as f64 / SAMPLE_RATE as f64);
        let mut last_chunk = Instant::now();
        let mut got_any_chunk = false;
        let mut last_probe = Instant::now();
        // L'état du monde au moment de l'ouverture : générations de reset et
        // empreinte du périphérique. Toute divergence ultérieure = réouverture.
        let my_epoch = sh.audio_reset.load(Ordering::Relaxed);
        let my_signature = device_signature(device_name.as_deref(), true, native);
        let mut my_gen = native_generation();
        // Début de l'épisode de zéros stricts en cours, s'il y en a un.
        let mut zero_since: Option<Instant> = None;

    while !is_shutdown(&sh) {
        // Réinitialisation demandée (bouton « Réinitialiser l'audio ») :
        // l'équivalent logiciel du débranchement/rebranchement du casque.
        if sh.audio_reset.load(Ordering::Relaxed) != my_epoch {
            tracing::info!("réinitialisation audio demandée — réouverture du micro");
            journal("réinitialisation demandée — réouverture du micro".into());
            drop(stream);
            continue 'device;
        }
        // Sonde d'empreinte : toutes les 3 s en croisière, et dès ~300 ms
        // après un événement de périphériques signalé par Windows — sans
        // sonder à chaque rappel, un glissement de volume en émet des
        // dizaines par seconde.
        let gen_now = native_generation();
        let probe_after = if gen_now != my_gen {
            Duration::from_millis(300)
        } else {
            Duration::from_secs(3)
        };
        if last_probe.elapsed() > probe_after {
            last_probe = Instant::now();
            my_gen = gen_now;
            // Sur un repli, on guette le retour du périphérique demandé. Sans
            // cette veille, un casque rebranché n'était jamais repris : le
            // repli fonctionnant, plus rien ne provoquait de réouverture.
            if fallback {
                if let Some(wanted) = device_name.as_deref() {
                    if device_present(&cpal::default_host(), wanted, true) {
                        tracing::info!("micro « {wanted} » de retour — reprise");
                        journal(format!("micro « {wanted} » de retour — reprise"));
                        drop(stream);
                        continue 'device;
                    }
                }
            }
            // Le périphérique ou son format a changé sous nos pieds (un jeu
            // qui se lance, Synapse qui rebrasse…) : le flux en cours peut
            // être un zombie qui capte dans le vide. On le rouvre.
            let now_sig = device_signature(device_name.as_deref(), true, native);
            if now_sig != my_signature {
                tracing::warn!(
                    "micro : périphérique ou format modifié ({:?} -> {:?}) — réouverture",
                    my_signature,
                    now_sig
                );
                journal(format!(
                    "micro : périphérique ou format modifié ({} -> {}) — réouverture",
                    my_signature.as_deref().unwrap_or("absent"),
                    now_sig.as_deref().unwrap_or("absent"),
                ));
                drop(stream);
                continue 'device;
            }
        }
        let chunk = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(c) => {
                last_chunk = Instant::now();
                if !got_any_chunk {
                    got_any_chunk = true;
                    starved_opens = 0;
                }
                c
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // La capture tourne en continu, micro coupé ou non : un
                // silence prolongé n'est pas un utilisateur qui se tait,
                // c'est un périphérique parti. Certains pilotes ne signalent
                // d'ailleurs aucune erreur — ils cessent juste de livrer,
                // d'où ce garde-fou en plus du rappel d'erreur.
                let dead = !alive.load(Ordering::Relaxed)
                    || last_chunk.elapsed() > Duration::from_secs(2);
                if !dead {
                    continue;
                }
                // Une seule ligne par disparition : un périphérique
                // durablement muet ferait sinon défiler le journal.
                if !sh.input_lost.swap(true, Ordering::Relaxed) {
                    tracing::warn!("micro perdu — réouverture");
                    journal("micro perdu (le flux ne livre plus) — réouverture".into());
                }
                // Trois ouvertures d'affilée sans le moindre bloc : ce n'est
                // plus un accident, c'est un micro affamé — le flux s'ouvre,
                // mais un autre logiciel (voix d'un jeu, pilote du casque)
                // tient la voie de capture. On escalade alors en catégorie
                // « communications » : la même voie que lui, donc partagée.
                // Nommé une fois par épisode, pas égrené.
                if !got_any_chunk {
                    starved_opens += 1;
                    if starved_opens == 3 {
                        tracing::warn!("micro affamé : 3 ouvertures sans un seul bloc");
                        journal(
                            "le micro s'ouvre mais ne livre rien (3 fois de suite) — un \
                             autre logiciel tient probablement la voie de capture (voix \
                             intégrée d'un jeu, pilote du casque)"
                                .into(),
                        );
                        if native && !comms {
                            comms = true;
                            starved_opens = 0;
                            journal(
                                "micro passé en catégorie communications — la voie de \
                                 traitement est désormais partagée avec la voix du jeu. \
                                 Si le volume de tes autres sons chute pendant le vocal : \
                                 Panneau son Windows → onglet Communication → « Ne rien \
                                 faire »"
                                    .into(),
                            );
                        }
                    }
                }
                sh.sending.store(false, Ordering::Relaxed);
                drop(stream);
                sleep_unless_shutdown(&sh, Duration::from_millis(500));
                continue 'device;
            }
            Err(_) => break,
        };
        // Zombie à zéros : après le passage d'un jeu, certains pilotes USB
        // continuent de livrer des trames à l'heure — mais toutes à zéro
        // strict, ce qu'aucun micro réel ne produit. Ni le rappel d'erreur
        // ni le garde-fou de livraison ne voient rien : seul le contenu
        // trahit la panne. Après du signal puis ZERO_WEDGE de zéros, on
        // rouvre — une fois (voir zero_reopen_armed).
        if chunk.iter().any(|&s| s != 0.0) {
            zero_since = None;
            zero_reopen_armed = true;
        } else if zero_reopen_armed
            && zero_since.get_or_insert_with(Instant::now).elapsed() > ZERO_WEDGE
        {
            tracing::warn!(
                "micro muet ({} s de zéros stricts) — réouverture préventive",
                ZERO_WEDGE.as_secs()
            );
            journal(format!(
                "micro muet ({} s de zéros stricts après du signal) — réouverture",
                ZERO_WEDGE.as_secs()
            ));
            zero_reopen_armed = false;
            drop(stream);
            continue 'device;
        }
        resampler.push(&chunk);
        while resampler.can_pull(FRAME_SAMPLES) {
            resampler.pull(&mut frame);

            // 1. Gain d'entrée.
            let gain = load_f32(&sh.input_gain);
            if (gain - 1.0).abs() > 0.001 {
                for s in frame.iter_mut() {
                    *s = (*s * gain).clamp(-1.0, 1.0);
                }
            }
            // 2. Débruitage — en continu (même micro coupé) pour que le
            // modèle garde son estimation du bruit ambiant à jour.
            match sh.noise_mode.load(Ordering::Relaxed) {
                NOISE_RNNOISE => denoiser.process(&mut frame),
                NOISE_DEEP => match deep.get_or_init() {
                    Some(d) => d.process(&mut frame),
                    // Modèle indisponible : repli silencieux sur RNNoise.
                    None => denoiser.process(&mut frame),
                },
                _ => {}
            }
            // 3. Porte de bruit : coupe le résidu sous le seuil choisi.
            gate.process(&mut frame, load_f32(&sh.gate_threshold));
            // 4. Gain automatique : normalise le niveau de la voix.
            if sh.agc.load(Ordering::Relaxed) {
                agc.process(&mut frame, load_f32(&sh.agc_target));
            }
            // 5. Vumètre : niveau après toute la chaîne (= ce qui part).
            let peak = frame.iter().fold(0f32, |m, s| m.max(s.abs()));
            sh.counters.mic_peak_bits.store(peak.to_bits(), Ordering::Relaxed);

            // 6. Décision d'émission : armé + activation vocale éventuelle.
            let armed = sh.transmitting.load(Ordering::Relaxed);
            let threshold = load_f32(&sh.vad_threshold);
            if peak >= threshold {
                last_voice = Instant::now();
            }
            let hangover =
                Duration::from_millis(sh.vad_hangover_ms.load(Ordering::Relaxed) as u64);
            let send = armed && (threshold <= 0.0 || last_voice.elapsed() < hangover);
            sh.sending.store(send, Ordering::Relaxed);
            if send {
                sender.apply_bitrate(sh.bitrate.load(Ordering::Relaxed));
                sender.apply_dred(sh.dred.load(Ordering::Relaxed));
                sender.send_frame(&sh, &frame);
            }
            // 7. Retour local (« s'écouter ») : la trame passe par un VRAI
            // aller-retour Opus (encodage + décodage au débit courant) —
            // tu entends exactement ce que les autres entendent, artéfacts
            // du codec compris. Tampon borné à ~250 ms (anti-dérive).
            if sh.loopback.load(Ordering::Relaxed) {
                let mon = monitor.get_or_insert_with(|| {
                    Monitor::new(sh.bitrate.load(Ordering::Relaxed))
                });
                let mut heard = [0f32; FRAME_SAMPLES];
                mon.process(&frame, sh.bitrate.load(Ordering::Relaxed), &mut heard);
                const LOOPBACK_CAP: usize = SAMPLE_RATE as usize / 4;
                let mut buf = sh.loopback_buf.lock().unwrap();
                buf.extend(heard.iter());
                while buf.len() > LOOPBACK_CAP {
                    buf.pop_front();
                }
            } else {
                monitor = None;
            }
        }
    }
        // Sortie de la boucle interne sans perte de périphérique : c'est un
        // arrêt du moteur.
        break 'device;
    }
    sh.sending.store(false, Ordering::Relaxed);
    sh.input_lost.store(false, Ordering::Relaxed);
    Ok(())
}

/// Flux d'entrée, quel que soit le moteur. Les valeurs ne sont jamais
/// *lues* : elles sont *tenues* — les lâcher ferme le flux, c'est tout leur
/// rôle. D'où l'allow.
#[allow(dead_code)]
enum InputStream {
    Cpal(cpal::Stream),
    #[cfg(windows)]
    Native(wasapi::NativeStream),
}

/// Flux de sortie, quel que soit le moteur. Même contrat de simple tenue.
#[allow(dead_code)]
enum OutputStream {
    Cpal(cpal::Stream),
    #[cfg(windows)]
    Native(wasapi::NativeStream),
}

/// Ouvre le micro et rend de quoi le lire, plus un drapeau que le rappel
/// d'erreur abaisse si le flux tombe.
/// Le dernier membre dit qu'on tourne sur un périphérique de repli, le
/// périphérique demandé étant absent.
type OpenedInput =
    (InputStream, std::sync::mpsc::Receiver<Vec<f32>>, u32, Arc<AtomicBool>, bool);

/// Génération du parc de périphériques signalée par Windows. Hors Windows,
/// constante : la sonde périodique reste le seul déclencheur.
#[cfg(windows)]
fn native_generation() -> u64 {
    wasapi::device_generation()
}
#[cfg(not(windows))]
fn native_generation() -> u64 {
    0
}

/// Empreinte du périphérique tel qu'il se présenterait si on l'ouvrait
/// maintenant : identité + format par défaut. Quand elle diverge de celle du
/// flux en cours, c'est qu'un logiciel (Synapse, un jeu…) a rebrassé les
/// périphériques ou changé le format — le flux ouvert peut alors être un
/// zombie : il tourne, mais le son ne passe plus. On rouvre.
///
/// L'empreinte suit le moteur : le natif voit l'identifiant d'endpoint (qui
/// change à la ré-énumération USB) et le défaut *communication*, cpal le nom
/// et le défaut console. Comparer des empreintes d'un même moteur suffit —
/// elles ne se croisent jamais.
fn device_signature(device_name: Option<&str>, input: bool, native: bool) -> Option<String> {
    #[cfg(windows)]
    if native {
        return wasapi::device_signature(device_name, input);
    }
    #[cfg(not(windows))]
    let _ = native;
    let host = cpal::default_host();
    let (device, fallback) = pick_device(&host, device_name, input);
    let device = device?;
    let cfg = if input {
        device.default_input_config().ok()?
    } else {
        device.default_output_config().ok()?
    };
    Some(format!(
        "{}|{}|{}|{}",
        device.name().unwrap_or_default(),
        cfg.sample_rate().0,
        cfg.channels(),
        fallback,
    ))
}

fn open_input(
    device_name: Option<&str>,
    native: bool,
    raw: bool,
    comms: bool,
) -> anyhow::Result<OpenedInput> {
    // Moteur natif d'abord, s'il est demandé ; toute erreur fait retomber
    // sur cpal dans la foulée — au pire, on se comporte comme avant.
    #[cfg(windows)]
    if native {
        let (tx, rx) = std::sync::mpsc::channel::<Vec<f32>>();
        let alive = Arc::new(AtomicBool::new(true));
        match wasapi::open_input(device_name, raw, comms, tx, alive.clone()) {
            Ok((stream, in_rate, fallback)) => {
                return Ok((InputStream::Native(stream), rx, in_rate, alive, fallback));
            }
            Err(e) => {
                tracing::warn!("micro natif indisponible : {e:#} — repli sur cpal");
                journal(format!("moteur natif indisponible (micro) : {e:#} — repli cpal"));
            }
        }
    }
    #[cfg(not(windows))]
    let _ = (native, raw, comms);
    // L'hôte est ré-interrogé à chaque tentative : c'est ce qui permet de
    // voir réapparaître un périphérique rebranché.
    let host = cpal::default_host();
    let (device, fallback) = pick_device(&host, device_name, true);
    if fallback {
        if let Some(name) = device_name {
            tracing::warn!("micro « {name} » introuvable, retour au défaut");
        }
    }
    let device = device.context("aucun périphérique d'entrée (micro) trouvé")?;
    let supported = device.default_input_config().context("config micro")?;
    let in_rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    tracing::info!(
        "micro : {} ({} Hz, {} canaux)",
        device.name().unwrap_or_default(),
        in_rate,
        channels
    );
    journal(format!(
        "micro ouvert : {} ({} Hz, {} canaux){}",
        device.name().unwrap_or_default(),
        in_rate,
        channels,
        if fallback { " — repli, le périphérique réglé est introuvable" } else { "" },
    ));

    let (tx, rx) = std::sync::mpsc::channel::<Vec<f32>>();
    let alive = Arc::new(AtomicBool::new(true));
    // Le callback cpal ne fait que convertir en mono et transmettre : tout
    // le travail (rééchantillonnage, encodage, envoi) se fait dans le thread
    // de capture, hors du chemin temps réel.
    let stream = build_input_stream(&device, &supported, channels, tx, alive.clone())?;
    stream.play()?;
    Ok((InputStream::Cpal(stream), rx, in_rate, alive, fallback))
}

/// Attend, en écourtant si le moteur s'arrête entre-temps.
fn sleep_unless_shutdown(sh: &Arc<Shared>, total: Duration) {
    let step = Duration::from_millis(50);
    let mut left = total;
    while left > Duration::ZERO && !is_shutdown(sh) {
        let nap = step.min(left);
        std::thread::sleep(nap);
        left -= nap;
    }
}

/// Moniteur « s'écouter » : un aller-retour complet par le codec Opus,
/// indépendant du flux réseau, pour entendre le rendu réel.
struct Monitor {
    encoder: opus::Encoder,
    decoder: opus::Decoder,
    bitrate: i32,
    buf: [u8; VOICE_MAX_PACKET],
}

impl Monitor {
    fn new(bitrate: i32) -> Self {
        let mut encoder =
            opus::Encoder::new(SAMPLE_RATE, opus::Channels::Mono, opus::Application::Voip)
                .expect("encodeur moniteur");
        let _ = encoder.set_bitrate(opus::Bitrate::Bits(bitrate));
        let decoder =
            opus::Decoder::new(SAMPLE_RATE, opus::Channels::Mono).expect("décodeur moniteur");
        Self { encoder, decoder, bitrate, buf: [0; VOICE_MAX_PACKET] }
    }

    fn process(&mut self, frame: &[f32], bitrate: i32, out: &mut [f32; FRAME_SAMPLES]) {
        if bitrate != self.bitrate
            && self.encoder.set_bitrate(opus::Bitrate::Bits(bitrate)).is_ok()
        {
            self.bitrate = bitrate;
        }
        let ok = self
            .encoder
            .encode_float(frame, &mut self.buf)
            .ok()
            .and_then(|n| self.decoder.decode_float(&self.buf[..n], out, false).ok())
            .is_some();
        if !ok {
            out.copy_from_slice(frame);
        }
    }
}

/// Porte de bruit : atténue progressivement le signal sous le seuil.
/// Ouverture rapide (ne mange pas les attaques), fermeture douce.
struct NoiseGate {
    env: f32,
    gain: f32,
}

impl NoiseGate {
    fn new() -> Self {
        Self { env: 0.0, gain: 1.0 }
    }

    fn process(&mut self, frame: &mut [f32], threshold: f32) {
        if threshold <= 0.0 {
            self.gain = 1.0;
            return;
        }
        let peak = frame.iter().fold(0f32, |m, s| m.max(s.abs()));
        self.env = peak.max(self.env * 0.92);
        let target = if self.env >= threshold { 1.0 } else { 0.0 };
        let rate = if target > self.gain { 0.6 } else { 0.08 };
        self.gain += (target - self.gain) * rate;
        if self.gain < 0.999 {
            for s in frame.iter_mut() {
                *s *= self.gain;
            }
        }
    }
}

/// Gain automatique : vise une crête de parole constante, avec une attaque
/// rapide (anti-saturation) et une détente lente (anti-pompage).
struct Agc {
    gain: f32,
}

impl Agc {
    /// En-dessous, on considère que c'est du silence : le gain ne bouge pas
    /// (sinon l'AGC amplifierait le bruit de fond entre les phrases).
    const GATE: f32 = 0.015;

    fn new() -> Self {
        Self { gain: 1.0 }
    }

    fn process(&mut self, frame: &mut [f32], target: f32) {
        let peak = frame.iter().fold(0f32, |m, s| m.max(s.abs()));
        if peak >= Self::GATE {
            let desired = (target / peak).clamp(0.2, 8.0);
            // Baisse vite (protège de la saturation), monte doucement.
            let rate = if desired < self.gain { 0.5 } else { 0.02 };
            self.gain += (desired - self.gain) * rate;
        }
        if (self.gain - 1.0).abs() > 0.001 {
            for s in frame.iter_mut() {
                *s = (*s * self.gain).clamp(-1.0, 1.0);
            }
        }
    }
}

/// Écrêtage doux du mix : linéaire jusqu'à 85 %, compression progressive
/// au-delà — plusieurs voix fortes saturent en douceur au lieu de craquer.
fn soft_clip(x: f32) -> f32 {
    const KNEE: f32 = 0.85;
    let a = x.abs();
    if a <= KNEE {
        x
    } else {
        let over = (a - KNEE) / (1.0 - KNEE);
        x.signum() * (KNEE + (1.0 - KNEE) * over / (1.0 + over))
    }
}

/// Débruitage DeepFilterNet3 : réseau de neurones (inférence tract, pur
/// Rust), blocs de 480 échantillons à 48 kHz. Qualité très supérieure à
/// RNNoise, au prix de ~30 ms de lookahead et ~1 ms de CPU par bloc.
struct DeepDenoiser {
    model: df::tract::DfTract,
    inp: ndarray::Array2<f32>,
    out: ndarray::Array2<f32>,
}

impl DeepDenoiser {
    fn new() -> anyhow::Result<Self> {
        let params = df::tract::RuntimeParams::default_with_ch(1);
        let model = df::tract::DfTract::new(df::tract::DfParams::default(), &params)
            .map_err(|e| anyhow::anyhow!("chargement DeepFilterNet : {e}"))?;
        anyhow::ensure!(model.sr as u32 == SAMPLE_RATE, "DeepFilterNet attend 48 kHz");
        let hop = model.hop_size;
        anyhow::ensure!(FRAME_SAMPLES % hop == 0, "hop DeepFilterNet incompatible");
        Ok(Self {
            model,
            inp: ndarray::Array2::zeros((1, hop)),
            out: ndarray::Array2::zeros((1, hop)),
        })
    }

    fn process(&mut self, frame: &mut [f32; FRAME_SAMPLES]) {
        let hop = self.inp.shape()[1];
        for block in frame.chunks_exact_mut(hop) {
            self.inp
                .row_mut(0)
                .as_slice_mut()
                .expect("layout contigu")
                .copy_from_slice(block);
            if self.model.process(self.inp.view(), self.out.view_mut()).is_ok() {
                block.copy_from_slice(self.out.row(0).as_slice().expect("layout contigu"));
            }
        }
    }
}

/// Chargement paresseux de DeepFilterNet : le modèle (~2 Mo, planification
/// tract comprise) n'est chargé que si le mode est activé, hors du chemin
/// temps réel critique. En cas d'échec, on n'essaie qu'une fois.
#[derive(Default)]
struct LazyDeep {
    state: Option<Option<Box<DeepDenoiser>>>,
}

impl LazyDeep {
    fn get_or_init(&mut self) -> Option<&mut DeepDenoiser> {
        self.state
            .get_or_insert_with(|| match DeepDenoiser::new() {
                Ok(d) => {
                    tracing::info!("DeepFilterNet3 chargé (débruitage studio)");
                    Some(Box::new(d))
                }
                Err(e) => {
                    tracing::error!("{e:#} — repli sur RNNoise");
                    None
                }
            })
            .as_deref_mut()
    }
}

/// Taille de bloc RNNoise : 10 ms à 48 kHz.
const RN_FRAME: usize = 480;

/// Suppression de bruit RNNoise sur des trames de 20 ms à 48 kHz.
/// RNNoise travaille par blocs de 10 ms (480 échantillons) et attend des
/// amplitudes à l'échelle i16 — d'où les conversions.
struct Denoiser {
    state: Box<nnnoiseless::DenoiseState<'static>>,
    scaled: [f32; RN_FRAME],
    out: [f32; RN_FRAME],
}

impl Denoiser {
    fn new() -> Self {
        assert_eq!(nnnoiseless::DenoiseState::FRAME_SIZE, RN_FRAME);
        Self {
            state: nnnoiseless::DenoiseState::new(),
            scaled: [0.0; RN_FRAME],
            out: [0.0; RN_FRAME],
        }
    }

    fn process(&mut self, frame: &mut [f32; FRAME_SAMPLES]) {
        for half in frame.chunks_exact_mut(RN_FRAME) {
            for (dst, &src) in self.scaled.iter_mut().zip(half.iter()) {
                *dst = src * 32767.0;
            }
            self.state.process_frame(&mut self.out, &self.scaled);
            for (dst, &src) in half.iter_mut().zip(self.out.iter()) {
                *dst = (src / 32767.0).clamp(-1.0, 1.0);
            }
        }
    }
}

/// Mode test : sinusoïde 440 Hz, cadencée à 20 ms, sans matériel audio.
fn tone_loop(sh: Arc<Shared>, user_id: u64, bitrate: i32, send: DatagramSend) {
    let Ok(mut sender) = Sender::new(user_id, bitrate, send) else {
        tracing::error!("création encodeur Opus impossible");
        return;
    };
    let mut phase = 0f32;
    let step = 2.0 * std::f32::consts::PI * 440.0 / SAMPLE_RATE as f32;
    let mut frame = [0f32; FRAME_SAMPLES];
    let start = Instant::now();
    let mut sent_frames = 0u64;

    while !is_shutdown(&sh) {
        let armed = sh.transmitting.load(Ordering::Relaxed);
        sh.sending.store(armed, Ordering::Relaxed);
        if armed {
            for s in frame.iter_mut() {
                *s = phase.sin() * 0.2;
                phase = (phase + step) % (2.0 * std::f32::consts::PI);
            }
            sender.send_frame(&sh, &frame);
        }
        // Cadence exacte : la (n+1)-ième trame part à start + n*20 ms.
        sent_frames += 1;
        let next = start + Duration::from_millis(20 * sent_frames);
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    }
}

fn to_mono(data: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return data.to_vec();
    }
    data.chunks_exact(channels)
        .map(|c| c.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Construit le flux d'entrée quel que soit le format d'échantillons du
/// périphérique, en convertissant vers f32.
fn build_input_stream(
    device: &cpal::Device,
    supported: &cpal::SupportedStreamConfig,
    channels: usize,
    tx: std::sync::mpsc::Sender<Vec<f32>>,
    alive: Arc<AtomicBool>,
) -> anyhow::Result<cpal::Stream> {
    fn build<T>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        channels: usize,
        tx: std::sync::mpsc::Sender<Vec<f32>>,
        alive: Arc<AtomicBool>,
    ) -> anyhow::Result<cpal::Stream>
    where
        T: cpal::SizedSample + dasp_sample::ToSample<f32>,
    {
        let stream = device.build_input_stream(
            config,
            move |data: &[T], _: &_| {
                let f: Vec<f32> = data.iter().map(|s| s.to_sample::<f32>()).collect();
                tx.send(to_mono(&f, channels)).ok();
            },
            // Signaler la panne, et pas seulement l'écrire : sans ce
            // drapeau, débrancher le micro le laissait mort jusqu'au
            // redémarrage de l'application. Une seule entrée de journal par
            // panne — le rappel peut se répéter tant que le flux agonise.
            move |e| {
                if alive.swap(false, Ordering::Relaxed) {
                    tracing::warn!("erreur flux micro : {e}");
                    journal(format!("erreur du flux micro : {e}"));
                }
            },
            None,
        )?;
        Ok(stream)
    }

    use cpal::SampleFormat as SF;
    let config = supported.config();
    let a = alive;
    match supported.sample_format() {
        SF::F32 => build::<f32>(device, &config, channels, tx, a),
        SF::I16 => build::<i16>(device, &config, channels, tx, a),
        SF::I32 => build::<i32>(device, &config, channels, tx, a),
        SF::U16 => build::<u16>(device, &config, channels, tx, a),
        SF::F64 => build::<f64>(device, &config, channels, tx, a),
        SF::I8 => build::<i8>(device, &config, channels, tx, a),
        SF::U8 => build::<u8>(device, &config, channels, tx, a),
        fmt => bail!("format micro non géré : {fmt:?}"),
    }
}

// ---------------------------------------------------------------------------
// Lecture
// ---------------------------------------------------------------------------

fn playback_loop(
    sh: Arc<Shared>,
    device_name: Option<String>,
    native: bool,
) -> anyhow::Result<()> {
    #[cfg(windows)]
    wasapi::ensure_notifications();
    // Même surveillance qu'à la capture : un casque débranché, ou qui sort
    // de veille, emporte le flux de sortie sans que rien ne le relance —
    // on n'entendait alors plus personne jusqu'au redémarrage.
    'device: while !is_shutdown(&sh) {
        let opened = open_output(&sh, device_name.as_deref(), native);
        let (stream, alive, ticks, fallback) = match opened {
            Ok(parts) => {
                // Comme au micro : un repli est signalé, sinon le son sortait
                // des haut-parleurs du portable sans que rien ne l'explique.
                sh.output_lost.store(false, Ordering::Relaxed);
                sh.output_fallback.store(parts.3, Ordering::Relaxed);
                parts
            }
            Err(e) => {
                if !sh.output_lost.swap(true, Ordering::Relaxed) {
                    tracing::warn!("sortie audio indisponible : {e:#} — nouvelle tentative");
                    journal(format!("sortie indisponible : {e:#} — tentatives en cours"));
                }
                sleep_unless_shutdown(&sh, Duration::from_millis(1000));
                continue 'device;
            }
        };

        // Le flux vit tant que la carte son réclame des échantillons. On
        // surveille ces demandes : un pilote peut cesser d'appeler sans
        // jamais signaler d'erreur.
        let mut last_seen = ticks.load(Ordering::Relaxed);
        let mut idle_rounds = 0u32;
        let mut last_probe = Instant::now();
        let my_epoch = sh.audio_reset.load(Ordering::Relaxed);
        let my_signature = device_signature(device_name.as_deref(), false, native);
        let mut my_gen = native_generation();
        while !is_shutdown(&sh) {
            std::thread::sleep(Duration::from_millis(500));
            // Réinitialisation demandée : rouvrir, comme un rebranchement.
            if sh.audio_reset.load(Ordering::Relaxed) != my_epoch {
                tracing::info!("réinitialisation audio demandée — réouverture de la sortie");
                journal("réinitialisation demandée — réouverture de la sortie".into());
                drop(stream);
                continue 'device;
            }
            // Comme à la capture : sonde accélérée après un événement de
            // périphériques signalé par Windows.
            let gen_now = native_generation();
            let probe_after = if gen_now != my_gen {
                Duration::from_millis(300)
            } else {
                Duration::from_secs(3)
            };
            if last_probe.elapsed() > probe_after {
                last_probe = Instant::now();
                my_gen = gen_now;
                // Retour du casque demandé : on le reprend, le repli n'étant
                // qu'un pis-aller que rien d'autre ne viendrait interrompre.
                if fallback {
                    if let Some(wanted) = device_name.as_deref() {
                        if device_present(&cpal::default_host(), wanted, false) {
                            tracing::info!("sortie « {wanted} » de retour — reprise");
                            journal(format!("sortie « {wanted} » de retour — reprise"));
                            drop(stream);
                            continue 'device;
                        }
                    }
                }
                // Périphérique ou format modifié (jeu qui se lance ou se
                // ferme, Synapse qui rebrasse) : le flux peut jouer dans le
                // vide sans une erreur — le zombie qui obligeait à
                // débrancher/rebrancher le casque. On rouvre nous-mêmes.
                let now_sig = device_signature(device_name.as_deref(), false, native);
                if now_sig != my_signature {
                    tracing::warn!(
                        "sortie : périphérique ou format modifié ({:?} -> {:?}) — réouverture",
                        my_signature,
                        now_sig
                    );
                    journal(format!(
                        "sortie : périphérique ou format modifié ({} -> {}) — réouverture",
                        my_signature.as_deref().unwrap_or("absent"),
                        now_sig.as_deref().unwrap_or("absent"),
                    ));
                    drop(stream);
                    continue 'device;
                }
            }
            let now = ticks.load(Ordering::Relaxed);
            idle_rounds = if now == last_seen { idle_rounds + 1 } else { 0 };
            last_seen = now;
            if !alive.load(Ordering::Relaxed) || idle_rounds >= 4 {
                if !sh.output_lost.swap(true, Ordering::Relaxed) {
                    tracing::warn!("sortie audio perdue — réouverture");
                    journal("sortie perdue (la carte ne réclame plus) — réouverture".into());
                }
                drop(stream);
                sleep_unless_shutdown(&sh, Duration::from_millis(500));
                continue 'device;
            }
        }
        break 'device;
    }
    sh.output_lost.store(false, Ordering::Relaxed);
    Ok(())
}

/// Ouvre la sortie audio. Rend le flux, le drapeau de vie posé par le
/// rappel d'erreur, et un compteur d'appels du rappel de données.
/// Le dernier membre dit qu'on tourne sur un périphérique de repli.
type OpenedOutput =
    (OutputStream, Arc<AtomicBool>, Arc<std::sync::atomic::AtomicU64>, bool);

fn open_output(
    sh: &Arc<Shared>,
    device_name: Option<&str>,
    native: bool,
) -> anyhow::Result<OpenedOutput> {
    let sh = sh.clone();
    // Compteur d'appels du fournisseur d'échantillons : c'est lui qui révèle
    // un pilote devenu muet sans le dire.
    let ticks = Arc::new(std::sync::atomic::AtomicU64::new(0));

    // Moteur natif d'abord, s'il est demandé ; toute erreur fait retomber
    // sur cpal dans la foulée — au pire, on se comporte comme avant.
    #[cfg(windows)]
    if native {
        let alive = Arc::new(AtomicBool::new(true));
        let sh_w = sh.clone();
        let ticks_w = ticks.clone();
        match wasapi::open_output(
            device_name,
            move |rate| output_writer(sh_w, ticks_w, rate),
            alive.clone(),
        ) {
            Ok((stream, fallback)) => {
                return Ok((OutputStream::Native(stream), alive, ticks, fallback));
            }
            Err(e) => {
                tracing::warn!("sortie native indisponible : {e:#} — repli sur cpal");
                journal(format!("moteur natif indisponible (sortie) : {e:#} — repli cpal"));
            }
        }
    }
    #[cfg(not(windows))]
    let _ = native;
    let host = cpal::default_host();
    let (device, fallback) = pick_device(&host, device_name, false);
    if fallback {
        if let Some(name) = device_name {
            tracing::warn!("sortie « {name} » introuvable, retour au défaut");
        }
    }
    let device = device.context("aucun périphérique de sortie audio trouvé")?;
    let supported = device.default_output_config().context("config sortie")?;
    let out_rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    tracing::info!(
        "sortie : {} ({} Hz, {} canaux)",
        device.name().unwrap_or_default(),
        out_rate,
        channels
    );
    journal(format!(
        "sortie ouverte : {} ({} Hz, {} canaux){}",
        device.name().unwrap_or_default(),
        out_rate,
        channels,
        if fallback { " — repli, le périphérique réglé est introuvable" } else { "" },
    ));

    let write_frames = output_writer(sh.clone(), ticks.clone(), out_rate);
    let alive = Arc::new(AtomicBool::new(true));
    let stream =
        build_output_stream(&device, &supported, channels, write_frames, alive.clone())?;
    stream.play()?;
    Ok((OutputStream::Cpal(stream), alive, ticks, fallback))
}

/// Fabrique le fournisseur d'échantillons de la sortie : `n` échantillons
/// mono, tirés du mix 48 kHz puis rééchantillonnés vers `out_rate`. Une
/// fabrique, et non une fermeture toute prête : la fréquence réelle du flux
/// n'est connue qu'une fois le périphérique ouvert, quel que soit le moteur.
fn output_writer(
    sh_cb: Arc<Shared>,
    ticks_cb: Arc<std::sync::atomic::AtomicU64>,
    out_rate: u32,
) -> impl FnMut(usize) -> Vec<f32> + Send + 'static {
    // Rééchantillonne le mix 48 kHz vers la fréquence du périphérique.
    let mut resampler = CubicResampler::new(SAMPLE_RATE as f64 / out_rate as f64);
    move |mono_needed: usize| -> Vec<f32> {
        ticks_cb.fetch_add(1, Ordering::Relaxed);
        // Tire du 48 kHz mixé tant que le rééchantillonneur en réclame.
        let mut out = vec![0f32; mono_needed];
        while !resampler.can_pull(mono_needed) {
            let mut mix = [0f32; FRAME_SAMPLES];
            let mut any = false;
            {
                // Fil temps réel : on ne touche qu'aux tampons déjà décodés.
                // Le verrou de la carte n'est tenu que le temps de la
                // parcourir, et celui de chaque tampon que le temps d'y puiser
                // des échantillons — jamais derrière un décodage.
                let volumes = sh_cb.volumes.lock().unwrap();
                let playouts = sh_cb.playouts.lock().unwrap();
                for (id, playout) in playouts.iter() {
                    let gain = volumes.get(id).copied().unwrap_or(1.0);
                    any |= playout.lock().unwrap().mix_into(&mut mix, gain);
                }
            }
            // Retour local : test micro (« s'écouter ») et son de test.
            {
                let mut loopback = sh_cb.loopback_buf.lock().unwrap();
                if !loopback.is_empty() {
                    for o in mix.iter_mut() {
                        match loopback.pop_front() {
                            Some(s) => *o += s,
                            None => break,
                        }
                    }
                }
            }
            // Effets sonores (notifications, tonalité de test) : mixés avant
            // le volume général, pour que baisser le son baisse aussi les
            // notifications — et écrêtés avec le reste plutôt qu'après.
            {
                let mut fx = sh_cb.effects_buf.lock().unwrap();
                if !fx.is_empty() {
                    let gain = load_f32(&sh_cb.effects_gain);
                    for o in mix.iter_mut() {
                        match fx.pop_front() {
                            Some(s) => *o += s * gain,
                            None => break,
                        }
                    }
                }
            }
            // Volume de sortie global + limiteur doux (anti-saturation quand
            // plusieurs voix fortes se superposent).
            let out_gain = load_f32(&sh_cb.output_gain);
            for o in mix.iter_mut() {
                *o = soft_clip(*o * out_gain);
            }
            if any {
                // De la voix distante part réellement vers la carte son.
                sh_cb.counters.played.fetch_add(FRAME_SAMPLES as u64, Ordering::Relaxed);
            }
            resampler.push(&mix);
        }
        resampler.pull(&mut out);
        out
    }
}

/// Construit le flux de sortie quel que soit le format d'échantillons du
/// périphérique. `write_frames(n)` fournit n échantillons mono f32.
fn build_output_stream(
    device: &cpal::Device,
    supported: &cpal::SupportedStreamConfig,
    channels: usize,
    write_frames: impl FnMut(usize) -> Vec<f32> + Send + 'static,
    alive: Arc<AtomicBool>,
) -> anyhow::Result<cpal::Stream> {
    fn build<T>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        channels: usize,
        mut wf: impl FnMut(usize) -> Vec<f32> + Send + 'static,
        alive: Arc<AtomicBool>,
    ) -> anyhow::Result<cpal::Stream>
    where
        T: cpal::SizedSample + dasp_sample::FromSample<f32>,
    {
        let stream = device.build_output_stream(
            config,
            move |data: &mut [T], _: &_| {
                let mono = wf(data.len() / channels);
                for (frame, &s) in data.chunks_exact_mut(channels).zip(mono.iter()) {
                    let v = T::from_sample_(s.clamp(-1.0, 1.0));
                    for c in frame.iter_mut() {
                        *c = v;
                    }
                }
            },
            // Comme pour le micro : signaler, pas seulement tracer — et une
            // seule entrée de journal par panne.
            move |e| {
                if alive.swap(false, Ordering::Relaxed) {
                    tracing::warn!("erreur flux sortie : {e}");
                    journal(format!("erreur du flux de sortie : {e}"));
                }
            },
            None,
        )?;
        Ok(stream)
    }

    use cpal::SampleFormat as SF;
    let config = supported.config();
    let a = alive;
    match supported.sample_format() {
        SF::F32 => build::<f32>(device, &config, channels, write_frames, a),
        SF::I16 => build::<i16>(device, &config, channels, write_frames, a),
        SF::I32 => build::<i32>(device, &config, channels, write_frames, a),
        SF::U16 => build::<u16>(device, &config, channels, write_frames, a),
        SF::F64 => build::<f64>(device, &config, channels, write_frames, a),
        SF::I8 => build::<i8>(device, &config, channels, write_frames, a),
        SF::U8 => build::<u8>(device, &config, channels, write_frames, a),
        fmt => bail!("format sortie non géré : {fmt:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Le compteur sert de nonce, et la clé de chiffrement ne change qu'au
    /// redémarrage du serveur : deux moteurs successifs — un changement de
    /// micro, une reconnexion — ne doivent pas repartir du même point, sous
    /// peine de rejouer des nonces déjà employés sous la même clé.
    #[test]
    fn two_senders_never_start_from_the_same_counter() {
        let noop: DatagramSend = Arc::new(|_: &[u8]| {});
        let starts: Vec<u64> = (0..8)
            .map(|_| Sender::new(42, 32_000, noop.clone()).unwrap().counter)
            .collect();

        // Aucun départ commun, et surtout jamais la valeur fixe d'avant.
        for (i, a) in starts.iter().enumerate() {
            assert_ne!(*a, 1, "un départ fixe rejouerait les nonces");
            for b in &starts[i + 1..] {
                assert_ne!(a, b, "deux moteurs sont partis du même compteur");
            }
        }
        // Borné à 48 bits : le compteur a toute la place de monter sans
        // jamais déborder, même sur une session interminable.
        assert!(starts.iter().all(|c| *c < 1 << 48));
    }

    /// Windows préfixe « N- » un périphérique ré-énuméré (lancement de jeu
    /// qui secoue l'USB) : le même casque doit être reconnu sous son nouveau
    /// nom, sans confondre pour autant des noms réellement différents.
    #[test]
    fn device_names_survive_usb_renumbering() {
        assert_eq!(
            normalized_device_name("Microphone (2- HyperX QuadCast S)"),
            "Microphone (HyperX QuadCast S)"
        );
        assert_eq!(normalized_device_name("2- USB Audio Device"), "USB Audio Device");
        assert_eq!(
            normalized_device_name("Micro (12- Foo (3- Bar))"),
            "Micro (Foo (Bar))"
        );
        // Sans compteur, rien ne bouge.
        assert_eq!(
            normalized_device_name("Casque (Arctis Nova 7)"),
            "Casque (Arctis Nova 7)"
        );
        // Un tiret sans chiffres, ou des chiffres sans « -  », restent tels
        // quels : « 7.1 » n'est pas un compteur de ré-énumération.
        assert_eq!(normalized_device_name("Micro (A- B)"), "Micro (A- B)");
        assert_eq!(
            normalized_device_name("Haut-parleurs (7.1 Surround)"),
            "Haut-parleurs (7.1 Surround)"
        );
    }

    #[test]
    fn journal_keeps_recent_events_bounded() {
        for i in 0..(JOURNAL_CAP + 50) {
            journal(format!("évt {i}"));
        }
        let snap = journal_snapshot();
        assert_eq!(snap.len(), JOURNAL_CAP);
        // Les plus anciens sont tombés, le plus récent est là.
        assert_eq!(snap.last().unwrap().1, format!("évt {}", JOURNAL_CAP + 49));
    }

    #[test]
    fn agc_tames_loud_and_boosts_quiet() {
        let mut agc = Agc::new();
        // Signal fort : le gain doit descendre vite sous 1.
        for _ in 0..10 {
            let mut frame = [0.9f32; FRAME_SAMPLES];
            agc.process(&mut frame, 0.30);
        }
        let mut frame = [0.9f32; FRAME_SAMPLES];
        agc.process(&mut frame, 0.30);
        let peak = frame.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(peak < 0.5, "voix forte non atténuée : {peak}");

        // Signal faible : le gain remonte progressivement au-dessus de 1.
        let mut agc = Agc::new();
        for _ in 0..300 {
            let mut frame = [0.05f32; FRAME_SAMPLES];
            agc.process(&mut frame, 0.30);
        }
        let mut frame = [0.05f32; FRAME_SAMPLES];
        agc.process(&mut frame, 0.30);
        let peak = frame.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.15, "voix faible non amplifiée : {peak}");

        // Silence : le gain ne doit pas pomper le bruit.
        let mut agc = Agc::new();
        for _ in 0..300 {
            let mut frame = [0.001f32; FRAME_SAMPLES];
            agc.process(&mut frame, 0.30);
        }
        assert!((agc.gain - 1.0).abs() < 0.01, "l'AGC a pompé le silence");
    }

    #[test]
    fn noise_gate_closes_on_silence_opens_on_voice() {
        let mut gate = NoiseGate::new();
        // Bruit résiduel faible : la porte doit se fermer.
        for _ in 0..100 {
            let mut frame = [0.01f32; FRAME_SAMPLES];
            gate.process(&mut frame, 0.05);
        }
        let mut frame = [0.01f32; FRAME_SAMPLES];
        gate.process(&mut frame, 0.05);
        assert!(frame[0].abs() < 0.002, "porte pas fermée : {}", frame[0]);
        // La voix rouvre la porte rapidement.
        for _ in 0..5 {
            let mut frame = [0.3f32; FRAME_SAMPLES];
            gate.process(&mut frame, 0.05);
        }
        let mut frame = [0.3f32; FRAME_SAMPLES];
        gate.process(&mut frame, 0.05);
        assert!(frame[0].abs() > 0.25, "porte pas rouverte : {}", frame[0]);
        // Seuil à zéro = transparente.
        let mut gate = NoiseGate::new();
        let mut frame = [0.01f32; FRAME_SAMPLES];
        gate.process(&mut frame, 0.0);
        assert_eq!(frame[0], 0.01);
    }

    #[test]
    fn deepfilternet_loads_and_processes() {
        let mut deep = DeepDenoiser::new().expect("chargement du modèle DeepFilterNet3");
        let mut frame = [0f32; FRAME_SAMPLES];
        // Bruit blanc pseudo-aléatoire : DFN3 doit l'atténuer fortement.
        let mut x = 0x12345678u32;
        for s in frame.iter_mut() {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            *s = (x as f32 / u32::MAX as f32 - 0.5) * 0.2;
        }
        let noisy_peak = frame.iter().fold(0f32, |m, s| m.max(s.abs()));
        for _ in 0..10 {
            deep.process(&mut frame);
        }
        let clean_peak = frame.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(
            clean_peak < noisy_peak * 0.5,
            "bruit blanc pas assez atténué : {noisy_peak} -> {clean_peak}"
        );
    }

    #[test]
    fn monitor_roundtrips_through_codec() {
        let mut monitor = Monitor::new(64_000);
        let mut input = [0f32; FRAME_SAMPLES];
        for (i, s) in input.iter_mut().enumerate() {
            *s = (i as f32 * 0.05).sin() * 0.3;
        }
        let mut out = [0f32; FRAME_SAMPLES];
        // Plusieurs trames : le codec a besoin d'un peu d'historique.
        for _ in 0..5 {
            monitor.process(&input, 64_000, &mut out);
        }
        // Le signal décodé doit exister et rester borné.
        let peak = out.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.05 && peak <= 1.0, "aller-retour codec suspect : {peak}");
    }

    #[test]
    fn soft_clip_is_transparent_then_bounded() {
        // Transparent sous le genou.
        assert_eq!(soft_clip(0.5), 0.5);
        assert_eq!(soft_clip(-0.5), -0.5);
        // Borné et monotone au-dessus.
        assert!(soft_clip(1.5) < 1.0);
        assert!(soft_clip(10.0) < 1.0);
        assert!(soft_clip(1.0) < soft_clip(2.0));
        assert_eq!(soft_clip(-3.0), -soft_clip(3.0));
    }

    #[test]
    fn encrypt_decrypt_roundtrip_and_tamper_rejected() {
        let key = [7u8; 32];
        let cipher = XChaCha20Poly1305::new(&key.into());
        let payload = b"trame opus factice";

        let sealed = cipher.encrypt(&nonce_for(42, 1), payload.as_ref()).unwrap();
        assert_eq!(sealed.len(), payload.len() + 16); // + tag Poly1305

        // Déchiffrement correct.
        let plain = cipher.decrypt(&nonce_for(42, 1), sealed.as_ref()).unwrap();
        assert_eq!(plain, payload);

        // Mauvais émetteur ou mauvais compteur -> nonce différent -> rejet.
        assert!(cipher.decrypt(&nonce_for(43, 1), sealed.as_ref()).is_err());
        assert!(cipher.decrypt(&nonce_for(42, 2), sealed.as_ref()).is_err());

        // Charge altérée -> rejet.
        let mut tampered = sealed.clone();
        tampered[0] ^= 1;
        assert!(cipher.decrypt(&nonce_for(42, 1), tampered.as_ref()).is_err());
    }
}
