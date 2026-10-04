//! Couche réseau du client PC. La connexion et la voix vivent dans ki-core,
//! partagées avec l'appli mobile ; il ne reste ici que l'émission du partage
//! d'écran, qui dépend de ki-video.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering as AtomOrd};
use std::sync::{Arc, Mutex};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ki_client_quic::quinn;
use ki_protocol::{ClientMsg, MediaHeader, StreamMeta};
use tokio::sync::mpsc as tokio_mpsc;

pub use ki_core::net::*;

/// L'émission vidéo, greffée sur la connexion de ki-core.
pub trait EmissionVideo {
    /// Prépare l'émission du partage d'écran : rend le rappel à donner à la
    /// boucle streamer.
    ///
    /// Chaque trame est chiffrée SUR LE FIL VIDÉO (clé du stream, en-tête en
    /// AAD, nonce à domaine) puis part dans SON flux QUIC unidirectionnel,
    /// priorité décroissante avec l'âge. File de deux trames vers la tâche
    /// d'émission : si le réseau ne suit pas, on jette à la source et on
    /// exige une trame clé — la même politique que le relais applique à un
    /// spectateur lent, appliquée à soi-même.
    ///
    /// Deux qualités (0.1.46) : chacune sa séquence, son domaine de nonce —
    /// sous la même clé, jamais le même nonce —, sa file et sa tâche : la
    /// basse ne fait pas la queue derrière une trame clé de la haute. La
    /// haute suspendue (personne ne la regarde) ne part pas.
    fn video_emit(
        &self,
        stream_id: u32,
        key: [u8; 32],
        force_idr: Arc<AtomicBool>,
        cadence: Arc<Mutex<StreamMeta>>,
        stats: Arc<ki_video::StageStats>,
        qualites: Arc<ki_video::Qualites>,
    ) -> Option<ki_video::FrameEmit>;
}

impl EmissionVideo for NetHandle {
    fn video_emit(
        &self,
        stream_id: u32,
        key: [u8; 32],
        force_idr: Arc<AtomicBool>,
        cadence: Arc<Mutex<StreamMeta>>,
        stats: Arc<ki_video::StageStats>,
        qualites: Arc<ki_video::Qualites>,
    ) -> Option<ki_video::FrameEmit> {
        let conn = self.connexion()?;
        let rt = self.runtime()?;
        let tx = lancer_envoi(&rt, conn.clone());
        let tx_basse = lancer_envoi(&rt, conn);
        let cipher = XChaCha20Poly1305::new(&key.into());
        let (seq, seq_basse) = (AtomicU64::new(0), AtomicU64::new(0));
        let (gop, gop_basse) = (AtomicU32::new(0), AtomicU32::new(0));
        let dims = AtomicU32::new(0);
        let cmd = self.commandes();
        Some(Arc::new(move |f: ki_video::EncodedFrame| {
            // Suspendue, la haute ne part pas — sans consommer de numéro :
            // le serveur lirait un trou pour une perte.
            if !f.basse && qualites.haute_suspendue() {
                return;
            }
            let (compteur, groupe, domaine) = if f.basse {
                (
                    &seq_basse,
                    &gop_basse,
                    ki_protocol::MEDIA_DOMAIN_VIDEO_BASSE,
                )
            } else {
                (&seq, &gop, ki_protocol::MEDIA_DOMAIN_VIDEO)
            };
            let s = compteur.fetch_add(1, AtomOrd::Relaxed);
            if f.idr {
                groupe.fetch_add(1, AtomOrd::Relaxed);
            }
            let header = MediaHeader {
                idr: f.idr,
                basse: f.basse,
                stream_id,
                seq: s,
                pts_us: f.pts_us,
                group_id: groupe.load(AtomOrd::Relaxed),
                width: f.width,
                height: f.height,
            };
            let mut head = [0u8; ki_protocol::MEDIA_HEADER_LEN];
            ki_protocol::write_media_header(&mut head, &header);
            let nonce = ki_protocol::nonce_for_media(domaine, stream_id, s);
            let Ok(sealed) = cipher.encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &f.data,
                    aad: &head,
                },
            ) else {
                return;
            };
            let mut bytes = Vec::with_capacity(head.len() + sealed.len());
            bytes.extend_from_slice(&head);
            bytes.extend_from_slice(&sealed);
            let file = if f.basse { &tx_basse } else { &tx };
            if file.try_send((s, bytes)).is_err() {
                // Le réseau ne suit pas : cette trame est perdue pour tout le
                // monde, la prochaine décodable de SA qualité devra être une
                // trame clé.
                if f.basse {
                    qualites.force_keyframe_basse();
                } else {
                    force_idr.store(true, AtomOrd::Relaxed);
                }
                stats.net_dropped.fetch_add(1, AtomOrd::Relaxed);
            }
            if f.basse {
                return;
            }
            // Dimensions changées (resize, jeu qui passe en fenêtré, réglage
            // de résolution) : le salon doit l'apprendre pour redimensionner
            // ses vues. Cadence et débit viennent des réglages, tenus à jour
            // par l'interface.
            let packed = ((f.width as u32) << 16) | f.height as u32;
            if dims.swap(packed, AtomOrd::Relaxed) != packed {
                let mut meta = *cadence.lock().unwrap();
                meta.width = f.width;
                meta.height = f.height;
                let _ = cmd.send(Cmd::Send(ClientMsg::StreamMetaUpdate { meta }));
            }
        }))
    }
}

/// La tâche d'envoi d'une qualité : chaque trame dans SON flux QUIC
/// unidirectionnel, le plus ancien d'abord — la priorité décroît avec
/// l'âge, en saturant (cf. le relais, même arithmétique). File de deux
/// trames : au-delà, l'émetteur jette à la source.
fn lancer_envoi(
    rt: &tokio::runtime::Handle,
    conn: quinn::Connection,
) -> tokio_mpsc::Sender<(u64, Vec<u8>)> {
    let (tx, mut rx) = tokio_mpsc::channel::<(u64, Vec<u8>)>(2);
    rt.spawn(async move {
        while let Some((seq, bytes)) = rx.recv().await {
            let Ok(mut flux) = conn.open_uni().await else {
                return;
            };
            let _ = flux.set_priority(0i32.saturating_sub(seq.min(i32::MAX as u64) as i32));
            if flux.write_all(&bytes).await.is_err() {
                return;
            }
            let _ = flux.finish();
        }
    });
    tx
}
