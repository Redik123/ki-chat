//! L'enregistreur de clips : les trente dernières secondes, à la touche
//! (PLAN-CLIPS.md, jalon C1).
//!
//! Tant qu'il tourne, la capture du partage d'écran et son encodeur (NVENC,
//! GOP d'une seconde) tournent aussi, mais rien ne part sur le réseau : les
//! trames encodées vont dans un **tampon circulaire** en mémoire, coupé à la
//! trame clé, qui ne garde que les N dernières secondes. À côté, trois
//! anneaux de son : le système sauf ki-chat (le jeu), le micro traité, le
//! mélange des copains — les deux derniers par les robinets du moteur vocal,
//! quand il est là. À l'appui, le tampon est photographié et un fil à part
//! écrit le MP4 (H.264 tel quel, AAC par piste : le mélange d'abord, pour
//! les lecteurs ordinaires, puis chaque source), sa vignette et sa fiche.
//!
//! Rien ne quitte la machine : la galerie est locale, le partage est un geste
//! (jalon C2).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use ki_media::annexb;
use ki_video::{CaptureSource, EncodedFrame, EncoderChoice, StageStats, StreamConfig, StreamerLoop};

use crate::ptt::Raccourci;

// ---------------------------------------------------------------------
// Réglages
// ---------------------------------------------------------------------

/// Le débit et la définition : ce que l'on est prêt à payer en mémoire, en
/// disque et en carte graphique.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Qualite {
    /// 1080p, 12 Mbit/s : 45 Mo pour 30 s.
    Equilibre,
    /// 1080p, 20 Mbit/s : pour les écrans très chargés.
    Haute,
    /// 720p, 8 Mbit/s : pour les petites machines.
    Legere,
}

impl Qualite {
    pub const TOUTES: [Qualite; 3] = [Qualite::Equilibre, Qualite::Haute, Qualite::Legere];

    pub fn label(self) -> &'static str {
        match self {
            Qualite::Equilibre => "Équilibrée — 1080p, 12 Mbit/s",
            Qualite::Haute => "Haute — 1080p, 20 Mbit/s",
            Qualite::Legere => "Légère — 720p, 8 Mbit/s",
        }
    }

    pub fn debit_bps(self) -> u32 {
        match self {
            Qualite::Equilibre => 12_000_000,
            Qualite::Haute => 20_000_000,
            Qualite::Legere => 8_000_000,
        }
    }

    pub fn hauteur_max(self) -> u32 {
        match self {
            Qualite::Legere => 720,
            _ => 1080,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Qualite::Equilibre => "equilibree",
            Qualite::Haute => "haute",
            Qualite::Legere => "legere",
        }
    }

    fn depuis(id: &str) -> Self {
        Self::TOUTES.into_iter().find(|q| q.id() == id).unwrap_or(Qualite::Equilibre)
    }
}

/// Ce que l'on filme.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Source {
    /// La fenêtre du jeu quand on la reconnaît, sinon l'écran principal.
    Auto,
    Ecran(usize),
    Fenetre(String),
}

impl Source {
    fn id(&self) -> String {
        match self {
            Source::Auto => "auto".into(),
            Source::Ecran(n) => format!("ecran:{n}"),
            Source::Fenetre(t) => format!("fenetre:{t}"),
        }
    }

    fn depuis(id: &str) -> Self {
        if let Some(n) = id.strip_prefix("ecran:") {
            Source::Ecran(n.parse().unwrap_or(0))
        } else if let Some(t) = id.strip_prefix("fenetre:") {
            Source::Fenetre(t.to_string())
        } else {
            Source::Auto
        }
    }
}

/// Les durées de tampon proposées, en secondes.
pub const DUREES: [u32; 4] = [15, 30, 60, 120];

/// Les réglages de l'enregistreur, persistés dans le stockage eframe.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Reglages {
    /// Démarrer l'enregistreur avec ki-chat.
    pub au_demarrage: bool,
    pub raccourci: Raccourci,
    pub duree_s: u32,
    pub qualite: Qualite,
    pub fps: u32,
    pub source: Source,
    /// Les pistes : le son du jeu (tout le système sauf ki-chat), son
    /// propre micro, les copains.
    pub jeu: bool,
    pub micro: bool,
    pub copains: bool,
    /// Le dossier des clips ; `None` = `Vidéos\ki-chat`.
    pub dossier: Option<PathBuf>,
    /// Un son de confirmation à l'appui.
    pub son: bool,
}

impl Default for Reglages {
    fn default() -> Self {
        Self {
            au_demarrage: false,
            raccourci: Raccourci::DEFAUT,
            duree_s: 30,
            qualite: Qualite::Equilibre,
            fps: 60,
            source: Source::Auto,
            jeu: true,
            micro: true,
            copains: true,
            dossier: None,
            son: true,
        }
    }
}

impl Reglages {
    pub fn load(get: impl Fn(&str, &str) -> String) -> Self {
        let dossier = get("clips_dossier", "");
        Self {
            au_demarrage: get("clips_auto", "off") == "on",
            raccourci: Raccourci::depuis(&get("clips_raccourci", &Raccourci::DEFAUT.id()))
                .unwrap_or(Raccourci::DEFAUT),
            duree_s: get("clips_duree", "30").parse().ok().filter(|d| DUREES.contains(d)).unwrap_or(30),
            qualite: Qualite::depuis(&get("clips_qualite", "equilibree")),
            fps: get("clips_fps", "60").parse().ok().filter(|f| [30, 60].contains(f)).unwrap_or(60),
            source: Source::depuis(&get("clips_source", "auto")),
            jeu: get("clips_jeu", "on") == "on",
            micro: get("clips_micro", "on") == "on",
            copains: get("clips_copains", "on") == "on",
            dossier: (!dossier.is_empty()).then(|| PathBuf::from(dossier)),
            son: get("clips_son", "on") == "on",
        }
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        let bit = |b: bool| if b { "on" } else { "off" }.to_string();
        storage.set_string("clips_auto", bit(self.au_demarrage));
        storage.set_string("clips_raccourci", self.raccourci.id());
        storage.set_string("clips_duree", self.duree_s.to_string());
        storage.set_string("clips_qualite", self.qualite.id().into());
        storage.set_string("clips_fps", self.fps.to_string());
        storage.set_string("clips_source", self.source.id());
        storage.set_string("clips_jeu", bit(self.jeu));
        storage.set_string("clips_micro", bit(self.micro));
        storage.set_string("clips_copains", bit(self.copains));
        storage.set_string(
            "clips_dossier",
            self.dossier.as_ref().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default(),
        );
        storage.set_string("clips_son", bit(self.son));
    }

    /// Le dossier où vont les clips, créé au besoin.
    pub fn dossier_effectif(&self) -> PathBuf {
        let d = self.dossier.clone().unwrap_or_else(dossier_par_defaut);
        let _ = std::fs::create_dir_all(&d);
        d
    }

