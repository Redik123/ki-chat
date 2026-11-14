//! Serveur de test pour Claude (MCP) : lire l'état de l'appli, la
//! photographier, y cliquer et y taper, sans personne devant l'écran.
//!
//! # Pourquoi
//!
//! Un correctif d'interface ne se prouvait que par des tests unitaires : rien
//! ne permettait à Claude de cliquer dans ki-chat ni de voir ce qu'il
//! affiche (les captures de fenêtre prises du dehors sortent blanches). Ce
//! module lui donne des yeux et des mains, par un relais (`ki-chat-relais`)
//! que Claude Code lance et qui parle à l'appli ouverte.
//!
//! # Garde-fous
//!
//! Un serveur de ce genre peut lire les messages et en envoyer. Il est donc :
//!
//! - **absent des versions publiées** : compilé en debug seulement ;
//! - **éteint par défaut** : il faut lancer `ki-chat --mcp` ;
//! - **sur un profil à part** ([`profil`]) : le ki-chat de test ne connaît ni
//!   les serveurs ni les mots de passe mémorisés de l'utilisateur ;
//! - **local** : un tube nommé de Windows, pas un port. Un navigateur ne peut
//!   pas l'ouvrir, une autre machine non plus (`reject_remote_clients`), et
//!   un autre compte Windows n'a pas le droit d'y écrire.
//!
//! # Comment
//!
//! Le relais envoie une demande par ligne (`{"id", "outil", "arguments"}`),
//! reçoit une réponse par ligne. Un fil tient le tube ; les demandes sont
//! exécutées sur le fil de l'interface, entre deux images, là où l'état de
//! l'appli se lit et se modifie sans verrou. Clics et touches passent par
//! `raw_input_hook` : egui les reçoit comme ceux d'une vraie souris et d'un
//! vrai clavier, et tout ce qui se clique se teste — pas seulement ce pour
//! quoi un outil a été prévu.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use eframe::egui;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use super::KiApp;

/// L'argument qui l'allume.
pub const ARG: &str = "--mcp";
/// Le tube que tient l'appli, et qu'ouvre le relais.
pub const TUBE: &str = r"\\.\pipe\ki-chat-mcp";

/// Une demande reste sans réponse au plus ce temps : au-delà, l'interface ne
/// tourne pas (fenêtre minimisée, appli figée) et il vaut mieux le dire.
const DELAI: Duration = Duration::from_secs(20);
/// Après un clic ou une touche, les images laissées à l'interface pour
/// réagir avant de traiter la demande suivante.
const REPOS: u32 = 3;
/// La plus grande ligne de journal ou de message rendue telle quelle.
const LARGEUR_TEXTE: usize = 300;

pub fn demande() -> bool {
    std::env::args().any(|a| a == ARG)
}

/// Où le ki-chat de test range ses réglages : à part de ceux de
/// l'utilisateur, et gardés d'un essai à l'autre (le serveur d'essai y reste
/// enregistré).
pub fn profil() -> std::path::PathBuf {
    let dossier = std::env::temp_dir().join("ki-chat-mcp");
    let _ = std::fs::create_dir_all(&dossier);
    dossier.join("app.ron")
}

struct Demande {
    outil: String,
    arguments: Value,
    reponse: tokio::sync::oneshot::Sender<Reponse>,
}

struct Reponse {
    ok: bool,
    texte: String,
    /// Un PNG en base64, pour `capture`.
    image: Option<String>,
}

impl Reponse {
    fn ok(texte: impl Into<String>) -> Self {
        Self { ok: true, texte: texte.into(), image: None }
    }
    fn erreur(texte: impl Into<String>) -> Self {
        Self { ok: false, texte: texte.into(), image: None }
    }
    fn json(&self, id: &Value) -> String {
        json!({ "id": id, "ok": self.ok, "texte": self.texte, "image": self.image }).to_string()
    }
}

