//! La zone de notification : ki-chat se réduit à côté de l'horloge au lieu
//! de quitter quand on ferme sa fenêtre.
//!
//! Le besoin : continuer à recevoir les messages, les sons, les pokes et
//! le vocal quand on a « fermé » ki-chat pour jouer — le réflexe Discord.
//! Tout ce que ki-chat fait en fond passe par `update()` (traitement des
//! messages, reprise réseau, push-to-talk) : la fenêtre doit donc rester
//! une fenêtre que Windows accepte de repeindre. La sonde
//! `examples/zone_probe.rs`, mesurée sur machine, a tranché :
//!
//! - `ViewportCommand::Visible(false)` **gèle `update()`** (plus de
//!   `WM_PAINT` sans `WS_VISIBLE`), et `Visible(true)` envoyé par egui ne
//!   rouvre même pas — seul un `ShowWindow` natif y arrive. Proscrit.
//! - Fenêtre **cloakée** par le compositeur (`DWMWA_CLOAK`) : invisible,
//!   exclue d'Alt+Tab (c'est le mécanisme des bureaux virtuels), et
//!   `update()` continue de tourner. C'est la voie principale.
//! - Fenêtre **minimisée** : `update()` vit aussi ; c'est le repli si le
//!   compositeur refuse le cloak. Elle reste dans Alt+Tab.
//!
//! Dans les deux cas le bouton de la barre des tâches est retiré par
//! `ITaskbarList::DeleteTab`, et remis par `AddTab` à la réouverture. On
//! minimise **aussi** en cloakant : une fenêtre cloakée garde le focus
//! clavier, et les touches du joueur seraient allées dans la saisie d'un
//! ki-chat invisible ; minimiser le rend à la fenêtre suivante.
//!
//! La réduction se fait en deux temps, dans l'ordre que la sonde a mesuré :
//! `ViewportCommand::Minimized(true)` d'abord — une commande qu'eframe
//! n'applique qu'après la peinture — puis, à l'image où winit rapporte la
//! fenêtre minimisée, la partie native (cloak, retrait de l'onglet). Poser
//! le natif tout de suite, c'est retirer l'onglet d'une fenêtre que le
//! shell s'apprête à montrer et minimiser : une séquence jamais observée.
//!
//! Windows peut aussi rendre la fenêtre sans nous : Alt+Tab quand elle est
//! seulement minimisée, « Basculer vers » du Gestionnaire des tâches, un
//! `SwitchToThisWindow` d'un autre programme. winit relit `IsIconic` à
//! chaque image : réduite et plus minimisée, la fenêtre est revenue seule,
//! et on rouvre proprement (uncloak, onglet, icône au repos) — sinon la
//! croix resterait inerte et une fenêtre cloakée prendrait le clavier du
//! joueur.
//!
//! L'icône (`tray-icon`) et l'`ITaskbarList` vivent sur le fil de la boucle
//! d'événements, créés dans `KiApp::new` : sous Windows l'icône est une
//! fenêtre-message servie par la boucle de ce fil. Ses événements passent
//! par un canal à nous, et chaque événement réveille la fenêtre
//! (`request_repaint`) — sans quoi il attendrait l'image suivante, qui
//! réduite peut tarder.
//!
//! Le garde-fou : un fil vérifie qu'une fenêtre réduite a bien peint dans
//! les trois dernières secondes. Sinon il le consigne et la rouvre par des
//! appels natifs (uncloak, `ShowWindow`, `AddTab`) — jamais de client
//! silencieux dont on ne peut plus rien faire. Pour ne pas prendre une
//! sortie de veille pour un gel, il somme d'abord (un `request_repaint`),
//! attend une seconde, puis constate. Là où le natif ne peut rien (macOS,
//! HWND inconnu), il ne prétend pas avoir rouvert : il laisse une consigne
//! que `tick` relève dès qu'`update()` repart, en rejouant la réouverture
//! par egui — à ce moment-là, les commandes passent.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use eframe::egui;

/// Argument de ligne de commande : « démarre puis réduis-toi ». Transmis à
/// l'instance suivante par la relance automatique (`secours`) et par le
/// redémarrage d'une mise à jour, quand on était réduit.
pub const ARG_REDUIT: &str = "--reduit";

/// Réduite, la fenêtre redemande une image à ce rythme : assez pour que le
/// garde-fou voie battre le cœur, et pour que les événements de l'icône ne
/// traînent pas.
pub const CADENCE_REDUITE: Duration = Duration::from_millis(250);

/// Sans image depuis ce délai, réduite, le garde-fou somme la fenêtre.
const SILENCE_MAX: Duration = Duration::from_secs(3);
/// Après la sommation, le temps laissé à `update()` pour répondre.
const DELAI_SOMMATION: Duration = Duration::from_secs(1);
/// `Minimized(true)` envoyée, le temps laissé à winit pour qu'on la voie
/// appliquée ; au-delà, la fenêtre ne se minimise pas et l'on renonce
/// plutôt que de cloaker une fenêtre qui garde le clavier.
#[cfg_attr(not(windows), allow(dead_code))]
const DELAI_MINIMISATION: Duration = Duration::from_secs(1);

/// Taille de l'icône de zone : Windows l'affiche en 16 ou 24 px selon
/// l'échelle, 32 se réduit proprement.
const TAILLE_ICONE: u32 = 32;

