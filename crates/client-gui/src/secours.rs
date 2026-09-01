//! Dispositif de secours : journal sur disque, panic hook, et relance après
//! une erreur fatale de la boucle graphique.
//!
//! Pourquoi ce module existe. Le binaire de production est compilé
//! `panic = "abort"` et `windows_subsystem = "windows"` : pas de console, et
//! le moindre panic tue le processus sans un mot. Par-dessus, eframe traite
//! toute erreur de rendu comme fatale : un seul `SwapBuffers` raté — pilote
//! graphique réinitialisé sous un jeu, veille moderne, bascule iGPU/dGPU d'un
//! portable — et sa boucle d'événements se termine « proprement ». Vu du
//! joueur : ki-chat disparaît en pleine conversation, sans laisser de trace,
//! pas même dans l'Observateur d'événements — la sortie est un exit normal.
//!
//! Trois réponses, ensemble :
//!
//! - un **journal sur disque**, en plus de stderr : la prochaine mort laisse
//!   une histoire lisible, au lieu d'un souvenir ;
//! - un **panic hook** qui écrit le panic dans ce journal avant l'abort —
//!   sans lui, `panic = "abort"` rend tout plantage muet ;
//! - la **relance** : quand `eframe::run_native` rend une erreur, `main`
//!   relance l'exécutable. Un hoquet graphique passager coûte une seconde de
//!   fenêtre, plus la soirée. Le compteur `--relance N` transmis à l'instance
//!   suivante borne le cas pathologique : une erreur qui revient dès le
//!   démarrage épuise son budget et rend la main.
//!
//! S'y ajoute un **rapport de plantage** dédié (`ki-chat.crash`) : chaque
//! panic et chaque raté graphique y laissent leur trace, à part du journal
//! courant. C'est ce fichier que le diagnostic partagé (opt-in) embarque au
//! premier envoi de la session suivante — le journal, lui, mélange le
//! plantage au tout-venant et se fait réécrire à chaque démarrage.

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Taille au-delà de laquelle le journal est archivé au démarrage. L'archive
/// (`.old`) garde la session précédente : c'est souvent elle qu'on veut lire
/// après un incident.
const JOURNAL_MAX: u64 = 2 * 1024 * 1024;

/// Relances consécutives tolérées quand l'application meurt aussitôt
/// relancée. Au-delà, l'erreur n'est pas un hoquet : on s'arrête.
const RELANCES_MAX: u32 = 2;

/// Durée de vie au-delà de laquelle une session compte comme saine : l'erreur
/// qui la termine est un incident neuf, et la relance repart d'un budget
/// entier au lieu d'entamer celui des échecs consécutifs.
const SESSION_SAINE: Duration = Duration::from_secs(60);

/// Argument passé à l'exécutable relancé, pour compter les relances.
const ARG_RELANCE: &str = "--relance";

/// Taille de poche du rapport de plantage : au-delà, seule la fin survit
/// avant l'ajout suivant. Les plantages sont rares, mais une pile complète
/// pèse, et ce fichier n'a pas vocation à raconter des mois.
const RAPPORT_MAX: u64 = 192 * 1024;

/// Ce qui part au maximum dans le diagnostic partagé : la fin du rapport.
/// Assez pour plusieurs panics avec leur pile, assez peu pour que le lot
/// HTTP reste léger (le serveur borne de toute façon chaque dépôt).
const RAPPORT_ENVOI_MAX: usize = 32 * 1024;

/// Installe les traces (stderr + fichier) et le panic hook. Rend le chemin du
/// journal, `None` si le disque n'en veut pas — l'application démarre quand
/// même, avec stderr pour seul témoin, comme avant.
pub fn installer() -> Option<PathBuf> {
    let ouvert = ouvrir_journal();
    let fichier = ouvert.as_ref().map(|(f, _)| f.clone());

    let pour_traces = fichier.clone();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        // Sans couleurs : les mêmes octets vont sur stderr et dans le
        // fichier, et des séquences ANSI dans un journal le rendent illisible.
        .with_ansi(false)
        .with_writer(move || Tee { fichier: pour_traces.clone() })
        .init();

    if let Some(f) = fichier {
        installer_panic_hook(f);
    }
    ouvert.map(|(_, chemin)| chemin)
}

