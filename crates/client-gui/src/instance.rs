//! Une seule instance de ki-chat par session Windows.
//!
//! Réduit à côté de l'horloge, ki-chat est invisible ; l'icône du Bureau
//! ou du menu Démarrer lançait alors un **second** ki-chat — deux
//! connexions au serveur, l'ancienne supplantée, et un joueur qui ne
//! comprend pas pourquoi « ça a redémarré ». Le premier lancé tient
//! désormais un verrou nommé (`Local\ki-chat-instance`, un mutex du
//! noyau, propre à la session) et affiche son numéro de processus
//! (`Local\ki-chat-instance-pid`). Celui qui trouve le verrou pris sonne
//! l'événement nommé `Local\ki-chat-reveil` — que le garde-fou de la zone
//! de notification écoute — et attend que l'**interface** de l'instance en
//! place accuse le réveil (`Local\ki-chat-reveil-ok`) : la fenêtre est
//! revenue, rouverte si elle était réduite, il se retire.
//!
//! Sans accusé, il ne se retire plus en silence. La 0.1.44 le faisait : un
//! ki-chat resté sans fenêtre tenait le verrou, et chaque lancement
//! attendait dix secondes puis s'en allait sans un mot — « ki-chat ne se
//! lance plus ». Désormais, quand en dix secondes ni l'accusé ni le verrou
//! ne viennent, une boîte de dialogue le dit, et propose de fermer l'ancien
//! pour démarrer.
//!
//! Le verrou se guette pendant l'attente : une mise à jour ou une relance
//! automatique lancent le nouveau processus pendant que l'ancien finit de
//! se fermer — il lâche le verrou en mourant, le nouveau l'obtient
//! (« abandonné », dit Windows, ce qui revient au même). Pour ouvrir
//! volontairement un deuxième client — deux comptes sur un même PC, un
//! essai —, `--nouvelle-instance` passe outre.
//!
//! Hors Windows, rien de tout cela : macOS ne lance une application
//! qu'une fois, Linux n'a pas de zone de notification chez nous. Le module
//! n'y est qu'une façade, d'où l'autorisation de code mort.

#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
use std::time::{Duration, Instant};

/// L'argument qui autorise une instance de plus.
pub const ARG_NOUVELLE: &str = "--nouvelle-instance";

/// Ce que le démarrage a trouvé.
pub enum Demarrage {
    /// Personne d'autre : le verrou est à nous, tant qu'il vit.
    Premiere(Verrou),
    /// Un ki-chat tourne déjà dans cette session, et il est revenu au
    /// premier plan : celui-ci se retire.
    DejaLancee,
    /// Un ki-chat tient le verrou mais ne répond pas — ni accusé, ni
    /// verrou rendu. À [`Bloquee::resoudre`] de demander quoi faire.
    Bloquee(Bloquee),
}

/// Le verrou d'instance — le mutex, tenu par le fil principal jusqu'à ce
/// qu'on le lâche (avant de relancer un successeur) ou que le processus
/// meure, et l'affiche de notre numéro de processus.
pub struct Verrou {
    #[cfg(windows)]
    mutex: isize,
    #[cfg(windows)]
    affiche: isize,
}

/// Une instance en place qui ne répond pas.
pub struct Bloquee {
    #[cfg(windows)]
    mutex: isize,
    /// Son numéro de processus, si elle l'affiche (depuis 0.1.45).
    pid: Option<u32>,
    /// Le réveil a pu être sonné : son garde-fou existe.
    sonnee: bool,
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
    use windows::core::{HSTRING, PWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, INVALID_HANDLE_VALUE, WAIT_ABANDONED,
        WAIT_FAILED, WAIT_OBJECT_0,
    };
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Memory::{
        CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_READ, FILE_MAP_WRITE,
        PAGE_READWRITE,
    };
    use windows::Win32::System::Threading::{
        CreateEventW, CreateMutexW, OpenEventW, OpenProcess, QueryFullProcessImageNameW, ReleaseMutex, SetEvent,
        TerminateProcess, WaitForMultipleObjects, WaitForSingleObject, EVENT_MODIFY_STATE, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDYES, MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO,
        MESSAGEBOX_RESULT, MESSAGEBOX_STYLE,
    };