/// Ce qu'a fait l'utilisateur sur l'icône.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evenement {
    /// Clic gauche, double-clic, ou « Ouvrir » du menu.
    Ouvrir,
    /// « Quitter » du menu : la fermeture normale doit suivre.
    Quitter,
}

/// Ce que `tick` rapporte à chaque image. Hors Windows, la fenêtre ne se
/// rouvre jamais « dehors » et sa réduction n'est jamais abandonnée.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Constat {
    /// Rien de neuf.
    Rien,
    /// Le garde-fou a rouvert la fenêtre de force : l'application doit se
    /// remettre en « ouverte » (l'icône l'est déjà).
    ReouvertureForcee,
    /// La fenêtre est revenue sans passer par l'icône (Alt+Tab, bouton de
    /// barre des tâches, un autre programme) : elle est rouverte
    /// proprement, l'application se remet en « ouverte ».
    RouverteDehors,
    /// La fenêtre ne s'est pas minimisée dans le délai : la réduction est
    /// abandonnée, la fenêtre reste ouverte.
    ReductionAbandonnee,
    /// Un second ki-chat a été lancé et s'est retiré en nous sonnant
    /// (`instance`) : la fenêtre doit revenir au premier plan, rouverte
    /// si elle était réduite.
    Revelee,
}

/// Le témoin « réduit » lu par `secours::relancer` et
/// `update::relaunch_if_requested`, après que l'application est détruite.
static REDUITE: AtomicBool = AtomicBool::new(false);

/// L'instance suivante doit-elle repartir réduite ?
pub fn repartir_reduit() -> bool {
    REDUITE.load(Ordering::Relaxed)
}

/// `--reduit` figure-t-il dans les arguments ?
pub fn demande_au_demarrage() -> bool {
    demande_dans(std::env::args())
}

fn demande_dans(mut args: impl Iterator<Item = String>) -> bool {
    args.any(|a| a == ARG_REDUIT)
}

/// Le texte du tooltip : le nom seul, ou ce qui attend.
pub fn tooltip(non_lus: u32, poke: Option<&str>) -> String {
    match (non_lus, poke) {
        (0, None) => "ki-chat".to_string(),
        (0, Some(qui)) => format!("ki-chat — {qui} te poke"),
        (1, None) => "ki-chat — 1 non-lu".to_string(),
        (n, None) => format!("ki-chat — {n} non-lus"),
        (1, Some(qui)) => format!("ki-chat — {qui} te poke, 1 non-lu"),
        (n, Some(qui)) => format!("ki-chat — {qui} te poke, {n} non-lus"),
    }
}

/// L'icône avec un point rouge en bas à droite, cerclé de sombre pour se
/// détacher du pictogramme comme d'un fond clair. RGBA non prémultiplié.
pub fn image_pastille(size: u32) -> Vec<u8> {
    let mut rgba = crate::appicon::render(size);
    let k = size as f32 / 32.0;
    let (cx, cy, r) = (size as f32 - 8.0 * k, size as f32 - 8.0 * k, 6.5 * k);
    let liseret = 1.5 * k;
    for py in 0..size {
        for px in 0..size {
            let d = (px as f32 + 0.5 - cx).hypot(py as f32 + 0.5 - cy);
            // Couverture du disque plein (liseret compris), puis du cœur.
            let plein = (0.5 - (d - r)).clamp(0.0, 1.0);
            if plein <= 0.0 {
                continue;
            }
            let coeur = (0.5 - (d - (r - liseret))).clamp(0.0, 1.0);
            let i = ((py * size + px) * 4) as usize;
            let (sr, sg, sb, sa) = (rgba[i] as f32, rgba[i + 1] as f32, rgba[i + 2] as f32, rgba[i + 3] as f32);
            // Le liseret est le fond du pictogramme ; le cœur, un rouge franc.
            let (lr, lg, lb) = (
                crate::appicon::BG[0] as f32,
                crate::appicon::BG[1] as f32,
                crate::appicon::BG[2] as f32,
            );
            let (rr, rg, rb) = (0xe5_u8 as f32, 0x3e_u8 as f32, 0x3e_u8 as f32);
            let (pr, pg, pb) = (
                lr + (rr - lr) * coeur,
                lg + (rg - lg) * coeur,
                lb + (rb - lb) * coeur,
            );
            let m = |s: f32, p: f32| (s + (p - s) * plein).round().clamp(0.0, 255.0) as u8;
            rgba[i] = m(sr, pr);
            rgba[i + 1] = m(sg, pg);
            rgba[i + 2] = m(sb, pb);
            rgba[i + 3] = m(sa, 255.0);
        }
    }
    rgba
}

/// Ce que le garde-fou partage avec l'application : un cœur qui bat, et
/// ce qu'il a fait de son côté.
struct Veille {
    /// Millisecondes écoulées, depuis `depart`, à la dernière image.
    derniere_image_ms: AtomicU64,
    depart: Instant,
    reduite: AtomicBool,
    /// Le garde-fou a rouvert la fenêtre lui-même ; `tick` le relève.
    forcee: AtomicBool,
    /// Un second lancement nous a sonnés ; `tick` le relève.
    revele: AtomicBool,
    hwnd: AtomicIsize,
    arret: AtomicBool,
}

impl Veille {
    fn battre(&self) {
        let ms = self.depart.elapsed().as_millis() as u64;
        self.derniere_image_ms.store(ms, Ordering::Relaxed);
    }

    fn silence(&self) -> Duration {
        let ms = self.derniere_image_ms.load(Ordering::Relaxed);
        self.depart.elapsed().saturating_sub(Duration::from_millis(ms))
    }
}

