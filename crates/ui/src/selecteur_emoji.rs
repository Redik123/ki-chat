//! Le sélecteur d'emoji : un bouton sourire, et au-dessus un panneau — une
//! recherche (en français ou en anglais), les récents, les catégories, la
//! grille en couleur et le nom de l'emoji survolé. Plus besoin d'ouvrir
//! celui de Windows.
//!
//! La liste vient d'Unicode (le crate `emojis`, ses noms anglais et ses
//! codes courts `:joy:`) ; les emoji courants ont en plus leurs mots
//! français, plus bas. Les récents et la teinte de peau se retiennent d'une
//! session à l'autre.

use std::collections::HashMap;
use std::sync::OnceLock;

use egui::{Align2, CornerRadius, FontId, Id, Key, ScrollArea, Sense, Ui, Vec2};
use emojis::{Emoji, Group, SkinTone};

use crate::composants as c;
use crate::icones::Icon;
use crate::jetons::{couleur, espace, rayon, texte};

/// Combien d'emoji par rangée.
const COLONNES: usize = 9;
/// Le côté d'une case.
const CASE: f32 = 36.0;
/// La hauteur de la grille, en rangées.
const RANGEES_VISIBLES: f32 = 7.0;
/// Combien de récents on garde : deux rangées.
const RECENTS: usize = 2 * COLONNES;
/// Combien de résultats de recherche on montre au plus.
const RESULTATS: usize = 12 * COLONNES;

/// Le bouton sourire et son panneau, ouvert au-dessus de lui : rend l'emoji
/// choisi. Le panneau se ferme au choix — Maj le garde ouvert —, d'un clic
/// ailleurs ou d'Échap.
pub fn bouton(ui: &mut Ui, bulle: &str) -> Option<String> {
    let reponse = c::icon_button(ui, Icon::Sourire, bulle);
    let mut choisi = None;
    egui::Popup::from_toggle_button_response(&reponse)
        .align(egui::RectAlign::TOP_END)
        .gap(espace::S)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            choisi = panneau(ui);
            if choisi.is_some() && !ui.input(|i| i.modifiers.shift) {
                ui.close();
            }
        });
    choisi
}

/// Ce qui se retient d'une session à l'autre.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct Memoire {
    /// Les derniers choisis, le plus récent d'abord, teinte comprise.
    recents: Vec<String>,
    /// La teinte de peau : 0 (par défaut) à 5 (foncée).
    teinte: u8,
}

/// Ce qui ne dure que le temps d'ouvrir le panneau.
#[derive(Clone, Default)]
struct Etat {
    recherche: String,
    /// La rangée des teintes est-elle dépliée ?
    teintes: bool,
    /// La rangée où défiler (un onglet de catégorie cliqué).
    aller_a: Option<usize>,
    /// La dernière image où le panneau s'est montré : s'il ne s'est pas
    /// montré à la précédente, il vient de s'ouvrir.
    derniere_image: u64,
    survol: Option<&'static Emoji>,
}

