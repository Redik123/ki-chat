//! ki-ui : la boîte à outils d'interface de ki-chat, par-dessus egui.
//!
//! - [`jetons`] : les couleurs, et les échelles des espacements, des rayons
//!   et des tailles de texte. Une seule source, pour que les écrans se
//!   ressemblent — le code en comptait douze tailles de texte et neuf
//!   espacements, souvent à un demi-point les uns des autres.
//! - [`flex`] : la mise en page flexbox (rangées, colonnes, retour à la
//!   ligne, répartition, éléments qui grandissent), calculée par taffy.
//! - [`composants`] : les briques dessinées à la main — boutons à icône,
//!   onglets, sections et lignes de réglage, choix segmentés et en
//!   pastilles, interrupteurs, curseurs, champs, bandeaux, encarts,
//!   rappels, étiquettes, vumètres, avatars —, et [`icones`], le jeu
//!   d'icônes vectorielles qu'ils emploient.
//! - [`emoji`] : les emoji en couleur, peints avec la police emoji du
//!   système, par-dessus le texte qu'egui met en page.
//! - [`selecteur_emoji`] : le bouton sourire et son panneau — recherche
//!   en français, récents, catégories, teintes de peau.
//! - [`liste`] : la liste virtualisée, qui ne construit que ce qui se voit
//!   et garde immobile la ligne qu'on lit quand le reste bouge.
//! - [`style`] : l'allure d'ensemble (polices, palette, espacements, thème
//!   sombre), installée en un appel.
//!
//! egui reste dessous : tout ce qui n'a pas besoin de ki-ui continue de
//! s'écrire en egui ordinaire, et ki-ui s'y mêle sans rien imposer.

pub mod composants;
pub mod emoji;
pub mod flex;
pub mod icones;
pub mod jetons;
pub mod liste;
pub mod selecteur_emoji;
pub mod style;
#[cfg(debug_assertions)]
mod mouchard;

pub use egui;
