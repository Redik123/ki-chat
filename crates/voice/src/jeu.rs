//! Le son du jeu dans le stream.
//!
//! Côté streamer : la boucle de tout ce que joue le système **sauf ki-chat**
//! (les spectateurs n'entendent donc pas leurs propres voix en retour),
//! Opus stéréo en mode « audio », un paquet par 20 ms remis à la couche
//! réseau, qui le chiffre et l'envoie en datagramme. Côté spectateur : le
//! lecteur, qui décode, masque les trous, ramène le son au niveau des voix,
//! et le verse dans la sortie du moteur vocal — même volume général, même
//! annulateur d'écho.

use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(windows)]
use std::sync::mpsc;
use std::sync::Arc;
#[cfg(windows)]
use std::time::Duration;

#[cfg(windows)]
use anyhow::Context;
#[cfg(windows)]
use ki_opus::{Application, Bitrate, Encoder};
use ki_opus::{Channels, Decoder};

#[cfg(windows)]
use crate::{journal, wasapi};
use crate::{VoiceEngine, SAMPLE_RATE};

/// Un paquet Opus encodé et son horodatage (µs depuis le début), à emporter.
pub type PaquetAudio = Arc<dyn Fn(&[u8], u64) + Send + Sync>;

/// Échantillons par trame et par canal : 20 ms à 48 kHz.
const TRAME: usize = (SAMPLE_RATE / 50) as usize;

/// La capture et l'encodage du son du jeu, tant que la poignée vit.
///
/// La boucle « tout le système sauf ce processus » est une capacité de
/// WASAPI : hors Windows, `start` refuse en le disant, et la diffusion
/// part sans son — la vidéo, elle, n'en dépend pas.
#[cfg_attr(not(windows), allow(dead_code))]
pub struct GameAudio {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl GameAudio {
    /// Démarre la boucle (tout le système sauf ce processus) et l'encodage
    /// à `bitrate` bits/s ; chaque paquet part par `emettre`, horodaté
    /// depuis `origine` — la même que la vidéo, c'est ce qui permet au
    /// spectateur de les remettre ensemble.
    #[cfg(not(windows))]
    pub fn start(
        _bitrate: i32,
        _emettre: PaquetAudio,
        _origine: std::time::Instant,
    ) -> anyhow::Result<Self> {
        anyhow::bail!(
            "la capture du son du jeu n'existe que sous Windows (boucle WASAPI par processus)"
        )
    }

