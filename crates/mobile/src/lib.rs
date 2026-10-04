//! ki-chat sur téléphone — étape 0 : l'essai de faisabilité.
//!
//! Une coquille minimale qui prouve que la chaîne marche sur Android :
//! connexion QUIC au serveur, authentification, salon vocal, micro et
//! haut-parleur par ki-voice (cpal → AAudio). Pas encore d'état de client
//! complet : il viendra avec la crate ki-core (étape 1).
//!
//! La page parle au Rust par quatre commandes (`connecter`, `rejoindre_vocal`,
//! `quitter_vocal`, `micro`) plus `etat_voix`, et reçoit en retour des
//! événements : `journal` (une ligne de texte), `salons` (la liste à
//! l'accueil), `deconnecte`.

use std::sync::{Arc, Mutex};

use ki_client_quic::QuicClient;
use ki_protocol::{ChannelKind, ClientMsg, ServerMsg};
use ki_voice::{VoiceConfig, VoiceEngine};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::mpsc;

/// La session en cours : la file des messages vers le serveur et le moteur
/// voix, démarré à l'accueil.
#[derive(Default)]
struct Session {
    envoi: Option<mpsc::UnboundedSender<ClientMsg>>,
    voix: Arc<Mutex<Option<VoiceEngine>>>,
}

type Etat = Mutex<Session>;

#[derive(Clone, Serialize)]
struct Salon {
    id: u32,
    nom: String,
    vocal: bool,
}

#[derive(Serialize)]
struct EtatVoix {
    actif: bool,
    envoyes: u64,
    recus: u64,
    perdus: u64,
    niveau_micro: f32,
    emetteurs: usize,
}

fn journal(app: &AppHandle, ligne: impl Into<String>) {
    let ligne = ligne.into();
    tracing::info!("{ligne}");
    let _ = app.emit("journal", ligne);
}

/// Se connecte, s'authentifie, puis laisse tourner deux tâches : la lecture
/// des messages du serveur et l'envoi de ceux de la page. Rend l'empreinte du
/// certificat du serveur.
#[tauri::command]
async fn connecter(
    app: AppHandle,
    etat: State<'_, Etat>,
    serveur: String,
    pseudo: String,
    mot_de_passe: String,
    invitation: Option<String>,
) -> Result<String, String> {
    // Une connexion à la fois : la précédente se ferme avec sa file.
    {
        let mut s = etat.lock().unwrap();
        s.envoi = None;
        s.voix.lock().unwrap().take();
    }

    journal(&app, format!("connexion à {serveur}…"));
    // Pas encore de carnet de serveurs : on accepte le certificat présenté et
    // on montre son empreinte, comme le client en ligne de commande.
    let mut client = QuicClient::connect(&serveur, None).await.map_err(|e| format!("{e:#}"))?;
    let empreinte = client.fingerprint.clone();
    client
        .send_msg(&ClientMsg::Auth {
            username: pseudo,
            password: mot_de_passe,
            invite: invitation.filter(|i| !i.trim().is_empty()),
            protocole: ki_protocol::PROTOCOLE,
        })
        .await
        .map_err(|e| format!("{e:#}"))?;
    let (mut writer, mut reader) = client.split();

    // Datagrammes voix entrants → moteur.
    let (voix_tx, voix_rx) = std::sync::mpsc::sync_channel::<bytes::Bytes>(ki_voice::VOICE_QUEUE);
    {
        let conn = reader.conn.clone();
        tauri::async_runtime::spawn(async move {
            while let Ok(dat) = conn.read_datagram().await {
                if matches!(
                    voix_tx.try_send(dat),
                    Err(std::sync::mpsc::TrySendError::Disconnected(_))
                ) {
                    break;
                }
            }
        });
    }

    let (envoi_tx, mut envoi_rx) = mpsc::unbounded_channel::<ClientMsg>();
    let voix = {
        let mut s = etat.lock().unwrap();
        s.envoi = Some(envoi_tx);
        s.voix.clone()
    };

    // Le moteur démarre à l'accueil (identité + clé voix).
    let emplacement = Arc::new(Mutex::new(Some(writer.conn.clone())));
    let mut voix_rx = Some(voix_rx);
    let app_lecture = app.clone();
    let voix_lecture = voix.clone();
    tauri::async_runtime::spawn(async move {
        let app = app_lecture;
        while let Some(msg) = reader.next_msg().await {
            match msg {
                ServerMsg::Welcome { user_id, voice_key, channels, .. } => {
                    journal(&app, format!("connecté (id {user_id})"));
                    let salons: Vec<Salon> = channels
                        .iter()
                        .map(|c| Salon {
                            id: c.id,
                            nom: c.name.clone(),
                            vocal: matches!(c.kind, ChannelKind::Voice),
                        })
                        .collect();
                    let _ = app.emit("salons", salons);
                    let cle: Option<[u8; 32]> =
                        ki_protocol::hex_decode(&voice_key).and_then(|v| v.try_into().ok());
                    let (Some(cle), Some(rx)) = (cle, voix_rx.take()) else {
                        journal(&app, "! clé voix invalide reçue du serveur");
                        continue;
                    };
                    let envoi = ki_client_quic::datagram_sender_slot(emplacement.clone());
                    match VoiceEngine::start(VoiceConfig::new(user_id, cle), envoi, rx) {
                        Ok(moteur) => {
                            *voix_lecture.lock().unwrap() = Some(moteur);
                            journal(&app, "moteur voix démarré");
                        }
                        Err(e) => journal(&app, format!("! vocal indisponible : {e:#}")),
                    }
                }
                ServerMsg::Chat { username, text, .. } => {
                    journal(&app, format!("<{username}> {text}"))
                }
                ServerMsg::UserJoined { username, .. } => {
                    journal(&app, format!("* {username} est en ligne"))
                }
                ServerMsg::MemberUpdate { member } => journal(
                    &app,
                    format!(
                        "* {} : {}",
                        member.username,
                        match member.voice {
                            Some(c) => format!("vocal {c}"),
                            None => "hors vocal".into(),
                        }
                    ),
                ),
                ServerMsg::VoiceLocked { channel, .. } => {
                    journal(&app, format!("! salon vocal {channel} verrouillé"))
                }
                ServerMsg::Error { message } => journal(&app, format!("! {message}")),
                _ => {}
            }
        }
        let raison = reader.conn.close_reason();
        voix_lecture.lock().unwrap().take();
        journal(
            &app,
            match raison {
                Some(r) => format!("connexion fermée : {r}"),
                None => "connexion fermée".into(),
            },
        );
        let _ = app.emit("deconnecte", ());
    });

    tauri::async_runtime::spawn(async move {
        while let Some(msg) = envoi_rx.recv().await {
            if let Err(e) = writer.send_msg(&msg).await {
                tracing::warn!("envoi au serveur : {e:#}");
                break;
            }
        }
        writer.close_gracefully().await;
    });

    Ok(empreinte)
}

