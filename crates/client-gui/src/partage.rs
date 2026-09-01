//! Le partage d'écran côté interface : l'état d'une diffusion en cours, et
//! le fil décodeur d'un spectateur — jalon S1b de PLAN-STREAM.md.
//!
//! Deux moitiés, volontairement dissymétriques :
//! - **diffuser** : la boucle streamer (crate vidéo) capture et encode ; la
//!   couche réseau (net.rs) chiffre et émet. Ici ne vit que l'assemblage et
//!   l'aperçu local.
//! - **regarder** : les trames arrivent brutes du réseau (chiffrées, une par
//!   flux QUIC, dans le désordre) ; le fil de ce module déchiffre, remet en
//!   ordre par numéro de séquence, décode, et dépose l'image pour l'UI.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ki_video::{RgbaFrame, StageStats, StreamerLoop, ViewerDecoder};

/// Une diffusion en cours, vue de l'interface.
///
/// Le drapeau « trame clé exigée » ne vit pas ici : le même Arc est partagé
/// entre la boucle (qui le lit) et l'émetteur réseau (qui le lève) — la
/// demande du serveur passe par `boucle.force_keyframe()`.
pub struct GoLive {
    pub boucle: StreamerLoop,
    /// L'instrumentation par étage — collectée dès S1b, affichée en S2
    /// (l'overlay de stats du streamer).
    #[allow(dead_code)]
    pub stats: Arc<StageStats>,
    pub stream_id: u32,
    /// L'aperçu local — exactement ce que les spectateurs reçoivent.
    pub apercu: Arc<Mutex<Option<RgbaFrame>>>,
}

impl GoLive {
    pub fn arreter(self) {
        self.boucle.stop();
    }
}

