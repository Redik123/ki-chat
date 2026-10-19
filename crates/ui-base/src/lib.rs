//! ki-ui-base : l'allure commune des applis ki-*, sans moteur d'interface.
//!
//! - [`couleur`] : la palette de ki-chat — fonds, traits, textes, accent,
//!   alertes, les huit teintes des pseudos —, en [`Couleur`] sRGB 8 bits ;
//! - [`espace`], [`rayon`], [`texte`] : les échelles des espacements, des
//!   rayons d'angle et des tailles de texte, en points.
//!
//! Chaque appli la traduit dans ses types : ki-ui pour egui (ki-chat),
//! ki-core en `0xRRGGBB` (l'appli mobile), ki-studio pour iced. Une couleur
//! ou un pas d'échelle se change ici, et toutes suivent.
//!
//! Les nouveaux écrans s'en tiennent à ces valeurs ; les anciens y viennent
//! à mesure qu'on les reprend. Une valeur qui manque se discute ici, pas
//! au coin d'un écran.

/// Une couleur sRGB, huit bits par canal, alpha non prémultiplié.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Couleur {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Couleur {
    /// Une couleur opaque.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// En `0xRRGGBB`, sans l'alpha.
    pub const fn en_u32(self) -> u32 {
        (self.r as u32) << 16 | (self.g as u32) << 8 | self.b as u32
    }

    /// « #0c0f14 », comme l'écrit une feuille de style ; « #0c0f14cc »
    /// quand elle est translucide.
    pub fn en_css(self) -> String {
        if self.a == 255 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
    }

    /// Les canaux de 0 à 1, pour les moteurs qui comptent en flottants
    /// (iced) : rouge, vert, bleu, alpha.
    pub fn en_f32(self) -> [f32; 4] {
        [self.r, self.g, self.b, self.a].map(|c| f32::from(c) / 255.0)
    }

    /// La même, translucide (fonds teintés).
    pub const fn translucide(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// Mélange linéaire avec une autre (`t` = 0 : celle-ci, 1 : l'autre),
    /// opaque.
    pub fn melanger(self, autre: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let f = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
        Self::rgb(f(self.r, autre.r), f(self.g, autre.g), f(self.b, autre.b))
    }
}

/// La palette : un gris-bleu profond façon poste de jeu, un unique accent
/// vert.
pub mod couleur {
    use super::Couleur;

    /// Fond le plus profond : zone de conversation, champs « creusés ».
    pub const BG_DEEP: Couleur = Couleur::rgb(0x0c, 0x0f, 0x14);
    /// Colonne de gauche (salons, membres).
    pub const BG_SIDE: Couleur = Couleur::rgb(0x11, 0x15, 0x1b);
    /// Fond général des panneaux.
    pub const BG_BASE: Couleur = Couleur::rgb(0x15, 0x1a, 0x21);
    /// Surfaces en relief : fenêtres, cartes, boutons.
    pub const BG_RAISED: Couleur = Couleur::rgb(0x1b, 0x21, 0x2a);
    pub const BG_HOVER: Couleur = Couleur::rgb(0x24, 0x2c, 0x37);
    pub const BG_ACTIVE: Couleur = Couleur::rgb(0x2d, 0x37, 0x44);
    /// Survol très discret (lignes de message).
    pub const BG_GHOST: Couleur = Couleur::rgb(0x1a, 0x20, 0x28);

    pub const BORDER: Couleur = Couleur::rgb(0x28, 0x31, 0x3c);
    pub const BORDER_SOFT: Couleur = Couleur::rgb(0x1e, 0x25, 0x2e);
    /// Trait ou texte très en retrait, mais encore lisible.
    pub const BORDER_STRONG: Couleur = Couleur::rgb(0x44, 0x51, 0x60);

    pub const TEXT: Couleur = Couleur::rgb(0xe7, 0xed, 0xf4);
    pub const TEXT_DIM: Couleur = Couleur::rgb(0x94, 0xa2, 0xb2);
    pub const TEXT_FAINT: Couleur = Couleur::rgb(0x63, 0x70, 0x7f);

    /// Accent de la marque (vert ki-chat).
    pub const ACCENT: Couleur = Couleur::rgb(0x00, 0xd2, 0x6a);
    /// Vert « quelqu'un parle ».
    pub const SPEAK: Couleur = Couleur::rgb(0x00, 0xe6, 0x76);
    pub const DANGER: Couleur = Couleur::rgb(0xff, 0x6b, 0x6b);
    pub const WARN: Couleur = Couleur::rgb(0xff, 0xa1, 0x57);
    pub const INFO: Couleur = Couleur::rgb(0x58, 0xa6, 0xff);
    /// Les invités web : un ambre à part, ni l'accent (le bot, le serveur)
    /// ni une couleur de rôle — « pas des nôtres, le temps d'une porte ».
    pub const INVITE: Couleur = Couleur::rgb(0xf0, 0xb8, 0x6c);