/// Le panneau seul : rend l'emoji choisi.
pub fn panneau(ui: &mut Ui) -> Option<String> {
    let id = Id::new("ki-ui-selecteur-emoji");
    let mut memoire = ui.data_mut(|d| d.get_persisted_mut_or_default::<Memoire>(id).clone());
    let mut etat = ui.data_mut(|d| d.get_temp_mut_or_default::<Etat>(id).clone());
    let image = ui.ctx().cumulative_frame_nr();
    let vient_de_s_ouvrir = etat.derniere_image + 1 < image;
    if vient_de_s_ouvrir {
        etat.recherche.clear();
        etat.teintes = false;
    }
    etat.derniere_image = image;
    let teinte = teinte_de(memoire.teinte);
    let largeur = COLONNES as f32 * CASE;
    ui.set_width(largeur);
    ui.spacing_mut().item_spacing = Vec2::new(espace::S, espace::S);
    let mut choisi: Option<&'static Emoji> = None;

    // La recherche, et la teinte de peau à côté.
    ui.horizontal(|ui| {
        let champ = ui.add(
            c::text_field(&mut etat.recherche, "Chercher un emoji", false)
                .desired_width(largeur - CASE - espace::S),
        );
        if vient_de_s_ouvrir {
            champ.request_focus();
        }
        // Entrée : le premier résultat.
        if champ.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            choisi = chercher(&plier(etat.recherche.trim())).first().map(|&e| avec_teinte(e, teinte));
        }
        let main = emojis::get("✋").map(|e| avec_teinte(e, teinte));
        if let Some(main) = main {
            if case(ui, main.as_str()).on_hover_text("Teinte de peau").clicked() {
                etat.teintes = !etat.teintes;
            }
        }
    });
    if etat.teintes {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for n in 0..6 {
                let Some(main) = emojis::get("✋").map(|e| avec_teinte(e, teinte_de(n))) else { continue };
                if case(ui, main.as_str()).clicked() {
                    memoire.teinte = n;
                    etat.teintes = false;
                }
            }
        });
    }

    let requete = plier(etat.recherche.trim());
    let catalogue = catalogue();
    // Les onglets des catégories : un clic y défile.
    if requete.is_empty() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for &(groupe, ligne) in &catalogue.debuts {
                let (nom, icone) = groupe_fr(groupe);
                if case(ui, icone).on_hover_text(nom).clicked() {
                    let decalage = usize::from(!memoire.recents.is_empty()) * (1 + memoire.recents.len().div_ceil(COLONNES));
                    etat.aller_a = Some(decalage + ligne);
                }
            }
        });
    }
    c::hairline(ui);

    // La grille : les récents puis les catégories, ou les résultats.
    let recents: Vec<&'static Emoji> = memoire.recents.iter().filter_map(|r| emojis::get(r)).collect();
    let resultats;
    let mut lignes: Vec<Ligne<'_>> = Vec::new();
    if requete.is_empty() {
        if !recents.is_empty() {
            lignes.push(Ligne::Titre("Récents"));
            lignes.extend(recents.chunks(COLONNES).map(|r| Ligne::Emoji(r, false)));
        }
        lignes.extend(catalogue.lignes.iter().cloned());
    } else {
        resultats = chercher(&requete);
        lignes.extend(resultats.chunks(COLONNES).map(|r| Ligne::Emoji(r, true)));
    }
    // Une hauteur fixe : posée dans une zone qui défile déjà, une grille
    // prendrait sinon la place qui reste à l'écran, une rangée parfois.
    let mut zone = ScrollArea::vertical()
        .id_salt("ki-ui-selecteur-emoji-grille")
        .max_height(RANGEES_VISIBLES * CASE)
        .min_scrolled_height(RANGEES_VISIBLES * CASE)
        .auto_shrink([false, true]);
    if let Some(n) = etat.aller_a.take() {
        zone = zone.vertical_scroll_offset(n as f32 * CASE);
    }
    if lignes.is_empty() {
        ui.add_space(espace::M);
        c::hint(ui, "Aucun emoji ne s'appelle comme ça.");
        ui.add_space(espace::M);
    } else {
        // Des rangées collées : show_rows compte leur hauteur, espacement
        // compris.
        ui.spacing_mut().item_spacing.y = 0.0;
        zone.show_rows(ui, CASE, lignes.len(), |ui, rangees| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            for ligne in &lignes[rangees] {
                match ligne {
                    Ligne::Titre(titre) => {
                        let (rect, _) = ui.allocate_exact_size(Vec2::new(largeur, CASE), Sense::hover());
                        ui.painter().text(
                            rect.left_center() + Vec2::new(espace::XS, 0.0),
                            Align2::LEFT_CENTER,
                            *titre,
                            FontId::proportional(texte::PETIT),
                            couleur::TEXT_DIM,
                        );
                    }
                    Ligne::Emoji(rangee, teinter) => {
                        ui.horizontal(|ui| {
                            for &e in *rangee {
                                let e = if *teinter { avec_teinte(e, teinte) } else { e };
                                let reponse = case(ui, e.as_str());
                                if reponse.hovered() {
                                    etat.survol = Some(e);
                                }
                                if reponse.clicked() {
                                    choisi = Some(e);
                                }
                            }
                        });
                    }
                }
            }
        });
    }

    // Le pied : l'emoji survolé, son nom et son code court.
    ui.spacing_mut().item_spacing.y = espace::S;
    c::hairline(ui);
    let (pied, _) = ui.allocate_exact_size(Vec2::new(largeur, 32.0), Sense::hover());
    if let Some(e) = etat.survol {
        let vignette = egui::Rect::from_min_size(pied.min, Vec2::splat(pied.height()));
        if !crate::emoji::peindre_vignette(ui.painter(), vignette.shrink(2.0), e.as_str()) {
            ui.painter().text(vignette.center(), Align2::CENTER_CENTER, e.as_str(), FontId::proportional(22.0), couleur::TEXT);
        }
        let x = vignette.right() + espace::S;
        let nom = nom_fr(e).unwrap_or_else(|| e.name());
        let code = e.shortcode().map(|s| format!("  :{s}:")).unwrap_or_default();
        let mut job = egui::text::LayoutJob::default();
        job.append(nom, 0.0, egui::TextFormat::simple(FontId::proportional(texte::COURANT), couleur::TEXT));
        job.append(&code, 0.0, egui::TextFormat::simple(FontId::proportional(texte::PETIT), couleur::TEXT_FAINT));
        job.wrap.max_width = pied.right() - x;
        job.wrap.max_rows = 1;
        let galley = ui.painter().layout_job(job);
        let y = pied.center().y - galley.size().y / 2.0;
        ui.painter().galley(egui::pos2(x, y), galley, couleur::TEXT);
    } else {
        ui.painter().text(
            pied.left_center(),
            Align2::LEFT_CENTER,
            "Survole un emoji pour voir son nom",
            FontId::proportional(texte::PETIT),
            couleur::TEXT_FAINT,
        );
    }

    if let Some(e) = choisi {
        let e = e.as_str().to_owned();
        memoire.recents.retain(|r| *r != e);
        memoire.recents.insert(0, e.clone());
        memoire.recents.truncate(RECENTS);
        ui.data_mut(|d| {
            *d.get_persisted_mut_or_default::<Memoire>(id) = memoire;
            *d.get_temp_mut_or_default::<Etat>(id) = etat;
        });
        return Some(e);
    }
    ui.data_mut(|d| {
        *d.get_persisted_mut_or_default::<Memoire>(id) = memoire;
        *d.get_temp_mut_or_default::<Etat>(id) = etat;
    });
    None
}

