//! Le thème de ki-chat : ce qui vient de ki-ui — la palette, l'allure
//! d'ensemble — sous ses noms de toujours, et ce qui n'appartient qu'à
//! ki-chat : la couleur d'un membre selon son rôle, l'icône de la fenêtre.

use eframe::egui::{self, Color32};

// ---------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------

/// La palette vit dans les jetons de ki-ui : une seule source pour toutes
/// les applis ki-*. Les noms restent ceux de toujours.
pub use ki_ui::jetons::couleur::*;
/// Les outils de couleur de ki-ui, sous leurs noms de toujours.
pub use ki_ui::jetons::couleur::{melanger as mix, pour_pseudo as color_for, translucide as alpha};

/// Couleur d'un membre : celle que son rôle lui donne, sinon le hachage de
/// son pseudo.
///
/// Le repli n'est pas un pis-aller : un serveur sans rôles colorés doit
/// continuer à afficher des pseudos distincts, comme avant.
pub fn member_color(color: Option<u32>, username: &str) -> Color32 {
    match color {
        Some(rgb) => Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8),
        None => color_for(username),
    }
}

/// L'allure de ki-chat — polices, palette, espacements, thème sombre —,
/// installée par ki-ui : la même pour toutes les applis ki-*.
pub use ki_ui::style::installer as install;

// ---------------------------------------------------------------------
// Icône de fenêtre
// ---------------------------------------------------------------------

/// Icône de fenêtre (barre des tâches, alt-tab) : le même pictogramme que
/// celui gravé dans l'exécutable, rendu pixel par pixel par `appicon` — pas
/// de fichier à embarquer, pas de carré blanc par défaut.
pub fn app_icon() -> egui::IconData {
    const S: u32 = 64;
    egui::IconData {
        rgba: crate::appicon::render(S),
        width: S,
        height: S,
    }
}

// ---------------------------------------------------------------------
// Les copies de la palette
// ---------------------------------------------------------------------

/// La palette de ki-chat existe en plusieurs exemplaires : ki-ui (le PC),
/// ki-core (l'appli mobile, en Rust, qui ne voit pas egui), la feuille de
/// la page des invités et celle de l'appli mobile. Ces tests cassent dès
/// qu'une copie s'écarte de ki-ui : une même personne changerait de
/// couleur selon l'appareil, une page changerait de ton selon la porte.
/// Ils vivent ici, le seul endroit qui voit à la fois ki-ui et ki-core.
#[cfg(test)]
mod copies {
    use super::*;

    const PORTE_CSS: &str = include_str!("../../server/src/porte.css");
    const PORTE_HTML: &str = include_str!("../../server/src/porte.html");
    const MOBILE: &str = include_str!("../../mobile/ui/index.html");
    const ICONE_ANDROID: &str = include_str!("../../mobile/icons/android/values/ic_launcher_background.xml");

    fn rgb(c: u32) -> Color32 {
        Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
    }

    fn hex(c: Color32) -> String {
        format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
    }

    /// « #00d26a » (ou « #00D26A ») en couleur.
    fn depuis_hex(h: &str) -> Color32 {
        let v = u32::from_str_radix(h.trim().trim_start_matches('#'), 16)
            .unwrap_or_else(|_| panic!("« {h} » n'est pas une couleur #rrggbb"));
        rgb(v)
    }

    /// Les variables `--nom: valeur` d'une feuille de style.
    fn variables(source: &str) -> Vec<(String, String)> {
        source
            .split([';', '{', '}', '\n'])
            .filter_map(|d| {
                let (nom, valeur) = d.trim().strip_prefix("--")?.split_once(':')?;
                Some((format!("--{}", nom.trim()), valeur.trim().to_owned()))
            })
            .collect()
    }

    /// Toutes les couleurs `#rrggbb` écrites dans un fichier.
    fn couleurs_ecrites(source: &str) -> Vec<String> {
        let o = source.as_bytes();
        let hexa = |i: usize| o.get(i).is_some_and(u8::is_ascii_hexdigit);
        (0..o.len())
            .filter(|&i| o[i] == b'#' && (1..=6).all(|k| hexa(i + k)) && !hexa(i + 7))
            .map(|i| source[i..i + 7].to_owned())
            .collect()
    }

    /// Les variables de `attendues` portent les couleurs de ki-ui ; toute
    /// autre couleur écrite dans le fichier est de la palette, ou figure
    /// dans `propres`, avec sa raison d'être.
    fn suit_la_palette(fichier: &str, source: &str, attendues: &[(&str, Color32)], propres: &[(&str, &str)]) {
        let declarees = variables(source);
        for (variable, couleur) in attendues {
            let valeur = declarees
                .iter()
                .find(|(n, _)| n == variable)
                .map(|(_, v)| v.as_str())
                .unwrap_or_else(|| panic!("{fichier} : la variable {variable} a disparu"));
            assert_eq!(
                depuis_hex(valeur),
                *couleur,
                "{fichier} : {variable} vaut {valeur}, ki-ui dit {}",
                hex(*couleur)
            );
        }
        let palette: Vec<Color32> = NOMMEES.iter().map(|(_, c)| *c).chain(PSEUDOS).collect();
        for ecrite in couleurs_ecrites(source) {
            let connue = palette.contains(&depuis_hex(&ecrite))
                || propres.iter().any(|(p, _)| p.eq_ignore_ascii_case(&ecrite));
            assert!(
                connue,
                "{fichier} : {ecrite} n'est pas dans la palette de ki-ui — prendre la couleur de la \
                 palette, ou la déclarer propre à ce fichier, avec sa raison"
            );
        }
    }

