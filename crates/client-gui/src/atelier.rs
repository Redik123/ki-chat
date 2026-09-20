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
//!
//! Le fil qui dépose et exporte ne laisse jamais l'atelier sans réponse :
//! chaque attente a une borne (qui suit la durée du clip), un état visible
//! (« dépôt · morceau 3/12 », « en file sur le serveur, 2 devant », « export
//! 42 % »), et une fin en clair — succès, ou l'erreur avec le texte du
//! serveur. Chaque étape et chaque échec s'écrivent au journal
//! (`ki_voice::journal`, lignes « clips : atelier … »), pour que le prochain
//! rapport dise autre chose que « ça charge à l'infini ».

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

/// De quoi parler au serveur depuis un fil. Le jeton est **partagé** avec
/// l'application et relu à chaque requête : une reconnexion pendant les
/// minutes d'un export en tire un nouveau, et l'ancien vaudrait « jeton
/// invalide » au moment de partager.
#[derive(Clone)]
pub struct Reseau {
    pub base: String,
    pub jeton: Arc<Mutex<String>>,
    pub agent: ureq::Agent,
}

impl Reseau {
    /// Le jeton du moment, en hexadécimal.
    pub fn token_hex(&self) -> String {
        self.jeton.lock().unwrap().clone()
    }
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
    /// Le pourcentage a un sens dans cette phase (dépôt, export) ; en
    /// file, non.
    avec_pour_cent: bool,
    /// Depuis quand le fil travaille.
    debut: Option<Instant>,
    /// L'identifiant du clip sur le serveur, dès qu'il est connu — dès la
    /// réponse de `/clips/fin`, avant même que le serveur l'ait préparé :
    /// si l'attente échoue ensuite, le prochain export ne renvoie pas tout.
    id: Option<String>,
    /// Le serveur a répondu « clip inconnu » pour l'identifiant de la fiche
    /// (purgé, effacé) : la fiche doit l'oublier.
    serveur_perdu: bool,
    /// Le nom du fichier produit, ou l'erreur.
    fini: Option<Result<String, String>>,
}

impl Suivi {
    fn phase(&mut self, phase: impl Into<String>, avec_pour_cent: bool) {
        self.phase = phase.into();
        self.pour_cent = 0;
        self.avec_pour_cent = avec_pour_cent;
    }

    /// Le texte de la barre : la phase, le pourcentage s'il veut dire
    /// quelque chose, et depuis combien de temps — une barre qui ne bouge
    /// pas pendant une minute reste ainsi lisible.
    fn texte(&self) -> String {
        let mut t = self.phase.clone();
        if self.avec_pour_cent {
            t.push_str(&format!("… {} %", self.pour_cent));
        } else {
            t.push('…');
        }
        if let Some(d) = self.debut {
            let s = d.elapsed().as_secs();
            if s >= 5 {
                t.push_str(&format!("  ({})", mmss(s * 1000)));
            }
        }
        t
    }
}

/// Un 404 de `/clips/{id}/exporter`, trié : le serveur ne connaît plus ce
/// clip (purgé par âge ou par plafond, retiré par un admin), ou il n'a pas
/// la route (serveur d'avant l'atelier). Le premier se répare en
/// redéposant ; le second, en mettant le serveur à jour.
#[derive(Debug, PartialEq, Eq)]
enum Absence {
    ClipInconnu,
    RouteAbsente,
}

fn trier_404(corps: &str) -> Absence {
    if corps.contains("clip inconnu") {
        Absence::ClipInconnu
    } else {
        Absence::RouteAbsente
    }
}

/// Ce que `export.json` dit, tel que le serveur l'écrit
/// (`serveur/export.rs`) : les champs d'avant sont là, les nouveaux sont
/// facultatifs.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
struct EtatExport {
    #[serde(default)]
    etat: String,
    #[serde(default)]
    pour_cent: u8,
    #[serde(default)]
    fichier: Option<String>,
    #[serde(default)]
    message: Option<String>,
    /// Combien de tâches la fabrique traite avant celle-ci (0.1.43).
    #[serde(default)]
    derriere: Option<u32>,
    /// « copie » ou « x264 » (0.1.43).
    #[serde(default)]
    mode: Option<String>,
}

/// Ce qu'on en fait.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// On attend encore : la phase à montrer, et le pourcentage s'il compte.
    Attendre { phase: String, pour_cent: Option<u8> },
    Pret,
    Erreur(String),
}

