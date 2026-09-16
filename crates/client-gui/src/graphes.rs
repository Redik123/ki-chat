//! Les graphiques de la page VALORANT, tous dessinés au painter d'egui :
//! une courbe dans le temps avec les paliers de rang en fond, une
//! sparkline, une bande de forme, des barres horizontales, une barre
//! victoires/défaites, une jauge, une heatmap de la semaine et les cases
//! d'un match manche par manche.
//!
//! Rien ici ne connaît les types du protocole, sauf [`ki_protocol::PointRR`]
//! (qui se convertit en [`PointCourbe`]) et [`ki_protocol::nom_de_rang`]
//! pour nommer les paliers : les pages construisent leurs séries et
//! passent des nombres. Aucune animation, aucun `request_repaint` — un
//! graphique se redessine quand egui redessine.
//!
//! Les projections (données → positions, pas des graduations, bornes des
//! bandes) sont des fonctions pures, sans `Ui`, testées en bas du fichier.
//! Elles ne divisent jamais par zéro et ne supposent rien de l'ordre des
//! points : une série se trie ici, du plus ancien au plus récent, et le
//! survol rend l'indice du point **dans la tranche reçue**.

use chrono::{Datelike, TimeZone, Timelike};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Response, Sense, Shape, Stroke, Ui,
    Vec2,
};
use ki_protocol::{nom_de_rang, PointRR};

use crate::theme::{
    self, BG_ACTIVE, BG_BASE, BG_DEEP, BORDER_SOFT, BORDER_STRONG, DANGER, SPEAK, TEXT, TEXT_DIM,
    TEXT_FAINT, WARN,
};
use crate::ui;

// ---------------------------------------------------------------------
// Conventions
// ---------------------------------------------------------------------

/// Marge intérieure d'un cadre de graphique.
const MARGE: Vec2 = Vec2::new(10.0, 8.0);
/// Deux graduations de dates ne s'approchent jamais plus que ça.
const ECART_GRADUATIONS: f32 = 56.0;
/// En dessous de cet écart entre deux points voisins, l'axe des dates
/// se replie sur un point par indice.
const REPLI_PX: f32 = 4.0;
/// Amplitude minimale de l'axe des valeurs, et marge de part et d'autre.
const AMPLITUDE_MIN: f32 = 40.0;
const MARGE_Y: f32 = 10.0;
/// Une bande de palier trop mince pour porter son nom.
const BANDE_NOMMEE_MIN: f32 = 12.0;
/// La place des étiquettes de dates sous la courbe.
const HAUTEUR_DATES: f32 = 13.0;
const JOUR_MS: u64 = 86_400_000;
const HEURE_MS: u64 = 3_600_000;
/// Les pas de graduation, du plus fin au plus large ; au-delà, des
/// multiples de trente jours.
const PAS: [u64; 9] = [
    HEURE_MS,
    3 * HEURE_MS,
    6 * HEURE_MS,
    12 * HEURE_MS,
    JOUR_MS,
    2 * JOUR_MS,
    7 * JOUR_MS,
    14 * JOUR_MS,
    30 * JOUR_MS,
];
const LEGENDE: f32 = 10.0;
/// Le rayon des points d'une courbe, et celui du halo de survol.
const RAYON_POINT: f32 = 3.0;
const RAYON_HALO: f32 = 6.0;
/// Les jours de la semaine, lundi en tête comme dans `activite`.
const JOURS: [&str; 7] = ["lun", "mar", "mer", "jeu", "ven", "sam", "dim"];

// ---------------------------------------------------------------------
// La courbe dans le temps
// ---------------------------------------------------------------------

/// Un point d'une courbe temporelle : sa date (ms Unix), sa valeur, le
/// sens du mouvement qui y mène (`None` : aucun ou inconnu) et une marque
/// à cercler (une descente protégée par un bouclier, par exemple).
#[derive(Debug, Clone, PartialEq)]
pub struct PointCourbe {
    pub date: u64,
    pub valeur: f32,
    pub monte: Option<bool>,
    pub marque: bool,
}

impl From<&PointRR> for PointCourbe {
    /// Un point de RR : la valeur sur l'échelle des paliers, monte ou
    /// descend selon le delta (ni l'un ni l'autre à zéro), marqué quand un
    /// bouclier a retenu la descente.
    fn from(p: &PointRR) -> Self {
        PointCourbe {
            date: p.date,
            valeur: valeur_rr(p.tier, p.rr),
            monte: (p.delta != 0).then_some(p.delta > 0),
            marque: p.protege,
        }
    }
}

/// Les abscisses d'une série, calculées une fois pour toute la courbe.
///
/// `ordre` range les indices de la tranche reçue du plus ancien au plus
/// récent ; `xs[k]` est l'abscisse (de 0 à `largeur`) du point `ordre[k]`,
/// `dates[k]` sa date. Quand deux dates voisines tombaient à moins de
/// [`REPLI_PX`], tout l'axe est replié sur un point par indice
/// (`par_indice`) : une soirée de dix classés reste lisible même si la
/// fenêtre couvre trente jours.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Projection {
    pub ordre: Vec<usize>,
    pub xs: Vec<f32>,
    pub dates: Vec<u64>,
    pub par_indice: bool,
    pub largeur: f32,
}

/// Projette une série sur `largeur` pixels ; `None` à moins de deux
/// points, il n'y a rien à tracer.
pub(crate) fn projeter(points: &[PointCourbe], largeur: f32) -> Option<Projection> {
    projeter_dates(&points.iter().map(|p| p.date).collect::<Vec<_>>(), largeur)
}

/// La même projection à partir des seules dates.
pub(crate) fn projeter_dates(dates: &[u64], largeur: f32) -> Option<Projection> {
    if dates.len() < 2 {
        return None;
    }
    let largeur = if largeur.is_finite() { largeur.max(0.0) } else { 0.0 };
    let mut ordre: Vec<usize> = (0..dates.len()).collect();
    ordre.sort_by_key(|&i| dates[i]);
    let dates: Vec<u64> = ordre.iter().map(|&i| dates[i]).collect();
    let (premier, dernier) = (dates[0], dates[dates.len() - 1]);
    let duree = dernier.saturating_sub(premier);
    let n = dates.len();
    let par_indice_xs = |k: usize| largeur * k as f32 / (n - 1) as f32;

    let mut xs: Vec<f32> = if duree == 0 {
        Vec::new()
    } else {
        dates
            .iter()
            .map(|&d| largeur * (d - premier) as f32 / duree as f32)
            .collect()
    };
    let serres = xs.is_empty() || xs.windows(2).any(|w| w[1] - w[0] < REPLI_PX);
    if serres {
        xs = (0..n).map(par_indice_xs).collect();
    }
    Some(Projection { ordre, xs, dates, par_indice: serres, largeur })
}