    /// Ce qui, changé, oblige à relancer l'enregistreur (le reste se lit à
    /// l'appui).
    pub fn relance_necessaire(&self, autre: &Reglages) -> bool {
        self.duree_s != autre.duree_s
            || self.qualite != autre.qualite
            || self.fps != autre.fps
            || self.source != autre.source
            || self.jeu != autre.jeu
            || self.micro != autre.micro
            || self.copains != autre.copains
    }
}

/// `Vidéos\ki-chat` — là où l'Explorateur range les vidéos.
pub fn dossier_par_defaut() -> PathBuf {
    let base = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .map(|p| p.join("Videos"))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Videos")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("ki-chat")
}

/// La source réelle et le nom de ce qu'on filme, d'après le réglage.
pub fn resoudre_source(source: &Source) -> (CaptureSource, String) {
    match source {
        Source::Ecran(n) => (CaptureSource::Monitor(*n), "Écran".into()),
        Source::Fenetre(t) => (CaptureSource::Window(t.clone()), nom_sur(t)),
        Source::Auto => {
            for f in ki_video::list_windows() {
                if let Some(nom) = crate::jeux::reconnaitre(&f.process) {
                    return (CaptureSource::Window(f.title), nom.to_string());
                }
            }
            (CaptureSource::Monitor(0), "Écran".into())
        }
    }
}

/// Un nom de fichier sûr et court, tiré d'un titre de fenêtre.
fn nom_sur(titre: &str) -> String {
    let nom: String = titre
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let nom: String = nom.chars().take(32).collect();
    if nom.trim().is_empty() {
        "Fenêtre".into()
    } else {
        nom.trim().to_string()
    }
}

// ---------------------------------------------------------------------
// Le tampon
// ---------------------------------------------------------------------

/// Durée, en µs, de `n` échantillons stéréo entrelacés à 48 kHz.
fn duree_stereo_us(n: usize) -> u64 {
    (n as u64 / 2) * 1_000_000 / 48_000
}

/// Le nombre d'échantillons stéréo (entrelacés) pour `us` microsecondes.
fn echantillons_stereo(us: u64) -> usize {
    ((us * 48_000 / 1_000_000) * 2) as usize
}

/// Un anneau de son stéréo, horodaté à son premier échantillon.
#[derive(Default)]
struct PisteAudio {
    echantillons: VecDeque<f32>,
    debut_us: u64,
}

impl PisteAudio {
    /// Ajoute un bloc qui **finit** à `fin_us`. Un trou (capture arrêtée
    /// un moment) se comble de silence, pour que l'horodatage reste vrai.
    fn pousser(&mut self, stereo: &[f32], fin_us: u64, garde_us: u64) {
        let duree = duree_stereo_us(stereo.len());
        if self.echantillons.is_empty() {
            self.debut_us = fin_us.saturating_sub(duree);
        } else {
            let fin_attendue = self.debut_us + duree_stereo_us(self.echantillons.len()) + duree;
            if fin_us > fin_attendue + 150_000 {
                let trou = echantillons_stereo(fin_us - fin_attendue);
                self.echantillons.extend(std::iter::repeat_n(0.0, trou));
            }
        }
        self.echantillons.extend(stereo.iter().copied());
        // On ne garde que ce qui peut encore servir, plus une marge.
        let total = duree_stereo_us(self.echantillons.len());
        if total > garde_us {
            let trop = echantillons_stereo(total - garde_us) & !1;
            self.echantillons.drain(..trop.min(self.echantillons.len()));
            self.debut_us += duree_stereo_us(trop);
        }
    }

    /// Les échantillons entre deux instants, silence là où l'on n'a rien.
    fn extraire(&self, de_us: u64, a_us: u64) -> Vec<f32> {
        let n = echantillons_stereo(a_us.saturating_sub(de_us));
        let mut sortie = vec![0.0f32; n];
        if self.echantillons.is_empty() {
            return sortie;
        }
        let fin_us = self.debut_us + duree_stereo_us(self.echantillons.len());
        // Le recouvrement entre [de, a] et [debut, fin].
        let d = de_us.max(self.debut_us);
        let f = a_us.min(fin_us);
        if d >= f {
            return sortie;
        }
        let depuis_source = echantillons_stereo(d - self.debut_us);
        let depuis_sortie = echantillons_stereo(d - de_us);
        let longueur = echantillons_stereo(f - d).min(n.saturating_sub(depuis_sortie));
        for i in 0..longueur {
            if let Some(v) = self.echantillons.get(depuis_source + i) {
                sortie[depuis_sortie + i] = *v;
            }
        }
        sortie
    }
}

/// Ce que l'on garde en mémoire.
struct Tampon {
    images: VecDeque<EncodedFrame>,
    octets: usize,
    duree_max_us: u64,
    jeu: PisteAudio,
    micro: PisteAudio,
    copains: PisteAudio,
    /// SPS et PPS de la dernière trame clé vue : la première image du clip
    /// doit les porter.
    parametres: Option<Vec<u8>>,
}

impl Tampon {
    fn new(duree_s: u32) -> Self {
        Self {
            images: VecDeque::new(),
            octets: 0,
            duree_max_us: u64::from(duree_s) * 1_000_000,
            jeu: PisteAudio::default(),
            micro: PisteAudio::default(),
            copains: PisteAudio::default(),
            parametres: None,
        }
    }

    fn pousser_image(&mut self, image: EncodedFrame) {
        if image.idr {
            if let Some(p) = annexb::parametres(&image.data) {
                self.parametres = Some(p);
            }
        }
        self.octets += image.data.len();
        self.images.push_back(image);
        // On jette par l'avant ce qui dépasse, en s'arrêtant sur une trame
        // clé : le tampon commence toujours par une image décodable.
        while let (Some(premiere), Some(derniere)) = (self.images.front(), self.images.back()) {
            let trop_long = derniere.pts_us.saturating_sub(premiere.pts_us) > self.duree_max_us + 1_000_000;
            if !trop_long && premiere.idr {
                break;
            }
            if !trop_long {
                // Pas trop long mais pas une trame clé en tête : on cherche
                // la première trame clé et on jette ce qui la précède.
                let Some(pos) = self.images.iter().position(|i| i.idr) else { break };
                for _ in 0..pos {
                    if let Some(i) = self.images.pop_front() {
                        self.octets -= i.data.len();
                    }
                }
                break;
            }
            if let Some(i) = self.images.pop_front() {
                self.octets -= i.data.len();
            }
        }
    }

    fn garde_us(&self) -> u64 {
        self.duree_max_us + 3_000_000
    }

