//! L'overlay « qui parle » par-dessus le jeu.
//!
//! Une fenêtre à part — toujours au-dessus, sans bordure, transparente,
//! transparente aux clics et jamais active : le jeu ne sait pas qu'elle
//! existe, l'anti-triche non plus (rien n'est injecté nulle part). Windows
//! la compose comme la bulle de volume ou une notification : là où le jeu
//! passe par le compositeur (fenêtré sans bordure, ou « plein écran » avec
//! les optimisations plein écran, le défaut), elle se voit ; en vrai plein
//! écran exclusif, rien ne peut passer au-dessus et elle ne coûte rien.
//!
//! Elle ne se montre que quand ki-chat n'a pas le focus (on est ailleurs :
//! dans le jeu) et qu'on est en salon vocal ; elle se repeint quand ça
//! change, à dix fois par seconde pendant qu'on parle, jamais plus.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, RichText, Vec2, ViewportBuilder};

use crate::theme::{TEXT, TEXT_DIM};
use crate::ui::{self, Tone};

/// Le titre de la fenêtre de l'overlay — c'est par lui qu'on la retrouve
/// pour la remettre au-dessus.
const TITRE: &str = "ki-chat — qui parle";

/// Les deux gestes Windows que l'overlay a besoin de faire lui-même : se
/// remettre au sommet de la pile des fenêtres « toujours au-dessus » (les
/// jeux s'y remettent aussi, à chaque reprise du focus — la dernière
/// demande gagne), et savoir qui est au premier plan.
#[cfg(windows)]
mod win {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, GetForegroundWindow, GetWindowLongW, GetWindowTextW, SetWindowPos,
        GWL_EXSTYLE, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WS_EX_TOPMOST,
    };

    /// Remet la fenêtre `titre` au sommet des « toujours au-dessus », sans
    /// la déplacer ni l'activer. `false` si elle n'existe pas (encore).
    pub fn remettre_au_dessus(titre: &str) -> bool {
        let large: Vec<u16> = titre.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let Ok(hwnd) = FindWindowW(None, PCWSTR(large.as_ptr())) else { return false };
            if hwnd.is_invalid() {
                return false;
            }
            SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
            .is_ok()
        }
    }

    /// Le titre de la fenêtre au premier plan, et si elle est elle-même
    /// « toujours au-dessus ».
    pub fn premier_plan() -> (String, bool) {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_invalid() {
                return (String::new(), false);
            }
            let mut tampon = [0u16; 128];
            let n = GetWindowTextW(hwnd, &mut tampon).max(0) as usize;
            let titre = String::from_utf16_lossy(&tampon[..n.min(128)]);
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            (titre, ex & WS_EX_TOPMOST.0 != 0)
        }
    }
}

#[cfg(not(windows))]
mod win {
    pub fn remettre_au_dessus(_titre: &str) -> bool {
        false
    }
    pub fn premier_plan() -> (String, bool) {
        (String::new(), false)
    }
}

/// Le coin de l'écran où l'overlay se pose.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Coin {
    HautGauche,
    HautDroite,
    BasGauche,
    BasDroite,
}

impl Coin {
    pub const TOUS: [(Coin, &'static str, &'static str); 4] = [
        (Coin::HautGauche, "haut-gauche", "En haut à gauche"),
        (Coin::HautDroite, "haut-droite", "En haut à droite"),
        (Coin::BasGauche, "bas-gauche", "En bas à gauche"),
        (Coin::BasDroite, "bas-droite", "En bas à droite"),
    ];

    fn id(self) -> &'static str {
        Self::TOUS.iter().find(|(c, _, _)| *c == self).map(|(_, id, _)| *id).unwrap_or("haut-gauche")
    }

    fn depuis(id: &str) -> Self {
        Self::TOUS.iter().find(|(_, i, _)| *i == id).map(|(c, _, _)| *c).unwrap_or(Coin::HautGauche)
    }

    pub fn libelle(self) -> &'static str {
        Self::TOUS.iter().find(|(c, _, _)| *c == self).map(|(_, _, l)| *l).unwrap_or("?")
    }
}

/// Un occupant du salon vocal, tel que l'overlay le montre.
pub struct Ligne {
    pub nom: String,
    pub parle: bool,
    pub photo: Option<egui::TextureHandle>,
}