impl Projection {
    /// L'abscisse d'une date quelconque : linéaire entre le premier et le
    /// dernier point, ou, en mode replié, interpolée entre les deux points
    /// qui l'encadrent — une frontière d'acte tombe ainsi toujours entre
    /// les bons matchs. Hors de la série : sur le bord le plus proche.
    pub fn x_de_date(&self, date: u64) -> f32 {
        let (premier, dernier) = (self.dates[0], self.dates[self.dates.len() - 1]);
        if date <= premier {
            return 0.0;
        }
        if date >= dernier {
            return self.largeur;
        }
        if !self.par_indice {
            let duree = dernier - premier;
            return self.largeur * (date - premier) as f32 / duree as f32;
        }
        // `partition_point` rend le premier k tel que dates[k] >= date ;
        // date > premier garantit k >= 1, date < dernier que k existe.
        let k = self.dates.partition_point(|&d| d < date).clamp(1, self.dates.len() - 1);
        let (d0, d1) = (self.dates[k - 1], self.dates[k]);
        let (x0, x1) = (self.xs[k - 1], self.xs[k]);
        if d1 <= d0 {
            return x1;
        }
        x0 + (x1 - x0) * (date - d0) as f32 / (d1 - d0) as f32
    }

    /// L'indice, dans la tranche d'origine, du point le plus proche de
    /// l'abscisse `x` (de 0 à `largeur`).
    pub fn plus_proche(&self, x: f32) -> Option<usize> {
        let mut meilleur: Option<(usize, f32)> = None;
        for (k, &px) in self.xs.iter().enumerate() {
            let ecart = (px - x).abs();
            if meilleur.is_none_or(|(_, e)| ecart < e) {
                meilleur = Some((k, ecart));
            }
        }
        meilleur.map(|(k, _)| self.ordre[k])
    }

    /// Les graduations de dates : `(x, date, dans_la_journee)`, jamais à
    /// moins de `ecart_min` pixels l'une de l'autre. `decalage_ms` est le
    /// décalage local par rapport à UTC, pour poser les traits à minuit
    /// (ou à l'heure ronde) de l'utilisateur.
    pub fn graduations(&self, decalage_ms: i64, ecart_min: f32) -> Vec<(f32, u64, bool)> {
        let (premier, dernier) = (self.dates[0], self.dates[self.dates.len() - 1]);
        let pas = pas_des_graduations(dernier.saturating_sub(premier), self.largeur, ecart_min);
        let dans_la_journee = pas < JOUR_MS;
        // Le premier repère aligné sur le pas, à l'heure locale, avant ou
        // sur le premier point ; on avance ensuite d'un pas à la fois.
        let pas_i = pas as i64;
        let mut t = (premier as i64 + decalage_ms).div_euclid(pas_i) * pas_i - decalage_ms;
        let mut sortie: Vec<(f32, u64, bool)> = Vec::new();
        let mut derniere_x: Option<f32> = None;
        // Une borne de sécurité : jamais plus d'un repère par pixel.
        let plafond = self.largeur.max(1.0) as usize + 2;
        while t <= dernier as i64 && sortie.len() < plafond {
            if t >= premier as i64 {
                let x = self.x_de_date(t as u64);
                if derniere_x.is_none_or(|d| x - d >= ecart_min) {
                    sortie.push((x, t as u64, dans_la_journee));
                    derniere_x = Some(x);
                }
            }
            t = t.saturating_add(pas_i);
        }
        sortie
    }
}

/// Le pas des graduations, en millisecondes : le plus fin des pas
/// connus (heures, puis 1, 2, 7, 14, 30 jours) qui laisse `ecart_min`
/// pixels entre deux repères ; au-delà de trente jours, le multiple de
/// trente jours qu'il faut.
pub(crate) fn pas_des_graduations(duree_ms: u64, largeur: f32, ecart_min: f32) -> u64 {
    if duree_ms == 0 || largeur <= 0.0 {
        return JOUR_MS;
    }
    let convient = |pas: u64| largeur * pas as f32 / duree_ms as f32 >= ecart_min;
    if let Some(&pas) = PAS.iter().find(|&&p| convient(p)) {
        return pas;
    }
    // Trente jours ne suffisent pas : combien de fois trente ?
    let minimum_ms = (ecart_min * duree_ms as f32 / largeur).ceil();
    let mois = (minimum_ms / (30.0 * JOUR_MS as f32)).ceil().max(1.0);
    // Un million de mois ne se dessine pas ; on borne avant de convertir.
    (mois.min(1_000_000.0) as u64).saturating_mul(30 * JOUR_MS)
}

