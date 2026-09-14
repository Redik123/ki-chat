//! Le raccourci global de l'enregistreur de clips, enregistré auprès de
//! Windows (`RegisterHotKey`).
//!
//! Le push-to-talk lit le clavier par sondage (`GetAsyncKeyState`), et cela
//! suffit tant que la fenêtre au premier plan est une fenêtre ordinaire.
//! Au-dessus de VALORANT, le sondage ne voyait plus rien : Windows retient
//! l'état des touches à une application quand celle qui a le premier plan
//! est plus privilégiée qu'elle — un jeu sous anti-triche l'est —, comme il
//! ne lui livre pas ses crochets clavier. C'est la même barrière qui oblige
//! Discord ou OBS à « tourner en administrateur » pour que leurs touches
//! marchent en jeu.
//!
//! Un raccourci enregistré par `RegisterHotKey` est d'une autre nature :
//! c'est le système qui le reconnaît, avant que la touche n'atteigne quelque
//! fenêtre que ce soit, et il prévient l'application par un message. C'est
//! le mécanisme d'Alt+Tab ; il passe au-dessus de tout, plein écran exclusif
//! compris, sans élévation, et le jeu ne voit pas la touche.
//!
//! Le message arrive dans la file du fil qui a enregistré le raccourci :
//! d'où un fil à part, qui ne fait que ça, et qui déclenche le clip lui-même
//! sans passer par l'interface — réduite derrière le jeu, elle peut dormir.
//! Si Windows refuse la combinaison (un autre programme la tient déjà), le
//! sondage du push-to-talk reprend le relais, et les réglages le disent.

use std::sync::atomic::AtomicU8;

use crate::ptt::{Action, Raccourci};

/// Ce que Windows a dit de la combinaison demandée.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Etat {
    /// Rien de demandé — ou pas de Windows.
    Aucun = 0,
    /// Enregistrée : elle passe au-dessus de tout.
    Enregistre = 1,
    /// Refusée — un autre programme la tient déjà. Le sondage prend le
    /// relais, mais lui ne voit pas au-dessus d'un jeu.
    Refuse = 2,
}

impl Etat {
    fn depuis(v: u8) -> Etat {
        match v {
            1 => Etat::Enregistre,
            2 => Etat::Refuse,
            _ => Etat::Aucun,
        }
    }
}

/// L'état partagé avec le fil de sondage, qui ne lit la combinaison que
/// si Windows ne s'en charge pas.
pub fn lire(etat: &AtomicU8) -> Etat {
    Etat::depuis(etat.load(std::sync::atomic::Ordering::Relaxed))
}

#[cfg(windows)]
pub use windows_impl::Global;

