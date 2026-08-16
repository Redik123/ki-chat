//! Transport QUIC : contrôle (flux fiable, JSON ligne à ligne) + voix
//! (datagrammes non fiables) sur UNE connexion par client, chiffrée TLS 1.3.
//!
//! - Le certificat auto-signé est généré au premier démarrage et persisté
//!   dans data/ (les clients d'un serveur privé ne vérifient pas la chaîne :
//!   le transport est chiffré, et la voix reste en plus chiffrée de bout en
//!   bout — le serveur ne peut pas l'écouter).
//! - Plus de jeton dans les paquets voix : un datagramme arrive sur la
//!   connexion authentifiée de son émetteur, le serveur vérifie simplement
//!   que l'en-tête porte le bon user_id.
//! - Le serveur mesure les pertes montantes de chaque émetteur (trous de
//!   compteurs) et les lui signale toutes les 5 s (débit adaptatif client).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use ki_protocol::{parse_voice_packet, ChatRecord, ClientMsg, ServerMsg, UserId};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

use crate::state::{now_millis, AppState, ConnectedUser};

/// Longueur maximale d'un motif de modération ou d'une étiquette
/// d'invitation. Ces chaînes viennent du client et finissent dans un fichier
/// de journal ainsi que dans l'interface des autres : elles se bornent et se
/// nettoient comme n'importe quel texte reçu.
const MAX_REASON: usize = 200;

pub async fn run(state: Arc<AppState>, port: u16) -> anyhow::Result<()> {
    let (cert, key) = load_or_create_cert(&state.data_dir)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut crypto = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("TLS 1.3")?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .context("certificat TLS")?;
    crypto.alpn_protocols = vec![b"ki-chat".to_vec()];

    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(crypto)?,
    ));
    let transport = Arc::get_mut(&mut server_config.transport)
        .expect("transport config unique");
    transport.max_idle_timeout(Some(Duration::from_secs(30).try_into()?));
    transport.keep_alive_interval(Some(Duration::from_secs(5)));
    // Anti-bufferbloat : le défaut d'1 Mio peut mettre ~2 minutes de voix en
    // file sous congestion. 32 Kio ≈ 1 s d'audio : au-delà, on jette du vieux.
    transport.datagram_send_buffer_size(32 * 1024);
    // Le défaut est ILLIMITÉ : un pair authentifié pouvait faire tamponner
    // des centaines de Mo par le serveur sans qu'il accepte un seul flux.
    transport.receive_window(quinn::VarInt::from_u32(16 * 1024 * 1024));
    // Le défaut (100) plafonnerait le relais vidéo à 100 trames en vol.
    transport.max_concurrent_uni_streams(quinn::VarInt::from_u32(256));

    let endpoint = quinn::Endpoint::server(server_config, ([0, 0, 0, 0], port).into())?;
    tune_socket();
    tracing::info!("QUIC en écoute sur le port {port}/udp (contrôle + voix)");

    while let Some(incoming) = endpoint.accept().await {
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(state, incoming).await {
                tracing::debug!("connexion terminée : {e:#}");
            }
        });
    }
    Ok(())
}

/// Le marquage DSCP EF et les buffers sont gérés par quinn/OS ; ce point
/// d'extension reste pour un réglage futur du socket de l'endpoint.
fn tune_socket() {}

/// Lit une ligne du flux de contrôle, en refusant celles qui dépassent
/// [`ki_protocol::MAX_LINE`].
///
/// `AsyncBufReadExt::lines()` ferait grandir son tampon jusqu'au prochain
/// saut de ligne, sans limite : un client qui n'en envoie jamais épuiserait
/// la mémoire du serveur avant même de s'authentifier. On borne donc chaque
/// lecture, et une ligne trop longue termine la connexion.
async fn read_line<R>(reader: &mut R) -> anyhow::Result<Option<String>>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncReadExt as _;

    let mut buf = Vec::new();
    let limit = ki_protocol::MAX_LINE as u64 + 1;
    reader.take(limit).read_until(b'\n', &mut buf).await?;

    if buf.len() > ki_protocol::MAX_LINE {
        anyhow::bail!("ligne de contrôle trop longue");
    }
    // Pas de saut de ligne : le flux s'est refermé (éventuellement au
    // milieu d'un message), il n'y a plus rien à lire.
    if buf.last() != Some(&b'\n') {
        return Ok(None);
    }
    buf.pop();
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    Ok(Some(String::from_utf8(buf)?))
}