/// Chemin du journal, recalculé — pour l'afficher à l'utilisateur sans avoir
/// à faire voyager celui rendu par [`installer`].
pub fn chemin_journal() -> Option<PathBuf> {
    Some(eframe::storage_dir("ki-chat")?.join("ki-chat.log"))
}

/// Chemin du rapport de plantage, à côté du journal.
pub fn chemin_crash() -> Option<PathBuf> {
    Some(eframe::storage_dir("ki-chat")?.join("ki-chat.crash"))
}

/// Consigne un plantage dans le rapport dédié, en plus du journal. Appelé
/// par le panic hook et par `main` quand la boucle graphique meurt : ce sont
/// les deux seules plumes de ce fichier, il ne contient donc que du
/// technique — jamais un message, jamais de l'audio.
pub fn consigner_crash(quoi: &str) {
    let Some(chemin) = chemin_crash() else { return };
    if let Some(dossier) = chemin.parent() {
        let _ = std::fs::create_dir_all(dossier);
    }
    consigner_dans(&chemin, quoi);
}

/// L'écriture elle-même, sur un chemin fourni — testable sans le dossier
/// d'eframe. Chaque échec est avalé : on est déjà en train de mourir, la
/// dernière chose à faire est d'échouer plus fort.
fn consigner_dans(chemin: &Path, quoi: &str) {
    borner_rapport(chemin);
    let quand = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f");
    if let Ok(mut f) =
        std::fs::OpenOptions::new().create(true).append(true).open(chemin)
    {
        let _ = writeln!(f, "==== CRASH {quand} ====\n{quoi}");
    }
}

/// Ramène le rapport à sa moitié de borne quand il enfle : les plantages
/// s'empilent en append, seuls les derniers comptent — les anciens sont
/// déjà partis dans le diagnostic, ou ne racontent plus rien d'actuel.
fn borner_rapport(chemin: &Path) {
    let trop = std::fs::metadata(chemin).map(|m| m.len() > RAPPORT_MAX).unwrap_or(false);
    if !trop {
        return;
    }
    let Ok(tout) = std::fs::read(chemin) else { return };
    let garde = tout.len().saturating_sub((RAPPORT_MAX / 2) as usize);
    let _ = std::fs::write(chemin, &tout[garde..]);
}

/// Le rapport de plantage à joindre au diagnostic partagé, ou `None` s'il
/// n'y a rien de neuf : pas de fichier, ou un horodatage de modification
/// déjà égal à `deja_envoye` — chaque plantage ne voyage qu'une fois.
/// Rend (horodatage à mémoriser, fin du rapport bornée à l'envoi).
pub fn rapport_a_envoyer(deja_envoye: &str) -> Option<(String, String)> {
    rapport_dans(&chemin_crash()?, deja_envoye)
}

fn rapport_dans(chemin: &Path, deja_envoye: &str) -> Option<(String, String)> {
    let tampon = std::fs::metadata(chemin)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos()
        .to_string();
    if tampon == deja_envoye {
        return None;
    }
    // Lecture en octets puis conversion tolérante : la borne d'archivage
    // coupe au milieu de n'importe quoi, un caractère abîmé ne doit pas
    // faire taire tout le rapport.
    let octets = std::fs::read(chemin).ok()?;
    let texte = String::from_utf8_lossy(&octets);
    let vise = texte.len().saturating_sub(RAPPORT_ENVOI_MAX);
    // Découpe à une frontière de caractère : on envoie du texte, pas un
    // début d'UTF-8 tronqué.
    let debut = (vise..=texte.len()).find(|i| texte.is_char_boundary(*i)).unwrap_or(0);
    Some((tampon, texte[debut..].to_string()))
}

/// Nombre de relances déjà subies par cette instance, lu des arguments.
/// Zéro pour un lancement ordinaire.
pub fn essais_actuels() -> u32 {
    essais_dans(std::env::args())
}

/// Décide d'une relance après une erreur de la boucle graphique. Rend le
/// compteur à transmettre à l'instance suivante, ou `None` si le budget est
/// épuisé — l'erreur revient dès le démarrage, relancer en boucle n'y
/// changerait rien.
pub fn decision_relance(essais: u32, vecu: Duration) -> Option<u32> {
    if vecu >= SESSION_SAINE {
        return Some(1);
    }
    let suivant = essais + 1;
    (suivant <= RELANCES_MAX).then_some(suivant)
}

