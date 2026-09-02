//! Le son du jeu dans le stream.
//!
//! Côté streamer : la boucle de tout ce que joue le système **sauf ki-chat**
//! (les spectateurs n'entendent donc pas leurs propres voix en retour),
//! Opus stéréo en mode « audio », un paquet par 20 ms remis à la couche
//! réseau, qui le chiffre et l'envoie en datagramme. Côté spectateur : le
//! lecteur, qui décode, masque les trous, et verse le son dans la sortie du
//! moteur vocal — même volume général, même annulateur d'écho.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use anyhow::Context;
use ki_opus::{Application, Bitrate, Channels, Decoder, Encoder};

use crate::{journal, wasapi, VoiceEngine, SAMPLE_RATE};

/// Un paquet Opus encodé et son horodatage (µs depuis le début), à emporter.
pub type PaquetAudio = Arc<dyn Fn(&[u8], u64) + Send + Sync>;

/// Échantillons par trame et par canal : 20 ms à 48 kHz.
const TRAME: usize = (SAMPLE_RATE / 50) as usize;

/// La capture et l'encodage du son du jeu, tant que la poignée vit.
pub struct GameAudio {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl GameAudio {
    /// Démarre la boucle (tout le système sauf ce processus) et l'encodage
    /// à `bitrate` bits/s ; chaque paquet part par `emettre`.
    pub fn start(bitrate: i32, emettre: PaquetAudio) -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::sync_channel::<Vec<f32>>(64);
        let alive = Arc::new(AtomicBool::new(true));
        let flux = wasapi::open_loopback(std::process::id(), tx, alive.clone())
            .context("capture du son du jeu")?;
        let mut enc = Encoder::new(SAMPLE_RATE, Channels::Stereo, Application::Audio)
            .map_err(|e| anyhow::anyhow!("encodeur Opus stéréo : {e}"))?;
        let _ = enc.set_bitrate(Bitrate::Bits(bitrate));
        let _ = enc.set_complexity(8);
        journal(format!(
            "son du jeu : capture de tout le système sauf ki-chat, Opus stéréo {} kbit/s",
            bitrate / 1000
        ));
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let thread = std::thread::Builder::new()
            .name("son-du-jeu".into())
            .spawn(move || {
                // Le flux vit aussi longtemps que ce fil.
                let _flux = flux;
                let mut accum: Vec<f32> = Vec::with_capacity(TRAME * 2 * 4);
                let mut sortie = vec![0u8; 1500];
                let mut trames: u64 = 0;
                while !stop_thread.load(Ordering::Relaxed) {
                    match rx.recv_timeout(Duration::from_millis(200)) {
                        Ok(bloc) => accum.extend_from_slice(&bloc),
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if !alive.load(Ordering::Relaxed) {
                                journal("son du jeu : la capture s'est arrêtée".into());
                                return;
                            }
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                    // Une trame Opus par 20 ms de stéréo entrelacée.
                    while accum.len() >= TRAME * 2 {
                        let trame: Vec<f32> = accum.drain(..TRAME * 2).collect();
                        match enc.encode_float(&trame, &mut sortie) {
                            Ok(n) => {
                                let pts_us = trames * 20_000;
                                trames += 1;
                                emettre(&sortie[..n], pts_us);
                            }
                            Err(e) => journal(format!("son du jeu : encodage raté ({e})")),
                        }
                    }
                }
            })
            .context("fil du son du jeu")?;
        Ok(Self { stop, thread: Some(thread) })
    }
}

impl Drop for GameAudio {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Le lecteur du spectateur : Opus stéréo → mono → la sortie du moteur.
pub struct Lecteur {
    dec: Decoder,
    dernier: Option<u64>,
    pcm: Vec<f32>,
    mono: Vec<f32>,
}

impl Lecteur {
    pub fn new() -> anyhow::Result<Self> {
        let dec = Decoder::new(SAMPLE_RATE, Channels::Stereo)
            .map_err(|e| anyhow::anyhow!("décodeur Opus stéréo : {e}"))?;
        Ok(Self {
            dec,
            dernier: None,
            // Jusqu'à 60 ms d'un coup, au cas où l'émetteur grouperait.
            pcm: vec![0.0; TRAME * 3 * 2],
            mono: Vec::with_capacity(TRAME * 3),
        })
    }