/// L'état lu contre le fichier qu'on a demandé. Un `pret` qui parle d'un
/// autre fichier est celui d'un export précédent, pas le nôtre : on ne le
/// prend pas pour argent comptant — c'est ainsi que « Partager » répondait
/// « ce fichier n'existe pas (encore) ».
fn interpreter(etat: &EtatExport, fichier: &str) -> Verdict {
    let le_notre = etat.fichier.as_deref().is_none_or(|f| f == fichier);
    match etat.etat.as_str() {
        "pret" if le_notre => Verdict::Pret,
        "erreur" if le_notre => Verdict::Erreur(
            etat.message
                .clone()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or_else(|| "échec de l'export".into()),
        ),
        "en_attente" => Verdict::Attendre {
            phase: match etat.derriere {
                Some(n) if n > 0 => format!("en file sur le serveur, {n} devant"),
                _ => "en file sur le serveur".into(),
            },
            pour_cent: None,
        },
        "en_cours" => Verdict::Attendre {
            phase: match etat.mode.as_deref() {
                Some("copie") => "export (coupe sans réencodage)".into(),
                _ => "export".into(),
            },
            pour_cent: Some(etat.pour_cent.min(100)),
        },
        // Un état d'un autre export, ou inconnu : le serveur n'a pas encore
        // écrit le nôtre.
        _ => Verdict::Attendre {
            phase: "en attente du serveur".into(),
            pour_cent: None,
        },
    }
}

/// Une fiche relue en boucle (`meta.json`, `export.json`) : `Ok(None)` tant
/// qu'elle manque ou que le serveur ne répond pas, `Err` quand ça dure.
struct Relecture {
    /// Depuis quand on ne reçoit que des 404.
    absente_depuis: Option<Instant>,
    /// Depuis quand le serveur ne répond pas (réseau, 5xx).
    muet_depuis: Option<Instant>,
}

/// Une fiche introuvable au-delà de ça, c'est un serveur qui ne l'a pas
/// écrite (disque plein, droits) — pas un délai.
const FICHE_ABSENTE_MAX: Duration = Duration::from_secs(20);
/// Un serveur muet au-delà de ça, c'est une panne, pas un hoquet.
const SERVEUR_MUET_MAX: Duration = Duration::from_secs(120);

impl Relecture {
    fn new() -> Self {
        Self { absente_depuis: None, muet_depuis: None }
    }

    /// Relit `url` ; rend le JSON s'il est là.
    fn lire(&mut self, reseau: &Reseau, url: &str, quoi: &str) -> Result<Option<serde_json::Value>, String> {
        match reseau.agent.get(url).timeout(Duration::from_secs(20)).call() {
            Ok(r) => {
                self.absente_depuis = None;
                self.muet_depuis = None;
                r.into_json().map(Some).map_err(|e| format!("{quoi} illisible : {e}"))
            }
            Err(ureq::Error::Status(404, _)) => {
                let depuis = *self.absente_depuis.get_or_insert_with(Instant::now);
                if depuis.elapsed() > FICHE_ABSENTE_MAX {
                    return Err(format!(
                        "le serveur n'a pas enregistré {quoi} (introuvable depuis {} s) — disque plein ou dossier illisible sur le serveur, préviens un admin",
                        depuis.elapsed().as_secs()
                    ));
                }
                Ok(None)
            }
            Err(e) => {
                let depuis = *self.muet_depuis.get_or_insert_with(Instant::now);
                if depuis.elapsed() > SERVEUR_MUET_MAX {
                    return Err(format!("le serveur ne répond plus : {}", crate::erreur_http(e)));
                }
                Ok(None)
            }
        }
    }
}

/// Le temps qu'on accorde au serveur pour préparer ou exporter : un socle,
/// plus quinze fois la durée de ce qu'il a à faire (un conteneur à un cœur
/// réencode à peine plus vite que le temps réel, et il y a parfois une file
/// devant), plafonné à une heure. Le serveur, lui, s'arrête à dix fois.
fn delai_pour(duree_ms: u64) -> Duration {
    let variable = Duration::from_secs((duree_ms / 1000).saturating_mul(15));
    (Duration::from_secs(600) + variable).min(Duration::from_secs(3600))
}

/// Un export dont l'état ne bouge plus pendant ça — ni le pourcentage, ni
/// l'état — est tenu pour bloqué. Sauf en file : voir [`avance`].
const EXPORT_FIGE_MAX: Duration = Duration::from_secs(900);

/// L'état relu compte-t-il comme « ça bouge » ? Tout changement, oui. Et
/// toute place en file, même identique : le serveur n'écrit `en_attente`
/// (avec `derriere`) **qu'une fois**, au dépôt, et ne réécrit l'état qu'à
/// la prise en charge — un export « fond flou » de trois minutes devant,
/// sur le conteneur, c'est un quart d'heure sans que rien ne change. Là,
/// seule la borne globale (`delai_pour`) compte ; déclarer « l'export
/// n'avance plus » faisait abandonner le fil pendant que le serveur allait
/// bel et bien faire l'export.
fn avance(dernier: Option<&EtatExport>, etat: &EtatExport) -> bool {
    etat.etat == "en_attente" || dernier != Some(etat)
}