/// Les bornes de l'axe des valeurs : le min et le max de la série élargis
/// de dix de chaque côté, et jamais moins de quarante d'amplitude.
pub(crate) fn bornes_y(valeurs: impl IntoIterator<Item = f32>) -> (f32, f32) {
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for v in valeurs.into_iter().filter(|v| v.is_finite()) {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if lo > hi {
        (0.0, AMPLITUDE_MIN)
    } else {
        let (mut lo, mut hi) = (lo - MARGE_Y, hi + MARGE_Y);
        if hi - lo < AMPLITUDE_MIN {
            let centre = (lo + hi) / 2.0;
            lo = centre - AMPLITUDE_MIN / 2.0;
            hi = centre + AMPLITUDE_MIN / 2.0;
        }
        (lo, hi)
    }
}

/// Les bandes de palier qui croisent `[lo, hi[` : `(palier, bas, haut)`
/// avec `bas`/`haut` rognés à l'échelle. Le palier `c` couvre les valeurs
/// `[c × 100, (c + 1) × 100[` — cent RR par rang.
pub(crate) fn bandes_de_palier(lo: f32, hi: f32) -> Vec<(u8, f32, f32)> {
    if !(lo.is_finite() && hi.is_finite()) || hi <= lo {
        return Vec::new();
    }
    let premier = (lo / 100.0).floor().max(0.0) as u32;
    let dernier = (hi / 100.0).ceil().min(255.0) as u32;
    (premier..dernier)
        .map(|c| {
            let bas = (c as f32 * 100.0).max(lo);
            let haut = ((c + 1) as f32 * 100.0).min(hi);
            (c as u8, bas, haut)
        })
        .filter(|(_, bas, haut)| haut > bas)
        .collect()
}

/// L'ordonnée d'une valeur dans `zone`, `lo` en bas et `hi` en haut.
fn y_de(valeur: f32, lo: f32, hi: f32, zone: Rect) -> f32 {
    let part = ((valeur - lo) / (hi - lo).max(f32::EPSILON)).clamp(0.0, 1.0);
    zone.bottom() - zone.height() * part
}

/// Le décalage de l'heure locale sur UTC, en millisecondes.
fn decalage_local_ms() -> i64 {
    chrono::Local::now().offset().local_minus_utc() as i64 * 1000
}

/// « 3/9 » ou, dans la journée, « 21h » — l'étiquette d'une graduation.
fn etiquette_de_date(ms: u64, dans_la_journee: bool) -> String {
    chrono::Local
        .timestamp_millis_opt(ms as i64)
        .single()
        .map(|t| {
            if dans_la_journee {
                format!("{}h", t.hour())
            } else {
                format!("{}/{}", t.day(), t.month())
            }
        })
        .unwrap_or_default()
}

/// Une courbe dans le temps, pleine largeur sur `hauteur` pixels, avec en
/// fond les bandes de palier (une teinte de rang par centaine, si
/// `paliers`), les frontières d'acte en pointillé vertical, le pic en
/// pointillé horizontal, des graduations de dates qui ne se chevauchent
/// pas, et le survol d'un point qui l'entoure d'un halo et montre
/// `info(indice)`. Rend l'indice survolé, dans la tranche reçue ; à moins
/// de deux points elle n'alloue rien et rend `None` — au parent d'écrire
/// un mot à la place.
#[allow(clippy::too_many_arguments)]
pub fn courbe_temps(
    ui: &mut Ui,
    id: egui::Id,
    points: &[PointCourbe],
    hauteur: f32,
    paliers: bool,
    frontieres: &[(u64, String)],
    pic: Option<f32>,
    info: impl Fn(usize) -> String,
) -> Option<usize> {
    let largeur = ui.available_width();
    let proj = projeter(points, (largeur - 2.0 * MARGE.x).max(1.0))?;
    let (lo, hi) = bornes_y(points.iter().map(|p| p.valeur));

    ui.push_id(id, |ui| {
        let (rect, reponse) = ui.allocate_exact_size(Vec2::new(largeur, hauteur), Sense::hover());
        if !ui.is_rect_visible(rect) {
            return None;
        }
        let painter = ui.painter().with_clip_rect(rect);
        painter.rect_filled(rect, CornerRadius::same(6), BG_DEEP);
        // La zone de la courbe : la marge, et sous elle la ligne des dates.
        let interieur = rect.shrink2(MARGE);
        let zone = Rect::from_min_max(
            interieur.min,
            Pos2::new(interieur.right(), (interieur.bottom() - HAUTEUR_DATES).max(interieur.top() + 1.0)),
        );
        let pos = |k: usize| Pos2::new(zone.left() + proj.xs[k], y_de(points[proj.ordre[k]].valeur, lo, hi, zone));

        // Les bandes de palier, puis leur nom quand elles ont la place.
        if paliers {
            for (palier, bas, haut) in bandes_de_palier(lo, hi) {
                let bande = Rect::from_min_max(
                    Pos2::new(rect.left(), y_de(haut, lo, hi, zone)),
                    Pos2::new(rect.right(), y_de(bas, lo, hi, zone)),
                );
                painter.rect_filled(bande, CornerRadius::ZERO, theme::alpha(couleur_de_rang(palier), 16));
                if bande.height() >= BANDE_NOMMEE_MIN {
                    painter.text(
                        Pos2::new(zone.left() + 2.0, bande.center().y),
                        Align2::LEFT_CENTER,
                        nom_de_rang(palier),
                        FontId::proportional(LEGENDE),
                        TEXT_FAINT,
                    );
                }
            }
        }

        // La grille : les graduations de dates, un trait fin et l'étiquette.
        let decalage = decalage_local_ms();
        let graduations = proj.graduations(decalage, ECART_GRADUATIONS);
        let derniere = graduations.len().saturating_sub(1);
        for (k, (x, date, dans_la_journee)) in graduations.iter().enumerate() {
            let x = zone.left() + x;
            painter.line_segment(
                [Pos2::new(x, zone.top()), Pos2::new(x, zone.bottom())],
                Stroke::new(1.0_f32, BORDER_SOFT),
            );
            // Aux deux bouts, l'étiquette se range vers l'intérieur.
            let ancre = if k == 0 && graduations.len() > 1 {
                Align2::LEFT_TOP
            } else if k == derniere && graduations.len() > 1 {
                Align2::RIGHT_TOP
            } else {
                Align2::CENTER_TOP
            };
            painter.text(
                Pos2::new(x, zone.bottom() + 2.0),
                ancre,
                etiquette_de_date(*date, *dans_la_journee),
                FontId::proportional(LEGENDE),
                TEXT_FAINT,
            );
        }

        // Les frontières d'acte : un pointillé vertical et l'étiquette en haut.
        for (date, etiquette) in frontieres {
            let x = zone.left() + proj.x_de_date(*date);
            painter.extend(Shape::dashed_line(
                &[Pos2::new(x, zone.top()), Pos2::new(x, zone.bottom())],
                Stroke::new(1.0_f32, BORDER_STRONG),
                3.0,
                3.0,
            ));
            if !etiquette.is_empty() {
                painter.text(
                    Pos2::new(x + 3.0, zone.top()),
                    Align2::LEFT_TOP,
                    etiquette,
                    FontId::proportional(LEGENDE),
                    TEXT_FAINT,
                );
            }
        }

        // Le pic, s'il tient dans l'échelle.
        if let Some(pic) = pic.filter(|p| p.is_finite() && (lo..=hi).contains(p)) {
            let y = y_de(pic, lo, hi, zone);
            painter.extend(Shape::dashed_line(
                &[Pos2::new(zone.left(), y), Pos2::new(zone.right(), y)],
                Stroke::new(1.0_f32, BORDER_STRONG),
                4.0,
                3.0,
            ));
            painter.text(
                Pos2::new(zone.right(), y - 1.0),
                Align2::RIGHT_BOTTOM,
                "pic",
                FontId::proportional(LEGENDE),
                TEXT_FAINT,
            );
        }

        // Les segments, teintés par le point d'arrivée, puis les points.
        let teinte = |p: &PointCourbe| match p.monte {
            Some(true) => SPEAK,
            Some(false) => DANGER,
            None => TEXT_DIM,
        };
        for k in 1..proj.ordre.len() {
            let arrivee = &points[proj.ordre[k]];
            painter.line_segment([pos(k - 1), pos(k)], Stroke::new(2.0_f32, teinte(arrivee)));
        }
        for k in 0..proj.ordre.len() {
            let p = &points[proj.ordre[k]];
            painter.circle_filled(pos(k), RAYON_POINT, teinte(p));
            if p.marque {
                painter.circle_stroke(pos(k), RAYON_POINT + 1.5, Stroke::new(1.5_f32, WARN));
            }
        }

        // Le survol : le point le plus proche en abscisse, cerclé et commenté.
        let survole = reponse.hover_pos().and_then(|souris| {
            let i = proj.plus_proche(souris.x - zone.left())?;
            let k = proj.ordre.iter().position(|&j| j == i)?;
            painter.circle_stroke(pos(k), RAYON_HALO, Stroke::new(1.5_f32, TEXT));
            let texte = info(i);
            if !texte.is_empty() {
                reponse.clone().on_hover_text(texte);
            }
            Some(i)
        });
        survole
    })
    .inner
}

// ---------------------------------------------------------------------
// Sparkline
// ---------------------------------------------------------------------

/// Une ligne minuscule sur `taille`, sans axes ni fond, à la `teinte`
/// que l'appelant lui donne — celle de son bilan, pour que la couleur dise
/// la même chose que l'infobulle : une série de RR dont le premier point
/// porte déjà un gain peut finir sous son départ avec une somme positive.
/// Un point sur la dernière valeur. À moins de deux valeurs, la place est
/// réservée mais rien n'est tracé. Rend la réponse, pour un tooltip du
/// parent.
pub fn sparkline(ui: &mut Ui, valeurs: &[f32], taille: Vec2, teinte: Color32) -> Response {
    let (rect, reponse) = ui.allocate_exact_size(taille, Sense::hover());
    if !ui.is_rect_visible(rect) || valeurs.len() < 2 {
        return reponse;
    }
    let valeurs: Vec<f32> = valeurs.iter().map(|v| if v.is_finite() { *v } else { 0.0 }).collect();
    let (lo, hi) = valeurs.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    // Une série plate se trace au milieu, sans diviser par zéro.
    let (lo, hi) = if hi <= lo { (lo - 1.0, hi + 1.0) } else { (lo, hi) };
    let zone = rect.shrink(2.0);
    let n = valeurs.len();
    let pos = |i: usize| {
        Pos2::new(
            zone.left() + zone.width() * i as f32 / (n - 1) as f32,
            zone.bottom() - zone.height() * ((valeurs[i] - lo) / (hi - lo)).clamp(0.0, 1.0),
        )
    };
    let trace: Vec<Pos2> = (0..n).map(pos).collect();
    let painter = ui.painter();
    painter.add(Shape::line(trace, Stroke::new(1.5_f32, teinte)));
    painter.circle_filled(pos(n - 1), 2.0, teinte);
    reponse
}

// ---------------------------------------------------------------------
// Bande de forme
// ---------------------------------------------------------------------

/// L'écart entre deux cases d'une bande.
const ECART_CASES: f32 = 2.0;

/// La case sous l'abscisse `x` (depuis le bord gauche), quand `n` cases
/// de `largeur` se suivent séparées d'`ecart` ; `None` hors des cases.
pub(crate) fn case_sous(x: f32, largeur: f32, ecart: f32, n: usize) -> Option<usize> {
    if n == 0 || largeur <= 0.0 || x < 0.0 {
        return None;
    }
    let pas = largeur + ecart;
    let i = (x / pas).floor();
    if i < 0.0 || i >= n as f32 {
        return None;
    }
    let i = i as usize;
    (x - i as f32 * pas <= largeur).then_some(i)
}

/// La couleur d'un résultat : 1 gagné, -1 perdu, 0 nul.
fn teinte_de_resultat(r: i8) -> Color32 {
    match r {
        1.. => SPEAK,
        ..=-1 => DANGER,
        0 => BORDER_STRONG,
    }
}

/// Les cases d'une forme, `resultats` du plus récent au plus ancien comme
/// `FicheValorant::forme` les rend, la plus récente dessinée **à droite**.
/// Une case de `case` pixels par résultat (1 victoire SPEAK, -1 défaite
/// DANGER, 0 nul gris) ; `info(indice)` en tooltip au survol. Rend la
/// réponse de toute la bande.
pub fn bande_forme(ui: &mut Ui, resultats: &[i8], case: Vec2, info: Option<&dyn Fn(usize) -> String>) -> Response {
    let n = resultats.len();
    let largeur = if n == 0 { 0.0 } else { case.x * n as f32 + ECART_CASES * (n - 1) as f32 };
    let (rect, reponse) = ui.allocate_exact_size(Vec2::new(largeur, case.y), Sense::hover());
    if !ui.is_rect_visible(rect) || n == 0 {
        return reponse;
    }
    let painter = ui.painter();
    // La colonne k (de gauche à droite) montre le résultat n − 1 − k.
    let indice = |colonne: usize| n - 1 - colonne;
    for colonne in 0..n {
        let x = rect.left() + colonne as f32 * (case.x + ECART_CASES);
        let r = Rect::from_min_size(Pos2::new(x, rect.top()), case);
        painter.rect_filled(r, CornerRadius::same(2), teinte_de_resultat(resultats[indice(colonne)]));
    }
    if let (Some(info), Some(souris)) = (info, reponse.hover_pos()) {
        if let Some(colonne) = case_sous(souris.x - rect.left(), case.x, ECART_CASES, n) {
            let i = indice(colonne);
            let x = rect.left() + colonne as f32 * (case.x + ECART_CASES);
            let r = Rect::from_min_size(Pos2::new(x, rect.top()), case);
            painter.rect_stroke(r.expand(1.0), CornerRadius::same(3), Stroke::new(1.0_f32, TEXT), egui::StrokeKind::Outside);
            let texte = info(i);
            if !texte.is_empty() {
                reponse.clone().on_hover_text(texte);
            }
        }
    }
    reponse
}

// ---------------------------------------------------------------------
// Barres horizontales
// ---------------------------------------------------------------------

/// Une ligne de [`barres`] : le libellé à gauche, une part colorée sur un
/// fond gris (toutes deux de 0 à 1 de la largeur du rail), le texte à
/// droite, et ce que dit le survol.
pub struct Barre<'a> {
    pub label: &'a str,
    pub part: f32,
    pub fond: f32,
    pub texte: String,
    pub couleur: Color32,
    pub info: String,
}

