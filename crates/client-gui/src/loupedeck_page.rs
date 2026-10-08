//! La page Loupedeck : un onglet par page de la grille, l'appareil dessiné
//! avec l'aperçu de la page choisie — branché ou non, ki-chat le dessine
//! comme l'appareil le ferait —, et le réglage de ce qu'on y clique : une
//! touche (son contenu, son action, son icône, son texte, sa couleur), un
//! bouton rond (son action, la couleur de sa lumière) ou une molette. Plus
//! bas, les couleurs de l'écran et les réglages des pages. Elle s'ouvre par
//! le bouton « Loupedeck » de la barre du bas, qui n'apparaît que si le
//! pilotage est allumé dans les réglages (onglet Bêta).

use eframe::egui::{self, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};

use crate::icons::Icon;
use crate::loupedeck::Peintre;
use crate::loupedeck_config::{self, Action, ActionMolette, Bouton, Case, Config, Widget, GENRES};
use crate::loupedeck_ui::{Apercu, Selection};
use crate::theme::{self, ACCENT, TEXT, TEXT_DIM, TEXT_FAINT};
use crate::ui::{self, Tone};
use crate::{KiApp, VoiceSnapshot};

/// La place d'une colonne de molettes, de chaque côté de l'écran, et celle
/// de la rangée des ronds dessous — à l'échelle 1, l'écran à 480 × 270.
const COLONNE: f32 = 68.0;
const RANGEE_RONDS: f32 = 84.0;
/// L'appareil dessiné à l'échelle 1 : il grandit avec la fenêtre.
const APPAREIL: Vec2 = Vec2::new(480.0 + 2.0 * COLONNE, 270.0 + RANGEE_RONDS);
/// Entre deux dessins de l'aperçu, quand une image manquait (une photo pas
/// encore sur le disque).
const RELANCE_APERCU: std::time::Duration = std::time::Duration::from_secs(2);

/// Les molettes, comme on les nomme.
const MOLETTES: [&str; 6] = [
    "Molette en haut à gauche",
    "Molette du milieu à gauche",
    "Molette en bas à gauche",
    "Molette en haut à droite",
    "Molette du milieu à droite",
    "Molette en bas à droite",
];

/// Les couleurs proposées d'un clic, avant le nuancier.
const NUANCES: [[u8; 3]; 10] = [
    [0, 210, 106],
    [0, 180, 220],
    [40, 140, 255],
    [150, 90, 255],
    [230, 40, 255],
    [255, 70, 85],
    [255, 140, 0],
    [255, 190, 0],
    [235, 235, 235],
    [120, 120, 120],
];

fn depuis_565(v: u16) -> [u8; 3] {
    [((v >> 11) << 3) as u8, (((v >> 5) & 0x3f) << 2) as u8, ((v & 0x1f) << 3) as u8]
}

/// Quelques pastilles, le nuancier, et le retour à la couleur d'origine.
fn couleur_ui(ui: &mut egui::Ui, couleur: &mut Option<[u8; 3]>, origine: [u8; 3]) {
    let mut c = couleur.unwrap_or(origine);
    for n in NUANCES {
        let (rect, rep) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
        ui.painter().rect_filled(rect, 4.0, Color32::from_rgb(n[0], n[1], n[2]));
        if c == n {
            ui.painter().rect_stroke(rect.expand(2.0), 5.0, Stroke::new(1.5_f32, TEXT), StrokeKind::Outside);
        }
        if rep.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            *couleur = Some(n);
        }
    }
    if ui.color_edit_button_srgb(&mut c).changed() {
        *couleur = Some(c);
    }
    if couleur.is_some() && ui.small_button("d'origine").clicked() {
        *couleur = None;
    }
}

