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
//! - la même relance **après un panic** : le hook, une fois le plantage
//!   écrit, lance le successeur avant que l'abort n'emporte le processus,
//!   et lui dit où l'on était (`--retour serveur:salon:vocal`) pour qu'il y
//!   revienne — un plantage en plein vocal coûte quelques secondes de
//!   silence, plus la soirée.
//!
//! S'y ajoute un **rapport de plantage** dédié (`ki-chat.crash`) : chaque
//! panic et chaque raté graphique y laissent leur trace, à part du journal
//! courant. C'est ce fichier qui part au serveur à la connexion suivante —
//! **option de partage cochée ou non**, décision de l'admin du groupe (voir
//! `envoyer_crash` dans main.rs) — et au seul serveur où le plantage a eu
//! lieu. Le journal, lui, mélange le plantage au tout-venant et se fait
//! réécrire à chaque démarrage.

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

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

/// Argument passé au successeur d'une instance morte d'un panic : c'est un
/// plantage, pas un hoquet graphique, et le bandeau du démarrage le dit.
const ARG_PLANTAGE: &str = "--plantage";

/// Argument qui dit au successeur où l'on était : `serveur:salon:vocal`, le
/// serveur par son numéro dans le carnet, 0 pour « nulle part ».
const ARG_RETOUR: &str = "--retour";

/// Le démarrage de cette instance : une session qui a duré est saine, et le
/// plantage qui la termine repart d'un budget de relances entier.
static DEPART: OnceLock<Instant> = OnceLock::new();

/// Où l'on est, tenu à jour par l'interface à chaque image : le hook de
/// panic n'a que ça à lire, sans verrou — il peut courir sur n'importe quel
/// fil, pendant que celui de l'interface tient les siens. 0 : nulle part.
static ICI_SERVEUR: AtomicU64 = AtomicU64::new(0);
static ICI_SALON: AtomicU32 = AtomicU32::new(0);
static ICI_VOCAL: AtomicU32 = AtomicU32::new(0);

/// Un successeur est déjà lancé : deux fils qui paniquent ensemble n'en
/// lancent pas deux.
static SUCCESSEUR_LANCE: AtomicBool = AtomicBool::new(false);

/// Où revenir après une relance : le serveur du carnet, le salon lu, le
/// salon vocal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Retour {
    pub serveur: u64,
    pub salon: Option<u32>,
    pub vocal: Option<u32>,
}

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
    DEPART.get_or_init(Instant::now);
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

/// Le marqueur d'une diffusion en cours, à côté du journal. Posé quand le
/// stream démarre, levé quand il s'arrête proprement : s'il est encore là
/// au démarrage suivant, la session précédente est morte en pleine
/// diffusion — ki-chat tué, ou la machine entière (écran bleu), ce que ni
/// le panic hook ni le journal audio ne peuvent raconter.
fn chemin_diffusion() -> Option<PathBuf> {
    Some(eframe::storage_dir("ki-chat")?.join("diffusion.en-cours"))
}

/// Pose le marqueur : `quoi` dit avec quel encodeur (« Nvenc », « Auto »,
/// « Logiciel »).
pub fn marquer_diffusion(quoi: &str) {
    if let Some(chemin) = chemin_diffusion() {
        let _ = std::fs::write(chemin, quoi);
    }
}

/// Lève le marqueur : la diffusion s'est arrêtée proprement.
pub fn lever_diffusion() {
    if let Some(chemin) = chemin_diffusion() {
        let _ = std::fs::remove_file(chemin);
    }
}

/// Au démarrage : le marqueur de la session précédente, s'il est resté —
/// et on le lève, pour ne le signaler qu'une fois.
pub fn diffusion_interrompue() -> Option<String> {
    let chemin = chemin_diffusion()?;
    let quoi = std::fs::read_to_string(&chemin).ok()?;
    let _ = std::fs::remove_file(chemin);
    Some(quoi.trim().to_string())
}