fn journal(texte: String) {
    ki_voice::journal(format!("clips : atelier : {texte}"));
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
        // L'export en cours. Tant qu'il tourne, on repeint sans attendre la
        // souris : la barre et son chrono avancent seuls.
        if let Export::EnCours(s) = &p.export {
            let s = s.lock().unwrap().clone();
            match &s.fini {
                Some(Ok(fichier)) => {
                    relire_fiche(p, &s);
                    p.export = Export::Pret { fichier: fichier.clone() };
                    self.info = Some(("export prêt".into(), Instant::now()));
                }
                Some(Err(e)) => {
                    relire_fiche(p, &s);
                    p.export = Export::Erreur(e.clone());
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
            debut: Some(Instant::now()),
            ..Default::default()
        }));
        p.export = Export::EnCours(suivi.clone());
        p.qr = None;
        let recette = Self::recette(p);
        let chemin = p.chemin.clone();
        let nom = format!("{}.mp4", p.nom);
        let nom_court = p.nom.clone();
        let pistes = p.fiche.as_ref().and_then(|f| f.pistes.clone());
        let duree_clip_ms = p.duree_ms;
        let duree_coupe_ms = p.fin_ms - p.debut_ms;
        let deja = p
            .fiche
            .as_ref()
            .filter(|f| f.serveur_base.as_deref() == Some(reseau.base.as_str()))
            .and_then(|f| f.serveur.clone());
        std::thread::Builder::new()
            .name("atelier-export".into())
            .spawn(move || {
                let debut = Instant::now();
                journal(format!(
                    "export de {nom_court} : {} ms → {} ms, {}",
                    recette["debut_ms"],
                    recette["fin_ms"],
                    match deja.as_deref() {
                        Some(id) => format!("déjà sur le serveur (clip {id})"),
                        None => "à déposer d'abord".into(),
                    }
                ));
                let depot = Depot { reseau: &reseau, chemin: &chemin, nom: &nom, pistes, duree_ms: duree_clip_ms };
                // Le serveur ne connaît plus ce clip, ou n'a pas su le
                // préparer : la fiche l'oublie tout de suite (sur le disque,
                // l'atelier peut être fermé), et on le redépose une fois.
                let perdu = |pourquoi: &str| {
                    journal(format!("export de {nom_court} : {pourquoi}, redépôt"));
                    suivi.lock().unwrap().serveur_perdu = true;
                    clips::oublier_serveur(&chemin);
                };
                let resultat = (|| -> Result<String, String> {
                    let id = match deja {
                        // Déjà sur le serveur d'après la fiche — mais prêt ?
                        // Un partage récent le prépare encore ; un partage
                        // d'hier a pu rater ou être purgé.
                        Some(id) => match depot.attendre_pret(&id, &suivi, true) {
                            Ok(()) => id,
                            Err(Attente::Perdu(pourquoi)) => {
                                perdu(&pourquoi);
                                depot.deposer(&suivi)?
                            }
                            Err(Attente::Echec(e)) => return Err(e),
                        },
                        None => depot.deposer(&suivi)?,
                    };
                    let (id, fichier) = match demander_export(&reseau, &id, &recette, &suivi) {
                        Ok(fichier) => (id, fichier),
                        Err(Echec::ClipInconnu) => {
                            perdu(&format!("le serveur ne connaît plus le clip {id}"));
                            let id = depot.deposer(&suivi)?;
                            let fichier = demander_export(&reseau, &id, &recette, &suivi)
                                .map_err(|e| e.texte())?;
                            (id, fichier)
                        }
                        // Un export tourne déjà sur ce clip (le nôtre, lancé
                        // d'un atelier fermé depuis) : on le suit au lieu
                        // d'échouer — et de tout refaire à la relance.
                        Err(Echec::DejaEnCours) => {
                            let fichier = fichier_en_cours(&reseau, &id)
                                .unwrap_or_else(|| nom_sortie(&recette).to_string());
                            journal(format!(
                                "export de {nom_court} : un export ({fichier}) est déjà en cours sur le clip {id}, on le suit"
                            ));
                            (id, fichier)
                        }
                        Err(e) => return Err(e.texte()),
                    };
                    suivre_export(&reseau, &id, &fichier, duree_coupe_ms, &suivi)?;
                    Ok(fichier)
                })();
                match &resultat {
                    Ok(f) => journal(format!(
                        "export de {nom_court} : {f} prêt en {}",
                        mmss(debut.elapsed().as_millis() as u64)
                    )),
                    Err(e) => journal(format!(
                        "export de {nom_court} : échec après {} : {e}",
                        mmss(debut.elapsed().as_millis() as u64)
                    )),
                }
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
                    .set("x-ki-token", &reseau.token_hex())
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
                    .set("x-ki-token", &reseau.token_hex())
                    .set("Content-Type", "application/json")
                    .timeout(Duration::from_secs(60))
                    .send_string(&corps.to_string())
                    .map_err(|e| match e {
                        // Le jeton est relu à chaque requête : un 401 ici,
                        // c'est qu'on n'est plus connecté du tout.
                        ureq::Error::Status(401, _) => {
                            "la session a été perdue entre-temps — reconnecte-toi, puis ferme et rouvre l'atelier"
                                .to_string()
                        }
                        autre => crate::erreur_http(autre),
                    })?;
                Ok(())
            })();
            match &resultat {
                Ok(()) => journal(format!("{fichier} du clip {id} partagé dans le salon {salon}")),
                Err(e) => journal(format!("partage de {fichier} (clip {id}) dans le salon {salon} : échec : {e}")),
            }
            *envoi.lock().unwrap() = Some(resultat);
        });
    }
}

