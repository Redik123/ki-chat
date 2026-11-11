//! Sauvegarde des données du serveur : un instantané chaque nuit, gardé en
//! rotation, vérifiable et restaurable — et un numéro de schéma pour ne
//! jamais relire les données d'une version plus récente.
//!
//! Il n'y avait rien : une commande `tar` dans la documentation, à lancer à
//! la main. Perdre le volume, c'était perdre les comptes, l'historique, les
//! comptes Riot liés et l'identité du serveur (le certificat que les clients
//! épinglent) d'un seul coup.
//!
//! # Ce qu'un instantané contient
//!
//! Ce qui ne se refait pas : les fichiers à la racine de `data/` (comptes,
//! salons, rôles, réglages, journaux des salons, journal d'audit, repères de
//! lecture, certificat et clé), le dossier `valorant/` en entier, et les
//! fichiers de `musique/` (les playlists — pas son cache audio). Les fichiers
//! partagés, les clips, les diagnostics et les outils n'y sont pas : ils sont
//! gros, bornés par leurs propres quotas, et le serveur vit sans eux.
//!
//! # Pourquoi une simple copie suffit, serveur en marche
//!
//! Les fichiers d'état sont écrits par renommage atomique ([`crate::store`]) :
//! une copie lit l'ancien contenu entier ou le nouveau, jamais un mélange.
//! Les journaux ne font que s'allonger : au pire la copie s'arrête au milieu
//! de la dernière ligne, que la relecture saute déjà.
//!
//! # Ce que ce module ne fait pas encore
//!
//! L'envoi hors de la machine. Un instantané vit sur le même disque que ce
//! qu'il protège : il sauve d'une erreur, d'un fichier abîmé, d'une mise à
//! jour ratée — pas de la perte du disque.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Datelike, NaiveDateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// La version du format des données. À monter quand un fichier change d'une
/// façon qu'une version d'avant relirait de travers : elle refusera alors de
/// démarrer dessus, au lieu de réécrire les fichiers en perdant en silence
/// les champs qu'elle ne connaît pas.
pub const SCHEMA: u32 = 1;

const DOSSIER: &str = "sauvegardes";
const MANIFESTE: &str = "MANIFESTE.json";
const ETIQUETTE: &str = "SCHEMA.json";
const AVANT_RESTAURATION: &str = "avant-restauration-";
const PARTIEL: &str = ".partiel";
const FORMAT_DATE: &str = "%Y%m%d-%H%M%S";

/// Un instantané par jour sur une semaine, puis un par semaine sur un mois.
const QUOTIDIENS: usize = 7;
const HEBDOMADAIRES: usize = 4;
/// Les instantanés pris avant une mise à jour, gardés à part.
const AVANT_MISE_A_JOUR: usize = 3;

/// Ce que `data/` dit de lui-même.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Etiquette {
    schema: u32,
    version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entree {
    /// Relatif à `data/`, séparé par des `/`.
    pub chemin: String,
    pub octets: u64,
    pub sha256: String,
}

/// Ce qu'un instantané contient, écrit dedans : de quoi le vérifier sans
/// rien d'autre que lui.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifeste {
    pub schema: u32,
    /// La version du serveur qui l'a pris.
    pub version: String,
    pub cree_le: String,
    pub motif: String,
    pub fichiers: Vec<Entree>,
    /// Les dossiers de `data/` laissés de côté, pour qu'on le sache.
    #[serde(default)]
    pub laisses: Vec<String>,
}

impl Manifeste {
    pub fn octets(&self) -> u64 {
        self.fichiers.iter().map(|f| f.octets).sum()
    }
}

fn hex(octets: &[u8]) -> String {
    octets.iter().map(|b| format!("{b:02x}")).collect()
}

/// L'empreinte et la taille d'un fichier, lu par morceaux : un journal de
/// salon n'a pas à tenir en mémoire.
fn empreinte(chemin: &Path) -> std::io::Result<(u64, String)> {
    let mut f = std::fs::File::open(chemin)?;
    let mut h = Sha256::new();
    let mut tampon = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut tampon)?;
        if n == 0 {
            break;
        }
        h.update(&tampon[..n]);
        total += n as u64;
    }
    Ok((total, hex(&h.finalize())))
}

fn est_temporaire(nom: &str) -> bool {
    nom.ends_with(".tmp")
}

/// Tous les fichiers sous `dossier`, relatifs à `racine`.
fn fichiers_sous(racine: &Path, dossier: &Path, recursif: bool, vers: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for e in std::fs::read_dir(dossier)? {
        let e = e?;
        let nom = e.file_name().to_string_lossy().into_owned();
        let genre = e.file_type()?;
        if genre.is_file() {
            if !est_temporaire(&nom) {
                if let Ok(relatif) = e.path().strip_prefix(racine) {
                    vers.push(relatif.to_path_buf());
                }
            }
        } else if genre.is_dir() && recursif {
            fichiers_sous(racine, &e.path(), true, vers)?;
        }
    }
    Ok(())
}

