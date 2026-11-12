//! Lancer ki-chat au démarrage de Windows, réduit à côté de l'horloge.
//!
//! Une valeur dans la clé `Run` de l'utilisateur — pas de service, pas de
//! tâche planifiée, aucun droit d'administrateur : c'est la case que
//! Windows montre dans « Applications de démarrage », et qu'on peut y
//! décocher. La vérité est dans le registre, pas dans les réglages de
//! ki-chat : on la relit, on ne la suppose pas.
//!
//! La commande porte `--reduit` : ki-chat s'ouvre dans la zone de
//! notification, sans surgir à l'ouverture de session, et se reconnecte au
//! dernier serveur si son mot de passe est retenu.
//!
//! En version de développement, tout s'écrit dans une clé d'essai sans
//! effet : cocher la case d'un ki-chat de test ne doit pas faire démarrer
//! un binaire de `target/` avec Windows.

use std::path::Path;

/// La clé que Windows lit à l'ouverture de session, sous `HKEY_CURRENT_USER`.
const CLE_RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Celle des versions de développement : personne ne la lit.
const CLE_ESSAI: &str = r"Software\ki-chat\essai-demarrage";
/// Le nom de la valeur. L'installeur le connaît aussi : il l'efface à la
/// désinstallation (`installer/ki-chat.iss`).
const NOM: &str = "ki-chat";

/// Ce que dit le registre.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Etat {
    /// ki-chat ne démarre pas avec Windows.
    Absent,
    /// Ce ki-chat-ci démarre avec Windows.
    Ici,
    /// Windows lance un autre ki-chat — une ancienne installation, ailleurs
    /// sur le disque : sa commande.
    Ailleurs(String),
}

impl Etat {
    pub(crate) fn actif(&self) -> bool {
        *self != Etat::Absent
    }
}

/// La commande que Windows lancera : l'exécutable entre guillemets — son
/// chemin a des espaces chez presque tout le monde — puis `--reduit`.
fn commande(exe: &Path) -> String {
    format!("\"{}\" {}", exe.display(), crate::zone::ARG_REDUIT)
}

/// Ce que vaut ce qui est rangé, pour l'exécutable `exe`. Les chemins de
/// Windows ne distinguent pas les majuscules.
fn juger(rangee: Option<&str>, exe: &Path) -> Etat {
    match rangee.map(str::trim).filter(|r| !r.is_empty()) {
        None => Etat::Absent,
        Some(r) if r.eq_ignore_ascii_case(&commande(exe)) => Etat::Ici,
        Some(r) => Etat::Ailleurs(r.to_string()),
    }
}

fn cle() -> &'static str {
    if cfg!(debug_assertions) { CLE_ESSAI } else { CLE_RUN }
}

/// Cette plateforme sait-elle démarrer ki-chat avec la session ?
pub(crate) fn disponible() -> bool {
    cfg!(windows)
}

/// Ce que dit le registre, maintenant.
pub(crate) fn etat() -> Etat {
    match std::env::current_exe() {
        Ok(exe) => juger(registre::lire(cle(), NOM).as_deref(), &exe),
        Err(_) => Etat::Absent,
    }
}

/// Inscrit ce ki-chat-ci au démarrage de Windows, ou l'en retire.
pub(crate) fn regler(actif: bool) -> Result<(), String> {
    if actif {
        let exe = std::env::current_exe().map_err(|e| format!("exécutable introuvable : {e}"))?;
        registre::ecrire(cle(), NOM, &commande(&exe))
    } else {
        registre::effacer(cle(), NOM)
    }
}

#[cfg(windows)]
mod registre {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
    use windows::Win32::System::Registry::{RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ};

    /// En UTF-16, terminé par un zéro : ce que Windows attend.
    fn large(texte: &str) -> Vec<u16> {
        texte.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn dire(quoi: &str, erreur: WIN32_ERROR) -> String {
        format!("{quoi} : {}", std::io::Error::from_raw_os_error(erreur.0 as i32))
    }

    /// La valeur `nom` de la clé `cle`, si elle existe et que c'est du texte.
    pub fn lire(cle: &str, nom: &str) -> Option<String> {
        let (cle, nom) = (large(cle), large(nom));
        let mut tampon = vec![0u16; 2048];
        let mut octets = (tampon.len() * 2) as u32;
        // SAFETY : les chaînes sont terminées par un zéro et vivent le temps
        // de l'appel ; `octets` dit la place du tampon, et Windows n'écrit
        // pas au-delà.
        let erreur = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(cle.as_ptr()),
                PCWSTR(nom.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(tampon.as_mut_ptr().cast()),
                Some(&mut octets),
            )
        };
        if erreur != ERROR_SUCCESS {
            return None;
        }
        let lus = (octets as usize / 2).min(tampon.len());
        let fin = tampon[..lus].iter().position(|c| *c == 0).unwrap_or(lus);
        Some(String::from_utf16_lossy(&tampon[..fin]))
    }