/// Un stream que l'on regarde.
pub struct Regard {
    pub stream_id: u32,
    /// Qui diffuse (pour le titre de la fenêtre).
    pub streamer: String,
    /// La dernière image décodée, prête à peindre.
    pub image: Arc<Mutex<Option<RgbaFrame>>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Regard {
    /// Démarre le fil décodeur. `rx` reçoit les trames brutes que la couche
    /// réseau aiguille (`set_video_feed`).
    pub fn demarrer(
        stream_id: u32,
        streamer: String,
        key: [u8; 32],
        rx: std_mpsc::Receiver<Vec<u8>>,
        ctx: eframe::egui::Context,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let image = Arc::new(Mutex::new(None));
        let worker = {
            let (stop, image) = (stop.clone(), image.clone());
            std::thread::Builder::new()
                .name("video-regard".into())
                .spawn(move || fil_decodeur(stream_id, key, rx, image, stop, ctx))
                .ok()
        };
        Self { stream_id, streamer, image, stop, worker }
    }

    pub fn arreter(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// Au-delà de tant de trames en attente derrière un trou, on cesse
/// d'espérer : saut à la prochaine trame clé disponible, ou table rase.
const ATTENTE_MAX: usize = 30;

/// Le fil d'un spectateur : déchiffre, remet en ordre, décode.
///
/// Les trames arrivent dans le désordre — un flux QUIC chacune, les petites
/// doublent les grosses. La lecture ne démarre qu'à une trame clé, puis
/// avance strictement en séquence ; un trou qui s'éternise se règle en
/// sautant à la trame clé suivante (le serveur en a déjà demandé une si un
/// envoi nous a été sacrifié).
fn fil_decodeur(
    stream_id: u32,
    key: [u8; 32],
    rx: std_mpsc::Receiver<Vec<u8>>,
    image: Arc<Mutex<Option<RgbaFrame>>>,
    stop: Arc<AtomicBool>,
    ctx: eframe::egui::Context,
) {
    let cipher = XChaCha20Poly1305::new(&key.into());
    let Ok(mut decodeur) = ViewerDecoder::new() else {
        tracing::error!("décodeur du spectateur indisponible");
        return;
    };
    // Les trames déchiffrées en attente de leur tour, par séquence.
    let mut attente: BTreeMap<u64, (bool, Vec<u8>)> = BTreeMap::new();
    let mut prochaine: Option<u64> = None;

    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(bytes) => {
                let Some(h) = ki_protocol::parse_media_header(&bytes) else { continue };
                if h.stream_id != stream_id {
                    continue;
                }
                let nonce = ki_protocol::nonce_for_media(
                    ki_protocol::MEDIA_DOMAIN_VIDEO,
                    stream_id,
                    h.seq,
                );
                // L'en-tête est l'AAD : altéré en route, le tag le trahit.
                let (aad, sealed) = bytes.split_at(ki_protocol::MEDIA_HEADER_LEN);
                let Ok(clair) =
                    cipher.decrypt(XNonce::from_slice(&nonce), Payload { msg: sealed, aad })
                else {
                    continue;
                };
                attente.insert(h.seq, (h.idr, clair));
            }
            Err(std_mpsc::RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                continue;
            }
            Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
        }

        // Point de départ : la première trame clé vue. Tout ce qui la
        // précède est indécodable — jeté sans regret.
        if prochaine.is_none() {
            if let Some(s) = attente
                .iter()
                .find(|(_, (idr, _))| *idr)
                .map(|(s, _)| *s)
            {
                attente.retain(|k, _| *k >= s);
                prochaine = Some(s);
            } else {
                if attente.len() > 2 * ATTENTE_MAX {
                    attente.clear();
                }
                continue;
            }
        }
        let Some(mut next) = prochaine else { continue };

        // Tout ce qui est contigu part au décodeur, dans l'ordre.
        while let Some((_, clair)) = attente.remove(&next) {
            if let Some(frame) = decodeur.decode(&clair) {
                *image.lock().unwrap() = Some(frame);
                // Seul moyen de peindre au rythme du stream : la boucle de
                // repeint de l'application est plafonnée à 20 fps sinon.
                ctx.request_repaint();
            }
            next = next.wrapping_add(1);
        }

        // Un trou qui s'éternise : on saute à la prochaine trame clé plutôt
        // que d'attendre une trame qui ne viendra peut-être jamais.
        if attente.len() > ATTENTE_MAX {
            if let Some(s) = attente
                .iter()
                .find(|(k, (idr, _))| **k > next && *idr)
                .map(|(s, _)| *s)
            {
                attente.retain(|k, _| *k >= s);
                next = s;
            } else {
                // Rien de décodable en réserve : table rase, la prochaine
                // trame clé relancera la lecture.
                attente.clear();
                prochaine = None;
                continue;
            }
        }
        prochaine = Some(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Le chiffrement d'une trame telle que l'émetteur la fabrique doit se
    /// déchiffrer telle que le spectateur la lit — en-tête en AAD compris :
    /// un octet d'en-tête réécrit par le chemin invalide le tag.
    #[test]
    fn une_trame_chiffree_fait_l_aller_retour() {
        let key = [9u8; 32];
        let cipher = XChaCha20Poly1305::new(&key.into());
        let h = ki_protocol::MediaHeader {
            idr: true,
            stream_id: 3,
            seq: 41,
            pts_us: 1_000_000,
            group_id: 2,
            width: 1280,
            height: 720,
        };
        let mut head = [0u8; ki_protocol::MEDIA_HEADER_LEN];
        ki_protocol::write_media_header(&mut head, &h);
        let nonce = ki_protocol::nonce_for_media(ki_protocol::MEDIA_DOMAIN_VIDEO, 3, 41);
        let sealed = cipher
            .encrypt(XNonce::from_slice(&nonce), Payload { msg: b"nal factice", aad: &head })
            .unwrap();

        let clair = cipher
            .decrypt(XNonce::from_slice(&nonce), Payload { msg: &sealed, aad: &head })
            .unwrap();
        assert_eq!(clair, b"nal factice");

        // En-tête trafiqué (le relais réécrirait la séquence ?) : rejet.
        let mut faux = head;
        faux[8] ^= 1;
        assert!(cipher
            .decrypt(XNonce::from_slice(&nonce), Payload { msg: &sealed, aad: &faux })
            .is_err());
    }
}
