//! Mise à jour automatique depuis les releases GitHub.
//!
//! Au démarrage, l'application demande à GitHub la dernière release publiée
//! et compare son étiquette à sa propre version. Si une version plus récente
//! existe, elle la propose — et ne touche à rien tant que l'utilisateur n'a
//! pas accepté. Un refus vaut pour cette version : on ne redemande qu'à la
//! suivante, pour que « non » veuille dire non plutôt que « pas cette fois ».
//!
//! Le remplacement du binaire en cours d'exécution repose sur une propriété
//! de Windows : on ne peut pas *supprimer* un exécutable chargé, mais on peut
//! le *renommer*. L'ancien est donc écarté sous un autre nom, le nouveau
//! prend sa place, et le résidu est balayé au démarrage suivant — moment où
//! il n'est plus chargé, donc effaçable.
//!
//! Tout se joue à côté de l'exécutable, sans élévation : l'installeur pose
//! l'application dans le profil de l'utilisateur (`%LOCALAPPDATA%`), pas dans
//! `Program Files`, précisément pour qu'elle puisse se remplacer toute seule.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;

/// Dépôt qui publie les releases.
const REPO: &str = "Redik123/ki-chat";
/// Nom de l'exécutable attaché à chaque release.
const ASSET: &str = "ki-chat.exe";
/// Signature détachée de l'exécutable, publiée à côté de lui.
const SIGNATURE_ASSET: &str = "ki-chat.exe.sig";

/// Clé publique Ed25519 des releases, en hexadécimal (32 octets, 64
/// caractères). Vide = vérification pas encore activée.
///
/// # Pourquoi une signature
///
/// L'application **remplace son propre exécutable**. Jusqu'ici, la seule
/// garantie d'intégrité était TLS jusqu'à GitHub : quiconque obtenait le droit
/// de publier une release — compte compromis, jeton d'action fuité, actif
/// remplacé après coup — exécutait du code arbitraire sur les machines de tout
/// le monde, sans que rien ne s'y oppose. Le contrôle de taille qui existait
/// n'attrape qu'un téléchargement tronqué, pas un binaire hostile.
///
/// La clé **privée** ne vit ni dans le dépôt ni dans l'intégration continue
/// autrement que comme secret ; la publique est gravée ici, dans le binaire
/// déjà installé. Un attaquant qui contrôle les releases ne peut donc pas
/// signer : il ne peut que faire échouer la mise à jour, ce qui se voit.
///
/// # Activation
///
/// Tant que cette constante est vide, la vérification est **annoncée comme
/// absente** dans les traces et la mise à jour se poursuit — c'est l'état
/// d'avant, ni meilleur ni pire. Y coller la clé publique suffit à la rendre
/// obligatoire, et il n'y a rien d'autre à changer. Voir `deploy/SIGNATURE.md`.
const RELEASE_PUBKEY_HEX: &str = "";
/// GitHub refuse les requêtes sans agent identifié.
const AGENT: &str = concat!("ki-chat/", env!("CARGO_PKG_VERSION"));
/// La vérification ne doit pas retarder le démarrage : au-delà, on abandonne
/// silencieusement — une mise à jour ratée n'est pas une panne.
const TIMEOUT: Duration = Duration::from_secs(10);
/// Un exécutable de plus de 200 Mo n'est pas le nôtre.
const MAX_BYTES: u64 = 200 * 1024 * 1024;
/// Inactivité tolérée pendant le téléchargement du binaire.
///
/// Distinct de `TIMEOUT` : trente mégaoctets ne passent pas en dix secondes
/// sur une ligne ordinaire. Ce délai-ci borne le **silence**, pas la durée
/// totale — une connexion lente aboutit, une connexion morte non.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);

/// Version de l'application en cours d'exécution.
pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Une release plus récente, telle que GitHub la décrit.
#[derive(Clone)]
pub struct Release {
    /// Étiquette débarrassée de son `v` : `0.2.0`.
    pub version: String,
    /// Notes de version, telles qu'écrites dans la release.
    pub notes: String,
    /// Téléchargement direct de l'exécutable.
    url: String,
    /// Téléchargement de la signature détachée. `None` = la release n'en
    /// publie pas.
    signature_url: Option<String>,
    /// Taille annoncée, pour la barre de progression.
    size: u64,
}

