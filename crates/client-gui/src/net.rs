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
    /// Empreinte du certificat présentée par le serveur, à retenir.
    Fingerprint(String),
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
    /// Moteur audio Windows natif (WASAPI direct) — cpal sinon.
    pub native_audio: bool,
    /// Mode brut du micro (effets tiers court-circuités). Natif seulement.
    pub raw_mic: bool,
    /// Micro en catégorie « communications » dès l'ouverture (partage de la
    /// voie de traitement avec la voix des jeux). Le moteur y bascule seul
    /// en cas de micro affamé ; la case rend le choix permanent.
    pub comms_mic: bool,
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
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VoiceParams {
    pub user_id: u64,
    pub key: [u8; 32],
}

pub struct Credentials {
    pub username: String,
    pub password: String,
    pub invite: Option<String>,
    /// Empreinte du certificat retenue lors des connexions précédentes.
    /// Vide = on ne connaît pas encore ce serveur.
    pub fingerprint: String,
}

/// Ce qui doit **survivre à une coupure** : le moteur voix et ses attaches.
///
/// Tout cela appartenait au `NetHandle`, c'est-à-dire à la connexion. Fermer
/// l'une emportait l'autre : chaque hoquet du réseau refermait le micro et la
/// sortie, puis les rouvrait — et si un jeu s'était emparé du périphérique
/// entre-temps, en mode exclusif, Windows ne le rendait plus. La reprise
/// automatique, en rendant les coupures routinières, aurait transformé ce
/// risque rare en risque régulier.
///
/// Le lien appartient donc à l'application, qui le prête aux connexions
/// successives. Une connexion qui meurt ne prend plus le son avec elle.
#[derive(Clone, Default)]
pub struct VoiceLink {
    /// Le moteur audio lui-même. Il tient les périphériques ouverts.
    pub engine: Arc<Mutex<Option<VoiceEngine>>>,
    /// Identité et clé voix du serveur, telles qu'annoncées au dernier
    /// `Welcome`. Comparées au suivant : identiques, le moteur en place
    /// convient, et l'on ne rouvre rien.
    pub params: Arc<Mutex<Option<VoiceParams>>>,
    /// La connexion QUIC **du moment**. L'émetteur de datagrammes du moteur
    /// lit cet emplacement à chaque trame : c'est par là qu'il suit les
    /// reconnexions sans être reconstruit.
    pub conn: Arc<Mutex<Option<quinn::Connection>>>,
    /// Où aiguiller les datagrammes voix entrants. Remplacé à chaque
    /// redémarrage du moteur, et **conservé** d'une connexion à l'autre.
    pub feed: Arc<Mutex<Option<std_mpsc::SyncSender<bytes::Bytes>>>>,
    /// Jeton de génération des (re)démarrages du moteur. Chaque redémarrage
    /// en prend un ; seul le porteur du plus récent a le droit d'installer le
    /// sien. Sans ce jeton, deux changements de périphérique rapprochés
    /// pouvaient laisser l'aiguillage branché sur un moteur déjà arrêté :
    /// surdité totale.
    gen: Arc<std::sync::atomic::AtomicU64>,
}

impl VoiceLink {
    /// Arrête le moteur et relâche les périphériques.
    ///
    /// Appelé aux seules sorties **définitives** : on quitte, on se
    /// déconnecte soi-même, on a été expulsé. Une coupure subie ne passe
    /// jamais par là — c'est tout l'objet de ce découpage.
    pub fn arreter(&self) {
        use std::sync::atomic::Ordering;
        // Invalider d'abord les redémarrages en vol, sans quoi l'un d'eux
        // réinstallerait un moteur juste après l'arrêt.
        self.gen.fetch_add(1, Ordering::SeqCst);
        *self.params.lock().unwrap() = None;
        *self.feed.lock().unwrap() = None;
        *self.conn.lock().unwrap() = None;
        let old = self.engine.lock().unwrap().take();
        if let Some(e) = old {
            e.shutdown();
        }
    }