    /// Écrit la valeur, en créant la clé s'il le faut.
    pub fn ecrire(cle: &str, nom: &str, valeur: &str) -> Result<(), String> {
        let (cle, nom, valeur) = (large(cle), large(nom), large(valeur));
        // SAFETY : les chaînes sont terminées par un zéro et vivent le temps
        // de l'appel ; la taille donnée est celle de `valeur`, zéro compris.
        let erreur = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                PCWSTR(cle.as_ptr()),
                PCWSTR(nom.as_ptr()),
                REG_SZ.0,
                Some(valeur.as_ptr().cast()),
                (valeur.len() * 2) as u32,
            )
        };
        if erreur == ERROR_SUCCESS { Ok(()) } else { Err(dire("écriture au registre refusée", erreur)) }
    }

    /// Efface la valeur. Déjà absente : c'est fait.
    pub fn effacer(cle: &str, nom: &str) -> Result<(), String> {
        let (cle, nom) = (large(cle), large(nom));
        // SAFETY : les chaînes sont terminées par un zéro et vivent le temps
        // de l'appel.
        let erreur = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, PCWSTR(cle.as_ptr()), PCWSTR(nom.as_ptr())) };
        if erreur == ERROR_SUCCESS || erreur == ERROR_FILE_NOT_FOUND { Ok(()) } else { Err(dire("effacement au registre refusé", erreur)) }
    }

    /// Efface une clé entière : pour ranger après un essai.
    #[cfg(test)]
    pub fn effacer_cle(cle: &str) {
        use windows::Win32::System::Registry::RegDeleteTreeW;
        let cle = large(cle);
        // SAFETY : la chaîne est terminée par un zéro et vit le temps de
        // l'appel.
        let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(cle.as_ptr())) };
    }
}

#[cfg(not(windows))]
mod registre {
    pub fn lire(_cle: &str, _nom: &str) -> Option<String> {
        None
    }
    pub fn ecrire(_cle: &str, _nom: &str, _valeur: &str) -> Result<(), String> {
        Err("le démarrage avec la session n'est pas géré sur ce système".into())
    }
    pub fn effacer(_cle: &str, _nom: &str) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_commande_porte_les_guillemets_et_reduit() {
        let exe = Path::new(r"C:\Users\Un Nom\AppData\Local\Programs\ki-chat\ki-chat.exe");
        assert_eq!(commande(exe), r#""C:\Users\Un Nom\AppData\Local\Programs\ki-chat\ki-chat.exe" --reduit"#);
    }

    #[test]
    fn ce_qui_est_range_se_juge() {
        let exe = Path::new(r"C:\Programmes\ki-chat\ki-chat.exe");
        assert_eq!(juger(None, exe), Etat::Absent);
        assert_eq!(juger(Some("  "), exe), Etat::Absent);
        assert_eq!(juger(Some(r#""C:\Programmes\ki-chat\ki-chat.exe" --reduit"#), exe), Etat::Ici);
        // Windows ne distingue pas les majuscules d'un chemin.
        assert_eq!(juger(Some(r#""c:\programmes\KI-CHAT\ki-chat.exe" --reduit"#), exe), Etat::Ici);
        // Une autre installation, ou la même sans `--reduit` : pas celle-ci.
        let ailleurs = r#""D:\Vieux\ki-chat.exe" --reduit"#;
        assert_eq!(juger(Some(ailleurs), exe), Etat::Ailleurs(ailleurs.into()));
        assert!(juger(Some(ailleurs), exe).actif() && !Etat::Absent.actif());
        assert!(matches!(juger(Some(r#""C:\Programmes\ki-chat\ki-chat.exe""#), exe), Etat::Ailleurs(_)));
    }

    /// Le registre, pour de vrai, dans une clé d'essai que personne ne lit :
    /// écrire, relire, effacer, et effacer ce qui n'existe plus.
    #[cfg(windows)]
    #[test]
    fn le_registre_ecrit_relit_et_efface() {
        let cle = format!(r"Software\ki-chat\essai-{}", std::process::id());
        assert_eq!(registre::lire(&cle, NOM), None);
        let valeur = r#""C:\Un chemin\à accents\ki-chat.exe" --reduit"#;
        registre::ecrire(&cle, NOM, valeur).unwrap();
        assert_eq!(registre::lire(&cle, NOM).as_deref(), Some(valeur));
        registre::ecrire(&cle, NOM, "autre").unwrap();
        assert_eq!(registre::lire(&cle, NOM).as_deref(), Some("autre"));
        registre::effacer(&cle, NOM).unwrap();
        assert_eq!(registre::lire(&cle, NOM), None);
        registre::effacer(&cle, NOM).unwrap();
        registre::effacer_cle(&cle);
    }

    /// Une version de développement n'écrit jamais dans la vraie clé.
    #[test]
    fn le_developpement_n_ecrit_pas_dans_la_vraie_cle() {
        if cfg!(debug_assertions) {
            assert_eq!(cle(), CLE_ESSAI);
        } else {
            assert_eq!(cle(), CLE_RUN);
        }
    }
}
