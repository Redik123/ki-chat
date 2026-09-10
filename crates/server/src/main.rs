//! ki-server : serveur ki-chat (contrôle + relais vocal sur QUIC, partage
//! de fichiers en HTTPS).
//!
//! Configuration par variables d'environnement :
//!   KI_TOKEN            jeton d'accès partagé (obligatoire en prod)
//!   KI_HTTP_PORT        port HTTP (partage de fichiers, défaut 8080)
//!   KI_UDP_PORT         port QUIC (contrôle + voix, défaut 9987)
//!   KI_DATA_DIR         dossier de persistance (défaut ./data)
//!   KI_HENRIK_KEY       clé HenrikDev pour les fiches VALORANT (sinon data/henrik.key ; absente : liaisons fermées)
//!   KI_FILES_MAX_BYTES  plafond global de data/files/ (défaut 2 Gio, 0 =
//!                       illimité) — au-delà, les partages les plus anciens
//!                       sont supprimés et les nouveaux envois refusés
//!   KI_FILES_TTL_DAYS   durée de vie d'un fichier partagé (défaut 30 jours,
//!                       0 = conservation sans limite d'âge)

mod accounts;
mod audit;
mod channels;
mod diag;
mod files;
mod history;
mod meta;
mod quic;
mod roles;
mod state;
mod store;
mod stream;
mod throttle;
mod valorant;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use axum::Router;
use tracing_subscriber::EnvFilter;

use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let token = std::env::var("KI_TOKEN").unwrap_or_else(|_| {
        tracing::warn!(
            "KI_TOKEN non défini — code d'invitation par défaut 'changeme' (dev uniquement)"
        );
        "changeme".into()
    });
    let http_port: u16 = env_port("KI_HTTP_PORT", 8080);
    let udp_port: u16 = env_port("KI_UDP_PORT", 9987);
    let data_dir = std::env::var("KI_DATA_DIR").unwrap_or_else(|_| "data".into());
    let files_quota = files::Quota {
        max_bytes: env_u64("KI_FILES_MAX_BYTES", files::DEFAULT_MAX_BYTES),
        ttl_days: env_u64("KI_FILES_TTL_DAYS", files::DEFAULT_TTL_DAYS),
    };

    let state = Arc::new(AppState::new(token, &data_dir, files_quota)?);

    // Purge du partage de fichiers. Sans elle, data/files/ ne fait que
    // grandir : sur le petit VPS qui héberge le serveur, le disque finit par
    // se remplir, et ce n'est pas seulement le partage qui tombe — plus
    // d'historique écrit, plus de sauvegarde des comptes.
    if files_quota.enabled() {
        let root = std::path::PathBuf::from(&data_dir).join("files");
        tokio::spawn(async move {
            loop {
                // Parcours de dossier et suppressions : sur le pool bloquant,
                // jamais sur la boucle qui relaie la voix.
                let dir = root.clone();
                if let Err(e) =
                    tokio::task::spawn_blocking(move || files::sweep(&dir, files_quota)).await
                {
                    tracing::error!("purge du partage interrompue : {e}");
                }
                tokio::time::sleep(files::SWEEP_INTERVAL).await;
            }
        });
    }

    // Transport QUIC : contrôle + voix sur une seule connexion chiffrée.
    // Sans lui il ne reste qu'un serveur de fichiers : ni chat, ni voix, ni
    // authentification. On s'arrête donc au lieu de survivre à moitié — sous
    // systemd comme sous Docker, c'est ce qui déclenche le redémarrage (et ce
    // qui évite un conteneur « healthy » où plus personne ne peut se
    // connecter).
    // Une seule identité pour les deux transports, chargée une seule fois.
    let (cert, key) = quic::load_or_create_cert(&data_dir)?;
    let quic_state = state.clone();
    let (quic_cert, quic_key) = (cert.clone(), key.clone_key());
    tokio::spawn(async move {
        match quic::run(quic_state, udp_port, quic_cert, quic_key).await {
            Ok(()) => tracing::error!("transport QUIC terminé sans erreur — arrêt"),
            Err(e) => tracing::error!("transport QUIC arrêté : {e:#}"),
        }
        std::process::exit(1);
    });

    // HTTPS : uniquement le partage de fichiers. Chiffré avec le **même**
    // certificat que le QUIC, donc reconnu par la même empreinte : le client
    // n'a qu'une identité de serveur à vérifier, et le contenu des fichiers
    // comme le jeton de session cessent de voyager en clair.
    //
    // Un navigateur, lui, avertira une fois que le certificat est auto-signé —
    // c'est le prix d'un serveur privé sans nom de domaine.
    // Le jeton d'accès aux diagnostics existe dès le démarrage : l'admin
    // sait où le lire avant le premier besoin.
    diag::init(&state);

    // Ce que le fil HenrikDev rapporte (liaison faite, fiche refaite) est
    // relayé d'ici : réponse à l'intéressé, roster à tout le monde. Et
    // chaque minute, les membres liés en ligne dont la fiche a plus d'une
    // demi-heure repassent en file.
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut tours: u32 = 0;
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                for r in state.valorant.resultats() {
                    match r {
                        valorant::Resultat::Liaison { user_id, ok, message, riot_id } => {
                            state.send_to(user_id, &ki_protocol::ServerMsg::LiaisonRiot { ok, message, riot_id });
                            state.broadcast_member(user_id);
                        }
                        valorant::Resultat::Fiche { user_id } => state.broadcast_member(user_id),
                    }
                }
                tours = tours.wrapping_add(1);
                if tours.is_multiple_of(120) {
                    let en_ligne: Vec<ki_protocol::UserId> = state.users.lock().unwrap().keys().copied().collect();
                    for id in state.valorant.a_rafraichir(&en_ligne, std::time::Duration::from_secs(30 * 60)) {
                        state.valorant.rafraichir(id);
                    }
                }
            }
        });
    }

    let app = Router::new()
        .route("/", get(|| async { "ki-chat server" }))
        .route(
            "/upload",
            post(files::upload).layer(DefaultBodyLimit::max(files::MAX_FILE_SIZE)),
        )
        // Diagnostics partagés : dépôt par les clients volontaires (jeton
        // voix), classement par version, lecture et purge par l'admin
        // (session ADMINISTRATOR ou jeton data/diag.token).
        .route(
            "/diag",
            post(diag::upload)
                .layer(DefaultBodyLimit::max(diag::MAX_BATCH))
                .get(diag::lister),
        )
        // L'état des lieux compté par le serveur : une ligne par version,
        // joueurs, sessions, réouvertures, famines, erreurs, crashs, taille.
        .route("/diag-resume", get(diag::resume))
        .route("/diag/{version}", axum::routing::delete(diag::supprimer))
        .route("/diag/{version}/{fichier}", get(diag::lire))
        .route("/files/{file_id}/{name}", get(files::download))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], http_port));
    tracing::info!(
        "ki-chat en écoute : QUIC {udp_port}/udp (contrôle + voix), fichiers HTTPS {addr}"
    );
    let tls = axum_server::tls_rustls::RustlsConfig::from_der(
        vec![cert.to_vec()],
        key.secret_der().to_vec(),
    )
    .await?;
    axum_server::bind_rustls(addr, tls)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await?;
    Ok(())
}

fn env_port(var: &str, default: u16) -> u16 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u64(var: &str, default: u64) -> u64 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