    /// Combien de temps attendre, au plus, que l'instance en place accuse
    /// le réveil ou rende le verrou : une fermeture propre prend une
    /// seconde, une mise à jour qui écrit ses réglages et rend l'audio un
    /// peu plus, un démarrage deux ou trois — jamais dix.
    pub const ATTENTE: Duration = Duration::from_secs(10);
    /// Le pas de l'attente : entre deux pas, on sonne encore si le réveil
    /// n'existait pas (l'instance en place démarre, son garde-fou pas
    /// encore né).
    const PAS: Duration = Duration::from_millis(250);
    /// Le temps laissé à l'ancien pour mourir quand on l'a fermé de force,
    /// et au verrou pour nous revenir.
    const APRES_FORCE: Duration = Duration::from_secs(5);

    /// Les noms des objets partagés. Ceux de la session sont ceux de la
    /// 0.1.44 — une 0.1.44 et une 0.1.45 se reconnaissent ; les tests
    /// prennent les leurs, pour ne jamais sonner le vrai ki-chat de la
    /// machine.
    pub struct Noms {
        verrou: HSTRING,
        reveil: HSTRING,
        accuse: HSTRING,
        affiche: HSTRING,
    }

    impl Noms {
        pub fn session() -> Self {
            Self::de("ki-chat")
        }

        pub fn de(racine: &str) -> Self {
            Self {
                verrou: HSTRING::from(format!("Local\\{racine}-instance")),
                reveil: HSTRING::from(format!("Local\\{racine}-reveil")),
                accuse: HSTRING::from(format!("Local\\{racine}-reveil-ok")),
                affiche: HSTRING::from(format!("Local\\{racine}-instance-pid")),
            }
        }
    }

    fn h(brut: isize) -> HANDLE {
        HANDLE(brut as *mut core::ffi::c_void)
    }

    fn brut(handle: HANDLE) -> isize {
        handle.0 as isize
    }

    pub fn fermer(brut: isize) {
        if brut != 0 {
            // SAFETY : un handle à nous, fermé une fois.
            unsafe {
                let _ = CloseHandle(h(brut));
            }
        }
    }

    pub fn prendre(noms: &Noms, attente: Duration) -> Demarrage {
        // SAFETY (tout ce module) : appels Win32 sans pointeur de notre
        // côté hors des vues projetées, lues et écrites sur quatre octets ;
        // les noms sont des chaînes larges terminées par un zéro
        // (`HSTRING`). Un échec rend une erreur : on démarre sans verrou
        // plutôt que de ne pas démarrer.
        let mutex = match unsafe { CreateMutexW(None, false, &noms.verrou) } {
            Ok(mutex) => mutex,
            Err(e) => {
                tracing::warn!("instance : verrou impossible à créer ({e}) — on démarre sans");
                return Demarrage::Premiere(Verrou { mutex: 0, affiche: 0 });
            }
        };
        let existait = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        // Libre — neuf, rendu, ou abandonné par un mort : il est à nous.
        let libre = unsafe { WaitForSingleObject(mutex, 0) };
        if libre == WAIT_OBJECT_0 || libre == WAIT_ABANDONED {
            if libre == WAIT_ABANDONED {
                tracing::info!("instance : le prédécesseur est mort sans rendre le verrou — on prend sa place");
            } else if existait {
                tracing::info!("instance : le prédécesseur a rendu le verrou — on prend sa place");
            }
            return Demarrage::Premiere(tenir(noms, mutex));
        }

        // Tenu : un ki-chat vit. On le sonne tout de suite, puis l'on guette
        // à la fois son accusé et le verrou — un prédécesseur qui se ferme
        // le rendra. L'accusé est à réarmement manuel : deux lancements
        // coup sur coup (un double-clic de trop) se réveillent sur le même.
        enum Issue {
            Verrou,
            Accuse,
            Silence,
        }
        let accuse = unsafe { CreateEventW(None, true, false, &noms.accuse) }.ok();
        let depart = Instant::now();
        let mut sonnee = sonner(noms);
        let mut objets = vec![mutex];
        objets.extend(accuse);
        let issue = loop {
            let r = unsafe { WaitForMultipleObjects(&objets, false, PAS.as_millis() as u32) };
            if r == WAIT_OBJECT_0 || r == WAIT_ABANDONED {
                break Issue::Verrou;
            }
            if r.0 == WAIT_OBJECT_0.0 + 1 {
                break Issue::Accuse;
            }
            if depart.elapsed() >= attente {
                break Issue::Silence;
            }
            if r == WAIT_FAILED {
                std::thread::sleep(PAS);
            }
            if !sonnee {
                sonnee = sonner(noms);
            }
        };
        if let Some(accuse) = accuse {
            unsafe {
                let _ = CloseHandle(accuse);
            }
        }
        let ms = depart.elapsed().as_millis();
        match issue {
            Issue::Verrou => {
                tracing::info!("instance : le prédécesseur a rendu le verrou après {ms} ms — on prend sa place");
                Demarrage::Premiere(tenir(noms, mutex))
            }
            Issue::Accuse => {
                fermer(brut(mutex));
                tracing::info!("instance : ki-chat tourne déjà — revenu au premier plan en {ms} ms, celui-ci se retire");
                Demarrage::DejaLancee
            }
            Issue::Silence => {
                let pid = pid_affiche(noms);
                tracing::warn!(
                    "instance : ki-chat tourne déjà (processus {}) mais ne répond pas depuis {} s — {}",
                    pid.map_or_else(|| "inconnu".to_string(), |p| p.to_string()),
                    attente.as_secs(),
                    if sonnee { "réveil sonné, pas d'accusé" } else { "pas de réveil à sonner" }
                );
                Demarrage::Bloquee(Bloquee { mutex: brut(mutex), pid, sonnee })
            }
        }
    }

