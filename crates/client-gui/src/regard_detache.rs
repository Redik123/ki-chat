//! Le stream regardé dans sa propre fenêtre système, à poser sur un second
//! écran ou à mettre en plein écran.
//!
//! Une fenêtre « différée » d'egui : elle a ses propres images, au rythme
//! du stream, sans repeindre la fenêtre principale à chacune — celle-ci peut
//! être réduite dans la zone de notification pendant ce temps (et eframe,
//! depuis 0.34, ne la réveille plus alors qu'au dixième de seconde). Le
//! décodeur la réveille directement (`partage::Regard::detache`) ; l'image,
//! elle la charge elle-même.
//!
//! Ses commandes — quitter, rattacher, plein écran, volume —, elle les
//! laisse à l'application : des demandes que celle-ci lit à sa prochaine
//! image, et pour lesquelles la fenêtre la réveille.
//!
//! L'image remplit la fenêtre, noir autour ; la barre de commandes se
//! montre au mouvement de la souris et s'efface deux secondes après. Échap
//! sort du plein écran, puis rattache ; F11 et le double-clic basculent le
//! plein écran ; la croix quitte le visionnage, comme dans ki-chat.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText, Sense, Vec2};

use crate::icons::Icon;
use crate::partage::{self, Cadence};
use crate::theme::{self, TEXT_DIM, TEXT_FAINT};
use crate::ui;

/// La fenêtre du stream détaché, pour qui veut la réveiller.
pub fn id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("regard-detache")
}

/// Ce que la fenêtre demande à l'application.
#[derive(Default)]
pub struct Demandes {
    pub quitter: bool,
    pub rattacher: bool,
    pub basculer_plein: bool,
    pub volume: Option<f32>,
}

/// La fenêtre détachée : vit tant que le stream est détaché.
pub struct Fenetre {
    etat: Arc<Mutex<Etat>>,
    icone: Arc<egui::IconData>,
}

/// Ce que la fenêtre et l'application se partagent. Les deux tournent sur
/// le même fil, l'une après l'autre : le verrou n'est jamais disputé.
struct Etat {
    // De l'application vers la fenêtre, à chaque image de l'application.
    plein_voulu: bool,
    volume: f32,
    meme_machine: bool,
    // À la fenêtre.
    image: Arc<Mutex<Option<egui::ColorImage>>>,
    images: Arc<AtomicU64>,
    basse: Arc<AtomicBool>,
    saut: Arc<AtomicU64>,
    tex: Option<egui::TextureHandle>,
    cadence: Cadence,
    /// Sa barre de titre a été passée en sombre — une fois par naissance
    /// de la fenêtre.
    barre_sombre: bool,
    /// Le dernier mouvement de souris : la barre de commandes s'efface peu
    /// après.
    souris: Instant,
    // De la fenêtre vers l'application.
    demandes: Demandes,
}

impl Fenetre {
    /// `tex` : l'image déjà affichée dans la fenêtre principale, reprise
    /// telle quelle — pas d'écran d'attente au détachement.
    pub fn new(r: &partage::Regard, tex: Option<egui::TextureHandle>) -> Self {
        let etat = Etat {
            plein_voulu: false,
            volume: 1.0,
            meme_machine: false,
            image: r.image.clone(),
            images: r.images.clone(),
            basse: r.basse.clone(),
            saut: r.saut.clone(),
            tex,
            cadence: Cadence::new(),
            barre_sombre: false,
            souris: Instant::now(),
            demandes: Demandes::default(),
        };
        Self { etat: Arc::new(Mutex::new(etat)), icone: Arc::new(theme::app_icon()) }
    }

    /// À chaque image de l'application : ce qu'elle veut, contre ce que la
    /// fenêtre demande depuis la dernière fois.
    pub fn echanger(&self, plein_voulu: bool, volume: f32, meme_machine: bool) -> Demandes {
        let mut e = self.etat.lock().unwrap();
        e.plein_voulu = plein_voulu;
        e.meme_machine = meme_machine;
        if e.demandes.volume.is_none() {
            e.volume = volume;
        }
        std::mem::take(&mut e.demandes)
    }

    /// L'image du stream, rendue à la fenêtre principale au rattachement.
    pub fn rendre_texture(&self) -> Option<egui::TextureHandle> {
        self.etat.lock().unwrap().tex.take()
    }

    /// À chaque image de l'application, tant que le stream est détaché :
    /// sans cet appel, egui ferme la fenêtre.
    pub fn montrer(&self, ctx: &egui::Context, titre: &str) {
        let builder = egui::ViewportBuilder::default()
            .with_title(format!("{titre} — ki-chat"))
            .with_inner_size([1280.0, 720.0])
            .with_min_inner_size([320.0, 180.0])
            .with_icon(self.icone.clone());
        let etat = self.etat.clone();
        ctx.show_viewport_deferred(id(), builder, move |ctx, _classe| {
            etat.lock().unwrap().dessiner(ctx);
        });
    }
}

