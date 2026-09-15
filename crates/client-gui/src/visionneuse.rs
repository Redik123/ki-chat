//! La visionneuse : une image ou une vidéo du chat, en grand, dans ki-chat —
//! plus de navigateur (PLAN-CLIPS.md, jalon C0).
//!
//! Un voile sombre par-dessus tout, le média ajusté au centre, une barre en
//! haut (titre, enregistrer sous, copier, ouvrir dans le navigateur, fermer)
//! et, pour une vidéo, une barre de lecture en bas. Les chevrons et ←/→
//! passent d'un média du salon à l'autre ; Échap ferme (géré par
//! l'application, avec ses autres fenêtres) ; un clic dans le vide aussi.
//!
//! La vidéo est lue par un fil à part (`Lecture`) : il ouvre le fichier du
//! cache avec `ki-media`, dépose le son dans la file « médias » du moteur
//! vocal, et pose chaque image au moment où le son qui va avec est parti
//! vers la carte. L'interface ne fait que prendre la dernière image et la
//! peindre. Sans piste audio, l'horloge murale tient lieu de son.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, CornerRadius, Rect, RichText, Sense, Stroke, Vec2};
use ki_media::{Flux, Paquet};
use ki_voice::medias::File;

use crate::icons::Icon;
use crate::images::{Preview, Previews};
use crate::medias::{self, Telechargement};
use crate::theme::{self, ACCENT, DANGER, TEXT, TEXT_DIM, TEXT_FAINT};
use crate::ui;

/// Ce que la visionneuse montre : une adresse de notre serveur, ou un
/// fichier de cette machine (un clip de la galerie).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cible {
    Image(String),
    Video(String),
    Fichier(PathBuf),
}

impl Cible {
    /// L'adresse, ou le chemin pour un fichier local.
    pub fn url(&self) -> &str {
        match self {
            Cible::Image(u) | Cible::Video(u) => u,
            Cible::Fichier(p) => p.to_str().unwrap_or(""),
        }
    }

    /// Le nom à afficher.
    pub fn nom(&self) -> String {
        match self {
            Cible::Image(u) | Cible::Video(u) => medias::nom_du_fichier(u),
            Cible::Fichier(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        }
    }

    pub fn est_video(&self) -> bool {
        matches!(self, Cible::Video(_) | Cible::Fichier(_))
    }

    pub fn est_locale(&self) -> bool {
        matches!(self, Cible::Fichier(_))
    }
}

/// Ce que l'interface demande à l'application — qui a le client HTTP, les
/// dialogues natifs et le presse-papiers.
pub enum Demande {
    EnregistrerSous(Cible),
    Copier(String),
    Navigateur(String),
}

// ---------------------------------------------------------------------
// Le fil de lecture
// ---------------------------------------------------------------------

enum Commande {
    Lecture(bool),
    Chercher(u64),
    Boucle(bool),
}

/// Ce que le fil de lecture et l'interface se partagent.
#[derive(Default)]
struct Partage {
    /// La dernière image décodée, pas encore prise par l'interface.
    image: Mutex<Option<egui::ColorImage>>,
    position_ms: AtomicU64,
    duree_ms: AtomicU64,
    /// Le fichier est ouvert : durée et pistes connues.
    prete: AtomicBool,
    lecture: AtomicBool,
    fin: AtomicBool,
    erreur: Mutex<Option<String>>,
}

/// Une vidéo en cours de lecture. Lâcher la poignée arrête le fil.
pub struct Lecture {
    partage: Arc<Partage>,
    tx: Option<mpsc::Sender<Commande>>,
    fil: Option<std::thread::JoinHandle<()>>,
}

impl Lecture {
    pub(crate) fn demarrer(chemin: PathBuf, file: Arc<File>, ctx: egui::Context) -> Self {
        let partage = Arc::new(Partage::default());
        let (tx, rx) = mpsc::channel();
        let p = partage.clone();
        let fil = std::thread::Builder::new()
            .name("visionneuse-lecture".into())
            .spawn(move || fil_lecture(chemin, file, p, rx, ctx))
            .ok();
        Self {
            partage,
            tx: Some(tx),
            fil,
        }
    }

