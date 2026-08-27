//! Connexion QUIC cliente vers un serveur ki-chat : flux de contrôle
//! (JSON ligne à ligne) + datagrammes voix, sur une connexion TLS 1.3.
//!
//! **Confiance au premier usage.** Un serveur privé s'authentifie avec un
//! certificat auto-signé, qu'aucune autorité ne contresigne : il n'y a donc
//! rien à valider au sens habituel. On mémorise en revanche son empreinte à
//! la première connexion, et l'on refuse net toute connexion ultérieure qui
//! n'en présente pas la même — c'est ce que fait SSH depuis toujours.
//!
//! Sans cela, n'importe qui sur le trajet pouvait se faire passer pour le
//! serveur, terminer le TLS, et lire le tout premier message : celui qui
//! porte le **mot de passe en clair**. Le chiffrement du transport ne protège
//! que de l'écoute passive tant que personne ne vérifie à qui l'on parle.

use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex};
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
    /// Empreinte du certificat présenté par le serveur, à mémoriser pour les
    /// connexions suivantes.
    pub fingerprint: String,
    send: quinn::SendStream,
    lines: BufReader<quinn::RecvStream>,
    /// Maintenu en vie tant que la connexion existe.
    _endpoint: quinn::Endpoint,
}

/// Configuration TLS cliente épinglée sur l'empreinte d'un serveur.
///
/// Sert au **partage de fichiers**, qui passe par HTTPS et non par QUIC : le
/// certificat est le même des deux côtés, la vérification doit donc l'être
/// aussi. Sans elle, le tunnel chiffrerait sans savoir à qui il parle — et
/// c'est le jeton de session qui y voyage.
pub fn pinned_tls_config(expected: Option<&str>) -> Arc<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("versions TLS")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinVerify {
            provider,
            expected: expected.map(str::to_string),
            seen: Arc::new(std::sync::Mutex::new(None)),
        }))
        .with_no_client_auth();
    Arc::new(config)
}

/// Empreinte SHA-256 d'un certificat, en hexadécimal groupé par octets.
///
/// Lisible à voix haute : c'est ainsi qu'on compare deux empreintes quand on
/// veut être sûr, en la lisant à celui qui héberge le serveur.
pub fn fingerprint_of(cert: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(cert);
    digest.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

impl QuicClient {
    /// Se connecte à `addr` (« hôte », « hôte:port », ou une ancienne URL
    /// « ws://hôte:port/ws » dont on extrait l'hôte) et ouvre le flux de
    /// contrôle. Le premier message envoyé doit être Auth.
    /// `expected` est l'empreinte mémorisée du serveur, si on le connaît
    /// déjà. `None` = première connexion : on accepte et l'on rend
    /// l'empreinte rencontrée, à conserver pour la prochaine fois.
    pub async fn connect(addr: &str, expected: Option<&str>) -> anyhow::Result<Self> {
        let (host, sockaddr) = resolve(addr)?;
        let seen = Arc::new(std::sync::Mutex::new(None));

        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut crypto = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .context("TLS 1.3")?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinVerify {
                provider,
                expected: expected.map(str::to_string),
                seen: seen.clone(),
            }))
            .with_no_client_auth();
        crypto.alpn_protocols = vec![ALPN.to_vec()];

        let mut client_config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?,
        ));
        let mut transport = quinn::TransportConfig::default();
        // Deux secondes de battement pour quinze d'inactivité tolérée.
        //
        // C'était cinq pour trente. Trente secondes à parler dans le vide
        // avant même de s'apercevoir que le lien est mort, c'est long quand
        // on compte sur la reprise automatique pour rendre les hoquets
        // invisibles. Et le serveur gardait tout ce temps un fantôme dans la
        // liste et dans le salon vocal.
        //
        // Le rapport des deux est ce qui compte : il dit combien de fois on
        // peut se signaler avant d'être déclaré mort. Il passe de 6 à 7,5 —
        // donc **plus** tolérant à la perte qu'avant, pour une détection deux
        // fois plus rapide. Le prix est un paquet toutes les deux secondes au
        // lieu de cinq : quinze par seconde pour trente joueurs, à comparer
        // aux cinquante par seconde d'un seul locuteur.
        //
        // Ce réglage n'était pas tenable avant R2 : chaque coupure refermait
        // le micro, et les rendre plus fréquentes aurait échangé une gêne
        // rare contre une gêne régulière. Le moteur voix survivant désormais
        // aux coupures, il ne coûte plus rien.
        transport.keep_alive_interval(Some(Duration::from_secs(2)));
        transport.max_idle_timeout(Some(Duration::from_secs(15).try_into()?));
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
        let (send, recv) = conn.open_bi().await.context("flux de contrôle")?;
        // Le contrôle passe devant tout média (symétrique du serveur).
        let _ = send.set_priority(10);
        // Une empreinte absente n'est pas un cas anodin : elle ferait
        // construire, plus loin, un client HTTPS qui n'épingle rien et
        // accepterait n'importe quel certificat pour porter le jeton de
        // session. On refuse la connexion plutôt que de céder en silence.
        let fingerprint = seen
            .lock()
            .unwrap()
            .clone()
            .context("le serveur n'a présenté aucun certificat")?;
        Ok(Self {
            conn,
            fingerprint,
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
            ControlWriter { conn: self.conn.clone(), send: self.send, endpoint: self._endpoint },
            ControlReader { conn: self.conn, lines: self.lines },
        )
    }
}