    /// Le verrou est à nous : on affiche notre numéro de processus, pour
    /// qu'un suivant puisse, si nous ne répondons plus, nous nommer — et
    /// nous fermer si l'utilisateur le veut.
    fn tenir(noms: &Noms, mutex: HANDLE) -> Verrou {
        let affiche = afficher_pid(noms).unwrap_or(0);
        tracing::info!("instance : verrou pris (processus {})", std::process::id());
        Verrou { mutex: brut(mutex), affiche }
    }

    fn afficher_pid(noms: &Noms) -> Option<isize> {
        unsafe {
            let projection =
                CreateFileMappingW(INVALID_HANDLE_VALUE, None, PAGE_READWRITE, 0, 4, &noms.affiche).ok()?;
            let vue = MapViewOfFile(projection, FILE_MAP_WRITE, 0, 0, 4);
            if vue.Value.is_null() {
                let _ = CloseHandle(projection);
                return None;
            }
            (vue.Value as *mut u32).write_unaligned(std::process::id());
            let _ = UnmapViewOfFile(vue);
            Some(brut(projection))
        }
    }

    fn pid_affiche(noms: &Noms) -> Option<u32> {
        unsafe {
            let projection = OpenFileMappingW(FILE_MAP_READ.0, false, &noms.affiche).ok()?;
            let vue = MapViewOfFile(projection, FILE_MAP_READ, 0, 0, 4);
            let pid = if vue.Value.is_null() {
                0
            } else {
                let pid = (vue.Value as *const u32).read_unaligned();
                let _ = UnmapViewOfFile(vue);
                pid
            };
            let _ = CloseHandle(projection);
            (pid != 0).then_some(pid)
        }
    }

    /// Sonne l'instance en place. Vrai si son réveil existe.
    fn sonner(noms: &Noms) -> bool {
        let Ok(ev) = (unsafe { OpenEventW(EVENT_MODIFY_STATE, false, &noms.reveil) }) else {
            return false;
        };
        let sonne = unsafe { SetEvent(ev) }.is_ok();
        unsafe {
            let _ = CloseHandle(ev);
        }
        sonne
    }

    /// L'interface est revenue : le lancement qui attend l'apprend. Si
    /// personne n'attend (plus), l'événement n'existe pas — rien à faire.
    pub fn accuser(noms: &Noms) {
        if let Ok(ev) = unsafe { OpenEventW(EVENT_MODIFY_STATE, false, &noms.accuse) } {
            unsafe {
                let _ = SetEvent(ev);
                let _ = CloseHandle(ev);
            }
        }
    }