/// Relance l'exécutable avec le compteur en argument. L'échec est consigné :
/// il n'y a rien d'autre à faire, l'instance courante est déjà condamnée.
pub fn relancer(essais: u32) {
    match std::env::current_exe() {
        Ok(exe) => match std::process::Command::new(exe)
            .arg(ARG_RELANCE)
            .arg(essais.to_string())
            .spawn()
        {
            Ok(_) => tracing::info!("relance automatique (tentative {essais})"),
            Err(e) => tracing::error!("relance impossible : {e}"),
        },
        Err(e) => tracing::error!("exécutable introuvable : {e}"),
    }
}

/// Écrit chaque événement sur stderr **et** dans le journal, sans tampon :
/// entre un abort et des octets encore en mémoire, les octets perdraient.
struct Tee {
    fichier: Option<Arc<File>>,
}

impl std::io::Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write_all(buf);
        if let Some(f) = &self.fichier {
            // Un disque plein ne doit pas faire taire stderr ni, surtout,
            // faire échouer la trace qui s'écrit.
            let _ = (&**f).write_all(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Ouvre le journal en ajout, après archivage éventuel.
fn ouvrir_journal() -> Option<(Arc<File>, PathBuf)> {
    let chemin = chemin_journal()?;
    if let Some(dossier) = chemin.parent() {
        std::fs::create_dir_all(dossier).ok()?;
    }
    archiver_si_plus_gros_que(&chemin, JOURNAL_MAX);
    let fichier = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&chemin)
        .ok()?;
    Some((Arc::new(fichier), chemin))
}

/// Écarte le journal vers `.old` quand il dépasse `max` octets. Une seule
/// génération d'archive : assez pour relire la session d'avant, sans que le
/// dossier n'enfle.
fn archiver_si_plus_gros_que(chemin: &Path, max: u64) {
    let trop = std::fs::metadata(chemin).map(|m| m.len() > max).unwrap_or(false);
    if !trop {
        return;
    }
    let archive = chemin.with_extension("log.old");
    // `rename` refuse d'écraser sous Windows : on retire l'ancienne archive
    // d'abord, comme le fait la mise à jour pour son binaire écarté.
    let _ = std::fs::remove_file(&archive);
    let _ = std::fs::rename(chemin, &archive);
}

/// Écrit tout panic dans le journal avant que `panic = "abort"` n'emporte le
/// processus. Le hook précédent (impression stderr) est conservé derrière.
fn installer_panic_hook(fichier: Arc<File>) {
    let precedent = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let quand = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f");
        // Sans les symboles (binaire `strip`), la pile n'a que des adresses —
        // elles se relisent contre le .pdb de la version concernée, c'est
        // toujours ça de plus qu'un fichier vide.
        let pile = std::backtrace::Backtrace::force_capture();
        let _ = writeln!(&*fichier, "==== PANIC {quand} ====\n{info}\n{pile}");
        // Le rapport dédié reçoit la même histoire : c'est lui que le
        // diagnostic partagé embarquera au prochain démarrage.
        consigner_crash(&format!("panic : {info}\n{pile}"));
        precedent(info);
    }));
}