/// Le fil du garde-fou : tourne toute la vie de l'application, ne regarde
/// que quand la fenêtre est réduite.
fn veiller(veille: Arc<Veille>, ctx: egui::Context) {
    // L'oreille des seconds lancements : elle vit avec ce fil, toute la
    // vie de l'application, réduite ou non.
    let reveil = crate::instance::Reveil::creer();
    while !veille.arret.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(500));
        if reveil.sonne() {
            ki_voice::journal("zone : un second ki-chat nous a sonnés — retour au premier plan".to_string());
            veille.revele.store(true, Ordering::Relaxed);
            ctx.request_repaint();
        }
        // Une consigne attend déjà que `tick` la relève : inutile de
        // sommer encore, `update()` ne répondra pas mieux la deuxième fois.
        if veille.forcee.load(Ordering::Relaxed) {
            continue;
        }
        if !veille.reduite.load(Ordering::Relaxed) || veille.silence() < SILENCE_MAX {
            continue;
        }
        // Sommation : une sortie de veille ou un fil réveillé tard
        // répondent à ça ; un `update()` gelé, non.
        ctx.request_repaint();
        std::thread::sleep(DELAI_SOMMATION);
        if !veille.reduite.load(Ordering::Relaxed) || veille.silence() < DELAI_SOMMATION {
            continue;
        }
        let silence = veille.silence();
        ki_voice::journal(format!(
            "zone : fenêtre réduite sans image depuis {:.1} s — réouverture forcée",
            silence.as_secs_f32()
        ));
        tracing::error!("zone : fenêtre réduite muette depuis {silence:?}, réouverture forcée");
        let hwnd = veille.hwnd.load(Ordering::Relaxed);
        // Le natif rouvre à coup sûr là où il existe ; ailleurs on ne
        // déclare rien de rouvert — ce serait un client perdu, l'icône
        // croyant la fenêtre visible. Dans les deux cas la consigne est
        // laissée à `tick`, qui rejouera la réouverture par egui.
        if natif::rouvrir_de_force(hwnd) {
            veille.reduite.store(false, Ordering::Relaxed);
            REDUITE.store(false, Ordering::Relaxed);
        } else {
            ki_voice::journal("zone : rien à rouvrir en natif ici — à update() de le faire quand elle repart".to_string());
        }
        veille.forcee.store(true, Ordering::Relaxed);
        ctx.request_repaint();
    }
}

/// Comment la fenêtre a été réduite — pour savoir comment la rouvrir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Cloakée par le compositeur, et minimisée pour rendre le focus.
    #[cfg(windows)]
    Cloak,
    /// Minimisée seulement (le cloak a échoué, ou pas de compositeur).
    #[cfg(not(target_os = "macos"))]
    Minimisee,
    /// macOS : fenêtre retirée (`Visible(false)`), winit y repeint quand
    /// même.
    #[cfg(target_os = "macos")]
    Cachee,
}

/// Où en est la fenêtre.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Etat {
    Ouverte,
    /// `Minimized(true)` est partie ; la partie native (cloak, retrait de
    /// l'onglet) attend d'avoir vu la fenêtre minimisée — l'ordre mesuré
    /// par la sonde. `depuis` : pour renoncer si ça ne vient pas.
    #[cfg(not(target_os = "macos"))]
    Minimisation { depuis: Instant },
    /// Réduite pour de bon, et comment.
    Reduite(Mode),
}

/// L'icône de zone, son menu, et l'état « réduite » de la fenêtre.
pub struct Zone {
    plateforme: plateforme::Icone,
    rx: Receiver<Evenement>,
    veille: Arc<Veille>,
    hwnd: isize,
    etat: Etat,
    /// Ce que l'icône montre en ce moment, pour ne la changer qu'au besoin.
    signal: (u32, Option<String>),
}

impl Zone {
    /// Crée l'icône et le garde-fou. À appeler depuis `KiApp::new`, sur le
    /// fil de la boucle d'événements. `hwnd` : la fenêtre principale, telle
    /// que `CreationContext` la donne (0 si inconnue : `attacher` plus tard).
    pub fn creer(hwnd: isize, ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        let plateforme = plateforme::Icone::creer(tx, ctx.clone());
        let zone = Self::assembler(plateforme, rx, hwnd);
        let v = zone.veille.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("ki-zone-veille".into())
            .spawn(move || veiller(v, ctx))
        {
            tracing::warn!("zone : garde-fou non démarré : {e}");
        }
        zone
    }

    fn assembler(plateforme: plateforme::Icone, rx: Receiver<Evenement>, hwnd: isize) -> Self {
        let veille = Arc::new(Veille {
            derniere_image_ms: AtomicU64::new(0),
            depart: Instant::now(),
            reduite: AtomicBool::new(false),
            forcee: AtomicBool::new(false),
            revele: AtomicBool::new(false),
            hwnd: AtomicIsize::new(hwnd),
            arret: AtomicBool::new(false),
        });
        Self { plateforme, rx, veille, hwnd, etat: Etat::Ouverte, signal: (0, None) }
    }

    /// Une zone sans icône ni garde-fou, pour éprouver la logique d'état
    /// sans toucher au shell.
    #[cfg(test)]
    fn factice() -> Self {
        let (_tx, rx) = mpsc::channel();
        Self::assembler(plateforme::Icone::muette(), rx, 0)
    }