    /// Redémarre le moteur voix avec de nouvelles préférences (périphérique,
    /// débruitage...). Fait dans un thread : l'arrêt peut prendre ~200 ms.
    ///
    /// Sérialisé par jeton : deux redémarrages rapprochés (deux réglages coup
    /// sur coup) lançaient deux fils sans ordre — le perdant pouvait
    /// réinstaller les ANCIENNES préférences, ou laisser l'aiguillage des
    /// datagrammes branché sur un moteur déjà arrêté : surdité totale. Seul
    /// le porteur du jeton le plus récent va au bout ; les autres renoncent
    /// au premier point de contrôle, et leur moteur éventuel part au Drop.
    ///
    /// Ne demande **plus** de connexion vivante : changer de micro pendant
    /// une coupure fonctionne, et le moteur repart aussitôt.
    pub fn restart_voice(&self, prefs: VoicePrefs) {
        use std::sync::atomic::Ordering;
        let engine_slot = self.engine.clone();
        let params_slot = self.params.clone();
        let conn_slot = self.conn.clone();
        let feed_slot = self.feed.clone();
        let gen_slot = self.gen.clone();
        let gen = gen_slot.fetch_add(1, Ordering::SeqCst) + 1;
        std::thread::spawn(move || {
            let Some(params) = *params_slot.lock().unwrap() else { return };
            // Le verrou est relâché **avant** l'arrêt : `shutdown` attend la
            // fin des fils audio, et le fil de l'interface prend ce même
            // verrou à chaque image — le tenir pendant l'attente figeait
            // l'affichage le temps du changement de périphérique.
            let old = {
                let mut slot = engine_slot.lock().unwrap();
                if gen_slot.load(Ordering::SeqCst) != gen {
                    return; // un redémarrage plus récent est déjà passé
                }
                slot.take()
            };
            if let Some(old) = old {
                old.shutdown();
            }
            // L'attente de shutdown (~200 ms) laisse tout le temps à un
            // redémarrage plus récent d'arriver : on revérifie.
            if gen_slot.load(Ordering::SeqCst) != gen {
                return;
            }
            let (tx, rx) = std_mpsc::sync_channel(ki_voice::VOICE_QUEUE);
            match start_engine(params, &prefs, &conn_slot, rx) {
                Ok(engine) => {
                    let mut slot = engine_slot.lock().unwrap();
                    if gen_slot.load(Ordering::SeqCst) == gen {
                        // L'aiguillage et le moteur s'installent ensemble,
                        // sous le même verrou de moteur : plus de fenêtre où
                        // les datagrammes partent vers un moteur mort.
                        *feed_slot.lock().unwrap() = Some(tx);
                        *slot = Some(engine);
                    }
                    // Sinon : moteur obsolète, son Drop l'arrête.
                }
                Err(e) => tracing::error!("redémarrage vocal impossible : {e:#}"),
            }
        });
    }
}

pub struct NetHandle {
    cmd_tx: tokio_mpsc::UnboundedSender<Cmd>,
    pub events: std_mpsc::Receiver<Event>,
    /// Le lien audio, **prêté** par l'application. La connexion s'en sert,
    /// elle ne le possède pas : sa fin ne l'emporte pas.
    link: VoiceLink,
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
        self.link
            .conn
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| c.rtt().as_millis() as u32)
    }

}

fn start_engine(
    params: VoiceParams,
    prefs: &VoicePrefs,
    conn: &Arc<Mutex<Option<quinn::Connection>>>,
    rx: std_mpsc::Receiver<bytes::Bytes>,
) -> anyhow::Result<VoiceEngine> {
    let mut cfg = VoiceConfig::new(params.user_id, params.key);
    cfg.input_device = prefs.input_device.clone();
    cfg.output_device = prefs.output_device.clone();
    cfg.native_audio = prefs.native_audio;
    cfg.raw_mic = prefs.raw_mic;
    cfg.comms_mic = prefs.comms_mic;
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
    // L'émetteur suit l'**emplacement** de connexion, pas une connexion :
    // c'est ce qui laisse ce moteur traverser une reconnexion intact.
    let engine =
        VoiceEngine::start(cfg, ki_client_quic::datagram_sender_slot(conn.clone()), rx)?;
    engine.set_dred(prefs.dred);
    Ok(engine)
}

/// Ouvre une connexion, en lui **prêtant** le lien audio de l'application.
///
/// Le lien n'est pas créé ici : il vit plus longtemps qu'une connexion, et
/// c'est précisément ce qui permet à une reconnexion de ne pas rouvrir les
/// périphériques.
pub fn connect(
    url: String,
    creds: Credentials,
    prefs: VoicePrefs,
    link: VoiceLink,
    ctx: eframe::egui::Context,
) -> NetHandle {
    let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
    let (event_tx, event_rx) = std_mpsc::channel();

    let worker = std::thread::spawn({
        let link = link.clone();
        move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = event_tx.send(Event::ConnectFailed(format!("runtime : {e}")));
                    ctx.request_repaint();
                    return;
                }
            };
            rt.block_on(run(url, creds, prefs, cmd_rx, event_tx, link, ctx));
        }
    });

    NetHandle { cmd_tx, events: event_rx, link, worker: Some(worker) }
}