    pub fn lacher(mutex: isize, affiche: isize) {
        if mutex != 0 {
            // Rendu par le fil qui le tient ; ailleurs, l'appel échoue sans
            // dommage et la mort du processus le rendra.
            unsafe {
                let _ = ReleaseMutex(h(mutex));
            }
        }
        fermer(mutex);
        fermer(affiche);
    }

    /// L'événement que les suivants sonnent — auto-réarmé : une sonnerie,
    /// une lecture.
    pub fn creer_reveil(noms: &Noms) -> Option<isize> {
        let ev = unsafe { CreateEventW(None, false, false, &noms.reveil) }.ok()?;
        Some(brut(ev))
    }

    pub fn sonne(ev: isize) -> bool {
        ev != 0 && unsafe { WaitForSingleObject(h(ev), 0) } == WAIT_OBJECT_0
    }

    /// L'instance en place ne répond pas : on le dit, et l'on propose de la
    /// fermer. Oui : elle est fermée de force, et le verrou nous revient.
    pub fn resoudre(mutex: isize, pid: Option<u32>, sonnee: bool) -> Demarrage {
        // Qui fermer : l'instance qui affiche son numéro. Une instance
        // d'avant 0.1.45 ne l'affiche pas (ni n'accuse le réveil) : ce sont
        // alors les ki-chat de la machine. La 0.1.44 tenait une instance
        // sonnée pour revenue, et se retirait — c'est ce silence-là, face à
        // une instance figée, qui faisait « ki-chat ne se lance plus ».
        let cibles = match pid {
            Some(pid) => vec![pid],
            None => autres_ki_chat(),
        };
        if cibles.is_empty() {
            fermer(mutex);
            tracing::warn!("instance : aucun ki-chat à fermer n'a été trouvé");
            boite(
                "ki-chat est déjà lancé, mais il ne répond pas.\n\n\
                 Ferme « ki-chat » dans le Gestionnaire des tâches (Ctrl+Maj+Échap), puis relance-le.",
                MB_OK | MB_ICONWARNING,
            );
            return Demarrage::DejaLancee;
        }
        let constat = if sonnee {
            "sa fenêtre n'est pas revenue en dix secondes"
        } else {
            "il n'a pas de fenêtre, dix secondes après"
        };
        let question = format!(
            "ki-chat est déjà lancé, mais il ne répond pas : {constat}.\n\n\
             Fermer l'ancien ki-chat et en ouvrir un nouveau ?"
        );
        if boite(&question, MB_YESNO | MB_ICONWARNING) != IDYES {
            fermer(mutex);
            tracing::info!("instance : l'utilisateur garde l'ancien ki-chat (processus {cibles:?})");
            return Demarrage::DejaLancee;
        }
        for pid in cibles {
            match fermer_de_force(pid) {
                Ok(()) => tracing::warn!("instance : l'ancien ki-chat (processus {pid}) fermé de force, à la demande"),
                Err(e) => tracing::warn!("instance : l'ancien ki-chat (processus {pid}) n'a pas pu être fermé : {e}"),
            }
        }
        let r = unsafe { WaitForSingleObject(h(mutex), APRES_FORCE.as_millis() as u32) };
        if r == WAIT_OBJECT_0 || r == WAIT_ABANDONED {
            return Demarrage::Premiere(tenir(&Noms::session(), h(mutex)));
        }
        fermer(mutex);
        boite(
            "L'ancien ki-chat n'a pas pu être fermé.\n\n\
             Ferme « ki-chat » dans le Gestionnaire des tâches (Ctrl+Maj+Échap), puis relance-le.",
            MB_OK | MB_ICONERROR,
        );
        Demarrage::DejaLancee
    }

