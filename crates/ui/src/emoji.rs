//! Les emoji en couleur.
//!
//! egui dessine le texte d'une seule couleur : ses emoji sont des contours,
//! teintés comme le texte. ki-ui les peint en couleur avec la police emoji
//! du système — sous Windows, Segoe UI Emoji, en aplats (ses calques COLR
//! v0, ceux d'avant les dégradés) :
//!
//! 1. la police est déclarée à egui dans une famille à part, [`FAMILLE`],
//!    que seuls les emoji emploient : egui les façonne avec elle — drapeaux,
//!    teintes de peau, séquences assemblés —, à la bonne largeur, curseur et
//!    sélection compris ;
//! 2. [`colorer`] met chaque emoji d'un texte dans cette famille, en
//!    transparent ;
//! 3. [`peindre`] pose par-dessus l'image couleur de chacun, peinte une fois
//!    par vello_cpu (le peintre des glyphes d'egui) et gardée en texture.
//!
//! [`label`] fait les trois pour un texte, [`image`] donne l'image d'un emoji
//! seul (un bouton de réaction), [`mise_en_page`] sert un champ de saisie.
//! Sans police emoji couleur sur la machine, rien ne change : les emoji
//! restent ceux d'egui.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use egui::text::{ByteIndex, LayoutJob, LayoutSection};
use egui::{
    Color32, ColorImage, Context, FontFamily, FontSelection, Galley, Painter, Pos2, Rect, Response,
    TextureHandle, TextureOptions, Ui, Vec2, WidgetText,
};
use skrifa::color::{Brush, ColorGlyphFormat, ColorPainter, CompositeMode, Transform};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::raw::types::BoundingBox;
use skrifa::raw::TableProvider;
use skrifa::{FontRef, GlyphId, MetadataProvider};
use unicode_properties::{EmojiStatus, UnicodeEmoji};
use unicode_segmentation::UnicodeSegmentation;
use vello_cpu::{color, kurbo};

/// La famille de polices des emoji couleur : la police emoji du système,
/// puis celles du texte, pour ce qu'elle ne connaîtrait pas.
pub const FAMILLE: &str = "ki-emoji";

/// La résolution des images, en pixels par em : de quoi rester net jusqu'à
/// un emoji de 48 points sur un écran à 150 %.
const RESOLUTION: f64 = 96.0;

/// Le dessin d'un emoji de Segoe UI Emoji fait un em de haut dans une case
/// de 1,37 em de large : à côté du texte, il paraît petit et flotte. Il est
/// grossi autour de son centre, et tient toujours dans sa case (😂, le plus
/// large, y prend 2688 unités sur 2812).
const GROSSISSEMENT: f32 = 1.2;

fn famille() -> FontFamily {
    FontFamily::Name(FAMILLE.into())
}

// ---------------------------------------------------------------------
// La police
// ---------------------------------------------------------------------

/// Les octets de la police emoji du système, lus une fois pour toute la
/// vie du programme (egui les garde aussi : ils ne sont pas copiés).
pub(crate) fn police() -> Option<&'static [u8]> {
    static POLICE: OnceLock<Option<&'static [u8]>> = OnceLock::new();
    *POLICE.get_or_init(|| {
        let octets = std::fs::read(chemin_police()?).ok()?;
        // Seulement une police dont on sait peindre les emoji : des calques
        // de couleur (COLR v0).
        let police = FontRef::new(&octets).ok()?;
        if police.colr().ok()?.num_base_glyph_records() == 0 {
            return None;
        }
        Some(&*Box::leak(octets.into_boxed_slice()))
    })
}

fn chemin_police() -> Option<std::path::PathBuf> {
    if cfg!(windows) {
        let windows = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
        let chemin = std::path::Path::new(&windows).join("Fonts").join("seguiemj.ttf");
        chemin.exists().then_some(chemin)
    } else {
        // macOS et Linux rangent leurs emoji en images (sbix, CBDT) : pas
        // encore lues. Leurs emoji restent ceux d'egui.
        None
    }
}

/// Déclare la police emoji à egui, si la machine en a une. Appelé par
/// [`crate::style::installer`].
pub(crate) fn installer(polices: &mut egui::FontDefinitions) {
    let Some(octets) = police() else { return };
    polices.font_data.insert(FAMILLE.to_owned(), Arc::new(egui::FontData::from_static(octets)));
    let mut membres = vec![FAMILLE.to_owned()];
    membres.extend(polices.families.get(&FontFamily::Proportional).cloned().unwrap_or_default());
    polices.families.insert(famille(), membres);
}

