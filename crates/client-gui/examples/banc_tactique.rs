//! Le banc de la carte tactique : la fenêtre « Carte tactique » avec des
//! traits inventés, sans serveur ni ki-chat autour.
//!
//! `cargo run -p ki-client-gui --example banc_tactique`
//!
//! `BANC_CARTES=maps.json` : la réponse de `https://valorant-api.com/v1/maps`,
//! d'où viennent les plans, comme dans ki-chat ; sans elle, un plan
//! d'Ascent réduit à quelques zones. `BANC_MINIMAPS=dossier` : les
//! minimaps, une par plan (`Ascent.png`…) ; sans elles, le quadrillage
//! d'attente.
//!
//! `BANC_CAPTURE=dossier` : le banc photographie chaque scène
//! (`tactique-1.png`…) puis se ferme. Sans capture, Espace passe à la
//! scène suivante. `BANC_PETIT=1` : un écran de 700×600, où la fenêtre
//! tombe à sa taille minimale (`petit-tactique-1.png`…).
//!
//! Les dernières scènes passent par de faux gestes — la molette, un trait
//! tiré à la souris — pour éprouver le zoom et le tracé comme une main.

#[allow(dead_code)]
#[path = "../src/tactique.rs"]
mod tactique;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use eframe::egui::{self, pos2, vec2, Pos2};
use ki_protocol::UserId;
use ki_ui::jetons::couleur::pour_pseudo;
use ki_protocol::{CarteTactique, EvenementTactique, OutilTactique};
use tactique::{Camp, Lien, Outil, Plan, Tactique, Trait};

const MOI: UserId = 1;
const NOMS: [&str; 4] = ["", "drion", "Kevin", "Lina"];

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(if petit() { [700.0, 600.0] } else { [1000.0, 900.0] })
            .with_title("Banc de la carte tactique"),
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native(
        "banc-tactique",
        options,
        Box::new(|cc| {
            ki_ui::style::installer(&cc.egui_ctx);
            let plans = lire_plans();
            let minimaps = lire_minimaps(&cc.egui_ctx, &plans);
            let mut banc = Banc {
                scene: 0,
                images: 0,
                demandee: false,
                capture: std::env::var_os("BANC_CAPTURE").map(std::path::PathBuf::from),
                tactique: Tactique::default(),
                plans,
                minimaps,
                sans_minimap: false,
            };
            banc.poser();
            Ok(Box::new(banc))
        }),
    )
}

fn petit() -> bool {
    std::env::var_os("BANC_PETIT").is_some()
}

/// Les plans de `BANC_CARTES`, lus comme le catalogue les lit ; sinon un
/// Ascent de secours.
fn lire_plans() -> Vec<Plan> {
    let lus: Vec<Plan> = std::env::var_os("BANC_CARTES")
        .and_then(|chemin| std::fs::read(chemin).ok())
        .and_then(|octets| serde_json::from_slice::<serde_json::Value>(&octets).ok())
        .map(|v| v["data"].as_array().into_iter().flatten().filter_map(tactique::plan_depuis).collect())
        .unwrap_or_default();
    if !lus.is_empty() {
        let mut lus = lus;
        lus.sort_by(|a, b| a.nom.cmp(&b.nom));
        for p in &lus {
            println!("{} : {} zones, {} quart(s) de tour", p.nom, p.zones.len(), p.quarts);
        }
        return lus;
    }
    let zones = [
        ("A Site", 0.35, 0.14),
        ("A Main", 0.48, 0.20),
        ("A Lobby", 0.60, 0.26),
        ("Mid Courtyard", 0.49, 0.49),
        ("B Main", 0.40, 0.71),
        ("B Site", 0.29, 0.74),
        ("Attacker Spawn", 0.82, 0.57),
        ("Defender Spawn", 0.13, 0.43),
    ];
    vec![Plan {
        nom: "Ascent".into(),
        minimap: String::new(),
        quarts: 1,
        zones: zones.iter().map(|(n, u, v)| (n.to_string(), *u, *v)).collect(),
    }]
}