pub struct ControlWriter {
    pub conn: quinn::Connection,
    send: quinn::SendStream,
    endpoint: quinn::Endpoint,
}

impl ControlWriter {
    pub async fn send_msg(&mut self, msg: &ClientMsg) -> anyhow::Result<()> {
        let mut json = serde_json::to_string(msg)?;
        json.push('\n');
        self.send.write_all(json.as_bytes()).await?;
        Ok(())
    }

    /// Ferme la connexion et **attend que la trame de fermeture soit
    /// réellement partie**.
    ///
    /// `close` ne fait que la mettre en file : si le processus s'arrête dans
    /// la foulée, elle n'atteint jamais le serveur, qui garde alors la
    /// session ouverte jusqu'à son expiration d'inactivité. `wait_idle`
    /// rend cette fermeture effective. Le délai borne l'attente, pour qu'une
    /// fermeture ne puisse jamais retenir l'application.
    pub async fn close_gracefully(self) {
        self.conn.close(0u32.into(), b"bye");
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(600),
            self.endpoint.wait_idle(),
        )
        .await;
    }

    /// Dernier RTT mesuré par QUIC, en ms.
    pub fn rtt_ms(&self) -> u32 {
        self.conn.rtt().as_millis() as u32
    }
}

/// Émetteur de datagrammes voix, tel que l'attend le moteur audio.
/// Le même contrat que `ki_voice::DatagramSend`, redit ici pour que ce crate
/// n'ait pas à dépendre du moteur.
pub type DatagramSend = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// Fabrique un émetteur de datagrammes voix (compatible avec le moteur
/// audio) : l'appel ne bloque jamais, les erreurs sont ignorées (un
/// datagramme perdu est un datagramme perdu).
pub fn datagram_sender(conn: &quinn::Connection) -> DatagramSend {
    let conn = conn.clone();
    Arc::new(move |pkt: &[u8]| {
        let _ = conn.send_datagram(bytes::Bytes::copy_from_slice(pkt));
    })
}

