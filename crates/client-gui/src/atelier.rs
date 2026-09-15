//! L'atelier : couper un clip, le mettre au format téléphone, lui donner un
//! titre, régler ses pistes, et l'exporter (PLAN-CLIPS.md, jalon C3).
//!
//! Tout ce qui se voit se fait ici, dans le client : l'aperçu est le
//! décodeur de la visionneuse dessiné dans un cadre 16:9 ou 9:16, le
//! recadrage un rectangle UV sur la texture, le « fond flou » une copie
//! minuscule de l'image agrandie avec filtrage — un faux flou qui suffit à
//! juger. Tout ce qui se fabrique se fait sur le serveur : la recette part
//! (bornes, cadre, titre, niveaux — jamais un filtre), ffmpeg y travaille, et
//! `export.json` dit où il en est. Puis trois gestes : enregistrer sous,
//! partager dans un salon, envoyer sur le téléphone par un QR code — un lien
//! à jeton, valable une heure.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, CornerRadius, Rect, RichText, Sense, Vec2};
use ki_protocol::ChannelId;
use ki_voice::medias::File;

use crate::clips::{self, ClipInfo, Fiche};
use crate::icons::Icon;
use crate::theme::{self, ACCENT, DANGER, TEXT, TEXT_DIM, TEXT_FAINT, WARN};
use crate::ui;
use crate::visionneuse::{mmss, Lecture};

/// De quoi parler au serveur depuis un fil.
#[derive(Clone)]
pub struct Reseau {
    pub base: String,
    pub token_hex: String,
    pub agent: ureq::Agent,
}

/// Le format de sortie.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    Original,
    Telephone,
}

/// La mise en page téléphone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cadre {
    Recadre,
    Resserre,
    FondFlou,
    Zoom,
}

/// Nombre de vignettes de la bande de temps.
const VIGNETTES: usize = 12;
/// Un export ne dépasse pas trois minutes (le serveur le refuse au-delà).
const DUREE_MAX_MS: u64 = 180_000;
const DUREE_MIN_MS: u64 = 500;
const HAUT: f32 = 48.0;
const BANDE: f32 = 96.0;
const PANNEAU: f32 = 330.0;

/// Ce que le fil d'export rapporte.
#[derive(Clone, Default)]
struct Suivi {
    phase: String,
    pour_cent: u8,
    /// L'identifiant du clip sur le serveur, dès qu'il est connu.
    id: Option<String>,
    /// Le nom du fichier produit, ou l'erreur.
    fini: Option<Result<String, String>>,
}

enum Export {
    Rien,
    EnCours(Arc<Mutex<Suivi>>),
    Pret { fichier: String },
    Erreur(String),
}

/// Ce qu'un fil rapporte quand il a fini : la valeur, ou l'erreur.
type Reponse<T> = Arc<Mutex<Option<Result<T, String>>>>;

/// Le QR code du téléphone.
struct Qr {
    texture: egui::TextureHandle,
    url: String,
    expire: Instant,
}

/// « Partager dans un salon », replié sous le bouton.
struct Repartage {
    salon: Option<ChannelId>,
    legende: String,
    envoi: Option<Reponse<()>>,
}

struct Projet {
    chemin: PathBuf,
    nom: String,
    fiche: Option<Fiche>,
    reseau: Option<Reseau>,
    lecture: Option<Lecture>,
    texture: Option<egui::TextureHandle>,
    /// La copie minuscule de l'image, pour le fond flou.
    flou: Option<egui::TextureHandle>,
    largeur: u32,
    hauteur: u32,
    duree_ms: u64,
    vignettes: Arc<Mutex<Vec<(u64, egui::ColorImage)>>>,
    textures_vignettes: Vec<(u64, egui::TextureHandle)>,
    debut_ms: u64,
    fin_ms: u64,
    /// Pour placer le lecteur au début de la coupe dès qu'il est prêt.
    a_placer: bool,
    format: Format,
    cadre: Cadre,
    /// La fenêtre 9:16, en fraction de ce qui reste (0 = bord gauche,
    /// 1 = bord droit) — et où elle finit, pour suivre l'action.
    x: f32,
    fin_x: Option<f32>,
    /// Ce que l'on déplace en glissant l'aperçu : le début ou la fin.
    glisse_fin: bool,
    /// Resserré : la part de ce qui dépasse du 9:16 que l'on garde (0 = le
    /// 9:16 pur, 1 = toute la largeur, serrée).
    serrage: f32,
    zoom: f32,
    zx: f32,
    zy: f32,
    titre: String,
    titre_bas: bool,
    jeu: f32,
    micro: f32,
    copains: f32,
    cadence: u32,
    export: Export,
    qr: Option<Qr>,
    demande_qr: Option<Reponse<String>>,
    repartage: Option<Repartage>,
    enregistrement: Option<(PathBuf, Arc<crate::medias::Telechargement>)>,
}

pub struct Atelier {
    projet: Option<Projet>,
    file: Arc<File>,
    volume: f32,
    info: Option<(String, Instant)>,
    avis: Arc<Mutex<Option<String>>>,
}

impl Atelier {
    pub fn new(file: Arc<File>, volume: f32) -> Self {
        Self {
            projet: None,
            file,
            volume,
            info: None,
            avis: Arc::new(Mutex::new(None)),
        }
    }

    pub fn est_ouvert(&self) -> bool {
        self.projet.is_some()
    }

    /// Une vidéo joue ou peut jouer : la sortie son doit exister.
    pub fn a_une_video(&self) -> bool {
        self.projet.as_ref().is_some_and(|p| p.lecture.is_some())
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
    }

    /// Ouvre un clip de la galerie. `reseau` : `None` hors connexion — on
    /// peut regarder et régler, pas exporter.
    pub fn ouvrir(&mut self, clip: &ClipInfo, reseau: Option<Reseau>, ctx: &egui::Context) {
        self.fermer();
        let vignettes = Arc::new(Mutex::new(Vec::new()));
        decoder_vignettes(clip.chemin.clone(), vignettes.clone(), ctx.clone());
        let pistes = clip
            .fiche
            .as_ref()
            .and_then(|f| f.pistes.clone())
            .unwrap_or_default();
        let a = |nom: &str| {
            if pistes.iter().any(|p| p == nom) {
                1.0
            } else {
                0.0
            }
        };
        self.projet = Some(Projet {
            chemin: clip.chemin.clone(),
            nom: clip.nom.clone(),
            fiche: clip.fiche.clone(),
            reseau,
            lecture: Some(Lecture::demarrer(
                clip.chemin.clone(),
                self.file.clone(),
                ctx.clone(),
            )),
            texture: None,
            flou: None,
            largeur: 0,
            hauteur: 0,
            duree_ms: 0,
            vignettes,
            textures_vignettes: Vec::new(),
            debut_ms: 0,
            fin_ms: 0,
            a_placer: true,
            format: Format::Telephone,
            cadre: Cadre::Recadre,
            x: 0.5,
            fin_x: None,
            glisse_fin: false,
            serrage: 0.5,
            zoom: 1.3,
            zx: 0.5,
            zy: 0.5,
            titre: String::new(),
            titre_bas: false,
            jeu: a("jeu"),
            micro: a("micro"),
            copains: a("copains"),
            cadence: 0,
            export: Export::Rien,
            qr: None,
            demande_qr: None,
            repartage: None,
            enregistrement: None,
        });
        self.info = None;
    }

    pub fn fermer(&mut self) {
        if self.projet.take().is_some() {
            self.file.vider();
            self.file.set_pause(true);
        }
    }

    fn dire(&mut self, texte: impl Into<String>) {
        self.info = Some((texte.into(), Instant::now()));
    }

