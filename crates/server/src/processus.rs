//! Les programmes externes que le serveur lance — yt-dlp, ffmpeg, ffprobe —
//! et les bornes qu'on leur pose.
//!
//! # Ce qu'un enfant n'hérite plus
//!
//! Ils héritaient de tout l'environnement du serveur, `KI_TOKEN` et
//! `KI_HENRIK_KEY` compris : une faille dans un démultiplexeur, sur un fichier
//! envoyé par un membre, livrait ces secrets. [`preparer`] ne leur transmet
//! plus qu'une liste courte de variables sans secret.
//!
//! # Tuer tout ce qu'on a lancé
//!
//! L'image embarque `yt-dlp_linux`, un exécutable PyInstaller « onefile » :
//! un chargeur qui extrait l'application dans `/tmp/_MEI…` puis lance
//! l'interpréteur en processus fils. `Child::kill` n'atteignait que le
//! chargeur : l'interpréteur survivait, gardait les tubes ouverts — le
//! serveur attendait sa fin, délais compris —, et le dossier extrait restait
//! sur le disque à chaque morceau sauté. Chaque enfant a désormais son propre
//! groupe de processus, et [`arreter`] le termine en entier : TERM d'abord,
//! que le chargeur relaie avant de nettoyer son dossier, KILL ensuite.
//!
//! # Lire sans se laisser remplir
//!
//! La sortie d'un outil dépend de ce qu'on lui donne, donc parfois d'un
//! fichier envoyé par un membre : [`lire_borne`] n'en garde qu'une quantité
//! fixée, et vide le reste sans le garder pour ne pas bloquer l'enfant.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Les seules variables transmises aux outils. Aucune ne porte de secret ;
/// celles de Windows ne servent qu'aux essais sur la machine de dev, où un
/// processus sans `SystemRoot` ne démarre même pas.
const ENV_GARDEES: [&str; 18] = [
    "PATH",
    "HOME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "XDG_CACHE_HOME",
    "FONTCONFIG_FILE",
    "FONTCONFIG_PATH",
    "SYSTEMROOT",
    "WINDIR",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "LOCALAPPDATA",
    "APPDATA",
];

/// Le dossier temporaire des outils, à part : ce qu'ils y laissent se purge
/// sans toucher au reste. Posé par [`init`].
static TEMPORAIRE: OnceLock<PathBuf> = OnceLock::new();

/// Au démarrage : crée le dossier temporaire des outils et jette ce qu'une
/// exécution précédente y a laissé.
pub fn init() {
    let dossier = std::env::temp_dir().join("ki-outils");
    let jetes = purger_temporaire(&dossier, Duration::ZERO);
    if jetes > 0 {
        tracing::info!("outils : {jetes} dossier(s) temporaire(s) d'une exécution précédente jeté(s)");
    }
    if let Err(e) = std::fs::create_dir_all(&dossier) {
        tracing::warn!("outils : dossier temporaire {} impossible : {e}", dossier.display());
        return;
    }
    let _ = TEMPORAIRE.set(dossier);
}

/// Le dossier temporaire des outils — celui du système tant que [`init`]
/// n'a pas tourné.
pub fn temporaire() -> PathBuf {
    TEMPORAIRE.get().cloned().unwrap_or_else(std::env::temp_dir)
}

/// Jette les dossiers et fichiers temporaires des outils plus vieux que
/// `age_min`. Un outil qui tourne encore garde le sien : il n'a pas cet âge.
pub fn purger(age_min: Duration) -> usize {
    match TEMPORAIRE.get() {
        Some(dossier) => purger_temporaire(dossier, age_min),
        None => 0,
    }
}

fn purger_temporaire(dossier: &Path, age_min: Duration) -> usize {
    let maintenant = std::time::SystemTime::now();
    let mut jetes = 0;
    for entree in std::fs::read_dir(dossier).into_iter().flatten().flatten() {
        let vieux = entree
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|quand| maintenant.duration_since(quand).ok())
            .is_some_and(|age| age >= age_min);
        if !vieux {
            continue;
        }
        let chemin = entree.path();
        let fait = if chemin.is_dir() {
            std::fs::remove_dir_all(&chemin)
        } else {
            std::fs::remove_file(&chemin)
        };
        if fait.is_ok() {
            jetes += 1;
        }
    }
    jetes
}