    /// Ce qu'il y a à écrire : les images depuis la première trame clé, et
    /// le son qui va avec.
    fn instantane(&self, pistes: (bool, bool, bool)) -> Option<Instantane> {
        let depart = self.images.iter().position(|i| i.idr)?;
        let images: Vec<EncodedFrame> = self
            .images
            .iter()
            .skip(depart)
            .map(|i| EncodedFrame {
                data: i.data.clone(),
                idr: i.idr,
                pts_us: i.pts_us,
                width: i.width,
                height: i.height,
            })
            .collect();
        let t0 = images.first()?.pts_us;
        let t1 = images.last()?.pts_us;
        if t1 <= t0 {
            return None;
        }
        // Une image de plus, pour que le son couvre la dernière.
        let t1 = t1 + 1_000_000 / 30;
        let mut sources = Vec::new();
        if pistes.0 {
            sources.push(("jeu", self.jeu.extraire(t0, t1)));
        }
        if pistes.1 {
            sources.push(("micro", self.micro.extraire(t0, t1)));
        }
        if pistes.2 {
            sources.push(("copains", self.copains.extraire(t0, t1)));
        }
        Some(Instantane { images, sources, parametres: self.parametres.clone() })
    }
}

/// La photographie du tampon, prête à écrire.
struct Instantane {
    images: Vec<EncodedFrame>,
    /// (nom de piste, stéréo entrelacé) — dans l'ordre des pistes du fichier.
    sources: Vec<(&'static str, Vec<f32>)>,
    parametres: Option<Vec<u8>>,
}

// ---------------------------------------------------------------------
// L'enregistreur
// ---------------------------------------------------------------------

/// Un clip écrit.
#[derive(Clone, Debug)]
pub struct Clip {
    pub chemin: PathBuf,
    pub duree_s: f32,
    pub taille: u64,
    /// Les pistes son après le mélange, dans l'ordre du fichier.
    pub pistes: Vec<&'static str>,
}

/// Ce que l'interface montre de l'enregistreur.
#[derive(Clone, Debug, Default)]
pub struct Etat {
    pub secondes: f32,
    pub megaoctets: f32,
    pub encodeur: String,
    pub source: String,
    pub ecriture_en_cours: bool,
}

struct Ecriture {
    resultat: Arc<Mutex<Option<Result<Clip, String>>>>,
    fil: Option<std::thread::JoinHandle<()>>,
}

/// Appelé sur le fil d'écriture quand le clip est là — ou raté.
pub type Fini = Arc<dyn Fn(&Result<Clip, String>) + Send + Sync>;

/// De quoi déclencher un clip depuis n'importe quel fil. Le raccourci
/// appuie dessus depuis le sien, sans passer par l'interface : réduite
/// derrière le jeu, elle peut ne pas repeindre avant longtemps, et le
/// tampon doit être photographié à l'instant de l'appui, pas au retour.
#[derive(Clone)]
pub struct Declencheur {
    tampon: Arc<Mutex<Tampon>>,
    ecriture: Arc<Mutex<Option<Ecriture>>>,
    nom_source: Arc<Mutex<String>>,
    reglages: Reglages,
    /// Ce qu'un appui n'a pas pu faire, gardé pour l'interface.
    avis: Arc<Mutex<VecDeque<String>>>,
    fini: Arc<Mutex<Option<Fini>>>,
}

impl Declencheur {
    /// L'appui : photographie le tampon et écrit le clip sur un fil.
    pub fn sauver(&self) -> Result<(), String> {
        let mut ecriture = self.ecriture.lock().unwrap();
        if ecriture.is_some() {
            return Err("un clip est déjà en cours d'écriture".into());
        }
        let pistes = (self.reglages.jeu, self.reglages.micro, self.reglages.copains);
        let instantane = self
            .tampon
            .lock()
            .unwrap()
            .instantane(pistes)
            .ok_or("rien à enregistrer encore : le tampon se remplit")?;
        let dossier = self.reglages.dossier_effectif();
        if let Some(libre) = espace_libre(&dossier) {
            if libre < 500 * 1024 * 1024 {
                return Err("moins de 500 Mo libres sur le disque des clips".into());
            }
        }
        let source_nom = self.nom_source.lock().unwrap().clone();
        let nom = format!("{} {}.mp4", chrono::Local::now().format("%Y-%m-%d %Hh%Mm%S"), source_nom);
        let chemin = dossier.join(nom);
        let fps = self.reglages.fps;
        let debit = self.reglages.qualite.debit_bps();
        let resultat: Arc<Mutex<Option<Result<Clip, String>>>> = Arc::new(Mutex::new(None));
        let r = resultat.clone();
        let fini = self.fini.clone();
        let fil = std::thread::Builder::new()
            .name("clips-ecriture".into())
            .spawn(move || {
                let sortie = ecrire_clip(instantane, &chemin, fps, debit);
                if let Ok(c) = &sortie {
                    let _ = vignette(&c.chemin);
                    ecrire_fiche(c, &source_nom);
                }
                let rappel = fini.lock().unwrap().clone();
                if let Some(f) = rappel {
                    f(&sortie);
                }
                *r.lock().unwrap() = Some(sortie);
            })
            .map_err(|e| e.to_string())?;
        *ecriture = Some(Ecriture { resultat, fil: Some(fil) });
        Ok(())
    }

    /// L'appui venu du raccourci : ce qui rate est gardé pour l'interface,
    /// qui le dira quand elle repassera (voir [`Enregistreur::tick`]).
    pub fn appuyer(&self) {
        if let Err(m) = self.sauver() {
            ki_video::journal(format!("clips : appui sans clip : {m}"));
            self.avis.lock().unwrap().push_back(m);
        }
    }

    /// Ce qui se passe quand un clip est écrit — le son de confirmation,
    /// depuis le fil d'écriture, pour qu'il parte même interface endormie.
    pub fn quand_fini(&self, f: Option<Fini>) {
        *self.fini.lock().unwrap() = f;
    }