async fn handle_connection(
    state: Arc<AppState>,
    incoming: quinn::Incoming,
) -> anyhow::Result<()> {
    let conn = incoming.await.context("poignée de main QUIC")?;
    // Le client ouvre le flux de contrôle et parle en premier (Auth).
    let (mut send, recv) = conn.accept_bi().await.context("flux de contrôle")?;
    // Le contrôle passe devant tout média : un KeyframeRequest ne doit
    // jamais attendre derrière 200 Ko de trame vidéo.
    let _ = send.set_priority(10);
    let mut lines = BufReader::new(recv);

    // --- Phase 1 : authentification ---
    let first = tokio::time::timeout(Duration::from_secs(10), read_line(&mut lines))
        .await
        .context("délai d'authentification dépassé")?
        .context("flux fermé")?
        .unwrap_or_default();
    let Ok(ClientMsg::Auth { username, password, invite }) =
        serde_json::from_str::<ClientMsg>(&first)
    else {
        send_direct(&mut send, &ServerMsg::Error {
            message: "le premier message doit être auth".into(),
        })
        .await;
        return Ok(());
    };
    // Bornes sur les champs d'authentification : ils arrivent avant tout
    // contrôle d'identité, donc de n'importe qui.
    let username = username.trim().to_string();
    if username.is_empty()
        || username.chars().count() > ki_protocol::MAX_USERNAME
        || username.chars().any(|c| c.is_control())
    {
        send_direct(&mut send, &ServerMsg::Error { message: "pseudo invalide".into() }).await;
        return Ok(());
    }
    if password.len() > ki_protocol::MAX_PASSWORD {
        send_direct(&mut send, &ServerMsg::Error { message: "mot de passe trop long".into() })
            .await;
        return Ok(());
    }
    if invite.as_ref().is_some_and(|c| c.len() > ki_protocol::MAX_INVITE) {
        send_direct(&mut send, &ServerMsg::Error {
            message: "code d'invitation invalide".into(),
        })
        .await;
        return Ok(());
    }
    // Anti-force brute. Le refus intervient **avant** le hachage : une
    // tentative bloquée ne coûte alors qu'une recherche dans une table,
    // là où un Argon2id coûte de la mémoire et du temps par essai.
    let peer = conn.remote_address().ip();
    if let Err(wait) = state.throttle.check(peer, &username) {
        send_direct(&mut send, &ServerMsg::Error {
            message: format!(
                "trop de tentatives — réessaie dans {} s",
                wait.as_secs().max(1)
            ),
        })
        .await;
        tracing::warn!("tentative bloquée : {username} depuis {peer}");
        return Ok(());
    }

    // Argon2id est volontairement lent : le lancer sur un ouvrier de la
    // boucle asynchrone bloquerait tout le trafic du serveur pendant ce
    // temps. Il part donc sur le pool bloquant.
    let auth = {
        let accounts = state.clone();
        let (user, pass, code) = (username.clone(), password.clone(), invite.clone());
        tokio::task::spawn_blocking(move || {
            accounts
                .accounts
                .authenticate(&user, &pass, code.as_deref(), &accounts.token)
        })
        .await
        .context("tâche d'authentification")?
    };
    let auth = match auth {
        Ok(a) => {
            state.throttle.record_success(peer, &username);
            a
        }
        Err(e) => {
            state.throttle.record_failure(peer, &username);
            send_direct(&mut send, &ServerMsg::Error { message: e }).await;
            return Ok(());
        }
    };
    let user_id = auth.id;
    // Un lien d'invitation permanent n'est acceptable que s'il laisse une
    // trace : on consigne ici quel code a créé quel compte, depuis quelle
    // adresse. C'est la contrepartie de la permanence.
    if let Some(code) = &auth.created_with {
        state
            .audit
            .record("invite.use", &username, "", &format!("{code} depuis {peer}"));
    }
    // Une session déjà ouverte sur ce compte cède la place à la nouvelle.
    //
    // Refuser était pire : quand l'application se ferme brutalement — ou
    // plante, ou perd le réseau — aucune fermeture propre ne part, et le
    // serveur gardait la session jusqu'à l'expiration d'inactivité de QUIC,
    // 30 secondes pendant lesquelles se reconnecter était impossible. Aucune
    // fermeture propre ne peut couvrir ces cas : c'est donc à l'ouverture
    // que la question se règle. C'est aussi ce que font Discord et Mumble.
    let previous = { state.users.lock().unwrap().contains_key(&user_id) };
    if previous {
        tracing::info!("{username} se reconnecte : la session précédente est fermée");
        state.disconnect(user_id);
    }

    // --- Phase 2 : enregistrement + tâches ---
    use rand::Rng;
    let voice_token: u64 = rand::rng().random();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMsg>();
    {
        let mut users = state.users.lock().unwrap();
        users.insert(
            user_id,
            ConnectedUser {
                username: username.clone(),
                channel: None,
                voice: None,
                speaking: false,
                admin: auth.admin,
                voice_token,
                tx: tx.clone(),
                conn: conn.clone(),
                chat_budget: Default::default(),
            },
        );
    }
    state.rebuild_voice_routes();

    let _ = tx.send(ServerMsg::Welcome {
        user_id,
        voice_token,
        udp_port: 0, // transport unifié : plus de port voix séparé
        voice_key: ki_protocol::hex_encode(&state.voice_key),
        is_admin: auth.admin,
        channels: state.channels.clone(),
        server: state.meta.get(),
    });
    tracing::info!("connexion : {username} (id {user_id})");
    // La liste du serveur vient de changer, pour tout le monde.
    state.broadcast_all(&ServerMsg::UserJoined { user_id, username: username.clone() });
    state.broadcast_all(&ServerMsg::Members { members: state.roster() });

    // Tâche d'écriture : sérialise les ServerMsg vers le flux de contrôle.
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let Ok(mut json) = serde_json::to_string(&msg) else { continue };
            json.push('\n');
            if send.write_all(json.as_bytes()).await.is_err() {
                break;
            }
        }
    });

    // Tâche voix : datagrammes entrants -> relais + suivi des pertes.
    let voice = tokio::spawn(voice_task(state.clone(), conn.clone(), user_id, tx.clone()));

    // Boucle de contrôle.
    while let Ok(Some(line)) = read_line(&mut lines).await {
        let Ok(msg) = serde_json::from_str::<ClientMsg>(&line) else {
            let _ = tx.send(ServerMsg::Error { message: "message invalide".into() });
            continue;
        };
        handle_msg(&state, user_id, &username, msg, &tx);
    }

    // --- Phase 3 : nettoyage ---
    // Sous garde du jeton : si ce compte s'est déjà reconnecté ailleurs, la
    // session en place n'est plus la nôtre et ne doit pas être touchée.
    tracing::info!("déconnexion : {username} (id {user_id})");
    state.disconnect_session(user_id, voice_token);
    voice.abort();
    writer.abort();
    Ok(())
}