/// Une case de la grille : l'emoji en vignette couleur (ou en texte, sans
/// police emoji), surligné au survol.
fn case(ui: &mut Ui, emoji: &str) -> egui::Response {
    let (rect, reponse) = ui.allocate_exact_size(Vec2::splat(CASE), Sense::click());
    if ui.is_rect_visible(rect) {
        if reponse.hovered() {
            ui.painter().rect_filled(rect.shrink(1.0), CornerRadius::same(rayon::M), couleur::BG_HOVER);
        }
        if !crate::emoji::peindre_vignette(ui.painter(), rect.shrink(6.0), emoji) {
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, emoji, FontId::proportional(22.0), couleur::TEXT);
        }
    }
    reponse
}

/// Une rangée de la grille. `bool` : l'emoji prend-il la teinte choisie
/// (les récents gardent la leur).
#[derive(Clone)]
enum Ligne<'a> {
    Titre(&'static str),
    Emoji(&'a [&'static Emoji], bool),
}

// ---------------------------------------------------------------------
// Le catalogue
// ---------------------------------------------------------------------

struct Catalogue {
    /// Les rangées : le titre de chaque catégorie, puis ses emoji.
    lignes: Vec<Ligne<'static>>,
    /// La rangée où commence chaque catégorie.
    debuts: Vec<(Group, usize)>,
    /// Chaque emoji, et ce qui le fait trouver : ses mots français et
    /// anglais, pliés (minuscules, sans accents).
    entrees: Vec<Entree>,
}

struct Entree {
    emoji: &'static Emoji,
    francais: Vec<String>,
    anglais: Vec<String>,
    /// Son rang dans la liste française (les courants d'abord), ou le
    /// dernier s'il n'y est pas.
    priorite: usize,
}

/// Le catalogue, fait une fois : sur une machine qui peint les emoji en
/// couleur, ceux que sa police ne connaît pas en sont écartés (plus récents
/// qu'elle, ou les drapeaux, que Windows ne dessine pas).
fn catalogue() -> &'static Catalogue {
    static CATALOGUE: OnceLock<Catalogue> = OnceLock::new();
    CATALOGUE.get_or_init(|| {
        let couleur = crate::emoji::police().is_some();
        let mots = mots_francais();
        let mut lignes = Vec::new();
        let mut debuts = Vec::new();
        let mut entrees = Vec::new();
        for groupe in Group::iter() {
            let membres: Vec<&'static Emoji> =
                groupe.emojis().filter(|e| !couleur || crate::emoji::peignable(e.as_str())).collect();
            if membres.is_empty() {
                continue;
            }
            debuts.push((groupe, lignes.len()));
            lignes.push(Ligne::Titre(groupe_fr(groupe).0));
            let membres: &'static [&'static Emoji] = Box::leak(membres.into_boxed_slice());
            lignes.extend(membres.chunks(COLONNES).map(|r| Ligne::Emoji(r, true)));
            for &emoji in membres {
                let (priorite, francais) = match mots.get(sans_selecteur(emoji.as_str()).as_str()) {
                    Some(&(rang, m)) => (rang, m.split(',').map(|p| plier(p.trim())).collect()),
                    None => (usize::MAX, Vec::new()),
                };
                let mut anglais = vec![plier(emoji.name())];
                anglais.extend(emoji.shortcodes().map(|s| plier(&s.replace('_', " "))));
                entrees.push(Entree { emoji, francais, anglais, priorite });
            }
        }
        Catalogue { lignes, debuts, entrees }
    })
}