    /// Peint l'atelier s'il est ouvert.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        salons: &[(ChannelId, String)],
        salon_courant: Option<ChannelId>,
    ) {
        if self.projet.is_none() {
            return;
        }
        let avis = self.avis.lock().unwrap().take();
        if let Some(m) = avis {
            self.dire(m);
        }
        if self
            .info
            .as_ref()
            .is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(5))
        {
            self.info = None;
        }
        self.file.set_gain(self.volume);
        self.avancer(ctx);

        let libre = ctx.memory(|m| m.focused().is_none());
        let espace = libre && ctx.input(|i| i.key_pressed(egui::Key::Space));

        let ecran = ctx.screen_rect();
        let mut fermer = false;
        let mut lancer_export = false;
        let mut demande_qr = false;
        let mut enregistrer = false;
        let mut repartager = false;
        let info = self.info.clone();
        let avis = self.avis.clone();
        let Some(p) = self.projet.as_mut() else {
            return;
        };
        egui::Area::new(egui::Id::new("atelier"))
            .order(egui::Order::Foreground)
            .fixed_pos(ecran.min)
            .show(ctx, |ui| {
                let (rect, _) = ui.allocate_exact_size(ecran.size(), Sense::click());
                ui.painter()
                    .rect_filled(rect, CornerRadius::ZERO, Color32::from_black_alpha(242));
                let haut = Rect::from_min_size(rect.min, Vec2::new(rect.width(), HAUT));
                let bande = Rect::from_min_size(
                    egui::pos2(rect.left(), rect.bottom() - BANDE),
                    Vec2::new(rect.width() - PANNEAU, BANDE),
                );
                let panneau =
                    Rect::from_min_max(egui::pos2(rect.right() - PANNEAU, haut.bottom()), rect.max);
                let apercu = Rect::from_min_max(
                    egui::pos2(rect.left(), haut.bottom()),
                    egui::pos2(panneau.left(), bande.top()),
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
                    ui::glyph(ui, Icon::Pencil, 16.0, TEXT_DIM);
                    ui.label(RichText::new("Atelier").color(TEXT_DIM).size(13.0));
                    ui.label(RichText::new(&p.nom).color(TEXT).size(14.0).strong());
                    if let Some((texte, _)) = &info {
                        ui.add_space(12.0);
                        ui.label(RichText::new(texte).color(ACCENT).size(12.5));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui::icon_button(ui, Icon::Close, "Fermer (Échap)").clicked() {
                            fermer = true;
                        }
                    });
                }

                // --- L'aperçu ---
                peindre_apercu(ui, p, apercu.shrink(16.0));

                // --- La bande de temps ---
                if espace {
                    basculer_lecture(p);
                }
                peindre_bande(ui, p, bande.shrink2(Vec2::new(16.0, 8.0)));

                // --- Le panneau ---
                let mut panneau_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(panneau.shrink2(Vec2::new(14.0, 10.0)))
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                egui::ScrollArea::vertical()
                    .id_salt("atelier-panneau")
                    .auto_shrink([false, false])
                    .show(&mut panneau_ui, |ui| {
                        let actions = panneau_reglages(ui, p, salons, salon_courant, &avis);
                        lancer_export |= actions.exporter;
                        demande_qr |= actions.telephone;
                        enregistrer |= actions.enregistrer;
                        repartager |= actions.repartager;
                    });
            });

        if lancer_export {
            self.lancer_export();
        }
        if demande_qr {
            self.demander_lien_telephone();
        }
        if enregistrer {
            self.enregistrer_sous(ctx);
        }
        if repartager {
            self.repartager();
        }
        if fermer {
            self.fermer();
        }
    }

    /// Ce qui avance sans nous : le lecteur, les vignettes, les fils.
    fn avancer(&mut self, ctx: &egui::Context) {
        let Some(p) = self.projet.as_mut() else {
            return;
        };
        if let Some(l) = &p.lecture {
            if let Some(e) = l.erreur() {
                self.info = Some((format!("lecture impossible : {e}"), Instant::now()));
            }
            if let Some(image) = l.prendre_image() {
                p.largeur = image.width() as u32;
                p.hauteur = image.height() as u32;
                if p.format == Format::Telephone && p.cadre == Cadre::FondFlou {
                    let petite = reduire(&image, 24);
                    match &mut p.flou {
                        Some(t) => t.set(petite, egui::TextureOptions::LINEAR),
                        None => {
                            p.flou = Some(ctx.load_texture(
                                "atelier-flou",
                                petite,
                                egui::TextureOptions::LINEAR,
                            ))
                        }
                    }
                }
                match &mut p.texture {
                    Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                    None => {
                        p.texture = Some(ctx.load_texture(
                            "atelier-image",
                            image,
                            egui::TextureOptions::LINEAR,
                        ))
                    }
                }
            }
            if l.prete() {
                if p.duree_ms == 0 {
                    p.duree_ms = l.duree_ms();
                    p.fin_ms = p.duree_ms.min(DUREE_MAX_MS);
                }
                if p.a_placer {
                    p.a_placer = false;
                    l.chercher(p.debut_ms);
                    l.jouer(true);
                }
                // La sélection tourne en boucle.
                if l.en_lecture() && l.position_ms() >= p.fin_ms && p.fin_ms > p.debut_ms {
                    l.chercher(p.debut_ms);
                }
            }
            if l.en_lecture() {
                ctx.request_repaint_after(Duration::from_millis(60));
            }
        }
        // Les vignettes décodées deviennent des textures, une fois.
        {
            let mut recues = p.vignettes.lock().unwrap();
            for (i, (t, image)) in recues.drain(..).enumerate() {
                let tex = ctx.load_texture(
                    format!("atelier-vignette-{}-{i}", t),
                    image,
                    egui::TextureOptions::LINEAR,
                );
                p.textures_vignettes.push((t, tex));
            }
        }
        p.textures_vignettes.sort_by_key(|(t, _)| *t);
        // L'export en cours.
        if let Export::EnCours(s) = &p.export {
            let s = s.lock().unwrap().clone();
            match s.fini {
                Some(Ok(fichier)) => {
                    if let Some(id) = &s.id {
                        noter_serveur(p, id);
                    }
                    p.export = Export::Pret { fichier };
                    self.info = Some(("export prêt".into(), Instant::now()));
                }
                Some(Err(e)) => {
                    if let Some(id) = &s.id {
                        noter_serveur(p, id);
                    }
                    p.export = Export::Erreur(e);
                }
                None => ctx.request_repaint_after(Duration::from_millis(300)),
            }
        }
        // Le lien du téléphone : reçu, il devient un QR code.
        if let Some(d) = &p.demande_qr {
            let reponse = d.lock().unwrap().take();
            match reponse {
                Some(Ok(url)) => {
                    p.demande_qr = None;
                    let url = adresse_joignable(url);
                    match qr_image(&url) {
                        Some(image) => {
                            let texture = ctx.load_texture(
                                "atelier-qr",
                                image,
                                egui::TextureOptions::NEAREST,
                            );
                            p.qr = Some(Qr {
                                texture,
                                url,
                                expire: Instant::now() + Duration::from_secs(3600),
                            });
                        }
                        None => {
                            self.info =
                                Some(("QR code impossible pour ce lien".into(), Instant::now()))
                        }
                    }
                }
                Some(Err(e)) => {
                    p.demande_qr = None;
                    self.info = Some((format!("lien téléphone : {e}"), Instant::now()));
                }
                None => ctx.request_repaint_after(Duration::from_millis(300)),
            }
        }
        // L'enregistrement sous.
        if let Some((chemin, t)) = &p.enregistrement {
            if t.fini.load(std::sync::atomic::Ordering::Relaxed) {
                let erreur = t.erreur.lock().unwrap().clone();
                self.info = Some((
                    match erreur {
                        Some(e) => format!("enregistrement impossible : {e}"),
                        None => format!("enregistré : {}", chemin.display()),
                    },
                    Instant::now(),
                ));
                p.enregistrement = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(300));
            }
        }
        // Le repartage.
        if let Some(r) = &mut p.repartage {
            if let Some(e) = &r.envoi {
                let fini = e.lock().unwrap().take();
                match fini {
                    Some(Ok(())) => {
                        self.info = Some(("partagé dans le salon".into(), Instant::now()));
                        p.repartage = None;
                    }
                    Some(Err(m)) => {
                        self.info = Some((format!("partage : {m}"), Instant::now()));
                        r.envoi = None;
                    }
                    None => ctx.request_repaint_after(Duration::from_millis(300)),
                }
            }
        }
    }

    /// La recette du projet, telle que le serveur la lit.
    fn recette(p: &Projet) -> serde_json::Value {
        let largeur_fenetre = (p.hauteur * 9 / 16) & !1;
        let libre = p.largeur.saturating_sub(largeur_fenetre) as f32;
        let x = (p.x * libre).round() as u32;
        let format = match p.format {
            Format::Original => serde_json::json!({ "type": "original" }),
            Format::Telephone => {
                let cadre = match p.cadre {
                    Cadre::Recadre => {
                        let mut c = serde_json::json!({ "type": "recadre", "x": x });
                        if let Some(f) = p.fin_x {
                            c["fin_x"] = serde_json::json!((f * libre).round() as u32);
                        }
                        c
                    }
                    Cadre::Resserre => {
                        let largeur = largeur_resserree(p);
                        serde_json::json!({
                            "type": "resserre",
                            "x": (p.x * p.largeur.saturating_sub(largeur) as f32).round() as u32,
                            "largeur": largeur,
                        })
                    }
                    Cadre::FondFlou => serde_json::json!({ "type": "fond_flou" }),
                    Cadre::Zoom => {
                        let l = ((largeur_fenetre as f32 / p.zoom) as u32) & !1;
                        let h = ((p.hauteur as f32 / p.zoom) as u32) & !1;
                        serde_json::json!({
                            "type": "zoom",
                            "x": (p.zx * p.largeur.saturating_sub(l) as f32).round() as u32,
                            "y": (p.zy * p.hauteur.saturating_sub(h) as f32).round() as u32,
                            "facteur": p.zoom,
                        })
                    }
                };
                serde_json::json!({ "type": "telephone", "cadre": cadre })
            }
        };
        let mut r = serde_json::json!({
            "debut_ms": p.debut_ms,
            "fin_ms": p.fin_ms,
            "format": format,
            "audio": { "jeu": p.jeu, "micro": p.micro, "copains": p.copains },
            "cadence": p.cadence,
        });
        let titre = p.titre.trim();
        if !titre.is_empty() {
            r["titre"] = serde_json::json!({ "texte": titre, "position": if p.titre_bas { "bas" } else { "haut" } });
        }
        r
    }

    /// L'export : dépôt du clip s'il n'est pas déjà sur ce serveur, recette,
    /// puis l'état relu chaque seconde — sur un fil.
    fn lancer_export(&mut self) {
        let Some(p) = self.projet.as_mut() else {
            return;
        };
        let Some(reseau) = p.reseau.clone() else {
            return;
        };
        if p.fin_ms <= p.debut_ms {
            return;
        }
        let suivi = Arc::new(Mutex::new(Suivi {
            phase: "préparation".into(),
            ..Default::default()
        }));
        p.export = Export::EnCours(suivi.clone());
        p.qr = None;
        let recette = Self::recette(p);
        let chemin = p.chemin.clone();
        let nom = format!("{}.mp4", p.nom);
        let pistes = p.fiche.as_ref().and_then(|f| f.pistes.clone());
        let deja = p
            .fiche
            .as_ref()
            .filter(|f| f.serveur_base.as_deref() == Some(reseau.base.as_str()))
            .and_then(|f| f.serveur.clone());
        std::thread::Builder::new()
            .name("atelier-export".into())
            .spawn(move || {
                let resultat = (|| -> Result<String, String> {
                    let id = match deja {
                        Some(id) => id,
                        None => {
                            suivi.lock().unwrap().phase = "dépôt du clip".into();
                            let id = deposer(&reseau, &chemin, &nom, pistes, &suivi)?;
                            suivi.lock().unwrap().id = Some(id.clone());
                            id
                        }
                    };
                    suivi.lock().unwrap().phase = "export".into();
                    let reponse = reseau
                        .agent
                        .post(&format!("{}/clips/{id}/exporter", reseau.base))
                        .set("x-ki-token", &reseau.token_hex)
                        .set("Content-Type", "application/json")
                        .timeout(Duration::from_secs(120))
                        .send_string(&recette.to_string())
                        .map_err(|e| match e {
                            ureq::Error::Status(404, _) => {
                                "le serveur n'a pas encore l'atelier (mise à jour nécessaire)"
                                    .to_string()
                            }
                            autre => crate::erreur_http(autre),
                        })?;
                    let json: serde_json::Value = reponse.into_json().map_err(|e| e.to_string())?;
                    let fichier = json["fichier"]
                        .as_str()
                        .ok_or("réponse invalide")?
                        .to_string();
                    // Puis l'état, jusqu'à la fin.
                    let debut = Instant::now();
                    loop {
                        std::thread::sleep(Duration::from_millis(900));
                        if debut.elapsed() > Duration::from_secs(900) {
                            return Err("l'export n'en finit pas".into());
                        }
                        let etat: serde_json::Value = match reseau
                            .agent
                            .get(&format!("{}/files/{id}/export.json", reseau.base))
                            .timeout(Duration::from_secs(20))
                            .call()
                        {
                            Ok(r) => r.into_json().map_err(|e| e.to_string())?,
                            Err(_) => continue,
                        };
                        let pc = etat["pour_cent"].as_u64().unwrap_or(0).min(100) as u8;
                        {
                            let mut s = suivi.lock().unwrap();
                            s.pour_cent = pc;
                            s.phase = match etat["etat"].as_str() {
                                Some("en_attente") => "en file sur le serveur".into(),
                                _ => "export".into(),
                            };
                        }
                        match etat["etat"].as_str() {
                            Some("pret") => return Ok(fichier),
                            Some("erreur") => {
                                return Err(etat["message"]
                                    .as_str()
                                    .unwrap_or("échec de l'export")
                                    .to_string())
                            }
                            _ => {}
                        }
                    }
                })();
                suivi.lock().unwrap().fini = Some(resultat);
            })
            .ok();
    }

    fn fichier_pret(p: &Projet) -> Option<(String, String)> {
        let fichier = match &p.export {
            Export::Pret { fichier } => fichier.clone(),
            _ => return None,
        };
        let id = p.fiche.as_ref().and_then(|f| f.serveur.clone())?;
        Some((id, fichier))
    }

    fn demander_lien_telephone(&mut self) {
        let Some(p) = self.projet.as_mut() else {
            return;
        };
        let (Some(reseau), Some((id, fichier))) = (p.reseau.clone(), Self::fichier_pret(p)) else {
            return;
        };
        let reponse = Arc::new(Mutex::new(None));
        p.demande_qr = Some(reponse.clone());
        std::thread::spawn(move || {
            let r = (|| -> Result<String, String> {
                let corps = serde_json::json!({ "fichier": fichier });
                let rep = reseau
                    .agent
                    .post(&format!("{}/clips/{id}/telephone", reseau.base))
                    .set("x-ki-token", &reseau.token_hex)
                    .set("Content-Type", "application/json")
                    .timeout(Duration::from_secs(30))
                    .send_string(&corps.to_string())
                    .map_err(crate::erreur_http)?;
                let json: serde_json::Value = rep.into_json().map_err(|e| e.to_string())?;
                json["url"]
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "réponse invalide".into())
            })();
            *reponse.lock().unwrap() = Some(r);
        });
    }

    fn enregistrer_sous(&mut self, ctx: &egui::Context) {
        let Some(p) = self.projet.as_mut() else {
            return;
        };
        let (Some(reseau), Some((id, fichier))) = (p.reseau.clone(), Self::fichier_pret(p)) else {
            return;
        };
        let suffixe = if fichier == "telephone.mp4" {
            "tiktok"
        } else {
            "court"
        };
        let propose = format!("{}-{suffixe}.mp4", p.nom);
        let Some(dest) = rfd::FileDialog::new().set_file_name(&propose).save_file() else {
            return;
        };
        let url = format!("{}/files/{id}/{fichier}", reseau.base);
        let suivi =
            crate::medias::telecharger(reseau.agent.clone(), url, dest.clone(), ctx.clone());
        p.enregistrement = Some((dest, suivi));
    }

    fn repartager(&mut self) {
        let Some(p) = self.projet.as_mut() else {
            return;
        };
        let (Some(reseau), Some((id, fichier))) = (p.reseau.clone(), Self::fichier_pret(p)) else {
            return;
        };
        let Some(r) = p.repartage.as_mut() else {
            return;
        };
        let Some(salon) = r.salon else { return };
        let envoi = Arc::new(Mutex::new(None));
        r.envoi = Some(envoi.clone());
        let legende = r.legende.trim().to_string();
        std::thread::spawn(move || {
            let resultat = (|| -> Result<(), String> {
                let corps =
                    serde_json::json!({ "channel": salon, "legende": legende, "fichier": fichier });
                reseau
                    .agent
                    .post(&format!("{}/clips/{id}/partager", reseau.base))
                    .set("x-ki-token", &reseau.token_hex)
                    .set("Content-Type", "application/json")
                    .timeout(Duration::from_secs(60))
                    .send_string(&corps.to_string())
                    .map_err(crate::erreur_http)?;
                Ok(())
            })();
            *envoi.lock().unwrap() = Some(resultat);
        });
    }
}