    /// Le HWND, s'il n'était pas connu à la création (celui de
    /// `eframe::Frame` — la sonde a montré qu'il est le même).
    pub fn attacher(&mut self, hwnd: isize) {
        if self.hwnd == 0 && hwnd != 0 {
            self.hwnd = hwnd;
            self.veille.hwnd.store(hwnd, Ordering::Relaxed);
        }
    }

    pub fn reduite(&self) -> bool {
        self.etat != Etat::Ouverte
    }

    /// La fenêtre est-elle montrée par le système ? eframe la crée cachée
    /// et ne la montre qu'après sa première image : réduire avant, c'est
    /// retirer un onglet qui n'existe pas encore — le shell le créerait à
    /// l'apparition, et un clic dessus restaurerait une fenêtre cloakée.
    pub fn fenetre_visible(&self, ctx: &egui::Context) -> bool {
        #[cfg(windows)]
        if self.hwnd != 0 {
            return natif::visible(self.hwnd);
        }
        // Sans HWND : une image achevée, la fenêtre a été montrée.
        ctx.cumulative_pass_nr() >= 1
    }

    /// Y a-t-il une icône pour revenir ? Sans elle (plateforme sans zone,
    /// ou création refusée par le shell), la croix doit fermer comme avant :
    /// réduire sans chemin de retour, ce serait un client perdu.
    pub fn disponible(&self) -> bool {
        self.plateforme.disponible()
    }

    /// Réduit la fenêtre. Sous Windows, elle est d'abord minimisée ; la
    /// partie native (cloak + retrait de l'onglet, ou onglet seul si le
    /// compositeur refuse) suit dans `tick`, une fois la minimisation vue.
    /// Déjà réduite, on rejoue la séquence depuis le début plutôt que de
    /// se taire : si l'on nous le demande, c'est que la fenêtre est visible.
    pub fn reduire(&mut self, ctx: &egui::Context) {
        match self.etat {
            Etat::Ouverte => {}
            #[cfg(not(target_os = "macos"))]
            Etat::Minimisation { .. } => return,
            Etat::Reduite(mode) => {
                ki_voice::journal("zone : réduction redemandée alors qu'on se croyait réduit — on repart du début".to_string());
                self.plateforme.rouvrir(self.hwnd, mode, ctx);
            }
        }
        self.etat = self.plateforme.reduire(self.hwnd, ctx);
        match self.etat {
            Etat::Reduite(mode) => ki_voice::journal(format!("zone : fenêtre réduite ({mode:?})")),
            _ => ki_voice::journal("zone : réduction demandée — minimisation d'abord".to_string()),
        }
        self.veille.battre();
        self.veille.reduite.store(true, Ordering::Relaxed);
        REDUITE.store(true, Ordering::Relaxed);
        // Que l'image suivante ne tarde pas : c'est elle qui achève.
        ctx.request_repaint();
    }

    /// Rouvre la fenêtre et remet l'icône au repos. Sans effet si ouverte.
    pub fn rouvrir(&mut self, ctx: &egui::Context) {
        if self.rouvrir_quoi_qu_il_en_soit(ctx) {
            ki_voice::journal("zone : fenêtre rouverte".to_string());
        }
    }

    /// La réouverture, état par état ; vrai s'il y avait quelque chose à
    /// rouvrir. Minimisation en cours : rien de natif n'est posé, mais
    /// remettre l'onglet et sortir de la minimisation ne coûtent rien.
    fn rouvrir_quoi_qu_il_en_soit(&mut self, ctx: &egui::Context) -> bool {
        let mode = match std::mem::replace(&mut self.etat, Etat::Ouverte) {
            Etat::Ouverte => return false,
            #[cfg(not(target_os = "macos"))]
            Etat::Minimisation { .. } => Mode::Minimisee,
            Etat::Reduite(mode) => mode,
        };
        self.veille.reduite.store(false, Ordering::Relaxed);
        REDUITE.store(false, Ordering::Relaxed);
        self.plateforme.rouvrir(self.hwnd, mode, ctx);
        self.signaler(0, None);
        true
    }

    /// À chaque image : le cœur bat ; on relève ce que le garde-fou a pu
    /// faire entre-temps ; on achève une réduction en cours ; et l'on
    /// s'aperçoit d'une fenêtre revenue sans nous.
    pub fn tick(&mut self, ctx: &egui::Context) -> Constat {
        self.veille.battre();
        if self.veille.forcee.swap(false, Ordering::Relaxed) {
            // Sous Windows le natif a déjà rouvert, et ceci est idempotent ;
            // ailleurs c'est ici que la fenêtre revient pour de bon —
            // `update()` tourne à nouveau, les commandes egui passent.
            self.rouvrir_quoi_qu_il_en_soit(ctx);
            ki_voice::journal("zone : réouverture forcée relevée, fenêtre rouverte".to_string());
            return Constat::ReouvertureForcee;
        }
        if self.veille.revele.swap(false, Ordering::Relaxed) {
            // Rouvrir ou ramener devant : c'est l'application qui sait
            // (elle a le contexte des deux), on ne fait que le dire.
            return Constat::Revelee;
        }
        #[cfg(not(target_os = "macos"))]
        {
            // winit relit `IsIconic` avant chaque image (hors macOS).
            let minimisee = ctx.input(|i| i.viewport().minimized);
            match self.etat {
                Etat::Minimisation { depuis } => {
                    if minimisee == Some(true) {
                        let mode = self.plateforme.achever(self.hwnd);
                        ki_voice::journal(format!("zone : fenêtre réduite ({mode:?})"));
                        self.etat = Etat::Reduite(mode);
                    } else if depuis.elapsed() >= DELAI_MINIMISATION {
                        ki_voice::journal(format!(
                            "zone : la fenêtre ne s'est pas minimisée en {DELAI_MINIMISATION:?} — réduction abandonnée"
                        ));
                        self.etat = Etat::Ouverte;
                        self.veille.reduite.store(false, Ordering::Relaxed);
                        REDUITE.store(false, Ordering::Relaxed);
                        return Constat::ReductionAbandonnee;
                    }
                }
                Etat::Reduite(_) if minimisee == Some(false) => {
                    ki_voice::journal("zone : fenêtre restaurée de l'extérieur (Alt+Tab, barre des tâches…) — rouverte".to_string());
                    self.rouvrir_quoi_qu_il_en_soit(ctx);
                    return Constat::RouverteDehors;
                }
                _ => {}
            }
        }
        if self.reduite() {
            ctx.request_repaint_after(CADENCE_REDUITE);
        }
        Constat::Rien
    }

