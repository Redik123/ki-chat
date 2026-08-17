//! Couche réseau du client GUI : connexion QUIC (contrôle + datagrammes
//! voix) pilotée par un thread dédié avec son propre runtime tokio,
//! commandes et événements échangés par canaux avec le thread UI.

use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};

use ki_client_quic::{quinn, QuicClient};
use ki_protocol::{ClientMsg, ServerMsg};
use ki_voice::{VoiceConfig, VoiceEngine};
use tokio::sync::mpsc as tokio_mpsc;

pub enum Event {
    Msg(ServerMsg),
    ConnectFailed(String),
    Disconnected,
}

pub enum Cmd {
    Send(ClientMsg),
    Quit,
}

/// Préférences audio appliquées au moteur voix.
#[derive(Clone)]
pub struct VoicePrefs {
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    /// Mode de suppression de bruit (ki_voice::NOISE_*).
    pub noise_mode: u8,
    /// Volumes par utilisateur (user_id -> gain, 1.0 = 100 %).
    pub volumes: std::collections::HashMap<u64, f32>,
    /// Gain d'entrée micro (1.0 = 100 %).
    pub input_gain: f32,
    /// Volume de sortie global (1.0 = 100 %).
    pub output_gain: f32,
    /// Seuil d'activation vocale (0.0 = désactivé).
    pub vad_threshold: f32,
    /// Maintien VAD en ms.
    pub vad_hangover_ms: u32,
    /// Débit Opus en bits/s.
    pub bitrate: i32,
    /// Gain automatique (AGC).
    pub agc: bool,
    /// Niveau cible de l'AGC (0..1).
    pub agc_target: f32,
    /// Porte de bruit (0.0 = désactivée).
    pub gate_threshold: f32,
    /// Tampon de gigue imposé en trames (0 = adaptatif).
    pub jitter_frames: usize,
    /// Redondance neuronale DRED (0 = désactivée, sinon valeur du CTL).
    pub dred: i32,
}

/// Identité voix reçue du serveur, conservée pour redémarrer le moteur
/// (changement de périphérique) sans se reconnecter.
#[derive(Clone, Copy)]
pub struct VoiceParams {
    pub user_id: u64,
    pub key: [u8; 32],
}

pub struct Credentials {
    pub username: String,
    pub password: String,
    pub invite: Option<String>,
}

pub struct NetHandle {
    cmd_tx: tokio_mpsc::UnboundedSender<Cmd>,
    pub events: std_mpsc::Receiver<Event>,
    /// Rempli par le thread réseau à la réception du Welcome.
    pub engine: Arc<Mutex<Option<VoiceEngine>>>,
    pub voice_params: Arc<Mutex<Option<VoiceParams>>>,
    /// Connexion QUIC (RTT, envoi de datagrammes).
    conn: Arc<Mutex<Option<quinn::Connection>>>,
    /// Où aiguiller les datagrammes voix entrants (remplacé à chaque
    /// redémarrage du moteur).
    voice_feed: Arc<Mutex<Option<std_mpsc::Sender<Vec<u8>>>>>,
    /// Fil réseau, pour pouvoir attendre qu'il ait vraiment fermé la
    /// connexion avant que le processus ne s'arrête.
    worker: Option<std::thread::JoinHandle<()>>,
}

impl NetHandle {
    pub fn send(&self, msg: ClientMsg) {
        let _ = self.cmd_tx.send(Cmd::Send(msg));
    }

    pub fn sender(&self) -> tokio_mpsc::UnboundedSender<Cmd> {
        self.cmd_tx.clone()
    }

    /// Ferme la connexion **et attend** que ce soit fait.
    ///
    /// Poster l'ordre sans attendre ne suffisait pas : le processus
    /// s'arrêtait avant que le fil réseau ne se réveille, la trame de
    /// fermeture QUIC ne partait jamais, et le serveur gardait la session
    /// ouverte jusqu'à son expiration d'inactivité — trente secondes pendant
    /// lesquelles on ne pouvait pas se reconnecter, et pendant lesquelles les
    /// autres continuaient de nous voir dans la liste.
    pub fn quit(&mut self) {
        let _ = self.cmd_tx.send(Cmd::Quit);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }

    /// RTT QUIC mesuré, en ms.
    pub fn rtt_ms(&self) -> Option<u32> {
        self.conn
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| c.rtt().as_millis() as u32)
    }

    /// Redémarre le moteur voix avec de nouvelles préférences (périphérique,
    /// débruitage...). Fait dans un thread : l'arrêt peut prendre ~200 ms.
    pub fn restart_voice(&self, prefs: VoicePrefs) {
        let engine_slot = self.engine.clone();
        let params_slot = self.voice_params.clone();
        let conn_slot = self.conn.clone();
        let feed_slot = self.voice_feed.clone();
        std::thread::spawn(move || {
            let Some(params) = *params_slot.lock().unwrap() else { return };
            let Some(conn) = conn_slot.lock().unwrap().clone() else { return };
            // Le verrou est relâché **avant** l'arrêt : `shutdown` attend la
            // fin des fils audio, et le fil de l'interface prend ce même
            // verrou à chaque image — le tenir pendant l'attente figeait
            // l'affichage le temps du changement de périphérique.
            let old = engine_slot.lock().unwrap().take();
            if let Some(old) = old {
                old.shutdown();
            }
            let (tx, rx) = std_mpsc::channel();
            *feed_slot.lock().unwrap() = Some(tx);
            match start_engine(params, &prefs, &conn, rx) {
                Ok(engine) => *engine_slot.lock().unwrap() = Some(engine),
                Err(e) => tracing::error!("redémarrage vocal impossible : {e:#}"),
            }
        });
    }
}