/// Un export refusé par le serveur.
enum Echec {
    /// 404 « clip inconnu » : à redéposer.
    ClipInconnu,
    /// 409 « un export est déjà en cours sur ce clip » : à suivre.
    DejaEnCours,
    Autre(String),
}

impl Echec {
    fn texte(self) -> String {
        match self {
            Echec::ClipInconnu => "le serveur ne connaît plus ce clip".into(),
            Echec::DejaEnCours => "un export est déjà en cours sur ce clip".into(),
            Echec::Autre(t) => t,
        }
    }
}

/// Le fichier que produira une recette — ce que le serveur répond à
/// `/exporter` (`serveur/export.rs::nom_sortie`), quand on n'a pas pu le lui
/// demander.
fn nom_sortie(recette: &serde_json::Value) -> &'static str {
    match recette["format"]["type"].as_str() {
        Some("telephone") => "telephone.mp4",
        _ => "export.mp4",
    }
}

/// Le fichier de l'export que le serveur dit « déjà en cours » : celui que
/// nomme son `export.json`, s'il se lit.
fn fichier_en_cours(reseau: &Reseau, id: &str) -> Option<String> {
    let url = format!("{}/files/{id}/export.json", reseau.base);
    let json: serde_json::Value = reseau.agent.get(&url).timeout(Duration::from_secs(20)).call().ok()?.into_json().ok()?;
    let etat: EtatExport = serde_json::from_value(json).ok()?;
    matches!(etat.etat.as_str(), "en_attente" | "en_cours")
        .then_some(etat.fichier)
        .flatten()
}

/// `POST /clips/{id}/exporter` : rend le nom du fichier que le serveur
/// produira.
fn demander_export(
    reseau: &Reseau,
    id: &str,
    recette: &serde_json::Value,
    suivi: &Arc<Mutex<Suivi>>,
) -> Result<String, Echec> {
    suivi.lock().unwrap().phase("demande d'export", false);
    let reponse = reseau
        .agent
        .post(&format!("{}/clips/{id}/exporter", reseau.base))
        .set("x-ki-token", &reseau.token_hex())
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(120))
        .send_string(&recette.to_string())
        .map_err(|e| match e {
            ureq::Error::Status(404, r) => {
                let corps = r.into_string().unwrap_or_default();
                match trier_404(&corps) {
                    Absence::ClipInconnu => Echec::ClipInconnu,
                    Absence::RouteAbsente => Echec::Autre(
                        "le serveur n'a pas encore l'atelier (mise à jour nécessaire)".into(),
                    ),
                }
            }
            ureq::Error::Status(409, r) => {
                let corps = r.into_string().unwrap_or_default();
                let corps = corps.trim();
                if corps.contains("déjà en cours") {
                    Echec::DejaEnCours
                } else if corps.contains("pas prêt") {
                    // On a pourtant relu `pret` juste avant : la
                    // préparation a été refaite entre-temps (reprise après
                    // un redémarrage du serveur).
                    Echec::Autre(format!("{corps} — le serveur le prépare de nouveau ; réessaie dans quelques minutes"))
                } else if corps.is_empty() {
                    Echec::Autre("le serveur répond 409".into())
                } else {
                    Echec::Autre(corps.chars().take(200).collect())
                }
            }
            ureq::Error::Status(401, _) => Echec::Autre(
                "la session a été perdue entre-temps — reconnecte-toi, puis ferme et rouvre l'atelier"
                    .into(),
            ),
            autre => Echec::Autre(crate::erreur_http(autre)),
        })?;
    let json: serde_json::Value = reponse
        .into_json()
        .map_err(|e| Echec::Autre(format!("réponse illisible : {e}")))?;
    let fichier = json["fichier"]
        .as_str()
        .ok_or_else(|| Echec::Autre("réponse invalide".into()))?
        .to_string();
    journal(format!("clip {id} : export accepté ({fichier})"));
    Ok(fichier)
}