    #[cfg(windows)]
    pub fn start(
        bitrate: i32,
        emettre: PaquetAudio,
        origine: std::time::Instant,
    ) -> anyhow::Result<Self> {
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
                // Taillée pour qu'un paquet — en-tête et tag compris — tienne
                // dans un datagramme au MTU initial de QUIC : au-delà, il
                // n'aurait pas quitté la machine.
                let mut sortie = vec![
                    0u8;
                    ki_protocol::DATAGRAMME_SUR - ki_protocol::AUDIO_HEADER_LEN - 16
                ];
                while !stop_thread.load(Ordering::Relaxed) {
                    // L'instant (µs depuis l'origine) où finit ce qui attend
                    // dans `accum` : le dernier bloc reçu vient d'être capturé.
                    let fin_us = match rx.recv_timeout(Duration::from_millis(200)) {
                        Ok(bloc) => {
                            accum.extend_from_slice(&bloc);
                            origine.elapsed().as_micros() as u64
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if !alive.load(Ordering::Relaxed) {
                                journal("son du jeu : la capture s'est arrêtée".into());
                                return;
                            }
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    };
                    // Une trame Opus par 20 ms de stéréo entrelacée. Son
                    // horodatage : la fin de l'attente vaut « maintenant »,
                    // chaque trame remonte d'autant (stéréo : deux
                    // échantillons par pas de temps).
                    while accum.len() >= TRAME * 2 {
                        let pts_us = fin_us
                            .saturating_sub(accum.len() as u64 * 1_000_000 / (2 * SAMPLE_RATE as u64));
                        let trame: Vec<f32> = accum.drain(..TRAME * 2).collect();
                        match enc.encode_float(&trame, &mut sortie) {
                            Ok(n) => emettre(&sortie[..n], pts_us),
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

/// Le son du système, brut : la même boucle « tout sauf ki-chat », mais
/// remise telle quelle (float 48 kHz stéréo entrelacé) à `recevoir`, sans
/// encodage — c'est la piste « jeu » d'un clip (PLAN-CLIPS.md, C1). Vit
/// tant que la poignée vit.
#[cfg_attr(not(windows), allow(dead_code))]
pub struct SonSysteme {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SonSysteme {
    #[cfg(not(windows))]
    pub fn start(_recevoir: crate::Robinet) -> anyhow::Result<Self> {
        anyhow::bail!("la capture du son du système n'existe que sous Windows (boucle WASAPI par processus)")
    }

    #[cfg(windows)]
    pub fn start(recevoir: crate::Robinet) -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::sync_channel::<Vec<f32>>(64);
        let alive = Arc::new(AtomicBool::new(true));
        let flux = wasapi::open_loopback(std::process::id(), tx, alive.clone())
            .context("capture du son du système")?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let thread = std::thread::Builder::new()
            .name("son-systeme".into())
            .spawn(move || {
                let _flux = flux;
                while !stop_thread.load(Ordering::Relaxed) {
                    match rx.recv_timeout(Duration::from_millis(200)) {
                        Ok(bloc) => recevoir(&bloc),
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if !alive.load(Ordering::Relaxed) {
                                journal("son du système : la capture s'est arrêtée".into());
                                return;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            })
            .context("fil du son du système")?;
        Ok(Self { stop, thread: Some(thread) })
    }
}

impl Drop for SonSysteme {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// La sonie visée pour le son du jeu, en dBFS efficaces : celle d'une voix
/// réglée par défaut (crêtes vers -10 dB, l'efficace une dizaine de dB
/// dessous), un rien en dessous — qu'on comprenne encore qui parle
/// par-dessus.
const SONIE_VISEE_DB: f32 = -22.0;
/// Le gain se tient entre ces bornes : au-delà de +18 dB, on ne relèverait
/// plus que du souffle.
const GAIN_MAX_DB: f32 = 18.0;
const GAIN_MIN_DB: f32 = -12.0;
/// Sous ce niveau, un bloc est un silence : il ne fait pas bouger la mesure.
const SILENCE_DB: f32 = -60.0;
/// La sonie se moyenne sur environ 3 s, en dB : une explosion ne fait pas
/// plonger le reste.
const MOYENNE_S: f32 = 3.0;
/// La vitesse du gain : il monte doucement (pas de pompage entre deux
/// bruits) et descend plus vite (pas de mauvaise surprise quand le jeu
/// s'emballe) ; trois fois plus vite au début, le temps de trouver le
/// niveau.
const MONTEE_DB_S: f32 = 2.0;
const DESCENTE_DB_S: f32 = 6.0;
const DEMARRAGE_S: f32 = 3.0;
/// Le plafond des crêtes : -1 dBFS.
const PLAFOND: f32 = 0.891;

/// Le son du jeu ramené au niveau des voix. Chez le streamer, il sort au
/// volume que le jeu a chez lui : un Valorant réglé à 40 % arrivait à 40 %
/// chez tous ses spectateurs, bien sous les voix — on montait le stream à
/// 200 %, et ça ne suffisait pas. Un gain lent suit la sonie de ce que joue
/// le stream, silences mis à part ; un limiteur, lié sur les deux côtés,
/// tient les crêtes. Le curseur du spectateur règle ensuite autour de ce
/// niveau.
#[derive(Clone, Debug)]
pub struct NiveauJeu {
    gain_db: f32,
    /// La sonie mesurée (dBFS efficaces), moyennée ; `None` avant le
    /// premier bloc qui ne soit pas un silence.
    sonie_db: Option<f32>,
    duree_s: f32,
    gain_limiteur: f32,
    relachement: f32,
}

impl Default for NiveauJeu {
    fn default() -> Self {
        Self::new()
    }
}

impl NiveauJeu {
    pub fn new() -> Self {
        Self {
            gain_db: 0.0,
            sonie_db: None,
            duree_s: 0.0,
            gain_limiteur: 1.0,
            // 60 ms pour relâcher.
            relachement: 1.0 - (-1.0 / (0.06 * SAMPLE_RATE as f32)).exp(),
        }
    }

    /// Le gain appliqué en ce moment, en dB.
    pub fn gain_db(&self) -> f32 {
        self.gain_db
    }

    /// Un bloc stéréo entrelacé, traité sur place.
    pub fn traiter(&mut self, pcm: &mut [f32]) {
        let n = pcm.len() / 2;
        if n == 0 {
            return;
        }
        let dt = n as f32 / SAMPLE_RATE as f32;
        let energie = pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len() as f32;
        let bloc_db = 10.0 * (energie + 1e-20).log10();
        if bloc_db > SILENCE_DB {
            let alpha = (dt / MOYENNE_S).min(1.0);
            self.sonie_db = Some(match self.sonie_db {
                None => bloc_db,
                Some(s) => s + alpha * (bloc_db - s),
            });
        }
        let gain_avant = self.gain_db;
        if let Some(sonie) = self.sonie_db {
            let voulu = (SONIE_VISEE_DB - sonie).clamp(GAIN_MIN_DB, GAIN_MAX_DB);
            let vite = if self.duree_s < DEMARRAGE_S { 3.0 } else { 1.0 };
            let pas = if voulu > self.gain_db { MONTEE_DB_S } else { DESCENTE_DB_S } * vite * dt;
            self.gain_db += (voulu - self.gain_db).clamp(-pas, pas);
        }
        self.duree_s += dt;
        // Le gain glisse le long du bloc (pas de marche audible), puis le
        // limiteur : attaque immédiate, sur la plus forte des deux voies.
        let (g0, g1) = (10f32.powf(gain_avant / 20.0), 10f32.powf(self.gain_db / 20.0));
        for i in 0..n {
            let g = g0 + (g1 - g0) * (i as f32 / n as f32);
            let (gauche, droite) = (pcm[2 * i] * g, pcm[2 * i + 1] * g);
            let crete = gauche.abs().max(droite.abs());
            let voulu = if crete > PLAFOND { PLAFOND / crete } else { 1.0 };
            if voulu < self.gain_limiteur {
                self.gain_limiteur = voulu;
            } else {
                self.gain_limiteur += (voulu - self.gain_limiteur) * self.relachement;
            }
            pcm[2 * i] = gauche * self.gain_limiteur;
            pcm[2 * i + 1] = droite * self.gain_limiteur;
        }
    }
}

/// Le lecteur du spectateur : Opus stéréo → la sortie du moteur, en
/// stéréo — le jeu à gauche et à droite comme chez le streamer.
pub struct Lecteur {
    dec: Decoder,
    dernier: Option<u64>,
    pcm: Vec<f32>,
    niveau: NiveauJeu,
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
            niveau: NiveauJeu::new(),
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

    /// `n` trames décodées : au niveau des voix, puis vers le moteur,
    /// stéréo entrelacé.
    fn pousser(&mut self, n: usize, engine: &VoiceEngine) {
        let n = n.min(self.pcm.len() / 2);
        self.niveau.traiter(&mut self.pcm[..n * 2]);
        engine.aux_push(&self.pcm[..n * 2]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ki_opus::{Application, Bitrate, Encoder};
    #[cfg(windows)]
    use std::sync::mpsc;
    use std::time::Duration;

    /// La boucle par processus s'ouvre sur cette machine — ou dit pourquoi
    /// pas (pas de périphérique de sortie, Windows trop ancien). On ne
    /// demande pas de son : rien ne joue pendant les tests.
    #[test]
    #[cfg(windows)]
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

    /// Un son de jeu stéréo à `niveau_db` dBFS efficaces : deux bruits
    /// colorés, un par côté, pour `secondes`.
    fn son_de_jeu(niveau_db: f32, secondes: f32) -> Vec<f32> {
        let n = (secondes * SAMPLE_RATE as f32) as usize;
        let mut etat = 0x1234_5678u32;
        let mut alea = || {
            etat ^= etat << 13;
            etat ^= etat >> 17;
            etat ^= etat << 5;
            etat as f32 / u32::MAX as f32 * 2.0 - 1.0
        };
        let (mut g, mut d) = (0f32, 0f32);
        let mut x = Vec::with_capacity(2 * n);
        for _ in 0..n {
            // Un bruit adouci (passe-bas d'ordre un) : plus proche d'un mixage de jeu.
            g += 0.2 * (alea() - g);
            d += 0.2 * (alea() - d);
            x.push(g);
            x.push(d);
        }
        let efficace = (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt();
        let voulu = 10f32.powf(niveau_db / 20.0);
        x.iter().map(|s| s * voulu / efficace).collect()
    }

    fn efficace_db(x: &[f32]) -> f32 {
        10.0 * (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).log10()
    }

    /// Bloc par bloc de 20 ms, comme le lecteur.
    fn normaliser(niveau: &mut NiveauJeu, x: &mut [f32]) {
        for bloc in x.chunks_mut(TRAME * 2) {
            niveau.traiter(bloc);
        }
    }

    #[test]
    fn un_stream_trop_faible_remonte_au_niveau_des_voix() {
        // Un jeu réglé bas chez le streamer : -40 dBFS.
        let mut x = son_de_jeu(-40.0, 12.0);
        let mut n = NiveauJeu::new();
        normaliser(&mut n, &mut x);
        let fin = &x[x.len() - 2 * SAMPLE_RATE as usize * 2..];
        let db = efficace_db(fin);
        // +18 dB au plus : -40 remonte à -22, la sonie visée.
        assert!((db - SONIE_VISEE_DB).abs() < 1.5, "{db:.1} dBFS");
    }

    #[test]
    fn un_stream_trop_fort_redescend_sans_crete() {
        let mut x = son_de_jeu(-12.0, 12.0);
        let mut n = NiveauJeu::new();
        normaliser(&mut n, &mut x);
        let fin = &x[x.len() - 2 * SAMPLE_RATE as usize * 2..];
        assert!((efficace_db(fin) - SONIE_VISEE_DB).abs() < 1.5, "{:.1} dBFS", efficace_db(fin));
        assert!(x.iter().all(|s| s.abs() <= PLAFOND + 1e-4));
    }

    #[test]
    fn une_explosion_ne_depasse_pas_le_plafond() {
        // Un jeu calme (le gain monte), puis une explosion à pleine échelle.
        let mut calme = son_de_jeu(-38.0, 8.0);
        let mut n = NiveauJeu::new();
        normaliser(&mut n, &mut calme);
        assert!(n.gain_db() > 10.0, "gain {:.1} dB", n.gain_db());
        let mut boum = son_de_jeu(-3.0, 0.5);
        normaliser(&mut n, &mut boum);
        assert!(boum.iter().all(|s| s.abs() <= PLAFOND + 1e-4));
    }

    #[test]
    fn le_silence_ne_fait_pas_monter_le_gain() {
        let mut x = vec![0f32; 2 * SAMPLE_RATE as usize * 5];
        let mut n = NiveauJeu::new();
        normaliser(&mut n, &mut x);
        assert_eq!(n.gain_db(), 0.0);
        assert!(x.iter().all(|s| *s == 0.0));
        // Un jeu, puis un long silence : le gain reste où il était.
        let mut jeu = son_de_jeu(-30.0, 6.0);
        normaliser(&mut n, &mut jeu);
        let gain = n.gain_db();
        let mut silence = vec![0f32; 2 * SAMPLE_RATE as usize * 5];
        normaliser(&mut n, &mut silence);
        assert!((n.gain_db() - gain).abs() < 0.5, "{gain:.1} → {:.1} dB", n.gain_db());
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