    fn ecriture_en_cours(&self) -> bool {
        self.ecriture.lock().unwrap().is_some()
    }
}

pub struct Enregistreur {
    tampon: Arc<Mutex<Tampon>>,
    stats: Arc<StageStats>,
    origine: Instant,
    boucle: Option<StreamerLoop>,
    force_idr: Arc<AtomicBool>,
    reglages: Reglages,
    source: CaptureSource,
    son_systeme: Option<ki_voice::jeu::SonSysteme>,
    robinet_micro: Option<ki_voice::Robinet>,
    robinet_copains: Option<ki_voice::Robinet>,
    declencheur: Declencheur,
    /// Dernière vérification de la source automatique.
    verif_source: Instant,
    pub erreur: Option<String>,
    /// L'enregistreur doit s'arrêter : l'encodeur logiciel a pris le relais
    /// de NVENC à une qualité qu'il ne tient pas en jeu (voir `tick`).
    pub fatal: bool,
    /// La ligne de statistiques du journal : quand, et les compteurs d'alors.
    stats_a: Instant,
    stats_avant: (u64, u64, u64, u64),
}

/// L'encodeur logiciel n'a rien à faire à 1080p ou à 60 i/s pendant une
/// partie : c'est un cœur entier, et le jeu le sent. La qualité « Légère »
/// (720p) à 30 i/s, elle, reste possible sans NVIDIA.
pub fn logiciel_trop_lourd(reglages: &Reglages) -> bool {
    reglages.qualite.hauteur_max() > 720 || reglages.fps > 30
}

impl Enregistreur {
    /// Démarre la capture et l'encodage vers le tampon.
    pub fn demarrer(reglages: &Reglages) -> anyhow::Result<Self> {
        let (source, nom_source) = resoudre_source(&reglages.source);
        let tampon = Arc::new(Mutex::new(Tampon::new(reglages.duree_s)));
        let origine = Instant::now();
        let stats = Arc::new(StageStats::default());
        let force_idr = Arc::new(AtomicBool::new(false));
        let declencheur = Declencheur {
            tampon: tampon.clone(),
            ecriture: Arc::new(Mutex::new(None)),
            nom_source: Arc::new(Mutex::new(nom_source)),
            reglages: reglages.clone(),
            avis: Arc::new(Mutex::new(VecDeque::new())),
            fini: Arc::new(Mutex::new(None)),
        };
        let mut moi = Self {
            tampon: tampon.clone(),
            stats,
            origine,
            boucle: None,
            force_idr,
            reglages: reglages.clone(),
            source: source.clone(),
            son_systeme: None,
            robinet_micro: None,
            robinet_copains: None,
            declencheur,
            verif_source: Instant::now(),
            erreur: None,
            fatal: false,
            stats_a: Instant::now(),
            stats_avant: (0, 0, 0, 0),
        };
        moi.lancer_capture(source)?;

        // Le son : le système sauf ki-chat, et les robinets du moteur.
        let garde = tampon.lock().unwrap().garde_us();
        if reglages.jeu {
            let t = tampon.clone();
            let recevoir: ki_voice::Robinet = Arc::new(move |stereo: &[f32]| {
                let fin = origine.elapsed().as_micros() as u64;
                t.lock().unwrap().jeu.pousser(stereo, fin, garde);
            });
            match ki_voice::jeu::SonSysteme::start(recevoir) {
                Ok(s) => moi.son_systeme = Some(s),
                Err(e) => {
                    ki_video::journal(format!("clips : pas de son du jeu ({e:#})"));
                    moi.erreur = Some(format!("son du jeu indisponible : {e:#}"));
                }
            }
        }
        if reglages.micro {
            let t = tampon.clone();
            moi.robinet_micro = Some(Arc::new(move |mono: &[f32]| {
                let stereo: Vec<f32> = mono.iter().flat_map(|v| [*v, *v]).collect();
                let fin = origine.elapsed().as_micros() as u64;
                t.lock().unwrap().micro.pousser(&stereo, fin, garde);
            }));
        }
        if reglages.copains {
            let t = tampon.clone();
            moi.robinet_copains = Some(Arc::new(move |mono: &[f32]| {
                let stereo: Vec<f32> = mono.iter().flat_map(|v| [*v, *v]).collect();
                let fin = origine.elapsed().as_micros() as u64;
                t.lock().unwrap().copains.pousser(&stereo, fin, garde);
            }));
        }
        crate::secours::marquer_clips();
        ki_video::journal(format!(
            "clips : enregistreur en marche ({}, {} s, {} i/s, {}) — {}",
            moi.nom_source(),
            reglages.duree_s,
            reglages.fps,
            reglages.qualite.label(),
            match &moi.source {
                CaptureSource::Window(t) => format!("fenêtre « {t} »"),
                CaptureSource::Monitor(n) => format!("écran {n}"),
            }
        ));
        Ok(moi)
    }

    fn lancer_capture(&mut self, source: CaptureSource) -> anyhow::Result<()> {
        let config = StreamConfig {
            source: source.clone(),
            max_height: self.reglages.qualite.hauteur_max(),
            fps: self.reglages.fps,
            bitrate_bps: self.reglages.qualite.debit_bps(),
            cursor: true,
            preview: false,
            encoder: EncoderChoice::Auto,
            gop_s: 1,
            profil: ki_video::Profil::Clip,
        };
        let tampon = self.tampon.clone();
        let emit: ki_video::FrameEmit = Arc::new(move |image: EncodedFrame| {
            tampon.lock().unwrap().pousser_image(image);
        });
        let apercu: ki_video::FrameSink = Arc::new(|_| {});
        let boucle = StreamerLoop::start(
            self.stats.clone(),
            apercu,
            emit,
            config,
            self.force_idr.clone(),
            self.origine,
        )?;
        self.boucle = Some(boucle);
        self.source = source;
        Ok(())
    }

    /// Les robinets à brancher sur le moteur vocal — à rappeler quand il
    /// (re)démarre ; `None` pour une piste qu'on ne garde pas.
    pub fn robinets(&self) -> (Option<ki_voice::Robinet>, Option<ki_voice::Robinet>) {
        (self.robinet_micro.clone(), self.robinet_copains.clone())
    }

    /// De quoi déclencher un clip depuis un autre fil (le raccourci).
    pub fn declencheur(&self) -> Declencheur {
        self.declencheur.clone()
    }

    /// Voir [`Declencheur::quand_fini`].
    pub fn quand_fini(&self, f: Option<Fini>) {
        self.declencheur.quand_fini(f);
    }

    fn nom_source(&self) -> String {
        self.declencheur.nom_source.lock().unwrap().clone()
    }

    /// Ce que montre l'interface.
    pub fn etat(&self) -> Etat {
        let t = self.tampon.lock().unwrap();
        let secondes = match (t.images.front(), t.images.back()) {
            (Some(a), Some(b)) => b.pts_us.saturating_sub(a.pts_us) as f32 / 1_000_000.0,
            _ => 0.0,
        };
        Etat {
            secondes,
            megaoctets: t.octets as f32 / (1024.0 * 1024.0),
            encodeur: if self.stats.materiel.load(Ordering::Relaxed) { "NVENC".into() } else { "logiciel".into() },
            source: self.nom_source(),
            ecriture_en_cours: self.declencheur.ecriture_en_cours(),
        }
    }