/// Cherche `--relance N` dans des arguments. Tout ce qui ne se lit pas
/// compte pour zéro : un argument abîmé ne doit pas fabriquer un budget.
fn essais_dans(args: impl Iterator<Item = String>) -> u32 {
    let mut args = args;
    while let Some(a) = args.next() {
        if a == ARG_RELANCE {
            return args.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(liste: &[&str]) -> impl Iterator<Item = String> {
        liste.iter().map(|s| s.to_string()).collect::<Vec<_>>().into_iter()
    }

    #[test]
    fn le_compteur_se_lit_des_arguments() {
        assert_eq!(essais_dans(args(&["ki-chat.exe"])), 0);
        assert_eq!(essais_dans(args(&["ki-chat.exe", "--relance", "2"])), 2);
        // Abîmé ou incomplet : zéro, jamais un budget inventé.
        assert_eq!(essais_dans(args(&["ki-chat.exe", "--relance", "beaucoup"])), 0);
        assert_eq!(essais_dans(args(&["ki-chat.exe", "--relance"])), 0);
    }

    #[test]
    fn une_session_saine_remet_le_budget_a_neuf() {
        // Une demi-heure de session : l'erreur est un incident neuf, on
        // relance quel que soit le passé de l'instance.
        let longue = Duration::from_secs(30 * 60);
        assert_eq!(decision_relance(0, longue), Some(1));
        assert_eq!(decision_relance(RELANCES_MAX, longue), Some(1));
    }

    #[test]
    fn les_echecs_immediats_epuisent_le_budget() {
        let aussitot = Duration::from_secs(3);
        assert_eq!(decision_relance(0, aussitot), Some(1));
        assert_eq!(decision_relance(1, aussitot), Some(2));
        // Troisième mort immédiate : on rend la main.
        assert_eq!(decision_relance(2, aussitot), None);
    }

    #[test]
    fn le_rapport_de_plantage_ne_part_qu_une_fois_et_seulement_sa_fin() {
        let dossier =
            std::env::temp_dir().join(format!("ki-secours-rapport-{}", std::process::id()));
        std::fs::create_dir_all(&dossier).expect("dossier de test");
        let chemin = dossier.join("ki-chat.crash");
        let _ = std::fs::remove_file(&chemin);

        // Pas de fichier : rien à envoyer, rien à faire.
        assert!(rapport_dans(&chemin, "").is_none());

        // Un plantage énorme : seule la fin part, bornée.
        let long = format!("{}la vraie fin", "x".repeat(2 * RAPPORT_ENVOI_MAX));
        std::fs::write(&chemin, &long).expect("écriture");
        let (tampon, texte) = rapport_dans(&chemin, "").expect("un rapport à envoyer");
        assert!(texte.len() <= RAPPORT_ENVOI_MAX);
        assert!(texte.ends_with("la vraie fin"));

        // Déjà envoyé : le même horodatage ne repart pas.
        assert!(rapport_dans(&chemin, &tampon).is_none());

        // Un nouveau plantage rajeunit le fichier : il repartira. (La pause
        // évite qu'une horloge de fichier grossière ne rende les deux
        // écritures indistinguables.)
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&chemin, "nouveau plantage").expect("écriture");
        assert!(rapport_dans(&chemin, &tampon).is_some());

        let _ = std::fs::remove_dir_all(&dossier);
    }

    #[test]
    fn les_plantages_s_empilent_dans_le_rapport_sans_l_enfler() {
        let dossier =
            std::env::temp_dir().join(format!("ki-secours-consigne-{}", std::process::id()));
        std::fs::create_dir_all(&dossier).expect("dossier de test");
        let chemin = dossier.join("ki-chat.crash");
        let _ = std::fs::remove_file(&chemin);

        consigner_dans(&chemin, "premier");
        consigner_dans(&chemin, "second");
        let texte = std::fs::read_to_string(&chemin).expect("lecture");
        assert!(texte.contains("premier") && texte.contains("second"));
        assert_eq!(texte.matches("==== CRASH ").count(), 2);

        // Un rapport au-delà de la borne est ramené à sa fin avant l'ajout.
        std::fs::write(&chemin, vec![b'x'; RAPPORT_MAX as usize + 1]).expect("écriture");
        consigner_dans(&chemin, "après la borne");
        assert!(std::fs::metadata(&chemin).expect("meta").len() < RAPPORT_MAX);
        assert!(std::fs::read_to_string(&chemin).expect("lecture").contains("après la borne"));

        let _ = std::fs::remove_dir_all(&dossier);
    }

    #[test]
    fn le_journal_s_archive_au_dela_du_seuil() {
        let dossier = std::env::temp_dir().join(format!("ki-secours-{}", std::process::id()));
        std::fs::create_dir_all(&dossier).expect("dossier de test");
        let chemin = dossier.join("ki-chat.log");
        let archive = dossier.join("ki-chat.log.old");

        // Petit : rien ne bouge.
        std::fs::write(&chemin, b"court").expect("écriture");
        archiver_si_plus_gros_que(&chemin, 100);
        assert!(chemin.exists() && !archive.exists());

        // Trop gros : écarté vers l'archive, en écrasant la précédente.
        std::fs::write(&archive, b"vieille archive").expect("écriture");
        std::fs::write(&chemin, vec![b'x'; 200]).expect("écriture");
        archiver_si_plus_gros_que(&chemin, 100);
        assert!(!chemin.exists(), "le journal plein doit être écarté");
        assert_eq!(std::fs::read(&archive).expect("archive").len(), 200);

        let _ = std::fs::remove_dir_all(&dossier);
    }
}