    /// Ce qui est arrivé sur l'icône depuis la dernière image.
    pub fn evenements(&mut self) -> Vec<Evenement> {
        let mut v = Vec::new();
        while let Ok(e) = self.rx.try_recv() {
            v.push(e);
        }
        v
    }

    /// Pastille et tooltip selon ce qui attend ; ne touche à l'icône que
    /// si ça change.
    pub fn signaler(&mut self, non_lus: u32, poke: Option<&str>) {
        let signal = (non_lus, poke.map(str::to_string));
        if signal == self.signal {
            return;
        }
        self.plateforme.signaler(non_lus > 0 || poke.is_some(), &tooltip(non_lus, poke));
        self.signal = signal;
    }
}

impl Zone {
    /// La fermeture commence : l'icône quitte la zone de notification tout
    /// de suite — ki-chat a l'air fermé parce qu'il l'est —, et le
    /// garde-fou s'arrête, son oreille avec : un lancement qui suit trouve
    /// le verrou rendu et démarre, sans sonner une instance qui s'en va.
    pub fn retirer(&mut self) {
        self.veille.arret.store(true, Ordering::Relaxed);
        self.plateforme.retirer();
    }
}

impl Drop for Zone {
    fn drop(&mut self) {
        self.veille.arret.store(true, Ordering::Relaxed);
    }
}

/// L'icône réelle : Windows et macOS par `tray-icon`, rien ailleurs.
#[cfg(any(windows, target_os = "macos"))]
mod plateforme {
    use super::{Etat, Evenement, Mode, OnceLock, Sender, TAILLE_ICONE};
    #[cfg(windows)]
    use super::Instant;
    use eframe::egui;
    use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    /// Les identifiants du menu, lus par le gestionnaire d'événements qui,
    /// lui, n'a pas accès à `Icone` (il est posé une fois pour tout le
    /// processus).
    static IDS: OnceLock<(MenuId, MenuId)> = OnceLock::new();

    pub struct Icone {
        icone: Option<TrayIcon>,
        #[cfg(windows)]
        barre: Option<super::natif::Barre>,
    }

    fn icone(pastille: bool) -> Option<Icon> {
        let rgba = if pastille { super::image_pastille(TAILLE_ICONE) } else { crate::appicon::render(TAILLE_ICONE) };
        match Icon::from_rgba(rgba, TAILLE_ICONE, TAILLE_ICONE) {
            Ok(i) => Some(i),
            Err(e) => {
                tracing::warn!("zone : icône impossible : {e}");
                None
            }
        }
    }

    impl Icone {
        /// Retire l'icône de la zone de notification (à la fermeture).
        pub fn retirer(&mut self) {
            self.icone = None;
        }