    /// Un paquet, dans l'ordre d'arrivée. Un paquet en retard est jeté ; un
    /// trou de quelques trames est masqué par le décodeur (PLC) plutôt que
    /// laissé en silence sec.
    pub fn jouer(&mut self, seq: u64, paquet: &[u8], engine: &VoiceEngine) {
        if let Some(d) = self.dernier {
            if seq <= d {
                return;
            }
            let trou = (seq - d - 1).min(5);
            for _ in 0..trou {
                // Une trame de 20 ms de masquage par paquet manquant : la
                // taille du tampon dit à libopus la durée à synthétiser.
                if let Ok(n) = self.dec.decode_float(&[], &mut self.pcm[..TRAME * 2], false) {
                    self.pousser(n, engine);
                }
            }
        }
        self.dernier = Some(seq);
        if let Ok(n) = self.dec.decode_float(paquet, &mut self.pcm, false) {
            self.pousser(n, engine);
        }
    }

    /// `n` échantillons par canal décodés : réduits en mono, vers le moteur.
    fn pousser(&mut self, n: usize, engine: &VoiceEngine) {
        let n = n.min(self.pcm.len() / 2);
        self.mono.clear();
        self.mono.extend((0..n).map(|i| (self.pcm[2 * i] + self.pcm[2 * i + 1]) * 0.5));
        engine.aux_push(&self.mono);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La boucle par processus s'ouvre sur cette machine — ou dit pourquoi
    /// pas (pas de périphérique de sortie, Windows trop ancien). On ne
    /// demande pas de son : rien ne joue pendant les tests.
    #[test]
    fn la_boucle_du_systeme_s_ouvre_ou_dit_pourquoi_pas() {
        let (tx, rx) = mpsc::sync_channel::<Vec<f32>>(8);
        let alive = Arc::new(AtomicBool::new(true));
        match wasapi::open_loopback(std::process::id(), tx, alive) {
            Ok(flux) => {
                match rx.recv_timeout(Duration::from_millis(500)) {
                    Ok(bloc) => eprintln!("boucle ouverte, premier bloc : {} échantillons", bloc.len()),
                    Err(_) => eprintln!("boucle ouverte, rien ne joue (normal pendant les tests)"),
                }
                drop(flux);
            }
            Err(e) => eprintln!("boucle indisponible ici : {e:#}"),
        }
    }

    /// L'encodeur stéréo « audio » et le lecteur se comprennent : une
    /// trame de sinusoïde traverse l'aller-retour à la bonne longueur.
    #[test]
    fn l_encodeur_stereo_et_le_decodeur_se_comprennent() {
        let mut enc = Encoder::new(SAMPLE_RATE, Channels::Stereo, Application::Audio).unwrap();
        enc.set_bitrate(Bitrate::Bits(96_000)).unwrap();
        let trame: Vec<f32> = (0..TRAME * 2)
            .map(|i| ((i / 2) as f32 * 0.05).sin() * 0.3)
            .collect();
        let mut sortie = vec![0u8; 1500];
        let n = enc.encode_float(&trame, &mut sortie).unwrap();
        assert!(n > 20 && n < 600, "{n} octets");
        let mut dec = Decoder::new(SAMPLE_RATE, Channels::Stereo).unwrap();
        let mut pcm = vec![0.0f32; TRAME * 3 * 2];
        let m = dec.decode_float(&sortie[..n], &mut pcm, false).unwrap();
        assert_eq!(m, TRAME);
        // Et le masquage d'un trou rend la durée demandée par le tampon.
        let p = dec.decode_float(&[], &mut pcm[..TRAME * 2], false).unwrap();
        assert_eq!(p, TRAME);
    }
}
