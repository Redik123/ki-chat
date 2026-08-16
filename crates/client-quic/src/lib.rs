//! Connexion QUIC cliente vers un serveur ki-chat : flux de contrôle
//! (JSON ligne à ligne) + datagrammes voix, sur une connexion TLS 1.3.
//!
//! Le certificat du serveur (auto-signé sur un serveur privé) n'est pas
//! vérifié : le transport reste chiffré contre l'écoute passive, et la voix
//! porte en plus son propre chiffrement de bout en bout. Pour un serveur
//! public avec domaine, une vraie vérification pourra être ajoutée.

use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use ki_protocol::{ClientMsg, ServerMsg};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

pub use quinn;

/// Port QUIC par défaut d'un serveur ki-chat.
pub const DEFAULT_PORT: u16 = 9987;
const ALPN: &[u8] = b"ki-chat";

pub struct QuicClient {
    pub conn: quinn::Connection,
    send: quinn::SendStream,
    lines: BufReader<quinn::RecvStream>,
    /// Maintenu en vie tant que la connexion existe.
    _endpoint: quinn::Endpoint,
}

impl QuicClient {
    /// Se connecte à `addr` (« hôte », « hôte:port », ou une ancienne URL
    /// « ws://hôte:port/ws » dont on extrait l'hôte) et ouvre le flux de
    /// contrôle. Le premier message envoyé doit être Auth.
    pub async fn connect(addr: &str) -> anyhow::Result<Self> {
        let (host, sockaddr) = resolve(addr)?;

        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut crypto = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .context("TLS 1.3")?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipVerify(provider)))
            .with_no_client_auth();
        crypto.alpn_protocols = vec![ALPN.to_vec()];

        let mut client_config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?,
        ));
        let mut transport = quinn::TransportConfig::default();
        transport.keep_alive_interval(Some(Duration::from_secs(5)));
        transport.max_idle_timeout(Some(Duration::from_secs(30).try_into()?));
        // Anti-bufferbloat : 32 Kio ≈ 1 s de voix en file au maximum (le
        // défaut d'1 Mio en autoriserait ~2 minutes sous congestion).
        transport.datagram_send_buffer_size(32 * 1024);
        // Borne la mémoire de réception (le défaut est illimité) tout en
        // laissant de la place à la vidéo à venir.
        transport.receive_window(quinn::VarInt::from_u32(16 * 1024 * 1024));
        // Relais vidéo : plus de 100 trames peuvent être en vol.
        transport.max_concurrent_uni_streams(quinn::VarInt::from_u32(256));
        client_config.transport_config(Arc::new(transport));

        let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;
        endpoint.set_default_client_config(client_config);

        let conn = endpoint
            .connect(sockaddr, &host)
            .context("adresse invalide")?
            .await
            .context("connexion impossible")?;
        let (mut send, recv) = conn.open_bi().await.context("flux de contrôle")?;
        // Le contrôle passe devant tout média (symétrique du serveur).
        let _ = send.set_priority(10);
        Ok(Self {
            conn,
            send,
            lines: BufReader::new(recv),
            _endpoint: endpoint,
        })
    }

    /// Envoie un message de contrôle (JSON + saut de ligne).
    pub async fn send_msg(&mut self, msg: &ClientMsg) -> anyhow::Result<()> {
        let mut json = serde_json::to_string(msg)?;
        json.push('\n');
        self.send.write_all(json.as_bytes()).await?;
        Ok(())
    }

    /// Attend le prochain message serveur. None = connexion fermée.
    pub async fn next_msg(&mut self) -> Option<ServerMsg> {
        loop {
            match read_line(&mut self.lines).await {
                Some(line) => {
                    if let Ok(msg) = serde_json::from_str::<ServerMsg>(&line) {
                        return Some(msg);
                    }
                }
                None => return None,
            }
        }
    }

    /// Scinde le client : la moitié lecture (messages serveur) et la moitié
    /// écriture + connexion (envoi de contrôle, datagrammes voix, RTT).
    pub fn split(self) -> (ControlWriter, ControlReader) {
        (
            ControlWriter { conn: self.conn.clone(), send: self.send, _endpoint: self._endpoint },
            ControlReader { conn: self.conn, lines: self.lines },
        )
    }
}

pub struct ControlWriter {
    pub conn: quinn::Connection,
    send: quinn::SendStream,
    _endpoint: quinn::Endpoint,
}