/// Où en est la mise à jour, du point de vue de l'interface.
#[derive(Clone)]
pub enum Status {
    /// Rien à afficher : vérification en cours, déjà à jour, ou refusée.
    Idle,
    /// Une version plus récente attend l'accord de l'utilisateur.
    Available(Release),
    /// Téléchargement en cours.
    Downloading { done: u64, total: u64 },
    /// Binaire remplacé : il ne reste qu'à redémarrer.
    Ready,
    /// Échec, avec de quoi comprendre.
    Failed(String),
}

/// Redémarrage demandé. Un booléen global plutôt qu'un canal : `main` le lit
/// une seule fois, après la fermeture propre de la fenêtre.
static RESTART: AtomicBool = AtomicBool::new(false);

/// Sonde GitHub au démarrage, puis pilote le téléchargement si on l'accepte.
pub struct Updater {
    state: Arc<Mutex<Status>>,
    /// Version que l'utilisateur a refusée, mémorisée d'une session à l'autre.
    skipped: Option<String>,
}

impl Updater {
    /// Lance la vérification en tâche de fond. Ne bloque jamais le démarrage :
    /// l'application s'ouvre pendant que la requête part.
    pub fn start(skipped: Option<String>, ctx: egui::Context) -> Self {
        sweep();

        let state = Arc::new(Mutex::new(Status::Idle));
        let slot = state.clone();
        let refused = skipped.clone();
        std::thread::spawn(move || {
            match fetch_latest() {
                Ok(Some(release)) if refused.as_deref() == Some(release.version.as_str()) => {
                    tracing::info!(version = %release.version, "mise à jour déjà refusée");
                }
                Ok(Some(release)) => {
                    tracing::info!(version = %release.version, "mise à jour disponible");
                    *slot.lock().unwrap() = Status::Available(release);
                }
                Ok(None) => tracing::debug!("déjà à jour"),
                // Pas de release publiée, pas de réseau, quota GitHub
                // atteint : rien de tout ça ne justifie d'alerter.
                Err(e) => tracing::info!("vérification des mises à jour : {e:#}"),
            }
            ctx.request_repaint();
        });

        Self { state, skipped }
    }

    /// État courant, pour l'affichage.
    pub fn status(&self) -> Status {
        self.state.lock().unwrap().clone()
    }

    /// Accepte la mise à jour : télécharge puis remplace le binaire.
    pub fn accept(&self, release: &Release, ctx: egui::Context) {
        let state = self.state.clone();
        let release = release.clone();
        *state.lock().unwrap() = Status::Downloading { done: 0, total: release.size };
        std::thread::spawn(move || {
            let outcome = download(&release, &state).and_then(|staged| {
                // Vérifier AVANT d'installer, et effacer le fichier quoi
                // qu'il arrive : un binaire non signé ne doit pas rester à
                // traîner à côté de l'exécutable sous un nom presque
                // identique.
                let verdict = verify(&staged, &release).and_then(|()| install(&staged));
                let _ = std::fs::remove_file(&staged);
                verdict
            });
            *state.lock().unwrap() = match outcome {
                Ok(()) => Status::Ready,
                Err(e) => {
                    tracing::warn!("mise à jour : {e:#}");
                    Status::Failed(format!("{e:#}"))
                }
            };
            ctx.request_repaint();
        });
    }

    /// Refuse cette version : on n'en reparle plus.
    pub fn skip(&mut self, version: &str) {
        self.skipped = Some(version.to_string());
        *self.state.lock().unwrap() = Status::Idle;
    }

    /// Ferme le message d'échec sans rien refuser.
    pub fn dismiss(&self) {
        *self.state.lock().unwrap() = Status::Idle;
    }

    /// Version refusée, à persister entre deux sessions.
    pub fn skipped(&self) -> &str {
        self.skipped.as_deref().unwrap_or("")
    }

    /// Page des releases, pour l'utilisateur qui préfère faire à la main.
    pub fn releases_page() -> String {
        format!("https://github.com/{REPO}/releases/latest")
    }
}

/// Demande le redémarrage. La fenêtre se referme d'abord — eframe enregistre
/// alors les réglages et le moteur audio rend les périphériques — et `main`
/// relance le binaire une fois la boucle sortie.
pub fn request_restart() {
    RESTART.store(true, Ordering::SeqCst);
}

