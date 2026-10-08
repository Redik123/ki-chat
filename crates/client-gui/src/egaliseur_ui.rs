//! L'éditeur d'égaliseur de la page Casque, comme en studio : la courbe
//! qu'on attrape à la souris, posée sur le spectre vivant de la voix.
//!
//! Glisser un point règle sa fréquence et son gain, la molette sa largeur ;
//! double-clic sur la courbe pour ajouter une bande, clic droit pour changer
//! sa forme ou la retirer. Sous le graphe, la bande choisie se règle au
//! chiffre près. Et « Comparer » coupe l'égaliseur le temps d'entendre la
//! différence — ce qui manquait pour savoir ce qu'on change.

use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2};
use ki_voice::egaliseur::{self as eq, Bande, Forme, BANDES_MAX, FREQ_MAX, FREQ_MIN, GAIN_MAX_DB, Q_MAX, Q_MIN};
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

use crate::icons::Icon;
use crate::theme::{self, ACCENT, INFO, TEXT, TEXT_DIM, TEXT_FAINT, WARN};
use crate::ui;

/// La fenêtre du spectre : 4 096 échantillons (85 ms), 11,7 Hz par case.
pub(crate) const FENETRE: usize = 4096;
/// Les points du spectre dessiné, espacés en fréquence logarithmique.
const POINTS: usize = 128;
/// La descente du spectre affiché quand le son retombe, en dB par seconde.
const RETOMBEE_DB_S: f32 = 36.0;
/// Le bas et le haut de l'échelle du spectre, en dBFS.
const SPECTRE_BAS: f32 = -84.0;
const SPECTRE_HAUT: f32 = -6.0;
/// La hauteur du graphe.
const HAUTEUR: f32 = 240.0;
/// Une couleur par bande.
const COULEURS: [Color32; BANDES_MAX] = [
    ACCENT,
    INFO,
    WARN,
    Color32::from_rgb(0xb3, 0x8b, 0xff),
    Color32::from_rgb(0xff, 0x7a, 0xb6),
    Color32::from_rgb(0xf5, 0xd0, 0x4a),
    Color32::from_rgb(0x4a, 0xd8, 0xe0),
    Color32::from_rgb(0xff, 0x8a, 0x6b),
];

/// Le spectre : une FFT sur la fenêtre la plus récente, ramenée sur des
/// points logarithmiques (la crête de chaque tranche), lissée à la descente
/// comme un analyseur de studio.
pub(crate) struct Analyseur {
    fft: Arc<dyn RealToComplex<f32>>,
    fenetre: Vec<f32>,
    entree: Vec<f32>,
    sortie: Vec<Complex<f32>>,
    /// Les tranches de chaque point : premières et dernières cases de la FFT.
    tranches: Vec<(usize, usize)>,
    frequences: Vec<f32>,
    /// Le spectre affiché, en dBFS.
    affiche: Vec<f32>,
}