/// Les emoji qui répondent à `requete` (déjà pliée). D'abord une
/// expression entière (« feu »), puis une expression qui commence par elle
/// (« cœur rouge » pour « cœur »), puis un mot exact pris au milieu (« cœur
/// en feu », « yeux en cœur »), puis un début de mot, puis un bout de mot — le français avant l'anglais à chaque
/// fois ; à égalité, les courants d'abord (l'ordre de la liste
/// française), puis l'ordre du catalogue. « feu » donne 🔥 avant 🍀
/// (« feuilles »).
fn chercher(requete: &str) -> Vec<&'static Emoji> {
    if requete.is_empty() {
        return Vec::new();
    }
    fn mots(phrases: &[String]) -> impl Iterator<Item = &str> {
        phrases.iter().flat_map(|p| std::iter::once(p.as_str()).chain(p.split([' ', '-', '\''])))
    }
    let phrase = |phrases: &[String]| phrases.iter().any(|p| p == requete);
    let en_tete = |phrases: &[String]| phrases.iter().any(|p| p.split([' ', '-', '\'']).next() == Some(requete));
    let exact = |phrases: &[String]| mots(phrases).any(|m| m == requete);
    let debut = |phrases: &[String]| mots(phrases).any(|m| m.starts_with(requete));
    let dedans = |phrases: &[String]| phrases.iter().any(|p| p.contains(requete));
    let mut trouves: Vec<(u8, usize, usize, &'static Emoji)> = catalogue()
        .entrees
        .iter()
        .enumerate()
        .filter_map(|(rang, e)| {
            let score = [
                phrase(&e.francais),
                en_tete(&e.francais),
                exact(&e.francais),
                phrase(&e.anglais),
                exact(&e.anglais),
                debut(&e.francais),
                debut(&e.anglais),
                dedans(&e.francais),
                dedans(&e.anglais),
            ]
            .iter()
            .position(|&oui| oui)?;
            Some((score as u8, e.priorite, rang, e.emoji))
        })
        .collect();
    trouves.sort_by_key(|&(score, priorite, rang, _)| (score, priorite, rang));
    trouves.into_iter().take(RESULTATS).map(|(_, _, _, e)| e).collect()
}

fn teinte_de(n: u8) -> SkinTone {
    match n {
        1 => SkinTone::Light,
        2 => SkinTone::MediumLight,
        3 => SkinTone::Medium,
        4 => SkinTone::MediumDark,
        5 => SkinTone::Dark,
        _ => SkinTone::Default,
    }
}

fn avec_teinte(emoji: &'static Emoji, teinte: SkinTone) -> &'static Emoji {
    if teinte == SkinTone::Default {
        return emoji;
    }
    emoji.with_skin_tone(teinte).unwrap_or(emoji)
}

fn groupe_fr(groupe: Group) -> (&'static str, &'static str) {
    match groupe {
        Group::SmileysAndEmotion => ("Smileys et émotions", "😀"),
        Group::PeopleAndBody => ("Personnes", "👋"),
        Group::AnimalsAndNature => ("Animaux et nature", "🐻"),
        Group::FoodAndDrink => ("Nourriture et boissons", "🍔"),
        Group::TravelAndPlaces => ("Voyages et lieux", "🚗"),
        Group::Activities => ("Activités", "⚽"),
        Group::Objects => ("Objets", "💡"),
        Group::Symbols => ("Symboles", "🔣"),
        Group::Flags => ("Drapeaux", "🏁"),
    }
}

/// Le nom français d'un emoji, s'il en a un : le premier de ses mots.
fn nom_fr(emoji: &Emoji) -> Option<&'static str> {
    mots_francais().get(sans_selecteur(emoji.as_str()).as_str()).and_then(|(_, m)| m.split(',').next()).map(str::trim)
}

/// Un emoji sans ses sélecteurs de présentation : la même clé, qu'on l'écrive
/// avec ou sans.
fn sans_selecteur(emoji: &str) -> String {
    emoji.chars().filter(|&c| c != '\u{FE0F}' && c != '\u{FE0E}').collect()
}

/// En minuscules, sans accents : « Cœur » et « coeur » se trouvent pareil.
fn plier(texte: &str) -> String {
    let mut plie = String::with_capacity(texte.len());
    for c in texte.chars().flat_map(char::to_lowercase) {
        match c {
            'à' | 'â' | 'ä' | 'á' | 'ã' => plie.push('a'),
            'é' | 'è' | 'ê' | 'ë' => plie.push('e'),
            'î' | 'ï' | 'í' | 'ì' => plie.push('i'),
            'ô' | 'ö' | 'ó' | 'ò' | 'õ' => plie.push('o'),
            'ù' | 'û' | 'ü' | 'ú' => plie.push('u'),
            'ç' => plie.push('c'),
            'ñ' => plie.push('n'),
            'ÿ' => plie.push('y'),
            'œ' => plie.push_str("oe"),
            'æ' => plie.push_str("ae"),
            '’' => plie.push('\''),
            c => plie.push(c),
        }
    }
    plie
}