/// Relance l'exécutable si une mise à jour vient d'être installée. Appelé par
/// `main` après la fermeture de la fenêtre.
pub fn relaunch_if_requested() {
    if !RESTART.load(Ordering::SeqCst) {
        return;
    }
    match std::env::current_exe().map(std::process::Command::new) {
        Ok(mut cmd) => {
            if let Err(e) = cmd.spawn() {
                tracing::error!("redémarrage impossible : {e}");
            }
        }
        Err(e) => tracing::error!("exécutable introuvable : {e}"),
    }
}

/// Efface ce qu'une mise à jour a laissé derrière elle : le binaire écarté,
/// et un téléchargement resté en plan. Au démarrage, plus rien de tout ça
/// n'est chargé, donc tout est effaçable.
fn sweep() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let _ = std::fs::remove_file(exe.with_extension("old"));
    let _ = std::fs::remove_file(exe.with_extension("new"));
}

/// Interroge GitHub. `Ok(None)` = on est déjà à jour.
fn fetch_latest() -> anyhow::Result<Option<Release>> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body: serde_json::Value = ureq::get(&url)
        .set("User-Agent", AGENT)
        .set("Accept", "application/vnd.github+json")
        .timeout(TIMEOUT)
        .call()?
        .into_json()?;

    // `/releases/latest` écarte déjà brouillons et pré-versions.
    let tag = body["tag_name"].as_str().unwrap_or_default();
    let version = tag.trim_start_matches('v').trim().to_string();
    if version.is_empty() || !newer(&version, current()) {
        return Ok(None);
    }

    let asset = body["assets"]
        .as_array()
        .and_then(|assets| assets.iter().find(|a| a["name"].as_str() == Some(ASSET)))
        .ok_or_else(|| anyhow::anyhow!("release {tag} sans exécutable {ASSET}"))?;
    let url = asset["browser_download_url"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("release {tag} sans lien de téléchargement"))?
        .to_string();
    // GitHub sert ses téléchargements en HTTPS ; on refuse le reste, une
    // redirection en clair suffirait sinon à substituer le binaire.
    anyhow::ensure!(url.starts_with("https://"), "lien de téléchargement non chiffré");

    // La signature est un actif comme un autre, publié à côté de l'exécutable.
    let signature_url = body["assets"]
        .as_array()
        .and_then(|assets| {
            assets.iter().find(|a| a["name"].as_str() == Some(SIGNATURE_ASSET))
        })
        .and_then(|a| a["browser_download_url"].as_str())
        .filter(|u| u.starts_with("https://"))
        .map(str::to_string);

    Ok(Some(Release {
        version,
        notes: body["body"].as_str().unwrap_or_default().trim().to_string(),
        url,
        signature_url,
        size: asset["size"].as_u64().unwrap_or(0),
    }))
}

/// Télécharge le nouvel exécutable à côté de l'ancien — même volume, donc le
/// remplacement se fera par un simple renommage, sans copie ni fenêtre où le
/// fichier serait à moitié écrit.
fn download(release: &Release, state: &Arc<Mutex<Status>>) -> anyhow::Result<PathBuf> {
    let staged = std::env::current_exe()?.with_extension("new");

    // `DOWNLOAD_TIMEOUT` et non `TIMEOUT` : trente mégaoctets ne passent pas
    // en dix secondes sur une ligne ordinaire. Mais un délai il en faut un —
    // il n'y en avait aucun, et un serveur qui accepte la connexion puis cesse
    // d'envoyer laissait le fil de mise à jour attendre pour toujours.
    let response = ureq::AgentBuilder::new()
        .timeout_connect(TIMEOUT)
        .timeout_read(DOWNLOAD_TIMEOUT)
        .build()
        .get(&release.url)
        .set("User-Agent", AGENT)
        .call()?;
    let total = response
        .header("Content-Length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(release.size);
    anyhow::ensure!(total <= MAX_BYTES, "téléchargement anormalement gros ({total} octets)");

    let mut reader = response.into_reader().take(MAX_BYTES);
    let mut file = std::fs::File::create(&staged)?;
    let mut buffer = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])?;
        done += read as u64;
        *state.lock().unwrap() = Status::Downloading { done, total };
    }
    file.sync_all()?;
    drop(file);

    // Une connexion coupée en route laisse un exécutable tronqué, qui
    // remplacerait silencieusement un binaire qui marche par un qui plante.
    if total > 0 && done != total {
        let _ = std::fs::remove_file(&staged);
        anyhow::bail!("téléchargement incomplet ({done} sur {total} octets)");
    }

    Ok(staged)
}