/// Le choix d'une action : son genre, puis son paramètre (la page, le
/// salon, le son). `ptt` : le push-to-talk se tient, il n'a de sens que sur
/// un bouton rond.
fn choix_action(
    ui: &mut egui::Ui,
    id: &str,
    action: &mut Action,
    ptt: bool,
    pages: &[String],
    salons: &[String],
    sons: &[String],
) {
    egui::ComboBox::from_id_salt(id)
        .width(260.0)
        .selected_text(RichText::new(action.nom()).color(TEXT))
        .show_ui(ui, |ui| {
            for genre in GENRES.iter() {
                if !ptt && *genre == Action::Ptt {
                    continue;
                }
                let choisi = action.meme_genre(genre);
                if ui.selectable_label(choisi, genre.nom()).clicked() && !choisi {
                    *action = match genre {
                        Action::Page(_) => Action::Page(0),
                        Action::Salon(_) => Action::Salon(salons.first().cloned().unwrap_or_default()),
                        Action::Son(_) => Action::Son(sons.first().cloned().unwrap_or_default()),
                        autre => autre.clone(),
                    };
                }
            }
        });
    let parametre = |ui: &mut egui::Ui, valeur: &mut String, liste: &[String], vide: &str| {
        if liste.is_empty() {
            ui.label(RichText::new(vide).color(TEXT_FAINT).size(12.0));
            return;
        }
        egui::ComboBox::from_id_salt((id, "parametre"))
            .width(200.0)
            .selected_text(RichText::new(valeur.as_str()).color(TEXT))
            .show_ui(ui, |ui| {
                for nom in liste {
                    ui.selectable_value(valeur, nom.clone(), nom);
                }
            });
    };
    match action {
        Action::Page(n) => {
            egui::ComboBox::from_id_salt((id, "page"))
                .width(200.0)
                .selected_text(RichText::new(pages.get(*n).map_or("?", String::as_str)).color(TEXT))
                .show_ui(ui, |ui| {
                    for (i, nom) in pages.iter().enumerate() {
                        ui.selectable_value(n, i, nom);
                    }
                });
        }
        Action::Salon(nom) => parametre(ui, nom, salons, "aucun salon vocal sur ce serveur"),
        Action::Son(nom) => {
            parametre(ui, nom, sons, "ta soundboard est vide : ajoute des sons depuis sa fenêtre")
        }
        _ => {}
    }
}

