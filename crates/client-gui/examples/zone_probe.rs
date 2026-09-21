//! SONDE, à lancer à la main — rien dans le dépôt ne l'exécute (ni la CI,
//! ni un test, ni un script). Elle OUVRE UNE FENÊTRE pendant ~35 s.
//! `cargo run -p ki-client-gui --example zone_probe`
//!
//! Sonde « réduire dans la zone de notification » : lève sur machine les
//! inconnues du rapport (`update()` tourne-t-il quand la fenêtre est cachée,
//! minimisée sans bouton, ou cachée par le compositeur ? l'icône de zone se
//! crée-t-elle depuis `KiApp::new` ?). Sans aucune interaction humaine : un
//! fil chronométré pilote le scénario, tout est écrit dans un journal, et la
//! fenêtre se ferme toute seule à 32 s.
//!
//! Ce qu'elle a mesuré (Windows 11, eframe 0.32), et qui a fixé `zone.rs` :
//! B (`Visible(false)`) fige `update()` et `Visible(true)` ne rouvre pas ;
//! C (minimisée + DeleteTab) et D (cloak + DeleteTab) laissent `update()`
//! tourner et se rouvrent proprement. Le journal va dans `JOURNAL` (un
//! chemin de session de l'auteur : à changer si on la relance ailleurs).
//!
//! Phases :
//! A (0-3 s)   fenêtre visible, mesure de référence ;
//! B (3-9 s)   `ViewportCommand::Visible(false)` ; retour par `Visible(true)`
//!             (egui) puis, si cela n'a pas suffi, `ShowWindow(SW_SHOW)` natif ;
//! C (12-18 s) `Minimized(true)` + `ITaskbarList::DeleteTab` ; retour par
//!             `AddTab` + `Minimized(false)` + `Focus` ;
//! D (21-27 s) `DwmSetWindowAttribute(DWMWA_CLOAK, TRUE)` + `DeleteTab` ;
//!             retour par CLOAK FALSE + `AddTab` ;
//! E (28-32 s) l'icône de zone (créée dans `new`, comme le ferait `KiApp::new`)
//!             et ses canaux d'événements, drainés à chaque image ; on lit les
//!             pixels de l'écran à l'endroit de l'icône (est-elle vraiment
//!             affichée ?), on ouvre son menu par `show_menu()` depuis
//!             `update()` (la boucle modale de TrackPopupMenu laisse-t-elle
//!             `update()` tourner ?) et on le referme par WM_CANCELMODE ;
//!             Close à 32 s.
//!
//! Le journal : une ligne « t=<s> phase=<nom> images=<n> » par seconde écrite
//! depuis `update()` (si elle manque, c'est que `update()` ne tourne plus), et
//! les constats du fil, préfixés « fil ».

use std::fs::OpenOptions;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU64, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use eframe::egui;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

const JOURNAL: &str = r"C:\Users\drion\AppData\Local\Temp\claude\I--dev-ki-chat\4dc055f7-57db-48b7-8e0d-5566bc630538\scratchpad\zone_probe.log";

/// Les phases, dans l'ordre ; le fil pose l'index, `update()` le lit.
const PHASES: [&str; 6] = ["A-visible", "B-cachee", "C-minimisee-sans-onglet", "D-cloak", "E-tray", "fin"];

static DEPART: OnceLock<Instant> = OnceLock::new();
static IMAGES: AtomicU64 = AtomicU64::new(0);
static PHASE: AtomicUsize = AtomicUsize::new(0);
static TERMINE: AtomicBool = AtomicBool::new(false);
/// Le fil demande à `update()` d'ouvrir le menu de l'icône (TrackPopupMenu
/// doit tourner sur le fil qui possède la fenêtre-message de l'icône).
static OUVRIR_MENU: AtomicBool = AtomicBool::new(false);
/// La fenêtre-message de l'icône et le rectangle de l'icône à l'écran,
/// notés par `Zone::creer` pour le fil (pixels, WM_CANCELMODE).
static ICONE_HWND: AtomicIsize = AtomicIsize::new(0);
static ICONE_RECT: [AtomicI32; 4] = [AtomicI32::new(0), AtomicI32::new(0), AtomicI32::new(0), AtomicI32::new(0)];

