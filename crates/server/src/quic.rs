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

/// Le certificat est **reçu** et non chargé ici : il est aussi celui du
/// partage de fichiers, et le charger de chaque côté en produisait deux
/// différents au premier démarrage — les deux appels constatant l'absence
/// avant que l'autre n'ait écrit. Le serveur présentait alors deux identités,
/// puis refusait de redémarrer sur la paire incohérente restée sur le disque.
pub async fn run(
    state: Arc<AppState>,
    port: u16,
    cert: rustls::pki_types::CertificateDer<'static>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
) -> anyhow::Result<()> {
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
    let (raw_tx, mut rx) = mpsc::channel::<ServerMsg>(crate::state::OUTBOX_CAP);
    let tx = crate::state::Outbox::new(raw_tx, conn.clone());
    // Les permissions se déduisent des rôles une fois pour toutes ici, et
    // sont rafraîchies à chaque changement : le chemin chaud n'a alors plus
    // à reprendre le magasin de rôles.
    let perms = state.roles.perms_of(&auth.roles);
    let rank = state.roles.rank_of(&auth.roles);
    let color = state.roles.color_of(&auth.roles);
    let is_admin = ki_protocol::perm::has(perms, ki_protocol::perm::ADMINISTRATOR);
    {
        let mut users = state.users.lock().unwrap();
        users.insert(
            user_id,
            ConnectedUser {
                username: username.clone(),
                channel: None,
                voice: None,
                speaking: false,
                roles: auth.roles.clone(),
                perms,
                rank,
                color,
                admin: is_admin,
                voice_token,
                tx: tx.clone(),
                conn: conn.clone(),
                chat_budget: Default::default(),
                voice_budget: crate::state::TokenBucket::new(3.0, 8.0),
            },
        );
    }
    state.rebuild_voice_routes();

    let _ = tx.send(ServerMsg::Welcome {
        user_id,
        voice_token,
        udp_port: 0, // transport unifié : plus de port voix séparé
        voice_key: ki_protocol::hex_encode(&state.voice_key),
        is_admin,
        perms,
        rank,
        roles: state.roles.list(),
        // Filtrée : un salon restreint ne doit pas seulement être
        // inaccessible, il ne doit pas même se deviner.
        channels: state.visible_channels(user_id),
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
            // Garde-fou : ne jamais émettre une ligne que le client refusera de
            // lire. Au-delà de MAX_LINE il ferme la connexion — une réponse
            // trop grosse déconnecterait donc son destinataire. Les réponses
            // d'historique sont déjà bornées à la source ; ceci couvre tout le
            // reste (journal d'audit, état admin) plutôt que de risquer une
            // déconnexion silencieuse.
            // `>=` et non `>` : le saut de ligne ajouté juste après compte
            // pour le lecteur d'en face, qui mesure le tampon **avant** de le
            // retirer. Un JSON de très exactement MAX_LINE partirait donc à
            // MAX_LINE + 1 octets et ferait tomber la connexion.
            if json.len() >= ki_protocol::MAX_LINE {
                tracing::error!(
                    "message de contrôle trop long ({} octets), ignoré",
                    json.len()
                );
                continue;
            }
            json.push('\n');
            if send.write_all(json.as_bytes()).await.is_err() {
                break;
            }
        }
    });

    // Tâche voix : datagrammes entrants -> relais + suivi des pertes.
    let voice = tokio::spawn(voice_task(state.clone(), conn.clone(), user_id, tx.clone()));

    // Boucle de contrôle.
    //
    // Le débit de **tout** le flux est borné, et pas seulement celui du chat.
    // Sans ça, un seul compte authentifié saturait le serveur avec des messages
    // minuscules mais coûteux — une requête d'historique relit le fichier du
    // salon sur le pool bloquant, un `request_avatars` engendre des
    // mégaoctets, une alternance `join_voice`/`leave_voice` un roster complet
    // pour tout le monde.
    //
    // Le dépassement **ne ferme pas la session** : la protection de la mémoire,
    // c'est la file d'envoi bornée, qui ne se déclenche que sur un client qui
    // ne lit réellement plus. Ce budget-ci ne protège que du coût de
    // traitement, et fermer sur ce critère atteindrait des clients légitimes —
    // remonter un fil enchaîne les requêtes au rythme des réponses, ce qui sur
    // un réseau local va vite. On refuse donc la requête, et la session
    // continue.
    let mut budget = crate::state::TokenBucket::new(100.0, 200.0);
    while let Ok(Some(line)) = read_line(&mut lines).await {
        let Ok(msg) = serde_json::from_str::<ClientMsg>(&line) else {
            let _ = tx.send(ServerMsg::Error { message: "message invalide".into() });
            continue;
        };
        if !budget.take() {
            tracing::debug!("{username} dépasse le débit du flux de contrôle : requête ignorée");
            let _ = tx.send(ServerMsg::Error {
                message: "trop de requêtes — ralentis un peu".into(),
            });
            continue;
        }
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
    tx: crate::state::Outbox,
) {
    let mut last_counter = 0u64;
    let mut expected = 0u64;
    let mut received = 0u64;
    let mut last_report = Instant::now();

    // Le relais amplifie : un paquet reçu repart vers chaque autre occupant du
    // salon. Sans borne, un seul émetteur saturait la liaison montante du
    // serveur et rendait la voix inaudible pour tout le monde. Un client normal
    // émet 50 paquets de 20 ms par seconde ; le double laisse toute la marge
    // utile, y compris à une rafale de rattrapage.
    let mut relay_budget = crate::state::TokenBucket::new(100.0, 200.0);

    while let Ok(dat) = conn.read_datagram().await {
        // `VOICE_MAX_PACKET` était déclaré par le protocole mais vérifié nulle
        // part côté serveur : rien n'empêchait de faire relayer des
        // datagrammes bien plus gros que ce que la voix produit.
        if dat.len() > ki_protocol::VOICE_MAX_PACKET {
            continue;
        }
        let Some(pkt) = parse_voice_packet(&dat) else { continue };
        // Anti-usurpation : l'en-tête doit porter l'identité de la connexion.
        if pkt.id != user_id || pkt.payload.is_empty() {
            continue;
        }
        if !relay_budget.take() {
            continue;
        }

        // Un écart démesuré n'est pas une rafale de pertes : c'est que
        // l'émetteur a recommencé à compter. Son compteur sert de nonce, il
        // repart donc d'un tirage aléatoire à chaque nouveau moteur — un
        // changement de micro en pleine conversation suffit. Compter cet écart
        // comme des pertes annonçait 100 % au client, qui effondrait son débit
        // pour rien. Au-delà du plausible, on se resynchronise sans rien
        // conclure. Dix secondes de trames : bien au-delà d'une vraie coupure,
        // bien en deçà d'un redémarrage de compteur.
        const MAX_PLAUSIBLE_GAP: u64 = 500;
        match pkt.counter.checked_sub(last_counter) {
            Some(gap) if last_counter != 0 && gap <= MAX_PLAUSIBLE_GAP => expected += gap,
            _ => expected += 1,
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
pub fn load_or_create_cert(
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
    let channel = state.users.lock().unwrap().get(&user_id).and_then(|u| u.channel)?;
    // Revérifié à chaque usage, et pas seulement au `Join` : ce champ a été
    // posé par un `Join` autrefois valide, qu'un changement de rôle ou une
    // restriction posée depuis a pu périmer. C'est ce qui protège d'un coup
    // l'écriture, la lecture de l'historique et la pagination.
    state.can_view(user_id, channel).then_some(channel)
}

/// Vérifie que l'appelant est admin ; sinon envoie une erreur.
fn require_admin(
    state: &Arc<AppState>,
    user_id: UserId,
    tx: &crate::state::Outbox,
) -> bool {
    require(state, user_id, tx, ki_protocol::perm::ADMINISTRATOR)
}

/// Vérifie une permission, et prévient l'appelant en cas de refus.
fn require(
    state: &Arc<AppState>,
    user_id: UserId,
    tx: &crate::state::Outbox,
    need: ki_protocol::Perms,
) -> bool {
    let ok = {
        let users = state.users.lock().unwrap();
        users.get(&user_id).is_some_and(|u| ki_protocol::perm::has(u.perms, need))
    };
    if !ok {
        let _ = tx.send(ServerMsg::Error { message: "tu n'as pas cette permission".into() });
    }
    ok
}

/// Consomme un jeton du budget d'entrées et sorties de vocal.
fn take_voice_budget(state: &Arc<AppState>, user_id: UserId) -> bool {
    let mut users = state.users.lock().unwrap();
    users.get_mut(&user_id).is_some_and(|u| u.voice_budget.take())
}

/// Rang d'un connecté, et rang associé à un compte (même hors ligne).
fn rank_of(state: &Arc<AppState>, user_id: UserId) -> u16 {
    let users = state.users.lock().unwrap();
    users.get(&user_id).map(|u| u.rank).unwrap_or(0)
}

fn rank_of_account(state: &Arc<AppState>, username: &str) -> u16 {
    state.roles.rank_of(&state.accounts.roles_of(username))
}

/// La hiérarchie : on n'agit que sur strictement plus bas que soi.
///
/// `ADMINISTRATOR` ne la contourne **jamais** — c'est précisément ce qui
/// empêche un second administrateur de bannir le propriétaire.
fn outranks_account(
    state: &Arc<AppState>,
    actor: UserId,
    target: &str,
    tx: &crate::state::Outbox,
) -> bool {
    let ok = rank_of_account(state, target) < rank_of(state, actor);
    if !ok {
        let _ = tx.send(ServerMsg::Error {
            message: "cette personne est d'un rang égal ou supérieur au tien".into(),
        });
    }
    ok
}

/// Envoie l'état admin complet (comptes avec statut en ligne + invitations).
fn send_admin_info(state: &Arc<AppState>, tx: &crate::state::Outbox) {
    let mut users = state.accounts.list(&state.roles);
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
    tx: &crate::state::Outbox,
) {
    match msg {
        ClientMsg::Auth { .. } => {
            let _ = tx.send(ServerMsg::Error { message: "déjà authentifié".into() });
        }
        ClientMsg::Join { channel } => {
            // Ouvrir un salon textuel ne concerne que celui qui le lit :
            // aucune présence à annoncer, personne n'a « rejoint » quoi que
            // ce soit du point de vue des autres.
            // Un salon restreint répond **exactement** comme un salon qui
            // n'existe pas : un message différent confirmerait son
            // existence, ce qui est déjà une fuite.
            if !state.channel_is(channel, ki_protocol::ChannelKind::Text)
                || !state.can_view(user_id, channel)
            {
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
        ClientMsg::JoinVoice { channel, password } => {
            if !require(state, user_id, tx, ki_protocol::perm::CONNECT_VOICE) {
                return;
            }
            // Un salon qu'on ne voit pas doit répondre comme un salon qui
            // n'existe pas : le message d'erreur ne doit rien confirmer.
            if !state.channel_is(channel, ki_protocol::ChannelKind::Voice)
                || !state.can_view(user_id, channel)
            {
                let _ = tx.send(ServerMsg::Error { message: "salon vocal inconnu".into() });
                return;
            }
            if let Err(wrong) = state.check_voice_lock(user_id, channel, password.as_deref()) {
                let _ = tx.send(ServerMsg::VoiceLocked { channel, wrong });
                return;
            }
            // Déjà dans ce salon : rien à faire, et surtout aucun jeton à
            // brûler. Le client s'est peut-être désynchronisé — on le recale
            // avec la liste des membres plutôt que de le laisser attendre une
            // confirmation qui ne viendrait jamais.
            let already = {
                let users = state.users.lock().unwrap();
                users.get(&user_id).and_then(|u| u.voice) == Some(channel)
            };
            if already {
                let _ = tx.send(ServerMsg::Members { members: state.roster() });
                return;
            }
            if !take_voice_budget(state, user_id) {
                let _ = tx.send(ServerMsg::Error {
                    message: "tu changes de salon vocal trop vite".into(),
                });
                return;
            }
            {
                let mut users = state.users.lock().unwrap();
                let Some(u) = users.get_mut(&user_id) else { return };
                u.voice = Some(channel);
                u.speaking = false;
            }
            state.rebuild_voice_routes();
            // La présence vocale intéresse tout le serveur : chacun voit
            // qui est où dans la liste de droite.
            state.broadcast_all(&ServerMsg::Members { members: state.roster() });
        }
        ClientMsg::LeaveVoice => {
            // **Jamais** limité. Refuser une sortie laissait la personne dans
            // le salon côté serveur alors que son interface la montrait
            // dehors : elle continuait d'être entendue sans le savoir. Et une
            // rafale de sorties ne coûte rien — après la première, il n'y a
            // plus de salon à quitter, donc plus de rediffusion.
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
            if !require(state, user_id, tx, ki_protocol::perm::SEND_MESSAGE) {
                return;
            }
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
        ClientMsg::HistoryBefore { before_ts, limit, .. } => {
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
                // Le salon voyage avec la page : la réponse sort d'ici hors de
                // l'ordre du flux, et peut donc arriver après que le
                // destinataire a changé de salon.
                let _ = tx.send(ServerMsg::HistoryPage { messages, more, channel });
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
            if !require(state, user_id, tx, ki_protocol::perm::KICK) {
                return;
            }
            // Le rang, en plus de la permission : c'est lui qui empêche
            // d'atteindre quelqu'un d'aussi haut placé que soi.
            if rank_of(state, target) >= rank_of(state, user_id) {
                let _ = tx.send(ServerMsg::Error {
                    message: "cette personne est d'un rang égal ou supérieur au tien".into(),
                });
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
                    // Le motif ne se réémet pas : s'il n'a pas pu être déposé,
                    // l'intéressé ne verra qu'une coupure sans explication, et
                    // il faut au moins que le journal le dise.
                    if t.send(ServerMsg::Kicked { reason }).is_err() {
                        tracing::warn!(
                            "motif d'expulsion non remis à {target_name} : sa session ne répondait plus"
                        );
                    }
                    state.disconnect(target);
                }
                None => {
                    let _ = tx.send(ServerMsg::Error { message: "utilisateur introuvable".into() });
                }
            }
        }
        ClientMsg::AdminListUsers => {
            if require(state, user_id, tx, ki_protocol::perm::KICK) {
                send_admin_info(state, tx);
            }
        }
        ClientMsg::AdminAuditLog { limit } => {
            if require(state, user_id, tx, ki_protocol::perm::VIEW_AUDIT_LOG) {
                let limit = limit.clamp(1, 500) as usize;
                let _ = tx.send(ServerMsg::AuditLog { records: state.audit.recent(limit) });
            }
        }
        ClientMsg::AdminCreateInvite { uses, label, ttl_secs } => {
            if require(state, user_id, tx, ki_protocol::perm::CREATE_INVITE) {
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
            if require(state, user_id, tx, ki_protocol::perm::MANAGE_INVITES) {
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
            if require(state, user_id, tx, ki_protocol::perm::RESET_PASSWORD)
                && (target == username || outranks_account(state, user_id, &target, tx))
            {
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
            if require(state, user_id, tx, ki_protocol::perm::BAN)
                && outranks_account(state, user_id, &target, tx)
            {
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
            // Le rang, comme pour bannir : sans lui, un modérateur pouvait
            // défaire le bannissement prononcé par le propriétaire et
            // réintroduire n'importe qui. Défaire une sanction demande la même
            // autorité que la poser.
            if require(state, user_id, tx, ki_protocol::perm::BAN)
                && outranks_account(state, user_id, &target, tx)
            {
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
            if require(state, user_id, tx, ki_protocol::perm::MANAGE_SERVER) {
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
            // Bornée à la taille d'un salon : pas de moisson du carnet. Et
            // **dédoublonnée** : la même identité répétée 64 fois renvoyait
            // 64 copies d'une vignette pouvant peser 96 Kio, soit plusieurs
            // mégaoctets engendrés par une requête de deux cents octets.
            let mut seen = std::collections::HashSet::new();
            let unique: Vec<UserId> =
                user_ids.into_iter().filter(|id| seen.insert(*id)).collect();
            for target in unique.into_iter().take(64) {
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
        ClientMsg::AdminListRoles => {
            let _ = tx.send(ServerMsg::Roles { roles: state.roles.list() });
        }
        ClientMsg::AdminCreateRole { name, color, rank, perms } => {
            if !require(state, user_id, tx, ki_protocol::perm::MANAGE_ROLES) {
                return;
            }
            // Deux gardes contre l'escalade en une étape : on ne crée pas un
            // rôle à son propre rang ou au-dessus (on ne pourrait plus y
            // toucher, et il pourrait agir sur nous), et l'on n'y met pas
            // une permission qu'on n'a pas soi-même — sans quoi « gérer les
            // rôles » suffirait à devenir administrateur.
            if rank >= rank_of(state, user_id) {
                let _ = tx.send(ServerMsg::Error {
                    message: "un rôle ne peut pas atteindre ton propre rang".into(),
                });
                return;
            }
            let Some(perms) = grantable(state, user_id, perms, tx) else { return };
            match state.roles.create(&name, color, rank, perms) {
                Ok(role) => {
                    state.audit.record("role.create", username, &role.name, "");
                    state.broadcast_all(&ServerMsg::Roles { roles: state.roles.list() });
                }
                Err(e) => {
                    let _ = tx.send(ServerMsg::Error { message: e });
                }
            }
        }
        ClientMsg::AdminEditRole { role } => {
            if !require(state, user_id, tx, ki_protocol::perm::MANAGE_ROLES) {
                return;
            }
            // Régler ce que reçoit **tout le monde** demande l'administration
            // complète. Avec « gérer les rôles » seul, on pouvait retirer à
            // `@everyone` les droits de base et se priver du même coup de
            // celui de les rendre : `grantable` compare aux permissions
            // effectives, qui incluent justement celles d'`@everyone`.
            if role.id == ki_protocol::ROLE_EVERYONE
                && !require(state, user_id, tx, ki_protocol::perm::ADMINISTRATOR)
            {
                return;
            }
            let mine = rank_of(state, user_id);
            let existing = state.roles.get(role.id);
            let current = existing.as_ref().map(|r| r.rank).unwrap_or(0);
            // Le rang **actuel** compte autant que le demandé : sinon on
            // baisserait d'abord un rôle trop haut pour ensuite le reprendre.
            if current >= mine || role.rank >= mine {
                let _ = tx.send(ServerMsg::Error {
                    message: "ce rôle est à ton rang ou au-dessus".into(),
                });
                return;
            }
            // Seules les permissions **ajoutées** doivent être à sa portée.
            // Exiger que tout le masque le soit rendait immodifiable — jusqu'au
            // simple renommage — tout rôle portant un droit qu'on n'a pas
            // soi-même : l'éditeur masque justement ces cases-là et renvoie le
            // masque entier, si bien qu'il n'y avait rien à décocher pour s'en
            // sortir. En garder un ne l'accorde à personne ; en retirer un
            // reste permis en toutes circonstances.
            // Les bits inconnus sont écartés avant la comparaison : le magasin
            // les jetterait de toute façon, et les compter ici ferait refuser
            // l'édition d'un client d'une version future avec un message qui
            // parle de permissions, ce qui n'aiderait personne.
            let held = existing.as_ref().map(|r| r.perms).unwrap_or(0);
            let wanted = role.perms & crate::roles::known_perms();
            if grantable(state, user_id, wanted & !held, tx).is_none() {
                return;
            }
            let name = role.name.clone();
            match state.roles.edit(role) {
                Ok(()) => {
                    state.audit.record("role.edit", username, &name, "");
                    state.broadcast_all(&ServerMsg::Roles { roles: state.roles.list() });
                    refresh_everyone(state);
                    state.reconcile_memberships();
                    // Les rangs affichés dans le panneau viennent des comptes :
                    // sans ce rafraîchissement, ils restent ceux d'avant, et
                    // les boutons se décident sur une mesure périmée.
                    send_admin_info(state, tx);
                }
                Err(e) => {
                    let _ = tx.send(ServerMsg::Error { message: e });
                }
            }
        }
        ClientMsg::AdminDeleteRole { id } => {
            if !require(state, user_id, tx, ki_protocol::perm::MANAGE_ROLES) {
                return;
            }
            let current = state.roles.get(id).map(|r| r.rank).unwrap_or(0);
            if current >= rank_of(state, user_id) {
                let _ = tx.send(ServerMsg::Error {
                    message: "ce rôle est à ton rang ou au-dessus".into(),
                });
                return;
            }
            match state.roles.delete(id) {
                Ok(role) => {
                    // Le rôle disparaît de partout : des comptes, et des
                    // restrictions de salon. Un identifiant mort qui traîne
                    // dans un `allowed_roles` rendrait le salon inaccessible
                    // sans qu'on comprenne pourquoi.
                    state.accounts.remove_role(id);
                    state.channels.forget_role(id);
                    state.audit.record("role.delete", username, &role.name, "");
                    state.broadcast_all(&ServerMsg::Roles { roles: state.roles.list() });
                    refresh_everyone(state);
                    state.reconcile_memberships();
                    send_admin_info(state, tx);
                }
                Err(e) => {
                    let _ = tx.send(ServerMsg::Error { message: e });
                }
            }
        }
        ClientMsg::AdminSetUserRoles { username: target, roles } => {
            // Jamais sur soi-même. Le rang ne suffit pas à l'interdire : entre
            // l'enregistrement d'une rétrogradation et le rafraîchissement des
            // sessions, on se compare à une version périmée de soi et l'on
            // passe. C'est la même règle explicite que pour l'expulsion.
            if target == username {
                let _ = tx.send(ServerMsg::Error {
                    message: "impossible de changer ses propres rôles".into(),
                });
                return;
            }
            if !require(state, user_id, tx, ki_protocol::perm::MANAGE_ROLES)
                || !outranks_account(state, user_id, &target, tx)
            {
                return;
            }
            // On n'attribue que des rôles strictement sous son propre rang.
            let mine = rank_of(state, user_id);
            let wanted = state.roles.sanitize(&roles);
            if wanted.iter().any(|id| state.roles.get(*id).is_some_and(|r| r.rank >= mine)) {
                let _ = tx.send(ServerMsg::Error {
                    message: "tu ne peux attribuer qu'un rôle sous ton rang".into(),
                });
                return;
            }
            // Et pas davantage de permissions que les siennes. Le rang seul ne
            // suffisait pas : « gérer les rôles » permettait alors de donner à
            // un compte de rang inférieur — donc à un second compte à soi — un
            // rôle portant des permissions qu'on ne détient pas, expulsion et
            // bannissement compris. C'est la même règle qu'à la création et à
            // l'édition d'un rôle, qui la vérifiaient déjà.
            // Seul ce que l'attribution **ajoute** est contrôlé. Exiger que
            // tout le lot soit à sa portée empêchait de toucher au moindre
            // compte portant déjà un droit qu'on n'a pas — jusqu'à lui poser
            // un simple rôle de couleur — alors que rien n'y est accordé de
            // neuf. C'est la même règle qu'à l'édition d'un rôle.
            let granted = wanted
                .iter()
                .filter_map(|id| state.roles.get(*id))
                .fold(0, |acc, r| acc | r.perms);
            let held_before = state.roles.perms_of(&state.accounts.roles_of(&target));
            if grantable(state, user_id, granted & !held_before, tx).is_none() {
                return;
            }
            match state.accounts.set_roles(&target, wanted.clone()) {
                Ok(()) => {
                    state.audit.record(
                        "member.roles",
                        username,
                        &target,
                        &format!("{} rôle(s)", wanted.len()),
                    );
                    refresh_everyone(state);
                    state.reconcile_memberships();
                    send_admin_info(state, tx);
                }
                Err(e) => {
                    let _ = tx.send(ServerMsg::Error { message: e });
                }
            }
        }
        ClientMsg::AdminCreateChannel { name, kind, allowed_roles } => {
            if !require(state, user_id, tx, ki_protocol::perm::MANAGE_CHANNELS) {
                return;
            }
            match state.channels.create(&name, kind, allowed_roles) {
                Ok(channel) => {
                    // Le journal du salon s'ouvre **avant** l'annonce :
                    // `History::append` échoue en silence sur un salon
                    // inconnu, et le premier message partirait dans le vide.
                    if channel.kind == ki_protocol::ChannelKind::Text {
                        if let Err(e) = state.history.open_channel(&state.data_dir, channel.id) {
                            tracing::error!("journal du salon {} : {e:#}", channel.id);
                        }
                    }
                    state.audit.record("channel.create", username, &channel.name, "");
                    state.push_channels();
                }
                Err(e) => {
                    let _ = tx.send(ServerMsg::Error { message: e });
                }
            }
        }
        ClientMsg::AdminEditChannel { channel } => {
            if !require(state, user_id, tx, ki_protocol::perm::MANAGE_CHANNELS) {
                return;
            }
            let name = channel.name.clone();
            match state.channels.edit(channel) {
                Ok(()) => {
                    state.audit.record("channel.edit", username, &name, "");
                    state.reconcile_memberships();
                }
                Err(e) => {
                    let _ = tx.send(ServerMsg::Error { message: e });
                }
            }
        }
        ClientMsg::AdminDeleteChannel { channel } => {
            if !require(state, user_id, tx, ki_protocol::perm::MANAGE_CHANNELS) {
                return;
            }
            match state.channels.delete(&state.data_dir, channel) {
                Ok(removed) => {
                    state.history.close_channel(channel);
                    state.voice_locks.lock().unwrap().remove(&channel);
                    state.audit.record(
                        "channel.delete",
                        username,
                        &removed.name,
                        "journal archivé",
                    );
                    state.reconcile_memberships();
                }
                Err(e) => {
                    let _ = tx.send(ServerMsg::Error { message: e });
                }
            }
        }
        ClientMsg::AdminReorderChannels { order } => {
            if !require(state, user_id, tx, ki_protocol::perm::MANAGE_CHANNELS) {
                return;
            }
            match state.channels.reorder(&order) {
                Ok(()) => state.push_channels(),
                Err(e) => {
                    let _ = tx.send(ServerMsg::Error { message: e });
                }
            }
        }
        ClientMsg::AdminSetVoicePassword { channel, password, ttl_secs } => {
            if !require(state, user_id, tx, ki_protocol::perm::MANAGE_CHANNELS) {
                return;
            }
            if !state.channel_is(channel, ki_protocol::ChannelKind::Voice) {
                let _ = tx.send(ServerMsg::Error { message: "salon vocal inconnu".into() });
                return;
            }
            match password {
                Some(password) if !password.is_empty() => {
                    // Bornée : un verrou « éphémère » d'une semaine n'en est
                    // plus un, et un verrou d'une seconde ne sert à rien.
                    let ttl = ttl_secs.clamp(60, 86_400) as u64;
                    state.voice_locks.lock().unwrap().insert(
                        channel,
                        crate::state::VoiceLock {
                            password,
                            expires_at: Instant::now() + Duration::from_secs(ttl),
                        },
                    );
                    state.audit.record(
                        "channel.voice_password",
                        username,
                        &state.channels.get(channel).map(|c| c.name).unwrap_or_default(),
                        &format!("verrou posé pour {} min", ttl / 60),
                    );
                    let _ = tx.send(ServerMsg::Info {
                        message: format!("salon verrouillé pour {} min", ttl / 60),
                    });
                }
                _ => {
                    state.voice_locks.lock().unwrap().remove(&channel);
                    state.audit.record(
                        "channel.voice_password",
                        username,
                        &state.channels.get(channel).map(|c| c.name).unwrap_or_default(),
                        "verrou retiré",
                    );
                    let _ = tx.send(ServerMsg::Info { message: "verrou retiré".into() });
                }
            }
            state.push_channels();
        }
        ClientMsg::Ping => {
            let _ = tx.send(ServerMsg::Pong);
        }
    }
}

/// Retire d'un masque les permissions que l'appelant ne détient pas.
///
/// `None` = il a tenté d'en accorder une qu'il n'a pas, on refuse au lieu de
/// rogner en silence : mieux vaut un message clair qu'un rôle qui ne fait
/// pas ce que son auteur croyait.
fn grantable(
    state: &Arc<AppState>,
    user_id: UserId,
    wanted: ki_protocol::Perms,
    tx: &crate::state::Outbox,
) -> Option<ki_protocol::Perms> {
    let mine = {
        let users = state.users.lock().unwrap();
        users.get(&user_id).map(|u| u.perms).unwrap_or(0)
    };
    if ki_protocol::perm::has(mine, ki_protocol::perm::ADMINISTRATOR) || wanted & !mine == 0 {
        return Some(wanted);
    }
    let _ = tx.send(ServerMsg::Error {
        message: "tu ne peux pas accorder une permission que tu n'as pas".into(),
    });
    None
}

/// Recalcule les permissions de tous les connectés.
///
/// Un changement de rôle touche potentiellement tout le monde, et les
/// permissions sont mises en cache sur chaque connexion : sans ce
/// rafraîchissement, elles resteraient celles d'avant jusqu'à la
/// reconnexion.
fn refresh_everyone(state: &Arc<AppState>) {
    let ids: Vec<UserId> = { state.users.lock().unwrap().keys().copied().collect() };
    for id in ids {
        state.refresh_member(id);
    }
}
