//! La file « médias » : le son d'une vidéo lue dans ki-chat (PLAN-CLIPS.md, C0).
//!
//! Le fil de lecture de la visionneuse y dépose du mono 48 kHz, quelques
//! centaines de millisecondes d'avance. Ce qui la vide est **soit** le rappel
//! de sortie du moteur vocal — en salon : même volume général, même limiteur,
//! et l'annulateur d'écho voit ce qui sort, donc les copains n'entendent pas
//! la vidéo revenir par le micro —, **soit** une sortie à part, quand il n'y a
//! pas de moteur (hors salon : on regarde un clip sans être en vocal). Un seul
//! consommateur à la fois, désigné par l'application ; sinon les deux se
//! partageraient les échantillons et l'on entendrait un hachis.
//!
//! Le compteur d'échantillons consommés est **l'horloge de la vidéo** : une
//! image s'affiche quand le son qui l'accompagne est réellement parti vers la
//! carte, pas quand on l'a déposé.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::resample::CubicResampler;
use crate::{FRAME_SAMPLES, SAMPLE_RATE};

/// Qui a le droit de puiser dans la file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Consommateur {
    /// Personne : la file se remplit sans se vider (une vidéo en pause, ou
    /// entre deux consommateurs).
    Personne = 0,
    /// Le rappel de sortie du moteur vocal.
    Moteur = 1,
    /// La sortie à part, hors salon.
    Seule = 2,
}

/// Au-delà d'une seconde d'avance, on refuse : le lecteur n'est pas censé
/// déposer plus, et une file qui grossit sans fin serait une fuite.
const AVANCE_MAX: usize = SAMPLE_RATE as usize;

pub struct File {
    buf: Mutex<VecDeque<f32>>,
    consomme: AtomicU64,
    gain: AtomicU32,
    qui: AtomicU8,
    /// En pause : la file garde ce qu'elle a, personne n'y puise, l'horloge
    /// s'arrête — c'est exactement ce qu'une vidéo en pause demande.
    pause: AtomicBool,
}

impl std::fmt::Debug for File {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "File(médias, {} en attente)", self.en_attente())
    }
}

impl Default for File {
    fn default() -> Self {
        Self {
            buf: Mutex::new(VecDeque::new()),
            consomme: AtomicU64::new(0),
            gain: AtomicU32::new(1.0f32.to_bits()),
            qui: AtomicU8::new(Consommateur::Personne as u8),
            pause: AtomicBool::new(true),
        }
    }
}

impl File {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Dépose du mono 48 kHz. Ce qui dépasserait une seconde d'avance est
    /// ignoré et rendu en nombre d'échantillons refusés.
    pub fn pousser(&self, mono: &[f32]) -> usize {
        let mut buf = self.buf.lock().unwrap();
        let place = AVANCE_MAX.saturating_sub(buf.len());
        let pris = mono.len().min(place);
        buf.extend(mono[..pris].iter().copied());
        mono.len() - pris
    }

    /// Échantillons déposés et pas encore joués.
    pub fn en_attente(&self) -> usize {
        self.buf.lock().unwrap().len()
    }

    /// Jette ce qui attend et remet l'horloge à zéro (recherche, nouveau
    /// fichier).
    pub fn vider(&self) {
        self.buf.lock().unwrap().clear();
        self.consomme.store(0, Ordering::Relaxed);
    }

    /// Échantillons réellement partis vers la carte depuis le dernier
    /// `vider` : l'horloge, en 48 000ᵉ de seconde.
    pub fn consommes(&self) -> u64 {
        self.consomme.load(Ordering::Relaxed)
    }

    /// Volume propre à la vidéo (1.0 = 100 %).
    pub fn set_gain(&self, gain: f32) {
        self.gain
            .store(gain.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
    }

    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }

    /// Pause : ce qui attend reste en attente, l'horloge ne bouge plus.
    pub fn set_pause(&self, pause: bool) {
        self.pause.store(pause, Ordering::Relaxed);
    }

    pub fn en_pause(&self) -> bool {
        self.pause.load(Ordering::Relaxed)
    }

    pub fn set_consommateur(&self, qui: Consommateur) {
        self.qui.store(qui as u8, Ordering::Relaxed);
    }