fn start_engine(
    params: VoiceParams,
    prefs: &VoicePrefs,
    conn: &quinn::Connection,
    rx: std_mpsc::Receiver<Vec<u8>>,
) -> anyhow::Result<VoiceEngine> {
    let mut cfg = VoiceConfig::new(params.user_id, params.key);
    cfg.input_device = prefs.input_device.clone();
    cfg.output_device = prefs.output_device.clone();
    cfg.noise_mode = prefs.noise_mode;
    cfg.volumes = prefs.volumes.clone();
    cfg.input_gain = prefs.input_gain;
    cfg.output_gain = prefs.output_gain;
    cfg.vad_threshold = prefs.vad_threshold;
    cfg.vad_hangover_ms = prefs.vad_hangover_ms;
    cfg.bitrate = prefs.bitrate;
    cfg.agc = prefs.agc;
    cfg.agc_target = prefs.agc_target;
    cfg.gate_threshold = prefs.gate_threshold;
    cfg.jitter_frames = prefs.jitter_frames;
    let engine = VoiceEngine::start(cfg, ki_client_quic::datagram_sender(conn), rx)?;
    engine.set_dred(prefs.dred);
    Ok(engine)
}

pub fn connect(
    url: String,
    creds: Credentials,
    prefs: VoicePrefs,
    ctx: eframe::egui::Context,
) -> NetHandle {
    let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
    let (event_tx, event_rx) = std_mpsc::channel();
    let engine = Arc::new(Mutex::new(None));
    let voice_params = Arc::new(Mutex::new(None));
    let conn = Arc::new(Mutex::new(None));
    let voice_feed: Arc<Mutex<Option<std_mpsc::Sender<Vec<u8>>>>> =
        Arc::new(Mutex::new(None));

    let worker = std::thread::spawn({
        let (engine, voice_params, conn, voice_feed) =
            (engine.clone(), voice_params.clone(), conn.clone(), voice_feed.clone());
        move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = event_tx.send(Event::ConnectFailed(format!("runtime : {e}")));
                    ctx.request_repaint();
                    return;
                }
            };
            rt.block_on(run(
                url, creds, prefs, cmd_rx, event_tx, engine, voice_params, conn, voice_feed, ctx,
            ));
        }
    });

    NetHandle {
        cmd_tx,
        events: event_rx,
        engine,
        voice_params,
        conn,
        voice_feed,
        worker: Some(worker),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    url: String,
    creds: Credentials,
    prefs: VoicePrefs,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<Cmd>,
    event_tx: std_mpsc::Sender<Event>,
    engine_slot: Arc<Mutex<Option<VoiceEngine>>>,
    params_slot: Arc<Mutex<Option<VoiceParams>>>,
    conn_slot: Arc<Mutex<Option<quinn::Connection>>>,
    feed_slot: Arc<Mutex<Option<std_mpsc::Sender<Vec<u8>>>>>,
    ctx: eframe::egui::Context,
) {
    let emit = |e: Event| {
        let _ = event_tx.send(e);
        ctx.request_repaint();
    };

    let mut client = match QuicClient::connect(&url).await {
        Ok(c) => c,
        Err(e) => {
            emit(Event::ConnectFailed(format!("{e:#}")));
            return;
        }
    };
    let auth = ClientMsg::Auth {
        username: creds.username,
        password: creds.password,
        invite: creds.invite,
    };
    if client.send_msg(&auth).await.is_err() {
        emit(Event::ConnectFailed("échec de l'authentification".into()));
        return;
    }
    let (mut writer, mut reader) = client.split();
    *conn_slot.lock().unwrap() = Some(writer.conn.clone());

    // Datagrammes voix entrants -> moteur audio (via l'aiguillage).
    {
        let conn = writer.conn.clone();
        let feed = feed_slot.clone();
        tokio::spawn(async move {
            while let Ok(dat) = conn.read_datagram().await {
                let feed = feed.lock().unwrap();
                if let Some(tx) = feed.as_ref() {
                    let _ = tx.send(dat.to_vec());
                }
            }
        });
    }

    let cleanup = |engine_slot: &Arc<Mutex<Option<VoiceEngine>>>| {
        let old = engine_slot.lock().unwrap().take();
        if let Some(e) = old {
            e.shutdown();
        }
    };

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                Some(Cmd::Send(msg)) => {
                    if writer.send_msg(&msg).await.is_err() {
                        cleanup(&engine_slot);
                        emit(Event::Disconnected);
                        return;
                    }
                }
                Some(Cmd::Quit) | None => {
                    cleanup(&engine_slot);
                    writer.close_gracefully().await;
                    return;
                }
            },
            msg = reader.next_msg() => match msg {
                Some(msg) => {
                    if let ServerMsg::Welcome { user_id, voice_key, .. } = &msg {
                        let key: Option<[u8; 32]> = ki_protocol::hex_decode(voice_key)
                            .and_then(|v| v.try_into().ok());
                        match key {
                            Some(key) => {
                                let params = VoiceParams { user_id: *user_id, key };
                                *params_slot.lock().unwrap() = Some(params);
                                let (tx, rx) = std_mpsc::channel();
                                *feed_slot.lock().unwrap() = Some(tx);
                                match start_engine(params, &prefs, &writer.conn, rx) {
                                    Ok(engine) => {
                                        *engine_slot.lock().unwrap() = Some(engine)
                                    }
                                    Err(e) => {
                                        tracing::error!("vocal indisponible : {e:#}")
                                    }
                                }
                            }
                            None => tracing::error!("clé voix invalide reçue du serveur"),
                        }
                    }
                    emit(Event::Msg(msg));
                }
                None => {
                    cleanup(&engine_slot);
                    emit(Event::Disconnected);
                    return;
                }
            },
        }
    }
}