/// Le marqueur de l'enregistreur de clips, sur le même modèle : posé
/// quand il démarre, levé quand il s'arrête proprement. Retrouvé au
/// démarrage suivant, il dit que la session est morte l'enregistreur en
/// marche — NVENC tournait, comme en diffusion.
fn chemin_clips() -> Option<PathBuf> {
    Some(eframe::storage_dir("ki-chat")?.join("clips.en-cours"))
}

pub fn marquer_clips() {
    if let Some(chemin) = chemin_clips() {
        let _ = std::fs::write(chemin, "enregistreur");
    }
}

pub fn lever_clips() {
    if let Some(chemin) = chemin_clips() {
        let _ = std::fs::remove_file(chemin);
    }
}

/// Au démarrage : vrai si la session précédente est morte l'enregistreur en
/// marche — et on lève le marqueur, pour ne le signaler qu'une fois.
pub fn clips_interrompus() -> bool {
    let Some(chemin) = chemin_clips() else { return false };
    let present = chemin.is_file();
    let _ = std::fs::remove_file(chemin);
    present
}

/// Le serveur de la session en cours : un plantage en porte l'adresse, et
/// son rapport ne part qu'à ce serveur-là (voir `rapport_a_envoyer`).
static SERVEUR: Mutex<Option<String>> = Mutex::new(None);

/// Appelé à l'accueil d'un serveur (son adresse) et à la fin de la session
/// (`None`).
pub fn serveur_courant(adresse: Option<&str>) {
    if let Ok(mut s) = SERVEUR.lock() {
        *s = adresse.map(str::to_owned);
    }
}

/// Préfixe de la ligne qui dit, dans le rapport, où le plantage a eu lieu.
const LIGNE_SERVEUR: &str = "serveur : ";

/// Consigne un plantage dans le rapport dédié, en plus du journal. Appelé
/// par le panic hook et par `main` quand la boucle graphique meurt : ce sont
/// les deux seules plumes de ce fichier, il ne contient donc que du
/// technique — jamais un message, jamais de l'audio (les citations d'un
/// message de panique sont masquées, voir `masquer_citations`).
pub fn consigner_crash(quoi: &str) {
    let Some(chemin) = chemin_crash() else { return };
    if let Some(dossier) = chemin.parent() {
        let _ = std::fs::create_dir_all(dossier);
    }
    // `try_lock` : on peut être en train de mourir n'importe où.
    let serveur = SERVEUR.try_lock().ok().and_then(|s| s.clone()).unwrap_or_default();
    consigner_dans(&chemin, &serveur, quoi);
}

/// L'écriture elle-même, sur un chemin fourni — testable sans le dossier
/// d'eframe. Chaque échec est avalé : on est déjà en train de mourir, la
/// dernière chose à faire est d'échouer plus fort.
fn consigner_dans(chemin: &Path, serveur: &str, quoi: &str) {
    borner_rapport(chemin);
    let quand = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f");
    if let Ok(mut f) =
        std::fs::OpenOptions::new().create(true).append(true).open(chemin)
    {
        let _ = writeln!(f, "==== CRASH {quand} ====\n{LIGNE_SERVEUR}{serveur}\n{quoi}");
    }
}

