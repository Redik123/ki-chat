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

/// Le fond des fenêtres : un noir presque pur. Sur l'overlay, on ne le voit
/// qu'au ras des ronds (la fenêtre est découpée à leur forme), où il fond
/// le lissage des bords vers du sombre ; sur la fenêtre principale, jamais
/// — les panneaux la couvrent.
pub const CLE: [f32; 4] = [1.0 / 255.0, 0.0, 1.0 / 255.0, 1.0];

/// Une forme de la fenêtre de l'overlay, en points, depuis son coin haut
/// gauche : ce que Windows en garde.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Forme {
    Rond { cx: f32, cy: f32, r: f32 },
    Pilule { x: f32, y: f32, w: f32, h: f32, rayon: f32 },
}

/// Les trois gestes Windows que l'overlay fait lui-même : se remettre au
/// sommet de la pile des fenêtres « toujours au-dessus » (les jeux s'y
/// remettent aussi, à chaque reprise du focus — la dernière demande gagne),
/// se découper à la forme de ses ronds, et savoir qui est au premier plan.
#[cfg(windows)]
mod win {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Gdi::{
        CombineRgn, CreateEllipticRgn, CreateRectRgn, CreateRoundRectRgn, DeleteObject, HGDIOBJ,
        SetWindowRgn, RGN_OR,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, GetForegroundWindow, GetWindowLongW, GetWindowRect, GetWindowTextW,
        SetWindowLongW, SetWindowPos, GWL_EXSTYLE, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOSIZE, WS_EX_LAYERED, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    };

    fn fenetre(titre: &str) -> Option<HWND> {
        let large: Vec<u16> = titre.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe { FindWindowW(None, PCWSTR(large.as_ptr())).ok().filter(|h| !h.is_invalid()) }
    }

