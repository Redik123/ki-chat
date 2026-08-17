//! Client CLI ki-chat : chat texte + vocal, transport QUIC.
//!
//! Usage : ki-client-cli <serveur> <pseudo> <mot_de_passe> [--invite <code>] [--tone] [--deaf]
//!   ex.  : ki-client-cli 127.0.0.1 drion monpass --invite changeme
//!
//!   <serveur>        « hôte » ou « hôte:port » (port QUIC, défaut 9987)
//!   --invite <code>  code d'invitation (création du compte au premier login)
//!   --tone           émet une sinusoïde 440 Hz au lieu du micro (test)
//!   --deaf           ne lit pas l'audio reçu (test sans matériel)

use ki_client_quic::QuicClient;
use ki_protocol::{ClientMsg, ServerMsg};
use ki_voice::{VoiceConfig, VoiceEngine};
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let invite = args
        .iter()
        .position(|a| a == "--invite")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let mut skip_next = false;
    let positional: Vec<&String> = args
        .iter()
        .filter(|a| {
            if skip_next {
                skip_next = false;
                return false;
            }
            if *a == "--invite" {
                skip_next = true;
                return false;
            }
            !a.starts_with("--")
        })
        .collect();
    let [server, username, password] = positional[..] else {
        eprintln!(
            "usage : ki-client-cli <serveur> <pseudo> <mot_de_passe> [--invite <code>] [--tone] [--deaf]"
        );
        std::process::exit(1);
    };
    let tone = args.iter().any(|a| a == "--tone");
    let deaf = args.iter().any(|a| a == "--deaf");

    // Pas de carnet de serveurs en ligne de commande : on accepte ce qui se
    // présente, mais l'empreinte est affichée pour pouvoir la comparer.
    let mut client = QuicClient::connect(server, None).await?;
    println!("empreinte du serveur : {}", client.fingerprint);
    client
        .send_msg(&ClientMsg::Auth {
            username: username.clone(),
            password: password.clone(),
            invite,
        })
        .await?;

    let (mut writer, mut reader) = client.split();

    // Le Welcome (identité + clé voix) déclenche le démarrage du moteur.
    let (welcome_tx, welcome_rx) = tokio::sync::oneshot::channel::<(u64, [u8; 32])>();

    // Datagrammes voix entrants -> moteur audio (canal std).
    let (voice_tx, voice_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    {
        let conn = reader.conn.clone();
        tokio::spawn(async move {
            while let Ok(dat) = conn.read_datagram().await {
                if voice_tx.send(dat.to_vec()).is_err() {
                    break;
                }
            }
        });
    }

    let reader_task = tokio::spawn(async move {
        let mut welcome_tx = Some(welcome_tx);
        while let Some(msg) = reader.next_msg().await {
            match msg {
                ServerMsg::Welcome { user_id, voice_key, channels, .. } => {
                    println!("connecté (id {user_id}). salons :");
                    for c in channels {
                        println!("  {} : {}", c.id, c.name);
                    }
                    println!(
                        "tape /join <id> pour lire un salon texte, /voice <id> pour \
                         rejoindre un vocal, /mic on pour parler"
                    );
                    let key: Option<[u8; 32]> = ki_protocol::hex_decode(&voice_key)
                        .and_then(|v| v.try_into().ok());
                    let Some(key) = key else {
                        eprintln!("! clé voix invalide reçue du serveur");
                        continue;
                    };
                    if let Some(tx) = welcome_tx.take() {
                        let _ = tx.send((user_id, key));
                    }
                }
                ServerMsg::ServerInfo { server } => {
                    println!("* le serveur s'appelle maintenant « {} »", server.name);
                }
                // Le client en ligne de commande n'affiche pas d'images.
                ServerMsg::Avatar { .. } => {}
                ServerMsg::Chat { username, text, .. } => println!("<{username}> {text}"),
                ServerMsg::History { messages } => {
                    for m in messages {
                        println!("  [ancien] <{}> {}", m.username, m.text);
                    }
                }
                ServerMsg::UserJoined { username, .. } => println!("* {username} a rejoint"),
                ServerMsg::UserLeft { user_id } => println!("* utilisateur {user_id} est parti"),
                ServerMsg::Members { members } => {
                    let names: Vec<_> = members.iter().map(|m| m.username.as_str()).collect();
                    println!("* présents : {}", names.join(", "));
                }
                ServerMsg::VoiceState { user_id, speaking } => {
                    println!(
                        "* utilisateur {user_id} {}",
                        if speaking { "parle" } else { "silencieux" }
                    );
                }
                ServerMsg::Error { message } => eprintln!("! erreur : {message}"),
                ServerMsg::Kicked { reason } => {
                    if reason.is_empty() {
                        println!("expulsé par un admin");
                    } else {
                        println!("expulsé par un admin : {reason}");
                    }
                    std::process::exit(0);
                }
                ServerMsg::AdminInfo { users, invites } => {
                    println!("* comptes :");
                    for u in users {
                        println!(
                            "    {} (id {}){}{}{}",
                            u.username,
                            u.user_id,
                            if u.admin { " [admin]" } else { "" },
                            if u.banned { " [banni]" } else { "" },
                            if u.online { " [en ligne]" } else { "" },
                        );
                    }
                    if !invites.is_empty() {
                        println!("* invitations :");
                        for i in invites {
                            println!(
                                "    {} ({} restant(s), {} utilisé(s)){}{}",
                                i.code,
                                i.uses_left.map(|n| n.to_string()).unwrap_or_else(|| "∞".into()),
                                i.uses,
                                if i.label.is_empty() {
                                    String::new()
                                } else {
                                    format!(" « {} »", i.label)
                                },
                                if i.revoked { " [révoquée]" } else { "" },
                            );
                        }
                    }
                }
                ServerMsg::HistoryPage { messages, more, .. } => {
                    println!("* {} message(s) plus anciens :", messages.len());
                    for m in messages {
                        println!("    [{}] {} : {}", m.ts, m.username, m.text);
                    }
                    if !more {
                        println!("* (début du salon)");
                    }
                }
                ServerMsg::Roles { roles } => {
                    println!("* rôles :");
                    for r in roles {
                        println!(
                            "    {} (id {}, rang {}){}",
                            r.name,
                            r.id,
                            r.rank,
                            if r.system { " [système]" } else { "" },
                        );
                    }
                }
                ServerMsg::Perms { rank, is_admin, .. } => {
                    println!(
                        "* tes droits ont changé (rang {rank}{})",
                        if is_admin { ", administrateur" } else { "" }
                    );
                }
                ServerMsg::ChannelsUpdated { channels } => {
                    println!("* salons :");
                    for c in channels {
                        println!(
                            "    {} : {}{}{}",
                            c.id,
                            c.name,
                            if c.allowed_roles.is_some() { " [privé]" } else { "" },
                            if c.locked { " [verrouillé]" } else { "" },
                        );
                    }
                }
                ServerMsg::VoiceLocked { channel, wrong } => {
                    if wrong {
                        eprintln!("! mot de passe incorrect pour le salon {channel}");
                    } else {
                        eprintln!(
                            "! salon {channel} verrouillé — /voice {channel} <mot de passe>"
                        );
                    }
                }
                ServerMsg::AuditLog { records } => {
                    println!("* journal d'audit :");
                    for r in records {
                        println!(
                            "    [{}] {} par {}{}{}",
                            r.ts,
                            r.action,
                            if r.actor.is_empty() { "le serveur" } else { &r.actor },
                            if r.target.is_empty() {
                                String::new()
                            } else {
                                format!(" sur {}", r.target)
                            },
                            if r.detail.is_empty() {
                                String::new()
                            } else {
                                format!(" — {}", r.detail)
                            },
                        );
                    }
                }
                ServerMsg::InviteCreated { code } => println!("* invitation créée : {code}"),
                ServerMsg::Info { message } => println!("* {message}"),
                ServerMsg::NetQuality { loss_pct } => {
                    if loss_pct >= 1.0 {
                        println!("* réseau : {loss_pct:.1} % de pertes montantes");
                    }
                }
                ServerMsg::Pong => {}
            }
        }
        println!("connexion fermée par le serveur");
        std::process::exit(0);
    });

    // Démarrage du moteur audio dès réception du Welcome.
    let (user_id, voice_key) = welcome_rx.await?;
    let mut cfg = VoiceConfig::new(user_id, voice_key);
    cfg.tone = tone;
    cfg.no_playback = deaf;
    let send = ki_client_quic::datagram_sender(&writer.conn);
    let engine = match VoiceEngine::start(cfg, send, voice_rx) {
        Ok(e) => Some(e),
        Err(e) => {
            eprintln!("! vocal indisponible : {e:#}");
            None
        }
    };

    // Boucle de saisie.
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let msg = if let Some(rest) = line.strip_prefix("/voice ") {
            // `/voice <id> [mot de passe]` — le mot de passe ne sert qu'aux
            // salons vocaux verrouillés.
            let mut parts = rest.trim().splitn(2, ' ');
            match parts.next().unwrap_or_default().parse() {
                Ok(channel) => ClientMsg::JoinVoice {
                    channel,
                    password: parts.next().map(|p| p.trim().to_string()),
                },
                Err(_) => {
                    eprintln!("! id de salon vocal invalide");
                    continue;
                }
            }
        } else if let Some(rest) = line.strip_prefix("/join ") {
            match rest.trim().parse() {
                Ok(channel) => ClientMsg::Join { channel },
                Err(_) => {
                    eprintln!("! id de salon invalide");
                    continue;
                }
            }
        } else if let Some(rest) = line.strip_prefix("/mic ") {
            let Some(engine) = &engine else {
                eprintln!("! vocal indisponible");
                continue;
            };
            let on = match rest.trim() {
                "on" => true,
                "off" => false,
                _ => {
                    eprintln!("! usage : /mic on|off");
                    continue;
                }
            };
            engine.set_transmit(on);
            println!("* micro {}", if on { "activé" } else { "coupé" });
            ClientMsg::VoiceState { speaking: on }
        } else if line == "/stats" {
            match &engine {
                Some(e) => {
                    let s = e.stats();
                    println!(
                        "* voix : {} envoyés, {} reçus, {} perdus, {} récupérés FEC, {} rejetés, {} émetteur(s), ping {} ms",
                        s.packets_sent,
                        s.packets_received,
                        s.packets_lost,
                        s.packets_recovered,
                        s.packets_rejected,
                        s.active_senders,
                        writer.rtt_ms(),
                    );
                    println!(
                        "* audio : niveau micro {:.3}, {} ms de voix jouée",
                        s.mic_peak,
                        s.samples_played / 48
                    );
                }
                None => eprintln!("! vocal indisponible"),
            }
            continue;
        } else if line == "/roles" {
            ClientMsg::AdminListRoles
        } else if let Some(rest) = line.strip_prefix("/setroles ") {
            // `/setroles <pseudo> [id...]` — sans id, retire tous les rôles.
            let mut parts = rest.trim().split(' ');
            let Some(username) = parts.next().filter(|u| !u.is_empty()) else {
                eprintln!("! usage : /setroles <pseudo> [id de rôle...]");
                continue;
            };
            ClientMsg::AdminSetUserRoles {
                username: username.to_string(),
                roles: parts.filter_map(|p| p.parse().ok()).collect(),
            }
        } else if let Some(rest) = line.strip_prefix("/mkchannel ") {
            // `/mkchannel <nom> [id de rôle...]` — sans rôle, salon public.
            let mut parts = rest.trim().split(' ');
            let name = parts.next().unwrap_or_default().to_string();
            let allowed: Vec<u32> = parts.filter_map(|p| p.parse().ok()).collect();
            ClientMsg::AdminCreateChannel {
                name,
                kind: ki_protocol::ChannelKind::Text,
                allowed_roles: (!allowed.is_empty()).then_some(allowed),
            }
        } else if let Some(rest) = line.strip_prefix("/rmchannel ") {
            match rest.trim().parse() {
                Ok(channel) => ClientMsg::AdminDeleteChannel { channel },
                Err(_) => {
                    eprintln!("! usage : /rmchannel <id>");
                    continue;
                }
            }
        } else if let Some(rest) = line.strip_prefix("/lock ") {
            // `/lock <id salon vocal> <mot de passe> [minutes]`
            let mut parts = rest.trim().splitn(3, ' ');
            let channel = parts.next().and_then(|c| c.parse().ok());
            match channel {
                Some(channel) => ClientMsg::AdminSetVoicePassword {
                    channel,
                    password: parts.next().map(str::to_string),
                    ttl_secs: parts
                        .next()
                        .and_then(|m| m.parse::<u32>().ok())
                        .unwrap_or(60)
                        .saturating_mul(60),
                },
                None => {
                    eprintln!("! usage : /lock <id> <mot de passe> [minutes]");
                    continue;
                }
            }
        } else if line == "/admin" {
            ClientMsg::AdminListUsers
        } else if line == "/audit" {
            ClientMsg::AdminAuditLog { limit: 40 }
        } else if line == "/invite" {
            ClientMsg::AdminCreateInvite { uses: Some(1), label: String::new(), ttl_secs: 0 }
        } else if line == "/invite-permanent" {
            ClientMsg::AdminCreateInvite { uses: None, label: String::new(), ttl_secs: 0 }
        } else if let Some(rest) = line.strip_prefix("/revoke ") {
            ClientMsg::AdminRevokeInvite { code: rest.trim().to_string() }
        } else if let Some(rest) = line.strip_prefix("/resetpw ") {
            let mut parts = rest.trim().splitn(2, ' ');
            match (parts.next(), parts.next()) {
                (Some(user), Some(pw)) => ClientMsg::AdminResetPassword {
                    username: user.to_string(),
                    new_password: pw.to_string(),
                },
                _ => {
                    eprintln!("! usage : /resetpw <pseudo> <nouveau_mdp>");
                    continue;
                }
            }
        } else if let Some(rest) = line.strip_prefix("/passwd ") {
            let mut parts = rest.trim().splitn(2, ' ');
            match (parts.next(), parts.next()) {
                (Some(old), Some(new)) => ClientMsg::ChangePassword {
                    old_password: old.to_string(),
                    new_password: new.to_string(),
                },
                _ => {
                    eprintln!("! usage : /passwd <ancien> <nouveau>");
                    continue;
                }
            }
        } else if let Some(rest) = line.strip_prefix("/ban ") {
            // `/ban <pseudo> [minutes] [motif]` — sans durée, définitif.
            let mut parts = rest.trim().splitn(3, ' ');
            let Some(username) = parts.next().filter(|u| !u.is_empty()) else {
                eprintln!("! usage : /ban <pseudo> [minutes] [motif]");
                continue;
            };
            let minutes: u64 = parts.next().and_then(|m| m.parse().ok()).unwrap_or(0);
            ClientMsg::AdminBan {
                username: username.to_string(),
                reason: parts.next().unwrap_or_default().to_string(),
                duration_secs: minutes * 60,
            }
        } else if let Some(rest) = line.strip_prefix("/unban ") {
            ClientMsg::AdminUnban { username: rest.trim().to_string() }
        } else if let Some(rest) = line.strip_prefix("/kick ") {
            let mut parts = rest.trim().splitn(2, ' ');
            match parts.next().map(str::parse) {
                Some(Ok(user_id)) => ClientMsg::Kick {
                    user_id,
                    reason: parts.next().unwrap_or_default().to_string(),
                },
                _ => {
                    eprintln!("! usage : /kick <id utilisateur> [motif]");
                    continue;
                }
            }
        } else if line == "/history" {
            ClientMsg::History { limit: 50 }
        } else if line == "/quit" {
            break;
        } else if line.starts_with('/') {
            eprintln!("! commande inconnue");
            continue;
        } else {
            ClientMsg::Chat { text: line }
        };
        if writer.send_msg(&msg).await.is_err() {
            break;
        }
    }

    if let Some(engine) = engine {
        engine.shutdown();
    }
    reader_task.abort();
    Ok(())
}