// ---------------------------------------------------------------------
// Ce qui s'affiche en emoji
// ---------------------------------------------------------------------

/// Cette grappe (un caractère perçu) s'affiche-t-elle en emoji, d'après
/// Unicode ? Un sélecteur de présentation l'emporte ; une séquence (teinte
/// de peau, drapeau, assemblage, touche) en est une ; un caractère seul
/// l'est s'il se présente en emoji par défaut — 😂 oui, ❤ ou ↩ non : ils
/// restent du texte sans leur sélecteur.
pub fn est_emoji(grappe: &str) -> bool {
    let Some(premier) = grappe.chars().next() else { return false };
    if grappe.contains('\u{FE0E}') {
        return false;
    }
    if grappe.contains('\u{FE0F}') {
        return premier.is_emoji_char();
    }
    if grappe.chars().nth(1).is_some() {
        let sequence = grappe.chars().any(|c| {
            c == '\u{200D}'
                || c == '\u{20E3}'
                || ('\u{1F3FB}'..='\u{1F3FF}').contains(&c)
                || ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
                || ('\u{E0020}'..='\u{E007F}').contains(&c)
        });
        return sequence && premier.is_emoji_char();
    }
    matches!(
        premier.emoji_status(),
        EmojiStatus::EmojiPresentation
            | EmojiStatus::EmojiPresentationAndModifierBase
            | EmojiStatus::EmojiPresentationAndEmojiComponent
            | EmojiStatus::EmojiPresentationAndModifierAndEmojiComponent
    )
}

/// Le texte peut-il contenir un emoji ? Un tri rapide, avant de découper en
/// grappes : du français ordinaire n'en a pas.
fn peut_contenir(texte: &str) -> bool {
    texte.chars().any(|c| (c.is_emoji_char() && !c.is_ascii()) || c == '\u{20E3}')
}

/// Le glyphe couleur d'une grappe dans la police emoji : la grappe façonnée
/// comme egui le fera, et un seul glyphe à calques de couleur au bout.
/// Gardé en mémoire : une grappe se façonne une fois.
fn glyphe(grappe: &str) -> Option<GlyphId> {
    static GLYPHES: OnceLock<Mutex<HashMap<String, Option<GlyphId>>>> = OnceLock::new();
    let glyphes = GLYPHES.get_or_init(Default::default);
    if let Some(connu) = glyphes.lock().ok()?.get(grappe) {
        return *connu;
    }
    let trouve = faconner(grappe);
    glyphes.lock().ok()?.insert(grappe.to_owned(), trouve);
    trouve
}

fn faconner(grappe: &str) -> Option<GlyphId> {
    static FACONNAGE: OnceLock<Option<harfrust::ShaperData>> = OnceLock::new();
    let police = FontRef::new(police()?).ok()?;
    let donnees = FACONNAGE.get_or_init(|| Some(harfrust::ShaperData::new(&police))).as_ref()?;
    let faconneur = donnees.shaper(&police).instance(None).build();
    let mut tampon = harfrust::UnicodeBuffer::new();
    tampon.push_str(grappe);
    tampon.guess_segment_properties();
    let sortie = faconneur.shape(tampon, harfrust::ShapeOptions::new());
    let couleurs = police.color_glyphs();
    let mut trouve = None;
    for info in sortie.glyph_infos() {
        let id = GlyphId::new(info.glyph_id);
        if id == GlyphId::NOTDEF {
            return None;
        }
        if couleurs.get_with_format(id, ColorGlyphFormat::ColrV0).is_some() {
            // Deux emoji pour une grappe : une séquence que la police ne
            // connaît pas. egui la dessinera à sa façon.
            if trouve.is_some() {
                return None;
            }
            trouve = Some(id);
        }
    }
    trouve
}

// ---------------------------------------------------------------------
// Le texte
// ---------------------------------------------------------------------