/// Relit `export.json` jusqu'à `pret` ou `erreur`. Borné trois fois : par
/// le délai qui suit la durée de la coupe, par un état qui ne bouge plus,
/// et par une fiche introuvable ou un serveur muet ([`Relecture`]).
fn suivre_export(
    reseau: &Reseau,
    id: &str,
    fichier: &str,
    duree_coupe_ms: u64,
    suivi: &Arc<Mutex<Suivi>>,
) -> Result<(), String> {
    suivi.lock().unwrap().phase("en attente du serveur", false);
    let url = format!("{}/files/{id}/export.json", reseau.base);
    let debut = Instant::now();
    let delai = delai_pour(duree_coupe_ms);
    let mut relecture = Relecture::new();
    let mut dernier: Option<EtatExport> = None;
    let mut fige_depuis = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(900));
        if debut.elapsed() > delai {
            return Err(format!(
                "l'export n'a pas fini en {} — le serveur est trop lent ou saturé ; il continue de son côté, ferme et rouvre l'atelier plus tard",
                mmss(delai.as_millis() as u64)
            ));
        }
        let Some(json) = relecture.lire(reseau, &url, "l'état de l'export")? else {
            continue;
        };
        let etat: EtatExport = serde_json::from_value(json).unwrap_or_default();
        if avance(dernier.as_ref(), &etat) {
            fige_depuis = Instant::now();
            dernier = Some(etat.clone());
        } else if fige_depuis.elapsed() > EXPORT_FIGE_MAX {
            return Err(format!(
                "l'export n'avance plus depuis {} ({}) — le serveur est peut-être saturé ; ferme et rouvre l'atelier plus tard",
                mmss(fige_depuis.elapsed().as_millis() as u64),
                suivi.lock().unwrap().phase
            ));
        }
        match interpreter(&etat, fichier) {
            Verdict::Attendre { phase, pour_cent } => {
                let mut s = suivi.lock().unwrap();
                if s.phase != phase {
                    s.phase(phase, pour_cent.is_some());
                }
                if let Some(pc) = pour_cent {
                    s.pour_cent = pc;
                }
            }
            Verdict::Pret => return Ok(()),
            Verdict::Erreur(m) => return Err(m),
        }
    }
}

/// Le fil a fini : la fiche sur le disque est la bonne — c'est lui qui l'a
/// écrite, au moment où il a su l'identifiant (`clips::noter_serveur`) ou
/// que le serveur ne le connaissait plus (`clips::oublier_serveur`), que
/// l'atelier soit resté ouvert ou non. On la relit ; sans dossier de
/// fiches (rien d'écrit), on applique en mémoire ce que le fil a appris,
/// pour que « Téléphone » et « Partager » trouvent l'identifiant.
fn relire_fiche(p: &mut Projet, s: &Suivi) {
    if let Some(fiche) = clips::lire_fiche(&p.chemin) {
        p.fiche = Some(fiche);
        return;
    }
    let mut fiche = p.fiche.clone().unwrap_or_default();
    if s.serveur_perdu {
        fiche.serveur = None;
        fiche.serveur_base = None;
    }
    if let (Some(id), Some(reseau)) = (&s.id, &p.reseau) {
        fiche.serveur = Some(id.clone());
        fiche.serveur_base = Some(reseau.base.clone());
    }
    p.fiche = Some(fiche);
}

/// Le dépôt d'un clip sur le serveur, sans le partager (pas de salon).
struct Depot<'a> {
    reseau: &'a Reseau,
    chemin: &'a std::path::Path,
    nom: &'a str,
    pistes: Option<Vec<String>>,
    /// La durée du clip : le temps de préparation en dépend.
    duree_ms: u64,
}

/// Pourquoi un clip que l'on croyait sur le serveur n'y est pas prêt.
enum Attente {
    /// Le serveur ne l'a plus (purgé, retiré), ou n'a pas su le préparer :
    /// la fiche doit l'oublier, et un nouveau dépôt lui redonne sa chance.
    Perdu(String),
    /// Tout le reste : le texte à montrer.
    Echec(String),
}

impl Attente {
    fn texte(self) -> String {
        match self {
            Attente::Perdu(t) | Attente::Echec(t) => t,
        }
    }
}