/// Ce qu'un instantané emporte (chemins relatifs à `data/`, triés), et les
/// dossiers laissés de côté.
fn portee(data: &Path) -> std::io::Result<(Vec<PathBuf>, Vec<String>)> {
    let mut fichiers = Vec::new();
    let mut laisses = Vec::new();
    for e in std::fs::read_dir(data)? {
        let e = e?;
        let nom = e.file_name().to_string_lossy().into_owned();
        let genre = e.file_type()?;
        if genre.is_file() {
            if !est_temporaire(&nom) {
                fichiers.push(PathBuf::from(&nom));
            }
        } else if genre.is_dir() {
            match nom.as_str() {
                // Les comptes Riot liés, les fiches, les parties : tout.
                "valorant" => fichiers_sous(data, &e.path(), true, &mut fichiers)?,
                // Les playlists et la file — pas le cache audio, qui se refait.
                "musique" => fichiers_sous(data, &e.path(), false, &mut fichiers)?,
                // Les nôtres : ni sauvegardés, ni à signaler.
                DOSSIER => {}
                n if n.starts_with(AVANT_RESTAURATION) => {}
                _ => laisses.push(nom),
            }
        }
    }
    fichiers.sort();
    laisses.sort();
    Ok((fichiers, laisses))
}

fn en_barres(relatif: &Path) -> String {
    relatif.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

/// Un chemin de manifeste ne sort jamais du dossier : ni absolu, ni `..`.
/// Un manifeste trafiqué ne doit pas faire écrire ailleurs à la restauration.
fn chemin_sur(chemin: &str) -> Option<PathBuf> {
    if chemin.is_empty() || chemin.contains('\\') || chemin.contains(':') {
        return None;
    }
    let mut p = PathBuf::new();
    for partie in chemin.split('/') {
        if partie.is_empty() || partie == "." || partie == ".." {
            return None;
        }
        p.push(partie);
    }
    Some(p)
}

fn motif_propre(motif: &str) -> String {
    let m: String = motif
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c.to_ascii_lowercase() } else { '-' })
        .take(40)
        .collect();
    if m.is_empty() {
        "manuel".into()
    } else {
        m
    }
}

fn creer_prive(dossier: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dossier)?;
    // Mots de passe hachés et clé privée : lisible de son seul propriétaire.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dossier, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// Prend un instantané de `data/` dans `data/sauvegardes/` et rend son