/// Met chaque emoji d'un texte dans la famille [`FAMILLE`], en transparent :
/// egui le façonne et lui réserve sa place, [`peindre`] y posera l'image.
/// Rend vrai s'il y en avait. Sans police emoji, ne touche à rien.
pub fn colorer(job: &mut LayoutJob) -> bool {
    if police().is_none() || !peut_contenir(&job.text) {
        return false;
    }
    let mut sections = Vec::with_capacity(job.sections.len());
    let mut change = false;
    for section in std::mem::take(&mut job.sections) {
        let (debut, fin) = (section.byte_range.start.0, section.byte_range.end.0);
        let texte = &job.text[debut..fin];
        let mut morceau = debut;
        let mut premier = true;
        let mut pousser = |sections: &mut Vec<LayoutSection>, de: usize, a: usize, emoji: bool| {
            if de == a {
                return;
            }
            let mut format = section.format.clone();
            if emoji {
                format.font_id.family = famille();
                format.color = Color32::TRANSPARENT;
            }
            sections.push(LayoutSection {
                leading_space: if std::mem::take(&mut premier) { section.leading_space } else { 0.0 },
                byte_range: ByteIndex(de)..ByteIndex(a),
                format,
            });
        };
        for (i, grappe) in texte.grapheme_indices(true) {
            if est_emoji(grappe) && glyphe(grappe).is_some() {
                let (de, a) = (debut + i, debut + i + grappe.len());
                pousser(&mut sections, morceau, de, false);
                pousser(&mut sections, de, a, true);
                morceau = a;
                change = true;
            }
        }
        pousser(&mut sections, morceau, fin, false);
    }
    job.sections = sections;
    change
}

/// Pose l'image couleur de chaque emoji de `galley` (mis en page après
/// [`colorer`]), peint à `galley_pos`.
pub fn peindre(painter: &Painter, galley_pos: Pos2, galley: &Galley) {
    let job = &galley.job;
    let famille = famille();
    // Où est chaque emoji, en caractères — son premier, et combien il en
    // compte : les glyphes d'une galley suivent les caractères un à un (egui
    // en ajoute de largeur nulle pour la suite d'une séquence).
    let mut emoji: Vec<(usize, usize, GlyphId)> = Vec::new();
    let mut octets = job.text.char_indices().map(|(o, _)| o).enumerate().peekable();
    for section in &job.sections {
        if section.format.font_id.family != famille {
            continue;
        }
        let (debut, fin) = (section.byte_range.start.0, section.byte_range.end.0);
        let grappe = &job.text[debut..fin];
        let Some(id) = glyphe(grappe) else { continue };
        while let Some(&(n, o)) = octets.peek() {
            if o >= debut {
                if o == debut {
                    emoji.push((n, grappe.chars().count(), id));
                }
                break;
            }
            octets.next();
        }
    }
    if emoji.is_empty() {
        return;
    }
    let dans_un_emoji = |c: usize| {
        let i = emoji.partition_point(|&(n, _, _)| n <= c);
        i > 0 && c < emoji[i - 1].0 + emoji[i - 1].1
    };
    let cache = cache(painter.ctx());
    let mut caractere = 0;
    for rangee in &galley.rows {
        let glyphes = &rangee.row.glyphs;
        // La ligne de base du texte de la rangée : celle de la police emoji
        // est un peu plus haute (egui aligne les morceaux par le bas). Sans
        // texte sur la rangée, celle de l'emoji.
        let texte = glyphes
            .iter()
            .enumerate()
            .find(|(i, g)| g.advance_width > 0.0 && !dans_un_emoji(caractere + i))
            .map(|(_, g)| g.pos.y);
        for (i, g) in glyphes.iter().enumerate() {
            let Ok(k) = emoji.binary_search_by_key(&(caractere + i), |&(n, _, _)| n) else { continue };
            let Some(rendu) = rendu(&cache, painter.ctx(), emoji[k].2) else { continue };
            if rendu.avance <= 0.0 || g.advance_width <= 0.0 {
                continue;
            }
            // Des unités de la police aux points : la largeur que egui a donnée
            // au glyphe, rapportée à son avance. Le dessin est centré dans sa
            // case, grossi autour de son propre centre.
            let echelle = g.advance_width / rendu.avance;
            let origine = galley_pos + rangee.pos.to_vec2();
            let centre = Pos2::new(
                origine.x + g.pos.x + rendu.bornes.center().x * echelle,
                origine.y + texte.unwrap_or(g.pos.y) + rendu.bornes.center().y * echelle,
            );
            let rect = Rect::from_center_size(centre, rendu.bornes.size() * echelle * GROSSISSEMENT);
            painter.image(
                rendu.texture.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        caractere += glyphes.len() + usize::from(rangee.ends_with_newline);
    }
}

/// Un label dont les emoji sont en couleur — sélection et copie comprises,
/// comme un label d'egui.
pub fn label(ui: &mut Ui, texte: impl Into<WidgetText>) -> Response {
    label_avec(ui, texte, |label| label)
}

/// Le même, tronqué d'un « … » s'il n'a pas la place.
pub fn label_tronque(ui: &mut Ui, texte: impl Into<WidgetText>) -> Response {
    label_avec(ui, texte, egui::Label::truncate)
}

/// Un label réglé à la façon d'egui — `|l| l.sense(Sense::click()).truncate()` —,
/// ses emoji en couleur.
pub fn label_avec(ui: &mut Ui, texte: impl Into<WidgetText>, regler: impl FnOnce(egui::Label) -> egui::Label) -> Response {
    let texte = texte.into();
    let job = texte.into_layout_job(ui.style(), FontSelection::Default, ui.text_valign());
    let mut job = Arc::unwrap_or_clone(job);
    if !colorer(&mut job) {
        return ui.add(regler(egui::Label::new(job)));
    }
    let (pos, galley, response) = regler(egui::Label::new(job)).layout_in_ui(ui);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, ui.is_enabled(), galley.text()));
    if ui.is_rect_visible(response.rect) {
        // Comme un label d'egui : sélectionnable si le style le veut.
        let couleur = ui.style().visuals.text_color();
        if ui.style().interaction.selectable_labels {
            egui::text_selection::LabelSelectionState::label_text_selection(
                ui,
                &response,
                pos,
                galley.clone(),
                couleur,
                egui::Stroke::NONE,
            );
        } else {
            ui.painter().add(egui::epaint::TextShape::new(pos, galley.clone(), couleur));
        }
        peindre(ui.painter(), pos, &galley);
    }
    response
}