        pub fn creer(tx: Sender<Evenement>, ctx: egui::Context) -> Self {
            let ouvrir = MenuItem::new("Ouvrir ki-chat", true, None);
            let quitter = MenuItem::new("Quitter", true, None);
            let _ = IDS.set((ouvrir.id().clone(), quitter.id().clone()));
            let menu = Menu::new();
            if let Err(e) = menu.append(&ouvrir).and_then(|()| menu.append(&quitter)) {
                tracing::warn!("zone : menu de l'icône incomplet : {e}");
            }

            // Les gestionnaires : pousser dans notre canal et réveiller la
            // fenêtre, rien d'autre — ils tournent pendant le dispatch du
            // message, sur le fil de la boucle. Une fois posés, les canaux
            // de `tray-icon` ne reçoivent plus rien : tout passe par ici.
            let (tx_menu, ctx_menu) = (tx.clone(), ctx.clone());
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                let Some((ouvrir, quitter)) = IDS.get() else { return };
                let ev = if e.id == *ouvrir {
                    Evenement::Ouvrir
                } else if e.id == *quitter {
                    Evenement::Quitter
                } else {
                    return;
                };
                let _ = tx_menu.send(ev);
                ctx_menu.request_repaint();
            }));
            TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
                let ouvrir = matches!(
                    e,
                    TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }
                        | TrayIconEvent::DoubleClick { button: MouseButton::Left, .. }
                );
                if ouvrir {
                    let _ = tx.send(Evenement::Ouvrir);
                    ctx.request_repaint();
                }
            }));

            let mut builder = TrayIconBuilder::new()
                .with_id("ki-chat")
                .with_tooltip("ki-chat")
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false);
            if let Some(i) = icone(false) {
                builder = builder.with_icon(i);
            }
            let icone = match builder.build() {
                Ok(i) => Some(i),
                Err(e) => {
                    tracing::warn!("zone : icône de la zone de notification impossible : {e}");
                    ki_voice::journal(format!("zone : icône impossible : {e}"));
                    None
                }
            };
            Self {
                icone,
                #[cfg(windows)]
                barre: super::natif::Barre::creer(),
            }
        }

        pub fn disponible(&self) -> bool {
            self.icone.is_some()
        }

        pub fn signaler(&self, pastille: bool, tooltip: &str) {
            let Some(i) = &self.icone else { return };
            if let Some(img) = icone(pastille) {
                let _ = i.set_icon(Some(img));
            }
            let _ = i.set_tooltip(Some(tooltip));
        }

        /// Sans icône ni barre : pour les essais.
        #[cfg(test)]
        pub fn muette() -> Self {
            Self {
                icone: None,
                #[cfg(windows)]
                barre: None,
            }
        }

        /// Premier temps : minimiser. Le focus part ailleurs, et la fenêtre
        /// iconique n'a plus de surface à cacher. La commande n'est
        /// appliquée qu'après la peinture (`SW_SHOW` puis `SW_MINIMIZE`
        /// chez winit) : le natif attend de la voir passée.
        #[cfg(windows)]
        pub fn reduire(&self, _hwnd: isize, ctx: &egui::Context) -> Etat {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            Etat::Minimisation { depuis: Instant::now() }
        }

        /// Second temps, la fenêtre vue minimisée : le cloak, qui l'exclut
        /// d'Alt+Tab, puis l'onglet retiré — la séquence mesurée par la
        /// sonde (DeleteTab après la minimisation effective).
        #[cfg(windows)]
        pub fn achever(&self, hwnd: isize) -> Mode {
            let mode = if super::natif::cloak(hwnd, true) { Mode::Cloak } else { Mode::Minimisee };
            if let Some(b) = &self.barre {
                b.retirer(hwnd);
            }
            mode
        }

        #[cfg(windows)]
        pub fn rouvrir(&self, hwnd: isize, mode: Mode, ctx: &egui::Context) {
            if mode == Mode::Cloak {
                super::natif::cloak(hwnd, false);
            }
            if let Some(b) = &self.barre {
                b.remettre(hwnd);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            // Par sécurité : si la fenêtre n'est pas visible (ce qui ne
            // devrait jamais arriver par ces chemins), un `ShowWindow` natif
            // — le seul qui rouvre à coup sûr.
            super::natif::montrer_si_cachee(hwnd);
        }

        #[cfg(target_os = "macos")]
        pub fn reduire(&self, _hwnd: isize, ctx: &egui::Context) -> Etat {
            // winit distribue les repeints lui-même sur macOS : une fenêtre
            // retirée (`orderOut`) laisse `update()` tourner. À confirmer
            // sur machine. Rien de natif à poser ensuite : réduite d'un coup.
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            Etat::Reduite(Mode::Cachee)
        }

        #[cfg(target_os = "macos")]
        pub fn rouvrir(&self, _hwnd: isize, _mode: Mode, ctx: &egui::Context) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }
}

/// Ailleurs (Linux) : pas d'icône, la croix ferme comme avant.
#[cfg(not(any(windows, target_os = "macos")))]
mod plateforme {
    use super::{Etat, Evenement, Instant, Mode, Sender};
    use eframe::egui;

    pub struct Icone;

    impl Icone {
        pub fn creer(_tx: Sender<Evenement>, _ctx: egui::Context) -> Self {
            Self
        }
        pub fn retirer(&mut self) {}
        #[cfg(test)]
        pub fn muette() -> Self {
            Self
        }
        pub fn disponible(&self) -> bool {
            false
        }
        pub fn signaler(&self, _pastille: bool, _tooltip: &str) {}
        pub fn reduire(&self, _hwnd: isize, ctx: &egui::Context) -> Etat {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            Etat::Minimisation { depuis: Instant::now() }
        }
        pub fn achever(&self, _hwnd: isize) -> Mode {
            Mode::Minimisee
        }
        pub fn rouvrir(&self, _hwnd: isize, _mode: Mode, ctx: &egui::Context) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }
}