/// Les minimaps de `BANC_MINIMAPS`, par nom de plan.
fn lire_minimaps(ctx: &egui::Context, plans: &[Plan]) -> HashMap<String, egui::TextureHandle> {
    let Some(dossier) = std::env::var_os("BANC_MINIMAPS").map(std::path::PathBuf::from) else {
        return HashMap::new();
    };
    plans
        .iter()
        .filter_map(|p| {
            let octets = std::fs::read(dossier.join(format!("{}.png", p.nom))).ok()?;
            let image = image::load_from_memory(&octets).ok()?.to_rgba8();
            let taille = [image.width() as usize, image.height() as usize];
            let couleurs = egui::ColorImage::from_rgba_unmultiplied(taille, image.as_raw());
            let texture = ctx.load_texture(format!("minimap:{}", p.nom), couleurs, egui::TextureOptions::LINEAR);
            Some((p.nom.clone(), texture))
        })
        .collect()
}

struct Banc {
    scene: usize,
    images: u32,
    demandee: bool,
    capture: Option<std::path::PathBuf>,
    tactique: Tactique,
    plans: Vec<Plan>,
    minimaps: HashMap<String, egui::TextureHandle>,
    /// La scène du quadrillage : la minimap n'est pas encore arrivée.
    sans_minimap: bool,
}

/// Le nombre de scènes.
const SCENES: usize = 8;

impl Banc {
    /// Un point d'une zone nommée du plan choisi, décalé ; le milieu de la
    /// carte si elle n'y est pas.
    fn ou(&self, zone: &str, decalage: (f32, f32)) -> Pos2 {
        let plan = self.plans.iter().find(|p| p.nom == self.tactique.carte);
        let (u, v) = plan
            .and_then(|p| p.zones.iter().find(|(n, ..)| n == zone))
            .map_or((0.5, 0.5), |(_, u, v)| (*u, *v));
        pos2(u + decalage.0, v + decalage.1)
    }

    /// Un chemin qui passe par ces zones, un point tous les 1 % de carte.
    fn chemin(&self, etapes: &[(&str, (f32, f32))]) -> Vec<Pos2> {
        let reperes: Vec<Pos2> = etapes.iter().map(|(z, d)| self.ou(z, *d)).collect();
        let mut points = Vec::new();
        for paire in reperes.windows(2) {
            let n = ((paire[1] - paire[0]).length() / 0.01).ceil().max(1.0) as usize;
            for k in 0..n {
                let t = k as f32 / n as f32;
                // Un léger tremblé : une main, pas une règle.
                let tremble = vec2((t * 9.0).sin(), (t * 7.0).cos()) * 0.002;
                points.push(paire[0] + (paire[1] - paire[0]) * t + tremble);
            }
        }
        points.extend(reperes.last());
        points
    }

    fn trait_de(&self, auteur: UserId, id: u32, outil: Outil, points: Vec<Pos2>, age: Option<Duration>) -> Trait {
        Trait {
            auteur,
            id,
            couleur: pour_pseudo(NOMS[auteur as usize]),
            outil,
            points,
            fini_le: age.map(|a| Instant::now() - a),
            maj: Instant::now(),
        }
    }

    /// Une soirée sur Ascent : drion pousse A, Kevin fume le site, Lina
    /// passe par le milieu, une flèche s'efface, une autre se trace encore.
    fn traits_d_ascent(&mut self) {
        let s = |x| Some(Duration::from_secs_f32(x));
        let traits = [
            self.trait_de(
                1,
                1,
                Outil::Fleche,
                self.chemin(&[("Attacker Spawn", (0.0, 0.0)), ("A Lobby", (0.0, 0.0)), ("A Main", (0.0, 0.0)), ("A Site", (0.02, 0.0))]),
                s(1.0),
            ),
            self.trait_de(2, 1, Outil::Zone, vec![self.ou("A Site", (0.0, 0.0)), self.ou("A Site", (0.045, 0.0))], s(4.0)),
            self.trait_de(
                3,
                1,
                Outil::Crayon,
                self.chemin(&[("Mid Courtyard", (0.03, 0.02)), ("Mid Courtyard", (-0.03, 0.06)), ("B Main", (0.0, -0.02))]),
                s(7.0),
            ),
            self.trait_de(
                2,
                2,
                Outil::Fleche,
                self.chemin(&[("B Main", (0.04, 0.0)), ("B Site", (0.03, -0.02))]),
                s(13.6),
            ),
            self.trait_de(
                3,
                2,
                Outil::Fleche,
                self.chemin(&[("Defender Spawn", (0.03, 0.0)), ("Mid Courtyard", (-0.05, -0.02))]),
                None,
            ),
        ];
        for t in traits {
            self.tactique.ajouter(t);
        }
    }