/// L'overlay : ses réglages (persistés) et sa petite mémoire.
pub struct Overlay {
    pub actif: bool,
    pub coin: Coin,
    /// Montrer aussi ceux qui se taisent ; sinon, seulement qui parle.
    pub toujours: bool,
    /// Dernier signal de parole par pseudo : l'anneau tient un peu après,
    /// sans quoi il clignoterait au rythme des trames.
    recemment: HashMap<String, Instant>,
    /// Un jeu tourne en plein écran exclusif : rien ne peut s'afficher
    /// au-dessus, l'overlay compris — autant le dire.
    pub exclusif: bool,
    /// Dernière sonde du plein écran exclusif et du premier plan.
    sonde: Instant,
    /// Dernière ré-affirmation du « toujours au-dessus ».
    reaffirme: Instant,
    /// La dernière fenêtre vue au premier plan (titre, toujours au-dessus),
    /// pour ne la consigner qu'au changement.
    devant: (String, bool),
}

/// Windows le sait : `SHQueryUserNotificationState` rend « un programme
/// Direct3D tourne en plein écran » quand un jeu a pris l'écran pour lui
/// seul — c'est le seul cas où l'overlay ne peut pas se montrer.
#[cfg(windows)]
fn plein_ecran_exclusif() -> bool {
    use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUNS_RUNNING_D3D_FULL_SCREEN};
    unsafe { SHQueryUserNotificationState().map(|s| s == QUNS_RUNNING_D3D_FULL_SCREEN).unwrap_or(false) }
}

#[cfg(not(windows))]
fn plein_ecran_exclusif() -> bool {
    false
}

const AVATAR: f32 = 22.0;
const HAUTEUR_LIGNE: f32 = 28.0;
const ECART: f32 = 4.0;
const PAD: f32 = 5.0;
/// L'anneau reste allumé tant de temps après le dernier signal : le temps
/// d'une respiration entre deux phrases, sans clignoter.
const TENUE: Duration = Duration::from_millis(1200);

impl Overlay {
    pub fn load(get: impl Fn(&str, &str) -> String) -> Self {
        Self {
            actif: get("overlay", "on") != "off",
            coin: Coin::depuis(&get("overlay_coin", "haut-gauche")),
            // Par défaut, rien à l'écran tant que personne ne parle : la
            // discrétion d'abord.
            toujours: get("overlay_toujours", "off") == "on",
            recemment: HashMap::new(),
            exclusif: false,
            sonde: Instant::now(),
            reaffirme: Instant::now(),
            devant: (String::new(), false),
        }
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        storage.set_string("overlay", if self.actif { "on" } else { "off" }.into());
        storage.set_string("overlay_coin", self.coin.id().into());
        storage.set_string("overlay_toujours", if self.toujours { "on" } else { "off" }.into());
    }