/// dossier. Il n'apparaît sous son nom qu'une fois complet : une copie
/// interrompue reste en `.partiel`, que la rotation emporte.
pub fn instantane(data: &Path, motif: &str, quand: DateTime<Utc>) -> std::io::Result<PathBuf> {
    let (fichiers, laisses) = portee(data)?;
    let racine = data.join(DOSSIER);
    creer_prive(&racine)?;
    let base = format!("{}-{}", quand.format(FORMAT_DATE), motif_propre(motif));
    // Deux instantanés dans la même seconde ne se marchent pas dessus.
    let mut nom = base.clone();
    let mut n = 2;
    while racine.join(&nom).exists() || racine.join(format!("{nom}{PARTIEL}")).exists() {
        nom = format!("{base}-{n}");
        n += 1;
    }
    let chantier = racine.join(format!("{nom}{PARTIEL}"));
    creer_prive(&chantier)?;
    let resultat = (|| {
        let mut entrees = Vec::with_capacity(fichiers.len());
        for relatif in &fichiers {
            let source = data.join(relatif);
            let cible = chantier.join(relatif);
            if let Some(parent) = cible.parent() {
                std::fs::create_dir_all(parent)?;
            }
            match std::fs::copy(&source, &cible) {
                Ok(_) => {}
                // Supprimé entre la liste et la copie (un salon retiré) : il
                // n'existe plus, il n'a pas à être sauvegardé.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            }
            // L'empreinte est celle de la **copie** : un journal a pu
            // s'allonger pendant qu'on le lisait, et c'est la copie qu'on
            // vérifiera.
            let (octets, sha256) = empreinte(&cible)?;
            entrees.push(Entree { chemin: en_barres(relatif), octets, sha256 });
        }
        let manifeste = Manifeste {
            schema: SCHEMA,
            version: env!("CARGO_PKG_VERSION").to_string(),
            cree_le: quand.to_rfc3339(),
            motif: motif.to_string(),
            fichiers: entrees,
            laisses,
        };
        let json = serde_json::to_vec_pretty(&manifeste).map_err(std::io::Error::other)?;
        crate::store::write_atomic_prive(&chantier.join(MANIFESTE), &json)?;
        Ok(())
    })();
    if let Err(e) = resultat {
        let _ = std::fs::remove_dir_all(&chantier);
        return Err(e);
    }
    let fini = racine.join(&nom);
    std::fs::rename(&chantier, &fini)?;
    Ok(fini)
}

/// Ce que la vérification d'un instantané a trouvé.
#[derive(Debug)]
pub struct Bilan {
    pub manifeste: Manifeste,
    /// Les dernières lignes de journal coupées en plein milieu par la copie
    /// (au plus une par journal) : sans gravité, la relecture les saute.
    pub lignes_coupees: usize,
}

/// Relit un instantané de bout en bout : chaque fichier est là, à sa taille,
/// avec son empreinte, et se lit — du JSON dans les `.json`, une ligne de
/// JSON par ligne dans les `.jsonl`.
pub fn verifier(instantane: &Path) -> Result<Bilan, String> {
    let brut = std::fs::read(instantane.join(MANIFESTE))
        .map_err(|e| format!("manifeste illisible dans {} : {e}", instantane.display()))?;
    let manifeste: Manifeste = serde_json::from_slice(&brut).map_err(|e| format!("manifeste invalide : {e}"))?;
    if manifeste.schema > SCHEMA {
        return Err(format!(
            "instantané au schéma {} : ce serveur ne connaît que le schéma {SCHEMA}",
            manifeste.schema
        ));
    }
    let mut lignes_coupees = 0;
    for f in &manifeste.fichiers {
        let relatif = chemin_sur(&f.chemin).ok_or_else(|| format!("chemin refusé dans le manifeste : {}", f.chemin))?;
        let chemin = instantane.join(relatif);
        let (octets, sha256) = empreinte(&chemin).map_err(|e| format!("{} : {e}", f.chemin))?;
        if octets != f.octets || sha256 != f.sha256 {
            return Err(format!("{} : le fichier ne correspond plus au manifeste", f.chemin));
        }
        if f.chemin.ends_with(".json") {
            let contenu = std::fs::read(&chemin).map_err(|e| format!("{} : {e}", f.chemin))?;
            serde_json::from_slice::<serde_json::Value>(&contenu)
                .map_err(|e| format!("{} : ce n'est pas du JSON ({e})", f.chemin))?;
        } else if f.chemin.ends_with(".jsonl") {
            let contenu = std::fs::read_to_string(&chemin).map_err(|e| format!("{} : {e}", f.chemin))?;
            let lignes: Vec<&str> = contenu.lines().filter(|l| !l.trim().is_empty()).collect();
            for (i, ligne) in lignes.iter().enumerate() {
                if serde_json::from_str::<serde_json::Value>(ligne).is_ok() {
                    continue;
                }
                // La dernière ligne, sans fin de ligne : la copie est passée
                // pendant qu'elle s'écrivait.
                if i + 1 == lignes.len() && !contenu.ends_with('\n') {
                    lignes_coupees += 1;
                } else {
                    return Err(format!("{} : ligne {} illisible", f.chemin, i + 1));
                }
            }
        }
    }
    Ok(Bilan { manifeste, lignes_coupees })
}

/// Remet `data/` dans l'état d'un instantané. **Le serveur doit être
/// arrêté.** Rien n'est supprimé : ce que l'instantané remplace est d'abord
/// mis de côté dans `data/avant-restauration-<date>/`, dont on rend le
/// chemin. Les fichiers partagés, clips et diagnostics ne sont pas touchés.
pub fn restaurer(instantane: &Path, data: &Path, quand: DateTime<Utc>) -> Result<PathBuf, String> {
    // On ne restaure que ce qu'on a su relire en entier.
    let bilan = verifier(instantane)?;
    let de_cote = data.join(format!("{AVANT_RESTAURATION}{}", quand.format(FORMAT_DATE)));
    creer_prive(&de_cote).map_err(|e| format!("{} : {e}", de_cote.display()))?;
    // Tout ce qu'un instantané aurait emporté s'en va, pas seulement ce que
    // celui-ci contient : le journal d'un salon créé depuis, laissé là,
    // serait repris par le prochain salon qui recevrait le même numéro.
    let (actuels, _) = portee(data).map_err(|e| format!("lecture de {} : {e}", data.display()))?;
    for relatif in &actuels {
        let cible = de_cote.join(relatif);
        if let Some(parent) = cible.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{} : {e}", parent.display()))?;
        }
        std::fs::rename(data.join(relatif), &cible).map_err(|e| format!("mise de côté de {} : {e}", relatif.display()))?;
    }
    for f in &bilan.manifeste.fichiers {
        let relatif = chemin_sur(&f.chemin).ok_or_else(|| format!("chemin refusé : {}", f.chemin))?;
        let cible = data.join(&relatif);
        if let Some(parent) = cible.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{} : {e}", parent.display()))?;
        }
        std::fs::copy(instantane.join(&relatif), &cible).map_err(|e| format!("restauration de {} : {e}", f.chemin))?;
    }
    Ok(de_cote)
}

/// Un instantané complet, tel que son nom le décrit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instantane {
    pub nom: String,
    pub quand: NaiveDateTime,
    /// Pris avant une mise à jour : gardé à part dans la rotation.
    pub avant: bool,
}

fn lire_nom(nom: &str) -> Option<Instantane> {
    if nom.ends_with(PARTIEL) || nom.len() < 16 || !nom.is_char_boundary(15) {
        return None;
    }
    let quand = NaiveDateTime::parse_from_str(&nom[..15], FORMAT_DATE).ok()?;
    Some(Instantane { nom: nom.to_string(), quand, avant: nom[15..].starts_with("-avant-") })
}

/// Les instantanés complets, du plus récent au plus ancien.
pub fn lister(data: &Path) -> Vec<Instantane> {
    let mut liste: Vec<Instantane> = std::fs::read_dir(data.join(DOSSIER))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| lire_nom(&e.file_name().to_string_lossy()))
        .collect();
    liste.sort_by(|a, b| b.quand.cmp(&a.quand).then_with(|| b.nom.cmp(&a.nom)));
    liste
}