    /// Les huit teintes des pseudos, dans l'ordre du hachage : le même sur
    /// le PC, dans ki-core et dans l'appli mobile.
    pub const PSEUDOS: [Couleur; 8] = [
        Couleur::rgb(0x2d, 0xd4, 0x8f),
        Couleur::rgb(0x62, 0xa8, 0xff),
        Couleur::rgb(0xff, 0xa9, 0x5c),
        Couleur::rgb(0xff, 0x8a, 0xc4),
        Couleur::rgb(0xba, 0x92, 0xff),
        Couleur::rgb(0x2a, 0xd3, 0xdd),
        Couleur::rgb(0xff, 0xd8, 0x63),
        Couleur::rgb(0xff, 0x7d, 0x7d),
    ];

    /// La palette, nommée : pour la montrer (la vitrine) et pour vérifier
    /// que ses copies hors de Rust la suivent.
    pub const NOMMEES: [(&str, Couleur); 19] = [
        ("BG_DEEP", BG_DEEP),
        ("BG_SIDE", BG_SIDE),
        ("BG_BASE", BG_BASE),
        ("BG_RAISED", BG_RAISED),
        ("BG_HOVER", BG_HOVER),
        ("BG_ACTIVE", BG_ACTIVE),
        ("BG_GHOST", BG_GHOST),
        ("BORDER", BORDER),
        ("BORDER_SOFT", BORDER_SOFT),
        ("BORDER_STRONG", BORDER_STRONG),
        ("TEXT", TEXT),
        ("TEXT_DIM", TEXT_DIM),
        ("TEXT_FAINT", TEXT_FAINT),
        ("ACCENT", ACCENT),
        ("SPEAK", SPEAK),
        ("DANGER", DANGER),
        ("WARN", WARN),
        ("INFO", INFO),
        ("INVITE", INVITE),
    ];

    /// Les couleurs d'une série — bandes d'un égaliseur, courbes d'un
    /// graphe : l'accent, l'info, l'alerte, puis des teintes des pseudos,
    /// franchement distinctes les unes des autres.
    pub const SERIES: [Couleur; 8] = [
        ACCENT, INFO, WARN, PSEUDOS[4], PSEUDOS[3], PSEUDOS[6], PSEUDOS[5], PSEUDOS[7],
    ];

    /// Couleur attribuée à un pseudo — même pseudo, même couleur, partout
    /// (et d'une appli ki-* à l'autre).
    ///
    /// Une palette fermée de huit teintes : les pseudos se lisent dans le fil
    /// de discussion, mieux vaut peu de couleurs mais franchement distinctes.
    pub fn pour_pseudo(nom: &str) -> Couleur {
        let h = nom.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(u32::from(b)));
        PSEUDOS[(h % PSEUDOS.len() as u32) as usize]
    }
}

/// Les espacements, en points : entre deux éléments, dans une marge.
pub mod espace {
    /// Entre une icône et son texte collé.
    pub const XXS: f32 = 2.0;
    /// Entre un titre et sa précision.
    pub const XS: f32 = 4.0;
    /// Entre deux contrôles d'une même rangée.
    pub const S: f32 = 6.0;
    /// L'écart courant.
    pub const M: f32 = 8.0;
    /// Entre deux blocs d'une même section ; marge d'une carte.
    pub const L: f32 = 12.0;
    /// Marge d'une section, d'une fenêtre.
    pub const XL: f32 = 16.0;
    /// Entre deux sections qui n'ont rien à voir.
    pub const XXL: f32 = 24.0;
}

/// Les rayons d'angle, en points entiers (egui les veut ainsi ; iced les
/// prend en flottants).
pub mod rayon {
    /// Pastilles, badges, petites cases.
    pub const S: u8 = 4;
    /// Boutons, champs, menus.
    pub const M: u8 = 6;
    /// Encarts, vignettes.
    pub const L: u8 = 8;
    /// Cartes, sections, barre de saisie.
    pub const XL: u8 = 12;
    /// Pilules : de bout en bout arrondies à leur hauteur courante.
    pub const PILULE: u8 = 14;
}