    fn commander(&self, c: Commande) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(c);
        }
    }

    pub(crate) fn jouer(&self, on: bool) {
        self.commander(Commande::Lecture(on));
    }

    pub(crate) fn chercher(&self, ms: u64) {
        self.commander(Commande::Chercher(ms));
    }

    fn boucle(&self, on: bool) {
        self.commander(Commande::Boucle(on));
    }

    pub(crate) fn prendre_image(&self) -> Option<egui::ColorImage> {
        self.partage.image.lock().unwrap().take()
    }

    pub(crate) fn position_ms(&self) -> u64 {
        self.partage.position_ms.load(Ordering::Relaxed)
    }

    pub(crate) fn duree_ms(&self) -> u64 {
        self.partage.duree_ms.load(Ordering::Relaxed)
    }

    pub(crate) fn prete(&self) -> bool {
        self.partage.prete.load(Ordering::Relaxed)
    }

    pub(crate) fn en_lecture(&self) -> bool {
        self.partage.lecture.load(Ordering::Relaxed)
    }

    pub(crate) fn erreur(&self) -> Option<String> {
        self.partage.erreur.lock().unwrap().clone()
    }
}

impl Drop for Lecture {
    fn drop(&mut self) {
        // Fermer le canal suffit : le fil s'en aperçoit à son prochain tour.
        self.tx = None;
        if let Some(f) = self.fil.take() {
            let _ = f.join();
        }
    }
}

/// Avance de son gardée devant la carte : 300 ms.
const AVANCE_SON: usize = 48_000 * 3 / 10;
/// Une image en retard de plus que ça sur l'horloge est sautée.
const RETARD_MAX_MS: u64 = 80;