    /// Les autres ki-chat de la machine — `ki-chat.exe`, ou `ki-chat.old` le
    /// temps d'une mise à jour —, nous exceptés. Ceux d'un autre compte
    /// Windows y figurent, mais `OpenProcess` nous les refusera.
    fn autres_ki_chat() -> Vec<u32> {
        let moi = std::process::id();
        let mut trouves = Vec::new();
        unsafe {
            let Ok(cliche) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
                return trouves;
            };
            let mut entree = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut encore = Process32FirstW(cliche, &mut entree).is_ok();
            while encore {
                let fin = entree.szExeFile.iter().position(|&c| c == 0).unwrap_or(entree.szExeFile.len());
                let nom = String::from_utf16_lossy(&entree.szExeFile[..fin]).to_lowercase();
                if entree.th32ProcessID != moi && nom.starts_with("ki-chat") {
                    trouves.push(entree.th32ProcessID);
                }
                encore = Process32NextW(cliche, &mut entree).is_ok();
            }
            let _ = CloseHandle(cliche);
        }
        trouves
    }

    /// Ferme de force un processus — seulement s'il est bien un ki-chat : un
    /// numéro de processus peut resservir, on ne ferme que ce qu'on
    /// reconnaît.
    fn fermer_de_force(pid: u32) -> Result<(), String> {
        unsafe {
            let p = OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, false, pid)
                .map_err(|e| e.to_string())?;
            let verdict = if est_ki_chat(p) {
                TerminateProcess(p, 1).map_err(|e| e.to_string()).map(|()| {
                    let _ = WaitForSingleObject(p, APRES_FORCE.as_millis() as u32);
                })
            } else {
                Err("ce processus n'est pas ki-chat".into())
            };
            let _ = CloseHandle(p);
            verdict
        }
    }

    /// Le nom de l'exécutable commence-t-il par « ki-chat » ? (`ki-chat.exe`,
    /// ou `ki-chat.old` le temps d'une mise à jour.)
    fn est_ki_chat(p: HANDLE) -> bool {
        let mut tampon = [0u16; 1024];
        let mut taille = tampon.len() as u32;
        let lu = unsafe { QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, PWSTR(tampon.as_mut_ptr()), &mut taille) };
        if lu.is_err() {
            return false;
        }
        let chemin = String::from_utf16_lossy(&tampon[..taille as usize]);
        std::path::Path::new(&chemin)
            .file_name()
            .is_some_and(|n| n.to_string_lossy().to_lowercase().starts_with("ki-chat"))
    }

    fn boite(texte: &str, style: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
        unsafe {
            MessageBoxW(
                None,
                &HSTRING::from(texte),
                &HSTRING::from("ki-chat"),
                style | MB_SETFOREGROUND | MB_TOPMOST,
            )
        }
    }
}

/// Au démarrage : le verrou ; ou le constat qu'un autre ki-chat est revenu
/// au premier plan ; ou qu'il tient le verrou sans répondre.
pub fn prendre() -> Demarrage {
    #[cfg(windows)]
    {
        if nouvelle_demandee() {
            tracing::info!("instance : {ARG_NOUVELLE} — pas de verrou");
            return Demarrage::Premiere(Verrou { mutex: 0, affiche: 0 });
        }
        natif::prendre(&natif::Noms::session(), natif::ATTENTE)
    }
    #[cfg(not(windows))]
    {
        Demarrage::Premiere(Verrou {})
    }
}

/// L'interface est revenue au premier plan après un réveil : le lancement
/// qui attend l'apprend, et se retire.
pub fn accuser_reveil() {
    #[cfg(windows)]
    natif::accuser(&natif::Noms::session());
}

impl Bloquee {
    /// Demande à l'utilisateur s'il faut fermer l'instance qui ne répond
    /// pas, et le fait s'il le veut : le verrou nous revient alors
    /// ([`Demarrage::Premiere`]). Sinon, celui-ci se retire.
    pub fn resoudre(self) -> Demarrage {
        #[cfg(windows)]
        {
            let (mutex, pid, sonnee) = (self.mutex, self.pid, self.sonnee);
            // Le handle passe à `natif::resoudre`, qui le ferme ou le garde.
            std::mem::forget(self);
            natif::resoudre(mutex, pid, sonnee)
        }
        #[cfg(not(windows))]
        {
            drop(self);
            Demarrage::DejaLancee
        }
    }
}

impl Drop for Bloquee {
    fn drop(&mut self) {
        #[cfg(windows)]
        natif::fermer(self.mutex);
    }
}

impl Verrou {
    /// Lâche le verrou avant l'heure — pour laisser la place au successeur
    /// qu'on relance (mise à jour, relance automatique).
    pub fn lacher(self) {
        drop(self);
    }
}