/// Ceux qu'on garde : le plus récent de chacun des sept derniers jours qui
/// en ont un, puis le plus récent de chacune des quatre semaines d'avant ;
/// et, à part, les trois derniers pris avant une mise à jour. `liste` : du
/// plus récent au plus ancien.
fn a_garder(liste: &[Instantane]) -> BTreeSet<String> {
    let mut garde = BTreeSet::new();
    for i in liste.iter().filter(|i| i.avant).take(AVANT_MISE_A_JOUR) {
        garde.insert(i.nom.clone());
    }
    let mut jours = Vec::new();
    let mut semaines = Vec::new();
    for i in liste.iter().filter(|i| !i.avant) {
        let jour = i.quand.date();
        if jours.contains(&jour) {
            continue;
        }
        if jours.len() < QUOTIDIENS {
            jours.push(jour);
            garde.insert(i.nom.clone());
            continue;
        }
        let semaine = (jour.iso_week().year(), jour.iso_week().week());
        if !semaines.contains(&semaine) && semaines.len() < HEBDOMADAIRES {
            semaines.push(semaine);
            garde.insert(i.nom.clone());
        }
    }
    garde
}

/// Supprime les instantanés que la rotation ne garde pas, et les copies
/// interrompues. Rend le nombre de dossiers supprimés.
pub fn roter(data: &Path) -> usize {
    let racine = data.join(DOSSIER);
    let garde = a_garder(&lister(data));
    let mut supprimes = 0;
    for e in std::fs::read_dir(&racine).into_iter().flatten().flatten() {
        let nom = e.file_name().to_string_lossy().into_owned();
        if !e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        // Un dossier qu'on ne sait pas lire n'est pas à nous : on n'y touche
        // pas. Une copie interrompue, si — sauf celle qui serait en cours.
        let a_retirer = if nom.ends_with(PARTIEL) {
            e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > Duration::from_secs(3600))
        } else {
            lire_nom(&nom).is_some() && !garde.contains(&nom)
        };
        if a_retirer {
            match std::fs::remove_dir_all(e.path()) {
                Ok(()) => supprimes += 1,
                Err(err) => tracing::warn!("sauvegarde : {nom} n'a pas pu être retiré : {err}"),
            }
        }
    }
    supprimes
}

/// Faut-il un instantané maintenant ? La nuit (2 h à 5 h UTC, soit 3 h à
/// 7 h pour le groupe) dès que le dernier a vingt heures ; à toute heure
/// s'il en a trente — le serveur était arrêté cette nuit-là ; tout de suite
/// s'il n'y en a aucun.
fn est_l_heure(age: Option<Duration>, heure_utc: u32) -> bool {
    match age {
        None => true,
        Some(a) if a >= Duration::from_secs(30 * 3600) => true,
        Some(a) => a >= Duration::from_secs(20 * 3600) && (2..=5).contains(&heure_utc),
    }
}

fn age_du_dernier(data: &Path, maintenant: DateTime<Utc>) -> Option<Duration> {
    let dernier = lister(data).into_iter().next()?;
    Some((maintenant.naive_utc() - dernier.quand).to_std().unwrap_or_default())
}

/// Une passe de la veille : l'instantané s'il est dû, puis la rotation.
fn passe(data: &Path, maintenant: DateTime<Utc>) {
    if !est_l_heure(age_du_dernier(data, maintenant), maintenant.hour()) {
        return;
    }
    match instantane(data, "nuit", maintenant) {
        Ok(dossier) => {
            let nom = dossier.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            match verifier(&dossier) {
                Ok(b) => tracing::info!(
                    "sauvegarde : {nom} — {} fichiers, {} Kio, relue sans défaut",
                    b.manifeste.fichiers.len(),
                    b.manifeste.octets() / 1024
                ),
                Err(e) => tracing::error!("sauvegarde : {nom} ne se relit pas : {e}"),
            }
            let retires = roter(data);
            if retires > 0 {
                tracing::info!("sauvegarde : {retires} ancien(s) instantané(s) retiré(s)");
            }
        }
        Err(e) => tracing::error!("sauvegarde : l'instantané a échoué : {e}"),
    }
}

/// Lance la veille : un instantané par nuit, tant que le serveur tourne.
/// Copie de fichiers : sur le pool bloquant, jamais sur la boucle qui relaie
/// la voix.
pub fn veiller(data: PathBuf) {
    tokio::spawn(async move {
        // Le démarrage d'abord : les magasins se chargent, les clients
        // reviennent.
        tokio::time::sleep(Duration::from_secs(120)).await;
        loop {
            let d = data.clone();
            if let Err(e) = tokio::task::spawn_blocking(move || passe(&d, Utc::now())).await {
                tracing::error!("sauvegarde : passe interrompue : {e}");
            }
            tokio::time::sleep(Duration::from_secs(1800)).await;
        }
    });
}

/// Les sauvegardes automatiques sont-elles voulues ? Oui, sauf
/// `KI_SAUVEGARDES=0`.
pub fn actives() -> bool {
    !matches!(std::env::var("KI_SAUVEGARDES").as_deref(), Ok("0" | "off" | "non"))
}