fn fil_lecture(
    chemin: PathBuf,
    file: Arc<File>,
    partage: Arc<Partage>,
    rx: mpsc::Receiver<Commande>,
    ctx: egui::Context,
) {
    file.vider();
    file.set_pause(true);
    let mut lecteur = match ki_media::ouvrir(&chemin) {
        Ok(l) => l,
        Err(e) => {
            *partage.erreur.lock().unwrap() = Some(format!("{e:#}"));
            ctx.request_repaint();
            return;
        }
    };
    let infos = lecteur.infos().clone();
    let (audio, video) = (infos.audio, infos.video);
    let duree = infos.duree_ms;
    partage.duree_ms.store(duree, Ordering::Relaxed);
    partage.prete.store(true, Ordering::Relaxed);

    let publier = |image: &ki_media::Image| {
        let taille = [image.largeur as usize, image.hauteur as usize];
        *partage.image.lock().unwrap() = Some(egui::ColorImage::from_rgba_unmultiplied(
            taille,
            &image.rgba,
        ));
        ctx.request_repaint();
    };
    let signaler = |e: anyhow::Error| {
        *partage.erreur.lock().unwrap() = Some(format!("{e:#}"));
    };

    // La première image tout de suite : c'est l'affiche, avant même « lire ».
    let mut attente: Option<ki_media::Image> = None;
    if video {
        match lecteur.suivant(Flux::Video) {
            Ok(Paquet::Image(i)) => publier(&i),
            Ok(_) => {}
            Err(e) => signaler(e),
        }
    }

    let mut base_ms: u64 = 0;
    let mut depart_mur: Option<Instant> = None;
    let mut lecture = false;
    let mut boucle = false;
    let mut fin_video = !video;
    let mut fin_audio = !audio;

    loop {
        // Les ordres de l'interface.
        let mut recherche: Option<u64> = None;
        loop {
            match rx.try_recv() {
                Ok(Commande::Lecture(on)) => {
                    if on && !lecture {
                        if partage.fin.load(Ordering::Relaxed) {
                            recherche = Some(0);
                        }
                        depart_mur = Some(Instant::now());
                    } else if !on && lecture {
                        base_ms = horloge(audio, &file, base_ms, depart_mur);
                        depart_mur = None;
                    }
                    lecture = on;
                    file.set_pause(!on);
                    partage.lecture.store(on, Ordering::Relaxed);
                    if on {
                        partage.fin.store(false, Ordering::Relaxed);
                    }
                }
                Ok(Commande::Chercher(ms)) => recherche = Some(ms.min(duree)),
                Ok(Commande::Boucle(b)) => boucle = b,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }
        if let Some(ms) = recherche {
            if let Err(e) = lecteur.chercher(ms) {
                signaler(e);
            }
            file.vider();
            base_ms = ms;
            depart_mur = lecture.then(Instant::now);
            attente = None;
            fin_video = !video;
            fin_audio = !audio;
            partage.fin.store(false, Ordering::Relaxed);
            partage.position_ms.store(ms, Ordering::Relaxed);
            // On avance jusqu'à la cible et l'on pose l'image, lecture ou
            // pas : tirer le curseur doit montrer où l'on est.
            if video {
                loop {
                    match lecteur.suivant(Flux::Video) {
                        Ok(Paquet::Image(i)) => {
                            if i.pts_ms + 20 >= ms {
                                publier(&i);
                                break;
                            }
                        }
                        Ok(Paquet::Fin) => {
                            fin_video = true;
                            break;
                        }
                        Ok(Paquet::Audio { .. }) => {}
                        Err(e) => {
                            signaler(e);
                            fin_video = true;
                            break;
                        }
                    }
                }
            }
        }

        if !lecture {
            std::thread::sleep(Duration::from_millis(15));
            continue;
        }

        let maintenant = horloge(audio, &file, base_ms, depart_mur);
        partage.position_ms.store(maintenant, Ordering::Relaxed);

        // Le son : une avance constante devant la carte. Ce qui est avant la
        // cible d'une recherche est jeté, un bloc à cheval est rogné.
        while audio && !fin_audio && file.en_attente() < AVANCE_SON {
            match lecteur.suivant(Flux::Audio) {
                Ok(Paquet::Audio { pts_ms, mono }) => {
                    let fin = pts_ms + (mono.len() as u64) * 1000 / 48_000;
                    if fin <= base_ms {
                        continue;
                    }
                    let saut = if pts_ms < base_ms {
                        ((base_ms - pts_ms) * 48) as usize
                    } else {
                        0
                    };
                    file.pousser(&mono[saut.min(mono.len())..]);
                }
                Ok(Paquet::Fin) => fin_audio = true,
                Ok(Paquet::Image(_)) => {}
                Err(e) => {
                    signaler(e);
                    fin_audio = true;
                }
            }
        }

        // L'image : la suivante attend son heure ; en retard, elle saute.
        if attente.is_none() && !fin_video {
            match lecteur.suivant(Flux::Video) {
                Ok(Paquet::Image(i)) => attente = Some(i),
                Ok(Paquet::Fin) => fin_video = true,
                Ok(Paquet::Audio { .. }) => {}
                Err(e) => {
                    signaler(e);
                    fin_video = true;
                }
            }
        }
        let mut sommeil = Duration::from_millis(4);
        if let Some(i) = attente.take() {
            if i.pts_ms <= maintenant + 2 {
                if maintenant.saturating_sub(i.pts_ms) <= RETARD_MAX_MS || fin_video {
                    publier(&i);
                }
                sommeil = Duration::ZERO;
            } else {
                sommeil = Duration::from_millis((i.pts_ms - maintenant).clamp(1, 10));
                attente = Some(i);
            }
        }

        // La fin : tout est lu et tout est joué.
        if fin_video && fin_audio && attente.is_none() && (!audio || file.en_attente() == 0) {
            if boucle {
                if let Err(e) = lecteur.chercher(0) {
                    signaler(e);
                }
                file.vider();
                base_ms = 0;
                depart_mur = Some(Instant::now());
                fin_video = !video;
                fin_audio = !audio;
            } else {
                lecture = false;
                file.set_pause(true);
                base_ms = duree;
                depart_mur = None;
                partage.position_ms.store(duree, Ordering::Relaxed);
                partage.lecture.store(false, Ordering::Relaxed);
                partage.fin.store(true, Ordering::Relaxed);
                ctx.request_repaint();
            }
            continue;
        }
        if !sommeil.is_zero() {
            std::thread::sleep(sommeil);
        }
    }
}

/// Où l'on en est : le son qui est parti, ou le temps qui a passé.
fn horloge(audio: bool, file: &File, base_ms: u64, depart_mur: Option<Instant>) -> u64 {
    if audio {
        base_ms + file.consommes() * 1000 / 48_000
    } else {
        base_ms
            + depart_mur
                .map(|d| d.elapsed().as_millis() as u64)
                .unwrap_or(0)
    }
}

// ---------------------------------------------------------------------
// L'état de la visionneuse
// ---------------------------------------------------------------------

struct Video {
    url: String,
    chemin: Option<PathBuf>,
    telechargement: Option<Arc<Telechargement>>,
    lecture: Option<Lecture>,
    texture: Option<egui::TextureHandle>,
    boucle: bool,
    /// Position visée pendant qu'on tire le curseur.
    glisse: Option<u64>,
    /// Lire dès que le fichier est prêt.
    a_lancer: bool,
    erreur: Option<String>,
}

pub struct Visionneuse {
    cible: Option<Cible>,
    liste: Vec<Cible>,
    index: usize,
    zoom: f32,
    pan: Vec2,
    video: Option<Video>,
    /// Volume propre aux vidéos (1.0 = 100 %), mémorisé.
    pub volume: f32,
    pub muet: bool,
    /// La file « médias » du moteur vocal — la nôtre, partagée avec lui.
    pub file: Arc<File>,
    info: Option<(String, Instant)>,
    /// Ce qu'un fil de fond (enregistrer sous, copier) veut faire dire.
    avis: Arc<Mutex<Option<String>>>,
}

const HAUT: f32 = 48.0;
const BAS_VIDEO: f32 = 60.0;
const BAS_IMAGE: f32 = 30.0;

impl Visionneuse {
    pub fn new(volume: f32) -> Self {
        Self {
            cible: None,
            liste: Vec::new(),
            index: 0,
            zoom: 1.0,
            pan: Vec2::ZERO,
            video: None,
            volume: volume.clamp(0.0, 1.0),
            muet: false,
            file: File::new(),
            info: None,
            avis: Arc::new(Mutex::new(None)),
        }
    }

    /// L'emplacement où un fil de fond dépose une ligne à afficher.
    pub fn avis(&self) -> Arc<Mutex<Option<String>>> {
        self.avis.clone()
    }

    pub fn est_ouverte(&self) -> bool {
        self.cible.is_some()
    }

    /// Une vidéo est ouverte (en train de se charger, de jouer, ou en pause).
    pub fn a_une_video(&self) -> bool {
        self.video.is_some()
    }

    /// Ouvre `cible`, avec la liste des médias du salon pour ←/→.
    pub fn ouvrir(&mut self, cible: Cible, liste: Vec<Cible>) {
        self.index = liste.iter().position(|c| *c == cible).unwrap_or(0);
        self.liste = liste;
        self.montrer(cible);
    }

    fn montrer(&mut self, cible: Cible) {
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
        self.video = None;
        self.file.vider();
        let video = match &cible {
            Cible::Video(url) => Some((url.clone(), medias::chemin_cache(url))),
            Cible::Fichier(p) => Some((p.to_string_lossy().into_owned(), Some(p.clone()))),
            Cible::Image(_) => None,
        };
        if let Some((url, chemin)) = video {
            self.video = Some(Video {
                url,
                chemin,
                telechargement: None,
                lecture: None,
                texture: None,
                boucle: false,
                glisse: None,
                a_lancer: true,
                erreur: None,
            });
        }
        self.cible = Some(cible);
    }

    pub fn fermer(&mut self) {
        self.cible = None;
        self.video = None;
        self.liste.clear();
        self.file.vider();
        self.file.set_pause(true);
    }

    fn aller(&mut self, pas: isize) {
        if self.liste.len() < 2 {
            return;
        }
        let n = self.liste.len() as isize;
        self.index = ((self.index as isize + pas).rem_euclid(n)) as usize;
        let cible = self.liste[self.index].clone();
        self.montrer(cible);
    }

    /// L'adresse de la vidéo dont il faut lancer le téléchargement, s'il y
    /// en a une : l'application le fait, elle a le client HTTP épinglé.
    pub fn video_a_telecharger(&self) -> Option<String> {
        let v = self.video.as_ref()?;
        let chemin = v.chemin.as_ref()?;
        (v.telechargement.is_none() && v.lecture.is_none() && !chemin.is_file())
            .then(|| v.url.clone())
    }

    pub fn lancer_telechargement(
        &mut self,
        agent: ureq::Agent,
        cible: String,
        ctx: &egui::Context,
    ) {
        let Some(v) = self.video.as_mut() else { return };
        let Some(chemin) = v.chemin.clone() else {
            return;
        };
        v.telechargement = Some(medias::telecharger(agent, cible, chemin, ctx.clone()));
    }

    /// Une ligne d'information dans la barre du haut, quelques secondes.
    pub fn dire(&mut self, texte: impl Into<String>) {
        self.info = Some((texte.into(), Instant::now()));
    }

    /// Fait avancer la vidéo : ouvre le fichier quand il est là, prend la
    /// dernière image, lance la lecture demandée, applique le volume.
    fn preparer_video(&mut self, ctx: &egui::Context) {
        let volume = if self.muet { 0.0 } else { self.volume };
        self.file.set_gain(volume);
        let file = self.file.clone();
        let Some(v) = self.video.as_mut() else { return };
        if v.lecture.is_none() && v.erreur.is_none() {
            let Some(chemin) = v.chemin.clone() else {
                v.erreur = Some("pas de dossier de cache sur cette machine".into());
                return;
            };
            let pret = match &v.telechargement {
                Some(t) if t.fini.load(Ordering::Relaxed) => {
                    if let Some(e) = t.erreur.lock().unwrap().clone() {
                        v.erreur = Some(format!("téléchargement impossible : {e}"));
                        return;
                    }
                    true
                }
                Some(_) => false,
                None => chemin.is_file(),
            };
            if pret {
                v.telechargement = None;
                v.lecture = Some(Lecture::demarrer(chemin, file, ctx.clone()));
            }
        }
        let Some(lecture) = v.lecture.as_ref() else {
            return;
        };
        if let Some(e) = lecture.erreur() {
            v.erreur = Some(e);
        }
        if let Some(image) = lecture.prendre_image() {
            match &mut v.texture {
                Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                None => {
                    v.texture = Some(ctx.load_texture(
                        "visionneuse-video",
                        image,
                        egui::TextureOptions::LINEAR,
                    ))
                }
            }
        }
        if v.a_lancer && lecture.prete() {
            v.a_lancer = false;
            lecture.boucle(v.boucle);
            lecture.jouer(true);
        }
        if lecture.en_lecture() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    /// Peint la visionneuse si elle est ouverte. Rend ce qu'elle demande à
    /// l'application.
    pub fn ui(&mut self, ctx: &egui::Context, previews: &mut Previews) -> Vec<Demande> {
        let mut demandes = Vec::new();
        let Some(cible) = self.cible.clone() else {
            return demandes;
        };
        self.preparer_video(ctx);
        let avis = self.avis.lock().unwrap().take();
        if let Some(m) = avis {
            self.dire(m);
        }
        if self
            .info
            .as_ref()
            .is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(4))
        {
            self.info = None;
        }

        // Le clavier : espace, ←/→ — seulement quand aucun champ n'écoute.
        let libre = ctx.memory(|m| m.focused().is_none());
        let (mut espace, mut gauche, mut droite) = (false, false, false);
        if libre {
            ctx.input(|i| {
                espace = i.key_pressed(egui::Key::Space);
                gauche = i.key_pressed(egui::Key::ArrowLeft);
                droite = i.key_pressed(egui::Key::ArrowRight);
            });
        }

        let ecran = ctx.screen_rect();
        let est_video = cible.est_video();
        let locale = cible.est_locale();
        let bas = if est_video { BAS_VIDEO } else { BAS_IMAGE };
        let mut fermer = false;
        let mut aller: isize = 0;
        egui::Area::new(egui::Id::new("visionneuse"))
            .order(egui::Order::Foreground)
            .fixed_pos(ecran.min)
            .show(ctx, |ui| {
                let (rect, fond) = ui.allocate_exact_size(ecran.size(), Sense::click());
                ui.painter()
                    .rect_filled(rect, CornerRadius::ZERO, Color32::from_black_alpha(238));
                let haut = Rect::from_min_size(rect.min, Vec2::new(rect.width(), HAUT));
                let pied = Rect::from_min_size(
                    egui::pos2(rect.left(), rect.bottom() - bas),
                    Vec2::new(rect.width(), bas),
                );
                let contenu = Rect::from_min_max(
                    egui::pos2(rect.left(), haut.bottom()),
                    egui::pos2(rect.right(), pied.top()),
                );

                // --- La barre du haut ---
                let mut haut_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(haut.shrink2(Vec2::new(14.0, 6.0)))
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                {
                    let ui = &mut haut_ui;
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let icone = if est_video { Icon::Film } else { Icon::Copy };
                    ui::glyph(ui, icone, 16.0, TEXT_DIM);
                    ui.label(RichText::new(cible.nom()).color(TEXT).size(14.0).strong());
                    if self.liste.len() > 1 {
                        ui.label(
                            RichText::new(format!("{} / {}", self.index + 1, self.liste.len()))
                                .color(TEXT_FAINT)
                                .size(12.0),
                        );
                    }
                    if let Some((texte, _)) = &self.info {
                        ui.add_space(12.0);
                        ui.label(RichText::new(texte).color(ACCENT).size(12.5));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui::icon_button(ui, Icon::Close, "Fermer (Échap)").clicked() {
                            fermer = true;
                        }
                        if !locale
                            && ui::icon_button(ui, Icon::Screen, "Ouvrir dans le navigateur").clicked()
                        {
                            demandes.push(Demande::Navigateur(cible.url().to_string()));
                        }
                        if !est_video && ui::icon_button(ui, Icon::Copy, "Copier l'image").clicked()
                        {
                            demandes.push(Demande::Copier(cible.url().to_string()));
                        }
                        if locale && ui::icon_button(ui, Icon::Hash, "Voir dans le dossier").clicked() {
                            if let Cible::Fichier(p) = &cible {
                                crate::clips::montrer_dans_le_dossier(p);
                            }
                        }
                        if ui::icon_button(ui, Icon::Download, "Enregistrer sous…").clicked() {
                            demandes.push(Demande::EnregistrerSous(cible.clone()));
                        }
                    });
                }

                // --- Le média ---
                let media_rect = match &cible {
                    Cible::Image(url) => self.peindre_image(ui, contenu, url, previews),
                    Cible::Video(_) | Cible::Fichier(_) => self.peindre_video(ui, contenu),
                };

                // --- Les chevrons ---
                if self.liste.len() > 1 {
                    let y = contenu.center().y;
                    let g = Rect::from_center_size(
                        egui::pos2(contenu.left() + 28.0, y),
                        Vec2::splat(40.0),
                    );
                    let d = Rect::from_center_size(
                        egui::pos2(contenu.right() - 28.0, y),
                        Vec2::splat(40.0),
                    );
                    for (r, icone, pas, bulle) in [
                        (g, Icon::ChevronLeft, -1isize, "Précédent (←)"),
                        (d, Icon::ChevronRight, 1isize, "Suivant (→)"),
                    ] {
                        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(r));
                        if ui::icon_button_ex(&mut c, icone, 40.0, bulle, None).clicked() {
                            aller = pas;
                        }
                    }
                }

                // --- La barre du bas ---
                if est_video {
                    let mut pied_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(pied.shrink2(Vec2::new(14.0, 8.0)))
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    self.barre_video(&mut pied_ui, espace, gauche, droite);
                } else {
                    let mut pied_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(pied.shrink2(Vec2::new(14.0, 4.0)))
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    pied_ui.label(
                        RichText::new(
                            "molette : zoom · glisser : déplacer · double-clic : ajuster",
                        )
                        .color(TEXT_FAINT)
                        .size(11.5),
                    );
                    if gauche {
                        aller = -1;
                    }
                    if droite {
                        aller = 1;
                    }
                }

                // Un clic dans le vide ferme — pas sur le média ni les barres.
                if fond.clicked() {
                    if let Some(pos) = fond.interact_pointer_pos() {
                        if !media_rect.contains(pos) && !haut.contains(pos) && !pied.contains(pos) {
                            fermer = true;
                        }
                    }
                }
            });

        if aller != 0 {
            self.aller(aller);
        }
        if fermer {
            self.fermer();
        }
        demandes
    }

    /// Peint l'image ajustée au cadre, avec zoom et déplacement. Rend le
    /// rectangle occupé.
    fn peindre_image(
        &mut self,
        ui: &mut egui::Ui,
        contenu: Rect,
        url: &str,
        previews: &mut Previews,
    ) -> Rect {
        let id = ui.id().with("visionneuse-image");
        let reponse = ui.interact(contenu, id, Sense::click_and_drag());
        let Some(preview) = previews.get(ui.ctx(), url) else {
            ui.painter().text(
                contenu.center(),
                egui::Align2::CENTER_CENTER,
                "cette image ne vient pas de notre serveur",
                egui::FontId::proportional(13.0),
                TEXT_DIM,
            );
            return contenu;
        };
        let texture = match preview {
            Preview::Ready(t) => t,
            Preview::Anime(a) => a.image_a(ui.input(|i| i.time)).clone(),
            Preview::Loading => {
                ui::spinner(
                    ui.painter(),
                    contenu.center(),
                    12.0,
                    ui.input(|i| i.time),
                    TEXT_FAINT,
                );
                ui.ctx().request_repaint_after(Duration::from_millis(50));
                return contenu;
            }
            Preview::Failed => {
                ui.painter().text(
                    contenu.center(),
                    egui::Align2::CENTER_CENTER,
                    "image illisible",
                    egui::FontId::proportional(13.0),
                    TEXT_DIM,
                );
                return contenu;
            }
        };
        // Zoom à la molette, déplacement au glisser, double-clic pour ajuster.
        let molette = ui.input(|i| i.smooth_scroll_delta.y);
        if reponse.hovered() && molette != 0.0 {
            let k = (1.0 + molette / 400.0).clamp(0.5, 2.0);
            self.zoom = (self.zoom * k).clamp(1.0, 12.0);
        }
        if reponse.dragged() {
            self.pan += reponse.drag_delta();
        }
        if reponse.double_clicked() {
            self.zoom = 1.0;
            self.pan = Vec2::ZERO;
        }
        let source = texture.size_vec2();
        let ajuste = (contenu.width() / source.x)
            .min(contenu.height() / source.y)
            .min(1.0);
        let taille = source * ajuste * self.zoom;
        // Le déplacement ne sort jamais l'image du cadre.
        let jeu = ((taille - contenu.size()) * 0.5).max(Vec2::ZERO);
        self.pan = Vec2::new(
            self.pan.x.clamp(-jeu.x, jeu.x),
            self.pan.y.clamp(-jeu.y, jeu.y),
        );
        let dest = Rect::from_center_size(contenu.center() + self.pan, taille);
        let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        ui.painter()
            .with_clip_rect(contenu)
            .image(texture.id(), dest, uv, Color32::WHITE);
        if self.zoom > 1.0 {
            ui.painter().text(
                egui::pos2(contenu.right() - 10.0, contenu.bottom() - 8.0),
                egui::Align2::RIGHT_BOTTOM,
                format!("{:.0} %", self.zoom * ajuste * 100.0),
                egui::FontId::proportional(11.5),
                TEXT_FAINT,
            );
        }
        dest.intersect(contenu)
    }

    /// Peint la vidéo (ou son chargement). Rend le rectangle occupé.
    fn peindre_video(&mut self, ui: &mut egui::Ui, contenu: Rect) -> Rect {
        let Some(v) = self.video.as_mut() else {
            return contenu;
        };
        let painter = ui.painter().with_clip_rect(contenu);
        let temps = ui.input(|i| i.time);
        let mut dest = contenu;
        if let Some(t) = &v.texture {
            let source = t.size_vec2();
            let ajuste = (contenu.width() / source.x).min(contenu.height() / source.y);
            dest = Rect::from_center_size(contenu.center(), source * ajuste);
            let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            painter.image(t.id(), dest, uv, Color32::WHITE);
        }
        if let Some(e) = &v.erreur {
            painter.text(
                contenu.center(),
                egui::Align2::CENTER_CENTER,
                format!("lecture impossible : {e}"),
                egui::FontId::proportional(13.0),
                DANGER,
            );
        } else if let Some(t) = &v.telechargement {
            let centre = contenu.center();
            ui::spinner(
                &painter,
                centre - Vec2::new(0.0, 22.0),
                12.0,
                temps,
                TEXT_FAINT,
            );
            let barre = Rect::from_center_size(centre + Vec2::new(0.0, 8.0), Vec2::new(240.0, 6.0));
            painter.rect_filled(barre, CornerRadius::same(3), theme::BG_ACTIVE);
            let texte = match t.avancement() {
                Some(p) => {
                    let plein = Rect::from_min_size(
                        barre.min,
                        Vec2::new(barre.width() * p, barre.height()),
                    );
                    painter.rect_filled(plein, CornerRadius::same(3), ACCENT);
                    format!("téléchargement… {:.0} %", p * 100.0)
                }
                None => {
                    let mo = t.recu.load(Ordering::Relaxed) as f32 / (1024.0 * 1024.0);
                    format!("téléchargement… {mo:.1} Mo")
                }
            };
            painter.text(
                centre + Vec2::new(0.0, 28.0),
                egui::Align2::CENTER_CENTER,
                texte,
                egui::FontId::proportional(12.5),
                TEXT_DIM,
            );
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        } else if v.texture.is_none() {
            ui::spinner(&painter, contenu.center(), 12.0, temps, TEXT_FAINT);
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
        dest
    }

    /// La barre de lecture : lire/pause, temps, curseur, volume, boucle.
    fn barre_video(&mut self, ui: &mut egui::Ui, espace: bool, gauche: bool, droite: bool) {
        let Some(v) = self.video.as_mut() else { return };
        ui.spacing_mut().item_spacing.x = 8.0;
        let (position, duree, en_lecture, prete) = match &v.lecture {
            Some(l) => (l.position_ms(), l.duree_ms(), l.en_lecture(), l.prete()),
            None => (0, 0, false, false),
        };
        let affichee = v.glisse.unwrap_or(position);

        let icone = if en_lecture { Icon::Pause } else { Icon::Play };
        let bulle = if en_lecture {
            "Pause (espace)"
        } else {
            "Lire (espace)"
        };
        let basculer = ui::icon_button_ex(ui, icone, 34.0, bulle, Some(TEXT)).clicked() || espace;
        if basculer && prete {
            if let Some(l) = &v.lecture {
                l.jouer(!en_lecture);
            }
        }
        ui.label(
            RichText::new(format!("{} / {}", mmss(affichee), mmss(duree)))
                .color(TEXT_DIM)
                .size(12.0)
                .monospace(),
        );

        // Le curseur : ce qui reste après le volume et les deux boutons.
        let reserve = 90.0 + 34.0 + 34.0 + 34.0 + 8.0 * 5.0;
        let largeur = (ui.available_width() - reserve).max(60.0);
        let (barre, reponse) =
            ui.allocate_exact_size(Vec2::new(largeur, 24.0), Sense::click_and_drag());
        let piste = Rect::from_center_size(barre.center(), Vec2::new(barre.width(), 6.0));
        ui.painter()
            .rect_filled(piste, CornerRadius::same(3), theme::BG_ACTIVE);
        if duree > 0 {
            let p = (affichee as f32 / duree as f32).clamp(0.0, 1.0);
            let plein =
                Rect::from_min_size(piste.min, Vec2::new(piste.width() * p, piste.height()));
            ui.painter()
                .rect_filled(plein, CornerRadius::same(3), ACCENT);
            let poignee = egui::pos2(piste.left() + piste.width() * p, piste.center().y);
            let rayon = if reponse.hovered() || reponse.dragged() {
                7.0
            } else {
                5.0
            };
            ui.painter().circle_filled(poignee, rayon, TEXT);
            ui.painter()
                .circle_stroke(poignee, rayon, Stroke::new(1.0_f32, theme::BG_DEEP));
        }
        let vise = |pos: egui::Pos2| -> u64 {
            let p = ((pos.x - piste.left()) / piste.width()).clamp(0.0, 1.0);
            (duree as f32 * p) as u64
        };
        if (reponse.dragged() || reponse.drag_started()) && duree > 0 {
            if let Some(pos) = reponse.interact_pointer_pos() {
                v.glisse = Some(vise(pos));
            }
        }
        let mut chercher: Option<u64> = None;
        if reponse.drag_stopped() {
            chercher = v.glisse.take();
        } else if reponse.clicked() && duree > 0 {
            if let Some(pos) = reponse.interact_pointer_pos() {
                chercher = Some(vise(pos));
            }
        }
        if duree > 0 {
            if gauche {
                chercher = Some(position.saturating_sub(5_000));
            }
            if droite {
                chercher = Some((position + 5_000).min(duree));
            }
        }
        if let (Some(ms), Some(l)) = (chercher, &v.lecture) {
            l.chercher(ms);
        }
        reponse.on_hover_cursor(egui::CursorIcon::PointingHand);

        // Volume, boucle.
        let icone_volume = if self.muet || self.volume == 0.0 {
            Icon::HeadphonesOff
        } else {
            Icon::Volume
        };
        if ui::icon_button_ex(ui, icone_volume, 34.0, "Couper / rétablir le son", None).clicked() {
            self.muet = !self.muet;
        }
        let mut pct = self.volume * 100.0;
        let curseur = egui::Slider::new(&mut pct, 0.0..=100.0).show_value(false);
        ui.style_mut().spacing.slider_width = 90.0;
        if ui
            .add(curseur)
            .on_hover_text(format!("volume de la vidéo : {pct:.0} %"))
            .changed()
        {
            self.volume = pct / 100.0;
            self.muet = false;
        }
        let teinte = if v.boucle { Some(ACCENT) } else { None };
        if ui::icon_button_ex(ui, Icon::Repeat, 34.0, "Lire en boucle", teinte).clicked() {
            v.boucle = !v.boucle;
            if let Some(l) = &v.lecture {
                l.boucle(v.boucle);
            }
        }
    }
}