    pub fn consommateur(&self) -> Consommateur {
        match self.qui.load(Ordering::Relaxed) {
            1 => Consommateur::Moteur,
            2 => Consommateur::Seule,
            _ => Consommateur::Personne,
        }
    }

    /// Ajoute à `mix` (avec le gain) ce qui attend, si `qui` est le
    /// consommateur désigné. Rend le nombre d'échantillons pris. Verrou
    /// bref, jamais derrière un décodage : c'est un chemin temps réel.
    pub fn mixer_dans(&self, mix: &mut [f32], qui: Consommateur) -> usize {
        if self.consommateur() != qui || self.en_pause() {
            return 0;
        }
        let gain = self.gain();
        let mut pris = 0usize;
        {
            let mut buf = self.buf.lock().unwrap();
            for o in mix.iter_mut() {
                match buf.pop_front() {
                    Some(s) => {
                        *o += s * gain;
                        pris += 1;
                    }
                    None => break,
                }
            }
        }
        if pris > 0 {
            self.consomme.fetch_add(pris as u64, Ordering::Relaxed);
        }
        pris
    }
}

/// La sortie à part : ouvre le périphérique de sortie (natif ou cpal) et y
/// joue la file — et rien d'autre. Vit tant que la poignée vit.
pub struct SortieSeule {
    stop: Arc<AtomicBool>,
    fil: Option<std::thread::JoinHandle<()>>,
}

impl SortieSeule {
    /// `peripherique` et `natif` : les mêmes réglages que le moteur vocal,
    /// pour que la vidéo sorte au même endroit que les voix.
    pub fn demarrer(
        file: Arc<File>,
        peripherique: Option<String>,
        natif: bool,
        robuste: bool,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_fil = stop.clone();
        let fil = std::thread::Builder::new()
            .name("medias-sortie".into())
            .spawn(move || boucle(file, peripherique, natif, robuste, stop_fil))
            .ok();
        Self { stop, fil }
    }
}

impl Drop for SortieSeule {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(f) = self.fil.take() {
            let _ = f.join();
        }
    }
}