/// La mise en page d'un champ de saisie, emoji compris : à donner à
/// `TextEdit::layouter`, puis [`peindre`] la sortie de son `show` (dans son
/// `text_clip_rect`).
pub fn mise_en_page(ui: &Ui, texte: &str, police: egui::FontId, largeur: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple(texte.to_owned(), police, Color32::PLACEHOLDER, largeur);
    colorer(&mut job);
    ui.fonts_mut(|f| f.layout_job(job))
}

/// L'image couleur d'un emoji seul — pour un bouton de réaction
/// (`Button::image_and_text`) ou une vignette. `None` sans police emoji, ou
/// pour un emoji qu'elle ne connaît pas.
pub fn image(ctx: &Context, emoji: &str) -> Option<egui::Image<'static>> {
    let id = glyphe(emoji)?;
    let rendu = rendu(&cache(ctx), ctx, id)?;
    Some(egui::Image::new(&rendu.texture))
}

// ---------------------------------------------------------------------
// Les images
// ---------------------------------------------------------------------

/// L'image d'un emoji, et où elle se pose par rapport à son glyphe : ses
/// bornes en unités de la police (y vers le bas, depuis la ligne de base),
/// et l'avance du glyphe.
#[derive(Clone)]
struct Rendu {
    texture: TextureHandle,
    bornes: Rect,
    avance: f32,
}

type Cache = Arc<Mutex<HashMap<GlyphId, Option<Rendu>>>>;

fn cache(ctx: &Context) -> Cache {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Cache>(egui::Id::new("ki-ui-emoji")).clone())
}

fn rendu(cache: &Cache, ctx: &Context, id: GlyphId) -> Option<Rendu> {
    if let Some(connu) = cache.lock().ok()?.get(&id) {
        return connu.clone();
    }
    let rendu = peindre_glyphe(id).map(|(image, bornes, avance)| Rendu {
        texture: ctx.load_texture(
            format!("emoji-{}", id.to_u32()),
            image,
            TextureOptions { mipmap_mode: Some(egui::TextureFilter::Linear), ..TextureOptions::LINEAR },
        ),
        bornes,
        avance,
    });
    cache.lock().ok()?.insert(id, rendu.clone());
    rendu
}

