//! ki-server : serveur ki-chat (contrôle + relais vocal sur QUIC, partage
//! de fichiers en HTTP).
//!
//! Configuration par variables d'environnement :
//!   KI_TOKEN     jeton d'accès partagé (obligatoire en prod)
//!   KI_HTTP_PORT port HTTP (partage de fichiers, défaut 8080)
//!   KI_UDP_PORT  port QUIC (contrôle + voix, défaut 9987)
//!   KI_DATA_DIR  dossier de persistance (défaut ./data)

mod accounts;
mod files;
mod history;
mod meta;
mod quic;
mod state;
mod throttle;

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

    let state = Arc::new(AppState::new(token, &data_dir)?);

    // Transport QUIC : contrôle + voix sur une seule connexion chiffrée.
    // Sans lui il ne reste qu'un serveur de fichiers : ni chat, ni voix, ni
    // authentification. On s'arrête donc au lieu de survivre à moitié — sous
    // systemd comme sous Docker, c'est ce qui déclenche le redémarrage (et ce
    // qui évite un conteneur « healthy » où plus personne ne peut se
    // connecter).
    let quic_state = state.clone();
    tokio::spawn(async move {
        match quic::run(quic_state, udp_port).await {
            Ok(()) => tracing::error!("transport QUIC terminé sans erreur — arrêt"),
            Err(e) => tracing::error!("transport QUIC arrêté : {e:#}"),
        }
        std::process::exit(1);
    });

    // HTTP : uniquement le partage de fichiers (les liens du chat doivent
    // rester ouvrables dans un navigateur).
    let app = Router::new()
        .route("/", get(|| async { "ki-chat server" }))
        .route(
            "/upload",
            post(files::upload).layer(DefaultBodyLimit::max(files::MAX_FILE_SIZE)),
        )
        .route("/files/{file_id}/{name}", get(files::download))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], http_port));
    tracing::info!(
        "ki-chat en écoute : QUIC {udp_port}/udp (contrôle + voix), fichiers HTTP {addr}"
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

fn env_port(var: &str, default: u16) -> u16 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
