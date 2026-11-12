//! La place de la fenêtre principale, d'un lancement à l'autre : où elle
//! était, et quelle taille elle avait.
//!
//! eframe savait la retenir, et on le lui avait interdit (`persist_window:
//! false`, voir `main`) : il la remettait telle quelle, y compris sur un
//! écran débranché depuis — la fenêtre revenait hors de vue. Ici la place
//! n'est rendue que si elle touche encore un écran présent ; sinon la
//! fenêtre s'ouvre où le système la pose, à la taille retenue.
//!
//! Tout est en **pixels** : les points d'egui dépendent de l'échelle de
//! l'interface et de celle de l'écran, qui changent toutes les deux.

use eframe::egui;

/// Les tailles en dehors desquelles une place rangée n'est pas crue : un
/// fichier abîmé ne doit pas ouvrir une fenêtre d'un pixel, ni de dix écrans.
const TAILLE_MIN: u32 = 300;
const TAILLE_MAX: u32 = 16_384;

/// Le coin de la fenêtre (bord extérieur) et la taille de son contenu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Place {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) largeur: u32,
    pub(crate) hauteur: u32,
}

impl Place {
    /// Relit « x,y,largeur,hauteur ». Abîmé, incomplet ou hors bornes : rien.
    pub(crate) fn lire(texte: &str) -> Option<Self> {
        let mut morceaux = texte.split(',').map(str::trim);
        let x: i32 = morceaux.next()?.parse().ok()?;
        let y: i32 = morceaux.next()?.parse().ok()?;
        let largeur: u32 = morceaux.next()?.parse().ok()?;
        let hauteur: u32 = morceaux.next()?.parse().ok()?;
        let plausible = |t: u32| (TAILLE_MIN..=TAILLE_MAX).contains(&t);
        (morceaux.next().is_none() && plausible(largeur) && plausible(hauteur)).then_some(Self { x, y, largeur, hauteur })
    }

    pub(crate) fn ecrire(&self) -> String {
        format!("{},{},{},{}", self.x, self.y, self.largeur, self.hauteur)
    }

    /// La place d'après ce qu'egui rapporte de la fenêtre, en points, et le
    /// nombre de pixels par point de cette image.
    pub(crate) fn relevee(dehors: egui::Rect, dedans: egui::Rect, pixels_par_point: f32) -> Option<Self> {
        let px = |v: f32| (v * pixels_par_point).round();
        let (largeur, hauteur) = (px(dedans.width()), px(dedans.height()));
        let plausible = |t: f32| t >= TAILLE_MIN as f32 && t <= TAILLE_MAX as f32;
        (pixels_par_point > 0.0 && plausible(largeur) && plausible(hauteur)).then(|| Self {
            x: px(dehors.left()) as i32,
            y: px(dehors.top()) as i32,
            largeur: largeur as u32,
            hauteur: hauteur as u32,
        })
    }

    /// Le coin, en points, pour une image à `pixels_par_point`.
    pub(crate) fn position(&self, pixels_par_point: f32) -> egui::Pos2 {
        egui::pos2(self.x as f32 / pixels_par_point, self.y as f32 / pixels_par_point)
    }

    /// La taille du contenu, en points, pour une image à `pixels_par_point`.
    pub(crate) fn taille(&self, pixels_par_point: f32) -> egui::Vec2 {
        egui::vec2(self.largeur as f32 / pixels_par_point, self.hauteur as f32 / pixels_par_point)
    }

    /// Touche-t-elle un écran présent ? On regarde la bande du haut, là où
    /// se trouve la barre de titre : c'est par elle qu'on rattrape une
    /// fenêtre. Hors de Windows on ne sait pas le dire : la position n'est
    /// alors pas rendue, la taille seule.
    #[cfg(windows)]
    pub(crate) fn sur_un_ecran(&self) -> bool {
        use windows::Win32::Foundation::RECT;
        use windows::Win32::Graphics::Gdi::{MonitorFromRect, MONITOR_DEFAULTTONULL};
        let marge = 40.min(self.largeur as i32 / 4);
        let bande = RECT { left: self.x + marge, top: self.y, right: self.x + self.largeur as i32 - marge, bottom: self.y + 40 };
        // SAFETY : `bande` vit le temps de l'appel, qui ne garde rien.
        !unsafe { MonitorFromRect(&bande, MONITOR_DEFAULTTONULL) }.is_invalid()
    }

    #[cfg(not(windows))]
    pub(crate) fn sur_un_ecran(&self) -> bool {
        false
    }
}

/// Ce qu'il reste à rendre de la place retenue, image après image : la
/// position d'abord, la taille deux images plus tard — passer d'un écran à
/// un autre change le nombre de pixels par point, et la taille se calcule
/// avec celui de l'écran d'arrivée.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rendu {
    Position(Place),
    Taille(Place, u8),
}