    /// À appeler à chaque image : surveille la source, ramasse le résultat
    /// d'une écriture. Rend le clip fini, ou l'erreur, une fois.
    pub fn tick(&mut self) -> Option<Result<Clip, String>> {
        // La source s'est évanouie (le jeu fermé) : on repasse sur l'écran,
        // ou sur ce que l'automatique trouve.
        if self.boucle.as_ref().is_some_and(|b| b.source_closed()) {
            ki_video::journal("clips : la fenêtre filmée a disparu — on repasse sur l'écran");
            if let Some(b) = self.boucle.take() {
                b.stop();
            }
            let (source, nom) = resoudre_source(&Source::Auto);
            *self.declencheur.nom_source.lock().unwrap() = nom;
            if let Err(e) = self.lancer_capture(source) {
                self.erreur = Some(format!("capture perdue : {e:#}"));
            }
        }
        // Toutes les dix secondes, en automatique : le jeu est-il arrivé ?
        if self.reglages.source == Source::Auto && self.verif_source.elapsed() > Duration::from_secs(10) {
            self.verif_source = Instant::now();
            let (source, nom) = resoudre_source(&Source::Auto);
            if source != self.source && !self.declencheur.ecriture_en_cours() {
                ki_video::journal(format!("clips : on filme maintenant {nom}"));
                if let Some(b) = self.boucle.take() {
                    b.stop();
                }
                *self.declencheur.nom_source.lock().unwrap() = nom;
                if let Err(e) = self.lancer_capture(source) {
                    self.erreur = Some(format!("capture : {e:#}"));
                }
            }
        }
        if let Some(a) = self.stats.prendre_avis() {
            self.erreur = Some(a);
        }
        // Ligne rouge du plan : jamais de logiciel à 1080p60 en jeu. Si NVENC
        // a refusé et que la qualité demandée dépasse ce que le logiciel
        // tient, on s'arrête et on le dit, plutôt que de plomber la partie.
        let encodees = self.stats.encoded.load(Ordering::Relaxed);
        if encodees > 0
            && !self.stats.materiel.load(Ordering::Relaxed)
            && !self.fatal
            && logiciel_trop_lourd(&self.reglages)
        {
            self.fatal = true;
            self.erreur = Some(
                "NVENC indisponible : l'enregistreur s'arrête plutôt que d'encoder en logiciel à cette                  qualité — passe en « Légère » à 30 i/s pour réessayer"
                    .into(),
            );
        }
        // Toutes les trente secondes, une ligne au journal : la charge en
        // jeu se lit ensuite dans les diagnostics.
        if self.stats_a.elapsed() >= Duration::from_secs(30) {
            let maintenant = (
                self.stats.captured.load(Ordering::Relaxed),
                encodees,
                self.stats.skipped.load(Ordering::Relaxed),
                self.stats.encoded_bytes.load(Ordering::Relaxed),
            );
            let dt = self.stats_a.elapsed().as_secs_f32().max(0.1);
            let avant = self.stats_avant;
            let etat = self.etat();
            ki_video::journal(format!(
                "clips : {:.0} i/s capturées, {:.0} encodées, {} sautées, {} kbit/s, conversion {:.1} ms,                  encodage {:.1} ms, tampon {:.0} s / {:.0} Mo, {}",
                (maintenant.0 - avant.0) as f32 / dt,
                (maintenant.1 - avant.1) as f32 / dt,
                maintenant.2 - avant.2,
                (maintenant.3 - avant.3) * 8 / 1000 / dt as u64,
                self.stats.convert_ms.get(),
                self.stats.encode_ms.get(),
                etat.secondes,
                etat.megaoctets,
                etat.encodeur
            ));
            self.stats_a = Instant::now();
            self.stats_avant = maintenant;
        }
        // Un appui du raccourci qui n'a rien donné : à dire, un par image.
        if let Some(m) = self.declencheur.avis.lock().unwrap().pop_front() {
            return Some(Err(m));
        }
        let ecriture_finie = {
            let mut ecriture = self.declencheur.ecriture.lock().unwrap();
            let finie = ecriture.as_ref().is_some_and(|e| e.resultat.lock().unwrap().is_some());
            finie.then(|| ecriture.take()).flatten()
        };
        if let Some(mut e) = ecriture_finie {
            if let Some(f) = e.fil.take() {
                let _ = f.join();
            }
            return e.resultat.lock().unwrap().take();
        }
        None
    }

    /// L'appui, depuis l'interface (le bouton « Clip ! ») : voir
    /// [`Declencheur::sauver`].
    pub fn sauver(&self) -> Result<(), String> {
        self.declencheur.sauver()
    }

    /// Arrête tout. Les robinets du moteur sont à débrancher par
    /// l'application, qui tient le moteur.
    pub fn arreter(mut self) {
        if let Some(b) = self.boucle.take() {
            b.stop();
        }
        self.son_systeme = None;
        let en_cours = self.declencheur.ecriture.lock().unwrap().take();
        if let Some(mut e) = en_cours {
            if let Some(f) = e.fil.take() {
                let _ = f.join();
            }
        }
        crate::secours::lever_clips();
        ki_video::journal("clips : enregistreur arrêté");
    }
}

/// L'espace libre sur le volume du dossier, si le système le dit.
#[cfg(windows)]
fn espace_libre(dossier: &Path) -> Option<u64> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let mut libre: u64 = 0;
    let chemin = HSTRING::from(&*dossier.to_string_lossy());
    unsafe { GetDiskFreeSpaceExW(&chemin, Some(&mut libre), None, None) }.ok()?;
    Some(libre)
}

#[cfg(not(windows))]
fn espace_libre(_dossier: &Path) -> Option<u64> {
    None
}

/// Écrit le MP4 : les images telles quelles, puis les pistes — le mélange
/// d'abord quand il y a plusieurs sources, puis chaque source.
fn ecrire_clip(inst: Instantane, chemin: &Path, fps: u32, debit_bps: u32) -> Result<Clip, String> {
    let premiere = inst.images.first().ok_or("aucune image")?;
    let noms: Vec<&'static str> = inst.sources.iter().map(|(n, _)| *n).collect();
    let t0 = premiere.pts_us;
    let (largeur, hauteur) = (u32::from(premiere.width), u32::from(premiere.height));
    let format = ki_media::FormatVideo {
        largeur,
        hauteur,
        fps,
        debit_bps,
        parametres: inst.parametres.clone(),
    };
    // Les pistes : [mélange, source…] ou [la source seule].
    let mut pistes: Vec<Vec<f32>> = Vec::new();
    if inst.sources.len() > 1 {
        let n = inst.sources.iter().map(|(_, s)| s.len()).max().unwrap_or(0);
        let mut mix = vec![0.0f32; n];
        for (_, s) in &inst.sources {
            for (m, v) in mix.iter_mut().zip(s.iter()) {
                *m += v;
            }
        }
        for m in mix.iter_mut() {
            *m = ecreter(*m);
        }
        pistes.push(mix);
    }
    for (_, s) in inst.sources {
        pistes.push(s);
    }
    let mut e = ki_media::ecrire(chemin, &format, pistes.len()).map_err(|e| format!("{e:#}"))?;
    let duree_image = 1_000_000 / u64::from(fps.max(1));
    // Entrelacé par le temps : le son de chaque piste suit les images.
    let mut curseurs = vec![0usize; pistes.len()];
    const BLOC: usize = 1920; // 20 ms de stéréo
    for (i, image) in inst.images.iter().enumerate() {
        let pts = image.pts_us - t0;
        let duree = inst
            .images
            .get(i + 1)
            .map(|n| n.pts_us.saturating_sub(image.pts_us))
            .filter(|d| *d > 0)
            .unwrap_or(duree_image);
        let mut donnees = &image.data[..];
        let avec_parametres;
        if i == 0 && annexb::parametres(donnees).is_none() {
            let mut d = inst.parametres.clone().unwrap_or_default();
            d.extend_from_slice(donnees);
            avec_parametres = d;
            donnees = &avec_parametres;
        }
        e.image(donnees, pts, duree, image.idr).map_err(|e| format!("{e:#}"))?;
        for (p, piste) in pistes.iter().enumerate() {
            while curseurs[p] < piste.len() {
                let debut = curseurs[p];
                let pts_son = duree_stereo_us(debut);
                if pts_son > pts + duree {
                    break;
                }
                let fin = (debut + BLOC).min(piste.len());
                e.son(p, &piste[debut..fin], pts_son).map_err(|e| format!("{e:#}"))?;
                curseurs[p] = fin;
            }
        }
    }
    // Ce qui reste de son après la dernière image.
    for (p, piste) in pistes.iter().enumerate() {
        while curseurs[p] < piste.len() {
            let debut = curseurs[p];
            let fin = (debut + BLOC).min(piste.len());
            e.son(p, &piste[debut..fin], duree_stereo_us(debut)).map_err(|e| format!("{e:#}"))?;
            curseurs[p] = fin;
        }
    }
    e.terminer().map_err(|e| format!("{e:#}"))?;
    let derniere = inst.images.last().ok_or("aucune image")?;
    let duree_s = (derniere.pts_us - t0 + duree_image) as f32 / 1_000_000.0;
    let taille = std::fs::metadata(chemin).map(|m| m.len()).unwrap_or(0);
    ki_video::journal(format!(
        "clips : {} — {:.1} s, {} images, {} Mo",
        chemin.display(),
        duree_s,
        inst.images.len(),
        taille / (1024 * 1024)
    ));
    Ok(Clip { chemin: chemin.to_path_buf(), duree_s, taille, pistes: noms })
}