/// Le clip vient d'être déposé sur ce serveur : la fiche s'en souvient,
/// pour ne pas le renvoyer au prochain export.
fn noter_serveur(p: &mut Projet, id: &str) {
    let Some(reseau) = &p.reseau else { return };
    let mut fiche = p.fiche.clone().unwrap_or_default();
    if fiche.serveur.as_deref() == Some(id) {
        return;
    }
    fiche.serveur = Some(id.to_string());
    fiche.serveur_base = Some(reseau.base.clone());
    clips::sauver_fiche(&p.chemin, &fiche);
    p.fiche = Some(fiche);
}

/// Dépose le clip sur le serveur sans le partager (pas de salon), et rend
/// son identifiant.
fn deposer(
    reseau: &Reseau,
    chemin: &std::path::Path,
    nom: &str,
    pistes: Option<Vec<String>>,
    suivi: &Arc<Mutex<Suivi>>,
) -> Result<String, String> {
    let taille = std::fs::metadata(chemin).map_err(|e| e.to_string())?.len();
    let progres = {
        let suivi = suivi.clone();
        move |pc: u64| suivi.lock().unwrap().pour_cent = pc.min(100) as u8
    };
    let (upload, parts) = match crate::envoyer_morceaux(
        &reseau.agent,
        &reseau.base,
        &reseau.token_hex,
        chemin,
        taille,
        &progres,
    )? {
        crate::EnvoiMorceaux::Envoye { upload, parts } => (upload, parts),
        crate::EnvoiMorceaux::ServeurAncien => {
            return Err("le serveur n'a pas encore l'atelier (mise à jour nécessaire)".into())
        }
    };
    let corps = serde_json::json!({ "nom": nom, "pistes": pistes, "voix": true });
    let reponse = reseau
        .agent
        .post(&format!(
            "{}/clips/fin?upload={upload}&parts={parts}",
            reseau.base
        ))
        .set("x-ki-token", &reseau.token_hex)
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(300))
        .send_string(&corps.to_string())
        .map_err(crate::erreur_http)?;
    let json: serde_json::Value = reponse.into_json().map_err(|e| e.to_string())?;
    let id = json["id"].as_str().ok_or("réponse invalide")?.to_string();
    // Le serveur prépare la version partagée avant d'accepter un export :
    // on attend qu'il ait fini, c'est l'affaire de quelques secondes.
    suivi.lock().unwrap().phase = "préparation sur le serveur".into();
    let debut = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(800));
        if debut.elapsed() > Duration::from_secs(600) {
            return Err("le serveur n'a pas fini de préparer le clip".into());
        }
        let meta: serde_json::Value = match reseau
            .agent
            .get(&format!("{}/files/{id}/meta.json", reseau.base))
            .timeout(Duration::from_secs(20))
            .call()
        {
            Ok(r) => r.into_json().map_err(|e| e.to_string())?,
            Err(_) => continue,
        };
        match meta["etat"].as_str() {
            Some("pret") => return Ok(id),
            Some("erreur") => {
                return Err(meta["message"]
                    .as_str()
                    .unwrap_or("clip illisible par le serveur")
                    .into())
            }
            _ => {}
        }
    }
}