/// Le côté interface du serveur.
pub struct Serveur {
    demandes: mpsc::Receiver<Demande>,
    /// Les événements à donner à egui, un lot par image : un clic, c'est
    /// l'appui dans une image et le relâchement dans la suivante.
    a_injecter: VecDeque<Vec<egui::Event>>,
    /// Les images encore laissées à l'interface avant la demande suivante.
    repos: u32,
    /// Depuis quand des événements attendent d'être donnés.
    injection_depuis: Option<Instant>,
    /// La capture demandée à egui, déposée ici par son rappel.
    image: Arc<Mutex<Option<Arc<egui::ColorImage>>>>,
    capture_en_attente: Option<(tokio::sync::oneshot::Sender<Reponse>, Instant)>,
    /// La fenêtre que la demande en cours vise : la principale, ou la page
    /// VALORANT détachée. Les événements n'entrent que dans celle-là — et
    /// chaque fenêtre a ses images, donc son entrée.
    cible: egui::ViewportId,
}

impl Serveur {
    /// Ouvre le tube et lance le fil qui le tient.
    pub fn demarrer(ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        let lance = std::thread::Builder::new().name("mcp".into()).spawn(move || ecouter(tx, ctx));
        match lance {
            Ok(_) => tracing::warn!("MCP de test actif sur {TUBE} — à ne jamais lancer chez un joueur"),
            Err(e) => tracing::error!("MCP : le fil n'a pas démarré : {e}"),
        }
        Self {
            demandes: rx,
            a_injecter: VecDeque::new(),
            repos: 0,
            injection_depuis: None,
            image: Arc::new(Mutex::new(None)),
            capture_en_attente: None,
            cible: egui::ViewportId::ROOT,
        }
    }

    /// Le lot d'événements de cette image, glissé dans l'entrée d'egui avant
    /// qu'il ne la lise.
    pub fn injecter(&mut self, ctx: &egui::Context, entree: &mut egui::RawInput) {
        // La fenêtre de test s'ouvre le plus souvent derrière celle de
        // Claude, sans le focus de Windows. Or egui ne tient un champ pour
        // « actif » que dans une fenêtre qui a le focus : Entrée n'envoyait
        // pas, la flèche haut ne reprenait pas le dernier message. Pour
        // egui, la fenêtre de test a toujours le focus.
        entree.focused = true;
        // eframe passe ici pour chaque fenêtre, avec l'entrée de celle-ci :
        // un clic destiné à la page détachée ne tombe pas dans la
        // principale, ni l'inverse.
        if entree.viewport_id != self.cible {
            return;
        }
        match self.a_injecter.pop_front() {
            Some(lot) => {
                entree.events.extend(lot);
                ctx.request_repaint_of(self.cible);
            }
            None if self.repos > 0 => {
                self.repos -= 1;
                ctx.request_repaint_of(self.cible);
            }
            None => {}
        }
        if self.a_injecter.is_empty() {
            self.injection_depuis = None;
        }
    }

    fn poser(&mut self, lots: Vec<Vec<egui::Event>>) {
        self.a_injecter.extend(lots);
        self.repos = REPOS;
        self.injection_depuis.get_or_insert_with(Instant::now);
    }
}

/// Le fil du tube : un client à la fois, une demande par ligne.
fn ecouter(tx: mpsc::Sender<Demande>, ctx: egui::Context) {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("MCP : pas de boucle d'événements : {e}");
            return;
        }
    };
    rt.block_on(async move {
        // `first_pipe_instance` : si un autre programme tient déjà ce nom,
        // on échoue plutôt que de lui parler sans le savoir.
        let mut premier = true;
        loop {
            let tube = match ServerOptions::new().first_pipe_instance(premier).reject_remote_clients(true).create(TUBE) {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!("MCP : le tube {TUBE} ne s'ouvre pas ({e}) — un autre ki-chat --mcp tourne ?");
                    return;
                }
            };
            premier = false;
            if tube.connect().await.is_err() {
                continue;
            }
            servir(tube, &tx, &ctx).await;
        }
    });
}