/// Écrêtage doux : plusieurs voix fortes qui se superposent ne saturent
/// pas brutalement.
fn ecreter(x: f32) -> f32 {
    if x.abs() <= 0.8 {
        x
    } else {
        x.signum() * (0.8 + (x.abs() - 0.8) / (1.0 + (x.abs() - 0.8) * 4.0)).min(1.0)
    }
}

// ---------------------------------------------------------------------
// La galerie
// ---------------------------------------------------------------------

/// Le dossier des vignettes de clips, créé au besoin.
fn dossier_vignettes() -> Option<PathBuf> {
    let dir = eframe::storage_dir("ki-chat")?.join("clips");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn empreinte(chemin: &Path) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in chemin.to_string_lossy().to_lowercase().bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Où va la vignette d'un clip.
pub fn chemin_vignette(clip: &Path) -> Option<PathBuf> {
    Some(dossier_vignettes()?.join(format!("{:016x}.jpg", empreinte(clip))))
}

/// La fiche d'un clip, à côté de sa vignette : ce que le fichier ne dit
/// pas de lui-même — ses pistes son après le mélange, dans l'ordre, et
/// d'où il vient. Le partage s'en sert pour proposer de retirer les voix
/// des copains ; un clip sans fiche (d'avant elle) se partage tel quel.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Fiche {
    /// Les pistes après le mélange, dans l'ordre du fichier ; `None` :
    /// inconnues (une fiche écrite après coup, pour un clip d'avant elle).
    #[serde(default)]
    pub pistes: Option<Vec<String>>,
    #[serde(default)]
    pub duree_s: f32,
    #[serde(default)]
    pub source: String,
    /// L'identifiant du clip sur le serveur, s'il y a été déposé (partagé,
    /// ou passé par l'atelier) — et lequel : un autre serveur, c'est un
    /// autre dépôt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serveur: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serveur_base: Option<String>,
}

/// Où va la fiche d'un clip.
pub fn chemin_fiche(clip: &Path) -> Option<PathBuf> {
    Some(dossier_vignettes()?.join(format!("{:016x}.json", empreinte(clip))))
}

pub fn ecrire_fiche(clip: &Clip, source: &str) {
    let fiche = Fiche {
        pistes: Some(clip.pistes.iter().map(|p| p.to_string()).collect()),
        duree_s: clip.duree_s,
        source: source.to_string(),
        serveur: None,
        serveur_base: None,
    };
    sauver_fiche(&clip.chemin, &fiche);
}

/// Écrit (ou réécrit) la fiche d'un clip.
pub fn sauver_fiche(clip: &Path, fiche: &Fiche) {
    if let (Some(chemin), Ok(json)) = (chemin_fiche(clip), serde_json::to_vec_pretty(fiche)) {
        let _ = std::fs::write(chemin, json);
    }
}

pub fn lire_fiche(clip: &Path) -> Option<Fiche> {
    let octets = std::fs::read(chemin_fiche(clip)?).ok()?;
    serde_json::from_slice(&octets).ok()
}

/// Le clip est sur ce serveur, sous cet identifiant : la fiche l'apprend,
/// telle qu'elle est **sur le disque** au moment où on le sait — depuis le
/// fil qui vient de recevoir la réponse de `/clips/fin`, pas depuis une
/// fenêtre qui a pu être fermée entre-temps. Sans ça, un atelier fermé
/// pendant la préparation redéposait le clip en entier au prochain export.
/// Rend la fiche écrite.
pub fn noter_serveur(clip: &Path, base: &str, id: &str) -> Fiche {
    let mut fiche = lire_fiche(clip).unwrap_or_default();
    if fiche.serveur.as_deref() != Some(id) || fiche.serveur_base.as_deref() != Some(base) {
        fiche.serveur = Some(id.to_string());
        fiche.serveur_base = Some(base.to_string());
        sauver_fiche(clip, &fiche);
    }
    fiche
}

/// Le serveur ne connaît plus ce clip (purgé, retiré, ou illisible chez
/// lui) : la fiche l'oublie, un prochain envoi le redéposera. Rend la fiche
/// écrite, s'il y en avait une.
pub fn oublier_serveur(clip: &Path) -> Option<Fiche> {
    let mut fiche = lire_fiche(clip)?;
    if fiche.serveur.is_some() || fiche.serveur_base.is_some() {
        fiche.serveur = None;
        fiche.serveur_base = None;
        sauver_fiche(clip, &fiche);
    }
    Some(fiche)
}

/// Fabrique la vignette (320 px de large, JPEG) d'un clip, depuis sa
/// première image. Rend son chemin.
pub fn vignette(clip: &Path) -> anyhow::Result<PathBuf> {
    let cible = chemin_vignette(clip).ok_or_else(|| anyhow::anyhow!("pas de dossier de vignettes"))?;
    let image = ki_media::premiere_image(clip)?;
    let source = image::RgbaImage::from_raw(image.largeur, image.hauteur, image.rgba)
        .ok_or_else(|| anyhow::anyhow!("image incohérente"))?;
    let largeur = 320u32.min(image.largeur.max(1));
    let hauteur = (u64::from(largeur) * u64::from(image.hauteur) / u64::from(image.largeur.max(1))).max(1) as u32;
    let petite = image::imageops::thumbnail(&source, largeur, hauteur);
    let rgb = image::DynamicImage::ImageRgba8(petite).to_rgb8();
    let mut octets = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut octets, 82);
    image::ImageEncoder::write_image(enc, rgb.as_raw(), largeur, hauteur, image::ExtendedColorType::Rgb8)?;
    std::fs::write(&cible, octets)?;
    Ok(cible)
}