/// Masque ce que le message d'une panique cite entre accents graves : la
/// bibliothèque standard y recopie la chaîne en cause (« byte index 5 is
/// not a char boundary … of `@toto é…` ») — le texte d'un message, qui n'a
/// rien à faire dans un rapport envoyé au serveur. Le reste (l'emplacement,
/// la pile) suffit à corriger.
fn masquer_citations(texte: &str) -> String {
    let mut sortie = String::with_capacity(texte.len());
    for (i, morceau) in texte.split('`').enumerate() {
        if i % 2 == 0 {
            sortie.push_str(morceau);
        } else {
            sortie.push_str(&format!("`[{} caractères masqués]`", morceau.chars().count()));
        }
    }
    sortie
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

/// Le rapport de plantage à envoyer à `serveur`, ou `None` s'il n'y a rien
/// de neuf : pas de fichier, ou un horodatage de modification déjà égal à
/// `deja_envoye` — chaque plantage ne voyage qu'une fois.
///
/// Seulement au serveur où le dernier plantage a eu lieu : il partait au
/// premier serveur rejoint ensuite, quel qu'il soit, et racontait à son admin
/// la session passée sur un autre. Ailleurs, le rapport attend sans être
/// consommé. Les plantages d'autres serveurs qui le précèdent sont écartés ;
/// ceux sans serveur (hors session, ou d'une version d'avant) partent.
/// Rend (horodatage à mémoriser, fin du rapport bornée à l'envoi).
pub fn rapport_a_envoyer(deja_envoye: &str, serveur: &str) -> Option<(String, String)> {
    rapport_dans(&chemin_crash()?, deja_envoye, serveur)
}

/// Le serveur d'un plantage consigné : `None` sans ligne `serveur`, et
/// `Some("")` pour un plantage hors session.
fn serveur_du(entree: &str) -> Option<&str> {
    entree.lines().nth(1)?.strip_prefix(LIGNE_SERVEUR).map(str::trim)
}

fn rapport_dans(chemin: &Path, deja_envoye: &str, serveur: &str) -> Option<(String, String)> {
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
    let brut = String::from_utf8_lossy(&octets);
    // Les plantages consignés, chacun avec son en-tête ; ce qui précède le
    // premier (une borne d'archivage coupée net) compte pour un sans serveur.
    let mut entrees: Vec<&str> = Vec::new();
    let mut reste: &str = &brut;
    loop {
        // La recherche repart après le premier caractère — entier : la
        // borne d'archivage coupe n'importe où, et un rapport peut commencer
        // au milieu d'un caractère devenu « � » (trois octets).
        let depart = reste.chars().next().map_or(reste.len(), char::len_utf8);
        match reste[depart..].find("==== CRASH ") {
            Some(i) => {
                entrees.push(&reste[..depart + i]);
                reste = &reste[depart + i..];
            }
            None => break,
        }
    }
    entrees.push(reste);
    let pour_ici = |e: &&str| serveur_du(e).is_none_or(|s| s.is_empty() || s == serveur);
    if !entrees.last().is_some_and(pour_ici) {
        return None;
    }
    let texte: String = entrees.into_iter().filter(pour_ici).collect();
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
/// `reduit` : on était réduit dans la zone de notification — l'instance
/// suivante y repart (`--reduit`), plutôt que de surgir en plein jeu.
pub fn relancer(essais: u32, reduit: bool) {
    match lancer_successeur(&arguments_du_successeur(essais, reduit, None, std::env::args())) {
        Ok(()) => tracing::info!("relance automatique (tentative {essais}, réduit : {reduit})"),
        Err(e) => tracing::error!("relance impossible : {e}"),
    }
}

/// Où l'on est, dit par l'interface à chaque image : le serveur (son numéro
/// dans le carnet) seulement une fois accueilli, le salon lu, le salon
/// vocal. C'est ce que le hook de panic transmettra au successeur.
pub fn situer(serveur: Option<u64>, salon: Option<u32>, vocal: Option<u32>) {
    ICI_SERVEUR.store(serveur.unwrap_or(0), Ordering::Relaxed);
    ICI_SALON.store(salon.unwrap_or(0), Ordering::Relaxed);
    ICI_VOCAL.store(vocal.unwrap_or(0), Ordering::Relaxed);
}

/// Où l'on était, d'après ce que l'interface a dit en dernier.
fn ici() -> Option<Retour> {
    let non_nul = |v: u32| Some(v).filter(|v| *v != 0);
    let serveur = ICI_SERVEUR.load(Ordering::Relaxed);
    (serveur != 0).then(|| Retour {
        serveur,
        salon: non_nul(ICI_SALON.load(Ordering::Relaxed)),
        vocal: non_nul(ICI_VOCAL.load(Ordering::Relaxed)),
    })
}

/// Les arguments du successeur : le compteur, l'état réduit, où revenir, et
/// ce qui doit se transmettre tel quel de `args` — le ki-chat de test reste
/// un ki-chat de test, et une seconde instance voulue le reste aussi : sans
/// ça, son successeur trouverait la première et se retirerait devant elle.
fn arguments_du_successeur(essais: u32, reduit: bool, retour: Option<Retour>, args: impl Iterator<Item = String>) -> Vec<String> {
    let mut suite = vec![ARG_RELANCE.to_string(), essais.to_string()];
    if reduit {
        suite.push(crate::zone::ARG_REDUIT.to_string());
    }
    if let Some(r) = retour {
        suite.push(ARG_RETOUR.to_string());
        suite.push(format!("{}:{}:{}", r.serveur, r.salon.unwrap_or(0), r.vocal.unwrap_or(0)));
    }
    suite.extend(args.filter(|a| a == "--mcp" || a == crate::instance::ARG_NOUVELLE));
    suite
}

fn lancer_successeur(arguments: &[String]) -> std::io::Result<()> {
    std::process::Command::new(std::env::current_exe()?).args(arguments).spawn().map(|_| ())
}

/// Après un panic, depuis le hook : lance le successeur, une fois, si le
/// budget de relances le permet. Rend ce qui s'est passé, à écrire au
/// journal — pas de `tracing` ici : le panic a pu naître sous son verrou.
///
/// Seulement quand un panic tue le processus (`panic = "abort"`, le binaire
/// publié). En version de développement un panic ne défait que son fil : le
/// processus vit, et un successeur ferait deux ki-chat.
fn relancer_apres_panic() -> String {
    if !cfg!(panic = "abort") {
        return "pas de relance : ce binaire survit à un panic".into();
    }
    if SUCCESSEUR_LANCE.swap(true, Ordering::SeqCst) {
        return "relance déjà lancée par un autre fil".into();
    }
    let vecu = DEPART.get().map(|d| d.elapsed()).unwrap_or_default();
    let Some(essais) = decision_relance(essais_actuels(), vecu) else {
        return "pas de relance : le plantage revient dès le démarrage, budget épuisé".into();
    };
    let reduit = crate::zone::repartir_reduit();
    let mut arguments = arguments_du_successeur(essais, reduit, ici(), std::env::args());
    arguments.push(ARG_PLANTAGE.to_string());
    match lancer_successeur(&arguments) {
        Ok(()) => format!("relance après plantage : {}", arguments.join(" ")),
        Err(e) => format!("relance impossible : {e}"),
    }
}

/// Cette instance succède-t-elle à un plantage (et non à un hoquet
/// graphique) ?
pub fn apres_plantage() -> bool {
    std::env::args().any(|a| a == ARG_PLANTAGE)
}

/// Où cette instance doit revenir, si celle d'avant le lui a dit.
pub fn retour_demande() -> Option<Retour> {
    retour_dans(std::env::args())
}

/// Cherche `--retour serveur:salon:vocal`. Abîmé, incomplet ou sans
/// serveur : rien — on ne revient pas à moitié.
fn retour_dans(mut args: impl Iterator<Item = String>) -> Option<Retour> {
    args.find(|a| a == ARG_RETOUR)?;
    let valeur = args.next()?;
    let mut morceaux = valeur.split(':');
    let serveur: u64 = morceaux.next()?.parse().ok()?;
    let salon: u32 = morceaux.next()?.parse().ok()?;
    let vocal: u32 = morceaux.next()?.parse().ok()?;
    if serveur == 0 || morceaux.next().is_some() {
        return None;
    }
    Some(Retour { serveur, salon: Some(salon).filter(|s| *s != 0), vocal: Some(vocal).filter(|v| *v != 0) })
}

/// Au-delà, une fermeture est tenue pour bloquée : un fil qu'on attend et
/// qui ne revient pas, un pilote qui ne rend pas la main. Une 0.1.43 est
/// ainsi restée figée trois quarts de minute après avoir installé la
/// 0.1.44, jusqu'à ce que Windows la tue — et la nouvelle version ne s'est
/// jamais relancée.
const FERMETURE_MAX: Duration = Duration::from_secs(10);

/// La fermeture commence : un fil la surveille. Si le processus vit encore
/// au bout de [`FERMETURE_MAX`], il relance la mise à jour qui attendait,
/// puis termine le processus sans attendre personne. Le successeur trouve
/// alors le verrou d'instance abandonné, et le prend.
pub fn surveiller_fermeture() {
    let lance = std::thread::Builder::new().name("ki-fermeture".into()).spawn(|| {
        std::thread::sleep(FERMETURE_MAX);
        tracing::error!(
            "fermeture bloquée depuis {} s — processus terminé de force",
            FERMETURE_MAX.as_secs()
        );
        consigner_crash("fermeture bloquée : processus terminé de force");
        crate::update::relaunch_if_requested(crate::zone::repartir_reduit());
        terminer_maintenant();
    });
    if let Err(e) = lance {
        tracing::warn!("fermeture : pas de surveillance ({e})");
    }
}

/// Le démontage de fin : des étapes lentes ou fragiles — des fils qu'on
/// attend, des périphériques qu'on rend —, jouées sur un fil à part et
/// attendues un temps borné. Ce qui traîne est nommé au journal, et ne
/// retient plus ni la fenêtre ni le verrou d'instance ; le garde-fou de
/// fermeture finit le travail. Ces étapes se jouaient une à une sur le fil
/// de l'interface, en détruisant l'application : une 0.1.44 est ainsi
/// restée sans fenêtre, le verrou en main, et ki-chat « ne s'ouvrait
/// plus » jusqu'à une fin de tâche.
#[derive(Default)]
pub struct Demontage {
    etapes: Vec<(&'static str, Etape)>,
}

/// Une étape du démontage : ce qu'elle arrête, jouée une fois.
type Etape = Box<dyn FnOnce() + Send>;

impl Demontage {
    pub fn etape(&mut self, nom: &'static str, etape: impl FnOnce() + Send + 'static) {
        self.etapes.push((nom, Box::new(etape)));
    }

    /// Joue les étapes dans l'ordre sur un fil à part, et les attend `max`
    /// au plus. Rend l'étape sur laquelle on a cessé d'attendre, s'il y en a
    /// une.
    pub fn jouer(self, max: Duration) -> Option<&'static str> {
        let en_cours: Arc<Mutex<&'static str>> = Arc::new(Mutex::new(""));
        let suivi = en_cours.clone();
        let (fini_tx, fini_rx) = mpsc::channel();
        let etapes = self.etapes;
        let lance = std::thread::Builder::new().name("ki-demontage".into()).spawn(move || {
            for (nom, etape) in etapes {
                *suivi.lock().unwrap_or_else(|e| e.into_inner()) = nom;
                let depart = Instant::now();
                etape();
                let ms = depart.elapsed().as_millis();
                if ms >= 100 {
                    tracing::info!("fermeture : {nom} en {ms} ms");
                }
            }
            let _ = fini_tx.send(());
        });
        if let Err(e) = lance {
            tracing::warn!("fermeture : pas de fil de démontage ({e})");
            return None;
        }
        match fini_rx.recv_timeout(max) {
            Ok(()) => None,
            Err(_) => {
                let nom = *en_cours.lock().unwrap_or_else(|e| e.into_inner());
                tracing::warn!(
                    "fermeture : le démontage traîne sur « {nom} » depuis {} ms — on n'attend plus",
                    max.as_millis()
                );
                Some(nom)
            }
        }
    }
}

/// Termine le processus tout de suite, sans détacher les bibliothèques ni
/// attendre personne : c'est justement là qu'une fermeture reste bloquée.
fn terminer_maintenant() -> ! {
    #[cfg(windows)]
    // SAFETY : le pseudo-handle du processus courant, toujours valide.
    unsafe {
        use windows::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
        let _ = TerminateProcess(GetCurrentProcess(), 0);
    }
    std::process::exit(0)
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
        // Le rapport dédié reçoit la même histoire, citations masquées :
        // c'est lui qui part au serveur à la connexion suivante.
        consigner_crash(&format!("panic : {}\n{pile}", masquer_citations(&info.to_string())));
        // Le successeur, avant que l'abort n'emporte tout : il attendra le
        // verrou d'instance, que notre mort lui rend.
        let _ = writeln!(&*fichier, "{}", relancer_apres_panic());
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

    /// Le démontage attend ses étapes, mais pas au-delà de sa borne : ce
    /// qui traîne est nommé, et l'appelant reprend la main.
    #[test]
    fn un_demontage_n_attend_pas_au_dela_de_sa_borne() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let fait = Arc::new(AtomicBool::new(false));
        let mut vite = Demontage::default();
        let f = fait.clone();
        vite.etape("rapide", move || f.store(true, Ordering::SeqCst));
        assert_eq!(vite.jouer(Duration::from_secs(5)), None);
        assert!(fait.load(Ordering::SeqCst), "l'étape a été jouée");

        let mut lent = Demontage::default();
        lent.etape("rapide", || {});
        lent.etape("lente", || std::thread::sleep(Duration::from_secs(3)));
        let depart = Instant::now();
        assert_eq!(lent.jouer(Duration::from_millis(200)), Some("lente"));
        assert!(depart.elapsed() < Duration::from_secs(2), "on n'a pas attendu la lente");
    }

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

    /// Ce que le successeur d'un plantage reçoit, et ce qu'il en relit : le
    /// compteur, l'état réduit, où revenir — et le ki-chat de test le reste.
    #[test]
    fn le_successeur_sait_d_ou_l_on_vient() {
        let ici = Retour { serveur: 3, salon: Some(2), vocal: Some(7) };
        let suite =
            arguments_du_successeur(1, true, Some(ici), args(&["ki-chat.exe", "--relance", "4", "--mcp", "--nouvelle-instance", "--reduit"]));
        assert_eq!(suite, ["--relance", "1", "--reduit", "--retour", "3:2:7", "--mcp", "--nouvelle-instance"]);
        // Relus par le successeur.
        let relus = std::iter::once("ki-chat.exe".to_string()).chain(suite);
        let relus: Vec<String> = relus.collect();
        assert_eq!(essais_dans(relus.iter().cloned()), 1);
        assert_eq!(retour_dans(relus.iter().cloned()), Some(ici));

        // Sans vocal, sans rien : les zéros ne sont pas des salons.
        let seul = Retour { serveur: 3, salon: None, vocal: None };
        let suite = arguments_du_successeur(2, false, Some(seul), args(&["ki-chat.exe"]));
        assert_eq!(suite, ["--relance", "2", "--retour", "3:0:0"]);
        assert_eq!(retour_dans(suite.into_iter()), Some(seul));
        assert_eq!(arguments_du_successeur(1, false, None, args(&["ki-chat.exe"])), ["--relance", "1"]);
        // Abîmé, incomplet, sans serveur : on ne revient pas à moitié.
        for abime in [&["--retour"][..], &["--retour", "3:2"], &["--retour", "0:2:7"], &["--retour", "a:b:c"], &["--retour", "3:2:7:9"], &["ki-chat.exe"]] {
            assert_eq!(retour_dans(args(abime)), None, "{abime:?}");
        }
    }

    /// Ce que l'interface dit de l'endroit où l'on est se relit tel quel, et
    /// « nulle part » ne fabrique pas de retour.
    #[test]
    fn l_endroit_ou_l_on_est_se_relit() {
        situer(Some(5), Some(1), None);
        assert_eq!(ici(), Some(Retour { serveur: 5, salon: Some(1), vocal: None }));
        situer(None, Some(1), Some(4));
        assert_eq!(ici(), None, "pas accueilli : pas de retour");
        // En version de développement, un panic ne tue pas le processus :
        // aucun successeur n'est lancé.
        if !cfg!(panic = "abort") {
            assert!(relancer_apres_panic().starts_with("pas de relance"));
        }
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
        assert!(rapport_dans(&chemin, "", "a:9987").is_none());

        // Un plantage énorme : seule la fin part, bornée.
        let long = format!("{}la vraie fin", "x".repeat(2 * RAPPORT_ENVOI_MAX));
        std::fs::write(&chemin, &long).expect("écriture");
        let (tampon, texte) = rapport_dans(&chemin, "", "a:9987").expect("un rapport à envoyer");
        assert!(texte.len() <= RAPPORT_ENVOI_MAX);
        assert!(texte.ends_with("la vraie fin"));

        // Déjà envoyé : le même horodatage ne repart pas.
        assert!(rapport_dans(&chemin, &tampon, "a:9987").is_none());

        // Un nouveau plantage rajeunit le fichier : il repartira. (La pause
        // évite qu'une horloge de fichier grossière ne rende les deux
        // écritures indistinguables.)
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&chemin, "nouveau plantage").expect("écriture");
        assert!(rapport_dans(&chemin, &tampon, "a:9987").is_some());

        let _ = std::fs::remove_dir_all(&dossier);
    }

    /// Un plantage ne se raconte qu'au serveur où il a eu lieu : ailleurs,
    /// le rapport attend sans être consommé ; chez lui, il part sans les
    /// plantages d'autres serveurs qui le précèdent.
    #[test]
    fn le_rapport_ne_part_qu_au_serveur_du_plantage() {
        let dossier =
            std::env::temp_dir().join(format!("ki-secours-serveur-{}", std::process::id()));
        std::fs::create_dir_all(&dossier).expect("dossier de test");
        let chemin = dossier.join("ki-chat.crash");
        let _ = std::fs::remove_file(&chemin);

        consigner_dans(&chemin, "b:9987", "plantage chez B");
        consigner_dans(&chemin, "", "plantage hors session");
        consigner_dans(&chemin, "a:9987", "plantage chez A");
        assert!(rapport_dans(&chemin, "", "b:9987").is_none(), "B n'a pas à savoir");
        let (_, texte) = rapport_dans(&chemin, "", "a:9987").expect("A le reçoit");
        assert!(texte.contains("plantage chez A") && texte.contains("hors session"));
        assert!(!texte.contains("chez B"));

        let _ = std::fs::remove_dir_all(&dossier);
    }

    /// Un rapport que la borne a coupé au milieu d'un caractère se lit quand
    /// même, sans paniquer.
    #[test]
    fn un_rapport_coupe_au_milieu_d_un_caractere_se_lit() {
        let dossier =
            std::env::temp_dir().join(format!("ki-secours-coupe-{}", std::process::id()));
        std::fs::create_dir_all(&dossier).expect("dossier de test");
        let chemin = dossier.join("ki-chat.crash");
        let mut octets = "é".as_bytes()[1..].to_vec();
        octets.extend_from_slice("é\n==== CRASH 2026 ====\nserveur : a:9987\nla fin\n".as_bytes());
        std::fs::write(&chemin, octets).expect("écriture");
        let (_, texte) = rapport_dans(&chemin, "", "a:9987").expect("un rapport à envoyer");
        assert!(texte.ends_with("la fin\n"));
        let _ = std::fs::remove_dir_all(&dossier);
    }

    /// Ce que la bibliothèque standard cite d'une chaîne en cause ne quitte
    /// pas la machine ; l'emplacement, si.
    #[test]
    fn les_citations_d_une_panique_sont_masquees() {
        let masque = masquer_citations(
            "byte index 7 is not a char boundary; it is inside 'é' of `@toto é secret` at src/main.rs:12",
        );
        assert!(!masque.contains("secret"));
        assert!(masque.contains("src/main.rs:12"));
        assert!(masque.contains("[14 caractères masqués]"));
    }

    #[test]
    fn les_plantages_s_empilent_dans_le_rapport_sans_l_enfler() {
        let dossier =
            std::env::temp_dir().join(format!("ki-secours-consigne-{}", std::process::id()));
        std::fs::create_dir_all(&dossier).expect("dossier de test");
        let chemin = dossier.join("ki-chat.crash");
        let _ = std::fs::remove_file(&chemin);

        consigner_dans(&chemin, "", "premier");
        consigner_dans(&chemin, "", "second");
        let texte = std::fs::read_to_string(&chemin).expect("lecture");
        assert!(texte.contains("premier") && texte.contains("second"));
        assert_eq!(texte.matches("==== CRASH ").count(), 2);

        // Un rapport au-delà de la borne est ramené à sa fin avant l'ajout.
        std::fs::write(&chemin, vec![b'x'; RAPPORT_MAX as usize + 1]).expect("écriture");
        consigner_dans(&chemin, "", "après la borne");
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