    #[test]
    fn un_pseudo_a_la_meme_couleur_sur_pc_et_sur_mobile() {
        let noms = (0..400)
            .map(|i| format!("joueur{i}"))
            .chain(["Redik_", "Kiwi", "Éloïse", "ñandú", "🐸 crapaud", ""].map(String::from));
        let mut teintes_vues = [false; 8];
        for nom in noms {
            let pc = color_for(&nom);
            assert_eq!(pc, rgb(ki_core::apparence::couleur_pseudo(&nom)), "« {nom} »");
            if let Some(i) = PSEUDOS.iter().position(|t| *t == pc) {
                teintes_vues[i] = true;
            }
        }
        // Chaque teinte est sortie au moins une fois : la palette de ki-core
        // est la même, dans le même ordre.
        assert!(teintes_vues.iter().all(|v| *v), "teintes jamais tirées : {teintes_vues:?}");
        assert_eq!(rgb(ki_core::apparence::INVITE), INVITE);
    }

    #[test]
    fn un_rang_a_la_meme_couleur_sur_pc_et_sur_mobile() {
        for palier in 0..=30 {
            assert_eq!(
                crate::graphes::couleur_de_rang(palier),
                rgb(ki_core::apparence::couleur_rang(palier)),
                "palier {palier}"
            );
        }
    }

    #[test]
    fn la_page_des_invites_suit_la_palette() {
        let attendues = [
            ("--fond", BG_DEEP),
            ("--colonne", BG_SIDE),
            ("--panneau", BG_BASE),
            ("--releve", BG_RAISED),
            ("--survol", BG_HOVER),
            ("--actif", BG_ACTIVE),
            ("--bord", BORDER),
            ("--bord-doux", BORDER_SOFT),
            ("--texte", TEXT),
            ("--texte-dim", TEXT_DIM),
            ("--texte-faible", TEXT_FAINT),
            ("--accent", ACCENT),
            ("--parle", SPEAK),
            ("--danger", DANGER),
            ("--alerte", WARN),
            ("--invite", INVITE),
        ];
        let propres = [
            ("#00a352", "le survol du bouton principal : le web l'assombrit, le PC l'éclaircit"),
            ("#06210f", "le texte posé sur l'accent"),
        ];
        suit_la_palette("porte.css", PORTE_CSS, &attendues, &propres);
        suit_la_palette("porte.html", PORTE_HTML, &[], &[]);
        // Les mesures aussi : le rayon des cartes et la marge des panneaux.
        let mesure = |variable: &str| {
            variables(PORTE_CSS)
                .into_iter()
                .find(|(n, _)| n == variable)
                .and_then(|(_, v)| v.strip_suffix("px").and_then(|n| n.parse::<f32>().ok()))
                .unwrap_or_else(|| panic!("porte.css : {variable} manque, ou n'est pas en px"))
        };
        assert_eq!(mesure("--rayon"), f32::from(ki_ui::jetons::rayon::XL));
        assert_eq!(mesure("--marge"), ki_ui::jetons::espace::XL);
    }

    #[test]
    fn l_appli_mobile_suit_la_palette() {
        let attendues = [
            ("--fond", BG_DEEP),
            ("--cote", BG_SIDE),
            ("--base", BG_BASE),
            ("--releve", BG_RAISED),
            ("--survol", BG_HOVER),
            ("--actif", BG_ACTIVE),
            ("--bord", BORDER),
            ("--bord-doux", BORDER_SOFT),
            ("--texte", TEXT),
            ("--doux", TEXT_DIM),
            ("--pale", TEXT_FAINT),
            ("--accent", ACCENT),
            ("--parle", SPEAK),
            ("--danger", DANGER),
            ("--info", INFO),
        ];
        let propres = [
            ("#04140b", "le texte posé sur l'accent"),
            ("#3a1d22", "le fond opaque du bandeau d'erreur"),
            ("#ffb4b4", "le texte du bandeau d'erreur"),
            ("#3b2a12", "le fond opaque du bandeau d'alerte"),
            ("#ffc785", "le texte du bandeau d'alerte"),
        ];
        suit_la_palette("index.html (mobile)", MOBILE, &attendues, &propres);
        // Le repli JavaScript des couleurs de pseudos : la même palette, dans
        // le même ordre — l'ordre fait la couleur de chacun.
        let debut = MOBILE.find("const teintes = [").expect("index.html : la palette des pseudos a disparu");
        let fin = debut + MOBILE[debut..].find(']').expect("index.html : palette des pseudos mal fermée");
        let teintes: Vec<Color32> = couleurs_ecrites(&MOBILE[debut..fin]).iter().map(|h| depuis_hex(h)).collect();
        assert_eq!(teintes, PSEUDOS.to_vec(), "index.html : les teintes des pseudos ont divergé");
        // L'icône Android : sur le fond de l'accent.
        let icone: Vec<Color32> = couleurs_ecrites(ICONE_ANDROID).iter().map(|h| depuis_hex(h)).collect();
        assert_eq!(icone, [ACCENT], "l'icône Android a changé de fond");
    }
}