    /// Montre (ou non) l'overlay pour cette image. `focus_principal` : la
    /// fenêtre de ki-chat a le clavier — alors on la voit, pas besoin de
    /// doublon. Sans ligne (hors vocal), rien.
    pub fn montrer(&mut self, ctx: &egui::Context, lignes: Vec<Ligne>, focus_principal: bool) {
        if !self.actif || focus_principal || lignes.is_empty() {
            return;
        }
        let now = Instant::now();
        // Toutes les deux secondes : un jeu a-t-il pris l'écran pour lui
        // seul ? Le changement d'état va au journal — c'est la réponse à
        // « l'overlay ne s'affiche pas », lisible à distance.
        if now.duration_since(self.sonde) > Duration::from_secs(2) {
            self.sonde = now;
            // Qui est devant, et se met-il lui-même au-dessus ? Consigné au
            // changement : c'est la réponse à « l'overlay ne se voit pas ».
            let devant = win::premier_plan();
            if devant != self.devant && devant.0 != TITRE {
                ki_voice::journal(format!(
                    "overlay : au premier plan « {} » (toujours au-dessus : {})",
                    devant.0,
                    if devant.1 { "oui" } else { "non" }
                ));
                self.devant = devant;
            }
            let exclusif = plein_ecran_exclusif();
            if exclusif != self.exclusif {
                self.exclusif = exclusif;
                ki_voice::journal(
                    if exclusif {
                        "overlay : un jeu tourne en plein écran exclusif, rien ne peut s'afficher \
                         au-dessus — passe-le en fenêtré plein écran"
                    } else {
                        "overlay : fin du plein écran exclusif, l'overlay peut s'afficher"
                    }
                    .into(),
                );
            }
        }
        for l in &lignes {
            if l.parle {
                self.recemment.insert(l.nom.clone(), now);
            }
        }
        // Les absents ne s'accumulent pas.
        self.recemment.retain(|_, t| now.duration_since(*t) < Duration::from_secs(60));
        let mut affichees: Vec<(String, bool, Option<egui::TextureHandle>)> = lignes
            .into_iter()
            .map(|l| {
                let tenu = self
                    .recemment
                    .get(&l.nom)
                    .is_some_and(|t| now.duration_since(*t) < TENUE);
                (l.nom, l.parle || tenu, l.photo)
            })
            .collect();
        if !self.toujours {
            affichees.retain(|(_, parle, _)| *parle);
        }
        if affichees.is_empty() {
            return;
        }

        // Une pilule par personne, ajustée à son pseudo : pas de bloc.
        let police = egui::FontId::proportional(12.5);
        let largeurs: Vec<f32> = affichees
            .iter()
            .map(|(nom, _, _)| {
                ctx.fonts(|f| f.layout_no_wrap(nom.clone(), police.clone(), Color32::WHITE))
                    .size()
                    .x
            })
            .collect();
        let largeur_pilule = |texte: f32| PAD + AVATAR + 6.0 + texte + PAD + 2.0;
        let largeur = largeurs.iter().copied().fold(0.0, f32::max);
        let largeur = largeur_pilule(largeur).ceil();
        let hauteur = (affichees.len() as f32 * (HAUTEUR_LIGNE + ECART) - ECART + 2.0).ceil();
        let ecran = ctx
            .input(|i| i.viewport().monitor_size)
            .unwrap_or(Vec2::new(1920.0, 1080.0));
        let pos = match self.coin {
            Coin::HautGauche => Pos2::new(16.0, 16.0),
            Coin::HautDroite => Pos2::new(ecran.x - largeur - 16.0, 16.0),
            // 64 px du bas : la barre des tâches, quand elle est là.
            Coin::BasGauche => Pos2::new(16.0, ecran.y - hauteur - 64.0),
            Coin::BasDroite => Pos2::new(ecran.x - largeur - 16.0, ecran.y - hauteur - 64.0),
        };
        let anime = affichees.iter().any(|(_, parle, _)| *parle);
        let id = egui::ViewportId::from_hash_of("overlay-qui-parle");
        let builder = ViewportBuilder::default()
            .with_title(TITRE)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_taskbar(false)
            .with_resizable(false)
            .with_mouse_passthrough(true)
            .with_active(false)
            .with_inner_size([largeur, hauteur])
            .with_position(pos);
        ctx.show_viewport_immediate(id, builder, move |ctx, _classe| {
            egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
                let rect = ui.max_rect();
                let painter = ui.painter();
                for (i, (nom, parle, photo)) in affichees.iter().enumerate() {
                    let y = rect.top() + 1.0 + i as f32 * (HAUTEUR_LIGNE + ECART);
                    let pilule = Rect::from_min_size(
                        Pos2::new(rect.left(), y),
                        Vec2::new(largeur_pilule(largeurs[i]), HAUTEUR_LIGNE),
                    );
                    // Un voile noir léger, à peine plus dense pour qui parle ;
                    // qui se tait s'estompe.
                    let voile = if *parle { 140 } else { 80 };
                    painter.rect_filled(
                        pilule,
                        CornerRadius::same(14),
                        Color32::from_rgba_unmultiplied(0, 0, 0, voile),
                    );
                    let avatar = Rect::from_min_size(
                        Pos2::new(pilule.left() + PAD, y + (HAUTEUR_LIGNE - AVATAR) / 2.0),
                        Vec2::splat(AVATAR),
                    );
                    ui::paint_avatar(
                        painter,
                        avatar,
                        nom,
                        *parle,
                        photo.as_ref(),
                        Color32::from_rgb(16, 16, 16),
                    );
                    let couleur = if *parle {
                        Color32::WHITE
                    } else {
                        Color32::from_rgba_unmultiplied(255, 255, 255, 150)
                    };
                    painter.text(
                        Pos2::new(avatar.right() + 6.0, y + HAUTEUR_LIGNE / 2.0),
                        egui::Align2::LEFT_CENTER,
                        nom,
                        police.clone(),
                        couleur,
                    );
                }
            });
            if anime {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        });
        // Les jeux en fenêtré plein écran se remettent eux-mêmes tout en
        // haut quand ils reprennent le focus, par-dessus nous — Valorant le
        // fait. Toutes les 300 ms, on repasse au sommet de la pile des
        // fenêtres « toujours au-dessus », sans bouger ni prendre le focus :
        // ce que font toutes les apps de viseur, pour la même raison.
        if now.duration_since(self.reaffirme) > Duration::from_millis(300) {
            self.reaffirme = now;
            win::remettre_au_dessus(TITRE);
        }
    }
}