/// Ce que le périphérique tient ouvert, quel que soit le moteur.
enum Flux {
    Cpal(#[allow(dead_code)] cpal::Stream),
    #[cfg(windows)]
    Natif(#[allow(dead_code)] crate::wasapi::NativeStream),
}

fn boucle(
    file: Arc<File>,
    peripherique: Option<String>,
    natif: bool,
    robuste: bool,
    stop: Arc<AtomicBool>,
) {
    let mut avertie = false;
    while !stop.load(Ordering::Relaxed) {
        let vivant = Arc::new(AtomicBool::new(true));
        let flux = match ouvrir(
            &file,
            peripherique.as_deref(),
            natif,
            robuste,
            vivant.clone(),
        ) {
            Ok(f) => {
                avertie = false;
                f
            }
            Err(e) => {
                if !avertie {
                    avertie = true;
                    tracing::warn!("sortie médias indisponible : {e:#} — nouvelle tentative");
                    crate::journal(format!("sortie médias indisponible : {e:#}"));
                }
                attendre(&stop, Duration::from_secs(1));
                continue;
            }
        };
        // Le flux vit tant que la carte réclame ; un casque débranché le
        // fait tomber, et l'on rouvre.
        while !stop.load(Ordering::Relaxed) && vivant.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }
        drop(flux);
    }
}

fn attendre(stop: &AtomicBool, duree: Duration) {
    let debut = std::time::Instant::now();
    while !stop.load(Ordering::Relaxed) && debut.elapsed() < duree {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Le fournisseur d'échantillons : `n` échantillons mono à la cadence du
/// périphérique, tirés de la file à 48 kHz et rééchantillonnés.
fn ecrivain(file: Arc<File>, cadence: u32) -> impl FnMut(&mut [f32]) + Send + 'static {
    let mut reech = CubicResampler::new(SAMPLE_RATE as f64 / cadence.max(1) as f64);
    move |out: &mut [f32]| {
        while !reech.can_pull(out.len()) {
            let mut mix = [0f32; FRAME_SAMPLES];
            file.mixer_dans(&mut mix, Consommateur::Seule);
            for o in mix.iter_mut() {
                *o = crate::soft_clip(*o);
            }
            reech.push(&mix);
        }
        reech.pull(out);
    }
}

fn ouvrir(
    file: &Arc<File>,
    peripherique: Option<&str>,
    natif: bool,
    robuste: bool,
    vivant: Arc<AtomicBool>,
) -> anyhow::Result<Flux> {
    use anyhow::Context as _;
    use cpal::traits::{DeviceTrait as _, StreamTrait as _};
    #[cfg(windows)]
    if natif {
        let f = file.clone();
        match crate::wasapi::open_output(
            peripherique,
            move |cadence| ecrivain(f, cadence),
            vivant.clone(),
            robuste,
        ) {
            Ok((flux, _repli)) => return Ok(Flux::Natif(flux)),
            Err(e) => tracing::warn!("sortie médias native : {e:#} — repli cpal"),
        }
    }
    #[cfg(not(windows))]
    let _ = (natif, robuste);
    let hote = cpal::default_host();
    let (device, _repli) = crate::pick_device(&hote, peripherique, false);
    let device = device.context("aucun périphérique de sortie audio")?;
    let supporte = device.default_output_config().context("config sortie")?;
    let cadence = supporte.sample_rate().0;
    let voies = supporte.channels() as usize;
    let flux = crate::build_output_stream(
        &device,
        &supporte,
        voies,
        ecrivain(file.clone(), cadence),
        vivant,
    )?;
    flux.play()?;
    Ok(Flux::Cpal(flux))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seul_le_consommateur_designe_puise() {
        let f = File::new();
        f.set_pause(false);
        f.pousser(&[0.5; 100]);
        let mut mix = [0f32; 10];
        assert_eq!(f.mixer_dans(&mut mix, Consommateur::Moteur), 0);
        assert_eq!(f.consommes(), 0);
        f.set_consommateur(Consommateur::Moteur);
        assert_eq!(f.mixer_dans(&mut mix, Consommateur::Seule), 0);
        assert_eq!(f.mixer_dans(&mut mix, Consommateur::Moteur), 10);
        assert_eq!(f.consommes(), 10);
        assert_eq!(f.en_attente(), 90);
        assert!(mix.iter().all(|v| (v - 0.5).abs() < 1e-6));
    }

    #[test]
    fn le_gain_s_applique_et_le_mix_s_ajoute() {
        let f = File::new();
        f.set_pause(false);
        f.set_consommateur(Consommateur::Seule);
        f.set_gain(0.5);
        f.pousser(&[1.0; 4]);
        let mut mix = [0.25f32; 4];
        f.mixer_dans(&mut mix, Consommateur::Seule);
        assert!(mix.iter().all(|v| (v - 0.75).abs() < 1e-6));
    }

    #[test]
    fn l_avance_est_bornee_et_vider_remet_l_horloge() {
        let f = File::new();
        f.set_pause(false);
        f.set_consommateur(Consommateur::Moteur);
        let refuse = f.pousser(&vec![0.0; AVANCE_MAX + 10]);
        assert_eq!(refuse, 10);
        assert_eq!(f.en_attente(), AVANCE_MAX);
        let mut mix = [0f32; FRAME_SAMPLES];
        f.mixer_dans(&mut mix, Consommateur::Moteur);
        assert_eq!(f.consommes(), FRAME_SAMPLES as u64);
        f.vider();
        assert_eq!((f.en_attente(), f.consommes()), (0, 0));
    }

    #[test]
    fn une_file_vide_ne_change_pas_le_mix() {
        let f = File::new();
        f.set_pause(false);
        f.set_consommateur(Consommateur::Moteur);
        let mut mix = [0.1f32; 8];
        assert_eq!(f.mixer_dans(&mut mix, Consommateur::Moteur), 0);
        assert!(mix.iter().all(|v| (v - 0.1).abs() < 1e-6));
    }

    #[test]
    fn en_pause_rien_ne_sort_et_l_horloge_ne_bouge_pas() {
        let f = File::new();
        f.set_consommateur(Consommateur::Moteur);
        f.pousser(&[0.5; 20]);
        let mut mix = [0f32; 10];
        assert!(f.en_pause());
        assert_eq!(f.mixer_dans(&mut mix, Consommateur::Moteur), 0);
        assert_eq!(f.consommes(), 0);
        f.set_pause(false);
        assert_eq!(f.mixer_dans(&mut mix, Consommateur::Moteur), 10);
        assert_eq!(f.consommes(), 10);
    }
}
