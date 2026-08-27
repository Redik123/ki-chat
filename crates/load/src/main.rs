//! `ki-load` : N clients virtuels sur un serveur ki-chat, pour mesurer ce
//! qu'il tient.
//!
//! ```text
//! ki-load <serveur> --clients 30 --invite changeme [--secondes 60]
//!         [--salon 101] [--muets 0] [--prefixe charge]
//! ```
//!
//! # Ce que ça fait, et ce que ça ne fait pas
//!
//! Chaque client ouvre une **vraie** connexion QUIC, crée son compte avec le
//! code d'invitation, entre dans un salon vocal et émet des datagrammes voix
//! **réellement chiffrés**, à la taille et au rythme d'un vrai client
//! (50 trames de 20 ms par seconde). Le serveur ne peut pas faire la
//! différence : il ne déchiffre jamais la voix, il relaie sur la foi de
//! l'en-tête.
//!
//! Ce qui est délibérément absent : le moteur audio. Pas de carte son, pas de
//! cpal, pas de WASAPI, pas d'encodeur Opus — trente encodeurs neuronaux sur
//! une machine mesureraient la machine, pas le serveur. La charge tourne donc
//! aussi bien depuis un conteneur Linux posé à côté du serveur.
//!
//! # Ce qu'on regarde
//!
//! - **le contrôle reçu** : c'est le chiffre de P5.1. Chaque entrée en vocal
//!   fait rediffuser le roster complet à tout le monde ; à trente, la
//!   montée en charge se paie en N². Les octets de JSON comptés ici sont
//!   exactement ce que `broadcast_all` produit.
//! - **les pertes montantes** que le serveur signale à chaque émetteur
//!   (`NetQuality`) : si elles montent, le relais ne suit plus.
//! - **le RTT** mesuré par QUIC, avant et pendant la charge.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ki_client_quic::QuicClient;
use ki_protocol::{ClientMsg, ServerMsg};

/// 20 ms à 48 kHz, comme le moteur.
const TRAME_MS: u64 = 20;
/// Charge utile d'une trame Opus à 64 kbps : 64 000 × 0,02 / 8 = 160 octets.
/// Le scellé ajoute 16 octets d'étiquette, l'en-tête 19 : 195 sur le fil.
const CHARGE_UTILE: usize = 160;

#[derive(Default)]
struct Compteurs {
    connectes: AtomicU64,
    echecs: AtomicU64,
    datagrammes_envoyes: AtomicU64,
    datagrammes_recus: AtomicU64,
    /// Messages de contrôle reçus, tous clients confondus.
    controle_messages: AtomicU64,
    /// Octets de JSON de contrôle reçus. La grandeur de P5.1.
    controle_octets: AtomicU64,
    /// Rosters complets reçus — la diffusion la plus chère du serveur.
    rosters: AtomicU64,
    /// Pertes montantes signalées par le serveur, en centièmes de pour-cent
    /// (les entiers se totalisent, les flottants dérivent).
    pertes_centiemes: AtomicU64,
    rapports_pertes: AtomicU64,
}

struct Options {
    serveur: String,
    clients: usize,
    invite: String,
    secondes: u64,
    salon: u32,
    /// Clients qui se connectent et entrent en vocal sans émettre. Un salon
    /// réel est surtout fait d'auditeurs : c'est eux qui reçoivent le relais,
    /// et donc eux qui coûtent au serveur.
    muets: usize,
    prefixe: String,
}

fn options() -> anyhow::Result<Options> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let valeur = |nom: &str| -> Option<String> {
        args.iter().position(|a| a == nom).and_then(|i| args.get(i + 1)).cloned()
    };
    let nombre = |nom: &str, defaut: u64| -> anyhow::Result<u64> {
        match valeur(nom) {
            Some(v) => v.parse().map_err(|_| anyhow::anyhow!("{nom} attend un nombre")),
            None => Ok(defaut),
        }
    };

    let serveur = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("usage : ki-load <serveur> --clients N --invite CODE"))?;
    let clients = nombre("--clients", 30)? as usize;
    let muets = nombre("--muets", 0)? as usize;
    anyhow::ensure!(clients >= 1, "il faut au moins un client");
    anyhow::ensure!(muets <= clients, "--muets ne peut pas dépasser --clients");

    Ok(Options {
        serveur,
        clients,
        invite: valeur("--invite").unwrap_or_else(|| "changeme".into()),
        secondes: nombre("--secondes", 60)?,
        salon: nombre("--salon", 101)? as u32,
        muets,
        prefixe: valeur("--prefixe").unwrap_or_else(|| "charge".into()),
    })
}