/// Des barres horizontales, une par ligne de `hauteur_ligne` : le libellé
/// sur `largeur_label` pixels (rogné s'il déborde), le rail creusé, le
/// `fond` gris (BG_ACTIVE) derrière la `part` colorée, le texte à droite ;
/// `info` en tooltip. Sans ligne, rien n'est dessiné.
pub fn barres(ui: &mut Ui, lignes: &[Barre], largeur_label: f32, hauteur_ligne: f32) {
    let police = FontId::proportional(12.0);
    let largeur = ui.available_width();
    for ligne in lignes {
        let (rect, reponse) = ui.allocate_exact_size(Vec2::new(largeur, hauteur_ligne), Sense::hover());
        if !ui.is_rect_visible(rect) {
            continue;
        }
        let libelle = ui.fonts(|f| f.layout_no_wrap(ligne.label.to_owned(), police.clone(), TEXT_DIM));
        let texte = ui.fonts(|f| f.layout_no_wrap(ligne.texte.clone(), police.clone(), TEXT));
        let painter = ui.painter();
        let centre_y = rect.center().y;

        // Le libellé, rogné à sa colonne.
        let zone_label = Rect::from_min_max(rect.min, Pos2::new(rect.left() + largeur_label, rect.bottom()));
        let hauteur_libelle = libelle.size().y;
        painter
            .with_clip_rect(zone_label)
            .galley(Pos2::new(rect.left(), centre_y - hauteur_libelle / 2.0), libelle, TEXT_DIM);

        // Le texte, calé à droite ; le rail prend ce qui reste entre les deux.
        let largeur_texte = texte.size().x;
        let hauteur_texte = texte.size().y;
        painter.galley(Pos2::new(rect.right() - largeur_texte, centre_y - hauteur_texte / 2.0), texte, TEXT);

        let gauche = rect.left() + largeur_label + 8.0;
        let droite = rect.right() - largeur_texte - 8.0;
        if droite - gauche >= 4.0 {
            let epaisseur = (hauteur_ligne * 0.5).clamp(4.0, 10.0);
            let rail = Rect::from_min_max(
                Pos2::new(gauche, centre_y - epaisseur / 2.0),
                Pos2::new(droite, centre_y + epaisseur / 2.0),
            );
            let arrondi = CornerRadius::same((epaisseur / 2.0).round().clamp(0.0, 255.0) as u8);
            painter.rect_filled(rail, arrondi, BG_DEEP);
            let remplir = |part: f32, couleur: Color32| {
                let part = if part.is_finite() { part.clamp(0.0, 1.0) } else { 0.0 };
                if part > 0.001 {
                    let w = (rail.width() * part).max(epaisseur);
                    painter.rect_filled(Rect::from_min_size(rail.min, Vec2::new(w, epaisseur)), arrondi, couleur);
                }
            };
            remplir(ligne.fond, BG_ACTIVE);
            remplir(ligne.part, ligne.couleur);
        }
        if !ligne.info.is_empty() {
            reponse.on_hover_text(&ligne.info);
        }
    }
}

