//! Sonde de transparence de l'overlay : crée la même fenêtre que l'overlay
//! et essaie, à la suite, les techniques de transparence possibles sur cette
//! machine, en lisant à chaque fois les pixels de l'écran et en testant les
//! clics à travers. `cargo run --release -p ki-client-gui --example overlay_probe`
//!
//! A. clé de couleur (LWA_COLORKEY) sur la fenêtre en couches ;
//! B. forme de fenêtre découpée (SetWindowRgn) ;
//! C. compositeur DWM (DwmExtendFrameIntoClientArea) sans le style en couches,
//!    avec un canal alpha et un fond à alpha 0.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eframe::egui;

const TITRE: &str = "sonde overlay";
static FOND_ALPHA_ZERO: AtomicBool = AtomicBool::new(false);

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([420.0, 260.0])
            .with_position([80.0, 80.0])
            .with_transparent(true)
            .with_title("sonde racine"),
        ..Default::default()
    };
    eframe::run_native(
        "sonde",
        options,
        Box::new(|_| Ok(Box::new(Sonde { depart: Instant::now(), etape: 0 }))),
    )
}

struct Sonde {
    depart: Instant,
    etape: usize,
}

impl eframe::App for Sonde {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if FOND_ALPHA_ZERO.load(Ordering::Relaxed) {
            [0.0, 0.0, 0.0, 0.0]
        } else {
            [1.0 / 255.0, 0.0, 1.0 / 255.0, 1.0]
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label("sonde de transparence — se ferme toute seule");
        });
        let builder = egui::ViewportBuilder::default()
            .with_title(TITRE)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_taskbar(false)
            .with_resizable(false)
            .with_mouse_passthrough(true)
            .with_active(false)
            .with_inner_size([120.0, 120.0])
            .with_position([600.0, 400.0]);
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("sonde-overlay"),
            builder,
            |ctx, _| {
                egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
                    ui.painter().circle_filled(
                        egui::pos2(60.0, 60.0),
                        30.0,
                        egui::Color32::from_rgb(0, 200, 80),
                    );
                });
            },
        );
        let t = self.depart.elapsed().as_secs_f32();
        let etapes: [(f32, fn()); 7] = [
            (1.0, || {
                win::etat("départ");
                win::poser_cle();
            }),
            (2.0, || win::lire("A. clé de couleur")),
            (2.5, win::poser_region),
            (3.5, || win::lire("B. forme découpée")),
            (4.0, || {
                win::retirer_region();
                win::mode_dwm();
                FOND_ALPHA_ZERO.store(true, Ordering::Relaxed);
            }),
            (5.0, || win::lire("C. compositeur DWM, fond alpha 0")),
            (5.5, || std::process::exit(0)),
        ];
        if let Some((quand, f)) = etapes.get(self.etape) {
            if t > *quand {
                self.etape += 1;
                f();
            }
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

#[cfg(windows)]
mod win {
    use super::TITRE;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT};
    use windows::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea;
    use windows::Win32::Graphics::Gdi::{CreateEllipticRgn, GetDC, GetPixel, ReleaseDC, SetWindowRgn, HRGN};
    use windows::Win32::UI::Controls::MARGINS;
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, GetLayeredWindowAttributes, GetWindowLongW, GetWindowRect,
        SetLayeredWindowAttributes, SetWindowLongW, SetWindowPos, WindowFromPoint,
        GWL_EXSTYLE, LAYERED_WINDOW_ATTRIBUTES_FLAGS, LWA_COLORKEY, SWP_FRAMECHANGED,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WS_EX_LAYERED, WS_EX_TRANSPARENT,
    };

    fn hwnd() -> Option<HWND> {
        let large: Vec<u16> = TITRE.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe { FindWindowW(None, PCWSTR(large.as_ptr())).ok().filter(|h| !h.is_invalid()) }
    }

    pub fn etat(quand: &str) {
        let Some(h) = hwnd() else {
            println!("[{quand}] fenêtre « {TITRE} » INTROUVABLE");
            return;
        };
        unsafe {
            let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
            let mut cle = COLORREF(0);
            let mut alpha = 0u8;
            let mut flags = LAYERED_WINDOW_ATTRIBUTES_FLAGS(0);
            let attrs =
                GetLayeredWindowAttributes(h, Some(&mut cle), Some(&mut alpha), Some(&mut flags));
            println!(
                "[{quand}] fenêtre trouvée ; ex-style 0x{ex:08x} (LAYERED {}, TRANSPARENT {}) ; \
                 attributs en couches : {:?} clé 0x{:06x} alpha {alpha} flags {}",
                ex & WS_EX_LAYERED.0 != 0,
                ex & WS_EX_TRANSPARENT.0 != 0,
                attrs.is_ok(),
                cle.0,
                flags.0
            );
        }
    }

    pub fn poser_cle() {
        let Some(h) = hwnd() else { return };
        unsafe {
            let r = SetLayeredWindowAttributes(h, COLORREF(0x0001_0001), 255, LWA_COLORKEY);
            println!("SetLayeredWindowAttributes(LWA_COLORKEY) : {r:?}");
        }
    }

    pub fn poser_region() {
        let Some(h) = hwnd() else { return };
        unsafe {
            let mut r = RECT::default();
            let _ = GetWindowRect(h, &mut r);
            let (w, hh) = (r.right - r.left, r.bottom - r.top);
            // Le rond de la sonde : centre au milieu, rayon un quart de la
            // largeur — la même proportion qu'en points.
            let cx = w / 2;
            let cy = hh / 2;
            let ray = w / 4;
            let rgn: HRGN = CreateEllipticRgn(cx - ray, cy - ray, cx + ray, cy + ray);
            let res = SetWindowRgn(h, Some(rgn), true);
            println!("SetWindowRgn(ellipse) : {res}");
        }
    }

    pub fn retirer_region() {
        let Some(h) = hwnd() else { return };
        unsafe {
            let res = SetWindowRgn(h, None, true);
            println!("SetWindowRgn(aucune) : {res}");
        }
    }

    pub fn mode_dwm() {
        let Some(h) = hwnd() else { return };
        unsafe {
            let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
            let sans = ex & !WS_EX_LAYERED.0;
            SetWindowLongW(h, GWL_EXSTYLE, sans as i32);
            let _ = SetWindowPos(
                h,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
            let marges =
                MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
            let r = DwmExtendFrameIntoClientArea(h, &marges);
            let ex2 = GetWindowLongW(h, GWL_EXSTYLE) as u32;
            println!(
                "mode DWM : LAYERED retiré (ex-style 0x{ex2:08x}, TRANSPARENT {}), \
                 DwmExtendFrameIntoClientArea : {r:?}",
                ex2 & WS_EX_TRANSPARENT.0 != 0
            );
        }
    }

    pub fn lire(quoi: &str) {
        let Some(h) = hwnd() else { return };
        unsafe {
            let mut r = RECT::default();
            let _ = GetWindowRect(h, &mut r);
            let dc = GetDC(None);
            let lire = |x: i32, y: i32| {
                let c = GetPixel(dc, x, y).0;
                (c & 0xff, (c >> 8) & 0xff, (c >> 16) & 0xff)
            };
            let (cx, cy) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
            let coin = lire(r.left + 6, r.top + 6);
            let centre = lire(cx, cy);
            ReleaseDC(None, dc);
            let sous_coin = WindowFromPoint(POINT { x: r.left + 6, y: r.top + 6 });
            let sous_centre = WindowFromPoint(POINT { x: cx, y: cy });
            let qui = |w: HWND| {
                if w == h {
                    "NOTRE fenêtre (pas à travers)"
                } else {
                    "une autre fenêtre (à travers)"
                }
            };
            println!(
                "[{quoi}] coin rgb{coin:?} ({}) ; centre rgb{centre:?} ; clic au coin → {} ; \
                 clic au centre → {}",
                verdict(coin),
                qui(sous_coin),
                qui(sous_centre),
            );
        }
    }

    fn verdict(c: (u32, u32, u32)) -> &'static str {
        if c == (1, 0, 1) {
            "couleur-clé opaque"
        } else if c == (0, 0, 0) {
            "NOIR"
        } else {
            "le bureau se voit → TRANSPARENT"
        }
    }
}

#[cfg(not(windows))]
mod win {
    pub fn etat(_: &str) {}
    pub fn poser_cle() {}
    pub fn poser_region() {}
    pub fn retirer_region() {}
    pub fn mode_dwm() {}
    pub fn lire(_: &str) {}
}