/// Les appels Win32 : cloak, `ShowWindow`, `ITaskbarList`. Repris tels
/// quels de la sonde, où ils ont été vus fonctionner.
#[cfg(windows)]
mod natif {
    use windows::core::BOOL;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{ITaskbarList, TaskbarList};
    use windows::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindowVisible, ShowWindow, SW_RESTORE, SW_SHOW};

    fn h(hwnd: isize) -> HWND {
        HWND(hwnd as *mut core::ffi::c_void)
    }

    /// `ITaskbarList`, lié au fil qui l'a créé (un objet COM ne voyage pas
    /// entre fils sans marshaling) : un par fil qui en a besoin.
    pub struct Barre {
        liste: ITaskbarList,
    }

    impl Barre {
        pub fn creer() -> Option<Self> {
            // Le fil de la boucle winit est déjà un appartement (winit y
            // appelle OleInitialize) : S_FALSE, sans conséquence. Sur le fil
            // du garde-fou, c'est nous qui l'ouvrons.
            let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            let liste: ITaskbarList = match unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER) } {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!("zone : ITaskbarList indisponible : {e}");
                    return None;
                }
            };
            if let Err(e) = unsafe { liste.HrInit() } {
                tracing::warn!("zone : ITaskbarList::HrInit : {e}");
                return None;
            }
            Some(Self { liste })
        }

        pub fn retirer(&self, hwnd: isize) {
            if hwnd == 0 {
                return;
            }
            if let Err(e) = unsafe { self.liste.DeleteTab(h(hwnd)) } {
                tracing::warn!("zone : DeleteTab : {e}");
            }
        }

        pub fn remettre(&self, hwnd: isize) {
            if hwnd == 0 {
                return;
            }
            if let Err(e) = unsafe { self.liste.AddTab(h(hwnd)) } {
                tracing::warn!("zone : AddTab : {e}");
            }
        }
    }

    /// Cache (ou remontre) la fenêtre par le compositeur. Vrai si l'appel
    /// a été accepté.
    pub fn cloak(hwnd: isize, oui: bool) -> bool {
        if hwnd == 0 {
            return false;
        }
        let val = BOOL::from(oui);
        let r = unsafe {
            DwmSetWindowAttribute(
                h(hwnd),
                DWMWA_CLOAK,
                &val as *const BOOL as *const _,
                std::mem::size_of::<BOOL>() as u32,
            )
        };
        if let Err(e) = &r {
            tracing::warn!("zone : DWMWA_CLOAK({oui}) : {e}");
        }
        r.is_ok()
    }

    /// La fenêtre est-elle montrée (`WS_VISIBLE`) ? Faux tant qu'eframe
    /// ne l'a pas révélée après sa première image.
    pub fn visible(hwnd: isize) -> bool {
        hwnd != 0 && unsafe { IsWindowVisible(h(hwnd)).as_bool() }
    }

    /// `ShowWindow(SW_SHOW)` si la fenêtre n'est pas visible.
    pub fn montrer_si_cachee(hwnd: isize) {
        if hwnd == 0 {
            return;
        }
        unsafe {
            if !IsWindowVisible(h(hwnd)).as_bool() {
                ki_voice::journal("zone : fenêtre invisible à la réouverture, ShowWindow(SW_SHOW)".to_string());
                let _ = ShowWindow(h(hwnd), SW_SHOW);
            }
        }
    }

    /// La réouverture du garde-fou, tout en natif — `update()` ne répond
    /// plus, aucune commande egui ne passerait. Vrai si l'on a agi ; sans
    /// HWND, rien n'est possible et l'on ne prétend pas le contraire.
    pub fn rouvrir_de_force(hwnd: isize) -> bool {
        if hwnd == 0 {
            return false;
        }
        cloak(hwnd, false);
        unsafe {
            if IsIconic(h(hwnd)).as_bool() {
                let _ = ShowWindow(h(hwnd), SW_RESTORE);
            }
            if !IsWindowVisible(h(hwnd)).as_bool() {
                let _ = ShowWindow(h(hwnd), SW_SHOW);
            }
        }
        if let Some(b) = Barre::creer() {
            b.remettre(hwnd);
        }
        true
    }
}

#[cfg(not(windows))]
mod natif {
    /// Rien à faire en natif hors Windows : sur macOS la fenêtre cachée
    /// repeint, et si elle ne le fait plus, aucun appel d'ici ne la
    /// rendrait. On répond « pas agi » : c'est `tick`, quand `update()`
    /// repart, qui rejouera `Visible(true)` par egui.
    pub fn rouvrir_de_force(_hwnd: isize) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_tooltip_compte_et_nomme() {
        assert_eq!(tooltip(0, None), "ki-chat");
        assert_eq!(tooltip(1, None), "ki-chat — 1 non-lu");
        assert_eq!(tooltip(3, None), "ki-chat — 3 non-lus");
        assert_eq!(tooltip(0, Some("Léa")), "ki-chat — Léa te poke");
        assert_eq!(tooltip(2, Some("Léa")), "ki-chat — Léa te poke, 2 non-lus");
    }

    #[test]
    fn largument_reduit_se_lit() {
        fn args(l: &[&str]) -> std::vec::IntoIter<String> {
            l.iter().map(|s| s.to_string()).collect::<Vec<_>>().into_iter()
        }
        assert!(!demande_dans(args(&["ki-chat.exe"])));
        assert!(demande_dans(args(&["ki-chat.exe", "--reduit"])));
        assert!(demande_dans(args(&["ki-chat.exe", "--relance", "1", "--reduit"])));
    }

    /// La pastille : un point rouge opaque en bas à droite, et le reste du
    /// pictogramme intact.
    #[test]
    fn la_pastille_est_rouge_en_bas_a_droite() {
        let s = TAILLE_ICONE;
        let base = crate::appicon::render(s);
        let img = image_pastille(s);
        assert_eq!(img.len(), (s * s * 4) as usize);
        let px = |img: &[u8], x: u32, y: u32| {
            let i = ((y * s + x) * 4) as usize;
            [img[i], img[i + 1], img[i + 2], img[i + 3]]
        };
        // Centre du point : rouge, opaque.
        let c = px(&img, s - 8, s - 8);
        assert!(c[0] > 200 && c[1] < 90 && c[2] < 90 && c[3] == 255, "{c:?}");
        // Coin opposé : inchangé.
        assert_eq!(px(&img, 6, 6), px(&base, 6, 6));
        assert_eq!(px(&img, s / 2, 6), px(&base, s / 2, 6));
    }