fn secondes() -> f64 {
    DEPART.get_or_init(Instant::now).elapsed().as_secs_f64()
}

fn phase() -> &'static str {
    PHASES[PHASE.load(Ordering::Relaxed).min(PHASES.len() - 1)]
}

/// Une ligne dans le journal et sur la console. Ouvrir/fermer à chaque ligne
/// coûte un peu, mais garantit que rien ne reste dans un tampon si la sonde
/// meurt brutalement.
fn journal(ligne: &str) {
    let texte = format!("t={:6.2} {ligne}", secondes());
    println!("{texte}");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(JOURNAL) {
        let _ = writeln!(f, "{texte}");
    }
}

fn main() -> eframe::Result {
    let _ = std::fs::remove_file(JOURNAL);
    DEPART.get_or_init(Instant::now);
    journal("sonde zone : démarrage");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 200.0])
            .with_position([120.0, 120.0])
            .with_title("ki-chat sonde"),
        ..Default::default()
    };
    let res = eframe::run_native("ki-chat sonde", options, Box::new(|cc| Ok(Box::new(Sonde::new(cc)))));
    journal(&format!("run_native rendu : {:?}", res.as_ref().map(|_| ())));
    res
}

struct Sonde {
    /// HWND vu par `CreationContext` ; comparé à celui de `eframe::Frame`.
    hwnd_creation: isize,
    fil_lance: bool,
    seconde_journalisee: u64,
    images_seconde_prec: u64,
    zone: zone::Zone,
}

impl Sonde {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let hwnd_creation = hwnd_de(cc).unwrap_or(0);
        journal(&format!("new(cc) : HWND via CreationContext = {hwnd_creation:#x}"));
        // Comme le ferait `KiApp::new` : l'icône naît sur le fil de la boucle
        // winit, une fois celle-ci démarrée.
        let zone = zone::Zone::creer();
        Self { hwnd_creation, fil_lance: false, seconde_journalisee: 0, images_seconde_prec: 0, zone }
    }
}

fn hwnd_de(h: &impl HasWindowHandle) -> Option<isize> {
    match h.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(w) => Some(w.hwnd.get()),
        _ => None,
    }
}

impl eframe::App for Sonde {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let n = IMAGES.fetch_add(1, Ordering::Relaxed) + 1;
        if !self.fil_lance {
            self.fil_lance = true;
            let hwnd = hwnd_de(frame).unwrap_or(0);
            journal(&format!(
                "premier update : HWND via Frame = {hwnd:#x} ({})",
                if hwnd == self.hwnd_creation { "identique à CreationContext" } else { "DIFFÉRENT de CreationContext" }
            ));
            let ctx_repeint = ctx.clone();
            std::thread::Builder::new()
                .name("sonde-repeint".into())
                .spawn(move || {
                    while !TERMINE.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(200));
                        ctx_repeint.request_repaint();
                    }
                })
                .expect("fil repeint");
            let ctx_scenario = ctx.clone();
            std::thread::Builder::new()
                .name("sonde-scenario".into())
                .spawn(move || scenario(ctx_scenario, hwnd))
                .expect("fil scénario");
        }

        // Une ligne par seconde entière : si elle manque, `update()` dormait.
        let s = secondes() as u64;
        if s > self.seconde_journalisee {
            let d = n - self.images_seconde_prec;
            journal(&format!("t={s} phase={} images={n} (+{d} depuis la dernière ligne)", phase()));
            self.seconde_journalisee = s;
            self.images_seconde_prec = n;
        }

        for ligne in self.zone.drainer() {
            journal(&format!("update : événement de zone : {ligne}"));
        }
        if OUVRIR_MENU.swap(false, Ordering::Relaxed) {
            let avant = (Instant::now(), IMAGES.load(Ordering::Relaxed));
            journal("update : show_menu() — début (boucle modale de TrackPopupMenu sur ce fil)");
            self.zone.montrer_menu();
            journal(&format!(
                "update : show_menu() — fin après {} ms ; images pendant la boucle modale : {}",
                avant.0.elapsed().as_millis(),
                IMAGES.load(Ordering::Relaxed) - avant.1
            ));
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("ki-chat sonde — se ferme toute seule à 32 s");
            ui.label(format!("phase {} — image n° {n}", phase()));
            ui.label(&self.zone.etat);
        });
    }
}

