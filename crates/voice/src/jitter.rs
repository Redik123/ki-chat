//! Jitter buffer + décodage par émetteur.
//!
//! Chaque émetteur distant a son propre décodeur Opus et sa file de paquets.
//! Les paquets sont remis en ordre par numéro de séquence ; une trame en
//! retard de plus de `MAX_PENDING` paquets est considérée perdue et
//! remplacée par la dissimulation de perte d'Opus (PLC).

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ki_opus as opus;

use crate::{FRAME_SAMPLES, SAMPLE_RATE};

/// Au-delà de ce nombre de paquets en attente derrière un trou, on déclare
/// la trame manquante perdue.
const MAX_PENDING: usize = 3;
/// Écart de séquence au-delà duquel on resynchronise (émetteur incohérent).
const RESYNC_GAP: u16 = 200;
/// Taille max du tampon décodé (1 s) — au-delà on jette le plus ancien.
const READY_CAP: usize = SAMPLE_RATE as usize;
/// Capacité réservée d'emblée au tampon de lecture : le plafond de latence le
/// plus large possible (10 trames imposables + 5 de marge). Réservée une fois
/// pour toutes, le fil réseau n'a plus jamais à réallouer sous le verrou que
/// le rappel de sortie peut vouloir prendre.
const READY_PREALLOC: usize = 15 * FRAME_SAMPLES;
/// Durée nominale d'une trame, en millisecondes.
const FRAME_MS: f32 = 20.0;
/// Au-delà de cet écart entre deux arrivées, ce n'est plus de la gigue : c'est
/// que l'émetteur s'était tu. Large de dix trames — une vraie rafale réseau
/// reste en dessous, un silence de parole largement au-dessus.
const TALKSPURT_MS: f32 = 200.0;
/// Marge tolérée au-dessus de la cible avant de rattraper la latence.
///
/// Le rattrapage jette de l'audio sans fondu : c'est un clic. Il ne doit donc
/// se déclencher que sur une vraie dérive, jamais sur trois paquets qui
/// arrivent groupés — ce qui est le quotidien du Wi-Fi. Quatre trames au-dessus
/// de la cible laissent passer les rafales ordinaires tout en plafonnant la
/// latence à 120 ms sur un réseau propre, là où elle pouvait s'installer à
/// 140 ms et n'en jamais redescendre.
const LATENCY_SLACK: usize = 4;

/// Fenêtre de recherche DRED : jusqu'à 1 s (50 trames de 20 ms) de passé
/// peut être resynthétisé depuis la redondance neuronale d'un paquet.
const DRED_WINDOW: u16 = 50;

/// Profondeur maximale de trou que l'on dissimule trame à trame. Au-delà,
/// on saute au bord du trou : synthétiser des dizaines de trames Deep PLC
/// coûteuses pour que l'écrêtage de latence les jette aussitôt, c'était tout
/// ce travail devant le rappel de sortie au pire moment — et pour rien.
const CONCEAL_MAX: u16 = 10;

/// Paquets « très en retard » consécutifs avant de conclure que l'émetteur
/// est reparti d'un compteur aléatoire juste derrière nous (le compteur sert
/// de nonce et repart au hasard à chaque recréation du moteur d'en face) et
/// de resynchroniser. Un vrai duplicata n'arrive jamais en série continue.
const BEHIND_RESYNC: u8 = 5;

/// Audio décodé, prêt à sortir vers la carte son.
///
/// **Séparé de l'état de décodage, et c'est tout l'intérêt.** Le rappel de
/// sortie est un fil temps réel : s'il attend, la carte son se retrouve sans
/// données et l'on entend un craquement. Or décoder coûte cher — Opus en
/// complexité 10 fait tourner un masquage de perte neuronal, et la
/// reconstruction DRED une synthèse complète — et un trou réseau peut demander
/// des dizaines de trames d'affilée. Tant que le décodeur et le tampon de
/// lecture partageaient un verrou, tout ce travail passait devant la carte son
/// précisément quand le réseau allait mal. Ici, le fil réseau ne prend ce
/// verrou que pour **déposer** des échantillons déjà décodés, et le rappel de
/// sortie que pour les consommer : quelques microsecondes de chaque côté.
pub struct Playout {
    ready: VecDeque<f32>,
    primed: bool,
    /// Niveau crête récent de ce locuteur (pour les vumètres), avec décroissance.
    level: f32,
    /// Trames d'avance à accumuler avant de jouer. Calculée côté décodeur,
    /// qui est le seul à mesurer la gigue.
    prime_frames: usize,
}

