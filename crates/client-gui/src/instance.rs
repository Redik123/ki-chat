//! Une seule instance de ki-chat par session Windows.
//!
//! Réduit à côté de l'horloge, ki-chat est invisible ; l'icône du Bureau
//! ou du menu Démarrer lançait alors un **second** ki-chat — deux
//! connexions au serveur, l'ancienne supplantée, et un joueur qui ne
//! comprend pas pourquoi « ça a redémarré ». Le premier lancé tient
//! désormais un verrou nommé (`Local\ki-chat-instance`, un mutex du
//! noyau, propre à la session) ; celui qui trouve le verrou pris sonne
//! l'événement nommé `Local\ki-chat-reveil` — que le garde-fou de la zone
//! de notification écoute — et se retire. L'instance en place revient
//! alors au premier plan, rouverte si elle était réduite.
//!
//! Le verrou s'attend jusqu'à dix secondes avant de conclure : une mise à
//! jour ou une relance automatique lancent le nouveau processus pendant
//! que l'ancien finit de se fermer — il lâche le verrou en mourant, le
//! nouveau l'obtient (« abandonné », dit Windows, ce qui revient au
//! même). Pour ouvrir volontairement un deuxième client — deux comptes
//! sur un même PC, un essai —, `--nouvelle-instance` passe outre.
//!
//! Hors Windows, rien de tout cela : macOS ne lance une application
//! qu'une fois, Linux n'a pas de zone de notification chez nous.

#[cfg(windows)]
use std::time::{Duration, Instant};

/// L'argument qui autorise une instance de plus.
pub const ARG_NOUVELLE: &str = "--nouvelle-instance";

/// Ce que le démarrage a trouvé.
pub enum Demarrage {
    /// Personne d'autre : le verrou est à nous, tant qu'il vit.
    Premiere(Verrou),
    /// Un ki-chat tourne déjà dans cette session.
    DejaLancee,
}

/// Le verrou d'instance — le mutex, tenu par le fil principal jusqu'à ce
/// qu'on le lâche (avant de relancer un successeur) ou que le processus
/// meure.
pub struct Verrou {
    #[cfg(windows)]
    mutex: isize,
}

/// `--nouvelle-instance` figure-t-il dans les arguments ?
pub fn nouvelle_demandee() -> bool {
    nouvelle_dans(std::env::args())
}

fn nouvelle_dans(mut args: impl Iterator<Item = String>) -> bool {
    args.any(|a| a == ARG_NOUVELLE)
}