#[cfg(windows)]
mod windows_impl {
    use std::sync::atomic::Ordering;
    use std::sync::{mpsc, Arc, Mutex};

    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
        MOD_SHIFT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetMessageW, PeekMessageW, PostThreadMessageW, MSG, PM_NOREMOVE, WM_APP, WM_HOTKEY,
        WM_QUIT, WM_USER,
    };

    use super::*;

    /// « Relis la combinaison voulue et (dés)enregistre-la. »
    const WM_REGLER: u32 = WM_APP + 1;

    /// Le fil qui tient le raccourci auprès de Windows.
    pub struct Global {
        fil: Option<std::thread::JoinHandle<()>>,
        id_fil: u32,
        voulu: Arc<Mutex<Option<Raccourci>>>,
    }

    impl Global {
        /// Lance le fil. `action` est appelée sur ce fil à chaque appui ;
        /// `etat` reçoit ce que Windows a dit de la combinaison (voir
        /// [`lire`]).
        pub fn demarrer(action: Action, etat: Arc<AtomicU8>) -> Option<Self> {
            let voulu = Arc::new(Mutex::new(None));
            let (pret_tx, pret_rx) = mpsc::channel();
            let fil = std::thread::Builder::new()
                .name("ki-raccourci".into())
                .spawn({
                    let voulu = voulu.clone();
                    move || boucle(voulu, etat, action, pret_tx)
                })
                .ok()?;
            let id_fil = pret_rx.recv().ok()?;
            Some(Self {
                fil: Some(fil),
                id_fil,
                voulu,
            })
        }

        /// La combinaison à tenir, `None` pour la lâcher. Le fil s'en
        /// occupe à son rythme ; l'état partagé dit où il en est.
        pub fn regler(&self, r: Option<Raccourci>) {
            *self.voulu.lock().unwrap() = r;
            // SAFETY : un message sans pointeur, vers un fil qui est à nous.
            let _ = unsafe { PostThreadMessageW(self.id_fil, WM_REGLER, WPARAM(0), LPARAM(0)) };
        }
    }

    impl Drop for Global {
        fn drop(&mut self) {
            // SAFETY : idem — WM_QUIT fait rendre GetMessage, la boucle sort.
            let _ = unsafe { PostThreadMessageW(self.id_fil, WM_QUIT, WPARAM(0), LPARAM(0)) };
            if let Some(f) = self.fil.take() {
                let _ = f.join();
            }
        }
    }

    fn boucle(
        voulu: Arc<Mutex<Option<Raccourci>>>,
        etat: Arc<AtomicU8>,
        action: Action,
        pret: mpsc::Sender<u32>,
    ) {
        let mut msg = MSG::default();
        // La file de messages d'un fil n'existe qu'à son premier appel au
        // sous-système fenêtres : ce PeekMessage la crée, avant que
        // quiconque ne puisse nous poster quoi que ce soit.
        // SAFETY : `msg` est à nous et vit toute la boucle.
        unsafe {
            let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
        }
        // SAFETY : sans argument, sans état.
        if pret.send(unsafe { GetCurrentThreadId() }).is_err() {
            return;
        }
        let mut poses: Vec<i32> = Vec::new();
        loop {
            // SAFETY : idem ; rend 0 sur WM_QUIT, -1 sur erreur.
            let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
            if r.0 <= 0 {
                break;
            }
            match msg.message {
                WM_HOTKEY => action(),
                WM_REGLER => {
                    liberer(&mut poses);
                    let r = *voulu.lock().unwrap();
                    let e = match r {
                        None => Etat::Aucun,
                        Some(r) if poser(&r, &mut poses) => Etat::Enregistre,
                        Some(r) => {
                            ki_voice::journal(format!(
                                "clips : Windows refuse le raccourci {} (déjà pris par un autre \
                                 programme) — lecture par sondage, qui ne voit pas au-dessus d'un jeu",
                                r.label()
                            ));
                            Etat::Refuse
                        }
                    };
                    etat.store(e as u8, Ordering::Relaxed);
                }
                _ => {}
            }
        }
        liberer(&mut poses);
    }

    /// Enregistre la combinaison et ses variantes. Vrai si la combinaison
    /// exacte est passée — les variantes sont un confort, et si l'exacte est
    /// refusée on lâche tout : le sondage prend le relais, et lui seul.
    fn poser(r: &Raccourci, poses: &mut Vec<i32>) -> bool {
        let mut exacte = false;
        for (i, v) in r.variantes().iter().enumerate() {
            let id = i as i32 + 1;
            let bit = |tenu: bool, m: HOT_KEY_MODIFIERS| if tenu { m.0 } else { 0 };
            let mods = HOT_KEY_MODIFIERS(
                bit(v.ctrl, MOD_CONTROL)
                    | bit(v.alt, MOD_ALT)
                    | bit(v.shift, MOD_SHIFT)
                    | MOD_NOREPEAT.0,
            );
            // SAFETY : sans fenêtre, le message ira à la file de ce fil.
            let ok = unsafe { RegisterHotKey(None, id, mods, v.touche.vk()) }.is_ok();
            if ok {
                poses.push(id);
            }
            if i == 0 {
                exacte = ok;
            }
        }
        if !exacte {
            liberer(poses);
        }
        exacte
    }

    fn liberer(poses: &mut Vec<i32>) {
        for id in poses.drain(..) {
            // SAFETY : un identifiant que nous avons posé sur ce fil.
            let _ = unsafe { UnregisterHotKey(None, id) };
        }
    }

    #[cfg(test)]
    mod tests {
        use std::time::Duration;

        use super::*;
        use crate::ptt::Touche;

        fn attendre(etat: &AtomicU8, autre_que: Etat) -> Etat {
            let mut vu = autre_que;
            for _ in 0..200 {
                vu = lire(etat);
                if vu != autre_que {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            vu
        }

        #[test]
        fn le_fil_du_raccourci_pose_une_combinaison_puis_la_lache() {
            let etat = Arc::new(AtomicU8::new(0));
            let g = Global::demarrer(Arc::new(|| {}), etat.clone()).expect("le fil démarre");
            assert_eq!(lire(&etat), Etat::Aucun);
            g.regler(Some(Raccourci {
                ctrl: true,
                alt: true,
                shift: true,
                touche: Touche::F(11),
            }));
            // Enregistrée, ou refusée si un autre programme la tient : dans
            // les deux cas, le fil a répondu.
            let vu = attendre(&etat, Etat::Aucun);
            assert_ne!(vu, Etat::Aucun, "le fil n'a pas répondu");
            g.regler(None);
            assert_eq!(attendre(&etat, vu), Etat::Aucun);
            drop(g);
        }
    }
}

/// Hors Windows, pas de raccourci global : le sondage fait tout.
#[cfg(not(windows))]
pub struct Global;

#[cfg(not(windows))]
impl Global {
    pub fn demarrer(_action: Action, _etat: std::sync::Arc<AtomicU8>) -> Option<Self> {
        None
    }

    pub fn regler(&self, _r: Option<Raccourci>) {}

    pub fn etat(&self) -> Etat {
        Etat::Aucun
    }
}