impl Playout {
    fn new() -> Self {
        Self {
            ready: VecDeque::with_capacity(READY_PREALLOC),
            primed: false,
            level: 0.0,
            prime_frames: 2,
        }
    }

    /// Niveau crête récent (0..1) de ce locuteur.
    pub fn level(&self) -> f32 {
        self.level
    }

    /// Additionne l'audio prêt dans `out`, pondéré par `gain` (1.0 = 100 %).
    /// Retourne vrai si de l'audio a été mixé. Ne produit rien tant que
    /// `prime_frames` trames ne sont pas accumulées (absorption du jitter),
    /// et se re-tamponne après une famine. Un gain de 0 consomme quand même
    /// le tampon (l'utilisateur est « muet » sans dériver en latence).
    pub fn mix_into(&mut self, out: &mut [f32], gain: f32) -> bool {
        if !self.primed {
            if self.ready.len() >= self.prime_frames * FRAME_SAMPLES {
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

pub struct Receiver {
    decoder: opus::Decoder,
    /// Décodeur de redondance neuronale (None si libopus l'a refusé).
    dred: Option<opus::Dred>,
    /// Dernier paquet porteur analysé (évite de re-parser à chaque trame
    /// d'un même trou).
    dred_carrier: Option<u16>,
    pending: BTreeMap<u16, Vec<u8>>,
    next_seq: Option<u16>,
    /// Échantillons décodés du tour courant, **hors verrou**. Déposés dans le
    /// `Playout` en une fois, à la fin de `push`.
    decoded: Vec<f32>,
    /// Le tampon de lecture, partagé avec le rappel de sortie.
    playout: Arc<Mutex<Playout>>,
    last_activity: Instant,
    /// Gigue réseau estimée (EWMA des écarts d'inter-arrivée, en ms).
    jitter_ms: f32,
    last_arrival: Option<Instant>,
    /// Plus grosse rafale récente, en trames livrées d'un coup.
    ///
    /// L'écart entre deux arrivées ne dit pas tout : sur un lien qui livre par
    /// paquets, les trames d'une même rafale arrivent quasiment ensemble, si
    /// bien que la déviation mesurée reste minuscule quelle que soit la
    /// **longueur** de la rafale. C'est pourtant elle qui dicte la taille du
    /// tampon. Un silence de parole, lui, ne produit jamais de rafale : la
    /// mesure distingue donc les deux là où l'horloge seule en est incapable.
    burst_frames: usize,
    last_burst_decay: Instant,
    /// Paquets « très en retard » consécutifs (voir BEHIND_RESYNC).
    behind_run: u8,
    /// Taille de tampon imposée par l'utilisateur (None = adaptatif).
    jitter_override: Option<usize>,
}

impl Receiver {
    pub fn new() -> Self {
        let mut decoder = opus::Decoder::new(SAMPLE_RATE, opus::Channels::Mono)
            .expect("création décodeur Opus");
        // Complexité maximale : active le Deep PLC (masquage de perte
        // neuronal) de libopus 1.6 sur les trames irrécupérables.
        let _ = decoder.set_complexity(10);
        Self {
            decoder,
            dred: opus::Dred::new().ok(),
            dred_carrier: None,
            pending: BTreeMap::new(),
            next_seq: None,
            decoded: Vec::with_capacity(FRAME_SAMPLES * 8),
            playout: Arc::new(Mutex::new(Playout::new())),
            last_activity: Instant::now(),
            jitter_ms: 0.0,
            last_arrival: None,
            burst_frames: 0,
            last_burst_decay: Instant::now(),
            behind_run: 0,
            jitter_override: None,
        }
    }

    /// Le tampon de lecture de ce locuteur, à confier au rappel de sortie.
    pub fn playout(&self) -> Arc<Mutex<Playout>> {
        self.playout.clone()
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

    /// Nombre de trames d'avance avant lecture, adapté à la gigue mesurée :
    /// 2 trames (40 ms) sur un réseau propre, jusqu'à 8 (160 ms) en Wi-Fi
    /// chaotique. C'est le cœur du jitter buffer adaptatif.
    fn prime_frames(&self) -> usize {
        if let Some(fixed) = self.jitter_override {
            return fixed.clamp(2, 10);
        }
        let frames = (self.jitter_ms * 2.0 / FRAME_MS).ceil() as usize + 1;
        // Le tampon doit couvrir la plus grosse rafale récente : sur un lien
        // qui livre par paquets, c'est elle qui décide, pas l'écart moyen.
        frames.max(self.burst_frames).clamp(2, 8)
    }

    /// Insère un paquet reçu. Retourne (trames perdues, trames récupérées
    /// par FEC parmi elles).
    pub fn push(&mut self, seq: u16, payload: &[u8]) -> (u64, u64) {
        let now = Instant::now();
        self.last_activity = now;

        // Mesure de la gigue : écart entre le rythme d'arrivée réel et les
        // 20 ms nominales, lissé (à la RFC 3550).
        //
        // Une reprise de parole n'en est pas. L'émetteur cesse d'émettre
        // pendant les silences — activation vocale ou touche de conversation —
        // si bien que la première trame d'une phrase arrive des secondes après
        // la précédente. Compter cet écart comme de la gigue faisait bondir
        // l'estimation à plusieurs centaines de millisecondes sur un réseau
        // parfait, et le tampon retenait alors le début de chaque phrase.
        if let Some(prev) = self.last_arrival {
            let delta_ms = now.duration_since(prev).as_secs_f32() * 1000.0;
            if delta_ms < TALKSPURT_MS {
                let deviation = (delta_ms - FRAME_MS).abs();
                self.jitter_ms += (deviation - self.jitter_ms) / 8.0;
            }
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
            // Trop loin dans le futur : on repart de là.
            self.pending.clear();
            self.next_seq = Some(seq);
        } else if ahead >= u16::MAX - RESYNC_GAP {
            // Duplicata ou paquet très en retard : ignoré — sauf si ça
            // insiste. Un émetteur dont le moteur est recréé repart d'un
            // compteur aléatoire ; s'il retombe juste derrière le nôtre,
            // chaque paquet du nouveau flux passerait pour un retardataire
            // pendant des secondes de mutisme. Une série ininterrompue de
            // « retardataires » n'est pas une série de duplicatas : on
            // resynchronise dessus.
            self.behind_run += 1;
            if self.behind_run < BEHIND_RESYNC {
                return (0, 0);
            }
            self.pending.clear();
            self.next_seq = Some(seq);
        }
        self.behind_run = 0;
        self.pending.insert(seq, payload.to_vec());

        // Draine tout ce qui est décodable dans l'ordre. Tout ce bloc décode
        // vers `self.decoded`, sans tenir le moindre verrou : c'est la partie
        // coûteuse, et le rappel de sortie ne doit jamais l'attendre.
        self.decoded.clear();
        let mut lost = 0u64;
        let mut recovered = 0u64;
        let mut delivered = 0usize;
        let mut next = self.next_seq.unwrap();
        loop {
            if let Some(p) = self.pending.remove(&next) {
                self.decode_into_ready(&p, false);
                self.dred_carrier = None;
                delivered += 1;
            } else if self.pending.len() > MAX_PENDING {
                // Un trou plus profond que ce qu'on dissimule trame à trame ?
                // On saute à son bord : ces trames sont perdues quoi qu'il
                // arrive (l'écrêtage de latence les jetterait), autant ne pas
                // les synthétiser une à une au Deep PLC.
                let dist = self
                    .pending
                    .keys()
                    .map(|s| s.wrapping_sub(next))
                    .min()
                    .unwrap_or(0);
                if dist > CONCEAL_MAX {
                    let skip = dist - CONCEAL_MAX;
                    lost += skip as u64;
                    next = next.wrapping_add(skip);
                    continue;
                }
                // Trame perdue. Trois niveaux de secours, du meilleur au
                // pis-aller : FEC LBRR (trame n+1), DRED (redondance
                // neuronale d'un paquet jusqu'à 1 s plus tard), Deep PLC.
                lost += 1;
                if let Some(next_packet) = self.pending.get(&next.wrapping_add(1)).cloned() {
                    self.decode_into_ready(&next_packet, true);
                    recovered += 1;
                } else if self.try_dred_recover(next) {
                    recovered += 1;
                } else {
                    self.decode_into_ready(&[], false); // PLC neuronal
                }
            } else {
                break;
            }
            next = next.wrapping_add(1);
        }
        self.next_seq = Some(next);

        // Profondeur de cette livraison — comptée sur les seuls paquets
        // réellement reçus : les trames de dissimulation d'un trou réseau ne
        // sont pas une rafale, et les compter épinglait le tampon au plafond
        // pendant des minutes après une simple coupure de 2 s. Bornée au
        // plafond utile de `prime_frames` (8), et oubli progressif : le
        // tampon redescend quand le lien redevient régulier.
        self.burst_frames = self.burst_frames.max(delivered.min(8));
        if self.last_burst_decay.elapsed() > Duration::from_secs(5) {
            self.burst_frames = self.burst_frames.saturating_sub(1);
            self.last_burst_decay = now;
        }

        // Anti-dérive de latence : au-delà de la cible adaptative, on rattrape
        // en sautant de l'audio ancien. L'écrêtage se fait **avant** le
        // verrou : un trou réseau de plusieurs secondes fait décoder jusqu'à
        // RESYNC_GAP trames d'un coup, et recopier ces ~750 Ko sous le verrou
        // pour les jeter aussitôt aurait remis une allocation et une longue
        // recopie sur le chemin du rappel de sortie — ce que cette séparation
        // existe précisément pour éviter.
        let prime = self.prime_frames();
        let cap = ((prime + LATENCY_SLACK) * FRAME_SAMPLES).min(READY_CAP);
        if self.decoded.len() > cap {
            let excess = self.decoded.len() - cap;
            self.decoded.drain(..excess);
        }
        {
            let mut playout = self.playout.lock().unwrap();
            playout.prime_frames = prime;
            // Ce que le dépôt va faire déborder est retiré d'abord : la
            // section critique reste bornée par `cap`, sans réallocation.
            let over = (playout.ready.len() + self.decoded.len()).saturating_sub(cap);
            let drop = over.min(playout.ready.len());
            playout.ready.drain(..drop);
            playout.ready.extend(self.decoded.iter().copied());
        }
        self.decoded.clear();
        (lost, recovered)
    }

    /// Tente de resynthétiser la trame manquante `missing` depuis la
    /// redondance neuronale (DRED) du paquet futur le plus proche.
    fn try_dred_recover(&mut self, missing: u16) -> bool {
        let Some(dred) = self.dred.as_mut() else { return false };
        // Cherche le porteur le plus proche (k=1 est déjà couvert par le FEC).
        let mut carrier: Option<(u16, Vec<u8>)> = None;
        for k in 2..=DRED_WINDOW {
            let seq = missing.wrapping_add(k);
            if let Some(p) = self.pending.get(&seq) {
                carrier = Some((k, p.clone()));
                break;
            }
        }
        let Some((dist, packet)) = carrier else { return false };
        let carrier_seq = missing.wrapping_add(dist);

        // Ne re-parse le porteur que s'il change (un trou de N trames est
        // typiquement couvert par le même paquet).
        if self.dred_carrier != Some(carrier_seq) {
            let max = DRED_WINDOW as i32 * FRAME_SAMPLES as i32;
            match dred.parse(&packet, max, SAMPLE_RATE as i32) {
                Ok(_) => self.dred_carrier = Some(carrier_seq),
                Err(_) => return false,
            }
        }
        // La redondance couvre `available` échantillons AVANT le porteur ;
        // notre trame est à `dist` trames avant lui.
        let offset = dist as i32 * FRAME_SAMPLES as i32;
        if dred.available() < offset {
            return false;
        }
        let mut pcm = [0f32; FRAME_SAMPLES];
        match dred.decode_into(&mut self.decoder, offset, &mut pcm) {
            Ok(n) if n > 0 => {
                self.decoded.extend_from_slice(&pcm[..n]);
                true
            }
            _ => false,
        }
    }

    /// Décode un paquet vers le tampon local (déposé plus tard, en une fois).
    /// `fec` = reconstruire la trame PRÉCÉDENTE à partir des données de
    /// redondance de ce paquet. Un paquet vide (sans fec) déclenche la
    /// dissimulation de perte (PLC).
    fn decode_into_ready(&mut self, packet: &[u8], fec: bool) {
        let mut pcm = [0f32; FRAME_SAMPLES];
        match self.decoder.decode_float(packet, &mut pcm, fec) {
            Ok(n) => self.decoded.extend_from_slice(&pcm[..n]),
            Err(_) => self.decoded.extend_from_slice(&pcm), // silence en dernier recours
        }
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
        assert!(rx.playout().lock().unwrap().mix_into(&mut out, 1.0));
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
        assert!(rx.playout().lock().unwrap().mix_into(&mut out, 1.0));
    }

    #[test]
    fn dred_recovers_multi_frame_hole_beyond_fec() {
        let mut enc = new_encoder();
        // Configuration de production : le budget DRED (VBR) dépend de la
        // perte annoncée — sans elle, l'encodeur n'embarque presque rien.
        enc.set_bitrate(opus::Bitrate::Bits(64_000)).unwrap();
        enc.set_inband_fec(true).unwrap();
        enc.set_packet_loss_perc(30).unwrap();
        enc.set_complexity(10).unwrap();
        enc.set_dred_duration(crate::DRED_DEFAULT)
            .expect("DRED refusé — libopus compilé sans OPUS_DRED ?");
        let mut rx = Receiver::new();

        // Un vrai signal continu (sinusoïde) : le DRED encode le passé.
        let mut phase = 0f32;
        let mut frames = Vec::new();
        for _ in 0..60 {
            let mut pcm = [0f32; FRAME_SAMPLES];
            for s in pcm.iter_mut() {
                *s = phase.sin() * 0.3;
                phase += 0.05;
            }
            let mut out = vec![0u8; 4000];
            let n = enc.encode_float(&pcm, &mut out).unwrap();
            out.truncate(n);
            frames.push(out);
        }

        // Trou de 4 trames (30..=33) : la 33 est récupérable par FEC (la 34
        // existe), les 30-32 exigent le DRED du paquet 34.
        let (mut lost, mut recovered) = (0u64, 0u64);
        for seq in 0..60u16 {
            if (30..=33).contains(&seq) {
                continue;
            }
            let (l, r) = rx.push(seq, &frames[seq as usize]);
            lost += l;
            recovered += r;
        }
        assert_eq!(lost, 4);
        assert_eq!(recovered, 4, "le DRED n'a pas tout reconstruit : {recovered}/4");
        let mut out = [0f32; FRAME_SAMPLES];
        assert!(rx.playout().lock().unwrap().mix_into(&mut out, 1.0));
    }

    /// Le rappel de sortie tient sa poignée de tampon **avant** que le fil
    /// réseau ne décode quoi que ce soit : ce qui est décodé ensuite doit lui
    /// parvenir par cette poignée-là. C'est tout le câblage de la séparation.
    #[test]
    fn what_the_decoder_produces_reaches_the_handle_taken_earlier() {
        let mut enc = new_encoder();
        let mut rx = Receiver::new();
        // La poignée est prise d'emblée, comme le fait le rappel de sortie.
        let playout = rx.playout();
        assert_eq!(playout.lock().unwrap().ready.len(), 0);

        for seq in 0..4 {
            rx.push(seq, &encoded_frame(&mut enc, 0.1));
        }
        assert_eq!(playout.lock().unwrap().ready.len(), 4 * FRAME_SAMPLES);

        // Et consommer par cette poignée retire bien du tampon partagé.
        let mut out = [0f32; FRAME_SAMPLES];
        assert!(playout.lock().unwrap().mix_into(&mut out, 1.0));
        assert_eq!(playout.lock().unwrap().ready.len(), 3 * FRAME_SAMPLES);
    }

    /// Le décodage ne doit pas se faire sous le verrou du tampon : c'est toute
    /// la raison d'être de la séparation. On le vérifie en gardant ce verrou
    /// depuis un autre fil pendant qu'une trame est décodée — `push` doit
    /// travailler quand même, et n'attendre qu'au moment de déposer.
    #[test]
    fn decoding_does_not_happen_under_the_playout_lock() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let mut enc = new_encoder();
        let mut rx = Receiver::new();
        let playout = rx.playout();
        rx.push(0, &encoded_frame(&mut enc, 0.1));

        let held = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let keeper = {
            let (playout, held, release) = (playout.clone(), held.clone(), release.clone());
            std::thread::spawn(move || {
                let _guard = playout.lock().unwrap();
                held.store(true, Ordering::SeqCst);
                while !release.load(Ordering::SeqCst) {
                    std::thread::yield_now();
                }
            })
        };
        while !held.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }

        // Le verrou est tenu ailleurs. `push` décode malgré tout ; il ne peut
        // se bloquer qu'au dépôt, que l'on débloque aussitôt.
        let frame = encoded_frame(&mut enc, 0.2);
        let pusher = std::thread::spawn(move || {
            rx.push(1, &frame);
            rx
        });
        std::thread::sleep(std::time::Duration::from_millis(30));
        release.store(true, Ordering::SeqCst);
        keeper.join().unwrap();
        let _rx = pusher.join().unwrap();

        // Les deux trames ont atteint le tampon.
        assert_eq!(playout.lock().unwrap().ready.len(), 2 * FRAME_SAMPLES);
    }

    /// Le compteur d'émission repart au hasard (c'est un nonce) : s'il
    /// retombe juste derrière notre séquence, l'ancien code ignorait chaque
    /// paquet du nouveau flux comme un « retardataire » — des secondes de
    /// mutisme. La série continue doit forcer la resynchronisation.
    #[test]
    fn sender_restarting_just_behind_resyncs() {
        let mut enc = new_encoder();
        let mut rx = Receiver::new();
        rx.push(1000, &encoded_frame(&mut enc, 0.1));
        for i in 0..12u16 {
            rx.push(950u16.wrapping_add(i), &encoded_frame(&mut enc, 0.2));
        }
        let mut out = [0f32; FRAME_SAMPLES];
        let mut mixed = false;
        for _ in 0..4 {
            mixed |= rx.playout().lock().unwrap().mix_into(&mut out, 1.0);
        }
        assert!(mixed, "l'émetteur reparti juste derrière est resté muet");
    }

    /// Un trou de 2 s ne doit ni synthétiser 100 trames de Deep PLC (jetées
    /// aussitôt par l'écrêtage de latence) ni épingler le tampon au plafond :
    /// on saute au bord du trou, seule la couture est dissimulée.
    #[test]
    fn deep_gap_is_skipped_not_synthesized() {
        let mut enc = new_encoder();
        let mut rx = Receiver::new();
        rx.push(0, &encoded_frame(&mut enc, 0.1));
        let mut total_lost = 0u64;
        for seq in 100..108u16 {
            let (l, _) = rx.push(seq, &encoded_frame(&mut enc, 0.1));
            total_lost += l;
        }
        assert_eq!(total_lost, 99, "toutes les trames du trou comptent comme perdues");
        let ready = rx.playout().lock().unwrap().ready.len();
        assert!(
            ready <= 25 * FRAME_SAMPLES,
            "le trou a été synthétisé : {} trames en tampon",
            ready / FRAME_SAMPLES
        );
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
        assert!(rx.playout().lock().unwrap().mix_into(&mut out, 1.0));
    }
}