/// Un émetteur lié à un **emplacement** de connexion plutôt qu'à une
/// connexion.
///
/// C'est ce qui permet au moteur voix de survivre à une coupure : il garde le
/// même émetteur d'un bout à l'autre, et celui-ci suit la connexion du
/// moment. Sans cette indirection, l'émetteur retenait une connexion morte,
/// et il fallait reconstruire le moteur — donc rouvrir le micro et la sortie,
/// exactement ce qu'on cherche à éviter.
///
/// Emplacement vide, pendant la coupure : la trame est jetée. C'est le bon
/// comportement, il n'y a nulle part où la mettre, et le chemin audio ne doit
/// jamais attendre — pas plus ici qu'ailleurs.
///
/// Le verrou est pris cinquante fois par seconde, sur le fil d'encodage et
/// non dans le rappel temps réel du périphérique : la seule chose qui le
/// dispute est la reconnexion, qui arrive une fois par heure au pire.
pub fn datagram_sender_slot(slot: Arc<Mutex<Option<quinn::Connection>>>) -> DatagramSend {
    Arc::new(move |pkt: &[u8]| {
        if let Some(conn) = slot.lock().unwrap().as_ref() {
            let _ = conn.send_datagram(bytes::Bytes::copy_from_slice(pkt));
        }
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

/// Confiance au premier usage : mémorise l'empreinte, refuse qu'elle change.
///
/// Il n'y a pas de chaîne à valider — le certificat d'un serveur privé est
/// auto-signé. Ce qui est vérifié, c'est la **continuité** : le serveur
/// d'aujourd'hui est-il celui d'hier ?
#[derive(Debug)]
struct PinVerify {
    provider: Arc<rustls::crypto::CryptoProvider>,
    /// Empreinte connue, si l'on s'est déjà connecté à ce serveur.
    expected: Option<String>,
    /// Empreinte réellement présentée, remontée à l'appelant.
    seen: Arc<std::sync::Mutex<Option<String>>>,
}

impl rustls::client::danger::ServerCertVerifier for PinVerify {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let seen = fingerprint_of(end_entity.as_ref());
        *self.seen.lock().unwrap() = Some(seen.clone());
        match &self.expected {
            // Première rencontre : on accepte et l'on retiendra.
            None => Ok(rustls::client::danger::ServerCertVerified::assertion()),
            Some(known) if *known == seen => {
                Ok(rustls::client::danger::ServerCertVerified::assertion())
            }
            // Changement d'identité : soit le serveur a été réinstallé, soit
            // quelqu'un se glisse entre les deux. On ne peut pas trancher, et
            // continuer livrerait le mot de passe : on refuse.
            Some(_) => Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            )),
        }
    }

    // Ces deux-là sont **la** vérification qui compte, et elles doivent faire
    // leur travail pour de bon.
    //
    // Un certificat est un document public : le serveur l'envoie en clair à
    // qui le lui demande, et une simple sonde suffit à s'en procurer une
    // copie. Reconnaître son empreinte ne prouve donc rien à soi seul. Ce qui
    // prouve que l'on parle bien au serveur, c'est la signature qu'il appose
    // sur la transcription de la poignée de main avec sa **clé privée**, que
    // lui seul détient. Les accepter sans les regarder — ce que faisait le
    // vérificateur d'origine, qui ne vérifiait rien du tout — laissait
    // n'importe qui rejouer le certificat légitime et se glisser au milieu,
    // empreinte parfaitement conforme à l'appui.
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Le cœur de la confiance au premier usage : on accepte un serveur
    /// inconnu, on le reconnaît ensuite, et l'on refuse quiconque se présente
    /// à sa place — c'est ce refus qui protège le mot de passe.
    #[test]
    fn a_server_that_changes_identity_is_refused() {
        use rustls::client::danger::ServerCertVerifier as _;
        use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

        let cert = CertificateDer::from(vec![1, 2, 3, 4]);
        let imposteur = CertificateDer::from(vec![9, 9, 9, 9]);
        let name = ServerName::try_from("ki-chat").unwrap();
        let now = UnixTime::now();
        let verifier = |expected: Option<String>| PinVerify {
            provider: Arc::new(rustls::crypto::ring::default_provider()),
            expected,
            seen: Arc::new(std::sync::Mutex::new(None)),
        };

        // Première connexion : rien de connu, on accepte et l'on retient.
        let first = verifier(None);
        assert!(first.verify_server_cert(&cert, &[], &name, &[], now).is_ok());
        let learned = first.seen.lock().unwrap().clone().expect("empreinte relevée");
        assert_eq!(learned, fingerprint_of(cert.as_ref()));

        // Retour sur le même serveur : reconnu.
        let known = verifier(Some(learned.clone()));
        assert!(known.verify_server_cert(&cert, &[], &name, &[], now).is_ok());

        // Quelqu'un d'autre au bout du fil : refusé.
        let known = verifier(Some(learned));
        assert!(known.verify_server_cert(&imposteur, &[], &name, &[], now).is_err());
    }

    #[test]
    fn fingerprints_are_stable_and_distinguish() {
        let a = fingerprint_of(b"certificat a");
        assert_eq!(a, fingerprint_of(b"certificat a"));
        assert_ne!(a, fingerprint_of(b"certificat b"));
        // 32 octets en hexadécimal, séparés par des deux-points : lisible à
        // voix haute pour comparer avec celui qui héberge le serveur.
        assert_eq!(a.split(':').count(), 32);
    }

    #[test]
    fn resolve_accepts_all_formats() {
        assert_eq!(resolve("127.0.0.1").unwrap().1.port(), DEFAULT_PORT);
        assert_eq!(resolve("127.0.0.1:5000").unwrap().1.port(), 5000);
        assert_eq!(resolve("ws://127.0.0.1:8080/ws").unwrap().1.port(), 8080);
        assert_eq!(resolve("localhost").unwrap().0, "localhost");
    }
}

#[cfg(test)]
mod signature_tests {
    use super::*;

    /// Le test qui compte vraiment : un certificat est **public**, une simple
    /// sonde suffit à s'en procurer une copie. Reconnaître son empreinte ne
    /// prouve donc rien — ce qui prouve qu'on parle au bon serveur, c'est la
    /// signature qu'il appose avec sa clé privée sur la poignée de main.
    /// Accepter cette signature sans la vérifier laissait n'importe qui
    /// rejouer le certificat légitime et se glisser au milieu.
    #[test]
    fn the_handshake_signature_is_actually_verified() {
        use rustls::client::danger::ServerCertVerifier as _;

        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = PinVerify {
            provider: provider.clone(),
            expected: None,
            seen: Arc::new(std::sync::Mutex::new(None)),
        };
        // Un vrai certificat, et une signature qui ne vaut rien.
        let cert = rcgen::generate_simple_self_signed(vec!["ki-chat".into()]).unwrap();
        let der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
        // Construite par décodage : le constructeur direct n'est pas public.
        // Format : schéma (u16), longueur (u16), puis la signature.
        use rustls::internal::msgs::codec::{Codec, Reader};
        let mut brut = vec![0x04, 0x03, 0x00, 0x40];
        brut.extend_from_slice(&[0u8; 64]);
        let bidon =
            rustls::DigitallySignedStruct::read(&mut Reader::init(&brut)).unwrap();

        // La signature est fausse : elle doit être rejetée. Le vérificateur
        // d'origine renvoyait « j'atteste » sans rien regarder.
        assert!(
            verifier.verify_tls13_signature(b"transcription", &der, &bidon).is_err(),
            "une signature invalide a été acceptée : l'épinglage ne protège de rien"
        );
    }
}