    /// Une image « à vide », avec l'état de minimisation que winit
    /// rapporterait ; rend ce que `tick` a constaté.
    fn image(zone: &mut Zone, ctx: &egui::Context, minimisee: Option<bool>) -> Constat {
        let mut input = egui::RawInput::default();
        input.viewports.entry(egui::ViewportId::ROOT).or_default().minimized = minimisee;
        let mut constat = Constat::Rien;
        let _ = ctx.run(input, |ctx| constat = zone.tick(ctx));
        constat
    }

    /// Sous Windows (et Linux), le natif n'est posé qu'une fois la
    /// minimisation vue : avant, on est « en réduction » ; après, réduit.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn la_reduction_attend_la_minimisation() {
        let ctx = egui::Context::default();
        let mut zone = Zone::factice();
        assert!(!zone.reduite());
        zone.reduire(&ctx);
        assert!(zone.reduite(), "dès la demande, on se tient pour réduit");
        assert!(matches!(zone.etat, Etat::Minimisation { .. }));
        assert!(zone.veille.reduite.load(Ordering::Relaxed));
        // Pas encore minimisée : rien de natif, on attend.
        assert_eq!(image(&mut zone, &ctx, Some(false)), Constat::Rien);
        assert!(matches!(zone.etat, Etat::Minimisation { .. }));
        // Vue minimisée : la réduction s'achève (sans HWND, pas de cloak).
        assert_eq!(image(&mut zone, &ctx, Some(true)), Constat::Rien);
        assert_eq!(zone.etat, Etat::Reduite(Mode::Minimisee));
        // Elle le reste tant que winit la voit minimisée.
        assert_eq!(image(&mut zone, &ctx, Some(true)), Constat::Rien);
        assert!(zone.reduite());
    }

    /// Réduite, puis restaurée par Alt+Tab ou un autre programme : `tick`
    /// s'en aperçoit, rouvre, et l'application est prévenue.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn la_fenetre_revenue_seule_est_relevee() {
        let ctx = egui::Context::default();
        let mut zone = Zone::factice();
        zone.reduire(&ctx);
        image(&mut zone, &ctx, Some(true));
        assert!(zone.reduite());
        assert_eq!(image(&mut zone, &ctx, Some(false)), Constat::RouverteDehors);
        assert!(!zone.reduite());
        assert!(!zone.veille.reduite.load(Ordering::Relaxed));
        // Et la croix peut réduire à nouveau.
        zone.reduire(&ctx);
        assert!(matches!(zone.etat, Etat::Minimisation { .. }));
    }

    /// Si la minimisation ne vient jamais, on ne cloake pas une fenêtre
    /// qui garde le clavier : on renonce, la fenêtre reste ouverte.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn sans_minimisation_la_reduction_est_abandonnee() {
        let ctx = egui::Context::default();
        let mut zone = Zone::factice();
        zone.reduire(&ctx);
        zone.etat = Etat::Minimisation { depuis: Instant::now() - DELAI_MINIMISATION * 2 };
        assert_eq!(image(&mut zone, &ctx, Some(false)), Constat::ReductionAbandonnee);
        assert!(!zone.reduite());
        assert!(!zone.veille.reduite.load(Ordering::Relaxed));
    }

    /// Le garde-fou a laissé sa consigne : `tick` rejoue la réouverture,
    /// quel que soit l'état où l'on en était.
    /// Un second lancement nous sonne : `tick` le dit une fois, puis se
    /// tait — c'est l'application qui rouvre ou ramène devant.
    #[test]
    fn un_reveil_se_releve_une_fois() {
        let ctx = egui::Context::default();
        let mut zone = Zone::factice();
        zone.veille.revele.store(true, Ordering::Relaxed);
        let mut premier = Constat::Rien;
        let _ = ctx.run(egui::RawInput::default(), |ctx| premier = zone.tick(ctx));
        assert_eq!(premier, Constat::Revelee);
        let mut second = Constat::Rien;
        let _ = ctx.run(egui::RawInput::default(), |ctx| second = zone.tick(ctx));
        assert_eq!(second, Constat::Rien);
    }

    #[test]
    fn la_reouverture_forcee_est_rejouee() {
        let ctx = egui::Context::default();
        let mut zone = Zone::factice();
        zone.reduire(&ctx);
        #[cfg(not(target_os = "macos"))]
        image(&mut zone, &ctx, Some(true));
        zone.veille.forcee.store(true, Ordering::Relaxed);
        assert_eq!(image(&mut zone, &ctx, Some(false)), Constat::ReouvertureForcee);
        assert!(!zone.reduite());
        assert!(!zone.veille.reduite.load(Ordering::Relaxed));
        assert!(!zone.veille.forcee.load(Ordering::Relaxed));
        assert_eq!(image(&mut zone, &ctx, Some(false)), Constat::Rien);
    }

    /// Le cœur bat, le silence se mesure depuis la dernière image.
    #[test]
    fn le_silence_se_mesure_depuis_la_derniere_image() {
        let v = Veille {
            derniere_image_ms: AtomicU64::new(0),
            depart: Instant::now() - Duration::from_secs(10),
            reduite: AtomicBool::new(true),
            forcee: AtomicBool::new(false),
            revele: AtomicBool::new(false),
            hwnd: AtomicIsize::new(0),
            arret: AtomicBool::new(false),
        };
        assert!(v.silence() >= Duration::from_secs(10));
        v.battre();
        assert!(v.silence() < Duration::from_secs(1));
    }
}
