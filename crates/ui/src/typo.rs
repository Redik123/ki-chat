//! Les rôles de texte et les tons : ce qu'un texte *est* (une aide, un
//! libellé, un titre de section) et ce qu'il *dit* (rien de spécial, une
//! alerte, une erreur), plutôt qu'une taille et une couleur recomposées à
//! chaque ligne.
//!
//! Le client comptait cent couples taille-couleur différents pour ses
//! `RichText`, dont une demi-douzaine font l'essentiel : ce sont eux, nommés.
//! Un écran qui écrit `Texte::Aide.riche(ui, "…")` suit l'échelle sans y
//! penser — et le jour où l'aide change de taille ou de teinte, elle change
//! partout.
//!
//! Les titres pèsent vraiment : egui n'a pas de graisse (`.strong()` n'y
//! change que la couleur, et plus rien dès qu'une couleur est donnée), d'où
//! la famille Ubuntu Medium que [`crate::style::installer`] installe. C'est
//! pour la trouver que [`Texte::riche`] demande l'`Ui`.

use egui::{Color32, FontId, Response, RichText, Ui};

use crate::icones::Icon;
use crate::jetons::{couleur, police, texte};

/// Ce qu'un texte, un bandeau ou un bouton dit : sa couleur, par son sens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ton {
    /// Le texte ordinaire.
    Neutre,
    /// Ce qui accompagne : un libellé, une légende.
    Discret,
    /// Ce qui s'efface : une aide, un horodatage.
    Pale,
    /// L'accent de l'appli : ce qui est actif, réussi, à faire.
    Accent,
    /// Ce qui demande attention sans être une erreur.
    Alerte,
    /// Une erreur, un geste qui détruit.
    Danger,
    /// Une information.
    Info,
}

impl Ton {
    /// Tous les tons, du plus neutre au plus vif : pour les montrer.
    pub const TOUS: [Ton; 7] =
        [Ton::Neutre, Ton::Discret, Ton::Pale, Ton::Accent, Ton::Alerte, Ton::Danger, Ton::Info];

    /// La couleur du ton, dans la palette.
    pub const fn couleur(self) -> Color32 {
        match self {
            Ton::Neutre => couleur::TEXT,
            Ton::Discret => couleur::TEXT_DIM,
            Ton::Pale => couleur::TEXT_FAINT,
            Ton::Accent => couleur::ACCENT,
            Ton::Alerte => couleur::WARN,
            Ton::Danger => couleur::DANGER,
            Ton::Info => couleur::INFO,
        }
    }

    /// L'icône qui va avec le ton, en tête d'un bandeau ou d'un toast.
    pub const fn icone(self) -> Icon {
        match self {
            Ton::Accent => Icon::Check,
            Ton::Alerte | Ton::Danger => Icon::Warning,
            Ton::Neutre | Ton::Discret | Ton::Pale | Ton::Info => Icon::Info,
        }
    }

    /// Le ton dit qu'il y a un problème : ce qui le porte reste plus
    /// longtemps à l'écran, par exemple.
    pub const fn est_grave(self) -> bool {
        matches!(self, Ton::Alerte | Ton::Danger)
    }
}

/// Le rôle d'un texte : sa taille, sa graisse et son ton d'origine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Texte {
    /// Une étiquette en capitales au-dessus d'un groupe.
    Minuscule,
    /// Une précision, une aide, un horodatage.
    Aide,
    /// Un libellé, une ligne de réglage, du texte secondaire.
    Libelle,
    /// Le corps : un message, une phrase.
    Corps,
    /// Le titre d'une section.
    TitreSection,
    /// Le titre d'une fenêtre.
    TitreFenetre,
    /// Le titre d'une page qui prend la place du chat.
    TitrePage,
    /// Un chiffre clé : ce qu'on lit de loin.
    Kpi,
}

impl Texte {
    /// Tous les rôles, du plus petit au plus grand : pour les montrer.
    pub const TOUS: [Texte; 8] = [
        Texte::Minuscule,
        Texte::Aide,
        Texte::Libelle,
        Texte::Corps,
        Texte::TitreSection,
        Texte::TitreFenetre,
        Texte::TitrePage,
        Texte::Kpi,
    ];

    /// La taille, en points : un jeton de [`crate::jetons::texte`].
    pub const fn taille(self) -> f32 {
        match self {
            Texte::Minuscule => texte::MINUSCULE,
            Texte::Aide => texte::PETIT,
            Texte::Libelle => texte::COURANT,
            Texte::Corps => texte::CORPS,
            Texte::TitreSection => texte::TITRE,
            Texte::TitreFenetre => texte::GRAND,
            Texte::TitrePage => texte::TITRE_PAGE,
            Texte::Kpi => texte::KPI,
        }
    }

    /// Les titres et les chiffres clés pèsent ; le reste est en corps
    /// ordinaire.
    pub const fn est_gras(self) -> bool {
        matches!(self, Texte::TitreSection | Texte::TitreFenetre | Texte::TitrePage | Texte::Kpi)
    }