/// Les mots français de chaque emoji qui en a, et son rang dans la liste.
fn mots_francais() -> &'static HashMap<String, (usize, &'static str)> {
    static MOTS: OnceLock<HashMap<String, (usize, &'static str)>> = OnceLock::new();
    MOTS.get_or_init(|| MOTS_FRANCAIS.iter().enumerate().map(|(rang, &(e, m))| (sans_selecteur(e), (rang, m))).collect())
}

/// Les emoji courants et leurs mots français — le premier sert de nom.
/// Les autres se trouvent par leur nom anglais et leur code court.
const MOTS_FRANCAIS: &[(&str, &str)] = &[
    // Smileys
    ("😀", "visage souriant, sourire, content, heureux"),
    ("😃", "grand sourire, content, joie"),
    ("😄", "sourire aux yeux rieurs, rire, joie"),
    ("😁", "sourire jusqu'aux oreilles, dents"),
    ("😂", "larmes de joie, mdr, lol, rire, pleurer de rire"),
    ("🤣", "mort de rire, mdr, ptdr, lol, rire"),
    ("😆", "rire aux éclats"),
    ("😅", "rire gêné, sueur, ouf"),
    ("🙂", "léger sourire"),
    ("🙃", "à l'envers, ironie"),
    ("😉", "clin d'œil"),
    ("😊", "sourire rougissant, content"),
    ("😇", "ange, innocent, auréole"),
    ("🥰", "amoureux, cœurs, adorer"),
    ("😍", "yeux en cœur, amoureux, j'adore"),
    ("🤩", "étoiles dans les yeux, waouh, génial"),
    ("😘", "bisou, baiser"),
    ("😋", "miam, délicieux, langue"),
    ("😛", "tirer la langue, taquin"),
    ("😜", "clin d'œil et langue, taquin"),
    ("🤪", "fou, dingue, zinzin"),
    ("😝", "langue et yeux plissés, beurk"),
    ("🤑", "argent, riche, fric"),
    ("🤗", "câlin, accolade"),
    ("🤭", "oups, main sur la bouche, rire caché"),
    ("🤫", "chut, silence, secret"),
    ("🤔", "réfléchir, hmm, penser, douter"),
    ("🤐", "bouche cousue, secret"),
    ("🤨", "sourcil levé, sceptique, louche"),
    ("😐", "neutre, blasé"),
    ("😑", "inexpressif, blasé"),
    ("😶", "sans voix, muet"),
    ("😏", "sourire en coin, narquois"),
    ("😒", "pas content, bof, blasé"),
    ("🙄", "yeux au ciel, lever les yeux"),
    ("😬", "grimace, malaise, oups"),
    ("😌", "soulagé, apaisé"),
    ("😔", "pensif, triste, déçu"),
    ("😪", "fatigué, sommeil"),
    ("🤤", "bave, envie, miam"),
    ("😴", "dormir, dodo, sommeil"),
    ("😷", "masque, malade"),
    ("🤒", "fièvre, malade, thermomètre"),
    ("🤢", "nausée, écœuré, beurk"),
    ("🤮", "vomir, dégoût, beurk"),
    ("🥵", "chaud, canicule, transpirer"),
    ("🥶", "froid, gelé, glacé"),
    ("😵", "étourdi, ko, sonné"),
    ("🤯", "tête qui explose, choqué"),
    ("🤠", "cowboy"),
    ("🥳", "fête, anniversaire, teuf"),
    ("😎", "cool, lunettes de soleil, classe"),
    ("🤓", "intello, geek, lunettes"),
    ("😕", "confus, perplexe"),
    ("😟", "inquiet, soucieux"),
    ("🙁", "pas content, triste"),
    ("☹️", "triste, mécontent"),
    ("😮", "bouche ouverte, surpris, oh"),
    ("😯", "étonné, surpris"),
    ("😲", "stupéfait, choqué"),
    ("😳", "gêné, rougir, choqué"),
    ("🥺", "yeux suppliants, s'il te plaît, mignon"),
    ("😦", "consterné"),
    ("😧", "angoissé"),
    ("😨", "peur, effrayé"),
    ("😰", "anxieux, sueur froide"),
    ("😥", "déçu mais soulagé"),
    ("😢", "pleurer, larme, triste"),
    ("😭", "pleurer fort, sanglots, triste"),
    ("😱", "cri, horreur, peur"),
    ("😖", "frustré, confus"),
    ("😣", "persévérer, effort"),
    ("😞", "déçu, déprimé"),
    ("😓", "sueur, dur"),
    ("😩", "épuisé, las"),
    ("😫", "crevé, fatigué"),
    ("🥱", "bâiller, ennui, fatigue"),
    ("😤", "souffler, rage, fier"),
    ("😡", "colère, rouge, énervé, fâché"),
    ("😠", "fâché, colère"),
    ("🤬", "jurer, insultes, rage"),
    ("😈", "diable souriant, malicieux"),
    ("👿", "diable en colère, démon"),
    ("💀", "crâne, mort, dead"),
    ("☠️", "tête de mort, poison, pirate"),
    ("💩", "caca, crotte"),
    ("🤡", "clown"),
    ("👻", "fantôme"),
    ("👽", "extraterrestre, alien"),
    ("🤖", "robot, bot"),
    ("🫡", "salut militaire, respect"),
    ("🫠", "fondre, gêne"),
    ("🥲", "sourire ému, larme de joie"),
    // Cœurs et symboles d'émotion
    ("❤️", "cœur rouge, amour, love"),
    ("🧡", "cœur orange"),
    ("💛", "cœur jaune"),
    ("💚", "cœur vert"),
    ("💙", "cœur bleu"),
    ("💜", "cœur violet"),
    ("🖤", "cœur noir"),
    ("🤍", "cœur blanc"),
    ("🤎", "cœur marron"),
    ("💔", "cœur brisé, rupture"),
    ("❤️‍🔥", "cœur en feu, passion"),
    ("💕", "deux cœurs, amour"),
    ("💖", "cœur étincelant"),
    ("💯", "cent, parfait, 100"),
    ("💢", "colère, énervement"),
    ("💥", "explosion, boum"),
    ("💫", "étourdi, étoile filante"),
    ("💦", "gouttes, sueur, eau"),
    ("💨", "vite, souffle, fuir"),
    ("💬", "bulle, message, parler"),
    ("💤", "dodo, sommeil, zzz"),
    // Mains et corps
    ("👋", "salut, coucou, bonjour, au revoir, main"),
    ("🤚", "dos de la main"),
    ("✋", "main levée, stop, high five, tope là"),
    ("🖐️", "main ouverte, cinq"),
    ("👌", "ok, parfait"),
    ("🤌", "doigts pincés, mais quoi"),
    ("✌️", "victoire, paix"),
    ("🤞", "doigts croisés, chance"),
    ("🤟", "je t'aime"),
    ("🤘", "rock, métal, cornes"),
    ("🤙", "appelle-moi, shaka"),
    ("👈", "à gauche, montrer"),
    ("👉", "à droite, montrer"),
    ("👆", "en haut"),
    ("👇", "en bas"),
    ("☝️", "index levé, attention"),
    ("👍", "pouce levé, ok, oui, d'accord, bien, j'aime"),
    ("👎", "pouce baissé, non, nul, j'aime pas"),
    ("✊", "poing levé"),
    ("👊", "poing, check"),
    ("🤛", "check à gauche, poing"),
    ("🤜", "check à droite, poing"),
    ("👏", "applaudir, bravo, clap"),
    ("🙌", "mains levées, hourra, youpi"),
    ("👐", "mains ouvertes"),
    ("🤝", "poignée de main, accord, deal"),
    ("🙏", "mains jointes, merci, prière, s'il te plaît, svp"),
    ("💪", "biceps, muscle, force, costaud"),
    ("🧠", "cerveau, intelligent"),
    ("👀", "yeux, regarder, mater"),
    ("👁️", "œil"),
    ("👅", "langue"),
    ("👄", "bouche, bisou"),
    ("🤷", "je sais pas, bof, haussement d'épaules"),
    ("🤦", "facepalm, désespoir, consterné"),
    ("🙋", "lever la main, moi"),
    ("🙅", "non, interdit, refus"),
    ("🙆", "ok, d'accord"),
    // Animaux et nature
    ("🐶", "chien"),
    ("🐱", "chat"),
    ("🐭", "souris"),
    ("🐰", "lapin"),
    ("🦊", "renard"),
    ("🐻", "ours"),
    ("🐼", "panda"),
    ("🐨", "koala"),
    ("🐯", "tigre"),
    ("🦁", "lion"),
    ("🐮", "vache"),
    ("🐷", "cochon"),
    ("🐸", "grenouille, crapaud"),
    ("🐵", "singe"),
    ("🙈", "singe qui ne voit rien, honte"),
    ("🙉", "singe qui n'entend rien"),
    ("🙊", "singe qui ne dit rien, oups"),
    ("🐔", "poule"),
    ("🐧", "pingouin, manchot"),
    ("🐦", "oiseau"),
    ("🦅", "aigle"),
    ("🦉", "hibou, chouette"),
    ("🐍", "serpent"),
    ("🐢", "tortue"),
    ("🦄", "licorne"),
    ("🐝", "abeille"),
    ("🦋", "papillon"),
    ("🐟", "poisson"),
    ("🐬", "dauphin"),
    ("🦈", "requin"),
    ("🐙", "pieuvre"),
    ("🌹", "rose, fleur"),
    ("🌸", "fleur de cerisier"),
    ("🌻", "tournesol"),
    ("🌳", "arbre"),
    ("🍀", "trèfle à quatre feuilles, chance"),
    ("🔥", "feu, flamme, chaud, incendie"),
    ("🌈", "arc-en-ciel"),
    ("☀️", "soleil"),
    ("🌙", "lune, croissant de lune"),
    ("⭐", "étoile"),
    ("🌟", "étoile brillante"),
    ("✨", "étincelles, brillant, magie"),
    ("⚡", "éclair, électrique, rapide"),
    ("❄️", "flocon, neige, froid"),
    ("🌊", "vague, mer"),
    ("💧", "goutte, eau"),
    // Nourriture
    ("🍕", "pizza"),
    ("🍔", "burger, hamburger"),
    ("🍟", "frites"),
    ("🌭", "hot-dog"),
    ("🌮", "tacos"),
    ("🍣", "sushi"),
    ("🍜", "ramen, nouilles"),
    ("🍩", "donut, beignet"),
    ("🍪", "cookie, biscuit"),
    ("🎂", "gâteau d'anniversaire"),
    ("🍰", "part de gâteau"),
    ("🍫", "chocolat"),
    ("🍿", "pop-corn"),
    ("🍎", "pomme"),
    ("🍌", "banane"),
    ("🍓", "fraise"),
    ("🍉", "pastèque"),
    ("🥐", "croissant"),
    ("🥖", "baguette, pain"),
    ("🧀", "fromage"),
    ("🍺", "bière"),
    ("🍻", "santé, trinquer, bières"),
    ("🍷", "vin"),
    ("🥂", "champagne, santé"),
    ("☕", "café"),
    ("🥤", "soda, boisson"),
    // Activités
    ("⚽", "ballon de foot, football"),
    ("🏀", "basket"),
    ("🏈", "football américain"),
    ("🎾", "tennis"),
    ("🏐", "volley"),
    ("🏓", "ping-pong"),
    ("🎮", "manette, jeu vidéo, jouer, gaming"),
    ("🕹️", "joystick, arcade"),
    ("🎯", "cible, dans le mille"),
    ("🎲", "dé, hasard"),
    ("♟️", "échecs, pion"),
    ("🏆", "trophée, victoire, gagné, champion"),
    ("🥇", "médaille d'or, premier"),
    ("🥈", "médaille d'argent, deuxième"),
    ("🥉", "médaille de bronze, troisième"),
    ("🏅", "médaille"),
    ("🎉", "cotillons, fête, bravo, confettis, youpi"),
    ("🎊", "confettis, fête"),
    ("🎁", "cadeau"),
    ("🎈", "ballon, fête"),
    ("🎵", "note de musique, musique"),
    ("🎶", "notes de musique, musique"),
    ("🎧", "casque audio, musique"),
    ("🎤", "micro, chanter, karaoké"),
    ("🎬", "clap de cinéma, film"),
    ("📸", "photo, flash"),
    ("🎨", "palette, peinture, art"),
    // Voyages
    ("🚗", "voiture"),
    ("🏎️", "voiture de course, rapide"),
    ("🚀", "fusée, décollage"),
    ("✈️", "avion"),
    ("🚂", "train"),
    ("🚲", "vélo"),
    ("🏠", "maison"),
    ("🏰", "château"),
    ("🌍", "terre, monde, planète"),
    ("⏰", "réveil, alarme, heure"),
    ("⌛", "sablier, attendre"),
    // Objets
    ("💻", "ordinateur portable, pc"),
    ("🖥️", "écran, ordinateur, pc"),
    ("⌨️", "clavier"),
    ("🖱️", "souris d'ordinateur"),
    ("📱", "téléphone, portable, smartphone"),
    ("📞", "combiné, appel, téléphone"),
    ("💡", "ampoule, idée"),
    ("💰", "sac d'argent, argent"),
    ("💸", "argent qui s'envole, dépense"),
    ("💎", "diamant, gemme"),
    ("🔑", "clé"),
    ("🔒", "cadenas fermé, verrouillé"),
    ("🔓", "cadenas ouvert, déverrouillé"),
    ("🔨", "marteau"),
    ("🛠️", "outils, réparer"),
    ("⚙️", "engrenage, réglages"),
    ("💣", "bombe"),
    ("🔫", "pistolet à eau"),
    ("🗡️", "dague, épée"),
    ("🛡️", "bouclier"),
    ("📦", "colis, paquet"),
    ("📝", "mémo, note, écrire"),
    ("📌", "punaise, épingle"),
    ("📎", "trombone"),
    ("✏️", "crayon"),
    ("📅", "calendrier, date"),
    ("📈", "graphique en hausse, hausse"),
    ("📉", "graphique en baisse, baisse"),
    ("🔔", "cloche, notification"),
    ("🔕", "cloche barrée, silence"),
    ("📢", "haut-parleur, annonce"),
    ("🔊", "volume fort, son"),
    ("🔇", "son coupé, muet"),
    ("💊", "pilule, médicament"),
    ("🗑️", "corbeille, poubelle"),
    // Symboles
    ("✅", "coche, validé, ok, fait"),
    ("☑️", "case cochée"),
    ("✔️", "coche"),
    ("❌", "croix, non, faux, erreur"),
    ("❓", "point d'interrogation, question, quoi"),
    ("❗", "point d'exclamation, attention"),
    ("‼️", "double exclamation"),
    ("⁉️", "exclamation et question"),
    ("⚠️", "attention, danger, avertissement"),
    ("🚫", "interdit, non"),
    ("⛔", "sens interdit, stop"),
    ("🔴", "rond rouge"),
    ("🟢", "rond vert"),
    ("🔵", "rond bleu"),
    ("⬆️", "flèche vers le haut"),
    ("⬇️", "flèche vers le bas"),
    ("➡️", "flèche vers la droite"),
    ("⬅️", "flèche vers la gauche"),
    ("🔄", "flèches en boucle, recharger"),
    ("➕", "plus"),
    ("➖", "moins"),
    ("♾️", "infini"),
    ("🆗", "ok"),
    ("🆕", "nouveau"),
    ("🆒", "cool"),
    ("🆘", "sos, aide"),
    ("🔞", "interdit aux moins de 18 ans"),
    // Drapeaux
    ("🏁", "drapeau à damier, arrivée, course"),
    ("🚩", "drapeau rouge, alerte"),
    ("🏳️", "drapeau blanc, abandon"),
    ("🏴‍☠️", "drapeau pirate"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plier_ote_majuscules_et_accents() {
        assert_eq!(plier("Cœur Éclaté à Noël"), "coeur eclate a noel");
    }

    /// En français comme en anglais, et le français d'abord.
    #[test]
    fn chercher_en_francais_et_en_anglais() {
        let premier = |q: &str| chercher(&plier(q)).first().map(|e| e.as_str().to_owned());
        assert_eq!(premier("feu").as_deref(), Some("🔥"));
        assert_eq!(premier("fire").as_deref(), Some("🔥"));
        assert_eq!(premier("coeur").as_deref(), Some("❤\u{fe0f}"));
        assert_eq!(premier("Cœur").as_deref(), Some("❤\u{fe0f}"));
        assert_eq!(premier("mdr").as_deref(), Some("😂"));
        assert_eq!(premier("joy").as_deref(), Some("😂"));
        assert_eq!(premier("pouce").as_deref(), Some("👍"));
        assert!(chercher(&plier("zzzqqq")).is_empty());
    }

    #[test]
    fn les_mots_francais_designent_de_vrais_emoji() {
        for (e, _) in MOTS_FRANCAIS {
            assert!(emojis::get(e).is_some(), "« {e} » n'est pas un emoji connu");
        }
    }

    #[test]
    fn le_catalogue_a_ses_categories() {
        let catalogue = catalogue();
        assert!(catalogue.debuts.len() >= 8, "{} catégories", catalogue.debuts.len());
        assert!(catalogue.entrees.len() > 1000, "{} emoji", catalogue.entrees.len());
        for &(_, ligne) in &catalogue.debuts {
            assert!(matches!(catalogue.lignes[ligne], Ligne::Titre(_)));
        }
    }

    #[test]
    fn une_teinte_s_applique_a_qui_en_a() {
        let pouce = emojis::get("👍").unwrap();
        assert_eq!(avec_teinte(pouce, SkinTone::Medium).as_str(), "👍🏽");
        let feu = emojis::get("🔥").unwrap();
        assert_eq!(avec_teinte(feu, SkinTone::Medium).as_str(), "🔥");
    }
}