impl Analyseur {
    pub(crate) fn new() -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FENETRE);
        let sortie = fft.make_output_vec();
        let fenetre = (0..FENETRE)
            .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / FENETRE as f32).cos())
            .collect();
        let frequences: Vec<f32> =
            (0..POINTS).map(|i| FREQ_MIN * (FREQ_MAX / FREQ_MIN).powf(i as f32 / (POINTS - 1) as f32)).collect();
        let case = ki_voice::SAMPLE_RATE as f32 / FENETRE as f32;
        let demi = (FREQ_MAX / FREQ_MIN).powf(0.5 / (POINTS - 1) as f32);
        let tranches = frequences
            .iter()
            .map(|&f| {
                let bas = ((f / demi) / case).floor() as usize;
                let haut = ((f * demi) / case).ceil() as usize;
                (bas.max(1), haut.clamp(bas.max(1), FENETRE / 2))
            })
            .collect();
        Self {
            fft,
            fenetre,
            entree: vec![0.0; FENETRE],
            sortie,
            tranches,
            frequences,
            affiche: vec![SPECTRE_BAS; POINTS],
        }
    }

    /// Nouvelle mesure sur les échantillons les plus récents ; faute d'une
    /// fenêtre entière (silence, moteur arrêté), le spectre retombe.
    pub(crate) fn nourrir(&mut self, echantillons: &[f32], dt: f32) {
        let retombee = RETOMBEE_DB_S * dt.clamp(0.0, 0.2);
        if echantillons.len() < FENETRE {
            for a in self.affiche.iter_mut() {
                *a = (*a - retombee).max(SPECTRE_BAS);
            }
            return;
        }
        let recents = &echantillons[echantillons.len() - FENETRE..];
        for ((e, &x), &w) in self.entree.iter_mut().zip(recents).zip(&self.fenetre) {
            *e = x * w;
        }
        if self.fft.process(&mut self.entree, &mut self.sortie).is_err() {
            return;
        }
        // Un sinus pleine échelle sort, fenêtre de Hann comprise, à N/4 dans
        // sa case : c'est la référence du 0 dBFS.
        let reference = (FENETRE as f32 / 4.0).powi(2);
        for (i, &(bas, haut)) in self.tranches.iter().enumerate() {
            let crete = self.sortie[bas..=haut].iter().fold(0f32, |m, c| m.max(c.norm_sqr()));
            let db = 10.0 * (crete / reference + 1e-12).log10();
            let a = &mut self.affiche[i];
            *a = if db > *a { db } else { (*a - retombee).max(db) }.max(SPECTRE_BAS);
        }
    }
}

/// L'état de l'éditeur, gardé d'une image à l'autre.
pub(crate) struct EditeurEq {
    /// La bande choisie (ses réglages s'affichent sous le graphe).
    pub(crate) selection: Option<usize>,
    /// La bande qu'on glisse en ce moment.
    glisse: Option<usize>,
    /// L'égaliseur est coupé le temps de comparer.
    pub(crate) comparer: bool,
    pub(crate) analyseur: Analyseur,
}

impl EditeurEq {
    pub(crate) fn new() -> Self {
        Self { selection: None, glisse: None, comparer: false, analyseur: Analyseur::new() }
    }
}

/// Les axes du graphe : fréquence logarithmique, gain linéaire en dB.
struct Axes {
    rect: Rect,
}

impl Axes {
    fn x(&self, f: f32) -> f32 {
        let t = (f / FREQ_MIN).ln() / (FREQ_MAX / FREQ_MIN).ln();
        self.rect.left() + t.clamp(0.0, 1.0) * self.rect.width()
    }
    fn f(&self, x: f32) -> f32 {
        let t = ((x - self.rect.left()) / self.rect.width()).clamp(0.0, 1.0);
        FREQ_MIN * (FREQ_MAX / FREQ_MIN).powf(t)
    }
    fn demi_hauteur(&self) -> f32 {
        self.rect.height() / 2.0 - 14.0
    }
    fn y(&self, db: f32) -> f32 {
        self.rect.center().y - db.clamp(-GAIN_MAX_DB, GAIN_MAX_DB) / GAIN_MAX_DB * self.demi_hauteur()
    }
    fn db(&self, y: f32) -> f32 {
        ((self.rect.center().y - y) / self.demi_hauteur() * GAIN_MAX_DB).clamp(-GAIN_MAX_DB, GAIN_MAX_DB)
    }
    fn y_spectre(&self, db: f32) -> f32 {
        let t = ((db - SPECTRE_BAS) / (SPECTRE_HAUT - SPECTRE_BAS)).clamp(0.0, 1.0);
        self.rect.bottom() - t * (self.rect.height() - 4.0)
    }
}

/// « 80 Hz », « 1,2 kHz ».
pub(crate) fn frequence_texte(f: f32) -> String {
    if f >= 1000.0 {
        let k = f / 1000.0;
        if k >= 10.0 || (k - k.round()).abs() < 0.05 {
            format!("{:.0} kHz", k)
        } else {
            format!("{:.1} kHz", k).replace('.', ",")
        }
    } else {
        format!("{f:.0} Hz")
    }
}

/// Le point d'une bande sur le graphe : à son gain, ou sur la ligne du zéro
/// pour les coupes, qui n'en ont pas.
fn point(axes: &Axes, b: &Bande) -> Pos2 {
    let db = if b.forme.a_un_gain() { b.gain_db } else { 0.0 };
    Pos2::new(axes.x(b.frequence), axes.y(db))
}