    fn poser(&mut self) {
        self.tactique = Tactique::default();
        self.tactique.ouvert = true;
        self.sans_minimap = false;
        let premier = |noms: &[&str]| noms.iter().find(|n| self.plans.iter().any(|p| p.nom == **n)).map(|n| n.to_string());
        let carte = match self.scene {
            2 => premier(&["Lotus", "Haven", "Ascent"]),
            _ => premier(&["Ascent"]),
        }
        .or_else(|| self.plans.first().map(|p| p.nom.clone()))
        .unwrap_or_default();
        self.tactique.choisir(&carte);
        match self.scene {
            // Ascent en attaque, puis en défense : les traits restent à
            // leur place sur la carte.
            0 => self.traits_d_ascent(),
            1 => {
                self.traits_d_ascent();
                self.tactique.camp = Camp::Defense;
            }
            // Une carte à trois sites, tournée autrement.
            2 => {
                let s = Some(Duration::from_secs(2));
                let a = self.trait_de(1, 1, Outil::Fleche, self.chemin(&[("Attacker Spawn", (0.0, 0.0)), ("C Site", (0.0, 0.0))]), s);
                let b = self.trait_de(2, 1, Outil::Zone, vec![self.ou("B Site", (0.0, 0.0)), self.ou("B Site", (0.0, 0.05))], s);
                self.tactique.ajouter(a);
                self.tactique.ajouter(b);
            }
            // Sans les noms, puis sans minimap (elle arrive encore).
            3 => {
                self.traits_d_ascent();
                self.tactique.noms = false;
            }
            4 => {
                self.traits_d_ascent();
                self.sans_minimap = true;
            }
            // La molette, puis un trait à la souris : voir `raw_input_hook`.
            5 => self.traits_d_ascent(),
            6 => self.tactique.outil = Outil::Fleche,
            // La carte du salon, telle que le serveur la raconte : son état,
            // puis les traits des autres par le fil — et moi, rendu muet,
            // qui regarde.
            _ => {
                self.tactique.relier(Lien::Salon);
                self.tactique.recevoir_etat(Some(CarteTactique { salon: 1, carte: carte.clone(), par: 2 }));
                self.tactique.muet = true;
                let fil = |points: Vec<Pos2>| -> Vec<[u16; 2]> {
                    points.iter().map(|p| [(p.x.clamp(0.0, 1.0) * 65535.0) as u16, (p.y.clamp(0.0, 1.0) * 65535.0) as u16]).collect()
                };
                let recus = [
                    (2, OutilTactique::Fleche, fil(self.chemin(&[("Attacker Spawn", (0.0, 0.0)), ("B Lobby", (0.0, 0.0)), ("B Main", (0.0, 0.0)), ("B Site", (0.0, 0.0))])), Some(9_000)),
                    (3, OutilTactique::Zone, fil(vec![self.ou("B Site", (0.0, 0.0)), self.ou("B Site", (0.04, 0.0))]), Some(2_000)),
                    (2, OutilTactique::Crayon, fil(self.chemin(&[("Mid Top", (0.0, 0.0)), ("Mid Courtyard", (0.0, 0.0))])), None),
                ];
                for (k, (auteur, outil, points, reste_ms)) in recus.into_iter().enumerate() {
                    // En deux morceaux, chacun à sa place, comme sur le fil.
                    let moitie = points.len() / 2;
                    let (debut, suite) = if outil == OutilTactique::Zone { (points.clone(), points) } else { (points[..moitie].to_vec(), points[moitie..].to_vec()) };
                    let depuis = if outil == OutilTactique::Zone { 0 } else { moitie as u32 };
                    let couleur = |id: UserId| pour_pseudo(NOMS[id as usize]);
                    let id = k as u32 + 1;
                    let morceau = |depuis, points, fin, reste_ms| EvenementTactique::Trait { auteur, id, outil, depuis, points, fin, reste_ms };
                    self.tactique.recevoir(morceau(0, debut, false, None), MOI, &couleur);
                    self.tactique.recevoir(morceau(depuis, suite, reste_ms.is_some(), reste_ms), MOI, &couleur);
                }
            }
        }
        self.images = 0;
        self.demandee = false;
    }
}