fn basculer_lecture(p: &mut Projet) {
    if let Some(l) = &p.lecture {
        if l.en_lecture() {
            l.jouer(false);
        } else {
            if l.position_ms() >= p.fin_ms || l.position_ms() < p.debut_ms {
                l.chercher(p.debut_ms);
            }
            l.jouer(true);
        }
    }
}

/// La position de la fenêtre 9:16 à cet instant de la coupe, en fraction.
fn x_a(p: &Projet, position_ms: u64) -> f32 {
    match p.fin_x {
        Some(fx) if p.fin_ms > p.debut_ms => {
            let t = (position_ms.saturating_sub(p.debut_ms) as f32
                / (p.fin_ms - p.debut_ms) as f32)
                .clamp(0.0, 1.0);
            p.x + (fx - p.x) * t
        }
        _ => p.x,
    }
}

/// L'aperçu : l'image dans son cadre, le titre par-dessus, et la souris
/// pour déplacer la fenêtre.
fn peindre_apercu(ui: &mut egui::Ui, p: &mut Projet, zone: Rect) {
    let painter = ui.painter().with_clip_rect(zone);
    let (Some(tex), true) = (&p.texture, p.largeur > 0 && p.hauteur > 0) else {
        ui::spinner(
            &painter,
            zone.center(),
            12.0,
            ui.input(|i| i.time),
            TEXT_FAINT,
        );
        painter.text(
            zone.center() + Vec2::new(0.0, 28.0),
            egui::Align2::CENTER_CENTER,
            "ouverture du clip…",
            egui::FontId::proportional(13.0),
            TEXT_FAINT,
        );
        return;
    };
    let (l, h) = (p.largeur as f32, p.hauteur as f32);
    let rapport = match p.format {
        Format::Original => l / h,
        Format::Telephone => 9.0 / 16.0,
    };
    let ajuste = (zone.width() / rapport).min(zone.height());
    let dest = Rect::from_center_size(zone.center(), Vec2::new(ajuste * rapport, ajuste));
    painter.rect_filled(dest, CornerRadius::same(4), Color32::BLACK);
    let position = p.lecture.as_ref().map(|l| l.position_ms()).unwrap_or(0);
    let fenetre = ((p.hauteur * 9 / 16) & !1) as f32;
    let libre = (l - fenetre).max(0.0);
    let plein = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    match (p.format, p.cadre) {
        (Format::Original, _) => {
            painter.image(tex.id(), dest, plein, Color32::WHITE);
        }
        (Format::Telephone, Cadre::Recadre) => {
            let x0 = x_a(p, position) * libre;
            let uv =
                Rect::from_min_max(egui::pos2(x0 / l, 0.0), egui::pos2((x0 + fenetre) / l, 1.0));
            painter.image(tex.id(), dest, uv, Color32::WHITE);
        }
        (Format::Telephone, Cadre::Resserre) => {
            // Une fenêtre plus large, étirée sur le cadre : c'est le serrage.
            let largeur = largeur_resserree(p) as f32;
            let x0 = p.x * (l - largeur).max(0.0);
            let uv =
                Rect::from_min_max(egui::pos2(x0 / l, 0.0), egui::pos2((x0 + largeur) / l, 1.0));
            painter.image(tex.id(), dest, uv, Color32::WHITE);
        }
        (Format::Telephone, Cadre::FondFlou) => {
            if let Some(flou) = &p.flou {
                // Couvrir le cadre en gardant le rapport de l'image.
                let echelle = (dest.width() / l).max(dest.height() / h);
                let fond =
                    Rect::from_center_size(dest.center(), Vec2::new(l * echelle, h * echelle));
                painter
                    .with_clip_rect(dest)
                    .image(flou.id(), fond, plein, Color32::from_gray(150));
            }
            let devant = Rect::from_center_size(
                dest.center(),
                Vec2::new(dest.width(), dest.width() * h / l),
            );
            painter.image(tex.id(), devant, plein, Color32::WHITE);
        }
        (Format::Telephone, Cadre::Zoom) => {
            let (fl, fh) = (fenetre / p.zoom, h / p.zoom);
            let x0 = p.zx * (l - fl).max(0.0);
            let y0 = p.zy * (h - fh).max(0.0);
            let uv = Rect::from_min_max(
                egui::pos2(x0 / l, y0 / h),
                egui::pos2((x0 + fl) / l, (y0 + fh) / h),
            );
            painter.image(tex.id(), dest, uv, Color32::WHITE);
        }
    }
    // Le titre, comme ffmpeg le posera : centré, en haut ou en bas.
    let titre = p.titre.trim();
    if !titre.is_empty() {
        let taille = (dest.height() / 26.0).max(9.0);
        let y = if p.titre_bas {
            dest.bottom() - dest.height() * 0.07
        } else {
            dest.top() + dest.height() * 0.07
        };
        let align = if p.titre_bas {
            egui::Align2::CENTER_BOTTOM
        } else {
            egui::Align2::CENTER_TOP
        };
        let police = egui::FontId::proportional(taille);
        let centre = egui::pos2(dest.center().x, y);
        for d in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
            painter.text(
                centre + Vec2::new(d.0 * 1.5, d.1 * 1.5),
                align,
                titre,
                police.clone(),
                Color32::BLACK,
            );
        }
        painter.text(centre, align, titre, police, Color32::WHITE);
    }
    painter.rect_stroke(
        dest,
        CornerRadius::same(4),
        egui::Stroke::new(1.0_f32, theme::BORDER_STRONG),
        egui::StrokeKind::Outside,
    );

    // Glisser l'image déplace la fenêtre.
    let deplacable = p.format == Format::Telephone
        && matches!(p.cadre, Cadre::Recadre | Cadre::Resserre | Cadre::Zoom);
    if deplacable {
        let r = ui.interact(
            dest,
            ui.id().with("atelier-apercu"),
            Sense::click_and_drag(),
        );
        if r.dragged() {
            let d = r.drag_delta();
            match p.cadre {
                Cadre::Recadre if libre > 0.0 => {
                    // Un pixel d'écran vaut `fenetre / dest.width()` pixels de source.
                    let dx = d.x * fenetre / dest.width() / libre;
                    if p.glisse_fin && p.fin_x.is_some() {
                        p.fin_x = p.fin_x.map(|f| (f + dx).clamp(0.0, 1.0));
                    } else {
                        p.x = (p.x + dx).clamp(0.0, 1.0);
                    }
                }
                Cadre::Resserre => {
                    let largeur = largeur_resserree(p) as f32;
                    if l > largeur {
                        p.x = (p.x + d.x * largeur / dest.width() / (l - largeur)).clamp(0.0, 1.0);
                    }
                }
                Cadre::Zoom => {
                    let (fl, fh) = (fenetre / p.zoom, h / p.zoom);
                    if l > fl {
                        p.zx = (p.zx + d.x * fl / dest.width() / (l - fl)).clamp(0.0, 1.0);
                    }
                    if h > fh {
                        p.zy = (p.zy + d.y * fh / dest.height() / (h - fh)).clamp(0.0, 1.0);
                    }
                }
                _ => {}
            }
        }
        if r.hovered() {
            ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Grab);
        }
    }
}