/// Peint les calques d'un emoji : l'image, ses bornes en unités de la police
/// (y vers le bas), l'avance du glyphe.
fn peindre_glyphe(id: GlyphId) -> Option<(ColorImage, Rect, f32)> {
    let police = FontRef::new(police()?).ok()?;
    let glyphe = police.color_glyphs().get_with_format(id, ColorGlyphFormat::ColrV0)?;
    let mut calques = Calques::default();
    glyphe.paint(LocationRef::default(), &mut calques).ok()?;
    let palettes = police.color_palettes();
    let palette = palettes.get(0)?;
    let couleurs = palette.colors();
    let contours = police.outline_glyphs();

    // Chaque calque : son contour (y vers le bas) et sa couleur.
    let mut peints = Vec::with_capacity(calques.0.len());
    let mut bornes: Option<kurbo::Rect> = None;
    for (calque, index, alpha) in calques.0 {
        let Some(contour) = contours.get(calque) else { continue };
        let mut chemin = kurbo::BezPath::new();
        let reglage = DrawSettings::unhinted(Size::unscaled(), LocationRef::default());
        if contour.draw(reglage, &mut Plume(&mut chemin)).is_err() || chemin.elements().is_empty() {
            continue;
        }
        let boite = chemin.control_box();
        bornes = Some(bornes.map_or(boite, |b| b.union(boite)));
        // 0xFFFF : « la couleur du texte » — du blanc, sur nos fonds sombres.
        let c = couleurs.get(usize::from(index)).map_or([255, 255, 255, 255], |c| [c.red, c.green, c.blue, c.alpha]);
        let a = (f32::from(c[3]) * alpha).round().clamp(0.0, 255.0) as u8;
        peints.push((chemin, color::AlphaColor::<color::Srgb>::from_rgba8(c[0], c[1], c[2], a)));
    }
    let bornes = bornes?;
    let par_em = RESOLUTION / f64::from(police.head().ok()?.units_per_em());
    // Un pixel de marge autour, pour l'anticrénelage.
    let marge = 1.0 / par_em;
    let bornes = bornes.inflate(marge, marge);
    let largeur = (bornes.width() * par_em).ceil().clamp(1.0, 2048.0) as u16;
    let hauteur = (bornes.height() * par_em).ceil().clamp(1.0, 2048.0) as u16;
    let mut toile = vello_cpu::RenderContext::new(largeur, hauteur);
    toile.set_transform(kurbo::Affine::scale(par_em) * kurbo::Affine::translate((-bornes.x0, -bornes.y0)));
    for (chemin, couleur) in &peints {
        toile.set_paint(*couleur);
        toile.fill_path(chemin);
    }
    let mut pixels = vello_cpu::Pixmap::new(largeur, hauteur);
    toile.render(&mut pixels, &mut vello_cpu::Resources::new());
    let image = ColorImage::from_rgba_premultiplied(
        [usize::from(largeur), usize::from(hauteur)],
        pixels.data_as_u8_slice(),
    );
    let avance = police.glyph_metrics(Size::unscaled(), LocationRef::default()).advance_width(id)?;
    // Les bornes de l'image : sa toile entière, pixels arrondis compris.
    let bornes = Rect::from_min_size(
        Pos2::new(bornes.x0 as f32, bornes.y0 as f32),
        Vec2::new(f32::from(largeur), f32::from(hauteur)) / par_em as f32,
    );
    Some((image, bornes, avance))
}

/// Relève les calques d'un emoji v0 : glyphe, entrée de palette, opacité.
#[derive(Default)]
struct Calques(Vec<(GlyphId, u16, f32)>);

impl ColorPainter for Calques {
    fn push_transform(&mut self, _: Transform) {}
    fn pop_transform(&mut self) {}
    fn push_clip_glyph(&mut self, _: GlyphId) {}
    fn push_clip_box(&mut self, _: BoundingBox<f32>) {}
    fn pop_clip(&mut self) {}
    fn fill(&mut self, _: Brush<'_>) {}
    fn push_layer(&mut self, _: CompositeMode) {}

    fn fill_glyph(&mut self, glyphe: GlyphId, _: Option<Transform>, brosse: Brush<'_>) {
        if let Brush::Solid { palette_index, alpha } = brosse {
            self.0.push((glyphe, palette_index, alpha));
        }
    }
}

/// Le contour d'un glyphe en chemin kurbo, y vers le bas (celui de l'écran).
struct Plume<'a>(&'a mut kurbo::BezPath);