impl Drop for Verrou {
    fn drop(&mut self) {
        #[cfg(windows)]
        natif::lacher(std::mem::take(&mut self.mutex), std::mem::take(&mut self.affiche));
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
            let ev = natif::creer_reveil(&natif::Noms::session()).unwrap_or(0);
            if ev == 0 {
                tracing::warn!("instance : événement de réveil impossible à créer — un second lancement ne pourra pas nous ramener");
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

    /// Des noms à ce test seul : jamais ceux du vrai ki-chat de la machine,
    /// qu'on ferait revenir au premier plan à chaque essai.
    #[cfg(windows)]
    fn racine(nom: &str) -> String {
        format!("ki-chat-essai-{}-{nom}", std::process::id())
    }

    /// Le premier prend le verrou ; le second sonne, l'interface du premier
    /// accuse, le second se retire — sans attendre les dix secondes.
    #[cfg(windows)]
    #[test]
    fn le_second_sonne_et_le_premier_accuse() {
        let racine = racine("accuse");
        let noms = natif::Noms::de(&racine);
        let Demarrage::Premiere(verrou) = natif::prendre(&noms, Duration::from_secs(5)) else {
            panic!("le premier prend le verrou");
        };
        // L'oreille du premier, et son interface qui accuse.
        let reveil = natif::creer_reveil(&noms).expect("réveil");
        let interface = {
            let racine = racine.clone();
            std::thread::spawn(move || {
                let noms = natif::Noms::de(&racine);
                let depart = Instant::now();
                while depart.elapsed() < Duration::from_secs(5) {
                    if natif::sonne(reveil) {
                        natif::accuser(&noms);
                        return true;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                false
            })
        };
        // Le second, sur un autre fil : le fil qui tient un mutex le
        // reprendrait librement.
        let second = std::thread::spawn(move || {
            let noms = natif::Noms::de(&racine);
            let depart = Instant::now();
            let retire = matches!(natif::prendre(&noms, Duration::from_secs(5)), Demarrage::DejaLancee);
            (retire, depart.elapsed())
        });
        let (retire, duree) = second.join().unwrap();
        assert!(interface.join().unwrap(), "le réveil a sonné");
        assert!(retire, "le second se retire");
        assert!(duree < Duration::from_secs(3), "sans attendre : {duree:?}");
        natif::fermer(reveil);
        verrou.lacher();
    }

    /// Un premier qui n'accuse pas — sans interface, ou figée — est déclaré
    /// bloqué, avec son numéro de processus ; et le verrou qu'un mort n'a
    /// pas rendu se reprend.
    #[cfg(windows)]
    #[test]
    fn un_premier_muet_est_bloque_et_un_mort_rend_le_verrou() {
        let racine = racine("muet");
        let (pret_tx, pret_rx) = std::sync::mpsc::channel();
        let (fin_tx, fin_rx) = std::sync::mpsc::channel::<()>();
        let premier = {
            let racine = racine.clone();
            std::thread::spawn(move || {
                let noms = natif::Noms::de(&racine);
                let Demarrage::Premiere(verrou) = natif::prendre(&noms, Duration::from_secs(1)) else {
                    panic!("le premier prend le verrou");
                };
                pret_tx.send(()).unwrap();
                fin_rx.recv().unwrap();
                // Mort sans rendre le verrou : le fil s'achève en le tenant.
                std::mem::forget(verrou);
            })
        };
        pret_rx.recv().unwrap();
        let noms = natif::Noms::de(&racine);
        let depart = Instant::now();
        let Demarrage::Bloquee(bloquee) = natif::prendre(&noms, Duration::from_millis(600)) else {
            panic!("un premier muet est déclaré bloqué");
        };
        assert!(depart.elapsed() >= Duration::from_millis(600), "on l'a attendu");
        assert_eq!(bloquee.pid, Some(std::process::id()), "il affiche son numéro");
        assert!(!bloquee.sonnee, "pas de garde-fou, pas de réveil à sonner");
        drop(bloquee);
        fin_tx.send(()).unwrap();
        premier.join().unwrap();
        let Demarrage::Premiere(verrou) = natif::prendre(&noms, Duration::from_secs(1)) else {
            panic!("le verrou abandonné par un mort se reprend");
        };
        verrou.lacher();
    }
}