/// Vérifie la signature Ed25519 du binaire téléchargé.
///
/// Trois cas, et c'est l'ordre qui compte.
///
/// **Aucune clé compilée** : la vérification n'est pas encore activée sur ce
/// binaire. On le dit et on continue — c'est exactement l'état d'avant, et
/// refuser ici enlèverait toute mise à jour à des gens qui n'y peuvent rien.
///
/// **Clé compilée, signature absente ou fausse** : on refuse. Un attaquant
/// qui contrôle les releases ne peut alors que faire échouer la mise à jour,
/// jamais en substituer une.
///
/// **Clé et signature valides** : on installe.
fn verify(staged: &Path, release: &Release) -> anyhow::Result<()> {
    let Some(pubkey) = release_pubkey()? else {
        tracing::warn!(
            "mise à jour non vérifiée : aucune clé de release n'est gravée dans ce \
             binaire (voir deploy/SIGNATURE.md)"
        );
        return Ok(());
    };

    let url = release
        .signature_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("la release ne publie pas de signature"))?;
    let mut signature = Vec::new();
    ureq::get(url)
        .set("User-Agent", AGENT)
        .timeout(TIMEOUT)
        .call()?
        .into_reader()
        // Une signature Ed25519 fait 64 octets ; on lit un peu plus pour
        // pouvoir distinguer « trop long » de « exactement bon ».
        .take(256)
        .read_to_end(&mut signature)?;
    let signature = parse_signature(&signature)?;

    let binaire = std::fs::read(staged)?;
    pubkey
        .verify_strict(&binaire, &signature)
        .map_err(|_| anyhow::anyhow!("signature invalide — mise à jour refusée"))?;
    tracing::info!("mise à jour {} : signature vérifiée", release.version);
    Ok(())
}

/// La clé publique gravée, décodée. `None` = vérification pas encore activée.
fn release_pubkey() -> anyhow::Result<Option<ed25519_dalek::VerifyingKey>> {
    let hex = RELEASE_PUBKEY_HEX.trim();
    if hex.is_empty() {
        return Ok(None);
    }
    let bytes = ki_protocol::hex_decode(hex)
        .ok_or_else(|| anyhow::anyhow!("clé de release illisible (hexadécimal attendu)"))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("clé de release de longueur inattendue"))?;
    Ok(Some(ed25519_dalek::VerifyingKey::from_bytes(&bytes)?))
}

/// Lit une signature, brute (64 octets) ou en hexadécimal.
///
/// L'hexadécimal est admis parce qu'un fichier de signature finit par passer
/// entre des mains humaines — copié dans un ticket, recollé à la main — et
/// qu'un format lisible évite d'y perdre des octets en chemin.
fn parse_signature(raw: &[u8]) -> anyhow::Result<ed25519_dalek::Signature> {
    if raw.len() == 64 {
        let bytes: [u8; 64] = raw.try_into().expect("longueur vérifiée");
        return Ok(ed25519_dalek::Signature::from_bytes(&bytes));
    }
    let texte = std::str::from_utf8(raw)
        .map_err(|_| anyhow::anyhow!("signature de {} octets, illisible", raw.len()))?
        .trim();
    let bytes = ki_protocol::hex_decode(texte)
        .ok_or_else(|| anyhow::anyhow!("signature ni brute ni hexadécimale"))?;
    let bytes: [u8; 64] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("signature de longueur inattendue"))?;
    Ok(ed25519_dalek::Signature::from_bytes(&bytes))
}

/// Met le binaire téléchargé à la place du binaire courant.
fn install(staged: &Path) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let old = exe.with_extension("old");
    let _ = std::fs::remove_file(&old);

    std::fs::rename(&exe, &old).map_err(|e| {
        anyhow::anyhow!("écriture impossible dans {} : {e}", parent(&exe))
    })?;
    if let Err(e) = std::fs::rename(staged, &exe) {
        // Remettre l'ancien en place : une version dépassée vaut mieux
        // qu'un dossier d'installation sans exécutable.
        let _ = std::fs::rename(&old, &exe);
        anyhow::bail!("remplacement impossible : {e}");
    }
    Ok(())
}