/// Les tailles de texte, en points — celles d'egui pour le corps (14) et
/// le petit (11,5) en font partie.
pub mod texte {
    /// Étiquettes en capitales au-dessus d'un groupe.
    pub const MINUSCULE: f32 = 10.5;
    /// Précisions, aides, horodatages.
    pub const PETIT: f32 = 11.5;
    /// Libellés, lignes de réglage, texte secondaire.
    pub const COURANT: f32 = 12.5;
    /// Le corps : messages, boutons.
    pub const CORPS: f32 = 14.0;
    /// Titre d'une section.
    pub const TITRE: f32 = 15.0;
    /// Titre d'une page, d'une fenêtre.
    pub const GRAND: f32 = 19.0;
    /// Le titre d'une page qui prend la place du chat.
    pub const TITRE_PAGE: f32 = 22.0;
    /// Le chiffre d'une tuile, d'une affiche : ce qu'on lit de loin.
    pub const KPI: f32 = 28.0;
}

/// Les largeurs qui décident d'une mise en page, en points.
pub mod largeur {
    /// Une tuile de chiffre clé, au plus étroit.
    pub const TUILE_MIN: f32 = 150.0;
    /// Une carte de membre, au plus étroite : cinq tiennent dans
    /// [`PAGE_REFERENCE`].
    pub const CARTE_MIN: f32 = 180.0;
    /// Le centre de ki-chat entre la colonne des salons et celle des
    /// membres, dans une fenêtre ordinaire : la largeur où une page doit
    /// être belle.
    pub const PAGE_REFERENCE: f32 = 980.0;
    /// En dessous, une page passe sur une seule colonne.
    pub const PAGE_ETROITE: f32 = 720.0;
}

#[cfg(test)]
mod tests {
    use super::couleur::*;
    use super::*;

    #[test]
    fn une_couleur_s_ecrit_de_toutes_les_facons() {
        assert_eq!(BG_DEEP.en_css(), "#0c0f14");
        assert_eq!(BG_DEEP.translucide(0xcc).en_css(), "#0c0f14cc");
        assert_eq!(ACCENT.en_u32(), 0x00d26a);
        assert_eq!(Couleur::rgb(255, 0, 51).en_f32(), [1.0, 0.0, 0.2, 1.0]);
    }

    #[test]
    fn un_melange_va_d_un_bout_a_l_autre() {
        assert_eq!(BORDER.melanger(ACCENT, 0.0), BORDER);
        assert_eq!(BORDER.melanger(ACCENT, 1.0), ACCENT);
        assert_eq!(BORDER.melanger(ACCENT, 7.0), ACCENT, "borné");
        assert_eq!(Couleur::rgb(0, 0, 0).melanger(Couleur::rgb(255, 255, 255), 0.5), Couleur::rgb(128, 128, 128));
        assert_eq!(ACCENT.translucide(10).melanger(ACCENT, 0.5).a, 255, "le mélange est opaque");
    }

    /// Le hachage ne bouge pas : un membre garde sa couleur d'une version à
    /// l'autre, et d'une appli à l'autre.
    #[test]
    fn un_pseudo_garde_sa_couleur() {
        assert_eq!(pour_pseudo("Redik_"), PSEUDOS[4]);
        assert_eq!(pour_pseudo("drion"), PSEUDOS[2]);
        assert_eq!(pour_pseudo("mg4230"), PSEUDOS[5]);
        assert_eq!(pour_pseudo(""), PSEUDOS[0]);
    }

    #[test]
    fn la_palette_nommee_est_complete_et_sans_doublon() {
        let mut noms: Vec<&str> = NOMMEES.iter().map(|(n, _)| *n).collect();
        noms.sort_unstable();
        noms.dedup();
        assert_eq!(noms.len(), NOMMEES.len());
        assert!(NOMMEES.iter().all(|(_, c)| c.a == 255), "la palette est opaque");
        assert!(NOMMEES.contains(&("ACCENT", ACCENT)) && NOMMEES.contains(&("INVITE", INVITE)));
    }

    #[test]
    fn les_echelles_montent() {
        let espaces = [espace::XXS, espace::XS, espace::S, espace::M, espace::L, espace::XL, espace::XXL];
        let rayons = [rayon::S, rayon::M, rayon::L, rayon::XL, rayon::PILULE];
        let textes = [texte::MINUSCULE, texte::PETIT, texte::COURANT, texte::CORPS, texte::TITRE, texte::GRAND];
        assert!(espaces.windows(2).all(|w| w[0] < w[1]));
        assert!(rayons.windows(2).all(|w| w[0] < w[1]));
        assert!(textes.windows(2).all(|w| w[0] < w[1]));
    }
}