/// La bande de temps : vignettes, la coupe entre deux poignées, la tête
/// de lecture, et les commandes.
fn peindre_bande(ui: &mut egui::Ui, p: &mut Projet, zone: Rect) {
    let commandes = Rect::from_min_size(zone.min, Vec2::new(zone.width(), 24.0));
    let bande = Rect::from_min_max(egui::pos2(zone.left(), commandes.bottom() + 6.0), zone.max);
    let painter = ui.painter().with_clip_rect(zone);
    let duree = p.duree_ms.max(1) as f32;
    let (position, en_lecture) = p
        .lecture
        .as_ref()
        .map(|l| (l.position_ms(), l.en_lecture()))
        .unwrap_or((0, false));

    // Les commandes.
    let mut ui_c = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(commandes)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    {
        let ui = &mut ui_c;
        ui.spacing_mut().item_spacing.x = 8.0;
        let icone = if en_lecture { Icon::Pause } else { Icon::Play };
        if ui::icon_button(ui, icone, "Lecture / pause (espace)").clicked() {
            basculer_lecture(p);
        }
        ui.label(
            RichText::new(format!("{} / {}", mmss(position), mmss(p.duree_ms)))
                .color(TEXT_DIM)
                .size(12.0)
                .monospace(),
        );
        ui.add_space(12.0);
        ui.label(
            RichText::new(format!(
                "coupe : {} → {}  ({:.1} s)",
                mmss(p.debut_ms),
                mmss(p.fin_ms),
                (p.fin_ms.saturating_sub(p.debut_ms)) as f32 / 1000.0
            ))
            .color(TEXT)
            .size(12.0),
        );
        if ui
            .small_button("début ici")
            .on_hover_text("le début de la coupe = la tête de lecture")
            .clicked()
        {
            p.debut_ms = position.min(p.fin_ms.saturating_sub(DUREE_MIN_MS));
            p.fin_ms = p.fin_ms.min(p.debut_ms + DUREE_MAX_MS);
        }
        if ui
            .small_button("fin ici")
            .on_hover_text("la fin de la coupe = la tête de lecture")
            .clicked()
        {
            p.fin_ms = position
                .max(p.debut_ms + DUREE_MIN_MS)
                .min(p.duree_ms)
                .min(p.debut_ms + DUREE_MAX_MS);
        }
        ui::hint(
            ui,
            "glisse les poignées, ou clique dans la bande pour te déplacer",
        );
    }

    // Les vignettes.
    painter.rect_filled(bande, CornerRadius::same(4), theme::BG_DEEP);
    if !p.textures_vignettes.is_empty() {
        let n = p.textures_vignettes.len() as f32;
        let largeur = bande.width() / n;
        for (i, (_, tex)) in p.textures_vignettes.iter().enumerate() {
            let r = Rect::from_min_size(
                egui::pos2(bande.left() + i as f32 * largeur, bande.top()),
                Vec2::new(largeur, bande.height()),
            );
            let source = tex.size_vec2();
            let echelle = (r.width() / source.x).max(r.height() / source.y);
            let image = Rect::from_center_size(r.center(), source * echelle);
            painter.with_clip_rect(r).image(
                tex.id(),
                image,
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::from_gray(200),
            );
        }
    }
    let x_de = |ms: u64| bande.left() + bande.width() * (ms as f32 / duree).clamp(0.0, 1.0);
    let ms_de = |x: f32| ((x - bande.left()) / bande.width()).clamp(0.0, 1.0) * duree;
    // Hors de la coupe : assombri.
    let (xd, xf) = (x_de(p.debut_ms), x_de(p.fin_ms));
    painter.rect_filled(
        Rect::from_min_max(bande.min, egui::pos2(xd, bande.bottom())),
        CornerRadius::ZERO,
        Color32::from_black_alpha(170),
    );
    painter.rect_filled(
        Rect::from_min_max(egui::pos2(xf, bande.top()), bande.max),
        CornerRadius::ZERO,
        Color32::from_black_alpha(170),
    );
    painter.rect_stroke(
        Rect::from_min_max(egui::pos2(xd, bande.top()), egui::pos2(xf, bande.bottom())),
        CornerRadius::ZERO,
        egui::Stroke::new(2.0_f32, ACCENT),
        egui::StrokeKind::Inside,
    );
    // La tête de lecture.
    let xp = x_de(position);
    painter.line_segment(
        [egui::pos2(xp, bande.top()), egui::pos2(xp, bande.bottom())],
        egui::Stroke::new(2.0_f32, Color32::WHITE),
    );

    // La bande elle-même : un clic déplace la tête.
    let r = ui.interact(
        bande,
        ui.id().with("atelier-bande"),
        Sense::click_and_drag(),
    );
    // Les poignées, par-dessus.
    let poignee = |x: f32| {
        Rect::from_center_size(
            egui::pos2(x, bande.center().y),
            Vec2::new(14.0, bande.height() + 8.0),
        )
    };
    let rd = ui.interact(poignee(xd), ui.id().with("atelier-debut"), Sense::drag());
    let rf = ui.interact(poignee(xf), ui.id().with("atelier-fin"), Sense::drag());
    for (rect, actif) in [
        (poignee(xd), rd.hovered() || rd.dragged()),
        (poignee(xf), rf.hovered() || rf.dragged()),
    ] {
        painter.rect_filled(
            rect.shrink2(Vec2::new(4.0, 0.0)),
            CornerRadius::same(3),
            if actif { Color32::WHITE } else { ACCENT },
        );
    }
    if rd.dragged() {
        let ms = ms_de(rd.interact_pointer_pos().map(|q| q.x).unwrap_or(xd)) as u64;
        p.debut_ms = ms.min(p.fin_ms.saturating_sub(DUREE_MIN_MS));
        p.fin_ms = p.fin_ms.min(p.debut_ms + DUREE_MAX_MS);
        if let Some(l) = &p.lecture {
            l.chercher(p.debut_ms);
        }
    } else if rf.dragged() {
        let ms = ms_de(rf.interact_pointer_pos().map(|q| q.x).unwrap_or(xf)) as u64;
        p.fin_ms = ms
            .max(p.debut_ms + DUREE_MIN_MS)
            .min(p.duree_ms)
            .min(p.debut_ms + DUREE_MAX_MS);
        if let Some(l) = &p.lecture {
            l.chercher(p.fin_ms.saturating_sub(300));
        }
    } else if r.clicked() || r.dragged() {
        if let Some(q) = r.interact_pointer_pos() {
            let ms = ms_de(q.x) as u64;
            if let Some(l) = &p.lecture {
                l.chercher(ms);
            }
        }
    }
    if rd.hovered() || rf.hovered() || rd.dragged() || rf.dragged() {
        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::ResizeHorizontal);
    }
}