/// Prépare une commande avant son lancement : environnement réduit à
/// [`ENV_GARDEES`], dossier temporaire à part, et — sous Unix — un groupe de
/// processus à elle, pour que [`arreter`] atteigne aussi ses propres enfants.
pub fn preparer(cmd: &mut Command) -> &mut Command {
    let gardees: Vec<(&str, OsString)> = ENV_GARDEES
        .iter()
        .filter_map(|nom| std::env::var_os(nom).map(|v| (*nom, v)))
        .collect();
    cmd.env_clear();
    for (nom, valeur) in gardees {
        cmd.env(nom, valeur);
    }
    if let Some(dossier) = TEMPORAIRE.get() {
        cmd.env("TMPDIR", dossier);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd
}

/// Le temps laissé à un groupe après TERM avant KILL : de quoi laisser le
/// chargeur de yt-dlp nettoyer son dossier extrait.
#[cfg(unix)]
const GRACE: Duration = Duration::from_secs(2);

/// Termine un enfant lancé par [`preparer`], lui et tout son groupe, puis le
/// récolte — un enfant jamais attendu reste en zombie.
pub fn arreter(enfant: &mut Child) {
    if matches!(enfant.try_wait(), Ok(Some(_))) {
        tuer_le_groupe(enfant, false);
        return;
    }
    #[cfg(unix)]
    {
        signaler(enfant, libc::SIGTERM);
        let debut = Instant::now();
        while debut.elapsed() < GRACE {
            if matches!(enfant.try_wait(), Ok(Some(_))) {
                // Le chef est parti ; ses éventuels enfants restants aussi.
                tuer_le_groupe(enfant, false);
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    tuer_le_groupe(enfant, true);
}

/// KILL au groupe (sous Unix), puis attente du chef si `attendre`.
fn tuer_le_groupe(enfant: &mut Child, attendre: bool) {
    #[cfg(unix)]
    signaler(enfant, libc::SIGKILL);
    #[cfg(not(unix))]
    let _ = enfant.kill();
    if attendre {
        let _ = enfant.wait();
    }
}

#[cfg(unix)]
fn signaler(enfant: &Child, signal: libc::c_int) {
    let Ok(pid) = libc::pid_t::try_from(enfant.id()) else {
        return;
    };
    // SAFETY: `kill` ne touche à aucune mémoire du processus ; un groupe
    // déjà vide rend ESRCH, sans effet. Le signe négatif vise le groupe dont
    // l'enfant est le chef (`process_group(0)` dans `preparer`).
    unsafe {
        libc::kill(-pid, signal);
    }
}

/// Lit `lecteur` jusqu'au bout, en ne gardant que `max` octets ; le reste est
/// lu et jeté, pour ne jamais bloquer l'enfant sur un tube plein. Rend aussi
/// `true` si la sortie a été tronquée.
pub fn lire_borne(mut lecteur: impl Read, max: usize) -> (Vec<u8>, bool) {
    let mut garde = Vec::new();
    let mut tronque = false;
    let mut tampon = [0u8; 16 * 1024];
    loop {
        match lecteur.read(&mut tampon) {
            Ok(0) => break,
            Ok(n) => {
                let place = max.saturating_sub(garde.len());
                if n > place {
                    tronque = true;
                }
                garde.extend_from_slice(&tampon[..n.min(place)]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    (garde, tronque)
}

/// Lance `corps` sur un fil nommé. `std::thread::spawn` panique quand le
/// système refuse un fil — et avec `panic = "abort"`, c'était tout le serveur
/// qui tombait pour un morceau de musique.
pub fn fil<T: Send + 'static>(
    nom: &str,
    corps: impl FnOnce() -> T + Send + 'static,
) -> Result<std::thread::JoinHandle<T>, String> {
    std::thread::Builder::new()
        .name(nom.to_string())
        .spawn(corps)
        .map_err(|e| format!("fil « {nom} » impossible : {e}"))
}

/// Ce que [`executer_borne`] garde de la sortie standard.
pub const SORTIE_MAX: usize = 32 * 1024 * 1024;
/// Et de la sortie d'erreur : de quoi dire pourquoi, pas davantage.
const ERREUR_MAX: usize = 64 * 1024;

/// Lance la commande (préparée par [`preparer`]), lit sa sortie standard — au
/// plus [`SORTIE_MAX`] —, et l'arrête avec tout son groupe si elle dépasse le
/// délai. Rend la sortie, ou la fin de la sortie d'erreur.
pub fn executer_borne(
    cmd: &mut Command,
    delai: Duration,
    resume_erreur: impl Fn(&[u8]) -> String,
) -> Result<Vec<u8>, String> {
    let mut enfant = preparer(cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("lancement impossible : {e}"))?;
    let sortie = enfant.stdout.take().expect("stdout");
    let erreur = enfant.stderr.take().expect("stderr");
    // Lecture sur des fils à part : les tubes doivent être vidés pendant
    // qu'on surveille le délai, sinon un enfant bavard se bloque dessus.
    let lecteur = match fil("outil-sortie", move || lire_borne(sortie, SORTIE_MAX)) {
        Ok(l) => l,
        Err(e) => {
            arreter(&mut enfant);
            return Err(e);
        }
    };
    let lecteur_err = match fil("outil-erreur", move || lire_borne(erreur, ERREUR_MAX)) {
        Ok(l) => l,
        Err(e) => {
            arreter(&mut enfant);
            return Err(e);
        }
    };
    let debut = Instant::now();
    let statut = loop {
        match enfant.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if debut.elapsed() > delai => {
                arreter(&mut enfant);
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => {
                arreter(&mut enfant);
                break None;
            }
        }
    };
    // Le groupe entier est parti : les tubes sont fermés, les lecteurs
    // finissent. Sans ça, un petit-enfant resté vivant les tenait ouverts.
    tuer_le_groupe(&mut enfant, false);
    let (sortie, tronquee) = lecteur.join().unwrap_or_default();
    let (erreur, _) = lecteur_err.join().unwrap_or_default();
    match statut {
        Some(s) if s.success() && !tronquee => Ok(sortie),
        Some(s) if s.success() => Err(format!(
            "sortie trop longue (plus de {} Mo)",
            SORTIE_MAX / (1024 * 1024)
        )),
        Some(_) => Err(resume_erreur(&erreur)),
        None => Err("délai dépassé".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Au-delà de la borne, la sortie est tronquée — et tout est lu quand
    /// même, pour que l'enfant n'attende jamais sur un tube plein.
    #[test]
    fn la_lecture_est_bornee_et_vide_tout() {
        let source = vec![7u8; 100_000];
        let mut curseur = std::io::Cursor::new(source);
        let (garde, tronque) = lire_borne(&mut curseur, 1000);
        assert_eq!(garde.len(), 1000);
        assert!(tronque);
        assert_eq!(curseur.position(), 100_000, "tout doit être lu");
        let (garde, tronque) = lire_borne(std::io::Cursor::new(vec![1u8; 10]), 1000);
        assert_eq!((garde.len(), tronque), (10, false));
    }

    /// Un enfant ne reçoit aucun secret du serveur : seulement la courte
    /// liste des variables gardées.
    #[test]
    fn un_enfant_n_herite_d_aucun_secret() {
        let mut cmd = Command::new("inexistant");
        cmd.env("KI_TOKEN", "secret");
        preparer(&mut cmd);
        let transmises: Vec<String> = cmd
            .get_envs()
            .filter_map(|(k, v)| v.map(|_| k.to_string_lossy().into_owned()))
            .collect();
        assert!(!transmises.iter().any(|k| k.starts_with("KI_")), "{transmises:?}");
        assert!(transmises.iter().all(|k| ENV_GARDEES.contains(&k.as_str()) || k == "TMPDIR"));
    }

    /// Une commande qui dépasse son délai est arrêtée, et le délai tient.
    #[cfg(unix)]
    #[test]
    fn une_commande_trop_longue_est_arretee() {
        let debut = Instant::now();
        let r = executer_borne(
            Command::new("sh").args(["-c", "sleep 30"]),
            Duration::from_millis(200),
            |_| String::new(),
        );
        assert_eq!(r.err().as_deref(), Some("délai dépassé"));
        assert!(debut.elapsed() < Duration::from_secs(10), "{:?}", debut.elapsed());
    }

    /// Le groupe entier part : un petit-enfant qui garde la sortie ouverte
    /// ne retient plus le serveur jusqu'à sa propre fin.
    #[cfg(unix)]
    #[test]
    fn un_petit_enfant_ne_retient_pas_la_lecture() {
        let debut = Instant::now();
        let r = executer_borne(
            Command::new("sh").args(["-c", "sleep 30 & sleep 30"]),
            Duration::from_millis(200),
            |_| String::new(),
        );
        assert!(r.is_err());
        assert!(debut.elapsed() < Duration::from_secs(10), "{:?}", debut.elapsed());
    }
}
