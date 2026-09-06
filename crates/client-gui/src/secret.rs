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
const TAG: &str = if cfg!(windows) {
    "dpapi"
} else if cfg!(target_os = "macos") {
    "keychain"
} else {
    "none"
};

/// Vrai si cette plateforme sait ranger un secret en sûreté. Quand c'est
/// faux, on refuse de mémoriser plutôt que d'écrire en clair.
pub fn available() -> bool {
    cfg!(any(windows, target_os = "macos"))
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
// macOS : le Trousseau tient la clé, le blob est chiffré avec
// ---------------------------------------------------------------------

/// Le Trousseau sait ranger un mot de passe par serveur, mais on ne s'en
/// sert pas ainsi : chaque entrée serait une question de plus à
/// l'utilisateur (« ki-chat veut utiliser vos informations
/// confidentielles… »), et le fichier des serveurs ne porterait plus qu'une
/// référence, illisible sans le Trousseau qui va avec.
///
/// On y range donc **une seule clé**, tirée au hasard la première fois, et
/// les secrets sont chiffrés avec elle (XChaCha20-Poly1305, celui du partage
/// d'écran). Le blob est alors un blob comme sous DPAPI : autonome, et
/// illisible ailleurs — la clé ne quitte jamais cette session de cet
/// utilisateur, macOS y veille. Une entrée, une autorisation à donner.
///
/// Le paquet est signé ad hoc : à chaque mise à jour, macOS voit un
/// exécutable nouveau et redemande une fois l'accès à l'entrée. « Toujours
/// autoriser » vaut jusqu'à la suivante.
#[cfg(target_os = "macos")]
mod platform {
    use std::sync::Mutex;

    use chacha20poly1305::aead::{Aead, KeyInit};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};
    use security_framework::passwords::{get_generic_password, set_generic_password};

    /// Nom de l'entrée dans le Trousseau. Les tests prennent la leur : un
    /// binaire de test qui lirait l'entrée de l'application déclencherait la
    /// question d'accès en plein `cargo test`.
    const SERVICE: &str = if cfg!(test) { "ki-chat (tests)" } else { "ki-chat" };
    const ACCOUNT: &str = "clé du coffre";
    /// `errSecItemNotFound` : l'entrée n'existe pas encore.
    const INTROUVABLE: i32 = -25300;
    const NONCE: usize = 24;

    /// La clé, une fois obtenue : le Trousseau n'est interrogé qu'une fois
    /// par session, donc une seule question si macOS en pose une. Un refus
    /// n'est pas mémorisé — on redemandera.
    static CLE: Mutex<Option<[u8; 32]>> = Mutex::new(None);

    fn cle() -> Result<[u8; 32], String> {
        let mut cache = CLE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(k) = *cache {
            return Ok(k);
        }
        let k = match get_generic_password(SERVICE, ACCOUNT) {
            Ok(octets) => octets
                .try_into()
                .map_err(|_| "Trousseau : clé du coffre de longueur inattendue".to_string())?,
            Err(e) if e.code() == INTROUVABLE => {
                let mut neuve = [0u8; 32];
                security_framework::random::SecRandom::default()
                    .copy_bytes(&mut neuve)
                    .map_err(|e| format!("Trousseau : tirage de la clé : {e}"))?;
                set_generic_password(SERVICE, ACCOUNT, &neuve)
                    .map_err(|e| format!("Trousseau : {e}"))?;
                neuve
            }
            Err(e) => return Err(format!("Trousseau : {e}")),
        };
        *cache = Some(k);
        Ok(k)
    }

    pub fn protect(plaintext: &[u8]) -> Result<Vec<u8>, String> {
        let cipher = XChaCha20Poly1305::new(&cle()?.into());
        let mut nonce = [0u8; NONCE];
        security_framework::random::SecRandom::default()
            .copy_bytes(&mut nonce)
            .map_err(|e| format!("tirage du nonce : {e}"))?;
        let sealed = cipher
            .encrypt(XNonce::from_slice(&nonce), plaintext)
            .map_err(|_| "chiffrement impossible".to_string())?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    pub fn reveal(sealed: &[u8]) -> Result<Vec<u8>, String> {
        // Trop court pour être des nôtres : on le dit sans même déranger
        // le Trousseau.
        if sealed.len() < NONCE + 16 {
            return Err("blob tronqué".into());
        }
        let (nonce, corps) = sealed.split_at(NONCE);
        let cipher = XChaCha20Poly1305::new(&cle()?.into());
        cipher
            .decrypt(XNonce::from_slice(nonce), corps)
            .map_err(|_| "blob illisible avec la clé de ce Trousseau".to_string())
    }

    /// Efface l'entrée des tests, pour ne rien laisser dans le Trousseau
    /// de qui lance `cargo test`.
    #[cfg(test)]
    pub fn oublier() {
        let _ = security_framework::passwords::delete_generic_password(SERVICE, ACCOUNT);
        *CLE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

// ---------------------------------------------------------------------
// Ailleurs : rien tant que le coffre natif n'est pas branché
// ---------------------------------------------------------------------

#[cfg(not(any(windows, target_os = "macos")))]
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
    #[cfg(any(windows, target_os = "macos"))]
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

    /// Le Trousseau, en vrai : une entrée de test, créée puis effacée.
    #[test]
    #[cfg(target_os = "macos")]
    fn le_trousseau_scelle_et_rend() {
        let sealed = protect(SAMPLE).expect("Trousseau disponible");
        assert!(sealed.starts_with("keychain:"));
        assert!(!sealed.contains("factice"));
        assert_eq!(reveal(&sealed).as_deref(), Some(SAMPLE));
        // Deux scellés du même secret ne se ressemblent pas : le nonce.
        assert_ne!(protect(SAMPLE).unwrap(), sealed);
        // Un octet altéré, et c'est fini.
        let mut altere = base64::engine::general_purpose::STANDARD
            .decode(sealed.trim_start_matches("keychain:"))
            .unwrap();
        altere[30] ^= 1;
        let altere = format!(
            "keychain:{}",
            base64::engine::general_purpose::STANDARD.encode(altere)
        );
        assert!(reveal(&altere).is_none());
        platform::oublier();
    }

    #[test]
    fn a_blob_from_elsewhere_is_refused_not_guessed() {
        // Étiquette d'une autre plateforme, ou contenu impossible pour
        // celle-ci — tout cela se refuse sans toucher au coffre.
        assert!(reveal("keystore:AAAA").is_none());
        assert!(reveal("keychain:AAAA").is_none());
        // Bonne étiquette, contenu illisible.
        assert!(reveal("dpapi:pas du base64 !").is_none());
        assert!(reveal("dpapi:AAAA").is_none());
        // Pas d'étiquette du tout : ancien fichier en clair.
        assert!(reveal("un-secret-en-clair").is_none());
    }
}