impl Depot<'_> {
    /// Envoie les morceaux, demande l'assemblage, puis attend que le
    /// serveur ait préparé la version partagée (l'export l'exige). Rend
    /// l'identifiant — noté dans le suivi **et dans la fiche sur le disque**
    /// dès la réponse de `/clips/fin`, avant l'attente : si celle-ci échoue,
    /// ou si l'atelier a été fermé entre-temps, le clip est quand même là et
    /// le prochain export ne le renverra pas.
    fn deposer(&self, suivi: &Arc<Mutex<Suivi>>) -> Result<String, String> {
        let reseau = self.reseau;
        let taille = std::fs::metadata(self.chemin).map_err(|e| e.to_string())?.len();
        suivi.lock().unwrap().phase("dépôt du clip", true);
        journal(format!("dépôt de {} ({} Mo)", self.nom, taille / (1024 * 1024)));
        let progres = {
            let suivi = suivi.clone();
            move |pc: u64, morceau: u32, total: u32| {
                let mut s = suivi.lock().unwrap();
                s.pour_cent = pc.min(100) as u8;
                s.phase = format!("dépôt du clip · morceau {morceau}/{total}");
            }
        };
        let (upload, parts) = match crate::envoyer_morceaux(
            &reseau.agent,
            &reseau.base,
            &|| reseau.token_hex(),
            self.chemin,
            taille,
            &progres,
        )? {
            crate::EnvoiMorceaux::Envoye { upload, parts } => (upload, parts),
            crate::EnvoiMorceaux::ServeurAncien => {
                return Err("le serveur n'a pas encore l'atelier (mise à jour nécessaire)".into())
            }
        };
        suivi.lock().unwrap().phase("assemblage sur le serveur", false);
        let corps = serde_json::json!({ "nom": self.nom, "pistes": self.pistes, "voix": true });
        let reponse = reseau
            .agent
            .post(&format!(
                "{}/clips/fin?upload={upload}&parts={parts}",
                reseau.base
            ))
            .set("x-ki-token", &reseau.token_hex())
            .set("Content-Type", "application/json")
            .timeout(Duration::from_secs(300))
            .send_string(&corps.to_string())
            .map_err(|e| {
                let code = match &e {
                    ureq::Error::Status(c, _) => c.to_string(),
                    _ => "réseau".into(),
                };
                let texte = match e {
                    ureq::Error::Status(401, _) => crate::SESSION_PERDUE.to_string(),
                    ureq::Error::Status(404, _) => {
                        "le serveur n'a pas encore l'atelier (mise à jour nécessaire)".to_string()
                    }
                    autre => crate::erreur_http(autre),
                };
                journal(format!("dépôt de {} : /clips/fin refusé ({code}) : {texte}", self.nom));
                texte
            })?;
        let json: serde_json::Value = reponse.into_json().map_err(|e| e.to_string())?;
        let id = json["id"].as_str().ok_or("réponse invalide")?.to_string();
        suivi.lock().unwrap().id = Some(id.clone());
        clips::noter_serveur(self.chemin, &reseau.base, &id);
        journal(format!("dépôt de {} : reçu (clip {id}), préparation sur le serveur", self.nom));
        self.attendre_pret(&id, suivi, false).map_err(Attente::texte)?;
        Ok(id)
    }

    /// Attend que le serveur ait préparé la version partagée du clip `id`
    /// (`meta.json` à `pret`) : quelques secondes sur un PC, des minutes sur
    /// un conteneur à un cœur pour un clip « Haute ». `deja` : le clip
    /// n'est pas de ce fil mais d'un partage ou d'un export précédent — la
    /// fiche le croit sur le serveur ; alors une fiche introuvable, ou
    /// laissée en erreur par une préparation ratée (« ffmpeg : délai
    /// dépassé »), n'est pas un échec mais un clip [`Attente::Perdu`], à
    /// redéposer. Sans cette relecture, le chemin rapide envoyait
    /// `/exporter` tout de suite et recevait « le clip n'est pas prêt »,
    /// sans dire d'attendre, et sans issue si la préparation avait échoué.
    fn attendre_pret(&self, id: &str, suivi: &Arc<Mutex<Suivi>>, deja: bool) -> Result<(), Attente> {
        let reseau = self.reseau;
        suivi.lock().unwrap().phase("préparation sur le serveur", false);
        let url = format!("{}/files/{id}/meta.json", reseau.base);
        let mut relecture = Relecture::new();
        if deja {
            // Un premier regard, sans patience : ce qu'un clip d'hier a à
            // dire, il le dit tout de suite.
            match reseau.agent.get(&url).timeout(Duration::from_secs(20)).call() {
                Ok(r) => {
                    let meta: serde_json::Value = r
                        .into_json()
                        .map_err(|e| Attente::Echec(format!("la fiche du clip est illisible : {e}")))?;
                    match juger_meta(&meta) {
                        Presence::Pret => return Ok(()),
                        Presence::Erreur(m) => {
                            return Err(Attente::Perdu(format!(
                                "le serveur n'avait pas su préparer le clip {id} ({m})"
                            )))
                        }
                        Presence::EnPreparation => {}
                    }
                }
                Err(ureq::Error::Status(404, _)) => {
                    return Err(Attente::Perdu(format!("le serveur ne connaît plus le clip {id}")))
                }
                // Réseau, 5xx : la boucle en dessous saura patienter.
                Err(_) => {}
            }
        }
        let debut = Instant::now();
        let delai = delai_pour(self.duree_ms);
        loop {
            std::thread::sleep(Duration::from_millis(800));
            if debut.elapsed() > delai {
                return Err(Attente::Echec(format!(
                    "le serveur n'a pas fini de préparer le clip en {} — il est trop lent ou saturé ; le clip y est, réessaie plus tard sans le renvoyer",
                    mmss(delai.as_millis() as u64)
                )));
            }
            let Some(meta) = relecture.lire(reseau, &url, "la fiche du clip").map_err(Attente::Echec)? else {
                continue;
            };
            match juger_meta(&meta) {
                Presence::Pret => return Ok(()),
                Presence::Erreur(m) => return Err(Attente::Echec(m)),
                Presence::EnPreparation => {}
            }
        }
    }
}