impl eframe::App for Banc {
    fn persist_egui_memory(&self) -> bool {
        false
    }

    /// Les faux gestes des dernières scènes, image par image, dans le cadre
    /// de la fenêtre.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        let Some(fenetre) = ctx.memory(|m| m.area_rect(egui::Id::new("carte_tactique"))) else { return };
        // Le bas de la fenêtre est à la carte, sous la barre d'outils.
        let centre = fenetre.center() + vec2(0.0, 50.0);
        let i = self.images;
        let rien = egui::Modifiers::NONE;
        match self.scene {
            5 if i == 3 => {
                raw.events.push(egui::Event::PointerMoved(centre + vec2(-80.0, -120.0)));
                raw.events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: vec2(0.0, 300.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: rien,
                });
            }
            6 => {
                // Un arc de cercle, de bas à gauche vers le haut à droite.
                let arc = |t: f32| centre + vec2(-170.0 + 330.0 * t, 90.0 - 230.0 * t + 120.0 * (t * 3.1).sin());
                if i == 3 {
                    raw.events.push(egui::Event::PointerMoved(arc(0.0)));
                    raw.events.push(egui::Event::PointerButton {
                        pos: arc(0.0),
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: rien,
                    });
                } else if (4..=28).contains(&i) {
                    raw.events.push(egui::Event::PointerMoved(arc((i - 4) as f32 / 24.0)));
                } else if i == 29 {
                    raw.events.push(egui::Event::PointerButton {
                        pos: arc(1.0),
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: rien,
                    });
                }
            }
            _ => {}
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ui.painter().rect_filled(ctx.content_rect(), 0.0, ki_ui::jetons::couleur::BG_BASE);
        let plans = &self.plans;
        let choisi = self.tactique.choisi(plans);
        let minimap = choisi.and_then(|p| self.minimaps.get(&p.nom)).filter(|_| !self.sans_minimap);
        self.tactique.fenetre(&ctx, plans, minimap, MOI, pour_pseudo(NOMS[MOI as usize]));
        // Personne pour les recevoir : les gestes du banc restent ici.
        let _ = self.tactique.a_envoyer();

        let Some(dossier) = self.capture.clone() else {
            if ctx.input(|i| i.key_pressed(egui::Key::Space)) {
                self.scene = (self.scene + 1) % SCENES;
                self.poser();
            }
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        };
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let prefixe = if petit() { "petit-" } else { "" };
            let chemin = dossier.join(format!("{prefixe}tactique-{}.png", self.scene + 1));
            let fichier = std::fs::File::create(&chemin).expect("dossier de capture");
            let mut png = png::Encoder::new(std::io::BufWriter::new(fichier), image.width() as u32, image.height() as u32);
            png.set_color(png::ColorType::Rgba);
            png.set_depth(png::BitDepth::Eight);
            let octets: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            png.write_header().and_then(|mut w| w.write_image_data(&octets)).expect("écriture du PNG");
            println!("{}", chemin.display());
            self.scene += 1;
            if self.scene >= SCENES {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            self.poser();
        }
        self.images += 1;
        // Les scènes à gestes attendent leur fin : la molette se lisse sur
        // quelques images, le trait se tire sur vingt-cinq.
        let pret = match self.scene {
            5 => self.images > 40,
            6 => self.images > 34,
            _ => self.images > 8,
        };
        if !self.demandee && pret {
            self.demandee = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        // Le banc tourne en continu : les faux gestes vont image par image.
        ctx.request_repaint();
    }
}