impl OutlinePen for Plume<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to((f64::from(x), -f64::from(y)));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to((f64::from(x), -f64::from(y)));
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.0.quad_to((f64::from(cx), -f64::from(cy)), (f64::from(x), -f64::from(y)));
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.0.curve_to(
            (f64::from(cx0), -f64::from(cy0)),
            (f64::from(cx1), -f64::from(cy1)),
            (f64::from(x), -f64::from(y)),
        );
    }

    fn close(&mut self) {
        self.0.close_path();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ce_qui_s_affiche_en_emoji() {
        for e in ["😂", "🔥", "👍", "👍🏽", "🇫🇷", "👨‍💻", "❤️", "1️⃣", "🏳️‍🌈", "😶‍🌫️"] {
            assert!(est_emoji(e), "{e} devrait être un emoji");
        }
        // Du texte, même quand Unicode le connaît aussi en emoji : sans son
        // sélecteur, ❤ ou ↩ restent des caractères.
        for t in ["a", "é", "1", "#", "©", "❤", "↩", "✕", "●", "▶", "❤︎", "—"] {
            assert!(!est_emoji(t), "{t} ne devrait pas être un emoji");
        }
    }

    #[test]
    fn le_francais_ordinaire_ne_se_decoupe_pas() {
        assert!(!peut_contenir("Ça marche, à demain — « super » ! 1, 2, 3 #ki"));
        assert!(peut_contenir("gg 🔥"));
        assert!(peut_contenir("1️⃣"));
    }

    /// Avec la police emoji de la machine (Windows) : chaque emoji passe
    /// dans sa section, transparent, le texte autour ne bouge pas.
    #[test]
    fn colorer_met_les_emoji_a_part() {
        if police().is_none() {
            return;
        }
        let format = egui::TextFormat { color: Color32::RED, ..Default::default() };
        let mut job = LayoutJob::single_section("gg 🔥👍🏽 ok".to_owned(), format);
        assert!(colorer(&mut job));
        let morceaux: Vec<(&str, bool)> = job
            .sections
            .iter()
            .map(|s| (&job.text[s.byte_range.start.0..s.byte_range.end.0], s.format.font_id.family == famille()))
            .collect();
        assert_eq!(morceaux, [("gg ", false), ("🔥", true), ("👍🏽", true), (" ok", false)]);
        assert!(job.sections.iter().filter(|s| s.format.font_id.family == famille()).all(|s| s.format.color == Color32::TRANSPARENT));
        assert_eq!(job.sections[0].format.color, Color32::RED);
    }

    /// Tout le chemin, sans fenêtre : un label avec deux emoji pose deux
    /// images couleur, dans son rectangle.
    #[test]
    fn un_label_pose_ses_emoji() {
        if police().is_none() {
            return;
        }
        let ctx = Context::default();
        crate::style::installer(&ctx);
        let mut rect_label = Rect::NOTHING;
        let mut images = Vec::new();
        for _ in 0..3 {
            let entree = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 200.0))),
                ..Default::default()
            };
            let sortie = ctx.run_ui(entree, |ui| {
                rect_label = label(ui, egui::RichText::new("gg 🔥 bravo 👍🏽 !").size(14.0)).rect;
            });
            images = sortie
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Mesh(m) if m.texture_id != egui::TextureId::default() => Some(m.calc_bounds()),
                    _ => None,
                })
                .collect();
            sortie.drop_without_applying_deltas();
        }
        assert_eq!(images.len(), 2, "deux emoji, deux images : {images:?}");
        for image in &images {
            assert!(rect_label.expand(4.0).contains_rect(*image), "{image:?} hors de {rect_label:?}");
        }
        assert!(images[0].max.x < images[1].min.x, "dans l'ordre du texte");
    }

    /// Une image vraiment en couleur : des pixels saturés, pas un contour gris.
    #[test]
    fn un_emoji_se_peint_en_couleur() {
        if police().is_none() {
            return;
        }
        // Pas de drapeau : Windows n'en dessine pas, sa police les laisse en
        // deux lettres — partout dans le système.
        assert!(glyphe("🇫🇷").is_none());
        for e in ["😂", "🔥", "👍🏽", "👨‍💻", "❤️"] {
            let id = glyphe(e).unwrap_or_else(|| panic!("{e} : pas de glyphe couleur"));
            let (image, bornes, avance) = peindre_glyphe(id).expect("rendu");
            assert!(avance > 0.0 && bornes.width() > 0.0 && bornes.height() > 0.0);
            let colores = image
                .pixels
                .iter()
                .filter(|p| {
                    let (r, g, b) = (i32::from(p.r()), i32::from(p.g()), i32::from(p.b()));
                    p.a() > 200 && (r - g).abs().max((g - b).abs()).max((r - b).abs()) > 60
                })
                .count();
            assert!(colores > image.pixels.len() / 10, "{e} : {colores} pixels colorés sur {}", image.pixels.len());
        }
    }
}
