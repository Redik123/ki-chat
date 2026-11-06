//! Le banc des suggestions de la saisie : une zone de saisie et son panneau, sur
//! des membres inventés, sans serveur ni ki-chat autour.
//!
//! `cargo run -p ki-client-gui --example banc_suggestions`
//!
//! `BANC_CAPTURE=dossier` : le banc photographie chaque scène
//! (`suggestions-1.png`, `suggestions-2.png`…) puis se ferme. Sans capture, on
//! y tape pour de vrai.

#[allow(dead_code)]
#[path = "../src/suggestions.rs"]
mod suggestions;

use eframe::egui;
use ki_ui::jetons::{couleur as c, espace, marge, rayon};
use suggestions::{Membre, Sources, Suggestions};

/// Ce qui est écrit dans la zone de saisie, scène par scène.
const SCENES: [&str; 7] = ["salut @", "@mar", "/", "/pa", ":feu", "/poke @", "ça marche `@code`"];

/// La dernière capture rendue par egui : son rappel la dépose ici.
static CAPTURE: std::sync::Mutex<Option<std::sync::Arc<egui::ColorImage>>> = std::sync::Mutex::new(None);

struct Banc {
    scene: usize,
    images: u32,
    demandee: bool,
    capture: Option<std::path::PathBuf>,
    texte: String,
    suggestions: Suggestions,
}

fn id() -> egui::Id {
    egui::Id::unique("saisie_de_banc")
}

fn sources() -> Sources {
    let membre = |pseudo: &str, couleur, en_ligne, detail| Membre { pseudo: pseudo.into(), couleur, en_ligne, detail };
    Sources {
        membres: vec![
            membre("Jerem", egui::Color32::from_rgb(240, 110, 110), true, "en vocal"),
            membre("Marie", egui::Color32::from_rgb(120, 200, 255), true, "en ligne"),
            membre("Marc", egui::Color32::from_rgb(120, 220, 160), false, "hors ligne"),
            membre("Anne-Marie", egui::Color32::from_rgb(250, 190, 90), true, "en ligne"),
            membre("Pandora", egui::Color32::from_rgb(200, 150, 255), true, "en vocal"),
            membre("Un pseudo très long qui remplit", c::TEXT, true, "en ligne"),
        ],
        commandes: vec![
            suggestions::POKE,
            suggestions::MUSIQUE,
            suggestions::PAUSE,
            suggestions::SUIVANT,
            suggestions::CARTES,
        ],
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([720.0, 460.0]),
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native(
        "banc-suggestions",
        options,
        Box::new(|cc| {
            ki_ui::style::installer(&cc.egui_ctx);
            let capture = std::env::var_os("BANC_CAPTURE").map(std::path::PathBuf::from);
            let texte = if capture.is_some() { SCENES[0].to_string() } else { String::new() };
            Ok(Box::new(Banc {
                scene: 0,
                images: 0,
                demandee: false,
                capture,
                texte,
                suggestions: Suggestions::default(),
            }))
        }),
    )
}

impl Banc {
    /// Le curseur au bout du texte, comme si on venait de l'écrire.
    fn curseur_a_la_fin(&self, ctx: &egui::Context) {
        let mut etat = egui::TextEdit::load_state(ctx, id()).unwrap_or_default();
        let fin = egui::text::CCursor::new(self.texte.chars().count());
        etat.cursor.set_char_range(Some(egui::text::CCursorRange::one(fin)));
        etat.store(ctx, id());
    }

    fn appliquer(&mut self, ctx: &egui::Context, choix: &suggestions::Choix) {
        let octet = |n: usize| self.texte.char_indices().nth(n).map_or(self.texte.len(), |(o, _)| o);
        let (a, b) = (octet(choix.debut), octet(choix.fin));
        self.texte.replace_range(a..b, &choix.insertion);
        let mut etat = egui::TextEdit::load_state(ctx, id()).unwrap_or_default();
        let apres = egui::text::CCursor::new(choix.debut + choix.insertion.chars().count());
        etat.cursor.set_char_range(Some(egui::text::CCursorRange::one(apres)));
        etat.store(ctx, id());
    }

    fn photographier(&mut self, ctx: &egui::Context) {
        let Some(dossier) = self.capture.clone() else { return };
        let image = CAPTURE.lock().ok().and_then(|mut c| c.take());
        if let Some(image) = image {
            let chemin = dossier.join(format!("suggestions-{}.png", self.scene + 1));
            let fichier = std::fs::File::create(&chemin).expect("dossier de capture");
            let mut png = png::Encoder::new(std::io::BufWriter::new(fichier), image.width() as u32, image.height() as u32);
            png.set_color(png::ColorType::Rgba);
            png.set_depth(png::BitDepth::Eight);
            let octets: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            png.write_header().and_then(|mut w| w.write_image_data(&octets)).expect("écriture du PNG");
            println!("{}", chemin.display());
            self.scene += 1;
            self.images = 0;
            self.demandee = false;
            match SCENES.get(self.scene) {
                Some(texte) => {
                    self.texte = (*texte).to_string();
                    self.curseur_a_la_fin(ctx);
                }
                None => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
            return;
        }
        if !self.demandee && self.images > 8 {
            self.demandee = true;
            ctx.request_screenshot(|image| {
                if let Ok(mut c) = CAPTURE.lock() {
                    *c = Some(image);
                }
            });
        }
    }
}

impl eframe::App for Banc {
    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.images += 1;
        if self.capture.is_some() {
            ctx.request_repaint();
            if self.images == 1 {
                self.curseur_a_la_fin(&ctx);
            }
        }
        self.suggestions.echap(&ctx);
        if let Some(choix) = self.suggestions.clavier(&ctx) {
            self.appliquer(&ctx, &choix);
        }
        let mut curseur = None;
        let mut focus = false;
        let mut ancre = egui::Rect::NOTHING;
        egui::Panel::bottom("saisie").frame(egui::Frame::NONE.fill(c::BG_BASE).inner_margin(marge::egale(espace::L))).show(ui, |ui| {
            let cadre = egui::Frame::NONE
                .fill(c::BG_RAISED)
                .stroke(egui::Stroke::new(1.0_f32, c::BORDER))
                .corner_radius(egui::CornerRadius::same(rayon::XL))
                .inner_margin(marge::symetrique(espace::M, espace::S))
                .show(ui, |ui| {
                    let sortie = egui::TextEdit::multiline(&mut self.texte)
                        .id(id())
                        .desired_rows(1)
                        .desired_width(f32::INFINITY)
                        .frame(egui::Frame::new().inner_margin(marge::symetrique(espace::XS, espace::XS)))
                        .hint_text("Message dans #général")
                        .show(ui);
                    curseur = sortie.cursor_range.filter(|r| r.primary == r.secondary).map(|r| r.primary.index.0);
                    let reponse = sortie.response.response;
                    if self.images <= 2 {
                        reponse.request_focus();
                    }
                    focus = reponse.has_focus();
                });
            ancre = cadre.response.rect;
        });
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(c::BG_BASE)).show(ui, |_ui| {});
        self.suggestions.actualiser(&ctx, &self.texte, curseur, focus, sources);
        self.suggestions.retenir_le_focus(&ctx, id());
        if let Some(choix) = self.suggestions.panneau(&ctx, ancre) {
            self.appliquer(&ctx, &choix);
            ctx.memory_mut(|m| m.request_focus(id()));
        }
        self.photographier(&ctx);
    }
}