impl Rendu {
    /// Par où commencer : la position si elle touche un écran, sinon la
    /// taille seule.
    pub(crate) fn depuis(place: Place, sur_un_ecran: bool) -> Self {
        if sur_un_ecran { Rendu::Position(place) } else { Rendu::Taille(place, 0) }
    }

    /// Une image passe. Rend la commande à envoyer à la fenêtre, s'il y en
    /// a une, et ce qu'il reste à faire ensuite.
    pub(crate) fn avancer(self, pixels_par_point: f32) -> (Option<egui::ViewportCommand>, Option<Rendu>) {
        match self {
            Rendu::Position(place) => (Some(egui::ViewportCommand::OuterPosition(place.position(pixels_par_point))), Some(Rendu::Taille(place, 2))),
            Rendu::Taille(place, 0) => (Some(egui::ViewportCommand::InnerSize(place.taille(pixels_par_point))), None),
            Rendu::Taille(place, attente) => (None, Some(Rendu::Taille(place, attente - 1))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_place_s_ecrit_et_se_relit() {
        let place = Place { x: -1900, y: 40, largeur: 1350, hauteur: 840 };
        assert_eq!(place.ecrire(), "-1900,40,1350,840");
        assert_eq!(Place::lire(&place.ecrire()), Some(place));
        assert_eq!(Place::lire(" 10, 20 ,900,600 "), Some(Place { x: 10, y: 20, largeur: 900, hauteur: 600 }));
        // Abîmé, incomplet, hors bornes : rien, la fenêtre s'ouvre comme
        // sur une installation neuve.
        for abime in ["", "10,20,900", "10,20,900,600,1", "a,b,c,d", "10,20,1,600", "10,20,900,99999", "10,20,-900,600"] {
            assert_eq!(Place::lire(abime), None, "« {abime} »");
        }
    }

    /// Ce qu'egui rapporte en points revient en pixels, et repart en points
    /// pour une autre échelle sans que la fenêtre change de taille.
    #[test]
    fn la_place_tient_en_pixels_quelle_que_soit_l_echelle() {
        let dehors = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(920.0, 600.0));
        let dedans = egui::Rect::from_min_size(egui::pos2(110.0, 80.0), egui::vec2(900.0, 560.0));
        let place = Place::relevee(dehors, dedans, 1.5).unwrap();
        assert_eq!(place, Place { x: 150, y: 75, largeur: 1350, hauteur: 840 });
        // À 100 %, la même fenêtre fait 1350×840 points.
        assert_eq!(place.taille(1.0), egui::vec2(1350.0, 840.0));
        assert_eq!(place.position(1.5), egui::pos2(100.0, 50.0));
        // Une fenêtre réduite à rien (minimisée) n'est pas une place.
        assert_eq!(Place::relevee(dehors, egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(10.0, 10.0)), 1.0), None);
        assert_eq!(Place::relevee(dehors, dedans, 0.0), None);
    }

    /// La position d'abord, la taille deux images après ; sans écran où la
    /// poser, la taille seule.
    #[test]
    fn la_place_se_rend_en_deux_temps() {
        let place = Place { x: 200, y: 100, largeur: 1200, hauteur: 800 };
        let (commande, suite) = Rendu::depuis(place, true).avancer(1.0);
        assert!(matches!(commande, Some(egui::ViewportCommand::OuterPosition(p)) if p == egui::pos2(200.0, 100.0)));
        let (commande, suite) = suite.unwrap().avancer(1.0);
        assert!(commande.is_none());
        let (commande, suite) = suite.unwrap().avancer(1.0);
        assert!(commande.is_none());
        // L'écran d'arrivée est à 125 % : la taille se calcule avec lui.
        let (commande, suite) = suite.unwrap().avancer(1.25);
        assert!(matches!(commande, Some(egui::ViewportCommand::InnerSize(t)) if t == egui::vec2(960.0, 640.0)));
        assert!(suite.is_none());

        let (commande, suite) = Rendu::depuis(place, false).avancer(2.0);
        assert!(matches!(commande, Some(egui::ViewportCommand::InnerSize(t)) if t == egui::vec2(600.0, 400.0)));
        assert!(suite.is_none());
    }

    /// Une place très loin de tout écran n'est pas rendue ; l'origine de
    /// l'écran principal, si.
    #[cfg(windows)]
    #[test]
    fn une_place_hors_de_tout_ecran_se_voit() {
        assert!(!Place { x: 900_000, y: 900_000, largeur: 1120, hauteur: 720 }.sur_un_ecran());
        assert!(Place { x: 0, y: 0, largeur: 1120, hauteur: 720 }.sur_un_ecran());
    }
}
