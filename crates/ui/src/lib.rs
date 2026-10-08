//! ki-ui : la boîte à outils d'interface de ki-chat, par-dessus egui.
//!
//! - [`jetons`] : les couleurs, et les échelles des espacements, des rayons
//!   et des tailles de texte. Une seule source, pour que les écrans se
//!   ressemblent — le code en comptait douze tailles de texte et neuf
//!   espacements, souvent à un demi-point les uns des autres.
//! - [`flex`] : la mise en page flexbox (rangées, colonnes, retour à la
//!   ligne, répartition, éléments qui grandissent), calculée par taffy.
//! - [`composants`] : les briques dessinées à la main — boutons à icône,
//!   sections et lignes de réglage, interrupteurs, bandeaux, vumètres,
//!   avatars —, et [`icones`], le jeu d'icônes vectorielles qu'ils
//!   emploient.
//!
//! egui reste dessous : tout ce qui n'a pas besoin de ki-ui continue de
//! s'écrire en egui ordinaire, et ki-ui s'y mêle sans rien imposer.

pub mod composants;
pub mod flex;
pub mod icones;
pub mod jetons;
#[cfg(debug_assertions)]
mod mouchard;

pub use egui;
