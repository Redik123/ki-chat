//! Coffre à secrets : protège les mots de passe mémorisés avec le
//! mécanisme natif de la plateforme.
//!
//! # Pourquoi une abstraction plutôt qu'un chiffrement maison
//!
//! Un chiffrement maison a toujours le même problème : où ranger la clé ?
//! La dériver d'un identifiant machine (adresse MAC, numéro de série…) ne
//! règle rien — ces valeurs ne sont pas secrètes, elles se lisent sur la
//! machine et sur le réseau, donc la clé voyage avec le coffre. Et elles ne
//! sont même pas lisibles partout : Android et iOS renvoient une adresse MAC
//! bidon à toutes les applications depuis des années.
//!
//! Chaque système d'exploitation sait déjà faire ça correctement, avec une
//! clé dérivée de la session de l'utilisateur et gardée hors de portée du
//! processus : DPAPI sur Windows, le Trousseau sur macOS et iOS, le Keystore
//! sur Android. Ce module expose donc une porte unique, et chaque plateforme
//! y branche son mécanisme.
//!
//! # Format stocké
//!
//! `<mécanisme>:<base64>` — l'étiquette permet de reconnaître un secret
//! produit ailleurs (autre machine, autre système, sauvegarde importée) et
//! de l'ignorer proprement au lieu de rendre n'importe quoi.

use base64::Engine as _;

/// Étiquette du mécanisme en service sur cette plateforme.
const TAG: &str = if cfg!(windows) { "dpapi" } else { "none" };

/// Vrai si cette plateforme sait ranger un secret en sûreté. Quand c'est
/// faux, on refuse de mémoriser plutôt que d'écrire en clair.
pub fn available() -> bool {
    cfg!(windows)
}

/// Chiffre un secret pour cette machine et cet utilisateur.
pub fn protect(plaintext: &str) -> Result<String, String> {
    let sealed = platform::protect(plaintext.as_bytes())?;
    Ok(format!("{TAG}:{}", base64::engine::general_purpose::STANDARD.encode(sealed)))
}

/// Déchiffre un secret. Renvoie `None` si le blob vient d'un autre
/// mécanisme, d'une autre machine, ou s'il est abîmé — dans tous ces cas on
/// se contente de redemander le mot de passe.
pub fn reveal(blob: &str) -> Option<String> {
    let (tag, payload) = blob.split_once(':')?;
    if tag != TAG {
        return None;
    }
    let sealed = base64::engine::general_purpose::STANDARD.decode(payload).ok()?;
    let plain = platform::reveal(&sealed).ok()?;
    String::from_utf8(plain).ok()
}

// ---------------------------------------------------------------------
// Windows : DPAPI, portée utilisateur
// ---------------------------------------------------------------------

#[cfg(windows)]
mod platform {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData,
    };

    /// Ne jamais afficher de fenêtre : on chiffre pendant le rendu.
    const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;

    /// Reprend le contenu d'un blob rendu par DPAPI, puis le libère.
    ///
    /// # Safety
    /// `blob` doit être un blob rempli par DPAPI et pas encore libéré.
    unsafe fn take(blob: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        let bytes = unsafe {
            std::slice::from_raw_parts(blob.pbData, blob.cbData as usize).to_vec()
        };
        unsafe { LocalFree(Some(HLOCAL(blob.pbData as *mut _))) };
        bytes
    }

    pub fn protect(plaintext: &[u8]) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: plaintext.len() as u32,
            pbData: plaintext.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptProtectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
            .map_err(|e| format!("DPAPI : {e}"))?;
            Ok(take(output))
        }
    }

    pub fn reveal(sealed: &[u8]) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: sealed.len() as u32,
            pbData: sealed.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptUnprotectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
            .map_err(|e| format!("DPAPI : {e}"))?;
            Ok(take(output))
        }
    }
}

// ---------------------------------------------------------------------
// Ailleurs : rien tant que le coffre natif n'est pas branché
// ---------------------------------------------------------------------

#[cfg(not(windows))]
mod platform {
    const MISSING: &str = "aucun coffre à secrets sur cette plateforme";

    pub fn protect(_plaintext: &[u8]) -> Result<Vec<u8>, String> {
        Err(MISSING.into())
    }

    pub fn reveal(_sealed: &[u8]) -> Result<Vec<u8>, String> {
        Err(MISSING.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Phrase de test volontairement fictive, avec des caractères
    /// multi-octets : c'est l'encodage qu'on veut éprouver, pas un secret.
    const SAMPLE: &str = "mot-de-passe-factice é€… 123";

    #[test]
    #[cfg(windows)]
    fn a_secret_survives_a_round_trip() {
        let sealed = protect(SAMPLE).expect("DPAPI disponible");
        assert!(sealed.starts_with("dpapi:"));
        // Le secret ne doit apparaître nulle part dans le blob.
        assert!(!sealed.contains("factice"));
        assert_eq!(reveal(&sealed).as_deref(), Some(SAMPLE));
    }

    #[test]
    #[cfg(windows)]
    fn an_empty_secret_is_handled() {
        let sealed = protect("").unwrap();
        assert_eq!(reveal(&sealed).as_deref(), Some(""));
    }

    #[test]
    fn a_blob_from_elsewhere_is_refused_not_guessed() {
        // Étiquette inconnue : secret produit par une autre plateforme.
        assert!(reveal("keychain:AAAA").is_none());
        // Bonne étiquette, contenu illisible.
        assert!(reveal("dpapi:pas du base64 !").is_none());
        assert!(reveal("dpapi:AAAA").is_none());
        // Pas d'étiquette du tout : ancien fichier en clair.
        assert!(reveal("un-secret-en-clair").is_none());
    }
}