/// À faire avant de charger quoi que ce soit : refuser des données d'un
/// schéma plus récent, et prendre un instantané quand le serveur a changé de
/// version depuis le dernier démarrage — avant que la nouvelle ne réécrive
/// le moindre fichier.
pub fn au_demarrage(data: &Path, version: &str, sauvegarder: bool, forcer: bool) -> Result<(), String> {
    std::fs::create_dir_all(data).map_err(|e| format!("{} : {e}", data.display()))?;
    let chemin = data.join(ETIQUETTE);
    let connue: Option<Etiquette> = match std::fs::read(&chemin) {
        Ok(brut) => match serde_json::from_slice(&brut) {
            Ok(e) => Some(e),
            Err(e) => {
                tracing::warn!("{ETIQUETTE} illisible ({e}) : tenu pour absent");
                None
            }
        },
        Err(_) => None,
    };
    if let Some(e) = &connue {
        if e.schema > SCHEMA && !forcer {
            return Err(format!(
                "les données de {} sont au schéma {} (écrites par ki-server {}), ce serveur ne connaît que le \
                 schéma {SCHEMA}. Démarrer dessus réécrirait les fichiers en perdant ce qu'il ne comprend pas. \
                 Remets la version {} ou plus récente, ou restaure un instantané d'avant ; KI_SCHEMA_FORCER=1 \
                 passe outre, à tes risques.",
                data.display(),
                e.schema,
                e.version,
                e.version
            ));
        }
    }
    let a_jour = connue.as_ref().is_some_and(|e| e.schema == SCHEMA && e.version == version);
    if a_jour {
        return Ok(());
    }
    // Une installation qui a déjà vécu, et une autre version qu'au dernier
    // démarrage (ou la première à tenir cette étiquette) : on garde l'état
    // d'avant. Un échec ne bloque pas le démarrage — un disque plein ne doit
    // pas ajouter une panne à la panne —, mais il se dit.
    if sauvegarder && data.join("users.json").exists() {
        match instantane(data, &format!("avant-{version}"), Utc::now()) {
            Ok(dossier) => {
                tracing::info!("sauvegarde : état d'avant la {version} gardé dans {}", dossier.display());
                roter(data);
            }
            Err(e) => tracing::error!("sauvegarde : l'instantané d'avant la {version} a échoué : {e}"),
        }
    }
    let etiquette = Etiquette { schema: SCHEMA, version: version.to_string() };
    let json = serde_json::to_vec_pretty(&etiquette).map_err(|e| e.to_string())?;
    crate::store::write_atomic(&chemin, &json).map_err(|e| format!("{} : {e}", chemin.display()))
}

/// Désigne un instantané : son nom dans `data/sauvegardes/`, un chemin, ou
/// rien — le plus récent.
fn designer(data: &Path, argument: Option<&String>) -> Result<PathBuf, String> {
    match argument {
        Some(a) => {
            let dans = data.join(DOSSIER).join(a);
            if dans.join(MANIFESTE).is_file() {
                return Ok(dans);
            }
            let direct = PathBuf::from(a);
            if direct.join(MANIFESTE).is_file() {
                return Ok(direct);
            }
            Err(format!("aucun instantané « {a} » (ni dans {}, ni à ce chemin)", data.join(DOSSIER).display()))
        }
        None => lister(data)
            .into_iter()
            .next()
            .map(|i| data.join(DOSSIER).join(i.nom))
            .ok_or_else(|| format!("aucun instantané dans {}", data.join(DOSSIER).display())),
    }
}