async fn run(
    url: String,
    creds: Credentials,
    prefs: VoicePrefs,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<Cmd>,
    event_tx: std_mpsc::Sender<Event>,
    link: VoiceLink,
    ctx: eframe::egui::Context,
) {
    use std::sync::atomic::Ordering;
    let VoiceLink {
        engine: engine_slot,
        params: params_slot,
        conn: conn_slot,
        feed: feed_slot,
        gen: restart_gen,
    } = link;
    let emit = |e: Event| {
        let _ = event_tx.send(e);
        ctx.request_repaint();
    };

    let known = (!creds.fingerprint.is_empty()).then_some(creds.fingerprint.as_str());
    let mut client = match QuicClient::connect(&url, known).await {
        Ok(c) => c,
        Err(e) => {
            emit(Event::ConnectFailed(format!("{e:#}")));
            return;
        }
    };
    // Première connexion : l'empreinte est retenue pour les suivantes.
    emit(Event::Fingerprint(client.fingerprint.clone()));
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
                    // `dat` est un `Bytes` : le transmettre ne copie rien, là
                    // où `to_vec()` recopiait chaque paquet. Et `try_send` ne
                    // bloque jamais la pompe de datagrammes — file pleine, on
                    // jette : une trame qui attendrait derrière deux secondes
                    // d'arriéré ne vaut plus rien de toute façon.
                    let _ = tx.try_send(dat);
                }
            }
        });
    }

    // Fin de cette connexion — et **rien d'autre**. On repose la connexion,
    // le moteur reste debout avec ses périphériques.
    //
    // C'est le renversement de R2. Ce démontage arrêtait le moteur : la
    // connexion possédait le son, et le perdre coûtait le micro. Il
    // appartient désormais à l'application, seule à décider de l'arrêter
    // (`VoiceLink::arreter`), et seulement pour de bon.
    //
    // Les paramètres et l'aiguillage restent en place à dessein : ils
    // permettent de changer de périphérique **pendant** la coupure, et de
    // reconnaître au prochain `Welcome` que le moteur en cours convient.
    let cleanup = || {
        *conn_slot.lock().unwrap() = None;
    };

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                Some(Cmd::Send(msg)) => {
                    if writer.send_msg(&msg).await.is_err() {
                        cleanup();
                        emit(Event::Disconnected);
                        return;
                    }
                }
                Some(Cmd::Quit) | None => {
                    cleanup();
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
                                // Le moteur en place convient-il ?
                                //
                                // L'identifiant est celui du **compte** : il
                                // ne bouge pas d'une connexion à l'autre. La
                                // clé voix, elle, est retirée au sort à
                                // chaque démarrage du **processus** serveur.
                                // Donc : un hoquet du réseau les laisse
                                // identiques, et le moteur qui tourne fait
                                // parfaitement l'affaire — on ne rouvre ni le
                                // micro ni la sortie. Un vrai redémarrage du
                                // serveur change la clé, et là il faut bien
                                // le refaire : c'est rare, et ça ne tombe pas
                                // au milieu d'une partie.
                                //
                                // Les deux verrous se prennent l'un après
                                // l'autre, jamais imbriqués : c'est gratuit
                                // ici, et ça évite d'avoir à démontrer que
                                // l'ordre est le même partout ailleurs.
                                let memes = *params_slot.lock().unwrap() == Some(params);
                                let inchange =
                                    memes && engine_slot.lock().unwrap().is_some();
                                if inchange {
                                    tracing::info!(
                                        "reconnexion : le moteur voix en place est conservé"
                                    );
                                } else {
                                    *params_slot.lock().unwrap() = Some(params);
                                    // Invalide tout redémarrage encore en vol
                                    // (d'avant une reconnexion) : le moteur du
                                    // Welcome fait foi.
                                    restart_gen.fetch_add(1, Ordering::SeqCst);
                                    // L'ancien moteur sort du verrou **avant**
                                    // d'être arrêté. Son arrêt attend la fin
                                    // des fils audio — deux cents
                                    // millisecondes — et l'interface prend ce
                                    // même verrou à chaque image : le tenir
                                    // pendant l'attente figerait l'affichage,
                                    // et bloquerait au passage la boucle
                                    // réseau qui exécute ceci. Le cas
                                    // n'existait pas avant R2, le moteur
                                    // partant à la déconnexion ; il existe
                                    // depuis qu'il lui survit.
                                    let ancien = engine_slot.lock().unwrap().take();
                                    if let Some(ancien) = ancien {
                                        ancien.shutdown();
                                    }
                                    let (tx, rx) =
                                        std_mpsc::sync_channel(ki_voice::VOICE_QUEUE);
                                    match start_engine(params, &prefs, &conn_slot, rx) {
                                        Ok(engine) => {
                                            let mut slot = engine_slot.lock().unwrap();
                                            *feed_slot.lock().unwrap() = Some(tx);
                                            *slot = Some(engine);
                                        }
                                        Err(e) => {
                                            tracing::error!("vocal indisponible : {e:#}")
                                        }
                                    }
                                }
                            }
                            None => tracing::error!("clé voix invalide reçue du serveur"),
                        }
                    }
                    emit(Event::Msg(msg));
                }
                None => {
                    cleanup();
                    emit(Event::Disconnected);
                    return;
                }
            },
        }
    }
}