fn envoyer(etat: &State<'_, Etat>, msg: ClientMsg) -> Result<(), String> {
    let s = etat.lock().unwrap();
    let envoi = s.envoi.as_ref().ok_or("pas connecté")?;
    envoi.send(msg).map_err(|_| "connexion fermée".to_string())
}

#[tauri::command]
fn rejoindre_vocal(etat: State<'_, Etat>, salon: u32) -> Result<(), String> {
    envoyer(&etat, ClientMsg::JoinVoice { channel: salon, password: None })
}

#[tauri::command]
fn quitter_vocal(etat: State<'_, Etat>) -> Result<(), String> {
    if let Some(m) = etat.lock().unwrap().voix.lock().unwrap().as_ref() {
        m.set_transmit(false);
    }
    envoyer(&etat, ClientMsg::LeaveVoice)
}

/// Micro ouvert (émission continue, comme `/mic on` de la ligne de commande)
/// ou coupé.
#[tauri::command]
fn micro(etat: State<'_, Etat>, ouvert: bool) -> Result<(), String> {
    {
        let s = etat.lock().unwrap();
        let voix = s.voix.lock().unwrap();
        let moteur = voix.as_ref().ok_or("vocal indisponible")?;
        moteur.set_transmit(ouvert);
    }
    envoyer(&etat, ClientMsg::VoiceState { speaking: ouvert, muted: !ouvert })
}

#[tauri::command]
fn etat_voix(etat: State<'_, Etat>) -> EtatVoix {
    let s = etat.lock().unwrap();
    let voix = s.voix.lock().unwrap();
    match voix.as_ref() {
        Some(m) => {
            let st = m.stats();
            EtatVoix {
                actif: true,
                envoyes: st.packets_sent,
                recus: st.packets_received,
                perdus: st.packets_lost,
                niveau_micro: st.mic_peak,
                emetteurs: st.active_senders,
            }
        }
        None => EtatVoix {
            actif: false,
            envoyes: 0,
            recus: 0,
            perdus: 0,
            niveau_micro: 0.0,
            emetteurs: 0,
        },
    }
}

fn traces() {
    use tracing_subscriber::prelude::*;
    let filtre = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info".into());
    #[cfg(target_os = "android")]
    {
        let _ = tracing_subscriber::registry()
            .with(filtre)
            .with(tracing_android::layer("ki-chat").expect("couche logcat"))
            .try_init();
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = tracing_subscriber::registry()
            .with(filtre)
            .with(tracing_subscriber::fmt::layer())
            .try_init();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    traces();
    tauri::Builder::default()
        .manage(Etat::default())
        .invoke_handler(tauri::generate_handler![
            connecter,
            rejoindre_vocal,
            quitter_vocal,
            micro,
            etat_voix
        ])
        .run(tauri::generate_context!())
        .expect("lancement de l'appli");
}