/// Les sous-commandes : `ki-server sauvegarder`, `sauvegardes`,
/// `verifier [instantané]`, `restaurer <instantané>`. Rend le code de sortie
/// si `args` en était une, `None` sinon — le serveur démarre alors comme
/// d'habitude.
pub fn commande(args: &[String], data: &Path) -> Option<i32> {
    let resultat = match args.first().map(String::as_str)? {
        "sauvegarder" => instantane(data, "manuel", Utc::now()).map_err(|e| e.to_string()).and_then(|dossier| {
            let bilan = verifier(&dossier)?;
            println!(
                "Instantané pris et relu : {} ({} fichiers, {} Kio).",
                dossier.display(),
                bilan.manifeste.fichiers.len(),
                bilan.manifeste.octets() / 1024
            );
            if !bilan.manifeste.laisses.is_empty() {
                println!("Laissés de côté : {}.", bilan.manifeste.laisses.join(", "));
            }
            Ok(())
        }),
        "sauvegardes" => {
            let liste = lister(data);
            if liste.is_empty() {
                println!("Aucun instantané dans {}.", data.join(DOSSIER).display());
            }
            for i in liste {
                println!("{}", i.nom);
            }
            Ok(())
        }
        "verifier" => designer(data, args.get(1)).and_then(|dossier| {
            let bilan = verifier(&dossier)?;
            println!(
                "{} : {} fichiers, {} Kio, pris par ki-server {} le {} — relu sans défaut.",
                dossier.display(),
                bilan.manifeste.fichiers.len(),
                bilan.manifeste.octets() / 1024,
                bilan.manifeste.version,
                bilan.manifeste.cree_le
            );
            if bilan.lignes_coupees > 0 {
                println!(
                    "{} journal(aux) copié(s) en pleine écriture : la dernière ligne sera sautée à la relecture.",
                    bilan.lignes_coupees
                );
            }
            Ok(())
        }),
        "restaurer" => match args.get(1) {
            None => Err("restaurer quoi ? Donne le nom d'un instantané (ki-server sauvegardes les liste).".into()),
            Some(_) => designer(data, args.get(1)).and_then(|dossier| {
                let de_cote = restaurer(&dossier, data, Utc::now())?;
                println!("Restauré depuis {}.", dossier.display());
                println!("L'état d'avant est gardé dans {} : à supprimer quand tout va bien.", de_cote.display());
                Ok(())
            }),
        },
        _ => return None,
    };
    Some(match resultat {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("Échec : {e}");
            1
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// Un dossier `data/` d'essai, propre à ce test.
    fn banc(nom: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ki-sauvegarde-{nom}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn poser(data: &Path, relatif: &str, contenu: &str) {
        let chemin = data.join(relatif);
        std::fs::create_dir_all(chemin.parent().unwrap()).unwrap();
        std::fs::write(chemin, contenu).unwrap();
    }

    fn lire(data: &Path, relatif: &str) -> Option<String> {
        std::fs::read_to_string(data.join(relatif)).ok()
    }

    /// Un `data/` qui ressemble au vrai.
    fn peupler(data: &Path) {
        poser(data, "users.json", r#"{"users":[{"id":1,"name":"max"}]}"#);
        poser(data, "channels.json", r#"{"next_id":4,"channels":[1,2,3]}"#);
        poser(data, "channel-1.jsonl", "{\"t\":\"salut\"}\n{\"t\":\"ça va\"}\n");
        poser(data, "quic-key.der", "clé");
        poser(data, "valorant/comptes.json", r#"{"1":{"puuid":"abc"}}"#);
        poser(data, "valorant/parties/1.jsonl", "{\"id\":\"m1\"}\n");
        poser(data, "musique/playlists.json", "{}");
        // Ce qu'un instantané laisse : gros, borné ailleurs, ou refaisable.
        poser(data, "musique/cache/piste.opus", "du son");
        poser(data, "files/abc/photo.png", "une image");
        poser(data, "clips/x/clip.mp4", "une vidéo");
        poser(data, "users.json.123.4.tmp", "une écriture en cours");
    }

    fn le(jour: u32, heure: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, jour, heure, 0, 0).unwrap()
    }

    #[test]
    fn un_instantane_emporte_ce_qui_ne_se_refait_pas_et_se_relit() {
        let data = banc("contenu");
        peupler(&data);
        let dossier = instantane(&data, "nuit", le(12, 3)).unwrap();
        assert_eq!(dossier.file_name().unwrap().to_string_lossy(), "20261112-030000-nuit");
        let bilan = verifier(&dossier).unwrap();
        let chemins: Vec<&str> = bilan.manifeste.fichiers.iter().map(|f| f.chemin.as_str()).collect();
        assert_eq!(
            chemins,
            [
                "channel-1.jsonl",
                "channels.json",
                "musique/playlists.json",
                "quic-key.der",
                "users.json",
                "valorant/comptes.json",
                "valorant/parties/1.jsonl",
            ]
        );
        // Les gros dossiers sont nommés, pas copiés ; le cache et le
        // temporaire ne sont ni l'un ni l'autre.
        assert_eq!(bilan.manifeste.laisses, ["clips", "files"]);
        assert!(!dossier.join("musique/cache").exists());
        assert_eq!(bilan.lignes_coupees, 0);
        // Un second instantané ne s'emporte pas lui-même.
        let second = instantane(&data, "nuit", le(13, 3)).unwrap();
        assert!(!second.join(DOSSIER).exists());
        assert_eq!(verifier(&second).unwrap().manifeste.fichiers.len(), 7);
        std::fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn la_verification_voit_un_fichier_abime_manquant_ou_illisible() {
        let data = banc("abime");
        peupler(&data);
        let dossier = instantane(&data, "nuit", le(12, 3)).unwrap();
        // Un octet changé.
        std::fs::write(dossier.join("users.json"), r#"{"users":[{"id":1,"name":"mox"}]}"#).unwrap();
        assert!(verifier(&dossier).unwrap_err().contains("users.json"));
        // Un fichier disparu.
        let dossier = instantane(&data, "nuit", le(13, 3)).unwrap();
        std::fs::remove_file(dossier.join("valorant/comptes.json")).unwrap();
        assert!(verifier(&dossier).unwrap_err().contains("valorant/comptes.json"));
        // Un état déjà abîmé au moment de la copie : fidèle, mais illisible —
        // la vérification le dit, au lieu de laisser croire à une sauvegarde.
        poser(&data, "roles.json", "{ pas du json");
        let dossier = instantane(&data, "nuit", le(14, 3)).unwrap();
        assert!(verifier(&dossier).unwrap_err().contains("roles.json"));
        std::fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn un_journal_copie_en_pleine_ecriture_reste_bon() {
        let data = banc("coupe");
        peupler(&data);
        // La copie est passée au milieu de la dernière ligne.
        poser(&data, "channel-2.jsonl", "{\"t\":\"entier\"}\n{\"t\":\"coup");
        let dossier = instantane(&data, "nuit", le(12, 3)).unwrap();
        assert_eq!(verifier(&dossier).unwrap().lignes_coupees, 1);
        // Une ligne abîmée au milieu, elle, n'est pas une copie en cours.
        poser(&data, "channel-2.jsonl", "{\"t\":\"entier\"}\n{\"t\":\"coup\n{\"t\":\"suite\"}\n");
        let dossier = instantane(&data, "nuit", le(13, 3)).unwrap();
        assert!(verifier(&dossier).unwrap_err().contains("ligne 2"));
        std::fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn restaurer_remet_l_etat_et_ne_supprime_rien() {
        let data = banc("restaure");
        peupler(&data);
        let dossier = instantane(&data, "nuit", le(12, 3)).unwrap();
        // Depuis : un compte modifié, un salon créé, un compte Riot délié,
        // et une photo partagée.
        poser(&data, "users.json", r#"{"users":[]}"#);
        poser(&data, "channel-9.jsonl", "{\"t\":\"écrit après\"}\n");
        std::fs::remove_file(data.join("valorant/comptes.json")).unwrap();
        poser(&data, "files/def/autre.png", "une autre image");

        let de_cote = restaurer(&dossier, &data, le(14, 10)).unwrap();
        // L'état de l'instantané est revenu…
        assert_eq!(lire(&data, "users.json").unwrap(), r#"{"users":[{"id":1,"name":"max"}]}"#);
        assert_eq!(lire(&data, "valorant/comptes.json").unwrap(), r#"{"1":{"puuid":"abc"}}"#);
        assert_eq!(lire(&data, "valorant/parties/1.jsonl").unwrap(), "{\"id\":\"m1\"}\n");
        // … le journal du salon créé depuis n'y est plus (le prochain salon 9
        // ne doit pas en hériter), mais il n'est pas perdu.
        assert!(lire(&data, "channel-9.jsonl").is_none());
        assert_eq!(lire(&de_cote, "channel-9.jsonl").unwrap(), "{\"t\":\"écrit après\"}\n");
        assert_eq!(lire(&de_cote, "users.json").unwrap(), r#"{"users":[]}"#);
        // Ce qu'un instantané ne porte pas n'a pas bougé.
        assert_eq!(lire(&data, "files/def/autre.png").unwrap(), "une autre image");
        assert_eq!(lire(&data, "musique/cache/piste.opus").unwrap(), "du son");
        // Et l'état mis de côté ne repart pas dans l'instantané suivant.
        let suivant = instantane(&data, "nuit", le(15, 3)).unwrap();
        assert!(verifier(&suivant).unwrap().manifeste.fichiers.iter().all(|f| !f.chemin.contains("avant-")));
        std::fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn un_instantane_abime_ne_se_restaure_pas() {
        let data = banc("refus");
        peupler(&data);
        let dossier = instantane(&data, "nuit", le(12, 3)).unwrap();
        std::fs::write(dossier.join("channels.json"), "autre chose").unwrap();
        poser(&data, "users.json", r#"{"users":["état courant"]}"#);
        assert!(restaurer(&dossier, &data, le(14, 10)).is_err());
        // Rien n'a bougé : on a refusé avant de toucher à quoi que ce soit.
        assert_eq!(lire(&data, "users.json").unwrap(), r#"{"users":["état courant"]}"#);
        std::fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn un_manifeste_ne_fait_pas_ecrire_hors_du_dossier() {
        assert_eq!(chemin_sur("valorant/parties/1.jsonl"), Some(PathBuf::from("valorant").join("parties").join("1.jsonl")));
        for mauvais in ["../users.json", "/etc/passwd", "a/../../b", "C:/Windows/x", "a\\b", "", "a//b", "./a"] {
            assert_eq!(chemin_sur(mauvais), None, "{mauvais}");
        }
    }

    fn i(nom: &str) -> Instantane {
        lire_nom(nom).unwrap()
    }

    #[test]
    fn la_rotation_garde_sept_jours_quatre_semaines_et_trois_mises_a_jour() {
        // Du plus récent au plus ancien : un par nuit du 30 au 1er novembre,
        // deux le 30, puis octobre.
        let mut noms: Vec<String> = vec!["20261130-150000-manuel".into()];
        noms.extend((1..=30).rev().map(|j| format!("202611{j:02}-030000-nuit")));
        noms.extend((1..=31).rev().map(|j| format!("202610{j:02}-030000-nuit")));
        let liste: Vec<Instantane> = noms.iter().map(|n| i(n)).collect();
        let garde = a_garder(&liste);
        let attendus = [
            // Sept jours : le plus récent de chacun (le manuel du 30, pas la nuit).
            "20261130-150000-manuel",
            "20261129-030000-nuit",
            "20261128-030000-nuit",
            "20261127-030000-nuit",
            "20261126-030000-nuit",
            "20261125-030000-nuit",
            "20261124-030000-nuit",
            // Puis le plus récent de chacune des quatre semaines d'avant
            // (lundi 23 clôt la semaine 48 entamée ; dimanches 22, 15, 8).
            "20261123-030000-nuit",
            "20261122-030000-nuit",
            "20261115-030000-nuit",
            "20261108-030000-nuit",
        ];
        assert_eq!(garde, attendus.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>());

        // Les instantanés d'avant mise à jour vivent à part : les trois
        // derniers, sans prendre la place d'une nuit.
        let avec: Vec<Instantane> = [
            "20261130-030000-nuit",
            "20261129-120000-avant-0.1.73",
            "20261120-120000-avant-0.1.72",
            "20261110-120000-avant-0.1.71",
            "20261101-120000-avant-0.1.70",
        ]
        .iter()
        .map(|n| i(n))
        .collect();
        let garde = a_garder(&avec);
        assert!(garde.contains("20261130-030000-nuit"));
        assert!(garde.contains("20261110-120000-avant-0.1.71"));
        assert!(!garde.contains("20261101-120000-avant-0.1.70"));
    }

    #[test]
    fn roter_ne_touche_qu_a_ses_instantanes() {
        let data = banc("rotation");
        peupler(&data);
        for jour in 1..=12 {
            instantane(&data, "nuit", le(jour, 3)).unwrap();
        }
        // Un dossier posé là par quelqu'un : pas à nous.
        poser(&data, "sauvegardes/a-garder-a-la-main/note.txt", "ne pas toucher");
        let retires = roter(&data);
        let restants: Vec<String> = lister(&data).into_iter().map(|i| i.nom).collect();
        // Douze nuits : les sept dernières, et une par semaine d'avant
        // (dimanche 1er, puis jeudi 5 pour la semaine du 2 au 8).
        assert_eq!(restants.len(), 9, "{restants:?}");
        assert_eq!(retires, 3);
        assert_eq!(restants[0], "20261112-030000-nuit");
        assert!(data.join("sauvegardes/a-garder-a-la-main/note.txt").exists());
        std::fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn l_instantane_se_prend_la_nuit_ou_des_qu_il_manque() {
        let h = |n: u64| Some(Duration::from_secs(n * 3600));
        // Aucun : tout de suite, quelle que soit l'heure.
        assert!(est_l_heure(None, 20));
        // Celui de la nuit dernière : pas avant la nuit suivante.
        assert!(!est_l_heure(h(10), 13));
        assert!(!est_l_heure(h(19), 3));
        assert!(est_l_heure(h(23), 3));
        // Vingt-deux heures après, mais en plein après-midi : on attend.
        assert!(!est_l_heure(h(22), 14));
        // Le serveur était arrêté cette nuit : on n'attend pas la suivante.
        assert!(est_l_heure(h(31), 14));
    }

    #[test]
    fn un_schema_plus_recent_empeche_de_demarrer() {
        let data = banc("schema");
        // Dossier neuf : rien à sauvegarder, l'étiquette se pose.
        au_demarrage(&data, "0.1.70", true, false).unwrap();
        assert!(lister(&data).is_empty());
        assert!(lire(&data, ETIQUETTE).unwrap().contains("0.1.70"));
        // Écrit par une version d'après, au schéma suivant.
        let futur = Etiquette { schema: SCHEMA + 1, version: "0.9.0".into() };
        std::fs::write(data.join(ETIQUETTE), serde_json::to_vec(&futur).unwrap()).unwrap();
        let refus = au_demarrage(&data, "0.1.70", true, false).unwrap_err();
        assert!(refus.contains("0.9.0") && refus.contains("KI_SCHEMA_FORCER"), "{refus}");
        // Le refus n'a rien réécrit.
        assert!(lire(&data, ETIQUETTE).unwrap().contains("0.9.0"));
        // Forcé : ça démarre, et l'étiquette redevient la nôtre.
        au_demarrage(&data, "0.1.70", true, true).unwrap();
        assert!(lire(&data, ETIQUETTE).unwrap().contains("0.1.70"));
        std::fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn un_changement_de_version_garde_l_etat_d_avant() {
        let data = banc("version");
        peupler(&data);
        // Première fois que ce serveur tient une étiquette, sur une
        // installation qui a déjà vécu : l'état d'avant est gardé.
        au_demarrage(&data, "0.1.70", true, false).unwrap();
        let liste = lister(&data);
        assert_eq!(liste.len(), 1);
        assert!(liste[0].avant && liste[0].nom.ends_with("-avant-0.1.70"), "{liste:?}");
        // Redémarrage à la même version : rien de plus.
        au_demarrage(&data, "0.1.70", true, false).unwrap();
        assert_eq!(lister(&data).len(), 1);
        // Mise à jour : un de plus.
        au_demarrage(&data, "0.1.71", true, false).unwrap();
        assert_eq!(lister(&data).len(), 2);
        // Sauvegardes coupées : l'étiquette suit, sans instantané.
        au_demarrage(&data, "0.1.72", false, false).unwrap();
        assert_eq!(lister(&data).len(), 2);
        assert!(lire(&data, ETIQUETTE).unwrap().contains("0.1.72"));
        std::fs::remove_dir_all(&data).unwrap();
    }
}