    /// Remet la fenêtre `titre` au sommet des « toujours au-dessus », sans
    /// la déplacer ni l'activer, en s'assurant des deux styles qui font
    /// passer les clics à travers. `false` si elle n'existe pas (encore).
    pub fn remettre_au_dessus(titre: &str) -> bool {
        let Some(hwnd) = fenetre(titre) else { return false };
        unsafe {
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            let voulu = ex | WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0;
            if voulu != ex {
                SetWindowLongW(hwnd, GWL_EXSTYLE, voulu as i32);
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

    /// Découpe la fenêtre à la forme de ses ronds et pilules : en dehors,
    /// elle n'existe pas — ni à l'écran, ni pour la souris.
    ///
    /// Pourquoi une découpe : « les clics passent à travers » se fait à
    /// Windows par le style de fenêtre en couches, et une fenêtre en couches
    /// ignore la transparence par pixel de ce qu'on y dessine — le fond
    /// sortait noir. La clé de couleur, l'autre technique classique, ne
    /// passe pas non plus ici (la sonde `overlay_probe` l'a montré : Windows
    /// convertit les couleurs de cette fenêtre, la clé ne correspond jamais).
    /// La forme, elle, ne dépend d'aucune couleur.
    ///
    /// `taille` : la taille logique de la fenêtre (points), pour convertir
    /// les formes en pixels d'écran d'après sa taille réelle.
    pub fn decouper(titre: &str, taille: (f32, f32), formes: &[super::Forme]) -> bool {
        let Some(hwnd) = fenetre(titre) else { return false };
        unsafe {
            let mut r = RECT::default();
            if GetWindowRect(hwnd, &mut r).is_err() {
                return false;
            }
            let px = ((r.right - r.left) as f32, (r.bottom - r.top) as f32);
            let echelle = if taille.0 > 0.0 { px.0 / taille.0 } else { 1.0 };
            let region = CreateRectRgn(0, 0, 0, 0);
            for f in formes {
                let part = match *f {
                    super::Forme::Rond { cx, cy, r } => CreateEllipticRgn(
                        ((cx - r) * echelle).floor() as i32,
                        ((cy - r) * echelle).floor() as i32,
                        ((cx + r) * echelle).ceil() as i32,
                        ((cy + r) * echelle).ceil() as i32,
                    ),
                    super::Forme::Pilule { x, y, w, h, rayon } => CreateRoundRectRgn(
                        (x * echelle).floor() as i32,
                        (y * echelle).floor() as i32,
                        ((x + w) * echelle).ceil() as i32,
                        ((y + h) * echelle).ceil() as i32,
                        (2.0 * rayon * echelle) as i32,
                        (2.0 * rayon * echelle) as i32,
                    ),
                };
                CombineRgn(Some(region), Some(region), Some(part), RGN_OR);
                let _ = DeleteObject(HGDIOBJ(part.0));
            }
            // La région appartient désormais à la fenêtre : on ne la libère
            // pas nous-mêmes.
            SetWindowRgn(hwnd, Some(region), true) != 0
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
    pub fn decouper(_titre: &str, _taille: (f32, f32), _formes: &[super::Forme]) -> bool {
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
    /// Le pseudo à côté du rond, dans une pilule ; sinon le rond seul.
    pub pseudo: bool,
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
    /// La découpe appliquée à la fenêtre, pour ne la refaire qu'au
    /// changement — la refaire à chaque image ferait clignoter.
    decoupe: Option<Vec<Forme>>,
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
/// Le rond seul, sans pseudo : un peu plus grand, il est tout l'overlay.
const AVATAR_SEUL: f32 = 32.0;
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
            pseudo: get("overlay_pseudo", "off") == "on",
            recemment: HashMap::new(),
            exclusif: false,
            sonde: Instant::now(),
            reaffirme: Instant::now(),
            devant: (String::new(), false),
            decoupe: None,
        }
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        storage.set_string("overlay", if self.actif { "on" } else { "off" }.into());
        storage.set_string("overlay_coin", self.coin.id().into());
        storage.set_string("overlay_toujours", if self.toujours { "on" } else { "off" }.into());
        storage.set_string("overlay_pseudo", if self.pseudo { "on" } else { "off" }.into());
    }

    /// Montre (ou non) l'overlay pour cette image. `focus_principal` : la
    /// fenêtre de ki-chat a le clavier — alors on la voit, pas besoin de
    /// doublon. Sans ligne (hors vocal), rien.
    pub fn montrer(&mut self, ctx: &egui::Context, lignes: Vec<Ligne>, focus_principal: bool) {
        if !self.actif || focus_principal || lignes.is_empty() {
            // La fenêtre va disparaître : sa découpe repartira de zéro.
            self.decoupe = None;
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
            self.decoupe = None;
            return;
        }

        // Sans pseudo : un rond par personne, rien d'autre. Avec : une
        // pilule ajustée à son pseudo — jamais un bloc.
        let avec_pseudo = self.pseudo;
        let police = egui::FontId::proportional(12.5);
        let largeurs: Vec<f32> = if avec_pseudo {
            affichees
                .iter()
                .map(|(nom, _, _)| {
                    ctx.fonts(|f| f.layout_no_wrap(nom.clone(), police.clone(), Color32::WHITE))
                        .size()
                        .x
                })
                .collect()
        } else {
            vec![0.0; affichees.len()]
        };
        let largeur_pilule = move |texte: f32| PAD + AVATAR + 6.0 + texte + PAD + 2.0;
        let (taille_avatar, hauteur_ligne) =
            if avec_pseudo { (AVATAR, HAUTEUR_LIGNE) } else { (AVATAR_SEUL, AVATAR_SEUL + 4.0) };
        let largeur = if avec_pseudo {
            largeur_pilule(largeurs.iter().copied().fold(0.0, f32::max)).ceil()
        } else {
            hauteur_ligne
        };
        let hauteur = (affichees.len() as f32 * (hauteur_ligne + ECART) - ECART + 2.0).ceil();
        // La forme de la fenêtre : la même géométrie que le dessin, depuis
        // le coin haut gauche.
        let formes: Vec<Forme> = (0..affichees.len())
            .map(|i| {
                let y = 1.0 + i as f32 * (hauteur_ligne + ECART);
                if avec_pseudo {
                    Forme::Pilule {
                        x: 0.0,
                        y,
                        w: largeur_pilule(largeurs[i]),
                        h: HAUTEUR_LIGNE,
                        rayon: 14.0,
                    }
                } else {
                    Forme::Rond {
                        cx: 2.0 + taille_avatar / 2.0,
                        cy: y + 2.0 + taille_avatar / 2.0,
                        r: taille_avatar / 2.0,
                    }
                }
            })
            .collect();
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
        // Pas de « transparent » ici : la transparence vient de la clé de
        // couleur (voir `win::remettre_au_dessus`), la seule qui marche avec
        // les clics à travers.
        let builder = ViewportBuilder::default()
            .with_title(TITRE)
            .with_decorations(false)
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
                    let y = rect.top() + 1.0 + i as f32 * (hauteur_ligne + ECART);
                    if !avec_pseudo {
                        // Le rond seul : la photo ou l'initiale, l'anneau
                        // vert quand ça parle, un voile quand ça se tait.
                        let avatar = Rect::from_min_size(
                            Pos2::new(rect.left() + 2.0, y + 2.0),
                            Vec2::splat(taille_avatar),
                        );
                        ui::paint_avatar(
                            painter,
                            avatar,
                            nom,
                            *parle,
                            photo.as_ref(),
                            Color32::TRANSPARENT,
                        );
                        if !*parle {
                            painter.circle_filled(
                                avatar.center(),
                                taille_avatar / 2.0,
                                Color32::from_rgba_unmultiplied(0, 0, 0, 130),
                            );
                        }
                        continue;
                    }
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
        // La découpe suit la forme, et ne se refait qu'au changement.
        if self.decoupe.as_ref() != Some(&formes) && win::decouper(TITRE, (largeur, hauteur), &formes) {
            self.decoupe = Some(formes);
        }
        // Les jeux en fenêtré plein écran se remettent eux-mêmes tout en
        // haut quand ils reprennent le focus, par-dessus nous — Valorant le
        // fait. Toutes les 300 ms, on repasse au sommet de la pile des
        // fenêtres « toujours au-dessus », sans bouger ni prendre le focus :
        // ce que font toutes les apps de viseur, pour la même raison. (La
        // première fois passe tout de suite : le dernier passage date du
        // lancement.)
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
            .checkbox(&mut o.pseudo, "Afficher le pseudo à côté du rond")
            .on_hover_text("décoché : juste le rond de l'avatar, avec l'anneau vert de qui parle")
            .changed()
        {
            change = true;
        }
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
        assert!(o.actif && !o.toujours && !o.pseudo && !o.exclusif);
        o.actif = false;
        o.coin = Coin::BasDroite;
        o.toujours = true;
        o.pseudo = true;
        o.save(&mut M(&mut memoire));
        let relu = Overlay::load(|k, d| memoire.get(k).cloned().unwrap_or_else(|| d.to_string()));
        assert!(!relu.actif && relu.toujours && relu.pseudo);
        assert_eq!(relu.coin, Coin::BasDroite);
    }
}