/// Ce que le panneau demande à l'atelier.
#[derive(Default)]
struct Actions {
    exporter: bool,
    telephone: bool,
    enregistrer: bool,
    repartager: bool,
}

fn panneau_reglages(
    ui: &mut egui::Ui,
    p: &mut Projet,
    salons: &[(ChannelId, String)],
    salon_courant: Option<ChannelId>,
    avis: &Arc<Mutex<Option<String>>>,
) -> Actions {
    let mut actions = Actions::default();
    ui.spacing_mut().item_spacing.y = 6.0;

    ui::section_label(ui, "Format");
    ui.horizontal(|ui| {
        ui.selectable_value(&mut p.format, Format::Telephone, "Téléphone 9:16");
        ui.selectable_value(&mut p.format, Format::Original, "Original 16:9");
    });
    if p.format == Format::Telephone {
        ui.add_space(4.0);
        ui::field_label(ui, "Mise en page");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut p.cadre, Cadre::Recadre, "Recadré");
            ui.selectable_value(&mut p.cadre, Cadre::Resserre, "Resserré");
            ui.selectable_value(&mut p.cadre, Cadre::FondFlou, "Fond flou");
            ui.selectable_value(&mut p.cadre, Cadre::Zoom, "Zoom");
        });
        match p.cadre {
            Cadre::Recadre => {
                ui.add(
                    egui::Slider::new(&mut p.x, 0.0..=1.0)
                        .show_value(false)
                        .text("position"),
                );
                let mut suivre = p.fin_x.is_some();
                if ui
                    .checkbox(&mut suivre, "suivre l'action : une position de fin")
                    .changed()
                {
                    p.fin_x = suivre.then_some(p.x);
                    p.glisse_fin = suivre;
                }
                if let Some(f) = &mut p.fin_x {
                    ui.add(
                        egui::Slider::new(f, 0.0..=1.0)
                            .show_value(false)
                            .text("position de fin"),
                    );
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("glisser l'aperçu déplace")
                                .color(TEXT_FAINT)
                                .size(11.5),
                        );
                        ui.selectable_value(&mut p.glisse_fin, false, "le début");
                        ui.selectable_value(&mut p.glisse_fin, true, "la fin");
                    });
                    ui::hint(ui, "la fenêtre glisse du début à la fin de la coupe ; l'aperçu le montre en lisant");
                } else {
                    ui::hint(ui, "glisse l'aperçu pour placer la fenêtre");
                }
            }
            Cadre::Resserre => {
                let (fenetre, total) =
                    (((p.hauteur * 9 / 16) & !1) as f32, p.largeur.max(1) as f32);
                ui.add(
                    egui::Slider::new(&mut p.serrage, 0.0..=1.0)
                        .text("largeur gardée")
                        .custom_formatter(move |v, _| {
                            format!(
                                "{:.0} %",
                                (fenetre + v as f32 * (total - fenetre)) / total * 100.0
                            )
                        }),
                );
                ui.add(
                    egui::Slider::new(&mut p.x, 0.0..=1.0)
                        .show_value(false)
                        .text("position"),
                );
                ui::hint(
                    ui,
                    "l'image est serrée dans le cadre : on garde presque tout, un peu déformé ; glisse \
                     l'aperçu pour placer la fenêtre",
                );
            }
            Cadre::FondFlou => {
                ui::hint(
                    ui,
                    "la vidéo entière au milieu, floutée et agrandie derrière",
                );
            }
            Cadre::Zoom => {
                ui.add(egui::Slider::new(&mut p.zoom, 1.0..=2.0).text("zoom"));
                ui::hint(ui, "glisse l'aperçu pour viser");
            }
        }
    } else {
        ui::hint(ui, "coupé, 1080p au plus, tel quel");
    }

    ui.add_space(8.0);
    ui::section_label(ui, "Titre");
    ui.add(
        egui::TextEdit::singleline(&mut p.titre)
            .hint_text("un titre, si tu veux")
            .desired_width(f32::INFINITY),
    );
    if p.titre.chars().count() > 80 {
        p.titre = p.titre.chars().take(80).collect();
    }
    if !p.titre.trim().is_empty() {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut p.titre_bas, false, "en haut");
            ui.selectable_value(&mut p.titre_bas, true, "en bas");
        });
    }

    ui.add_space(8.0);
    ui::section_label(ui, "Son");
    let pistes: Vec<String> = p
        .fiche
        .as_ref()
        .and_then(|f| f.pistes.clone())
        .unwrap_or_default();
    if pistes.is_empty() {
        ui::hint(
            ui,
            "pistes inconnues sur ce clip : le son est gardé tel quel",
        );
    } else {
        for nom in &pistes {
            let (v, libelle) = match nom.as_str() {
                "jeu" => (&mut p.jeu, "le jeu"),
                "micro" => (&mut p.micro, "ton micro"),
                "copains" => (&mut p.copains, "les copains"),
                _ => continue,
            };
            ui.add(
                egui::Slider::new(v, 0.0..=2.0)
                    .text(libelle)
                    .custom_formatter(|x, _| format!("{:.0} %", x * 100.0)),
            );
        }
    }

    ui.add_space(8.0);
    ui::section_label(ui, "Cadence");
    ui.horizontal(|ui| {
        ui.selectable_value(&mut p.cadence, 0, "source");
        ui.selectable_value(&mut p.cadence, 30, "30");
        ui.selectable_value(&mut p.cadence, 60, "60");
    });

    ui.add_space(12.0);
    ui::hairline(ui);
    ui.add_space(6.0);
    match &p.export {
        Export::EnCours(s) => {
            let s = s.lock().unwrap().clone();
            ui.add(
                egui::ProgressBar::new(f32::from(s.pour_cent) / 100.0)
                    .text(format!("{}… {} %", s.phase, s.pour_cent)),
            );
            ui::hint(ui, "le serveur travaille ; tu peux continuer à régler, l'export en cours ne change plus");
        }
        Export::Erreur(e) => {
            ui.label(
                RichText::new(format!("échec : {e}"))
                    .color(DANGER)
                    .size(12.0),
            );
        }
        _ => {}
    }
    let peut_exporter = p.reseau.is_some()
        && p.duree_ms > 0
        && p.fin_ms > p.debut_ms
        && !matches!(p.export, Export::EnCours(_));
    if p.reseau.is_none() {
        ui.label(
            RichText::new("connecte-toi pour exporter")
                .color(WARN)
                .size(12.0),
        );
    }
    ui.add_enabled_ui(peut_exporter, |ui| {
        if ui::primary_button(ui, Some(Icon::Film), "Exporter", None).clicked() {
            actions.exporter = true;
        }
    });
    ui::hint(
        ui,
        "l'original reste sur ce PC ; le serveur fabrique la vidéo d'après tes réglages",
    );

    if let Export::Pret { fichier } = &p.export {
        let fichier = fichier.clone();
        ui.add_space(10.0);
        ui::section_label(ui, "C'est prêt");
        ui::hint(
            ui,
            &format!(
                "{} · {}",
                if fichier == "telephone.mp4" {
                    "1080×1920, pour TikTok et Instagram"
                } else {
                    "16:9, coupé"
                },
                fichier
            ),
        );
        let en_cours = p.enregistrement.is_some();
        ui.add_enabled_ui(!en_cours, |ui| {
            if ui::button(ui, Icon::Download, "Enregistrer sous…").clicked() {
                actions.enregistrer = true;
            }
        });
        if let Some((_, t)) = &p.enregistrement {
            ui::hint(
                ui,
                &match t.avancement() {
                    Some(a) => format!("téléchargement… {:.0} %", a * 100.0),
                    None => "téléchargement…".into(),
                },
            );
        }
        // Partager dans un salon.
        let mut annuler_repartage = false;
        if p.repartage.is_none() {
            if ui::button(ui, Icon::Hash, "Partager dans un salon").clicked() {
                let salon = salon_courant
                    .filter(|id| salons.iter().any(|(s, _)| s == id))
                    .or_else(|| salons.first().map(|(id, _)| *id));
                p.repartage = Some(Repartage {
                    salon,
                    legende: String::new(),
                    envoi: None,
                });
            }
        } else if let Some(r) = p.repartage.as_mut() {
            let nom_salon = salons
                .iter()
                .find(|(id, _)| Some(*id) == r.salon)
                .map(|(_, n)| format!("#{n}"))
                .unwrap_or_else(|| "choisir…".into());
            egui::ComboBox::from_id_salt("atelier-salon")
                .selected_text(nom_salon)
                .width(200.0)
                .show_ui(ui, |ui| {
                    for (id, nom) in salons {
                        ui.selectable_value(&mut r.salon, Some(*id), format!("#{nom}"));
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut r.legende)
                    .hint_text("une légende, si tu veux")
                    .desired_width(f32::INFINITY),
            );
            let envoi = r.envoi.is_some();
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!envoi && r.salon.is_some(), |ui| {
                    if ui::primary_button(ui, Some(Icon::Hash), "Partager", None).clicked() {
                        actions.repartager = true;
                    }
                });
                if !envoi && ui::button(ui, Icon::Close, "Annuler").clicked() {
                    annuler_repartage = true;
                }
                if envoi {
                    ui.label(RichText::new("envoi…").color(TEXT_DIM).size(12.0));
                }
            });
        }
        if annuler_repartage {
            p.repartage = None;
        }
        // Le téléphone.
        let demande = p.demande_qr.is_some();
        ui.add_enabled_ui(!demande, |ui| {
            if ui::button(ui, Icon::Screen, "Envoyer sur le téléphone").clicked() {
                actions.telephone = true;
            }
        });
        if demande {
            ui::hint(ui, "lien en préparation…");
        }
        if let Some(qr) = &p.qr {
            let cote = 200.0;
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(cote), Sense::hover());
            ui.painter()
                .rect_filled(rect, CornerRadius::same(6), Color32::WHITE);
            ui.painter().image(
                qr.texture.id(),
                rect.shrink(8.0),
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            let reste = qr
                .expire
                .saturating_duration_since(Instant::now())
                .as_secs()
                / 60;
            ui::hint(
                ui,
                &format!(
                    "scanne avec l'appareil photo du téléphone, puis partage la vidéo sur TikTok ou Instagram — \
                     valable {reste} min ; le navigateur avertira une fois du certificat du serveur, continue ; \
                     serveur sur ce PC : le téléphone doit être sur le même Wi-Fi"
                ),
            );
            if ui.small_button("copier le lien").clicked() {
                ui.ctx().copy_text(qr.url.clone());
                *avis.lock().unwrap() = Some("lien copié".into());
            }
        }
    }
    actions
}