/// « 1:05 » — les secondes toujours sur deux chiffres.
pub fn mmss(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_temps_s_ecrivent_en_minutes_et_secondes() {
        assert_eq!(mmss(0), "0:00");
        assert_eq!(mmss(65_400), "1:05");
        assert_eq!(mmss(3_600_000), "60:00");
    }

    #[test]
    fn la_liste_tourne_en_rond() {
        let mut v = Visionneuse::new(0.8);
        let liste = vec![
            Cible::Image("https://s/files/a/1.png".into()),
            Cible::Image("https://s/files/a/2.png".into()),
            Cible::Image("https://s/files/a/3.png".into()),
        ];
        v.ouvrir(liste[1].clone(), liste.clone());
        assert_eq!(v.index, 1);
        v.aller(1);
        assert_eq!(v.cible, Some(liste[2].clone()));
        v.aller(1);
        assert_eq!(v.cible, Some(liste[0].clone()));
        v.aller(-1);
        assert_eq!(v.cible, Some(liste[2].clone()));
        v.fermer();
        assert!(!v.est_ouverte());
    }

    #[test]
    fn une_video_sans_cache_demande_un_telechargement() {
        let mut v = Visionneuse::new(0.8);
        let url = "https://s:8080/files/0123456789abcdef/jamais-vu.mp4".to_string();
        v.ouvrir(Cible::Video(url.clone()), vec![]);
        assert!(v.a_une_video());
        // Sur une machine sans dossier de cache, rien à télécharger non plus.
        if medias::dossier_cache().is_some() {
            assert_eq!(v.video_a_telecharger(), Some(url));
        }
    }
}
