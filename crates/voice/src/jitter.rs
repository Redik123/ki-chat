//! Jitter buffer + décodage par émetteur.
//!
//! Chaque émetteur distant a son propre décodeur Opus et sa file de paquets.
//! Les paquets sont remis en ordre par numéro de séquence ; une trame en
//! retard de plus de `MAX_PENDING` paquets est considérée perdue et
//! remplacée par la dissimulation de perte d'Opus (PLC).

use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

use crate::{FRAME_SAMPLES, SAMPLE_RATE};

/// Au-delà de ce nombre de paquets en attente derrière un trou, on déclare
/// la trame manquante perdue.
const MAX_PENDING: usize = 3;
/// Écart de séquence au-delà duquel on resynchronise (émetteur incohérent).
const RESYNC_GAP: u16 = 200;
/// Taille max du tampon décodé (1 s) — au-delà on jette le plus ancien.
const READY_CAP: usize = SAMPLE_RATE as usize;
/// Durée nominale d'une trame, en millisecondes.
const FRAME_MS: f32 = 20.0;

pub struct Receiver {
    decoder: opus::Decoder,
    pending: BTreeMap<u16, Vec<u8>>,
    next_seq: Option<u16>,
    ready: VecDeque<f32>,
    primed: bool,
    last_activity: Instant,
    /// Gigue réseau estimée (EWMA des écarts d'inter-arrivée, en ms).
    jitter_ms: f32,
    last_arrival: Option<Instant>,
    /// Niveau crête récent de ce locuteur (pour les vumètres), avec décroissance.
    level: f32,
    /// Taille de tampon imposée par l'utilisateur (None = adaptatif).
    jitter_override: Option<usize>,
}

impl Receiver {
    pub fn new() -> Self {
        Self {
            decoder: opus::Decoder::new(SAMPLE_RATE, opus::Channels::Mono)
                .expect("création décodeur Opus"),
            pending: BTreeMap::new(),
            next_seq: None,
            ready: VecDeque::new(),
            primed: false,
            last_activity: Instant::now(),
            jitter_ms: 0.0,
            last_arrival: None,
            level: 0.0,
            jitter_override: None,
        }
    }

    /// Impose une taille de tampon fixe (en trames), ou None pour l'adaptatif.
    pub fn set_jitter_override(&mut self, frames: Option<usize>) {
        self.jitter_override = frames;
    }

    /// Gigue réseau mesurée pour ce locuteur, en ms.
    pub fn jitter_ms(&self) -> f32 {
        self.jitter_ms
    }

    pub fn last_activity(&self) -> Instant {
        self.last_activity
    }

    /// Niveau crête récent (0..1) de ce locuteur.
    pub fn level(&self) -> f32 {
        self.level
    }

    /// Nombre de trames d'avance avant lecture, adapté à la gigue mesurée :
    /// 2 trames (40 ms) sur un réseau propre, jusqu'à 8 (160 ms) en Wi-Fi
    /// chaotique. C'est le cœur du jitter buffer adaptatif.
    fn prime_frames(&self) -> usize {
        if let Some(fixed) = self.jitter_override {
            return fixed.clamp(2, 10);
        }
        let frames = (self.jitter_ms * 2.0 / FRAME_MS).ceil() as usize + 1;
        frames.clamp(2, 8)
    }

    /// Insère un paquet reçu. Retourne (trames perdues, trames récupérées
    /// par FEC parmi elles).
    pub fn push(&mut self, seq: u16, payload: &[u8]) -> (u64, u64) {
        let now = Instant::now();
        self.last_activity = now;

        // Mesure de la gigue : écart entre le rythme d'arrivée réel et les
        // 20 ms nominales, lissé (à la RFC 3550).
        if let Some(prev) = self.last_arrival {
            let delta_ms = now.duration_since(prev).as_secs_f32() * 1000.0;
            let deviation = (delta_ms - FRAME_MS).abs();
            self.jitter_ms += (deviation - self.jitter_ms) / 8.0;
        }
        self.last_arrival = Some(now);

        let next = match self.next_seq {
            Some(n) => n,
            None => {
                self.next_seq = Some(seq);
                seq
            }
        };

        let ahead = seq.wrapping_sub(next);
        if ahead > RESYNC_GAP && ahead < u16::MAX - RESYNC_GAP {
            // Trop loin dans le futur ou le passé : on repart de là.
            self.pending.clear();
            self.next_seq = Some(seq);
        } else if ahead >= u16::MAX - RESYNC_GAP {
            // Duplicata ou paquet très en retard : ignoré.
            return (0, 0);
        }
        self.pending.insert(seq, payload.to_vec());

        // Draine tout ce qui est décodable dans l'ordre.
        let mut lost = 0u64;
        let mut recovered = 0u64;
        let mut next = self.next_seq.unwrap();
        loop {
            if let Some(p) = self.pending.remove(&next) {
                self.decode_into_ready(&p, false);
            } else if self.pending.len() > MAX_PENDING {
                // Trame perdue. Si la suivante est déjà là, ses données FEC
                // permettent de la RECONSTRUIRE (bien mieux que le masquage).
                lost += 1;
                match self.pending.get(&next.wrapping_add(1)).cloned() {
                    Some(next_packet) => {
                        self.decode_into_ready(&next_packet, true);
                        recovered += 1;
                    }
                    None => self.decode_into_ready(&[], false), // PLC
                }
            } else {
                break;
            }
            next = next.wrapping_add(1);
        }
        self.next_seq = Some(next);

        // Anti-dérive de latence : si le tampon dépasse nettement la cible
        // adaptative, on rattrape en sautant de l'audio ancien.
        let latency_cap = (self.prime_frames() + 5) * FRAME_SAMPLES;
        while self.ready.len() > latency_cap.min(READY_CAP) {
            self.ready.drain(..FRAME_SAMPLES.min(self.ready.len()));
        }
        (lost, recovered)
    }