impl KiApp {
    /// La fenêtre, si elle est ouverte, et l'aperçu de la page qu'on y
    /// regarde.
    pub(crate) fn loupedeck_window(&mut self, ctx: &egui::Context, voice: &VoiceSnapshot) {
        if !(self.loupedeck_etat.page_ouverte && self.loupedeck_etat.actif) {
            self.loupedeck_etat.apercu = None;
            return;
        }
        let n = self.loupedeck_etat.onglet.min(self.loupedeck_etat.config.pages.len() - 1);
        self.loupedeck_etat.onglet = n;
        self.preparer_page(ctx, n);
        self.mettre_apercu_a_jour(ctx, n, voice);

        // On la déplace et on l'agrandit à volonté ; egui retient sa place
        // et sa taille d'une session à l'autre.
        let ecran = ctx.content_rect();
        let mut ouvert = true;
        egui::Window::new("Loupedeck Live")
            .open(&mut ouvert)
            .collapsible(false)
            .resizable(true)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ecran.center())
            .default_size([780.0_f32.min(ecran.width() - 40.0), 860.0_f32.min(ecran.height() - 60.0)])
            .min_size([560.0, 420.0])
            .show(ctx, |ui| self.loupedeck_page_ui(ui));
        if !ouvert {
            self.loupedeck_etat.page_ouverte = false;
        }
    }

    /// Redessine l'aperçu quand ce qu'il montre change — ou, une image y
    /// manquant, de temps en temps, le temps qu'elle arrive.
    fn mettre_apercu_a_jour(&mut self, ctx: &egui::Context, n: usize, voice: &VoiceSnapshot) {
        let affichage = self.affichage_page(n, voice);
        let apercu = self.loupedeck_etat.apercu.get_or_insert_with(|| Apercu {
            peintre: Peintre::new(),
            dernier: None,
            texture: None,
            complet: true,
            dessine: std::time::Instant::now(),
        });
        let relance = !apercu.complet && apercu.dessine.elapsed() > RELANCE_APERCU;
        if apercu.dernier.as_ref() == Some(&affichage) && !relance {
            return;
        }
        let (pixels, complet) = apercu.peintre.ecran(&affichage);
        let rgb: Vec<u8> = pixels.iter().flat_map(|v| depuis_565(*v)).collect();
        let image = egui::ColorImage::from_rgb([480, 270], &rgb);
        let options = egui::TextureOptions::LINEAR;
        match &mut apercu.texture {
            Some(texture) => texture.set(image, options),
            None => apercu.texture = Some(ctx.load_texture("loupedeck-apercu", image, options)),
        }
        apercu.dernier = Some(affichage);
        apercu.complet = complet;
        apercu.dessine = std::time::Instant::now();
        if !complet {
            ctx.request_repaint_after(RELANCE_APERCU);
        }
    }

    fn loupedeck_page_ui(&mut self, ui: &mut egui::Ui) {
        // --- L'état et la luminosité ---------------------------------------
        let (texte, couleur, detail) = self.statut_loupedeck();
        ui.horizontal(|ui| {
            let r = ui.label(RichText::new(texte).color(couleur).size(12.5));
            if let Some(e) = detail {
                r.on_hover_text(e);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut l = self.loupedeck_etat.config.luminosite as f32;
                if ui.add(egui::Slider::new(&mut l, 0.0..=10.0).step_by(1.0).show_value(false)).changed() {
                    self.loupedeck_etat.config.luminosite = l as u8;
                }
                ui.label(RichText::new("Luminosité").color(TEXT_DIM).size(12.0));
            });
        });
        ui.add_space(6.0);

        // L'appareil prend toute la largeur, sans dépasser les deux tiers de
        // la hauteur : agrandir la fenêtre l'agrandit.
        let echelle =
            (ui.available_width() / APPAREIL.x).min(ui.available_height() * 0.62 / APPAREIL.y).clamp(0.75, 3.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            self.onglets_ui(ui);
            ui.add_space(6.0);
            ui.vertical_centered(|ui| self.dessin_loupedeck(ui, echelle));
            ui.add_space(8.0);
            self.reglage_selection_ui(ui);
            ui.add_space(10.0);
            ui::hairline(ui);
            ui.add_space(8.0);
            self.reglages_generaux_ui(ui);
        });
    }

    /// Un onglet par page (le point : celle que montre l'appareil), « + »
    /// pour en ajouter ; sous les onglets, le nom de la page regardée.
    fn onglets_ui(&mut self, ui: &mut egui::Ui) {
        let etat = &mut self.loupedeck_etat;
        let mut onglet = None;
        let mut ajouter = false;
        ui.horizontal_wrapped(|ui| {
            for (i, page) in etat.config.pages.iter().enumerate() {
                let texte = if i == etat.ecran { format!("{}  ●", page.nom) } else { page.nom.clone() };
                let bulle = if i == etat.ecran { "la page que montre l'appareil" } else { "voir et régler cette page" };
                if ui.selectable_label(i == etat.onglet, RichText::new(texte).size(13.0)).on_hover_text(bulle).clicked() {
                    onglet = Some(i);
                }
            }
            if ui::icon_button_ex(ui, Icon::Plus, 22.0, "ajouter une page", None).clicked() {
                ajouter = true;
            }
        });
        if ajouter {
            onglet = Some(etat.config.ajouter_page());
        }
        if let Some(i) = onglet {
            etat.onglet = i;
            if matches!(etat.selection, Some(Selection::Case(_))) {
                etat.selection = None;
            }
        }

        let mut retirer = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Nom").color(TEXT_DIM).size(12.5));
            let page = &mut etat.config.pages[etat.onglet];
            ui.add(ui::text_field(&mut page.nom, "nom de la page", false).desired_width(160.0));
            if page.nom.chars().count() > 20 {
                page.nom = page.nom.chars().take(20).collect();
            }
            if etat.onglet != etat.ecran && ui::button(ui, Icon::Screen, "La montrer sur l'appareil").clicked() {
                etat.ecran = etat.onglet;
            }
            if etat.config.pages.len() > 1
                && ui::tinted_button(ui, Some(Icon::Trash), "Retirer la page", Tone::Danger).clicked()
            {
                retirer = true;
            }
        });
        if retirer {
            let n = etat.onglet;
            etat.config.retirer_page(n);
            etat.onglet = n.min(etat.config.pages.len() - 1);
            if etat.ecran >= n {
                etat.ecran = etat.ecran.saturating_sub(1).min(etat.config.pages.len() - 1);
            }
            etat.selection = None;
        }
    }

    /// L'appareil : l'aperçu de la page regardée, trois molettes de chaque
    /// côté, huit ronds dessous avec leur lumière. Un clic choisit ce qu'on
    /// règle — une touche de l'écran, une molette (ou sa case sur une
    /// bande), un rond ; un second clic le relâche.
    fn dessin_loupedeck(&mut self, ui: &mut egui::Ui, e: f32) {
        let ecran = Vec2::new(480.0, 270.0) * e;
        let (cadre, _) = ui.allocate_exact_size(APPAREIL * e, Sense::hover());
        let p = ui.painter_at(cadre);
        p.rect_filled(cadre, 16.0 * e, Color32::from_rgb(0x15, 0x18, 0x1d));
        p.rect_stroke(cadre, 16.0 * e, Stroke::new(1.0_f32, theme::BORDER), StrokeKind::Inside);

        let zone = Rect::from_min_size(Pos2::new(cadre.center().x - ecran.x / 2.0, cadre.top() + 14.0 * e), ecran);
        let apercu = self.loupedeck_etat.apercu.as_ref();
        match apercu.and_then(|a| a.texture.as_ref()) {
            Some(texture) => {
                let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                p.image(texture.id(), zone, uv, Color32::WHITE);
            }
            None => {
                p.rect_filled(zone, 6.0, Color32::from_rgb(0x08, 0x0a, 0x0d));
            }
        }
        let lumieres = apercu.and_then(|a| a.dernier.as_ref()).map(|a| a.ronds).unwrap_or_default();

        let config = self.loupedeck_etat.config.clone();
        let selection = self.loupedeck_etat.selection;
        let pages = &config.pages;
        let mut clic = None;

        // L'écran : une touche, ou la case d'une molette sur une bande.
        let rep = ui.interact(zone, ui.id().with("loupedeck-ecran"), Sense::click());
        let case_sous = |pos: Pos2| -> Selection {
            let (x, y) = ((pos.x - zone.left()) / e, (pos.y - zone.top()) / e);
            let rangee = ((y / 90.0) as usize).min(2);
            if x < 60.0 {
                Selection::Molette(rangee)
            } else if x >= 420.0 {
                Selection::Molette(3 + rangee)
            } else {
                Selection::Case(rangee * 4 + (((x - 60.0) / 90.0) as usize).min(3))
            }
        };
        let rect_de = |s: Selection| -> Option<Rect> {
            let (x, y, l) = match s {
                Selection::Case(t) => (60.0 + (t % 4) as f32 * 90.0, (t / 4) as f32 * 90.0, 90.0),
                Selection::Molette(m) if m < 3 => (0.0, m as f32 * 90.0, 60.0),
                Selection::Molette(m) => (420.0, (m - 3) as f32 * 90.0, 60.0),
                Selection::Rond(_) => return None,
            };
            Some(Rect::from_min_size(zone.min + Vec2::new(x, y) * e, Vec2::new(l, 90.0) * e))
        };
        if let Some(pos) = rep.hover_pos() {
            if let Some(r) = rect_de(case_sous(pos)) {
                p.rect_stroke(r.shrink(2.0), 6.0, Stroke::new(1.0_f32, TEXT_DIM), StrokeKind::Inside);
            }
        }
        if let Some(r) = selection.and_then(rect_de) {
            p.rect_stroke(r.shrink(1.0), 6.0, Stroke::new(2.5_f32, ACCENT), StrokeKind::Inside);
        }
        if rep.clicked() {
            clic = rep.interact_pointer_pos().map(case_sous);
        }

        // Les molettes, en face des tiers de l'écran.
        for (i, action) in config.molettes.iter().enumerate() {
            let gauche = i < 3;
            let cx = if gauche { zone.left() - COLONNE * e / 2.0 } else { zone.right() + COLONNE * e / 2.0 };
            let cy = zone.top() + ((i % 3) as f32 + 0.5) * zone.height() / 3.0 - 5.0 * e;
            let centre = Pos2::new(cx, cy);
            let r = 18.0 * e;
            let rep = ui.interact(
                Rect::from_center_size(centre, Vec2::splat(2.0 * r + 7.0 * e)),
                ui.id().with(("loupedeck-molette", i)),
                Sense::click(),
            );
            let choisie = selection == Some(Selection::Molette(i));
            let fond = if rep.hovered() { Color32::from_rgb(0x34, 0x39, 0x42) } else { Color32::from_rgb(0x26, 0x2a, 0x31) };
            p.circle_filled(centre, r, fond);
            p.line_segment(
                [centre + Vec2::new(0.0, -r + 3.5 * e), centre + Vec2::new(0.0, -r + 9.5 * e)],
                Stroke::new(2.0_f32, TEXT_DIM),
            );
            if choisie {
                p.circle_stroke(centre, r + 3.0 * e, Stroke::new(2.0_f32, ACCENT));
            }
            let couleur = if choisie { ACCENT } else if *action == ActionMolette::Rien { TEXT_FAINT } else { TEXT_DIM };
            p.text(centre + Vec2::new(0.0, r + 9.5 * e), egui::Align2::CENTER_CENTER, action.court(), FontId::proportional(10.0 * e), couleur);
            if rep.on_hover_text(action.nom()).clicked() {
                clic = Some(Selection::Molette(i));
            }
        }

        // Les ronds, sous l'écran, avec la couleur de leur lumière.
        let pas = zone.width() / 8.0;
        for (i, action) in config.ronds.iter().enumerate() {
            let centre = Pos2::new(zone.left() + pas * (i as f32 + 0.5), zone.bottom() + 30.0 * e);
            let r = 14.0 * e;
            let rep = ui.interact(
                Rect::from_center_size(centre, Vec2::splat(2.0 * r + 7.0 * e)),
                ui.id().with(("loupedeck-rond", i)),
                Sense::click(),
            );
            let choisi = selection == Some(Selection::Rond(i));
            let fond = if rep.hovered() { Color32::from_rgb(0x30, 0x35, 0x3d) } else { Color32::from_rgb(0x21, 0x24, 0x2a) };
            p.circle_filled(centre, r, fond);
            let [rr, gg, bb] = lumieres[i];
            let lumiere = if [rr, gg, bb] == [0, 0, 0] { Color32::from_rgb(0x30, 0x34, 0x3b) } else { Color32::from_rgb(rr, gg, bb) };
            p.circle_stroke(centre, r - 1.7 * e, Stroke::new(2.2 * e, lumiere));
            // Comme sur l'appareil : un cercle sur le premier, des chiffres
            // sur les autres.
            if i == 0 {
                p.circle_stroke(centre, 3.5 * e, Stroke::new(1.5_f32, TEXT_DIM));
            } else {
                p.text(centre, egui::Align2::CENTER_CENTER, i.to_string(), FontId::proportional(10.0 * e), TEXT_DIM);
            }
            if choisi {
                p.circle_stroke(centre, r + 3.0 * e, Stroke::new(2.0_f32, ACCENT));
            }
            let couleur = if choisi { ACCENT } else if *action == Action::Rien { TEXT_FAINT } else { TEXT_DIM };
            let mut court = action.court(pages);
            if court.chars().count() > 9 {
                court = court.chars().take(8).collect::<String>() + "…";
            }
            p.text(centre + Vec2::new(0.0, r + 9.5 * e), egui::Align2::CENTER_CENTER, court, FontId::proportional(10.0 * e), couleur);
            if rep.on_hover_text(action.nom()).clicked() {
                clic = Some(Selection::Rond(i));
            }
        }
        if let Some(s) = clic {
            self.loupedeck_etat.selection = if selection == Some(s) { None } else { Some(s) };
            self.loupedeck_etat.choix_icone = false;
        }
    }

    /// Le réglage de ce qu'on a cliqué sur l'appareil dessiné.
    fn reglage_selection_ui(&mut self, ui: &mut egui::Ui) {
        // Les listes des choix, lues avant de prêter la configuration.
        let noms_pages: Vec<String> = self.loupedeck_etat.config.pages.iter().map(|p| p.nom.clone()).collect();
        let salons: Vec<String> = self
            .channels
            .iter()
            .filter(|c| c.kind == ki_protocol::ChannelKind::Voice)
            .map(|c| c.name.clone())
            .collect();
        self.soundboard.preparer();
        let sons: Vec<String> = self.soundboard.sons.iter().map(|s| s.nom.clone()).collect();
        let titre = |ui: &mut egui::Ui, texte: &str| {
            ui.label(RichText::new(texte).color(TEXT).size(13.5).strong());
            ui.add_space(4.0);
        };

        let etat = &mut self.loupedeck_etat;
        match etat.selection {
            None => {
                ui.label(
                    RichText::new(
                        "Clique sur une touche de l'écran, un bouton rond ou une molette pour choisir ce qu'il \
                         fait.",
                    )
                    .color(TEXT_DIM)
                    .size(12.5),
                );
            }
            Some(Selection::Rond(i)) => {
                let nom = if i == 0 { "Bouton rond ○, le premier à gauche".to_string() } else { format!("Bouton rond {i}") };
                titre(ui, &nom);
                let config = &mut etat.config;
                ui.horizontal(|ui| {
                    choix_action(ui, "loupedeck_rond", &mut config.ronds[i], true, &noms_pages, &salons, &sons);
                });
                ui::precision(ui, config.ronds[i].aide());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Lumière").color(TEXT_DIM).size(12.5));
                    let origine = config.ronds[i].couleur();
                    couleur_ui(ui, &mut config.couleurs_ronds[i], origine);
                });
                ui::precision(
                    ui,
                    "En veilleuse au repos, vive quand l'action est allumée. Le rouge des alertes — micro \
                     coupé, sourd, en direct — ne change pas.",
                );
            }
            Some(Selection::Molette(i)) => {
                titre(ui, MOLETTES[i]);
                let actuel = &mut etat.config.molettes[i];
                egui::ComboBox::from_id_salt("loupedeck_molette")
                    .width(260.0)
                    .selected_text(RichText::new(actuel.nom()).color(TEXT))
                    .show_ui(ui, |ui| {
                        for a in ActionMolette::TOUTES {
                            ui.selectable_value(actuel, a, a.nom());
                        }
                    });
                ui::precision(ui, actuel.aide());
            }
            Some(Selection::Case(t)) => {
                let onglet = etat.onglet;
                titre(ui, &format!("Touche {} de la page « {} »", t + 1, noms_pages[onglet]));
                // Le texte d'origine du bouton, en indice du champ.
                let origine = match &etat.config.pages[onglet].cases[t] {
                    Case::Bouton(b) => b.action.court(&etat.config.pages),
                    _ => String::new(),
                };
                let case = &mut etat.config.pages[onglet].cases[t];
                contenu_ui(ui, case, t);
                if let Case::Bouton(b) = case {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Action").color(TEXT_DIM).size(12.5));
                        choix_action(ui, "loupedeck_case", &mut b.action, false, &noms_pages, &salons, &sons);
                    });
                    ui::precision(ui, b.action.aide());
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Texte").color(TEXT_DIM).size(12.5));
                        ui.add(ui::text_field(&mut b.texte, &origine, false).desired_width(170.0));
                        if b.texte.chars().count() > 16 {
                            b.texte = b.texte.chars().take(16).collect();
                        }
                        ui.add_space(10.0);
                        ui.label(RichText::new("Icône").color(TEXT_DIM).size(12.5));
                        let icone = b.icone.unwrap_or(b.action.icone());
                        if ui::icon_button_ex(ui, icone, 26.0, "choisir l'icône", Some(ACCENT)).clicked() {
                            etat.choix_icone = !etat.choix_icone;
                        }
                        if b.icone.is_some() && ui.small_button("d'origine").clicked() {
                            b.icone = None;
                        }
                    });
                    if etat.choix_icone {
                        ui.horizontal_wrapped(|ui| {
                            for icone in Icon::TOUTES {
                                if ui::icon_button_ex(ui, icone, 26.0, &format!("{icone:?}"), None).clicked() {
                                    b.icone = Some(icone);
                                    etat.choix_icone = false;
                                }
                            }
                        });
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Couleur").color(TEXT_DIM).size(12.5));
                        let origine = b.action.couleur();
                        couleur_ui(ui, &mut b.couleur, origine);
                    });
                }
            }
        }
    }

    /// Les couleurs de l'écran, et les réglages des pages.
    fn reglages_generaux_ui(&mut self, ui: &mut egui::Ui) {
        let etat = &mut self.loupedeck_etat;
        let config = &mut etat.config;
        ui.label(RichText::new("Couleurs de l'écran").color(TEXT).size(13.5).strong());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Fond").color(TEXT_DIM).size(12.5));
            ui.color_edit_button_srgb(&mut config.fond);
            ui.add_space(10.0);
            ui.label(RichText::new("Jauges").color(TEXT_DIM).size(12.5));
            ui.color_edit_button_srgb(&mut config.accent);
            if (config.fond, config.accent) != (loupedeck_config::FOND, loupedeck_config::ACCENT)
                && ui.small_button("d'origine").clicked()
            {
                config.fond = loupedeck_config::FOND;
                config.accent = loupedeck_config::ACCENT;
            }
        });
        ui.add_space(8.0);
        ui.checkbox(&mut config.valo_auto, "Montrer la première page VALORANT quand une partie commence");
        ui.horizontal(|ui| {
            ui.label(RichText::new("Toucher un clip :").color(TEXT_DIM).size(12.5));
            ui.radio_value(&mut config.toucher_partage, true, "le partager");
            ui.radio_value(&mut config.toucher_partage, false, "le lire");
        });
        ui::precision(ui, "Glisser le doigt sur la bande de gauche règle le volume général.");
        ui.add_space(8.0);
        if ui::button(ui, Icon::Refresh, "Tout remettre d'origine").clicked() {
            *config = Config::default();
            etat.onglet = 0;
            etat.ecran = 0;
            etat.selection = None;
        }
    }
}