// ---------------------------------------------------------------------
// Barre victoires / défaites
// ---------------------------------------------------------------------

/// Une barre de `largeur` pixels partagée entre victoires (SPEAK),
/// défaites (DANGER) et nuls (gris), au prorata ; vide et creusée quand
/// tout est à zéro. Rend la réponse, pour un tooltip du parent.
pub fn barre_vd(ui: &mut Ui, v: u32, d: u32, nuls: u32, largeur: f32) -> Response {
    let hauteur = 8.0;
    let (rect, reponse) = ui.allocate_exact_size(Vec2::new(largeur.max(0.0), hauteur), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return reponse;
    }
    let painter = ui.painter();
    let arrondi = CornerRadius::same(4);
    painter.rect_filled(rect, arrondi, BG_DEEP);
    let total = v as u64 + d as u64 + nuls as u64;
    if total == 0 {
        return reponse;
    }
    let part = |n: u32| rect.width() * n as f32 / total as f32;
    let mut x = rect.left();
    for (n, couleur) in [(v, SPEAK), (d, DANGER), (nuls, BORDER_STRONG)] {
        let w = part(n);
        if w > 0.0 {
            let morceau = Rect::from_min_size(Pos2::new(x, rect.top()), Vec2::new(w, hauteur));
            painter.rect_filled(morceau, CornerRadius::ZERO, couleur);
            x += w;
        }
    }
    // Les bouts arrondis par-dessus les morceaux carrés : une découpe
    // en creux de la couleur du rail, comme le vumètre.
    painter.rect_stroke(rect, arrondi, Stroke::new(1.0_f32, BG_DEEP), egui::StrokeKind::Inside);
    reponse
}

// ---------------------------------------------------------------------
// Jauge
// ---------------------------------------------------------------------

/// Une jauge de `largeur` pixels remplie à `part` (0 à 1) en `couleur`,
/// sur le vumètre de `ui::paint_meter`, avec un repère vertical WARN à
/// `repere` (0 à 1) s'il y en a un — la médiane du groupe, par exemple.
/// Rend la réponse, pour un tooltip du parent.
pub fn jauge(ui: &mut Ui, part: f32, repere: Option<f32>, largeur: f32, couleur: Color32) -> Response {
    let (rect, reponse) = ui.allocate_exact_size(Vec2::new(largeur.max(0.0), 6.0), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return reponse;
    }
    let part = if part.is_finite() { part } else { 0.0 };
    ui::paint_meter(ui.painter(), rect, part, couleur);
    if let Some(r) = repere.filter(|r| r.is_finite()) {
        let x = rect.left() + rect.width() * r.clamp(0.0, 1.0);
        ui.painter().line_segment(
            [Pos2::new(x, rect.top() - 2.0), Pos2::new(x, rect.bottom() + 2.0)],
            Stroke::new(1.5_f32, WARN),
        );
    }
    reponse
}

// ---------------------------------------------------------------------
// Heatmap de la semaine
// ---------------------------------------------------------------------

/// La largeur des noms de jours, à gauche de la heatmap.
const LARGEUR_JOURS: f32 = 26.0;
/// La ligne des heures, au-dessus.
const HAUTEUR_HEURES: f32 = 12.0;