/// L'éditeur : préréglages, graphe, réglages de la bande choisie. Rend vrai
/// quand les bandes ont changé (ou que la comparaison a basculé).
pub(crate) fn editeur(
    ui: &mut egui::Ui,
    bandes: &mut Vec<Bande>,
    etat: &mut EditeurEq,
    prereglages: &[eq::Prereglage],
) -> bool {
    let mut change = false;
    if etat.selection.is_some_and(|i| i >= bandes.len()) {
        etat.selection = None;
    }

    // --- Préréglages, comparaison, remise à plat ---
    ui.horizontal_wrapped(|ui| {
        let actuel = prereglages
            .iter()
            .find(|(_, fabrique)| fabrique() == *bandes)
            .map(|(nom, _)| *nom)
            .unwrap_or("Personnalisé");
        egui::ComboBox::from_id_salt(ui.id().with("prereglage"))
            .width(150.0)
            .selected_text(RichText::new(actuel).color(TEXT))
            .show_ui(ui, |ui| {
                for (nom, fabrique) in prereglages {
                    if ui.selectable_label(*nom == actuel, *nom).clicked() {
                        *bandes = fabrique();
                        etat.selection = None;
                        change = true;
                    }
                }
            });
        if ui::interrupteur(ui, &mut etat.comparer, "Comparer sans égaliseur").changed() {
            change = true;
        }
        if !bandes.is_empty() && ui::button(ui, Icon::Refresh, "À plat").clicked() {
            bandes.clear();
            etat.selection = None;
            change = true;
        }
    });
    ui.add_space(8.0);

    // --- Le graphe ---
    let largeur = ui.available_width().max(260.0);
    let (rect, reponse) = ui.allocate_exact_size(Vec2::new(largeur, HAUTEUR), Sense::click_and_drag());
    let axes = Axes { rect: rect.shrink2(Vec2::new(10.0, 0.0)) };
    let pointeur = reponse.hover_pos().or_else(|| reponse.interact_pointer_pos());
    let proche = |p: Pos2, bandes: &[Bande]| -> Option<usize> {
        bandes
            .iter()
            .enumerate()
            .map(|(i, b)| (i, point(&axes, b).distance(p)))
            .filter(|(_, d)| *d < 14.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };

    if reponse.drag_started() {
        etat.glisse = pointeur.and_then(|p| proche(p, bandes));
        if etat.glisse.is_some() {
            etat.selection = etat.glisse;
        }
    }
    if reponse.dragged() {
        if let (Some(i), Some(p)) = (etat.glisse, reponse.interact_pointer_pos()) {
            let b = &mut bandes[i];
            b.frequence = axes.f(p.x);
            if b.forme.a_un_gain() {
                b.gain_db = (axes.db(p.y) * 10.0).round() / 10.0;
            }
            *b = b.bornee();
            change = true;
        }
    }
    if reponse.drag_stopped() {
        etat.glisse = None;
    }
    if reponse.clicked() {
        etat.selection = pointeur.and_then(|p| proche(p, bandes));
    }
    if reponse.secondary_clicked() {
        if let Some(i) = pointeur.and_then(|p| proche(p, bandes)) {
            etat.selection = Some(i);
        }
    }
    if reponse.double_clicked() {
        match pointeur.and_then(|p| proche(p, bandes)) {
            // Sur un point : son gain revient à zéro.
            Some(i) => {
                bandes[i].gain_db = 0.0;
                change = true;
            }
            // Ailleurs : une cloche naît là où l'on a cliqué.
            None if bandes.len() < BANDES_MAX => {
                if let Some(p) = pointeur {
                    bandes.push(Bande::new(Forme::Cloche, axes.f(p.x), axes.db(p.y), 1.0));
                    etat.selection = Some(bandes.len() - 1);
                    change = true;
                }
            }
            None => {}
        }
    }
    // La molette règle la largeur de la bande choisie — et ne fait plus
    // défiler la page tant qu'on est sur le graphe.
    if reponse.hovered() {
        if let Some(i) = etat.selection {
            let defilement = ui.input(|inp| inp.smooth_scroll_delta.y);
            if defilement != 0.0 {
                let b = &mut bandes[i];
                b.q = (b.q * 2f32.powf(defilement / 240.0)).clamp(Q_MIN, Q_MAX);
                change = true;
                ui.input_mut(|inp| inp.smooth_scroll_delta = Vec2::ZERO);
            }
        }
        if let Some(i) = etat.selection {
            if ui.input(|inp| inp.key_pressed(egui::Key::Delete)) {
                bandes.remove(i);
                etat.selection = None;
                change = true;
            }
        }
    }
    reponse.context_menu(|ui| {
        let Some(i) = etat.selection.filter(|&i| i < bandes.len()) else {
            ui.label(RichText::new("Clic droit sur un point").color(TEXT_DIM));
            return;
        };
        for forme in Forme::TOUTES {
            if ui.selectable_label(bandes[i].forme == forme, forme.nom()).clicked() {
                bandes[i].forme = forme;
                change = true;
                ui.close();
            }
        }
        ui.separator();
        if bandes[i].forme.a_une_pente() && ui.checkbox(&mut bandes[i].raide, "24 dB par octave").changed() {
            change = true;
        }
        if ui.checkbox(&mut bandes[i].active, "Active").changed() {
            change = true;
        }
        if ui.button("Retirer la bande").clicked() {
            bandes.remove(i);
            etat.selection = None;
            change = true;
            ui.close();
        }
    });

    if ui.is_rect_visible(rect) {
        dessiner(ui, &axes, rect, bandes, etat);
    }
    ui.add_space(8.0);

    // --- La bande choisie, au chiffre près ---
    match etat.selection.filter(|&i| i < bandes.len()) {
        Some(i) => {
            let (modifiee, retirer) = reglages_bande(ui, i, &mut bandes[i], &mut etat.selection);
            change |= modifiee;
            if retirer {
                bandes.remove(i);
                etat.selection = None;
                change = true;
            }
        }
        None => {
            ui.horizontal_wrapped(|ui| {
                if bandes.len() < BANDES_MAX && ui::button(ui, Icon::Plus, "Ajouter une bande").clicked() {
                    bandes.push(Bande::new(Forme::Cloche, 1_000.0, 0.0, 1.0));
                    etat.selection = Some(bandes.len() - 1);
                    change = true;
                }
                ui.label(
                    RichText::new(
                        "Glisse les points · molette : largeur · double-clic : ajouter une bande · \
                         clic droit : forme, retirer",
                    )
                    .color(TEXT_FAINT)
                    .size(11.5),
                );
            });
        }
    }
    change
}

/// La ligne de réglages de la bande choisie. Rend (modifiée, à retirer).
fn reglages_bande(ui: &mut egui::Ui, i: usize, b: &mut Bande, selection: &mut Option<usize>) -> (bool, bool) {
    let mut change = false;
    let mut retirer = false;
    ui.horizontal_wrapped(|ui| {
        let (pastille, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
        ui.painter().circle_filled(pastille.center(), 6.0, COULEURS[i % BANDES_MAX]);
        egui::ComboBox::from_id_salt(ui.id().with(("forme", i)))
            .width(120.0)
            .selected_text(RichText::new(b.forme.nom()).color(TEXT))
            .show_ui(ui, |ui| {
                for forme in Forme::TOUTES {
                    if ui.selectable_label(b.forme == forme, forme.nom()).clicked() {
                        b.forme = forme;
                        change = true;
                    }
                }
            });
        ui.label(RichText::new("fréquence").color(TEXT_DIM).size(12.0));
        let vitesse = b.frequence as f64 * 0.005;
        change |= ui
            .add(
                egui::DragValue::new(&mut b.frequence)
                    .speed(vitesse)
                    .range(FREQ_MIN..=FREQ_MAX)
                    .custom_formatter(|v, _| frequence_texte(v as f32))
                    .custom_parser(|t| {
                        let t = t.trim().to_lowercase().replace(',', ".");
                        match t.strip_suffix("khz").or_else(|| t.strip_suffix('k')) {
                            Some(k) => k.trim().parse::<f64>().ok().map(|k| k * 1000.0),
                            None => t.trim_end_matches("hz").trim().parse().ok(),
                        }
                    }),
            )
            .changed();
        if b.forme.a_un_gain() {
            ui.label(RichText::new("gain").color(TEXT_DIM).size(12.0));
            change |= ui
                .add(
                    egui::DragValue::new(&mut b.gain_db)
                        .speed(0.1)
                        .range(-GAIN_MAX_DB..=GAIN_MAX_DB)
                        .fixed_decimals(1)
                        .suffix(" dB"),
                )
                .changed();
        }
        if !(b.forme.a_une_pente() && b.raide) {
            ui.label(RichText::new("largeur (Q)").color(TEXT_DIM).size(12.0));
            change |= ui
                .add(egui::DragValue::new(&mut b.q).speed(0.01).range(Q_MIN..=Q_MAX).fixed_decimals(2))
                .changed();
        }
        if b.forme.a_une_pente() {
            change |= ui.checkbox(&mut b.raide, "24 dB/oct").changed();
        }
        change |= ui.checkbox(&mut b.active, "active").changed();
        if ui::icon_button(ui, Icon::Trash, "Retirer la bande").clicked() {
            retirer = true;
        }
        if ui::icon_button(ui, Icon::Close, "Désélectionner").clicked() {
            *selection = None;
        }
    });
    *b = b.bornee();
    (change, retirer)
}

/// Le graphe : grille, spectre, courbe, points.
fn dessiner(ui: &egui::Ui, axes: &Axes, rect: Rect, bandes: &[Bande], etat: &EditeurEq) {
    let p = ui.painter_at(rect);
    p.rect_filled(rect, CornerRadius::same(10), theme::BG_DEEP);
    let grille = theme::alpha(Color32::WHITE, 14);
    let police = egui::FontId::proportional(10.0);
    for f in [50.0, 100.0, 200.0, 500.0, 1_000.0, 2_000.0, 5_000.0, 10_000.0] {
        let x = axes.x(f);
        p.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(1.0_f32, grille));
        p.text(Pos2::new(x + 3.0, rect.bottom() - 3.0), egui::Align2::LEFT_BOTTOM, frequence_texte(f), police.clone(), TEXT_FAINT);
    }
    for db in [-12.0f32, -6.0, 6.0, 12.0] {
        let y = axes.y(db);
        p.line_segment([Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)], Stroke::new(1.0_f32, grille));
        p.text(Pos2::new(rect.left() + 4.0, y - 1.0), egui::Align2::LEFT_BOTTOM, format!("{db:+.0}"), police.clone(), TEXT_FAINT);
    }
    let zero = axes.y(0.0);
    p.line_segment(
        [Pos2::new(rect.left(), zero), Pos2::new(rect.right(), zero)],
        Stroke::new(1.0_f32, theme::alpha(Color32::WHITE, 40)),
    );

    // Le spectre de la voix, en fond.
    let a = &etat.analyseur;
    let haut: Vec<Pos2> = a
        .frequences
        .iter()
        .zip(&a.affiche)
        .map(|(&f, &db)| Pos2::new(axes.x(f), axes.y_spectre(db)))
        .collect();
    let mut maillage = egui::Mesh::default();
    let fond = theme::alpha(INFO, 34);
    for (i, &h) in haut.iter().enumerate() {
        maillage.colored_vertex(h, fond);
        maillage.colored_vertex(Pos2::new(h.x, rect.bottom()), theme::alpha(INFO, 6));
        if i > 0 {
            let k = (2 * i) as u32;
            maillage.add_triangle(k - 2, k - 1, k);
            maillage.add_triangle(k - 1, k + 1, k);
        }
    }
    p.add(Shape::mesh(maillage));
    p.add(Shape::line(haut, Stroke::new(1.0_f32, theme::alpha(INFO, 110))));

    // La courbe de l'égaliseur, et l'aire entre elle et le zéro.
    let n = 220;
    let courbe: Vec<Pos2> = (0..=n)
        .map(|i| {
            let x = axes.rect.left() + axes.rect.width() * i as f32 / n as f32;
            Pos2::new(x, axes.y(eq::reponse_db(bandes, axes.f(x))))
        })
        .collect();
    let couleur_courbe = if etat.comparer { theme::alpha(ACCENT, 70) } else { ACCENT };
    let mut aire = egui::Mesh::default();
    for (i, &c) in courbe.iter().enumerate() {
        aire.colored_vertex(c, theme::alpha(ACCENT, if etat.comparer { 8 } else { 30 }));
        aire.colored_vertex(Pos2::new(c.x, zero), theme::alpha(ACCENT, 4));
        if i > 0 {
            let k = (2 * i) as u32;
            aire.add_triangle(k - 2, k - 1, k);
            aire.add_triangle(k - 1, k + 1, k);
        }
    }
    p.add(Shape::mesh(aire));
    p.add(Shape::line(courbe, Stroke::new(2.0_f32, couleur_courbe)));

    // La bande choisie : sa propre courbe, fine.
    if let Some(i) = etat.selection.filter(|&i| i < bandes.len()) {
        let seule: Vec<Pos2> = (0..=n)
            .map(|k| {
                let x = axes.rect.left() + axes.rect.width() * k as f32 / n as f32;
                Pos2::new(x, axes.y(bandes[i].reponse_db(axes.f(x))))
            })
            .collect();
        p.add(Shape::line(seule, Stroke::new(1.0_f32, theme::alpha(COULEURS[i % BANDES_MAX], 150))));
    }

    // Les points.
    for (i, b) in bandes.iter().enumerate() {
        let c = point(axes, b);
        let couleur = COULEURS[i % BANDES_MAX];
        let choisi = etat.selection == Some(i);
        let r = if choisi { 9.0 } else { 7.0 };
        if b.active {
            p.circle_filled(c, r, couleur);
        } else {
            p.circle_stroke(c, r, Stroke::new(1.5_f32, couleur));
        }
        if choisi {
            p.circle_stroke(c, r + 3.0, Stroke::new(1.5_f32, Color32::WHITE));
        }
        p.text(
            c,
            egui::Align2::CENTER_CENTER,
            format!("{}", i + 1),
            egui::FontId::proportional(9.5),
            if b.active { theme::BG_DEEP } else { couleur },
        );
    }
    if etat.comparer {
        p.text(
            rect.center_top() + Vec2::new(0.0, 8.0),
            egui::Align2::CENTER_TOP,
            "comparaison : égaliseur coupé",
            egui::FontId::proportional(11.5),
            WARN,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_axes_font_l_aller_retour() {
        let axes = Axes { rect: Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(600.0, 240.0)) };
        for f in [20.0f32, 80.0, 1_000.0, 12_345.0, 20_000.0] {
            assert!((axes.f(axes.x(f)) - f).abs() / f < 1e-3, "{f}");
        }
        for db in [-18.0f32, -3.5, 0.0, 6.0, 18.0] {
            assert!((axes.db(axes.y(db)) - db).abs() < 1e-3, "{db}");
        }
    }

    /// Un sinus pleine échelle se lit à 0 dBFS, là où il est.
    #[test]
    fn le_spectre_lit_un_sinus_au_bon_endroit() {
        let mut a = Analyseur::new();
        let f = 1_000.0;
        let s: Vec<f32> = (0..FENETRE)
            .map(|i| (2.0 * std::f32::consts::PI * f * i as f32 / ki_voice::SAMPLE_RATE as f32).sin())
            .collect();
        a.nourrir(&s, 0.03);
        let (i, &db) = a
            .affiche
            .iter()
            .enumerate()
            .max_by(|x, y| x.1.total_cmp(y.1))
            .unwrap();
        assert!((a.frequences[i] / f - 1.0).abs() < 0.06, "crête à {} Hz", a.frequences[i]);
        assert!(db.abs() < 1.0, "{db} dBFS");
        // Loin du sinus, presque rien.
        let loin = a.frequences.iter().position(|&x| x > 8_000.0).unwrap();
        assert!(a.affiche[loin] < -50.0);
    }

    #[test]
    fn les_frequences_se_lisent_en_francais() {
        assert_eq!(frequence_texte(80.0), "80 Hz");
        assert_eq!(frequence_texte(1_000.0), "1 kHz");
        assert_eq!(frequence_texte(1_300.0), "1,3 kHz");
        assert_eq!(frequence_texte(12_000.0), "12 kHz");
    }
}