/// Relais des datagrammes voix d'UN émetteur vers son salon, avec mesure
/// des pertes montantes (trous de compteurs, fenêtres de 5 s).
async fn voice_task(
    state: Arc<AppState>,
    conn: quinn::Connection,
    user_id: UserId,
    tx: mpsc::UnboundedSender<ServerMsg>,
) {
    let mut last_counter = 0u64;
    let mut expected = 0u64;
    let mut received = 0u64;
    let mut last_report = Instant::now();

    while let Ok(dat) = conn.read_datagram().await {
        let Some(pkt) = parse_voice_packet(&dat) else { continue };
        // Anti-usurpation : l'en-tête doit porter l'identité de la connexion.
        if pkt.id != user_id || pkt.payload.is_empty() {
            continue;
        }

        if pkt.counter > last_counter && last_counter != 0 {
            expected += pkt.counter - last_counter;
        } else {
            expected += 1;
        }
        received += 1;
        last_counter = pkt.counter;

        // Relais : une lecture partagée de la table précalculée.
        {
            let routes = state.voice_routes.read().unwrap();
            if let Some(channel) = routes.channel_of.get(&user_id) {
                if let Some(peers) = routes.peers.get(channel) {
                    for (peer_id, peer_conn) in peers {
                        if *peer_id != user_id {
                            // Clonage de Bytes : compteur de références, pas de copie.
                            let _ = peer_conn.send_datagram(dat.clone());
                        }
                    }
                }
            }
        }

        if last_report.elapsed() >= Duration::from_secs(5) {
            if expected >= 50 {
                let loss_pct =
                    (100.0 * (1.0 - received as f64 / expected as f64)).clamp(0.0, 100.0);
                let _ = tx.send(ServerMsg::NetQuality { loss_pct: loss_pct as f32 });
                if loss_pct >= 5.0 {
                    tracing::info!("pertes montantes de l'utilisateur {user_id} : {loss_pct:.1} %");
                }
            }
            expected = 0;
            received = 0;
            last_report = Instant::now();
        }
    }
}