impl Drop for Sonde {
    fn drop(&mut self) {
        journal("Sonde détruite (l'icône de zone est retirée avec elle)");
    }
}

/// Le scénario chronométré. Il possède le HWND (un entier : `HWND` n'est pas
/// `Send`) et un `Context` clone ; les commandes egui partent par
/// `send_viewport_cmd`, les appels natifs vont droit à la fenêtre.
fn scenario(ctx: egui::Context, hwnd: isize) {
    let com = win::initialiser_com();
    journal(&format!("fil : CoInitializeEx(APARTMENTTHREADED) → {com}"));
    let barre = win::Barre::creer();
    journal(&format!("fil : CoCreateInstance(TaskbarList) + HrInit → {}", barre.etat));
    let etat = |quand: &str| journal(&format!("fil [{quand}] images={} {}", IMAGES.load(Ordering::Relaxed), win::etat(hwnd)));
    let attendre = |jusqua: f64| {
        while secondes() < jusqua {
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let repeindre = |ctx: &egui::Context| ctx.request_repaint();

    etat("A départ");
    attendre(1.0);
    etat("A 1 s");
    attendre(3.0);

    // Phase B : cachée par egui.
    PHASE.store(1, Ordering::Relaxed);
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    repeindre(&ctx);
    journal("fil : B → ViewportCommand::Visible(false) envoyé");
    attendre(4.0);
    etat("B +1 s");
    attendre(6.0);
    etat("B +3 s");
    attendre(9.0);
    etat("B +6 s, avant retour");
    let images_avant = IMAGES.load(Ordering::Relaxed);
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    repeindre(&ctx);
    journal("fil : B → ViewportCommand::Visible(true) envoyé par egui");
    attendre(10.5);
    let images_apres = IMAGES.load(Ordering::Relaxed);
    etat("B retour egui +1,5 s");
    if win::visible(hwnd) {
        journal(&format!("fil : B → Visible(true) via egui A SUFFI (images {images_avant} → {images_apres})"));
    } else {
        journal(&format!(
            "fil : B → Visible(true) via egui N'A PAS SUFFI (images {images_avant} → {images_apres}) ; ShowWindow(SW_SHOW) natif"
        ));
        journal(&format!("fil : ShowWindow(SW_SHOW) → {}", win::montrer(hwnd)));
        repeindre(&ctx);
    }
    attendre(11.5);
    etat("B après retour");

    // Phase C : minimisée, sans bouton dans la barre des tâches.
    attendre(12.0);
    PHASE.store(2, Ordering::Relaxed);
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    repeindre(&ctx);
    journal("fil : C → ViewportCommand::Minimized(true) envoyé");
    attendre(12.3);
    journal(&format!("fil : C → DeleteTab → {}", barre.retirer(hwnd)));
    attendre(13.0);
    etat("C +1 s");
    attendre(15.0);
    etat("C +3 s");
    attendre(18.0);
    etat("C +6 s, avant retour");
    journal(&format!("fil : C → AddTab → {}", barre.remettre(hwnd)));
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    repeindre(&ctx);
    journal("fil : C → Minimized(false) + Focus envoyés");
    attendre(19.0);
    etat("C après retour");

    // Phase D : cachée par le compositeur (DWMWA_CLOAK), sans bouton.
    attendre(21.0);
    PHASE.store(3, Ordering::Relaxed);
    journal(&format!("fil : D → DwmSetWindowAttribute(DWMWA_CLOAK, TRUE) → {}", win::cloak(hwnd, true)));
    journal(&format!("fil : D → DeleteTab → {}", barre.retirer(hwnd)));
    attendre(22.0);
    etat("D +1 s");
    attendre(24.0);
    etat("D +3 s");
    attendre(27.0);
    etat("D +6 s, avant retour");
    journal(&format!("fil : D → DwmSetWindowAttribute(DWMWA_CLOAK, FALSE) → {}", win::cloak(hwnd, false)));
    journal(&format!("fil : D → AddTab → {}", barre.remettre(hwnd)));
    repeindre(&ctx);
    attendre(28.0);
    etat("D après retour");

    // Phase E : l'icône de zone (déjà créée). Aucun clic possible sans
    // humain : on lit l'écran, puis on ouvre et referme le menu nous-mêmes.
    PHASE.store(4, Ordering::Relaxed);
    let rect: [i32; 4] = std::array::from_fn(|i| ICONE_RECT[i].load(Ordering::Relaxed));
    journal(&format!("fil : E → pixels de l'écran dans le rectangle de l'icône : {}", win::pixels_verts(rect)));
    attendre(28.5);
    OUVRIR_MENU.store(true, Ordering::Relaxed);
    repeindre(&ctx);
    journal("fil : E → demande d'ouverture du menu (show_menu dans update)");
    attendre(29.5);
    etat("E menu ouvert +1 s");
    attendre(30.5);
    etat("E menu ouvert +2 s");
    let icone = ICONE_HWND.load(Ordering::Relaxed);
    journal(&format!("fil : E → WM_CANCELMODE à la fenêtre de l'icône → {}", win::fermer_menu(icone)));
    attendre(31.5);
    etat("E menu refermé");
    attendre(32.0);
    PHASE.store(5, Ordering::Relaxed);
    etat("fin");
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    repeindre(&ctx);
    journal("fil : ViewportCommand::Close envoyé");
    TERMINE.store(true, Ordering::Relaxed);
    attendre(36.0);
    journal("fil : la fenêtre ne s'est PAS fermée en 4 s ; sortie forcée");
    std::process::exit(2);
}

/// L'icône de la zone de notification et ses canaux, comme les tiendrait
/// `zone.rs`. Sur Linux la sonde n'a pas d'icône (le crate n'y est pas tiré).
#[cfg(any(windows, target_os = "macos"))]
mod zone {
    use super::journal;
    use std::sync::atomic::Ordering;
    use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
    use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};

    pub struct Zone {
        _icone: Option<TrayIcon>,
        ouvrir: MenuId,
        quitter: MenuId,
        pub etat: String,
    }

    /// Un rond vert sur fond transparent, 16×16, dessiné en mémoire.
    fn image() -> Vec<u8> {
        let mut rgba = Vec::with_capacity(16 * 16 * 4);
        for y in 0..16 {
            for x in 0..16 {
                let (dx, dy) = (x as f32 - 7.5, y as f32 - 7.5);
                let dedans = dx * dx + dy * dy <= 7.0 * 7.0;
                rgba.extend_from_slice(if dedans { &[0, 200, 80, 255] } else { &[0, 0, 0, 0] });
            }
        }
        rgba
    }

    impl Zone {
        pub fn creer() -> Self {
            let ouvrir = MenuItem::new("Ouvrir", true, None);
            let quitter = MenuItem::new("Quitter", true, None);
            let ids = (ouvrir.id().clone(), quitter.id().clone());
            let menu = Menu::new();
            let m1 = menu.append(&ouvrir);
            let m2 = menu.append(&quitter);
            journal(&format!("zone : menu Ouvrir/Quitter → {m1:?} {m2:?}"));
            let icone = match tray_icon::Icon::from_rgba(image(), 16, 16) {
                Ok(i) => i,
                Err(e) => {
                    journal(&format!("zone : Icon::from_rgba a ÉCHOUÉ : {e}"));
                    return Self { _icone: None, ouvrir: ids.0, quitter: ids.1, etat: "icône : échec".into() };
                }
            };
            let res = TrayIconBuilder::new()
                .with_id("ki-chat-sonde")
                .with_tooltip("ki-chat sonde")
                .with_icon(icone)
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .build();
            match res {
                Ok(icone) => {
                    // tray-icon 0.25.1 ne remonte PAS l'échec de
                    // Shell_NotifyIcon(NIM_ADD) (il attend TaskbarCreated) ;
                    // `rect()` = Shell_NotifyIconGetRect, qui ne répond que si
                    // le shell connaît l'icône : c'est notre témoin.
                    let rect = icone.rect();
                    let etat = match rect {
                        Some(r) => format!(
                            "icône créée, connue du shell (Shell_NotifyIconGetRect) : {}×{} à ({}, {})",
                            r.size.width, r.size.height, r.position.x, r.position.y
                        ),
                        None => "icône créée mais INCONNUE du shell (Shell_NotifyIconGetRect a échoué)".into(),
                    };
                    journal(&format!("zone : {etat}"));
                    if let Some(r) = rect {
                        let v = [r.position.x as i32, r.position.y as i32, r.size.width as i32, r.size.height as i32];
                        for (i, x) in v.into_iter().enumerate() {
                            super::ICONE_RECT[i].store(x, Ordering::Relaxed);
                        }
                    }
                    #[cfg(windows)]
                    {
                        let h = icone.window_handle() as isize;
                        super::ICONE_HWND.store(h, Ordering::Relaxed);
                        journal(&format!("zone : fenêtre-message de l'icône HWND={h:#x}"));
                    }
                    Self { _icone: Some(icone), ouvrir: ids.0, quitter: ids.1, etat }
                }
                Err(e) => {
                    let etat = format!("TrayIconBuilder::build a ÉCHOUÉ : {e}");
                    journal(&format!("zone : {etat}"));
                    Self { _icone: None, ouvrir: ids.0, quitter: ids.1, etat }
                }
            }
        }

        /// Ouvre le menu contextuel comme le ferait un clic droit : bloque
        /// tant que le menu est ouvert (TrackPopupMenu).
        pub fn montrer_menu(&self) {
            if let Some(i) = &self._icone {
                i.show_menu();
            }
        }

        /// Ce qui est arrivé sur les deux canaux depuis la dernière image.
        pub fn drainer(&mut self) -> Vec<String> {
            let mut lignes = Vec::new();
            while let Ok(e) = TrayIconEvent::receiver().try_recv() {
                lignes.push(format!("TrayIconEvent {e:?}"));
            }
            while let Ok(e) = MenuEvent::receiver().try_recv() {
                let quoi = if e.id == self.ouvrir {
                    "Ouvrir"
                } else if e.id == self.quitter {
                    "Quitter"
                } else {
                    "inconnu"
                };
                lignes.push(format!("MenuEvent {quoi} ({:?})", e.id));
            }
            lignes
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod zone {
    pub struct Zone {
        pub etat: String,
    }
    impl Zone {
        pub fn creer() -> Self {
            Self { etat: "pas d'icône de zone sur cette plateforme".into() }
        }
        pub fn drainer(&mut self) -> Vec<String> {
            Vec::new()
        }
        pub fn montrer_menu(&self) {}
    }
}

#[cfg(windows)]
mod win {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{GetDC, GetPixel, ReleaseDC};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DwmSetWindowAttribute, DWMWA_CLOAK, DWMWA_CLOAKED};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{ITaskbarList, TaskbarList};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowLongW, GetWindowRect, IsIconic, IsWindowVisible, SendMessageW, ShowWindow,
        GWL_EXSTYLE, SW_SHOW, WM_CANCELMODE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    };

    fn h(hwnd: isize) -> HWND {
        HWND(hwnd as *mut core::ffi::c_void)
    }

    pub fn initialiser_com() -> String {
        format!("{:?}", unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) })
    }

    /// `ITaskbarList`, créé sur le fil du scénario (un objet COM ne voyage
    /// pas entre fils sans marshaling : il vit et meurt ici).
    pub struct Barre {
        liste: Option<ITaskbarList>,
        pub etat: String,
    }

    impl Barre {
        pub fn creer() -> Self {
            let res: windows::core::Result<ITaskbarList> =
                unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER) };
            match res {
                Ok(liste) => {
                    let init = unsafe { liste.HrInit() };
                    Self { liste: Some(liste), etat: format!("ok, HrInit {init:?}") }
                }
                Err(e) => Self { liste: None, etat: format!("ÉCHEC {e}") },
            }
        }
        pub fn retirer(&self, hwnd: isize) -> String {
            match &self.liste {
                Some(l) => format!("{:?}", unsafe { l.DeleteTab(h(hwnd)) }),
                None => "pas de ITaskbarList".into(),
            }
        }
        pub fn remettre(&self, hwnd: isize) -> String {
            match &self.liste {
                Some(l) => format!("{:?}", unsafe { l.AddTab(h(hwnd)) }),
                None => "pas de ITaskbarList".into(),
            }
        }
    }

    pub fn visible(hwnd: isize) -> bool {
        unsafe { IsWindowVisible(h(hwnd)).as_bool() }
    }

    pub fn montrer(hwnd: isize) -> String {
        format!("{:?}", unsafe { ShowWindow(h(hwnd), SW_SHOW) })
    }

    pub fn cloak(hwnd: isize, oui: bool) -> String {
        let val: BOOL = BOOL::from(oui);
        let r = unsafe {
            DwmSetWindowAttribute(h(hwnd), DWMWA_CLOAK, &val as *const BOOL as *const _, std::mem::size_of::<BOOL>() as u32)
        };
        format!("{r:?}")
    }

    /// Combien de pixels « vert sonde » (0,200,80) l'écran montre dans le
    /// rectangle que le shell donne pour l'icône : s'il y en a, l'icône est
    /// bien affichée là (et non repliée derrière le chevron de débordement).
    pub fn pixels_verts(rect: [i32; 4]) -> String {
        let [x, y, w, hh] = rect;
        if w <= 0 || hh <= 0 {
            return "rectangle vide".into();
        }
        unsafe {
            let dc = GetDC(None);
            let mut verts = 0;
            let mut total = 0;
            for yy in y..y + hh {
                for xx in x..x + w {
                    let c = GetPixel(dc, xx, yy).0;
                    let (r, g, b) = (c & 0xff, (c >> 8) & 0xff, (c >> 16) & 0xff);
                    total += 1;
                    if r < 40 && (170..=230).contains(&g) && (50..=110).contains(&b) {
                        verts += 1;
                    }
                }
            }
            ReleaseDC(None, dc);
            format!("{verts} pixels verts sur {total} dans ({x},{y}) {w}×{hh}")
        }
    }

    /// Referme un menu contextuel ouvert par TrackPopupMenu sur une autre
    /// fenêtre : WM_CANCELMODE à son propriétaire.
    pub fn fermer_menu(hwnd: isize) -> String {
        if hwnd == 0 {
            return "pas de fenêtre d'icône".into();
        }
        format!("{:?}", unsafe { SendMessageW(h(hwnd), WM_CANCELMODE, Some(WPARAM(0)), Some(LPARAM(0))) })
    }

    /// Un instantané objectif de la fenêtre : ce qu'un humain verrait, ou
    /// presque (Alt+Tab ne se mesure pas ; on donne les styles dont il dépend).
    pub fn etat(hwnd: isize) -> String {
        unsafe {
            let w = h(hwnd);
            let mut r = RECT::default();
            let _ = GetWindowRect(w, &mut r);
            let mut cloaked: u32 = 0;
            let dwm = DwmGetWindowAttribute(
                w,
                DWMWA_CLOAKED,
                &mut cloaked as *mut u32 as *mut _,
                std::mem::size_of::<u32>() as u32,
            );
            let ex = GetWindowLongW(w, GWL_EXSTYLE) as u32;
            format!(
                "visible={} iconique={} cloaked={} ({dwm:?}) premier-plan={} rect=({},{})-({},{}) ex-style={ex:#x} TOOLWINDOW={} APPWINDOW={}",
                IsWindowVisible(w).as_bool(),
                IsIconic(w).as_bool(),
                cloaked,
                GetForegroundWindow() == w,
                r.left,
                r.top,
                r.right,
                r.bottom,
                ex & WS_EX_TOOLWINDOW.0 != 0,
                ex & WS_EX_APPWINDOW.0 != 0,
            )
        }
    }
}

#[cfg(not(windows))]
mod win {
    pub fn initialiser_com() -> String {
        "sans objet".into()
    }
    pub struct Barre {
        pub etat: String,
    }
    impl Barre {
        pub fn creer() -> Self {
            Self { etat: "sans objet".into() }
        }
        pub fn retirer(&self, _: isize) -> String {
            "sans objet".into()
        }
        pub fn remettre(&self, _: isize) -> String {
            "sans objet".into()
        }
    }
    pub fn visible(_: isize) -> bool {
        true
    }
    pub fn montrer(_: isize) -> String {
        "sans objet".into()
    }
    pub fn cloak(_: isize, _: bool) -> String {
        "sans objet".into()
    }
    pub fn etat(_: isize) -> String {
        "sans objet".into()
    }
    pub fn pixels_verts(_: [i32; 4]) -> String {
        "sans objet".into()
    }
    pub fn fermer_menu(_: isize) -> String {
        "sans objet".into()
    }
}