/// Ce que `meta.json` dit du clip.
#[derive(Debug, PartialEq, Eq)]
enum Presence {
    Pret,
    EnPreparation,
    /// Le message du serveur, ou un texte à nous.
    Erreur(String),
}

fn juger_meta(meta: &serde_json::Value) -> Presence {
    match meta["etat"].as_str() {
        Some("pret") => Presence::Pret,
        Some("erreur") => Presence::Erreur(
            meta["message"]
                .as_str()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or("clip illisible par le serveur")
                .into(),
        ),
        _ => Presence::EnPreparation,
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
            let barre = egui::ProgressBar::new(f32::from(s.pour_cent) / 100.0).text(s.texte());
            ui.add(if s.avec_pour_cent { barre } else { barre.animate(true) });
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

    /// Un 404 de `/exporter` n'est pas toujours « mets le serveur à jour » :
    /// « clip inconnu », c'est un clip purgé, à redéposer.
    #[test]
    fn un_404_se_trie_entre_clip_purge_et_serveur_d_avant() {
        assert_eq!(trier_404("clip inconnu"), Absence::ClipInconnu);
        assert_eq!(trier_404(""), Absence::RouteAbsente);
        assert_eq!(trier_404("<html>404 Not Found</html>"), Absence::RouteAbsente);
    }

    /// `export.json` tel que le serveur l'écrit, d'hier et d'aujourd'hui,
    /// et ce qu'on en montre.
    #[test]
    fn l_etat_d_export_se_lit_et_se_juge_contre_le_fichier_demande() {
        // Un serveur 0.1.42 : pas de `derriere` ni de `mode`.
        let ancien: EtatExport = serde_json::from_str(
            r#"{"etat":"en_cours","pour_cent":42,"fichier":"telephone.mp4","message":null,"duree_s":0.0,"largeur":0,"hauteur":0,"taille":0}"#,
        )
        .unwrap();
        assert_eq!(
            interpreter(&ancien, "telephone.mp4"),
            Verdict::Attendre { phase: "export".into(), pour_cent: Some(42) }
        );
        // Un serveur 0.1.43 : la place en file, le mode, l'horodatage.
        let en_file: EtatExport = serde_json::from_str(
            r#"{"etat":"en_attente","pour_cent":0,"fichier":"export.mp4","derriere":2,"depuis":1758400000}"#,
        )
        .unwrap();
        assert_eq!(
            interpreter(&en_file, "export.mp4"),
            Verdict::Attendre { phase: "en file sur le serveur, 2 devant".into(), pour_cent: None }
        );
        let copie: EtatExport =
            serde_json::from_str(r#"{"etat":"en_cours","pour_cent":7,"fichier":"export.mp4","mode":"copie"}"#).unwrap();
        assert!(matches!(interpreter(&copie, "export.mp4"), Verdict::Attendre { pour_cent: Some(7), .. }));
        // Prêt — mais seulement pour le fichier demandé : le `pret` d'un
        // export précédent (l'autre nom) n'est pas le nôtre.
        let pret: EtatExport =
            serde_json::from_str(r#"{"etat":"pret","pour_cent":100,"fichier":"telephone.mp4"}"#).unwrap();
        assert_eq!(interpreter(&pret, "telephone.mp4"), Verdict::Pret);
        assert!(matches!(interpreter(&pret, "export.mp4"), Verdict::Attendre { .. }));
        // L'erreur porte le texte du serveur, ou un texte à nous.
        let erreur: EtatExport =
            serde_json::from_str(r#"{"etat":"erreur","fichier":"export.mp4","message":"ffmpeg : délai dépassé"}"#).unwrap();
        assert_eq!(interpreter(&erreur, "export.mp4"), Verdict::Erreur("ffmpeg : délai dépassé".into()));
        let muette: EtatExport = serde_json::from_str(r#"{"etat":"erreur"}"#).unwrap();
        assert_eq!(interpreter(&muette, "export.mp4"), Verdict::Erreur("échec de l'export".into()));
        // N'importe quoi : on attend, pas de plantage.
        let vide: EtatExport = serde_json::from_str("{}").unwrap();
        assert!(matches!(interpreter(&vide, "export.mp4"), Verdict::Attendre { .. }));
    }

    /// En file, un état relu identique n'est pas un export figé : le
    /// serveur n'écrit `en_attente` qu'une fois. Sous ffmpeg, si.
    #[test]
    fn en_file_un_etat_identique_n_est_pas_fige() {
        let en_file: EtatExport =
            serde_json::from_str(r#"{"etat":"en_attente","fichier":"export.mp4","derriere":1}"#).unwrap();
        assert!(avance(None, &en_file));
        assert!(avance(Some(&en_file), &en_file), "toujours 1 devant : on attend, sans compter le figé");
        let en_cours: EtatExport =
            serde_json::from_str(r#"{"etat":"en_cours","fichier":"export.mp4","pour_cent":12}"#).unwrap();
        assert!(avance(Some(&en_file), &en_cours), "pris en charge : ça bouge");
        assert!(!avance(Some(&en_cours), &en_cours), "même pourcentage : figé");
        let plus_loin: EtatExport =
            serde_json::from_str(r#"{"etat":"en_cours","fichier":"export.mp4","pour_cent":13}"#).unwrap();
        assert!(avance(Some(&en_cours), &plus_loin));
    }

    /// Ce que `meta.json` dit d'un clip déjà sur le serveur, et le fichier
    /// qu'une recette produit quand on n'a pas pu le demander au serveur.
    #[test]
    fn la_fiche_du_clip_se_juge_et_la_recette_nomme_sa_sortie() {
        assert_eq!(juger_meta(&serde_json::json!({"etat":"pret"})), Presence::Pret);
        assert_eq!(juger_meta(&serde_json::json!({"etat":"en_preparation"})), Presence::EnPreparation);
        assert_eq!(juger_meta(&serde_json::json!({})), Presence::EnPreparation);
        assert_eq!(
            juger_meta(&serde_json::json!({"etat":"erreur","message":"vidéo illisible : ffmpeg : délai dépassé"})),
            Presence::Erreur("vidéo illisible : ffmpeg : délai dépassé".into())
        );
        assert_eq!(
            juger_meta(&serde_json::json!({"etat":"erreur","message":"  "})),
            Presence::Erreur("clip illisible par le serveur".into())
        );
        let mut p = projet_d_essai();
        assert_eq!(nom_sortie(&Atelier::recette(&p)), "telephone.mp4");
        p.format = Format::Original;
        assert_eq!(nom_sortie(&Atelier::recette(&p)), "export.mp4");
        assert_eq!(nom_sortie(&serde_json::json!({})), "export.mp4");
    }

    /// Le fil a fini : la fiche du projet suit ce qu'il a appris — depuis
    /// le disque quand il y a écrit, sinon en mémoire.
    #[test]
    fn la_fiche_du_projet_suit_le_fil() {
        let mut p = projet_d_essai();
        p.reseau = Some(Reseau {
            base: "https://ts:8080".into(),
            jeton: Arc::new(Mutex::new("ab".into())),
            agent: ureq::Agent::new(),
        });
        // Un chemin sans fiche sur le disque : ce que le fil a noté.
        let suivi = Suivi { id: Some("0123456789abcdef".into()), ..Default::default() };
        relire_fiche(&mut p, &suivi);
        let f = p.fiche.clone().unwrap();
        assert_eq!(f.serveur.as_deref(), Some("0123456789abcdef"));
        assert_eq!(f.serveur_base.as_deref(), Some("https://ts:8080"));
        assert!(f.pistes.is_some(), "le reste de la fiche est gardé");
        // Perdu puis redéposé : le nouvel identifiant.
        let suivi = Suivi { id: Some("fedcba9876543210".into()), serveur_perdu: true, ..Default::default() };
        relire_fiche(&mut p, &suivi);
        assert_eq!(p.fiche.as_ref().unwrap().serveur.as_deref(), Some("fedcba9876543210"));
        // Perdu sans redépôt (le dépôt a échoué) : plus de serveur.
        let suivi = Suivi { serveur_perdu: true, ..Default::default() };
        relire_fiche(&mut p, &suivi);
        assert!(p.fiche.as_ref().unwrap().serveur.is_none());
    }

    #[test]
    fn le_delai_suit_la_duree_de_la_coupe() {
        assert_eq!(delai_pour(0), Duration::from_secs(600));
        assert_eq!(delai_pour(30_000), Duration::from_secs(1050));
        assert_eq!(delai_pour(180_000), Duration::from_secs(3300));
        assert_eq!(delai_pour(3_600_000), Duration::from_secs(3600));
    }

    #[test]
    fn la_barre_dit_la_phase_et_le_temps_qui_passe() {
        let mut s = Suivi::default();
        s.phase("en file sur le serveur, 2 devant", false);
        assert_eq!(s.texte(), "en file sur le serveur, 2 devant…");
        s.phase("export", true);
        s.pour_cent = 42;
        assert_eq!(s.texte(), "export… 42 %");
        s.debut = Some(Instant::now() - Duration::from_secs(75));
        assert_eq!(s.texte(), "export… 42 %  (1:15)");
        // Un changement de phase remet le pourcentage à zéro : la barre
        // pleine du dépôt ne reste pas affichée pendant la préparation.
        s.phase("préparation sur le serveur", false);
        assert_eq!(s.pour_cent, 0);
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