/// La teinte d'une case : du fond au vert selon `valeur / max`, `max`
/// jamais nul ; zéro reste le fond.
pub(crate) fn teinte_de_case(valeur: u16, max: u16) -> Color32 {
    if valeur == 0 {
        return BG_BASE;
    }
    let part = valeur as f32 / max.max(1) as f32;
    theme::mix(theme::mix(BG_ACTIVE, SPEAK, 0.25), SPEAK, part.clamp(0.0, 1.0))
}

/// Sept lignes (lundi en haut) sur vingt-quatre colonnes : la case
/// `cases[jour × 24 + heure]` d'autant plus verte qu'elle compte de
/// parties (les cases manquantes valent zéro) ; les jours à gauche, une
/// heure sur six en haut ; `info(jour, heure)` en tooltip au survol.
pub fn heatmap_semaine(ui: &mut Ui, cases: &[u16], info: impl Fn(usize, usize) -> String) {
    let largeur = ui.available_width();
    let cote = ((largeur - 2.0 * MARGE.x - LARGEUR_JOURS) / 24.0).clamp(6.0, 18.0);
    let (w, h) = (cote, (cote * 0.8).clamp(6.0, 14.0));
    let pas = 1.0;
    let hauteur = 2.0 * MARGE.y + HAUTEUR_HEURES + 7.0 * (h + pas);
    let (rect, reponse) = ui.allocate_exact_size(Vec2::new(largeur, hauteur), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, CornerRadius::same(6), BG_DEEP);
    let origine = Pos2::new(rect.left() + MARGE.x + LARGEUR_JOURS, rect.top() + MARGE.y + HAUTEUR_HEURES);
    let valeur = |jour: usize, heure: usize| cases.get(jour * 24 + heure).copied().unwrap_or(0);
    let max = cases.iter().copied().max().unwrap_or(0);

    for (jour, nom) in JOURS.iter().enumerate() {
        let y = origine.y + jour as f32 * (h + pas);
        painter.text(
            Pos2::new(origine.x - 6.0, y + h / 2.0),
            Align2::RIGHT_CENTER,
            *nom,
            FontId::proportional(LEGENDE),
            TEXT_FAINT,
        );
        for heure in 0..24 {
            let x = origine.x + heure as f32 * (w + pas);
            let case = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h));
            painter.rect_filled(case, CornerRadius::same(2), teinte_de_case(valeur(jour, heure), max));
        }
    }
    for heure in (0..24).step_by(6) {
        let x = origine.x + heure as f32 * (w + pas);
        painter.text(
            Pos2::new(x, origine.y - 2.0),
            Align2::LEFT_BOTTOM,
            format!("{heure}h"),
            FontId::proportional(LEGENDE),
            TEXT_FAINT,
        );
    }

    if let Some(souris) = reponse.hover_pos() {
        let (colonne, ligne) = (
            case_sous(souris.x - origine.x, w, pas, 24),
            case_sous(souris.y - origine.y, h, pas, 7),
        );
        if let (Some(heure), Some(jour)) = (colonne, ligne) {
            let case = Rect::from_min_size(
                Pos2::new(origine.x + heure as f32 * (w + pas), origine.y + jour as f32 * (h + pas)),
                Vec2::new(w, h),
            );
            painter.rect_stroke(case, CornerRadius::same(2), Stroke::new(1.0_f32, TEXT), egui::StrokeKind::Outside);
            let texte = info(jour, heure);
            if !texte.is_empty() {
                reponse.on_hover_text(texte);
            }
        }
    }
}

// ---------------------------------------------------------------------
// Les manches d'un match
// ---------------------------------------------------------------------

/// La case d'une manche, et l'espace de la mi-temps.
const CASE_MANCHE: Vec2 = Vec2::new(10.0, 14.0);
const MI_TEMPS: usize = 12;
const ECART_MI_TEMPS: f32 = 6.0;

/// L'abscisse de la case `i` d'un déroulé : les cases se suivent, avec un
/// espace de plus à la mi-temps.
pub(crate) fn x_de_manche(i: usize) -> f32 {
    let mi_temps = if i >= MI_TEMPS { ECART_MI_TEMPS } else { 0.0 };
    i as f32 * (CASE_MANCHE.x + ECART_CASES) + mi_temps
}

/// La manche sous l'abscisse `x`, parmi `n`.
pub(crate) fn manche_sous(x: f32, n: usize) -> Option<usize> {
    (0..n).find(|&i| {
        let debut = x_de_manche(i);
        x >= debut && x <= debut + CASE_MANCHE.x
    })
}