/// Ce que la touche contient : vide, un bouton, une place du salon, un
/// chiffre VALORANT, un clip récent — et son numéro, s'il en a un.
fn contenu_ui(ui: &mut egui::Ui, case: &mut Case, t: usize) {
    let nom = |c: &Case| -> String {
        match c {
            Case::Vide => "Vide".into(),
            Case::Bouton(_) => "Un bouton".into(),
            Case::Vocal(_) => "Une place du salon vocal".into(),
            Case::Valo(w) => format!("VALORANT · {}", w.nom()),
            Case::Clip(_) => "Un clip récent".into(),
        }
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new("Contenu").color(TEXT_DIM).size(12.5));
        egui::ComboBox::from_id_salt("loupedeck_contenu")
            .width(280.0)
            .selected_text(RichText::new(nom(case)).color(TEXT))
            .show_ui(ui, |ui| {
                let mut choix = vec![
                    Case::Vide,
                    Case::Bouton(Bouton::nu(Action::Micro)),
                    Case::Vocal(t as u8),
                ];
                choix.extend(Widget::TOUS.map(Case::Valo));
                choix.push(Case::Clip(0));
                for c in choix {
                    let meme = std::mem::discriminant(&c) == std::mem::discriminant(case)
                        && (!matches!(c, Case::Valo(_)) || c == *case);
                    if ui.selectable_label(meme, nom(&c)).clicked() && !meme {
                        *case = c;
                    }
                }
            });
        match case {
            Case::Vocal(n) => {
                let mut k = *n as u32 + 1;
                if ui.add(egui::DragValue::new(&mut k).range(1..=12).prefix("place n° ")).changed() {
                    *n = (k - 1) as u8;
                }
            }
            Case::Clip(n) => {
                let mut k = *n as u32 + 1;
                if ui.add(egui::DragValue::new(&mut k).range(1..=12).prefix("n° ")).changed() {
                    *n = (k - 1) as u8;
                }
            }
            _ => {}
        }
    });
    let aide = match case {
        Case::Vide => "La touche reste noire et ne fait rien.",
        Case::Bouton(_) => "Une action d'un toucher, avec l'icône, le texte et la couleur que tu veux.",
        Case::Vocal(_) => {
            "En vocal : la personne à cette place (toi d'abord) — un toucher la choisit pour la molette \
             de volume. Hors vocal : le salon vocal à cette place, où un toucher te fait entrer."
        }
        Case::Valo(Widget::Partie) => "En direct pendant la partie : le score, la carte. Toucher : suivre ta partie.",
        Case::Valo(_) => "Mis à jour après chaque match. Toucher : ta fiche, ou les stats du groupe.",
        Case::Clip(_) => "Le n° 1 est ton dernier clip. Toucher : le partager ou le lire, selon le réglage plus bas.",
    };
    ui::precision(ui, aide);
}
