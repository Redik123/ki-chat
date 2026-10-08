//! Les jetons de design : la palette, et les échelles des espacements, des
//! rayons d'angle et des tailles de texte.
//!
//! Les nouveaux écrans s'en tiennent à ces valeurs ; les anciens y viennent
//! à mesure qu'on les reprend. Une valeur qui manque se discute ici, pas
//! au coin d'un écran.

use egui::Color32;

/// La palette : un gris-bleu profond façon poste de jeu, un unique accent
/// vert.
pub mod couleur {
    use super::Color32;

    /// Fond le plus profond : zone de conversation, champs « creusés ».
    pub const BG_DEEP: Color32 = Color32::from_rgb(0x0c, 0x0f, 0x14);
    /// Colonne de gauche (salons, membres).
    pub const BG_SIDE: Color32 = Color32::from_rgb(0x11, 0x15, 0x1b);
    /// Fond général des panneaux.
    pub const BG_BASE: Color32 = Color32::from_rgb(0x15, 0x1a, 0x21);
    /// Surfaces en relief : fenêtres, cartes, boutons.
    pub const BG_RAISED: Color32 = Color32::from_rgb(0x1b, 0x21, 0x2a);
    pub const BG_HOVER: Color32 = Color32::from_rgb(0x24, 0x2c, 0x37);
    pub const BG_ACTIVE: Color32 = Color32::from_rgb(0x2d, 0x37, 0x44);
    /// Survol très discret (lignes de message).
    pub const BG_GHOST: Color32 = Color32::from_rgb(0x1a, 0x20, 0x28);

    pub const BORDER: Color32 = Color32::from_rgb(0x28, 0x31, 0x3c);
    pub const BORDER_SOFT: Color32 = Color32::from_rgb(0x1e, 0x25, 0x2e);
    /// Trait ou texte très en retrait, mais encore lisible.
    pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x44, 0x51, 0x60);

    pub const TEXT: Color32 = Color32::from_rgb(0xe7, 0xed, 0xf4);
    pub const TEXT_DIM: Color32 = Color32::from_rgb(0x94, 0xa2, 0xb2);
    pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x63, 0x70, 0x7f);

    /// Accent de la marque (vert ki-chat).
    pub const ACCENT: Color32 = Color32::from_rgb(0x00, 0xd2, 0x6a);
    /// Vert « quelqu'un parle ».
    pub const SPEAK: Color32 = Color32::from_rgb(0x00, 0xe6, 0x76);
    pub const DANGER: Color32 = Color32::from_rgb(0xff, 0x6b, 0x6b);
    pub const WARN: Color32 = Color32::from_rgb(0xff, 0xa1, 0x57);
    pub const INFO: Color32 = Color32::from_rgb(0x58, 0xa6, 0xff);
    /// Les invités web : un ambre à part, ni l'accent (le bot, le serveur)
    /// ni une couleur de rôle — « pas des nôtres, le temps d'une porte ».
    pub const INVITE: Color32 = Color32::from_rgb(0xf0, 0xb8, 0x6c);
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

/// Les rayons d'angle, en points (ceux d'egui sont des entiers).
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
}