/// Les vignettes de la bande, décodées une fois à l'ouverture, sur un fil.
fn decoder_vignettes(
    chemin: PathBuf,
    sortie: Arc<Mutex<Vec<(u64, egui::ColorImage)>>>,
    ctx: egui::Context,
) {
    std::thread::Builder::new()
        .name("atelier-vignettes".into())
        .spawn(move || {
            let Ok(mut lecteur) = ki_media::ouvrir(&chemin) else {
                return;
            };
            let duree = lecteur.infos().duree_ms;
            if duree == 0 {
                return;
            }
            for i in 0..VIGNETTES {
                let cible = duree * (2 * i as u64 + 1) / (2 * VIGNETTES as u64);
                if lecteur.chercher(cible).is_err() {
                    break;
                }
                let mut image = None;
                for _ in 0..240 {
                    match lecteur.suivant(ki_media::Flux::Video) {
                        Ok(ki_media::Paquet::Image(im)) if im.pts_ms + 20 >= cible => {
                            image = Some(im);
                            break;
                        }
                        Ok(ki_media::Paquet::Image(_)) => continue,
                        _ => break,
                    }
                }
                let Some(im) = image else { break };
                let couleur = egui::ColorImage::from_rgba_unmultiplied(
                    [im.largeur as usize, im.hauteur as usize],
                    &im.rgba,
                );
                sortie.lock().unwrap().push((cible, reduire(&couleur, 160)));
                ctx.request_repaint();
            }
        })
        .ok();
}

/// Une copie réduite à `largeur` pixels de large (au plus proche), assez
/// pour une vignette ou un faux flou.
fn reduire(image: &egui::ColorImage, largeur: usize) -> egui::ColorImage {
    let [l, h] = image.size;
    if l == 0 || h == 0 || l <= largeur {
        return image.clone();
    }
    let nl = largeur.max(1);
    let nh = ((h * nl) / l).max(1);
    let mut pixels = Vec::with_capacity(nl * nh);
    for y in 0..nh {
        let sy = (y * h / nh).min(h - 1);
        for x in 0..nl {
            let sx = (x * l / nl).min(l - 1);
            pixels.push(image.pixels[sy * l + sx]);
        }
    }
    egui::ColorImage {
        size: [nl, nh],
        pixels,
        ..Default::default()
    }
}