async fn send_direct(send: &mut quinn::SendStream, msg: &ServerMsg) {
    if let Ok(mut json) = serde_json::to_string(msg) {
        json.push('\n');
        let _ = send.write_all(json.as_bytes()).await;
        let _ = send.finish();
    }
}

/// Certificat TLS auto-signé, généré une fois et persisté dans data/.
fn load_or_create_cert(
    data_dir: &str,
) -> anyhow::Result<(
    rustls::pki_types::CertificateDer<'static>,
    rustls::pki_types::PrivateKeyDer<'static>,
)> {
    let dir = PathBuf::from(data_dir);
    std::fs::create_dir_all(&dir)?;
    let cert_path = dir.join("quic-cert.der");
    let key_path = dir.join("quic-key.der");

    if cert_path.exists() && key_path.exists() {
        let cert = std::fs::read(&cert_path)?;
        let key = std::fs::read(&key_path)?;
        return Ok((
            rustls::pki_types::CertificateDer::from(cert),
            rustls::pki_types::PrivateKeyDer::try_from(key)
                .map_err(|e| anyhow::anyhow!("clé TLS corrompue : {e}"))?,
        ));
    }

    tracing::info!("génération du certificat TLS auto-signé (premier démarrage)");
    let certified = rcgen::generate_simple_self_signed(vec!["ki-chat".into()])?;
    let cert_der = certified.cert.der().to_vec();
    let key_der = certified.key_pair.serialize_der();
    std::fs::write(&cert_path, &cert_der)?;
    std::fs::write(&key_path, &key_der)?;
    Ok((
        rustls::pki_types::CertificateDer::from(cert_der),
        rustls::pki_types::PrivateKeyDer::try_from(key_der)
            .map_err(|e| anyhow::anyhow!("clé TLS invalide : {e}"))?,
    ))
}

// ---------------------------------------------------------------------------
// Logique métier, indépendante du transport
// ---------------------------------------------------------------------------

fn current_channel(state: &Arc<AppState>, user_id: UserId) -> Option<ki_protocol::ChannelId> {
    state.users.lock().unwrap().get(&user_id).and_then(|u| u.channel)
}

/// Vérifie que l'appelant est admin ; sinon envoie une erreur.
fn require_admin(
    state: &Arc<AppState>,
    user_id: UserId,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) -> bool {
    let is_admin = {
        let users = state.users.lock().unwrap();
        users.get(&user_id).is_some_and(|u| u.admin)
    };
    if !is_admin {
        let _ = tx.send(ServerMsg::Error { message: "réservé aux admins".into() });
    }
    is_admin
}

/// Envoie l'état admin complet (comptes avec statut en ligne + invitations).
fn send_admin_info(state: &Arc<AppState>, tx: &mpsc::UnboundedSender<ServerMsg>) {
    let mut users = state.accounts.list();
    {
        let connected = state.users.lock().unwrap();
        for u in &mut users {
            u.online = connected.contains_key(&u.user_id);
        }
    }
    let _ = tx.send(ServerMsg::AdminInfo { users, invites: state.accounts.invites() });
}
/// Résume un changement d'identité pour le journal d'audit. Le logo n'y
/// entre que par sa présence : y recopier plusieurs dizaines de kilo-octets
/// de base64 rendrait le fichier illisible.
fn describe_server_change(name: &Option<String>, icon: &ki_protocol::IconChange) -> String {
    let mut parts = Vec::new();
    if let Some(name) = name {
        parts.push(format!("nom « {} »", name.trim()));
    }
    match icon {
        ki_protocol::IconChange::Keep => {}
        ki_protocol::IconChange::Clear => parts.push("logo retiré".into()),
        ki_protocol::IconChange::Set { .. } => parts.push("logo remplacé".into()),
    }
    parts.join(", ")
}