/// Un clip du dossier, tel que la galerie le montre.
#[derive(Clone, Debug)]
pub struct ClipInfo {
    pub chemin: PathBuf,
    pub nom: String,
    pub modifie: std::time::SystemTime,
    pub taille: u64,
    pub vignette: Option<PathBuf>,
    /// Sa fiche, si l'enregistreur l'a écrite.
    pub fiche: Option<Fiche>,
}

/// Les clips du dossier, les plus récents d'abord.
pub fn lister(dossier: &Path) -> Vec<ClipInfo> {
    let Ok(entrees) = std::fs::read_dir(dossier) else { return Vec::new() };
    let mut clips: Vec<ClipInfo> = entrees
        .flatten()
        .filter_map(|e| {
            let chemin = e.path();
            let ext = chemin.extension()?.to_string_lossy().to_lowercase();
            if ext != "mp4" {
                return None;
            }
            let meta = e.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            let vignette = chemin_vignette(&chemin).filter(|v| v.is_file());
            let fiche = lire_fiche(&chemin);
            Some(ClipInfo {
                nom: chemin.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                modifie: meta.modified().unwrap_or(std::time::UNIX_EPOCH),
                taille: meta.len(),
                vignette,
                fiche,
                chemin,
            })
        })
        .collect();
    clips.sort_by_key(|c| std::cmp::Reverse(c.modifie));
    clips
}

/// Ouvre l'Explorateur sur le clip.
pub fn montrer_dans_le_dossier(chemin: &Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer.exe")
            .arg(format!("/select,{}", chemin.display()))
            .spawn();
    }
    #[cfg(not(windows))]
    {
        if let Some(d) = chemin.parent() {
            let _ = std::process::Command::new("open").arg(d).spawn();
        }
    }
}

/// « 45 Mo », « 1,2 Go ».
pub fn taille_lisible(octets: u64) -> String {
    if octets >= 1024 * 1024 * 1024 {
        format!("{:.1} Go", octets as f64 / (1024.0 * 1024.0 * 1024.0)).replace('.', ",")
    } else {
        format!("{} Mo", octets / (1024 * 1024))
    }
}