    /// Le ton du rôle quand on n'en dit rien.
    pub const fn ton(self) -> Ton {
        match self {
            Texte::Minuscule | Texte::Aide => Ton::Pale,
            Texte::Libelle => Ton::Discret,
            Texte::Corps | Texte::TitreSection | Texte::TitreFenetre | Texte::TitrePage | Texte::Kpi => Ton::Neutre,
        }
    }

    /// La police du rôle : pour qui peint lui-même son texte.
    pub fn police(self, ctx: &egui::Context) -> FontId {
        if self.est_gras() {
            police::moyenne(ctx, self.taille())
        } else {
            FontId::proportional(self.taille())
        }
    }

    /// Le texte dans son rôle et son ton d'origine, prêt à poser
    /// (`ui.label(…)`) ou à donner à un autre widget.
    pub fn riche(self, ui: &Ui, contenu: impl Into<String>) -> RichText {
        self.riche_ton(ui, self.ton(), contenu)
    }

    /// Le texte dans son rôle, sur un autre ton : une aide qui alerte, un
    /// libellé qui s'efface. Pour une couleur hors des tons — celle d'un
    /// pseudo, d'une série —, `.color(…)` sur ce que rend [`Texte::riche`].
    pub fn riche_ton(self, ui: &Ui, ton: Ton, contenu: impl Into<String>) -> RichText {
        let riche = RichText::new(contenu).color(ton.couleur());
        if self.est_gras() {
            riche.font(police::moyenne(ui.ctx(), self.taille()))
        } else {
            riche.size(self.taille())
        }
    }
}

/// Pose un texte dans son rôle, sur son ton d'origine.
pub fn texte(ui: &mut Ui, role: Texte, contenu: impl Into<String>) -> Response {
    let riche = role.riche(ui, contenu);
    ui.label(riche)
}

/// Pose un texte dans son rôle, sur le ton donné.
pub fn texte_ton(ui: &mut Ui, role: Texte, ton: Ton, contenu: impl Into<String>) -> Response {
    let riche = role.riche_ton(ui, ton, contenu);
    ui.label(riche)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// L'échelle des rôles monte, et chaque rôle prend sa taille dans les
    /// jetons : aucune taille hors système ne se glisse ici.
    #[test]
    fn les_roles_montent_et_restent_sur_les_jetons() {
        let tailles: Vec<f32> = Texte::TOUS.iter().map(|r| r.taille()).collect();
        assert!(tailles.windows(2).all(|w| w[0] < w[1]), "{tailles:?}");
        let jetons = [
            texte::MINUSCULE,
            texte::PETIT,
            texte::COURANT,
            texte::CORPS,
            texte::TITRE,
            texte::GRAND,
            texte::TITRE_PAGE,
            texte::KPI,
        ];
        assert_eq!(tailles, jetons);
    }

    #[test]
    fn seuls_les_titres_et_les_chiffres_pesent() {
        let gras: Vec<Texte> = Texte::TOUS.into_iter().filter(|r| r.est_gras()).collect();
        assert_eq!(gras, [Texte::TitreSection, Texte::TitreFenetre, Texte::TitrePage, Texte::Kpi]);
        // Ce qui pèse se lit en clair : jamais un titre pâle d'origine.
        assert!(gras.iter().all(|r| r.ton() == Ton::Neutre));
    }

    #[test]
    fn chaque_ton_a_sa_couleur() {
        let mut couleurs: Vec<Color32> = Ton::TOUS.iter().map(|t| t.couleur()).collect();
        couleurs.dedup();
        assert_eq!(couleurs.len(), Ton::TOUS.len(), "deux tons de la même couleur");
        assert!(Ton::Danger.est_grave() && Ton::Alerte.est_grave());
        assert!(!Ton::Info.est_grave() && !Ton::Accent.est_grave() && !Ton::Neutre.est_grave());
    }

    /// Un rôle se pose dans un contexte sans l'allure de ki-ui (un test, un
    /// outil) : le gras retombe sur le corps ordinaire au lieu de paniquer
    /// sur une famille inconnue.
    #[test]
    fn un_titre_se_pose_sans_l_allure_installee() {
        let ctx = egui::Context::default();
        ctx.run_ui(Default::default(), |ui| {
            for role in Texte::TOUS {
                texte(ui, role, "Portez ce vieux whisky");
                texte_ton(ui, role, Ton::Alerte, "au juge blond");
            }
            assert_eq!(Texte::TitreSection.police(ui.ctx()), FontId::proportional(texte::TITRE));
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn un_role_rend_sa_taille_et_la_couleur_du_ton() {
        let ctx = egui::Context::default();
        ctx.run_ui(Default::default(), |ui| {
            let aide = Texte::Aide.riche(ui, "aide");
            assert_eq!(aide.text(), "aide");
            // Un texte posé à la main, comme le client l'écrivait partout.
            let a_la_main = RichText::new("aide").color(couleur::TEXT_FAINT).size(texte::PETIT);
            assert_eq!(aide, a_la_main);
            let alerte = Texte::Libelle.riche_ton(ui, Ton::Alerte, "x");
            assert_eq!(alerte, RichText::new("x").color(couleur::WARN).size(texte::COURANT));
        })
        .drop_without_applying_deltas();
    }
}