/// Les réglages de l'overlay, dans ⚙. Rend `true` si quelque chose a changé.
pub fn reglages_ui(ui: &mut egui::Ui, o: &mut Overlay) -> bool {
    let mut change = false;
    if ui
        .checkbox(&mut o.actif, "Afficher qui parle par-dessus le jeu")
        .on_hover_text(
            "une petite fenêtre toujours au-dessus, transparente aux clics, qui ne \
             touche pas au jeu — rien n'est injecté, l'anti-triche n'y voit rien. \
             Visible dès que ki-chat n'est pas au premier plan et qu'on est en vocal.",
        )
        .changed()
    {
        change = true;
    }
    if o.actif {
        ui.horizontal(|ui| {
            ui.label(RichText::new("position").color(TEXT_DIM).size(12.5));
            egui::ComboBox::from_id_salt("overlay_coin")
                .width(170.0)
                .selected_text(RichText::new(o.coin.libelle()).color(TEXT))
                .show_ui(ui, |ui| {
                    for (c, _, l) in Coin::TOUS {
                        if ui.selectable_label(o.coin == c, l).clicked() {
                            o.coin = c;
                            change = true;
                        }
                    }
                });
        });
        if ui
            .checkbox(&mut o.toujours, "Montrer aussi ceux qui se taisent (estompés)")
            .on_hover_text(
                "décoché : seuls ceux qui parlent apparaissent, et l'overlay disparaît au \
                 silence — le plus discret",
            )
            .changed()
        {
            change = true;
        }
    }
    if o.exclusif {
        ui.add_space(4.0);
        ui::banner(
            ui,
            Tone::Warn,
            "en ce moment, un jeu tourne en plein écran exclusif : rien ne peut s'afficher \
             au-dessus, l'overlay compris. Dans le jeu, mode d'affichage → « Fenêtré plein \
             écran » (Valorant : Paramètres → Vidéo → Général).",
            false,
        );
    }
    ui::hint(
        ui,
        "se voit sur un jeu en fenêtré plein écran (sans bordure) ; en plein écran exclusif — \
         le mode « Plein écran » de Valorant, par exemple — rien ne peut passer au-dessus \
         du jeu, pas même la bulle de volume de Windows",
    );
    change
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_coins_font_l_aller_retour_par_leur_identifiant() {
        for (c, id, _) in Coin::TOUS {
            assert_eq!(Coin::depuis(id), c);
            assert_eq!(c.id(), id);
        }
        assert_eq!(Coin::depuis("n'importe quoi"), Coin::HautGauche);
    }

    #[test]
    fn les_reglages_se_relisent() {
        let mut memoire = std::collections::HashMap::new();
        struct M<'a>(&'a mut std::collections::HashMap<String, String>);
        impl eframe::Storage for M<'_> {
            fn get_string(&self, key: &str) -> Option<String> {
                self.0.get(key).cloned()
            }
            fn set_string(&mut self, key: &str, value: String) {
                self.0.insert(key.to_string(), value);
            }
            fn flush(&mut self) {}
        }
        let mut o = Overlay::load(|_, d| d.to_string());
        assert!(o.actif && !o.toujours && !o.exclusif);
        o.actif = false;
        o.coin = Coin::BasDroite;
        o.toujours = true;
        o.save(&mut M(&mut memoire));
        let relu = Overlay::load(|k, d| memoire.get(k).cloned().unwrap_or_else(|| d.to_string()));
        assert!(!relu.actif && relu.toujours);
        assert_eq!(relu.coin, Coin::BasDroite);
    }
}