/// Dossier d'un chemin, pour les messages d'erreur.
fn parent(path: &Path) -> String {
    path.parent().unwrap_or(path).display().to_string()
}

/// `a` est-il strictement postérieur à `b` ?
fn newer(a: &str, b: &str) -> bool {
    parts(a) > parts(b)
}

/// Découpe une version en trois nombres. La comparaison est numérique champ
/// par champ — comparer les chaînes classerait `0.10.0` avant `0.9.0`.
fn parts(version: &str) -> [u32; 3] {
    let mut out = [0u32; 3];
    // Un suffixe de pré-version (`-rc1`) ou de build (`+abc`) ne participe
    // pas à l'ordre : il n'est là que pour les humains.
    let core = version.split(['-', '+']).next().unwrap_or_default();
    for (slot, field) in out.iter_mut().zip(core.split('.')) {
        *slot = field.trim().parse().unwrap_or(0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{newer, parse_signature};
    use ed25519_dalek::{Signer, SigningKey};

    /// Une clé de test, fixe : un test qui tire au sort n'échoue qu'une fois
    /// sur mille et personne ne sait pourquoi.
    fn cle() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    /// Le contrat qui compte : ce que signe la chaîne de publication, le
    /// client l'accepte — et rien d'autre.
    ///
    /// Le vérificateur et le signeur (`examples/signer.rs`) partagent la même
    /// version d'`ed25519-dalek`, dans le même `Cargo.lock` ; ce test le
    /// vérifie sur les deux formats de signature que le signeur peut produire.
    #[test]
    fn une_signature_valide_passe_une_alteration_non() {
        let cle = cle();
        let binaire = b"MZ\x90\x00 un executable imaginaire";
        let signature = cle.sign(binaire);
        let publique = cle.verifying_key();

        // Format hexadécimal — celui qu'écrit le signeur.
        let hex: String = signature.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        let relue = parse_signature(hex.as_bytes()).expect("hexadécimal accepté");
        assert!(publique.verify_strict(binaire, &relue).is_ok());

        // Format brut — accepté aussi, pour ne pas dépendre d'un
        // copier-coller qui aurait transformé le fichier.
        let brute = parse_signature(&signature.to_bytes()).expect("brut accepté");
        assert!(publique.verify_strict(binaire, &brute).is_ok());

        // Un octet changé dans le binaire, et c'est fini.
        let mut altere = binaire.to_vec();
        altere[3] ^= 0x01;
        assert!(
            publique.verify_strict(&altere, &relue).is_err(),
            "un binaire modifié ne doit jamais passer"
        );

        // Une autre clé ne signe pas pour nous.
        let intrus = SigningKey::from_bytes(&[9u8; 32]);
        let usurpee = intrus.sign(binaire);
        assert!(publique.verify_strict(binaire, &usurpee).is_err());
    }

    /// Ce qu'on peut recevoir de travers : tronqué, vide, ou pas de
    /// l'hexadécimal du tout. Aucun de ces cas ne doit passer pour valide.
    #[test]
    fn une_signature_mal_formee_est_refusee() {
        assert!(parse_signature(b"").is_err());
        assert!(parse_signature(b"pas de l'hexadecimal").is_err());
        // 63 caractères : un de moins qu'une moitié de signature.
        assert!(parse_signature("ab".repeat(31).as_bytes()).is_err());
        // 65 octets bruts : un de trop.
        assert!(parse_signature(&[0u8; 65]).is_err());
    }

    #[test]
    fn ordre_numerique_pas_lexicographique() {
        assert!(newer("0.10.0", "0.9.0"));
        assert!(newer("1.0.0", "0.99.99"));
        assert!(newer("0.1.1", "0.1.0"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn suffixes_et_champs_manquants_tolérés() {
        assert!(newer("0.2", "0.1.9"));
        assert!(!newer("0.1.0-rc1", "0.1.0"));
        assert!(newer("0.2.0+build7", "0.1.0"));
        assert!(!newer("", "0.1.0"));
    }
}