/// Demande à Windows un minuteur à la milliseconde.
///
/// Par défaut il bat à 15,6 ms : `interval(20 ms)` livre alors une trame
/// toutes les 31 ms, soit **32 par seconde au lieu de 50**. La charge
/// sous-chargerait le serveur d'un tiers, et le bilan mentirait sans qu'on
/// puisse le voir — le pire défaut possible pour un outil de mesure.
///
/// Le réglage est à l'échelle du processus et Windows le relâche à la
/// fermeture ; `ki-load` étant un outil qui tourne une minute puis s'arrête,
/// il n'y a rien à défaire.
#[cfg(windows)]
fn minuteur_fin() {
    // SAFETY: appel sans effet de bord mémoire, à la valeur minimale
    // universellement acceptée (1 ms).
    unsafe {
        windows::Win32::Media::timeBeginPeriod(1);
    }
}

#[cfg(not(windows))]
fn minuteur_fin() {}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let opts = options()?;
    minuteur_fin();
    let c = Arc::new(Compteurs::default());
    println!(
        "charge : {} clients ({} muets) sur {}, salon vocal {}, {} s",
        opts.clients, opts.muets, opts.serveur, opts.salon, opts.secondes
    );

    let fin = Instant::now() + Duration::from_secs(opts.secondes);
    let mut taches = Vec::new();
    for i in 0..opts.clients {
        let (c, serveur, invite, prefixe) = (
            c.clone(),
            opts.serveur.clone(),
            opts.invite.clone(),
            opts.prefixe.clone(),
        );
        // Les derniers sont les muets : ils écoutent sans émettre.
        let emet = i < opts.clients - opts.muets;
        let salon = opts.salon;
        taches.push(tokio::spawn(async move {
            let pseudo = format!("{prefixe}{i:03}");
            if let Err(e) = un_client(&serveur, &pseudo, &invite, salon, emet, fin, &c).await {
                c.echecs.fetch_add(1, Ordering::Relaxed);
                tracing::warn!("{pseudo} : {e:#}");
            }
        }));
        // Étalement : chaque authentification coûte un Argon2id complet, et
        // trente d'un coup mesureraient la file du pool bloquant plutôt que
        // le service. Un humain n'arrive pas non plus tout à la même
        // milliseconde.
        tokio::time::sleep(Duration::from_millis(120)).await;
    }

    // Compte rendu périodique : la charge est un régime, pas un instantané.
    let rapport = {
        let c = c.clone();
        tokio::spawn(async move {
            let mut precedent = (0u64, 0u64, 0u64);
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                if Instant::now() >= fin {
                    return;
                }
                let envoyes = c.datagrammes_envoyes.load(Ordering::Relaxed);
                let recus = c.datagrammes_recus.load(Ordering::Relaxed);
                let octets = c.controle_octets.load(Ordering::Relaxed);
                println!(
                    "  {:>3} connectés · voix {:>6}/s émis, {:>6}/s reçus · \
                     contrôle {:>7} o/s",
                    c.connectes.load(Ordering::Relaxed),
                    (envoyes - precedent.0) / 5,
                    (recus - precedent.1) / 5,
                    (octets - precedent.2) / 5,
                );
                precedent = (envoyes, recus, octets);
            }
        })
    };

    for t in taches {
        let _ = t.await;
    }
    rapport.abort();
    bilan(&c, &opts);
    Ok(())
}