impl Etat {
    fn dessiner(&mut self, ctx: &egui::Context) {
        if let Some(image) = self.image.lock().unwrap().take() {
            match &mut self.tex {
                Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                None => self.tex = Some(ctx.load_texture("regard", image, egui::TextureOptions::LINEAR)),
            }
        }
        self.cadence.relever(self.images.load(Ordering::Relaxed), 0);
        let etat = ligne_etat(
            self.tex.as_ref(),
            self.cadence.fps,
            self.basse.load(Ordering::Relaxed),
            self.saut.load(Ordering::Relaxed),
        );

        if !self.barre_sombre {
            self.barre_sombre = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
        }
        // Le plein écran suit ce qu'on veut, dès que la fenêtre existe.
        let plein = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if plein != self.plein_voulu {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.plein_voulu));
        }
        let mut d = Demandes::default();
        if ctx.input(|i| i.viewport().close_requested()) {
            d.quitter = true;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if plein {
                d.basculer_plein = true;
            } else {
                d.rattacher = true;
            }
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F11)) {
            d.basculer_plein = true;
        }
        if ctx.input(|i| i.pointer.delta() != Vec2::ZERO || i.pointer.any_down()) {
            self.souris = Instant::now();
            // Pour effacer la barre à l'heure, même si rien d'autre ne
            // repeint d'ici là.
            ctx.request_repaint_after(Duration::from_millis(2600));
        }
        let barre_visible = self.souris.elapsed() < Duration::from_millis(2500);

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::BLACK))
            .show(ctx, |ui| {
                let dispo = ui.max_rect();
                let toile = ui.interact(dispo, egui::Id::new("regard-toile"), Sense::click());
                if toile.double_clicked() {
                    d.basculer_plein = true;
                }
                match &self.tex {
                    Some(tex) => {
                        let taille = tex.size_vec2();
                        let echelle = (dispo.width() / taille.x).min(dispo.height() / taille.y);
                        let rect = egui::Rect::from_center_size(dispo.center(), taille * echelle);
                        ui.painter().image(
                            tex.id(),
                            rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    None => {
                        ui.painter().text(
                            dispo.center(),
                            egui::Align2::CENTER_CENTER,
                            "en attente de la première image…",
                            egui::FontId::proportional(16.0),
                            TEXT_DIM,
                        );
                    }
                }
                if !barre_visible {
                    return;
                }
                let barre = egui::Rect::from_min_max(egui::pos2(dispo.left(), dispo.bottom() - 46.0), dispo.max);
                ui.painter().rect_filled(barre, 0.0, theme::alpha(Color32::BLACK, 175));
                let mut ligne = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(barre.shrink2(Vec2::new(12.0, 7.0)))
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                if ui::button(&mut ligne, Icon::Close, "Quitter").clicked() {
                    d.quitter = true;
                }
                if ui::button(&mut ligne, Icon::ChevronLeft, "Rattacher")
                    .on_hover_text("revenir dans la fenêtre de ki-chat (Échap)")
                    .clicked()
                {
                    d.rattacher = true;
                }
                let mot = if plein { "Quitter le plein écran" } else { "Plein écran" };
                if ui::button(&mut ligne, Icon::Screen, mot)
                    .on_hover_text("F11, ou double-clic sur l'image")
                    .clicked()
                {
                    d.basculer_plein = true;
                }
                ligne.add_space(8.0);
                if self.meme_machine {
                    ligne.label(
                        RichText::new("son du jeu coupé : le streamer est sur ce PC")
                            .color(TEXT_FAINT)
                            .size(11.5),
                    );
                } else {
                    let mut pct = self.volume * 100.0;
                    if ligne
                        .add(egui::Slider::new(&mut pct, 0.0..=200.0).text("son du jeu").suffix(" %").integer())
                        .on_hover_text(
                            "le son du stream est ramené au niveau des voix : 100 %, c'est ce niveau ; \
                             ce curseur l'ajuste",
                        )
                        .changed()
                    {
                        self.volume = pct / 100.0;
                        d.volume = Some(self.volume);
                    }
                }
                if !etat.is_empty() {
                    ligne.label(RichText::new(etat).color(TEXT_FAINT).size(11.0).monospace());
                }
            });

        // Les demandes s'ajoutent à celles que l'application n'a pas encore
        // lues, et la réveillent : réduite, elle ne repeint pas d'elle-même.
        let rien = !d.quitter && !d.rattacher && !d.basculer_plein && d.volume.is_none();
        if !rien {
            let en_cours = &mut self.demandes;
            en_cours.quitter |= d.quitter;
            en_cours.rattacher |= d.rattacher;
            en_cours.basculer_plein ^= d.basculer_plein;
            if d.volume.is_some() {
                en_cours.volume = d.volume;
            }
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
    }
}

/// La ligne d'état d'un stream regardé : sa taille, sa cadence, et ce qui
/// pourrait passer pour une panne — la qualité basse (sa connexion ne
/// suivait pas la haute), les sauts (son PC ne décode pas assez vite,
/// l'image saute pour rester à l'heure ; dit pendant dix secondes).
pub fn ligne_etat(tex: Option<&egui::TextureHandle>, fps: f32, basse: bool, saut_ms: u64) -> String {
    let Some(tex) = tex else { return String::new() };
    let [w, h] = tex.size();
    let mut base = format!("{w}x{h} · {fps:.0} i/s");
    if basse {
        base.push_str(" · qualité réduite pour ta connexion");
    }
    let maintenant = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if saut_ms > 0 && maintenant.saturating_sub(saut_ms) < 10_000 {
        base.push_str(" · ton PC ne suit pas : l'image saute pour rester à l'heure");
    }
    base
}