#[cfg(windows)]
mod natif {
    use super::*;
    use windows::core::w;
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0,
    };
    use windows::Win32::System::Threading::{
        CreateEventW, CreateMutexW, OpenEventW, ReleaseMutex, SetEvent, WaitForSingleObject,
        EVENT_MODIFY_STATE,
    };

    /// Combien de temps attendre qu'un prédécesseur lâche le verrou : une
    /// fermeture propre prend une seconde, une mise à jour qui écrit ses
    /// réglages et rend l'audio un peu plus — jamais dix.
    const ATTENTE_VERROU: Duration = Duration::from_secs(10);
    /// Combien de temps chercher l'événement de l'instance en place : elle
    /// peut être en train de démarrer, son garde-fou pas encore né.
    const ATTENTE_REVEIL: Duration = Duration::from_secs(3);

    fn h(brut: isize) -> HANDLE {
        HANDLE(brut as *mut core::ffi::c_void)
    }

    pub fn prendre() -> Demarrage {
        if nouvelle_demandee() {
            tracing::info!("instance : {} — pas de verrou", ARG_NOUVELLE);
            return Demarrage::Premiere(Verrou { mutex: 0 });
        }
        // SAFETY : appels Win32 sans pointeur de notre côté ; le nom est
        // une chaîne large constante. Un échec rend un handle nul : on
        // démarre sans verrou plutôt que de ne pas démarrer.
        let mutex = unsafe { CreateMutexW(None, false, w!("Local\\ki-chat-instance")) };
        let Ok(mutex) = mutex else {
            tracing::warn!("instance : verrou impossible à créer — on démarre sans");
            return Demarrage::Premiere(Verrou { mutex: 0 });
        };
        let existait = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let attente = if existait { ATTENTE_VERROU } else { Duration::ZERO };
        let resultat = unsafe { WaitForSingleObject(mutex, attente.as_millis() as u32) };
        if resultat == WAIT_OBJECT_0 || resultat == WAIT_ABANDONED {
            if existait {
                tracing::info!("instance : le prédécesseur a lâché le verrou, on prend sa place");
            }
            return Demarrage::Premiere(Verrou { mutex: mutex.0 as isize });
        }
        unsafe {
            let _ = CloseHandle(mutex);
        }
        Demarrage::DejaLancee
    }

    /// Sonne l'instance en place. Vrai si elle a été trouvée.
    pub fn reveiller_l_autre() -> bool {
        let depart = Instant::now();
        loop {
            // SAFETY : voir `prendre`.
            if let Ok(ev) = unsafe { OpenEventW(EVENT_MODIFY_STATE, false, w!("Local\\ki-chat-reveil")) } {
                let sonne = unsafe { SetEvent(ev) }.is_ok();
                unsafe {
                    let _ = CloseHandle(ev);
                }
                if sonne {
                    return true;
                }
            }
            if depart.elapsed() >= ATTENTE_REVEIL {
                return false;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    pub fn lacher(mutex: isize) {
        if mutex == 0 {
            return;
        }
        // SAFETY : le handle vient de `CreateMutexW` et n'est libéré
        // qu'ici, une fois.
        unsafe {
            let _ = ReleaseMutex(h(mutex));
            let _ = CloseHandle(h(mutex));
        }
    }

    /// L'événement que les suivants sonnent — auto-réarmé : une sonnerie,
    /// une lecture.
    pub fn creer_reveil() -> Option<isize> {
        // SAFETY : voir `prendre`.
        let ev = unsafe { CreateEventW(None, false, false, w!("Local\\ki-chat-reveil")) }.ok()?;
        Some(ev.0 as isize)
    }

    pub fn sonne(ev: isize) -> bool {
        ev != 0 && unsafe { WaitForSingleObject(h(ev), 0) } == WAIT_OBJECT_0
    }

    pub fn fermer(ev: isize) {
        if ev != 0 {
            // SAFETY : handle de `CreateEventW`, fermé une fois.
            unsafe {
                let _ = CloseHandle(h(ev));
            }
        }
    }
}

/// Au démarrage : le verrou, ou le constat qu'un autre le tient.
pub fn prendre() -> Demarrage {
    #[cfg(windows)]
    {
        natif::prendre()
    }
    #[cfg(not(windows))]
    {
        Demarrage::Premiere(Verrou {})
    }
}

/// Un autre ki-chat tourne : on le ramène au premier plan. Vrai s'il a
/// été trouvé ; sinon on le dit, et l'appelant se retire quand même —
/// mieux vaut un clic pour rien qu'un doublon.
pub fn reveiller_l_autre() -> bool {
    #[cfg(windows)]
    {
        natif::reveiller_l_autre()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

impl Verrou {
    /// Lâche le verrou avant l'heure — pour laisser la place au successeur
    /// qu'on relance (mise à jour, relance automatique).
    pub fn lacher(self) {
        #[cfg(windows)]
        natif::lacher(self.mutex);
    }
}

/// L'oreille de l'instance en place : le garde-fou de la zone de
/// notification l'interroge à chaque tour.
pub struct Reveil {
    #[cfg(windows)]
    ev: isize,
}

impl Reveil {
    /// Sans Windows, une oreille sourde — rien ne sonnera jamais.
    pub fn creer() -> Self {
        #[cfg(windows)]
        {
            let ev = natif::creer_reveil().unwrap_or(0);
            if ev == 0 {
                tracing::warn!("instance : événement de réveil impossible à créer — un second lancement ouvrira un doublon");
            }
            Self { ev }
        }
        #[cfg(not(windows))]
        {
            Self {}
        }
    }

    /// Quelqu'un a sonné depuis la dernière fois ?
    pub fn sonne(&self) -> bool {
        #[cfg(windows)]
        {
            natif::sonne(self.ev)
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}

impl Drop for Reveil {
    fn drop(&mut self) {
        #[cfg(windows)]
        natif::fermer(self.ev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// L'argument se reconnaît tel quel, et seulement lui.
    #[test]
    fn l_argument_d_une_instance_de_plus_se_lit() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>().into_iter();
        assert!(nouvelle_dans(args(&["ki-chat.exe", ARG_NOUVELLE])));
        assert!(!nouvelle_dans(args(&["ki-chat.exe", "--reduit"])));
        assert!(!nouvelle_dans(args(&["ki-chat.exe"])));
    }

    /// Sous Windows : le premier prend le verrou, le second le trouve pris
    /// et sonne un réveil que le premier entend — une fois.
    #[cfg(windows)]
    #[test]
    fn le_second_lancement_sonne_le_premier() {
        // Un autre test, ou un vrai ki-chat, peut tenir le verrou :
        // on ne conclut alors rien sur `prendre`, seulement sur le réveil.
        let reveil = Reveil::creer();
        assert!(!reveil.sonne(), "rien n'a sonné encore");
        assert!(reveiller_l_autre(), "l'événement existe, il se sonne");
        assert!(reveil.sonne(), "la sonnerie se relève");
        assert!(!reveil.sonne(), "et une seule fois");
    }
}