async fn servir(tube: NamedPipeServer, tx: &mpsc::Sender<Demande>, ctx: &egui::Context) {
    let (lecture, mut ecriture) = tokio::io::split(tube);
    let mut lignes = tokio::io::BufReader::new(lecture).lines();
    while let Ok(Some(ligne)) = lignes.next_line().await {
        if ligne.trim().is_empty() {
            continue;
        }
        let (id, reponse) = match serde_json::from_str::<Value>(&ligne) {
            Ok(v) => {
                let id = v.get("id").cloned().unwrap_or(Value::Null);
                let outil = v.get("outil").and_then(Value::as_str).unwrap_or_default().to_string();
                let arguments = v.get("arguments").cloned().unwrap_or(Value::Null);
                let (rtx, rrx) = tokio::sync::oneshot::channel();
                if tx.send(Demande { outil, arguments, reponse: rtx }).is_err() {
                    return;
                }
                ctx.request_repaint();
                let r = match tokio::time::timeout(DELAI + Duration::from_secs(5), rrx).await {
                    Ok(Ok(r)) => r,
                    _ => Reponse::erreur("ki-chat n'a pas répondu : sa fenêtre est peut-être minimisée, ou l'appli figée"),
                };
                (id, r)
            }
            Err(e) => (Value::Null, Reponse::erreur(format!("demande illisible : {e}"))),
        };
        let mut sortie = reponse.json(&id);
        sortie.push('\n');
        if ecriture.write_all(sortie.as_bytes()).await.is_err() || ecriture.flush().await.is_err() {
            return;
        }
    }
}

fn court(s: &str) -> String {
    let une_ligne = s.replace(['\n', '\r'], " ");
    if une_ligne.chars().count() <= LARGEUR_TEXTE {
        une_ligne
    } else {
        let debut: String = une_ligne.chars().take(LARGEUR_TEXTE).collect();
        format!("{debut}…")
    }
}

fn nombre(arguments: &Value, cle: &str) -> Option<f32> {
    arguments.get(cle).and_then(Value::as_f64).map(|v| v as f32)
}

fn modificateurs(arguments: &Value) -> egui::Modifiers {
    let vrai = |cle: &str| arguments.get(cle).and_then(Value::as_bool).unwrap_or(false);
    let ctrl = vrai("ctrl");
    egui::Modifiers { alt: vrai("alt"), ctrl, shift: vrai("maj"), mac_cmd: false, command: ctrl }
}

/// Les lots d'un clic : la souris arrive, appuie, puis relâche à l'image
/// suivante — comme une main.
fn lots_de_clic(pos: egui::Pos2, bouton: egui::PointerButton, double: bool, m: egui::Modifiers) -> Vec<Vec<egui::Event>> {
    let appui = |pressed| egui::Event::PointerButton { pos, button: bouton, pressed, modifiers: m };
    let mut lots = vec![vec![egui::Event::PointerMoved(pos)], vec![appui(true)], vec![appui(false)]];
    if double {
        lots.push(vec![appui(true)]);
        lots.push(vec![appui(false)]);
    }
    lots
}

/// Les lots d'une touche : enfoncée, puis relâchée à l'image suivante.
fn lots_de_touche(touche: egui::Key, m: egui::Modifiers) -> Vec<Vec<egui::Event>> {
    let e = |pressed| egui::Event::Key { key: touche, physical_key: None, pressed, repeat: false, modifiers: m };
    vec![vec![e(true)], vec![e(false)]]
}

/// L'image d'egui en PNG, ramenée à un pixel par point : les coordonnées
/// qu'on y lit sont celles qu'attend `cliquer`.
fn en_png(image: &egui::ColorImage, points: egui::Vec2) -> Result<Vec<u8>, String> {
    let [l, h] = image.size;
    let octets: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
    let brut = image::RgbaImage::from_raw(l as u32, h as u32, octets).ok_or("image vide")?;
    let (lp, hp) = (points.x.round().max(1.0) as u32, points.y.round().max(1.0) as u32);
    let finale = if (lp, hp) == (l as u32, h as u32) {
        brut
    } else {
        image::imageops::resize(&brut, lp, hp, image::imageops::FilterType::Triangle)
    };
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(finale)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(png)
}

/// Les dernières lignes d'un fichier, sans le charger en entier.
fn fin_du_fichier(chemin: &std::path::Path, lignes: usize) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(chemin)?;
    let taille = f.metadata()?.len();
    let depuis = taille.saturating_sub(256 * 1024);
    f.seek(SeekFrom::Start(depuis))?;
    let mut octets = Vec::new();
    f.read_to_end(&mut octets)?;
    let texte = String::from_utf8_lossy(&octets);
    let toutes: Vec<&str> = texte.lines().collect();
    let debut = toutes.len().saturating_sub(lignes);
    Ok(toutes[debut..].join("\n"))
}