/// Un repaint périodique tant que l'enregistreur tourne : l'indicateur
/// respire, le tampon se lit.
pub fn cadence_ui(ctx: &egui::Context) {
    ctx.request_repaint_after(Duration::from_millis(500));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(pts_us: u64, idr: bool) -> EncodedFrame {
        EncodedFrame { data: vec![0u8; 100], idr, pts_us, width: 16, height: 16 }
    }

    #[test]
    fn le_logiciel_ne_tient_que_la_qualite_legere_a_trente() {
        let mut r = Reglages { qualite: Qualite::Legere, fps: 30, ..Default::default() };
        assert!(!logiciel_trop_lourd(&r));
        r.fps = 60;
        assert!(logiciel_trop_lourd(&r));
        r.fps = 30;
        r.qualite = Qualite::Equilibre;
        assert!(logiciel_trop_lourd(&r));
    }

    #[test]
    fn la_fiche_d_un_clip_fait_l_aller_retour() {
        let chemin = std::env::temp_dir().join(format!("ki-clip-fiche-{}.mp4", std::process::id()));
        let clip = Clip { chemin: chemin.clone(), duree_s: 12.5, taille: 42, pistes: vec!["jeu", "copains"] };
        ecrire_fiche(&clip, "VALORANT");
        let fiche = lire_fiche(&chemin).expect("la fiche se relit");
        assert_eq!(fiche.pistes.as_deref(), Some(&["jeu".to_string(), "copains".to_string()][..]));
        assert_eq!(fiche.source, "VALORANT");
        assert!((fiche.duree_s - 12.5).abs() < 0.01);
        // Un clip d'avant la fiche n'en a pas : il se partage tel quel.
        assert!(lire_fiche(&chemin.with_extension("autre.mp4")).is_none());
        // Le dépôt sur un serveur se note, sans inventer de pistes.
        let mut apres = Fiche { serveur: Some("0123456789abcdef".into()), ..Default::default() };
        apres.serveur_base = Some("https://x:8080".into());
        sauver_fiche(&chemin, &apres);
        let relue = lire_fiche(&chemin).unwrap();
        assert_eq!(relue.serveur.as_deref(), Some("0123456789abcdef"));
        assert!(relue.pistes.is_none());
        if let Some(f) = chemin_fiche(&chemin) {
            let _ = std::fs::remove_file(f);
        }
    }

    /// Le serveur se note et s'oublie **sur le disque**, en gardant le reste
    /// de la fiche : c'est ce qu'un fil fait pendant que l'atelier est
    /// peut-être fermé.
    #[test]
    fn la_fiche_note_et_oublie_le_serveur_sans_perdre_le_reste() {
        let chemin = std::env::temp_dir().join(format!("ki-clip-serveur-{}.mp4", std::process::id()));
        if chemin_fiche(&chemin).is_none() {
            eprintln!("pas de dossier de vignettes : test sauté");
            return;
        }
        let clip = Clip { chemin: chemin.clone(), duree_s: 8.0, taille: 42, pistes: vec!["jeu", "micro"] };
        ecrire_fiche(&clip, "VALORANT");
        let notee = noter_serveur(&chemin, "https://ts:8080", "0123456789abcdef");
        assert_eq!(notee.serveur.as_deref(), Some("0123456789abcdef"));
        assert_eq!(notee.serveur_base.as_deref(), Some("https://ts:8080"));
        let relue = lire_fiche(&chemin).unwrap();
        assert_eq!(relue, notee, "écrite telle quelle");
        assert_eq!(relue.source, "VALORANT", "le reste de la fiche est gardé");
        assert_eq!(relue.pistes.as_deref().map(|p| p.len()), Some(2));
        // Un autre serveur : un autre dépôt.
        let ailleurs = noter_serveur(&chemin, "https://autre:8080", "fedcba9876543210");
        assert_eq!(lire_fiche(&chemin).unwrap(), ailleurs);
        // Oublié : plus de serveur, le reste intact.
        let oubliee = oublier_serveur(&chemin).unwrap();
        assert!(oubliee.serveur.is_none() && oubliee.serveur_base.is_none());
        let relue = lire_fiche(&chemin).unwrap();
        assert_eq!(relue, oubliee);
        assert_eq!(relue.source, "VALORANT");
        // Sans fiche du tout : rien à oublier, rien d'inventé.
        assert!(oublier_serveur(&chemin.with_extension("inconnu.mp4")).is_none());
        if let Some(f) = chemin_fiche(&chemin) {
            let _ = std::fs::remove_file(f);
        }
    }

    #[test]
    fn le_tampon_commence_par_une_trame_cle_et_tient_sa_duree() {
        let mut t = Tampon::new(2);
        // 5 s d'images à 10 i/s, trame clé chaque seconde.
        for i in 0..50u64 {
            t.pousser_image(image(i * 100_000, i % 10 == 0));
        }
        let premiere = t.images.front().unwrap();
        assert!(premiere.idr);
        let duree = t.images.back().unwrap().pts_us - premiere.pts_us;
        assert!(duree <= 3_000_000, "{duree}");
        assert!(duree >= 2_000_000, "{duree}");
        assert_eq!(t.octets, t.images.len() * 100);
    }

    #[test]
    fn l_instantane_part_de_la_trame_cle_et_le_son_suit() {
        let mut t = Tampon::new(30);
        for i in 0..20u64 {
            t.pousser_image(image(1_000_000 + i * 100_000, i == 0 || i == 10));
        }
        // Du son sur la piste jeu de 0,5 s à 3,5 s.
        let bloc = vec![0.5f32; 1920];
        for b in 0..150u64 {
            t.jeu.pousser(&bloc, 500_000 + (b + 1) * 20_000, t.garde_us());
        }
        let inst = t.instantane((true, false, true)).unwrap();
        assert_eq!(inst.images.len(), 20);
        assert!(inst.images[0].idr);
        assert_eq!(inst.sources.len(), 2);
        let (nom, jeu) = &inst.sources[0];
        assert_eq!(*nom, "jeu");
        // 1,9 s d'images plus une : ~1,93 s de son.
        assert!((jeu.len() as f64 / 96_000.0 - 1.93).abs() < 0.05, "{}", jeu.len());
        assert!(jeu.iter().all(|v| (v - 0.5).abs() < 1e-6));
        // Les copains n'ont rien dit : du silence, de la même longueur.
        assert_eq!(inst.sources[1].1.len(), jeu.len());
        assert!(inst.sources[1].1.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn un_trou_dans_le_son_se_comble_de_silence() {
        let mut p = PisteAudio::default();
        p.pousser(&[1.0; 1920], 20_000, 10_000_000);
        // Le bloc suivant arrive 500 ms trop tard.
        p.pousser(&[1.0; 1920], 540_000, 10_000_000);
        assert_eq!(p.debut_us, 0);
        let s = p.extraire(0, 540_000);
        assert_eq!(s.len(), echantillons_stereo(540_000));
        assert!(s[..1920].iter().all(|v| *v == 1.0));
        assert!(s[2000..40_000].iter().all(|v| *v == 0.0));
        assert!(s[s.len() - 1920..].iter().all(|v| *v == 1.0));
    }

    #[test]
    fn extraire_hors_de_la_piste_donne_du_silence() {
        let mut p = PisteAudio::default();
        p.pousser(&[0.3; 1920], 1_020_000, 10_000_000);
        let avant = p.extraire(0, 500_000);
        assert!(avant.iter().all(|v| *v == 0.0));
        let dedans = p.extraire(1_000_000, 1_020_000);
        assert_eq!(dedans.len(), 1920);
        assert!(dedans.iter().all(|v| (v - 0.3).abs() < 1e-6));
    }

    #[test]
    fn les_reglages_font_l_aller_retour() {
        let r = Reglages {
            duree_s: 60,
            qualite: Qualite::Haute,
            source: Source::Fenetre("VALORANT".into()),
            copains: false,
            dossier: Some(PathBuf::from("D:/clips")),
            ..Reglages::default()
        };
        struct S(std::collections::HashMap<String, String>);
        impl eframe::Storage for S {
            fn get_string(&self, key: &str) -> Option<String> {
                self.0.get(key).cloned()
            }
            fn set_string(&mut self, key: &str, value: String) {
                self.0.insert(key.into(), value);
            }
            fn flush(&mut self) {}
        }
        let mut s = S(Default::default());
        r.save(&mut s);
        let relu = Reglages::load(|k, d| s.0.get(k).cloned().unwrap_or_else(|| d.to_string()));
        assert_eq!(relu, r);
    }

    #[test]
    fn les_noms_de_fenetre_deviennent_des_noms_de_fichier() {
        assert_eq!(nom_sur("VALORANT  "), "VALORANT");
        assert_eq!(nom_sur("Discord | #général: bla/bla"), "Discord général bla bla");
        assert_eq!(nom_sur("///"), "Fenêtre");
        assert_eq!(taille_lisible(45 * 1024 * 1024), "45 Mo");
        assert_eq!(taille_lisible(1_300_000_000), "1,2 Go");
    }

    /// Le vrai circuit sur cette machine : l'écran principal filmé quelques
    /// secondes, un clip écrit, relu avec ki-media. Ignoré par défaut (il
    /// faut un écran, et NVENC ou un processeur qui suit) :
    /// `cargo test -p ki-client-gui enregistre -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn l_enregistreur_filme_l_ecran_et_ecrit_un_clip_relisible() {
        let dossier = std::env::temp_dir().join(format!("ki-clips-essai-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dossier);
        let reglages = Reglages {
            duree_s: 15,
            qualite: Qualite::Legere,
            fps: 30,
            source: Source::Ecran(0),
            jeu: true,
            micro: false,
            copains: false,
            dossier: Some(dossier.clone()),
            ..Reglages::default()
        };
        let mut e = Enregistreur::demarrer(&reglages).expect("démarrage");
        std::thread::sleep(Duration::from_secs(4));
        let etat = e.etat();
        eprintln!("tampon : {:.1} s, {:.1} Mo, {}", etat.secondes, etat.megaoctets, etat.encodeur);
        assert!(etat.secondes >= 2.0, "le tampon ne se remplit pas ({:.1} s)", etat.secondes);
        e.sauver().expect("photographie du tampon");
        let debut = Instant::now();
        let clip = loop {
            if let Some(r) = e.tick() {
                break r.expect("écriture du clip");
            }
            assert!(debut.elapsed() < Duration::from_secs(30), "l'écriture n'aboutit pas");
            std::thread::sleep(Duration::from_millis(50));
        };
        eprintln!("clip : {} ({:.1} s, {} Ko)", clip.chemin.display(), clip.duree_s, clip.taille / 1024);
        e.arreter();
        assert!(clip.duree_s >= 2.0);
        let mut l = ki_media::ouvrir(&clip.chemin).expect("relecture");
        let infos = l.infos().clone();
        assert!(infos.video, "{infos:?}");
        assert!(infos.audio, "pas de piste son : {infos:?}");
        assert!(infos.largeur > 0 && infos.hauteur <= 720, "{infos:?}");
        assert!(infos.duree_ms >= 2_000, "{infos:?}");
        let mut images = 0;
        while let ki_media::Paquet::Image(_) = l.suivant(ki_media::Flux::Video).expect("image") {
            images += 1;
        }
        assert!(images >= 50, "{images} images");
        assert!(vignette(&clip.chemin).is_ok() || chemin_vignette(&clip.chemin).is_some_and(|v| v.is_file()));
        let _ = std::fs::remove_dir_all(&dossier);
    }

    #[test]
    fn l_ecretage_reste_borne_et_transparent() {
        assert_eq!(ecreter(0.5), 0.5);
        assert!(ecreter(3.0) <= 1.0 && ecreter(3.0) > 0.8);
        assert!(ecreter(-3.0) >= -1.0);
    }
}