/// La largeur de la fenêtre « resserrée », en pixels de la source : entre
/// le 9:16 pur et toute la largeur, selon le serrage.
fn largeur_resserree(p: &Projet) -> u32 {
    let fenetre = (p.hauteur * 9 / 16) & !1;
    let l = fenetre as f32 + p.serrage.clamp(0.0, 1.0) * p.largeur.saturating_sub(fenetre) as f32;
    (l.round() as u32).clamp(fenetre, p.largeur.max(fenetre)) & !1
}

/// Un lien vers 127.0.0.1 (le serveur lancé sur ce PC pour essayer) ne
/// mène nulle part depuis un téléphone : on y met l'adresse de ce PC sur
/// le réseau local — le téléphone doit être sur le même Wi-Fi.
fn adresse_joignable(url: String) -> String {
    let Some(reste) = url.strip_prefix("https://") else {
        return url;
    };
    let (hote_port, chemin) = match reste.split_once('/') {
        Some((h, c)) => (h, format!("/{c}")),
        None => (reste, String::new()),
    };
    let (hote, port) = match hote_port.rsplit_once(':') {
        Some((h, p)) => (h, format!(":{p}")),
        None => (hote_port, String::new()),
    };
    if !matches!(hote, "127.0.0.1" | "localhost" | "[::1]") {
        return url;
    }
    match ip_locale() {
        Some(ip) => format!("https://{ip}{port}{chemin}"),
        None => url,
    }
}

/// L'adresse de ce PC sur le réseau local : celle que le système choisirait
/// pour sortir — on ne fait que la lui demander, rien ne part.
fn ip_locale() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

/// Le QR code d'un lien, en image : un module = 6 pixels, une marge de
/// quatre modules — ce que les téléphones lisent sans hésiter.
fn qr_image(texte: &str) -> Option<egui::ColorImage> {
    let code = qrcode::QrCode::new(texte.as_bytes()).ok()?;
    let n = code.width();
    let couleurs = code.to_colors();
    const MODULE: usize = 6;
    const MARGE: usize = 4;
    let cote = (n + 2 * MARGE) * MODULE;
    let mut pixels = vec![Color32::WHITE; cote * cote];
    for y in 0..n {
        for x in 0..n {
            if couleurs[y * n + x] == qrcode::Color::Dark {
                for dy in 0..MODULE {
                    for dx in 0..MODULE {
                        let px = (x + MARGE) * MODULE + dx;
                        let py = (y + MARGE) * MODULE + dy;
                        pixels[py * cote + px] = Color32::BLACK;
                    }
                }
            }
        }
    }
    Some(egui::ColorImage {
        size: [cote, cote],
        pixels,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn une_image_se_reduit_en_gardant_ses_proportions() {
        let image = egui::ColorImage::filled([640, 360], Color32::RED);
        let petite = reduire(&image, 64);
        assert_eq!(petite.size, [64, 36]);
        assert!(petite.pixels.iter().all(|p| *p == Color32::RED));
        // Déjà plus petite : telle quelle.
        let mini = egui::ColorImage::filled([32, 18], Color32::BLUE);
        assert_eq!(reduire(&mini, 64).size, [32, 18]);
    }

    fn projet_d_essai() -> Projet {
        Projet {
            chemin: PathBuf::from("x.mp4"),
            nom: "x".into(),
            fiche: Some(Fiche {
                pistes: Some(vec!["jeu".into(), "micro".into(), "copains".into()]),
                ..Default::default()
            }),
            reseau: None,
            lecture: None,
            texture: None,
            flou: None,
            largeur: 1920,
            hauteur: 1080,
            duree_ms: 30_000,
            vignettes: Arc::new(Mutex::new(Vec::new())),
            textures_vignettes: Vec::new(),
            debut_ms: 1_000,
            fin_ms: 11_000,
            a_placer: false,
            format: Format::Telephone,
            cadre: Cadre::Recadre,
            x: 0.5,
            fin_x: Some(1.0),
            glisse_fin: false,
            serrage: 0.5,
            zoom: 1.5,
            zx: 0.25,
            zy: 0.5,
            titre: " ACE ".into(),
            titre_bas: true,
            jeu: 1.0,
            micro: 0.5,
            copains: 0.0,
            cadence: 30,
            export: Export::Rien,
            qr: None,
            demande_qr: None,
            repartage: None,
            enregistrement: None,
        }
    }

    /// La recette telle que le serveur la lit (`serveur/export.rs`) : les
    /// mêmes noms, les mêmes formes — c'est le contrat entre les deux.
    #[test]
    fn la_recette_parle_la_langue_du_serveur() {
        let mut p = projet_d_essai();
        let r = Atelier::recette(&p);
        assert_eq!(r["debut_ms"], 1000);
        assert_eq!(r["fin_ms"], 11000);
        assert_eq!(r["format"]["type"], "telephone");
        assert_eq!(r["format"]["cadre"]["type"], "recadre");
        // 1080 de haut → fenêtre de 606 ; 1920 − 606 = 1314 de course :
        // 657 à mi-chemin, 1314 au bout.
        assert_eq!(r["format"]["cadre"]["x"], 657);
        assert_eq!(r["format"]["cadre"]["fin_x"], 1314);
        assert_eq!(r["titre"]["texte"], "ACE");
        assert_eq!(r["titre"]["position"], "bas");
        assert_eq!(r["audio"]["micro"], 0.5);
        assert_eq!(r["audio"]["copains"], 0.0);
        assert_eq!(r["cadence"], 30);

        p.cadre = Cadre::Zoom;
        let r = Atelier::recette(&p);
        assert_eq!(r["format"]["cadre"]["type"], "zoom");
        // 606 / 1,5 = 404 et 1080 / 1,5 = 720 : x au quart de 1920 − 404,
        // y à la moitié de 1080 − 720.
        assert_eq!(r["format"]["cadre"]["x"], 379);
        assert_eq!(r["format"]["cadre"]["y"], 180);
        assert_eq!(r["format"]["cadre"]["facteur"], 1.5);

        p.cadre = Cadre::Resserre;
        let r = Atelier::recette(&p);
        assert_eq!(r["format"]["cadre"]["type"], "resserre");
        // Serrage 0,5 : 606 + 1314 / 2 = 1263 → 1262 (pair) ; x à la moitié
        // de 1920 − 1262.
        assert_eq!(r["format"]["cadre"]["largeur"], 1262);
        assert_eq!(r["format"]["cadre"]["x"], 329);

        p.cadre = Cadre::FondFlou;
        p.titre.clear();
        let r = Atelier::recette(&p);
        assert_eq!(r["format"]["cadre"]["type"], "fond_flou");
        assert!(r.get("titre").is_none());

        p.format = Format::Original;
        assert_eq!(Atelier::recette(&p)["format"]["type"], "original");
    }

    #[test]
    fn un_lien_local_devient_joignable_du_telephone() {
        // Une vraie adresse ne bouge pas.
        let distant = "https://ts.baws.fun:8080/tel/abc".to_string();
        assert_eq!(adresse_joignable(distant.clone()), distant);
        // La boucle locale devient l'adresse du PC — ou reste telle quelle
        // sans réseau ; dans les deux cas, port et chemin sont gardés.
        let local = adresse_joignable("https://127.0.0.1:8080/tel/abc".to_string());
        assert!(local.starts_with("https://"));
        assert!(local.ends_with(":8080/tel/abc"), "{local}");
        assert_eq!(
            adresse_joignable("http://127.0.0.1/x".into()),
            "http://127.0.0.1/x"
        );
    }

    #[test]
    fn un_lien_devient_un_qr_code_carre_avec_sa_marge() {
        let image =
            qr_image("https://ts.baws.fun:8080/tel/0123456789abcdef0123456789abcdef").unwrap();
        assert_eq!(image.size[0], image.size[1]);
        assert!(image.size[0] > 8 * 6 * 2);
        // La marge est blanche, et il y a du noir dedans.
        assert_eq!(image.pixels[0], Color32::WHITE);
        assert!(image.pixels.contains(&Color32::BLACK));
    }
}