impl KiApp {
    /// À chaque image, côté logique : les demandes arrivées sont exécutées,
    /// la capture attendue est rendue.
    pub(crate) fn mcp_tick(&mut self, ctx: &egui::Context) {
        let Some(mut serveur) = self.mcp.take() else { return };
        self.mcp_servir(&mut serveur, ctx);
        self.mcp = Some(serveur);
    }

    fn mcp_servir(&mut self, s: &mut Serveur, ctx: &egui::Context) {
        // Une capture en route : la rendre dès qu'egui l'a déposée.
        if let Some((_, depuis)) = &s.capture_en_attente {
            let image = s.image.lock().ok().and_then(|mut i| i.take());
            if let Some(image) = image {
                let (reponse, _) = s.capture_en_attente.take().expect("vérifiée juste au-dessus");
                // La fenêtre principale dit sa taille ; une autre se déduit
                // de son image, à l'échelle de l'interface.
                let points = if s.cible == egui::ViewportId::ROOT {
                    ctx.content_rect().size()
                } else {
                    egui::vec2(image.size[0] as f32, image.size[1] as f32) / ctx.pixels_per_point()
                };
                let r = match en_png(&image, points) {
                    Ok(png) => Reponse {
                        ok: true,
                        texte: format!(
                            "Capture de {}×{} : un pixel par point, les coordonnées lues ici sont celles de `cliquer`.",
                            points.x.round(),
                            points.y.round()
                        ),
                        image: Some(base64::engine::general_purpose::STANDARD.encode(png)),
                    },
                    Err(e) => Reponse::erreur(format!("capture illisible : {e}")),
                };
                let _ = reponse.send(r);
            } else if depuis.elapsed() > DELAI {
                let (reponse, _) = s.capture_en_attente.take().expect("vérifiée juste au-dessus");
                let _ = reponse.send(Reponse::erreur(
                    "pas de capture : la fenêtre ne se dessine pas (minimisée ou réduite ?)",
                ));
            } else {
                ctx.request_repaint();
                ctx.request_repaint_of(s.cible);
            }
            return;
        }
        // Des clics ou des touches attendent encore leur image : la demande
        // suivante lirait un état d'avant. Sauf si rien ne se dessine.
        if !s.a_injecter.is_empty() || s.repos > 0 {
            if s.injection_depuis.is_some_and(|t| t.elapsed() > DELAI) {
                s.a_injecter.clear();
                s.repos = 0;
                s.injection_depuis = None;
                tracing::warn!("MCP : événements abandonnés, l'interface ne tourne pas");
            } else {
                ctx.request_repaint();
                ctx.request_repaint_of(s.cible);
                return;
            }
        }
        // Une demande par image : chacune voit l'effet de la précédente.
        let Ok(d) = s.demandes.try_recv() else { return };
        ctx.request_repaint();
        // La fenêtre visée : la principale, sauf demande contraire.
        s.cible = match d.arguments.get("fenetre").and_then(Value::as_str) {
            None | Some("principale") => egui::ViewportId::ROOT,
            Some("valorant") if self.valo_fenetre.is_some() => crate::valo_detache::id(),
            Some("valorant") => {
                let _ = d.reponse.send(Reponse::erreur("la page VALORANT n'est pas détachée : pas de seconde fenêtre"));
                return;
            }
            Some(autre) => {
                let _ = d.reponse.send(Reponse::erreur(format!("fenêtre inconnue « {autre} » : principale ou valorant")));
                return;
            }
        };
        let r = match d.outil.as_str() {
            "capture" => {
                let depot = s.image.clone();
                let reveil = ctx.clone();
                if let Ok(mut i) = depot.lock() {
                    *i = None;
                }
                let rappel = egui::ScreenshotCallback::new(move |image| {
                    if let Ok(mut i) = depot.lock() {
                        *i = Some(image);
                    }
                    reveil.request_repaint();
                });
                ctx.send_viewport_cmd_to(s.cible, egui::ViewportCommand::Screenshot(rappel));
                s.capture_en_attente = Some((d.reponse, Instant::now()));
                return;
            }
            "etat" => Reponse::ok(self.mcp_etat(ctx).to_string()),
            "journal" => {
                let lignes = d.arguments.get("lignes").and_then(Value::as_u64).unwrap_or(60).clamp(1, 500) as usize;
                match crate::secours::chemin_journal() {
                    Some(chemin) => match fin_du_fichier(&chemin, lignes) {
                        Ok(t) => Reponse::ok(format!("{}\n{t}", chemin.display())),
                        Err(e) => Reponse::erreur(format!("{} : {e}", chemin.display())),
                    },
                    None => Reponse::erreur("pas de journal sur disque"),
                }
            }
            "cliquer" => match (nombre(&d.arguments, "x"), nombre(&d.arguments, "y")) {
                (Some(x), Some(y)) => {
                    let bouton = match d.arguments.get("bouton").and_then(Value::as_str) {
                        Some("droit") => egui::PointerButton::Secondary,
                        Some("milieu") => egui::PointerButton::Middle,
                        _ => egui::PointerButton::Primary,
                    };
                    let double = d.arguments.get("double").and_then(Value::as_bool).unwrap_or(false);
                    s.poser(lots_de_clic(egui::pos2(x, y), bouton, double, modificateurs(&d.arguments)));
                    Reponse::ok(format!("clic en ({x}, {y})"))
                }
                _ => Reponse::erreur("il faut x et y, en points (ceux de la capture)"),
            },
            "survoler" => match (nombre(&d.arguments, "x"), nombre(&d.arguments, "y")) {
                (Some(x), Some(y)) => {
                    s.poser(vec![vec![egui::Event::PointerMoved(egui::pos2(x, y))]]);
                    Reponse::ok(format!("souris en ({x}, {y})"))
                }
                _ => Reponse::erreur("il faut x et y, en points (ceux de la capture)"),
            },
            "defiler" => match (nombre(&d.arguments, "x"), nombre(&d.arguments, "y")) {
                (Some(x), Some(y)) => {
                    let dy = nombre(&d.arguments, "dy").unwrap_or(-120.0);
                    s.poser(vec![
                        vec![egui::Event::PointerMoved(egui::pos2(x, y))],
                        vec![egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point,
                            delta: egui::vec2(0.0, dy),
                            phase: egui::TouchPhase::Move,
                            source: egui::MouseWheelSource::Wheel,
                            modifiers: modificateurs(&d.arguments),
                        }],
                    ]);
                    Reponse::ok(format!("défilement de {dy} points en ({x}, {y})"))
                }
                _ => Reponse::erreur("il faut x et y, en points (ceux de la capture)"),
            },
            "taper" => match d.arguments.get("texte").and_then(Value::as_str) {
                Some(texte) if !texte.is_empty() => {
                    s.poser(vec![vec![egui::Event::Text(texte.to_string())]]);
                    Reponse::ok(format!("{} caractères donnés au champ qui a le focus", texte.chars().count()))
                }
                _ => Reponse::erreur("il faut un texte"),
            },
            "touche" => {
                let nom = d.arguments.get("touche").and_then(Value::as_str).unwrap_or_default();
                match egui::Key::from_name(nom) {
                    Some(touche) => {
                        s.poser(lots_de_touche(touche, modificateurs(&d.arguments)));
                        Reponse::ok(format!("touche {nom}"))
                    }
                    None => Reponse::erreur(format!(
                        "touche inconnue « {nom} » : Enter, Escape, Tab, Backspace, ArrowUp, ArrowDown, A…Z, F1…"
                    )),
                }
            }
            "aller_salon" => {
                let voulu = d.arguments.get("salon").map(|v| match v {
                    Value::String(nom) => self.session.channels.iter().find(|c| c.name.eq_ignore_ascii_case(nom)).map(|c| c.id),
                    autre => autre.as_u64().map(|n| n as u32).filter(|id| self.session.channels.iter().any(|c| c.id == *id)),
                });
                match voulu {
                    Some(Some(id)) if self.session.welcomed => {
                        self.join(id);
                        Reponse::ok(format!("salon {id} ouvert"))
                    }
                    Some(Some(_)) => Reponse::erreur("pas connecté à un serveur"),
                    _ => Reponse::erreur("salon introuvable : donne son nom ou son numéro (voir `etat`)"),
                }
            }
            "redimensionner" => match (nombre(&d.arguments, "largeur"), nombre(&d.arguments, "hauteur")) {
                (Some(l), Some(h)) if (200.0..=6000.0).contains(&l) && (200.0..=4000.0).contains(&h) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(l, h)));
                    // La nouvelle taille n'arrive qu'aux images suivantes :
                    // la demande d'après les attend, comme après un clic.
                    s.repos = REPOS;
                    Reponse::ok(format!("fenêtre demandée à {l}×{h} points (le minimum de l'appli s'applique)"))
                }
                _ => Reponse::erreur("il faut largeur et hauteur, en points"),
            },
            // La fenêtre principale réduite dans la barre des tâches, ou
            // rendue : réduite, eframe n'y dessine plus rien et seule la
            // logique tourne — ce que vit ki-chat derrière un jeu. De quoi
            // voir ce qui doit continuer sans elle.
            "reduire" => {
                let oui = d.arguments.get("oui").and_then(Value::as_bool).unwrap_or(true);
                ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Minimized(oui));
                Reponse::ok(if oui { "fenêtre principale réduite" } else { "fenêtre principale rendue" })
            }
            // Les deux boutons de « Mes réglages », sans leur fenêtre de choix
            // de fichier (elle est à Windows, on ne la pilote pas d'ici).
            "exporter_reglages" => match d.arguments.get("chemin").and_then(Value::as_str) {
                Some(chemin) if !chemin.is_empty() => {
                    self.reglages_demande = Some(crate::DemandeReglages::Exporter(chemin.into()));
                    s.repos = REPOS;
                    Reponse::ok(format!("export demandé vers {chemin} : `etat` dira s'il a réussi (bandeau_info ou bandeau_erreur)"))
                }
                _ => Reponse::erreur("il faut un chemin"),
            },
            "importer_reglages" => match d.arguments.get("chemin").and_then(Value::as_str) {
                Some(chemin) => match std::fs::read_to_string(chemin).map_err(|e| e.to_string()).and_then(|t| crate::prefs::lire_export(&t)) {
                    Ok(table) => {
                        let combien = table.len();
                        self.reglages_demande = Some(crate::DemandeReglages::Importer(table));
                        Reponse::ok(format!("{combien} réglages posés : ki-chat de test redémarre, attendre quelques secondes"))
                    }
                    Err(e) => Reponse::erreur(e),
                },
                None => Reponse::erreur("il faut un chemin"),
            },
            "fermer_ki_chat" => {
                self.quitter = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                Reponse::ok("ki-chat de test se ferme")
            }
            autre => Reponse::erreur(format!("outil inconnu : {autre}")),
        };
        let _ = d.reponse.send(r);
    }

    /// Ce que l'appli sait d'elle-même, pour qui ne la voit pas.
    fn mcp_etat(&self, ctx: &egui::Context) -> Value {
        let fenetre = ctx.content_rect().size();
        let salons: Vec<Value> = self.session
            .channels
            .iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "nom": c.name,
                    "genre": format!("{:?}", c.kind),
                    "non_lus": self.session.non_lus.get(&c.id).map_or(0, |n| n.nb),
                })
            })
            .collect();
        let derniers: Vec<Value> = self.session
            .messages
            .iter()
            .rev()
            .take(8)
            .rev()
            .map(|m| json!({ "de": m.username, "ts": m.ts, "texte": court(&m.text), "modifie": m.edited }))
            .collect();
        let ouvertes: Vec<&str> = [
            (self.show_settings, "réglages"),
            (self.admin.ouverte, "administration"),
            (self.show_account, "mon compte"),
            (self.recherche.ouverte, "recherche"),
            (self.aller_a.ouvert, "aller-a"),
            (self.raccourcis.ouverte, "raccourcis"),
            (self.clips.ouverte, "clips"),
            (self.diffusion.ouverte, "diffusion"),
            (self.menu_message.is_some(), "menu d'un message"),
        ]
        .into_iter()
        .filter_map(|(ouverte, nom)| ouverte.then_some(nom))
        .collect();
        json!({
            "version": crate::update::current(),
            "connecte": self.session.welcomed,
            "connexion_en_cours": self.session.connecting,
            "serveur": self.url,
            "pseudo": self.username,
            "salon_courant": self.session.current,
            "salons": salons,
            "messages_charges": self.session.messages.len(),
            "derniers_messages": derniers,
            "saisie": {
                "texte": self.input,
                "modifie_le_message": self.edition.map(|m| json!({ "auteur": m.user_id, "ts": m.ts })),
                "repond_a": self.reponse_a.as_ref().map(|r| json!({ "de": r.username, "extrait": r.excerpt })),
            },
            "vocal": { "salon": self.session.voice_channel, "micro_coupe": self.vocal.muted },
            "reduite": ctx.input(|i| i.viewport().minimized == Some(true)),
            "valorant": {
                "ouverte": self.valo.ouvert,
                "onglet": self.valo.onglet_devant(),
                "detachee": self.valo.detachee,
                "fenetre": self.valo_fenetre.is_some(),
                "fiche": self.donnees_valo.fiche.as_ref().map(|f| f.username.clone()),
            },
            "fenetres_ouvertes": ouvertes,
            "bandeau_erreur": self.error,
            "bandeau_info": self.info,
            "toasts": self.toasts.textes(),
            "fenetre": {
                "largeur": fenetre.x.round(),
                "hauteur": fenetre.y.round(),
                "pixels_par_point": ctx.pixels_per_point(),
                "a_le_focus": self.window_focused,
                "hors_de_vue": self.zone.hors_de_vue(),
                "plein_rythme": self.plein_rythme(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_clic_arrive_appuie_puis_relache_une_image_apres() {
        let lots = lots_de_clic(egui::pos2(10.0, 20.0), egui::PointerButton::Primary, false, egui::Modifiers::NONE);
        assert_eq!(lots.len(), 3);
        assert!(matches!(lots[0][0], egui::Event::PointerMoved(p) if p == egui::pos2(10.0, 20.0)));
        assert!(matches!(lots[1][0], egui::Event::PointerButton { pressed: true, .. }));
        assert!(matches!(lots[2][0], egui::Event::PointerButton { pressed: false, .. }));
        // Un double clic : deux appuis, deux relâchements.
        assert_eq!(lots_de_clic(egui::pos2(0.0, 0.0), egui::PointerButton::Primary, true, egui::Modifiers::NONE).len(), 5);
    }

    #[test]
    fn une_touche_porte_ses_modificateurs() {
        let m = modificateurs(&json!({ "ctrl": true, "maj": true }));
        assert!(m.ctrl && m.command && m.shift && !m.alt);
        let lots = lots_de_touche(egui::Key::K, m);
        assert!(matches!(&lots[0][0], egui::Event::Key { key: egui::Key::K, pressed: true, modifiers, .. } if modifiers.ctrl));
        assert!(matches!(&lots[1][0], egui::Event::Key { pressed: false, .. }));
    }

    #[test]
    fn la_capture_sort_a_un_pixel_par_point() {
        // 300×200 pixels pour une fenêtre de 150×100 points (écran à 200 %).
        let image = egui::ColorImage::filled([300, 200], egui::Color32::from_rgb(10, 20, 30));
        let png = en_png(&image, egui::vec2(150.0, 100.0)).unwrap();
        let relue = image::load_from_memory(&png).unwrap();
        assert_eq!((relue.width(), relue.height()), (150, 100));
        assert_eq!(relue.to_rgba8().get_pixel(75, 50).0, [10, 20, 30, 255]);
    }

    #[test]
    fn un_texte_long_est_coupe_sur_une_ligne() {
        assert_eq!(court("a\nb"), "a b");
        let long = "é".repeat(LARGEUR_TEXTE + 50);
        assert_eq!(court(&long).chars().count(), LARGEUR_TEXTE + 1);
    }

    #[test]
    fn le_journal_se_lit_par_la_fin() {
        let chemin = std::env::temp_dir().join(format!("ki-mcp-journal-{}.log", std::process::id()));
        let contenu: String = (1..=100).map(|i| format!("ligne {i}\n")).collect();
        std::fs::write(&chemin, contenu).unwrap();
        assert_eq!(fin_du_fichier(&chemin, 3).unwrap(), "ligne 98\nligne 99\nligne 100");
        std::fs::remove_file(&chemin).unwrap();
    }
}