/// Applique un changement d'identité du serveur, après validation.
fn apply_server_info(
    state: &Arc<AppState>,
    name: Option<String>,
    icon: ki_protocol::IconChange,
) -> Result<(), String> {
    if let Some(name) = name {
        let name = name.trim();
        if name.chars().count() > ki_protocol::MAX_SERVER_NAME {
            return Err(format!(
                "nom trop long ({} caractères max)",
                ki_protocol::MAX_SERVER_NAME
            ));
        }
        state.meta.set_name(name).map_err(|e| format!("{e:#}"))?;
    }
    match icon {
        ki_protocol::IconChange::Keep => {}
        ki_protocol::IconChange::Clear => {
            state.meta.set_icon(None).map_err(|e| format!("{e:#}"))?;
        }
        ki_protocol::IconChange::Set { data } => {
            // Le serveur ne décode pas l'image, mais il en contrôle
            // l'en-tête : il redistribue ce blob à tous les membres, il
            // n'a pas le droit de leur transmettre n'importe quoi.
            ki_protocol::check_thumbnail(&data)?;
            state.meta.set_icon(Some(data)).map_err(|e| format!("{e:#}"))?;
        }
    }
    Ok(())
}

fn handle_msg(
    state: &Arc<AppState>,
    user_id: UserId,
    username: &str,
    msg: ClientMsg,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    match msg {
        ClientMsg::Auth { .. } => {
            let _ = tx.send(ServerMsg::Error { message: "déjà authentifié".into() });
        }
        ClientMsg::Join { channel } => {
            // Ouvrir un salon textuel ne concerne que celui qui le lit :
            // aucune présence à annoncer, personne n'a « rejoint » quoi que
            // ce soit du point de vue des autres.
            if !state.channel_is(channel, ki_protocol::ChannelKind::Text) {
                let _ = tx.send(ServerMsg::Error { message: "salon textuel inconnu".into() });
                return;
            }
            let mut users = state.users.lock().unwrap();
            if let Some(u) = users.get_mut(&user_id) {
                u.channel = Some(channel);
            }
        }
        ClientMsg::Leave => {
            let mut users = state.users.lock().unwrap();
            if let Some(u) = users.get_mut(&user_id) {
                u.channel = None;
            }
        }
        ClientMsg::JoinVoice { channel } => {
            if !state.channel_is(channel, ki_protocol::ChannelKind::Voice) {
                let _ = tx.send(ServerMsg::Error { message: "salon vocal inconnu".into() });
                return;
            }
            {
                let mut users = state.users.lock().unwrap();
                let Some(u) = users.get_mut(&user_id) else { return };
                if u.voice == Some(channel) {
                    return;
                }
                u.voice = Some(channel);
                u.speaking = false;
            }
            state.rebuild_voice_routes();
            // La présence vocale intéresse tout le serveur : chacun voit
            // qui est où dans la liste de droite.
            state.broadcast_all(&ServerMsg::Members { members: state.roster() });
        }
        ClientMsg::LeaveVoice => {
            let was_in_voice = {
                let mut users = state.users.lock().unwrap();
                match users.get_mut(&user_id) {
                    Some(u) => {
                        u.speaking = false;
                        u.voice.take().is_some()
                    }
                    None => false,
                }
            };
            if was_in_voice {
                state.rebuild_voice_routes();
                state.broadcast_all(&ServerMsg::Members { members: state.roster() });
            }
        }
        ClientMsg::Chat { text } => {
            let Some(channel) = current_channel(state, user_id) else {
                let _ = tx.send(ServerMsg::Error { message: "rejoins un salon d'abord".into() });
                return;
            };
            // Anti-spam : sans quoi un client modifié remplit l'historique
            // et la bande passante de tout le monde aussi vite qu'il veut.
            let allowed = {
                let mut users = state.users.lock().unwrap();
                users.get_mut(&user_id).is_some_and(|u| u.chat_budget.take())
            };
            if !allowed {
                let _ = tx.send(ServerMsg::Error { message: "tu écris trop vite".into() });
                return;
            }
            // Le texte est relayé à tout le salon **et** gardé en mémoire
            // (1000 messages par salon) : sans borne, un seul message
            // suffirait à faire tomber le serveur.
            let text = match ki_protocol::clean_chat(&text) {
                Ok(text) => text,
                Err(e) => {
                    let _ = tx.send(ServerMsg::Error { message: e });
                    return;
                }
            };
            let ts = now_millis();
            let rec = ChatRecord {
                user_id,
                username: username.to_string(),
                text: text.clone(),
                ts,
            };
            state.history.append(channel, &rec);
            state.broadcast(
                channel,
                None,
                &ServerMsg::Chat { user_id, username: username.to_string(), text, ts },
            );
        }
        ClientMsg::History { limit } => {
            let Some(channel) = current_channel(state, user_id) else {
                let _ = tx.send(ServerMsg::Error { message: "rejoins un salon d'abord".into() });
                return;
            };
            let messages = state.history.recent(channel, limit.min(1000) as usize);
            let _ = tx.send(ServerMsg::History { messages });
        }
        ClientMsg::HistoryBefore { before_ts, limit } => {
            let Some(channel) = current_channel(state, user_id) else {
                let _ = tx.send(ServerMsg::Error { message: "rejoins un salon d'abord".into() });
                return;
            };
            // Remonter le fil peut demander de relire tout le fichier du
            // salon : hors de la boucle asynchrone, comme les sauvegardes.
            let (state, tx) = (state.clone(), tx.clone());
            let limit = limit.clamp(1, 200) as usize;
            tokio::task::spawn_blocking(move || {
                let (messages, more) =
                    state.history.before(&state.data_dir, channel, before_ts, limit);
                let _ = tx.send(ServerMsg::HistoryPage { messages, more });
            });
        }
        ClientMsg::VoiceState { speaking } => {
            let changed = {
                let mut users = state.users.lock().unwrap();
                let Some(u) = users.get_mut(&user_id) else { return };
                // On ne « parle » que depuis un salon vocal. Et on ne relaie
                // qu'un vrai changement : notre client ne transmet déjà que
                // les transitions, mais rien n'oblige l'autre bout à être lui.
                if u.voice.is_none() || u.speaking == speaking {
                    false
                } else {
                    u.speaking = speaking;
                    true
                }
            };
            // À diffuser à tout le serveur, pas au seul salon vocal : la barre
            // latérale liste les occupants de **tous** les salons vocaux et
            // allume leur anneau. Diffuser au salon aurait de toute façon été
            // sans effet, `broadcast` filtrant sur le salon **textuel** lu.
            if changed {
                state.broadcast_all_except(user_id, &ServerMsg::VoiceState { user_id, speaking });
            }
        }
        ClientMsg::Kick { user_id: target, reason } => {
            if !require_admin(state, user_id, tx) {
                return;
            }
            if target == user_id {
                let _ = tx.send(ServerMsg::Error { message: "impossible de s'expulser soi-même".into() });
                return;
            }
            let reason = ki_protocol::safe_display(&reason, MAX_REASON);
            let target_tx = {
                let users = state.users.lock().unwrap();
                users.get(&target).map(|u| (u.username.clone(), u.tx.clone()))
            };
            match target_tx {
                Some((target_name, t)) => {
                    tracing::info!("expulsion de l'utilisateur {target} par {username}");
                    state.audit.record("member.kick", username, &target_name, &reason);
                    let _ = t.send(ServerMsg::Kicked { reason });
                    state.disconnect(target);
                }
                None => {
                    let _ = tx.send(ServerMsg::Error { message: "utilisateur introuvable".into() });
                }
            }
        }
        ClientMsg::AdminListUsers => {
            if require_admin(state, user_id, tx) {
                send_admin_info(state, tx);
            }
        }
        ClientMsg::AdminAuditLog { limit } => {
            if require_admin(state, user_id, tx) {
                let limit = limit.clamp(1, 500) as usize;
                let _ = tx.send(ServerMsg::AuditLog { records: state.audit.recent(limit) });
            }
        }
        ClientMsg::AdminCreateInvite { uses, label, ttl_secs } => {
            if require_admin(state, user_id, tx) {
                let label = ki_protocol::safe_display(&label, MAX_REASON);
                let (state, tx) = (state.clone(), tx.clone());
                let actor = username.to_string();
                tokio::task::spawn_blocking(move || {
                    let code = state.accounts.create_invite(&actor, uses, &label, ttl_secs);
                    tracing::info!("invitation {code} créée par {actor}");
                    state.audit.record(
                        "invite.create",
                        &actor,
                        "",
                        &format!(
                            "{code} — {} usage(s){}",
                            uses.map(|n| n.to_string()).unwrap_or_else(|| "∞".into()),
                            if label.is_empty() {
                                String::new()
                            } else {
                                format!(" « {label} »")
                            },
                        ),
                    );
                    let _ = tx.send(ServerMsg::InviteCreated { code });
                    send_admin_info(&state, &tx);
                });
            }
        }
        ClientMsg::AdminRevokeInvite { code } => {
            if require_admin(state, user_id, tx) {
                let (state, tx) = (state.clone(), tx.clone());
                let actor = username.to_string();
                tokio::task::spawn_blocking(move || {
                    match state.accounts.revoke_invite(&code) {
                        Ok(()) => {
                            state.audit.record("invite.revoke", &actor, "", &code);
                            let _ = tx.send(ServerMsg::Info {
                                message: format!("invitation {code} révoquée"),
                            });
                            send_admin_info(&state, &tx);
                        }
                        Err(e) => {
                            let _ = tx.send(ServerMsg::Error { message: e });
                        }
                    }
                });
            }
        }
        ClientMsg::AdminResetPassword { username: target, new_password } => {
            if require_admin(state, user_id, tx) {
                // Un hachage Argon2id, volontairement lent, suivi de la
                // réécriture de `users.json` : hors de la boucle asynchrone,
                // sans quoi la voix de tout le monde hoquette pendant ce
                // temps. Même idiome que `ChangePassword` ci-dessous.
                let (state, tx) = (state.clone(), tx.clone());
                let actor = username.to_string();
                tokio::task::spawn_blocking(move || {
                    match state.accounts.reset_password(&actor, &target, &new_password) {
                        Ok(()) => {
                            state.audit.record("member.password_reset", &actor, &target, "");
                            let _ = tx.send(ServerMsg::Info {
                                message: format!("mot de passe de {target} réinitialisé"),
                            });
                            send_admin_info(&state, &tx);
                        }
                        Err(e) => {
                            let _ = tx.send(ServerMsg::Error { message: e });
                        }
                    }
                });
            }
        }
        // Conservé pour les clients antérieurs à `AdminBan` : un blocage sans
        // motif ni durée, c'est-à-dire un bannissement définitif.
        ClientMsg::AdminSetBanned { username: target, banned } => {
            if banned {
                let msg = ClientMsg::AdminBan { username: target, reason: String::new(), duration_secs: 0 };
                handle_msg(state, user_id, username, msg, tx);
            } else {
                handle_msg(state, user_id, username, ClientMsg::AdminUnban { username: target }, tx);
            }
        }
        ClientMsg::AdminBan { username: target, reason, duration_secs } => {
            if require_admin(state, user_id, tx) {
                let reason = ki_protocol::safe_display(&reason, MAX_REASON);
                // `users.json` porte les photos de profil en base64 : sa
                // réécriture pèse plusieurs mégaoctets sur un serveur bien
                // rempli. Hors de la boucle asynchrone, donc.
                let (state, tx) = (state.clone(), tx.clone());
                let actor = username.to_string();
                tokio::task::spawn_blocking(move || {
                    match state.accounts.ban(&actor, &target, &reason, duration_secs) {
                        Ok(()) => {
                            state.audit.record(
                                "member.ban",
                                &actor,
                                &target,
                                &format!(
                                    "{}{}",
                                    if duration_secs == 0 {
                                        "définitif".to_string()
                                    } else {
                                        format!("{} min", duration_secs / 60)
                                    },
                                    if reason.is_empty() {
                                        String::new()
                                    } else {
                                        format!(" — {reason}")
                                    },
                                ),
                            );
                            // Un compte banni alors qu'il est en ligne s'en
                            // va tout de suite : sans ça, il reste jusqu'à ce
                            // qu'il se déconnecte de lui-même.
                            let online = {
                                let users = state.users.lock().unwrap();
                                users
                                    .iter()
                                    .find(|(_, u)| u.username == target)
                                    .map(|(id, u)| (*id, u.tx.clone()))
                            };
                            if let Some((target_id, target_tx)) = online {
                                let _ =
                                    target_tx.send(ServerMsg::Kicked { reason: reason.clone() });
                                state.disconnect(target_id);
                            }
                            let _ = tx.send(ServerMsg::Info { message: format!("{target} banni") });
                            send_admin_info(&state, &tx);
                        }
                        Err(e) => {
                            let _ = tx.send(ServerMsg::Error { message: e });
                        }
                    }
                });
            }
        }
        ClientMsg::AdminUnban { username: target } => {
            if require_admin(state, user_id, tx) {
                let (state, tx) = (state.clone(), tx.clone());
                let actor = username.to_string();
                tokio::task::spawn_blocking(move || {
                    match state.accounts.unban(&actor, &target) {
                        Ok(()) => {
                            state.audit.record("member.unban", &actor, &target, "");
                            let _ =
                                tx.send(ServerMsg::Info { message: format!("{target} débanni") });
                            send_admin_info(&state, &tx);
                        }
                        Err(e) => {
                            let _ = tx.send(ServerMsg::Error { message: e });
                        }
                    }
                });
            }
        }
        ClientMsg::AdminSetServerInfo { name, icon } => {
            if require_admin(state, user_id, tx) {
                let changed = describe_server_change(&name, &icon);
                // Le logo est un PNG en base64 : `server.json` pèse jusqu'à
                // une centaine de kilo-octets, réécrits en entier. Hors de la
                // boucle asynchrone, comme les autres écritures d'état.
                let (state, tx) = (state.clone(), tx.clone());
                let actor = username.to_string();
                tokio::task::spawn_blocking(move || {
                    match apply_server_info(&state, name, icon) {
                        Ok(()) => {
                            state.audit.record("server.info", &actor, "", &changed);
                            // L'identité est publique : tout le monde la reçoit.
                            state
                                .broadcast_all(&ServerMsg::ServerInfo { server: state.meta.get() });
                            let _ = tx.send(ServerMsg::Info {
                                message: "identité du serveur mise à jour".into(),
                            });
                        }
                        Err(e) => {
                            let _ = tx.send(ServerMsg::Error { message: e });
                        }
                    }
                });
            }
        }
        ClientMsg::SetAvatar { avatar } => {
            // Décodage base64, contrôle de l'en-tête PNG et réécriture de
            // `users.json` : trop lourd pour la boucle asynchrone.
            let (state, tx) = (state.clone(), tx.clone());
            let actor = username.to_string();
            tokio::task::spawn_blocking(move || {
                let outcome = match avatar {
                    ki_protocol::IconChange::Keep => Ok(()),
                    ki_protocol::IconChange::Clear => state.accounts.set_avatar(&actor, None),
                    ki_protocol::IconChange::Set { data } => {
                        // Même contrôle que pour le logo : la photo part
                        // ensuite chez tout le monde.
                        match ki_protocol::check_thumbnail(&data) {
                            Ok(()) => state.accounts.set_avatar(&actor, Some(data)),
                            Err(e) => Err(e),
                        }
                    }
                };
                match outcome {
                    Ok(()) => {
                        // La nouvelle photo part à tout le monde : les autres
                        // n'ont pas à la redemander.
                        let data = state.accounts.avatar_of(user_id);
                        let hash = ki_protocol::avatar_hash(data.as_deref()).unwrap_or_default();
                        state.broadcast_all(&ServerMsg::Avatar { user_id, hash, data });
                        let _ = tx.send(ServerMsg::Info { message: "photo mise à jour".into() });
                    }
                    Err(e) => {
                        let _ = tx.send(ServerMsg::Error { message: e });
                    }
                }
            });
        }
        ClientMsg::RequestAvatars { user_ids } => {
            // Bornée à la taille d'un salon : pas de moisson du carnet.
            for target in user_ids.into_iter().take(64) {
                let data = state.accounts.avatar_of(target);
                let Some(hash) = ki_protocol::avatar_hash(data.as_deref()) else { continue };
                let _ = tx.send(ServerMsg::Avatar { user_id: target, hash, data });
            }
        }
        ClientMsg::ChangePassword { old_password, new_password } => {
            // Deux hachages Argon2id : hors de la boucle asynchrone.
            let (state, tx) = (state.clone(), tx.clone());
            let username = username.to_string();
            tokio::task::spawn_blocking(move || {
                let outcome =
                    state.accounts.change_password(&username, &old_password, &new_password);
                let _ = tx.send(match outcome {
                    Ok(()) => ServerMsg::Info { message: "mot de passe changé".into() },
                    Err(e) => ServerMsg::Error { message: e },
                });
            });
        }
        ClientMsg::Ping => {
            let _ = tx.send(ServerMsg::Pong);
        }
    }
}