/// Une case par manche du `deroule` (`V` gagnée SPEAK, `D` perdue DANGER,
/// autre lettre en gris), dans l'ordre, un espace après la douzième pour
/// marquer la mi-temps ; « manche 13 · gagnée » au survol. Un déroulé
/// vide ne dessine rien.
pub fn cases_manches(ui: &mut Ui, deroule: &str) {
    let manches: Vec<char> = deroule.chars().collect();
    let n = manches.len();
    if n == 0 {
        return;
    }
    let largeur = x_de_manche(n - 1) + CASE_MANCHE.x;
    let (rect, reponse) = ui.allocate_exact_size(Vec2::new(largeur, CASE_MANCHE.y), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    let teinte = |c: char| match c.to_ascii_uppercase() {
        'V' => SPEAK,
        'D' => DANGER,
        _ => BORDER_STRONG,
    };
    for (i, &c) in manches.iter().enumerate() {
        let case = Rect::from_min_size(Pos2::new(rect.left() + x_de_manche(i), rect.top()), CASE_MANCHE);
        painter.rect_filled(case, CornerRadius::same(2), teinte(c));
    }
    if let Some(i) = reponse.hover_pos().and_then(|souris| manche_sous(souris.x - rect.left(), n)) {
        let case = Rect::from_min_size(Pos2::new(rect.left() + x_de_manche(i), rect.top()), CASE_MANCHE);
        painter.rect_stroke(case.expand(1.0), CornerRadius::same(3), Stroke::new(1.0_f32, TEXT), egui::StrokeKind::Outside);
        let issue = match manches[i].to_ascii_uppercase() {
            'V' => "gagnée",
            'D' => "perdue",
            _ => "?",
        };
        reponse.on_hover_text(format!("manche {} · {}", i + 1, issue));
    }
}

// ---------------------------------------------------------------------
// Les rangs
// ---------------------------------------------------------------------

/// La couleur d'un palier VALORANT, proche de celle du jeu : du gris du
/// Fer au jaune du Radiant.
pub fn couleur_de_rang(tier: u8) -> Color32 {
    match tier {
        3..=5 => Color32::from_rgb(0x8f, 0x8f, 0x8f),
        6..=8 => Color32::from_rgb(0xb5, 0x7f, 0x4a),
        9..=11 => Color32::from_rgb(0xc8, 0xd0, 0xd8),
        12..=14 => Color32::from_rgb(0xe8, 0xc0, 0x40),
        15..=17 => Color32::from_rgb(0x3f, 0xb8, 0xc8),
        18..=20 => Color32::from_rgb(0xb0, 0x7c, 0xf0),
        21..=23 => Color32::from_rgb(0x4f, 0xc8, 0x6a),
        24..=26 => Color32::from_rgb(0xe0, 0x4a, 0x5a),
        27.. => Color32::from_rgb(0xff, 0xf2, 0x9a),
        _ => TEXT_FAINT,
    }
}

/// Un rang sur une seule échelle : cent RR par palier — Diamant 2 à 57 RR
/// vaut 1 657, et les courbes se tracent dessus.
pub fn valeur_rr(tier: u8, rr: u16) -> f32 {
    tier as f32 * 100.0 + rr as f32
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn point(date: u64, valeur: f32) -> PointCourbe {
        PointCourbe { date, valeur, monte: None, marque: false }
    }

    #[test]
    fn les_graduations_du_temps_ne_se_chevauchent_pas() {
        // Cent points sur trente jours, à 420 px : un repère tous les sept
        // jours (98 px), jamais deux à moins de 56 px.
        let debut = 1_756_000_000_000_u64;
        let points: Vec<PointCourbe> = (0..100)
            .map(|i| point(debut + i * 30 * JOUR_MS / 99, 1_500.0 + i as f32))
            .collect();
        let proj = projeter(&points, 420.0).expect("deux points au moins");
        assert!(!proj.par_indice, "des points espacés de plus de 4 px gardent l'axe des dates");
        assert_eq!(pas_des_graduations(30 * JOUR_MS, 420.0, ECART_GRADUATIONS), 7 * JOUR_MS);
        let graduations = proj.graduations(0, ECART_GRADUATIONS);
        assert!(graduations.len() >= 3, "au moins trois repères sur un mois : {graduations:?}");
        for paire in graduations.windows(2) {
            assert!(paire[1].0 - paire[0].0 >= ECART_GRADUATIONS, "{paire:?}");
        }
        for (x, date, dans_la_journee) in &graduations {
            assert!((0.0..=420.0).contains(x));
            assert!(*date >= debut && *date <= debut + 30 * JOUR_MS);
            assert!(!dans_la_journee, "un pas d'une semaine se lit en jours");
        }

        // Une soirée de trois heures : des heures rondes, toujours à
        // 56 px au moins l'une de l'autre.
        let soiree: Vec<PointCourbe> = (0..10).map(|i| point(debut + i * 20 * 60_000, 1_500.0)).collect();
        let proj = projeter(&soiree, 300.0).unwrap();
        let graduations = proj.graduations(0, ECART_GRADUATIONS);
        assert!(!graduations.is_empty(), "une soirée a des heures rondes");
        assert!(graduations.iter().all(|g| g.2), "en dessous d'un jour, des heures");
        for paire in graduations.windows(2) {
            assert!(paire[1].0 - paire[0].0 >= ECART_GRADUATIONS, "{paire:?}");
        }

        // Au-delà de trente jours par repère, des multiples de trente.
        let pas = pas_des_graduations(1_000 * JOUR_MS, 200.0, ECART_GRADUATIONS);
        assert_eq!(pas % (30 * JOUR_MS), 0);
        assert!(200.0 * pas as f32 / (1_000.0 * JOUR_MS as f32) >= ECART_GRADUATIONS);
        // Sans durée ni largeur, un jour — et pas de division par zéro.
        assert_eq!(pas_des_graduations(0, 420.0, ECART_GRADUATIONS), JOUR_MS);
        assert_eq!(pas_des_graduations(JOUR_MS, 0.0, ECART_GRADUATIONS), JOUR_MS);
    }

    #[test]
    fn une_serie_d_un_point_ne_dessine_rien() {
        assert!(projeter(&[point(1_000, 1_200.0)], 400.0).is_none());
        assert!(projeter(&[], 400.0).is_none());
        assert!(projeter(&[point(1_000, 1_200.0), point(2_000, 1_210.0)], 400.0).is_some());
    }

    #[test]
    fn les_bandes_de_palier_couvrent_l_echelle() {
        let bandes = bandes_de_palier(1_240.0, 1_310.0);
        let paliers: Vec<u8> = bandes.iter().map(|b| b.0).collect();
        assert_eq!(paliers, vec![12, 13]);
        assert_eq!(bandes[0].1, 1_240.0);
        assert_eq!(bandes[0].2, 1_300.0);
        assert_eq!(bandes[1].1, 1_300.0);
        assert_eq!(bandes[1].2, 1_310.0);
        // Les bornes élargies par `bornes_y` donnent les mêmes paliers.
        let (lo, hi) = bornes_y([1_240.0, 1_310.0]);
        assert_eq!((lo, hi), (1_230.0, 1_320.0));
        let paliers: Vec<u8> = bandes_de_palier(lo, hi).iter().map(|b| b.0).collect();
        assert_eq!(paliers, vec![12, 13]);
        // Une échelle vide ou à l'envers : pas de bande, pas de panique.
        assert!(bandes_de_palier(1_300.0, 1_300.0).is_empty());
        assert!(bandes_de_palier(f32::NAN, 1_300.0).is_empty());
        assert!(bandes_de_palier(1_400.0, 1_300.0).is_empty());
        // Le Radiant et au-delà tiennent dans un u8 sans déborder.
        let hauts = bandes_de_palier(2_690.0, 2_720.0);
        assert_eq!(hauts.iter().map(|b| b.0).collect::<Vec<_>>(), vec![26, 27]);
    }

    #[test]
    fn l_axe_des_valeurs_a_toujours_de_l_amplitude() {
        // Une série plate s'élargit à quarante autour d'elle-même.
        assert_eq!(bornes_y([1_500.0, 1_500.0]), (1_480.0, 1_520.0));
        // Une série vide ou sans valeur finie a quand même une échelle.
        let (lo, hi) = bornes_y(std::iter::empty());
        assert!(hi > lo);
        let (lo, hi) = bornes_y([f32::NAN, f32::INFINITY]);
        assert!(hi > lo);
        // Dix de marge de chaque côté quand l'amplitude suffit.
        assert_eq!(bornes_y([1_000.0, 1_100.0]), (990.0, 1_110.0));
    }

    #[test]
    fn les_dates_serrees_se_replient_sur_l_indice() {
        let debut = 1_756_000_000_000_u64;
        // Deux matchs à la même minute, un autre trois semaines plus tard :
        // en dates réelles les deux premiers se confondraient.
        let points = [point(debut, 1.0), point(debut + 60_000, 2.0), point(debut + 21 * JOUR_MS, 3.0)];
        let proj = projeter(&points, 400.0).unwrap();
        assert!(proj.par_indice);
        assert_eq!(proj.xs, vec![0.0, 200.0, 400.0]);
        // Une frontière entre les deux premiers points tombe entre leurs
        // abscisses, pas au bord gauche.
        let x = proj.x_de_date(debut + 30_000);
        assert!(x > 0.0 && x < 200.0, "{x}");
        // Hors de la série, on reste sur le bord.
        assert_eq!(proj.x_de_date(debut - 1), 0.0);
        assert_eq!(proj.x_de_date(debut + 100 * JOUR_MS), 400.0);
        // Toutes les dates égales : replié aussi, sans division par zéro.
        let proj = projeter(&[point(debut, 1.0), point(debut, 2.0)], 100.0).unwrap();
        assert!(proj.par_indice);
        assert_eq!(proj.xs, vec![0.0, 100.0]);

        // Des dates espacées gardent l'échelle réelle, et une date
        // intermédiaire se projette linéairement.
        let points = [point(debut, 1.0), point(debut + 10 * JOUR_MS, 2.0), point(debut + 20 * JOUR_MS, 3.0)];
        let proj = projeter(&points, 400.0).unwrap();
        assert!(!proj.par_indice);
        assert_eq!(proj.xs, vec![0.0, 200.0, 400.0]);
        assert_eq!(proj.x_de_date(debut + 5 * JOUR_MS), 100.0);
    }

    #[test]
    fn le_survol_rend_l_indice_de_la_tranche_recue() {
        // La fiche arrive du plus récent au plus ancien ; la projection
        // trie, mais le survol parle des indices d'origine.
        let debut = 1_756_000_000_000_u64;
        let points = [
            point(debut + 2 * JOUR_MS, 3.0),
            point(debut + JOUR_MS, 2.0),
            point(debut, 1.0),
        ];
        let proj = projeter(&points, 200.0).unwrap();
        assert_eq!(proj.ordre, vec![2, 1, 0]);
        assert_eq!(proj.plus_proche(0.0), Some(2));
        assert_eq!(proj.plus_proche(95.0), Some(1));
        assert_eq!(proj.plus_proche(1_000.0), Some(0));
    }

    #[test]
    fn les_cases_se_retrouvent_sous_la_souris() {
        // Dix cases de 8 px espacées de 2.
        assert_eq!(case_sous(0.0, 8.0, 2.0, 10), Some(0));
        assert_eq!(case_sous(8.0, 8.0, 2.0, 10), Some(0));
        assert_eq!(case_sous(9.0, 8.0, 2.0, 10), None, "dans l'espace entre deux cases");
        assert_eq!(case_sous(10.0, 8.0, 2.0, 10), Some(1));
        assert_eq!(case_sous(97.0, 8.0, 2.0, 10), Some(9));
        assert_eq!(case_sous(120.0, 8.0, 2.0, 10), None);
        assert_eq!(case_sous(-1.0, 8.0, 2.0, 10), None);
        assert_eq!(case_sous(3.0, 8.0, 2.0, 0), None);
        assert_eq!(case_sous(3.0, 0.0, 2.0, 5), None);

        // Les manches : la treizième saute la mi-temps.
        assert_eq!(x_de_manche(0), 0.0);
        assert_eq!(x_de_manche(12), 12.0 * 12.0 + ECART_MI_TEMPS);
        assert_eq!(manche_sous(x_de_manche(12) + 1.0, 24), Some(12));
        assert_eq!(manche_sous(x_de_manche(12) - 1.0, 24), None);
        assert_eq!(manche_sous(x_de_manche(5) + 3.0, 24), Some(5));
        assert_eq!(manche_sous(-3.0, 24), None);
        assert_eq!(manche_sous(3.0, 0), None);
    }

    #[test]
    fn les_teintes_ne_divisent_pas_par_zero() {
        assert_eq!(teinte_de_case(0, 0), BG_BASE);
        assert_eq!(teinte_de_case(0, 10), BG_BASE);
        // Un max nul avec une valeur non nulle (une heatmap incohérente)
        // donne quand même une couleur.
        let _ = teinte_de_case(3, 0);
        assert_eq!(teinte_de_case(10, 10), SPEAK);
        assert_eq!(teinte_de_resultat(1), SPEAK);
        assert_eq!(teinte_de_resultat(-1), DANGER);
        assert_eq!(teinte_de_resultat(0), BORDER_STRONG);
    }

    #[test]
    fn les_rangs_ont_une_valeur_et_une_couleur() {
        assert_eq!(valeur_rr(0, 0), 0.0);
        assert_eq!(valeur_rr(16, 57), 1_657.0);
        assert_eq!(valeur_rr(27, 850), 3_550.0);
        assert_eq!(couleur_de_rang(0), TEXT_FAINT);
        assert_eq!(couleur_de_rang(3), couleur_de_rang(5));
        assert_ne!(couleur_de_rang(5), couleur_de_rang(6));
        assert_eq!(couleur_de_rang(27), couleur_de_rang(255));

        // Un point de RR devient un point de courbe : le sens d'après le
        // delta, la marque d'après le bouclier.
        let p = PointRR { tier: 16, rr: 57, delta: 19, protege: true, ..Default::default() };
        let c = PointCourbe::from(&p);
        assert_eq!(c.valeur, 1_657.0);
        assert_eq!(c.monte, Some(true));
        assert!(c.marque);
        let p = PointRR { delta: 0, ..Default::default() };
        assert_eq!(PointCourbe::from(&p).monte, None);
        let p = PointRR { delta: -3, ..Default::default() };
        assert_eq!(PointCourbe::from(&p).monte, Some(false));
    }
}