fn bilan(c: &Compteurs, opts: &Options) {
    let recus = c.datagrammes_recus.load(Ordering::Relaxed);
    let envoyes = c.datagrammes_envoyes.load(Ordering::Relaxed);
    let rosters = c.rosters.load(Ordering::Relaxed);
    let octets = c.controle_octets.load(Ordering::Relaxed);
    let rapports = c.rapports_pertes.load(Ordering::Relaxed);

    println!("\n--- bilan ---");
    println!(
        "connectés          {} / {} ({} échecs)",
        c.connectes.load(Ordering::Relaxed),
        opts.clients,
        c.echecs.load(Ordering::Relaxed)
    );
    let emetteurs = opts.clients.saturating_sub(opts.muets).max(1) as f64;
    let cadence = envoyes as f64 / emetteurs / opts.secondes as f64;
    println!("datagrammes émis   {envoyes} ({cadence:.1}/s par émetteur, visé 50)");
    // Un outil de mesure qui sous-charge sans le dire est pire qu'inutile.
    if cadence < 45.0 {
        println!(
            "  ⚠ cadence basse : la charge réelle est plus faible qu'annoncée
                 (minuteur système, ou machine saturée par la charge elle-même)"
        );
    }
    println!("datagrammes reçus  {recus}");
    // Le relais amplifie : chaque paquet part vers chaque autre occupant.
    // Le rapport reçus/émis dit si le serveur a tenu l'amplification.
    if envoyes > 0 {
        let attendu = (opts.clients.saturating_sub(1)) as f64;
        let reel = recus as f64 / envoyes as f64;
        println!(
            "amplification      {reel:.2}× (attendu ~{attendu:.0}× à {} en salon)",
            opts.clients
        );
    }
    if rapports > 0 {
        let moyenne = c.pertes_centiemes.load(Ordering::Relaxed) as f64 / rapports as f64 / 100.0;
        println!("pertes montantes   {moyenne:.2} % en moyenne (dit par le serveur)");
    }
    println!(
        "contrôle reçu      {} messages, {} Kio — dont {rosters} rosters complets",
        c.controle_messages.load(Ordering::Relaxed),
        octets / 1024
    );
    if rosters > 0 {
        println!(
            "                   soit {} o par roster en moyenne — c'est ce que\n\
             \x20                  `broadcast_all` sérialise une fois PAR destinataire (P5.1)",
            octets / rosters.max(1)
        );
    }
}

/// Un client virtuel, de la connexion à la fin de la charge.
async fn un_client(
    serveur: &str,
    pseudo: &str,
    invite: &str,
    salon: u32,
    emet: bool,
    fin: Instant,
    c: &Arc<Compteurs>,
) -> anyhow::Result<()> {
    // `None` : pas de carnet de serveurs ici, on accepte l'empreinte qui se
    // présente. C'est une charge de test contre son propre serveur, pas un
    // client de production.
    let mut client = QuicClient::connect(serveur, None).await?;
    client
        .send_msg(&ClientMsg::Auth {
            username: pseudo.to_string(),
            password: "charge-de-test".into(),
            invite: Some(invite.to_string()),
        })
        .await?;

    let (mut writer, mut reader) = client.split();
    let conn = writer.conn.clone();

    // Le Welcome porte l'identité et la clé voix : sans lui, rien à émettre.
    let (identite_tx, identite_rx) = tokio::sync::oneshot::channel::<(u64, [u8; 32])>();

    let lecteur = {
        let c = c.clone();
        tokio::spawn(async move {
            let mut identite_tx = Some(identite_tx);
            while let Some(msg) = reader.next_msg().await {
                c.controle_messages.fetch_add(1, Ordering::Relaxed);
                // On repasse par le JSON pour compter ce qui a réellement
                // traversé le fil : c'est la grandeur que P5.1 vise, et
                // l'estimer à la louche n'aurait aucun intérêt.
                if let Ok(json) = serde_json::to_string(&msg) {
                    c.controle_octets.fetch_add(json.len() as u64, Ordering::Relaxed);
                }
                match msg {
                    ServerMsg::Welcome { user_id, voice_key, .. } => {
                        let cle: Option<[u8; 32]> =
                            ki_protocol::hex_decode(&voice_key).and_then(|v| v.try_into().ok());
                        if let (Some(cle), Some(tx)) = (cle, identite_tx.take()) {
                            let _ = tx.send((user_id, cle));
                        }
                    }
                    ServerMsg::Members { .. } => {
                        c.rosters.fetch_add(1, Ordering::Relaxed);
                    }
                    ServerMsg::NetQuality { loss_pct } => {
                        c.pertes_centiemes
                            .fetch_add((loss_pct * 100.0) as u64, Ordering::Relaxed);
                        c.rapports_pertes.fetch_add(1, Ordering::Relaxed);
                    }
                    ServerMsg::Error { message } => {
                        tracing::warn!("refus du serveur : {message}");
                    }
                    _ => {}
                }
            }
        })
    };

    let (user_id, cle) = tokio::time::timeout(Duration::from_secs(20), identite_rx)
        .await
        .map_err(|_| anyhow::anyhow!("pas de Welcome en 20 s"))?
        .map_err(|_| anyhow::anyhow!("connexion fermée avant le Welcome"))?;
    c.connectes.fetch_add(1, Ordering::Relaxed);

    // Réception des datagrammes relayés : on ne décode rien, on compte. C'est
    // le travail que le serveur a fait pour nous.
    let receveur = {
        let (c, conn) = (c.clone(), conn.clone());
        tokio::spawn(async move {
            while conn.read_datagram().await.is_ok() {
                c.datagrammes_recus.fetch_add(1, Ordering::Relaxed);
            }
        })
    };

    writer.send_msg(&ClientMsg::JoinVoice { channel: salon, password: None }).await?;
    if emet {
        writer.send_msg(&ClientMsg::VoiceState { speaking: true, muted: false }).await?;
    }

    if emet {
        emettre(&conn, user_id, cle, fin, c).await;
    } else {
        // Un auditeur : il ne fait rien, mais il reçoit — et c'est lui qui
        // fait payer le relais.
        tokio::time::sleep_until(fin.into()).await;
    }

    writer.close_gracefully().await;
    receveur.abort();
    lecteur.abort();
    Ok(())
}