impl ControlWriter {
    pub async fn send_msg(&mut self, msg: &ClientMsg) -> anyhow::Result<()> {
        let mut json = serde_json::to_string(msg)?;
        json.push('\n');
        self.send.write_all(json.as_bytes()).await?;
        Ok(())
    }

    /// Dernier RTT mesuré par QUIC, en ms.
    pub fn rtt_ms(&self) -> u32 {
        self.conn.rtt().as_millis() as u32
    }
}

/// Fabrique un émetteur de datagrammes voix (compatible avec le moteur
/// audio) : l'appel ne bloque jamais, les erreurs sont ignorées (un
/// datagramme perdu est un datagramme perdu).
pub fn datagram_sender(
    conn: &quinn::Connection,
) -> Arc<dyn Fn(&[u8]) + Send + Sync> {
    let conn = conn.clone();
    Arc::new(move |pkt: &[u8]| {
        let _ = conn.send_datagram(bytes::Bytes::copy_from_slice(pkt));
    })
}

pub struct ControlReader {
    pub conn: quinn::Connection,
    lines: BufReader<quinn::RecvStream>,
}

impl ControlReader {
    pub async fn next_msg(&mut self) -> Option<ServerMsg> {
        loop {
            match read_line(&mut self.lines).await {
                Some(line) => {
                    if let Ok(msg) = serde_json::from_str::<ServerMsg>(&line) {
                        return Some(msg);
                    }
                }
                None => return None,
            }
        }
    }
}

/// Lit une ligne du flux de contrôle, en refusant celles qui dépassent
/// [`ki_protocol::MAX_LINE`].
///
/// Le client ne fait pas plus confiance au serveur que l'inverse : un
/// lecteur de lignes ordinaire ferait grandir son tampon sans limite, et un
/// serveur hostile — ou simplement en panne — épuiserait la mémoire de
/// l'application. Une ligne trop longue ferme la connexion.
async fn read_line(reader: &mut BufReader<quinn::RecvStream>) -> Option<String> {
    const NEWLINE: u8 = 10;
    const CARRIAGE_RETURN: u8 = 13;

    let mut buf = Vec::new();
    let limit = ki_protocol::MAX_LINE as u64 + 1;
    reader.take(limit).read_until(NEWLINE, &mut buf).await.ok()?;

    if buf.len() > ki_protocol::MAX_LINE {
        tracing::warn!("ligne de contrôle trop longue : connexion abandonnée");
        return None;
    }
    if buf.last() != Some(&NEWLINE) {
        return None; // flux refermé
    }
    buf.pop();
    if buf.last() == Some(&CARRIAGE_RETURN) {
        buf.pop();
    }
    String::from_utf8(buf).ok()
}

/// « hôte », « hôte:port », ou ancienne URL ws:// -> (hôte, adresse résolue).
fn resolve(addr: &str) -> anyhow::Result<(String, SocketAddr)> {
    let stripped = addr
        .trim()
        .strip_prefix("ws://")
        .or_else(|| addr.trim().strip_prefix("wss://"))
        .or_else(|| addr.trim().strip_prefix("quic://"))
        .unwrap_or(addr.trim());
    let host_port = stripped.split('/').next().unwrap_or(stripped);
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => match p.parse::<u16>() {
            Ok(port) => (h.to_string(), port),
            Err(_) => (host_port.to_string(), DEFAULT_PORT),
        },
        None => (host_port.to_string(), DEFAULT_PORT),
    };
    let sockaddr = (host.as_str(), port)
        .to_socket_addrs()
        .context("résolution DNS")?
        .next()
        .ok_or_else(|| anyhow::anyhow!("impossible de résoudre {host}"))?;
    Ok((host, sockaddr))
}

/// Accepte n'importe quel certificat serveur (serveur privé auto-signé).
#[derive(Debug)]
struct SkipVerify(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for SkipVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_accepts_all_formats() {
        assert_eq!(resolve("127.0.0.1").unwrap().1.port(), DEFAULT_PORT);
        assert_eq!(resolve("127.0.0.1:5000").unwrap().1.port(), 5000);
        assert_eq!(resolve("ws://127.0.0.1:8080/ws").unwrap().1.port(), 8080);
        assert_eq!(resolve("localhost").unwrap().0, "localhost");
    }
}