    /// Décode un paquet vers le tampon de lecture. `fec` = reconstruire la
    /// trame PRÉCÉDENTE à partir des données de redondance de ce paquet.
    /// Un paquet vide (sans fec) déclenche la dissimulation de perte (PLC).
    fn decode_into_ready(&mut self, packet: &[u8], fec: bool) {
        let mut pcm = [0f32; FRAME_SAMPLES];
        match self.decoder.decode_float(packet, &mut pcm, fec) {
            Ok(n) => self.ready.extend(&pcm[..n]),
            Err(_) => self.ready.extend(&pcm), // silence en dernier recours
        }
    }

    /// Additionne l'audio prêt dans `out`, pondéré par `gain` (1.0 = 100 %).
    /// Retourne vrai si de l'audio a été mixé. Ne produit rien tant que
    /// `PRIME_FRAMES` trames ne sont pas accumulées (absorption du jitter),
    /// et se re-tamponne après une famine. Un gain de 0 consomme quand même
    /// le tampon (l'utilisateur est « muet » sans dériver en latence).
    pub fn mix_into(&mut self, out: &mut [f32], gain: f32) -> bool {
        if !self.primed {
            if self.ready.len() >= self.prime_frames() * FRAME_SAMPLES {
                self.primed = true;
            } else {
                self.level *= 0.85;
                return false;
            }
        }
        if self.ready.is_empty() {
            self.primed = false;
            self.level *= 0.85;
            return false;
        }
        let n = out.len().min(self.ready.len());
        let mut peak = 0f32;
        for o in out.iter_mut().take(n) {
            let s = self.ready.pop_front().unwrap() * gain;
            peak = peak.max(s.abs());
            *o += s;
        }
        // Décroissance douce : le vumètre retombe naturellement.
        self.level = peak.max(self.level * 0.85);
        gain > 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded_frame(encoder: &mut opus::Encoder, value: f32) -> Vec<u8> {
        let pcm = [value; FRAME_SAMPLES];
        let mut out = vec![0u8; 1400];
        let n = encoder.encode_float(&pcm, &mut out).unwrap();
        out.truncate(n);
        out
    }

    fn new_encoder() -> opus::Encoder {
        opus::Encoder::new(SAMPLE_RATE, opus::Channels::Mono, opus::Application::Voip).unwrap()
    }

    #[test]
    fn in_order_packets_prime_and_mix() {
        let mut enc = new_encoder();
        let mut rx = Receiver::new();
        for seq in 0..4 {
            let (lost, _) = rx.push(seq, &encoded_frame(&mut enc, 0.1));
            assert_eq!(lost, 0);
        }
        let mut out = [0f32; FRAME_SAMPLES];
        assert!(rx.mix_into(&mut out, 1.0));
    }

    #[test]
    fn gap_counts_losses_and_recovers() {
        let mut enc = new_encoder();
        let mut rx = Receiver::new();
        rx.push(0, &encoded_frame(&mut enc, 0.1));
        // Le paquet 1 est perdu ; on envoie 2..=6.
        let (mut lost, mut recovered) = (0, 0);
        for seq in 2..=6 {
            let (l, r) = rx.push(seq, &encoded_frame(&mut enc, 0.1));
            lost += l;
            recovered += r;
        }
        assert_eq!(lost, 1);
        // La trame manquante a été reconstruite via le FEC du paquet suivant.
        assert_eq!(recovered, 1);
        let mut out = [0f32; FRAME_SAMPLES];
        assert!(rx.mix_into(&mut out, 1.0));
    }

    #[test]
    fn reordered_packets_play_in_order() {
        let mut enc = new_encoder();
        let mut rx = Receiver::new();
        let frames: Vec<Vec<u8>> = (0..4).map(|_| encoded_frame(&mut enc, 0.1)).collect();
        rx.push(0, &frames[0]);
        rx.push(2, &frames[2]); // arrive avant le 1
        let (lost, _) = rx.push(1, &frames[1]);
        assert_eq!(lost, 0);
        rx.push(3, &frames[3]);
        let mut out = [0f32; FRAME_SAMPLES];
        assert!(rx.mix_into(&mut out, 1.0));
    }
}