/// Émet des trames voix au rythme d'un vrai client, jusqu'à `fin`.
async fn emettre(
    conn: &ki_client_quic::quinn::Connection,
    user_id: u64,
    cle: [u8; 32],
    fin: Instant,
    c: &Arc<Compteurs>,
) {
    use rand::Rng;
    let chiffre = XChaCha20Poly1305::new((&cle).into());
    // Le compteur sert de nonce : il doit être imprévisible et ne jamais se
    // répéter sous une même clé. Même règle que le moteur — un départ fixe
    // rejouerait les nonces d'une exécution à l'autre.
    let mut compteur: u64 = rand::rng().random::<u64>() & ((1 << 48) - 1);
    // Une charge utile figée : le serveur ne la lit pas, et la chiffrer à
    // neuf à chaque trame mesurerait notre propre processeur.
    let clair = vec![0xA5u8; CHARGE_UTILE];

    let mut tic = tokio::time::interval(Duration::from_millis(TRAME_MS));
    // Un retard ne doit pas se rattraper en rafale : cinquante paquets d'un
    // coup ne ressemblent à aucun client réel, et le seau à jetons du relais
    // les refuserait de toute façon.
    tic.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    while Instant::now() < fin {
        tic.tick().await;
        let nonce = nonce_pour(user_id, compteur);
        let Ok(scelle) = chiffre.encrypt(&nonce, clair.as_ref()) else { continue };

        let mut paquet = vec![0u8; ki_protocol::VOICE_HEADER_LEN + scelle.len()];
        ki_protocol::write_voice_header(&mut paquet, user_id, compteur);
        paquet[ki_protocol::VOICE_HEADER_LEN..].copy_from_slice(&scelle);

        if conn.send_datagram(paquet.into()).is_ok() {
            c.datagrammes_envoyes.fetch_add(1, Ordering::Relaxed);
        }
        compteur = compteur.wrapping_add(1);
    }
}

/// Le nonce dérive de (émetteur, compteur) — la même construction que le
/// moteur, sans quoi les paquets seraient rejetés par de vrais clients
/// présents dans le même salon.
fn nonce_pour(user_id: u64, compteur: u64) -> XNonce {
    let mut n = [0u8; 24];
    n[0..8].copy_from_slice(&user_id.to_le_bytes());
    n[8..16].copy_from_slice(&compteur.to_le_bytes());
    XNonce::from(n)
}
