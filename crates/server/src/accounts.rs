//! Comptes utilisateurs persistants : pseudo + mot de passe (Argon2id).
//!
//! La création de compte est protégée par le code d'invitation du serveur
//! (KI_TOKEN) : au premier Auth d'un pseudo inconnu avec le bon code, le
//! compte est créé. Ensuite, seul le mot de passe compte.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use serde::{Deserialize, Serialize};

use ki_protocol::{AccountInfo, InviteInfo, UserId};
use rand::Rng;

#[derive(Serialize, Deserialize, Default)]
struct AccountsFile {
    next_id: UserId,
    /// pseudo -> compte
    users: HashMap<String, StoredUser>,
    /// Codes d'invitation à usage unique générés par les admins.
    #[serde(default)]
    invites: Vec<Invite>,
}

#[derive(Serialize, Deserialize, Clone)]
struct StoredUser {
    id: UserId,
    /// Hachage Argon2id au format PHC.
    hash: String,
    /// Le premier compte créé sur le serveur est admin.
    #[serde(default)]
    admin: bool,
    /// Un compte bloqué ne peut plus se connecter.
    #[serde(default)]
    banned: bool,
    /// Photo de profil : vignette PNG encodée en base64, choisie par le
    /// titulaire du compte lui-même.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    avatar: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Invite {
    code: String,
    uses_left: u32,
}

/// Résultat d'une authentification réussie.
pub struct AuthOk {
    pub id: UserId,
    pub admin: bool,
}

/// Bornes d'un mot de passe. Le minimum protège le compte ; le maximum
/// protège le serveur, qui hache ce que le client lui envoie.
fn check_password(password: &str) -> Result<(), String> {
    if password.len() < 6 {
        return Err("mot de passe trop court (6 caractères minimum)".into());
    }
    if password.len() > ki_protocol::MAX_PASSWORD {
        return Err("mot de passe trop long".into());
    }
    Ok(())
}

fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| "erreur interne".to_string())?
        .to_string())
}

pub struct Accounts {
    path: PathBuf,
    inner: Mutex<AccountsFile>,
}

impl Accounts {
    pub fn open(data_dir: &str) -> anyhow::Result<Self> {
        let path = PathBuf::from(data_dir).join("users.json");
        let inner = if path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&path)?)?
        } else {
            AccountsFile { next_id: 1, users: HashMap::new(), invites: Vec::new() }
        };
        Ok(Self { path, inner: Mutex::new(inner) })
    }

    /// Authentifie (ou crée, si invitation valide) un compte.
    pub fn authenticate(
        &self,
        username: &str,
        password: &str,
        invite: Option<&str>,
        server_invite: &str,
    ) -> Result<AuthOk, String> {
        let mut inner = self.inner.lock().unwrap();

        if let Some(user) = inner.users.get(username) {
            if user.banned {
                return Err("compte bloqué par un admin".into());
            }
            let parsed =
                PasswordHash::new(&user.hash).map_err(|_| "compte corrompu".to_string())?;
            return match Argon2::default().verify_password(password.as_bytes(), &parsed) {
                Ok(()) => Ok(AuthOk { id: user.id, admin: user.admin }),
                Err(_) => Err("mot de passe incorrect".into()),
            };
        }

        // Compte inconnu : création uniquement sur invitation valide —
        // le code maître du serveur, ou un code à usage unique.
        let Some(code) = invite else {
            return Err("compte inconnu — code d'invitation requis pour en créer un".into());
        };
        let one_shot = if code == server_invite {
            None
        } else {
            match inner.invites.iter().position(|i| i.code == code) {
                Some(pos) => Some(pos),
                None => return Err("code d'invitation invalide".into()),
            }
        };
        // Toutes les validations AVANT de consommer l'invitation.
        check_password(password)?;
        let hash = hash_password(password)?;
        if let Some(pos) = one_shot {
            inner.invites[pos].uses_left -= 1;
            if inner.invites[pos].uses_left == 0 {
                inner.invites.remove(pos);
            }
        }
        let id = inner.next_id;
        // Le tout premier compte du serveur devient admin.
        let admin = inner.users.is_empty();
        inner.next_id += 1;
        inner
            .users
            .insert(
                username.to_string(),
                StoredUser { id, hash, admin, banned: false, avatar: None },
            );
        self.save(&inner);
        tracing::info!("nouveau compte : {username} (id {id}, admin: {admin})");
        Ok(AuthOk { id, admin })
    }

    /// Tous les comptes, pour le panneau admin. `online` est complété par
    /// l'appelant (l'état des connexions vit dans AppState).
    pub fn list(&self) -> Vec<AccountInfo> {
        let inner = self.inner.lock().unwrap();
        let mut users: Vec<AccountInfo> = inner
            .users
            .iter()
            .map(|(name, u)| AccountInfo {
                username: name.clone(),
                user_id: u.id,
                admin: u.admin,
                banned: u.banned,
                online: false,
            })
            .collect();
        users.sort_by_key(|u| u.user_id);
        users
    }

    pub fn invites(&self) -> Vec<InviteInfo> {
        let inner = self.inner.lock().unwrap();
        inner
            .invites
            .iter()
            .map(|i| InviteInfo { code: i.code.clone(), uses_left: i.uses_left })
            .collect()
    }

    /// Génère un code d'invitation à usage unique, lisible et sans ambiguïté.
    pub fn create_invite(&self) -> String {
        const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
        let mut rng = rand::rng();
        let code: String = (0..10)
            .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
            .collect();
        let code = format!("ki-{code}");
        let mut inner = self.inner.lock().unwrap();
        inner.invites.push(Invite { code: code.clone(), uses_left: 1 });
        self.save(&inner);
        code
    }

    /// Redéfinit le mot de passe d'un compte. Un admin ne peut pas modifier
    /// un autre admin (mais peut se modifier lui-même).
    pub fn reset_password(
        &self,
        requester: &str,
        target: &str,
        new_password: &str,
    ) -> Result<(), String> {
        check_password(new_password)?;
        let hash = hash_password(new_password)?;
        let mut inner = self.inner.lock().unwrap();
        let Some(user) = inner.users.get_mut(target) else {
            return Err("compte inconnu".into());
        };
        if user.admin && requester != target {
            return Err("impossible de modifier le mot de passe d'un autre admin".into());
        }
        user.hash = hash;
        self.save(&inner);
        tracing::info!("mot de passe de {target} réinitialisé par {requester}");
        Ok(())
    }

    /// Change son propre mot de passe, après vérification de l'ancien.
    pub fn change_password(
        &self,
        username: &str,
        old_password: &str,
        new_password: &str,
    ) -> Result<(), String> {
        check_password(new_password)?;
        let hash = hash_password(new_password)?;
        let mut inner = self.inner.lock().unwrap();
        let Some(user) = inner.users.get_mut(username) else {
            return Err("compte inconnu".into());
        };
        let parsed = PasswordHash::new(&user.hash).map_err(|_| "compte corrompu".to_string())?;
        if Argon2::default()
            .verify_password(old_password.as_bytes(), &parsed)
            .is_err()
        {
            return Err("ancien mot de passe incorrect".into());
        }
        user.hash = hash;
        self.save(&inner);
        tracing::info!("{username} a changé son mot de passe");
        Ok(())
    }

    /// Bloque ou débloque un compte. Les admins ne peuvent pas être bloqués.
    pub fn set_banned(&self, requester: &str, target: &str, banned: bool) -> Result<(), String> {
        if requester == target {
            return Err("impossible de se bloquer soi-même".into());
        }
        let mut inner = self.inner.lock().unwrap();
        let Some(user) = inner.users.get_mut(target) else {
            return Err("compte inconnu".into());
        };
        if user.admin {
            return Err("impossible de bloquer un admin".into());
        }
        user.banned = banned;
        self.save(&inner);
        tracing::info!(
            "{target} {} par {requester}",
            if banned { "bloqué" } else { "débloqué" }
        );
        Ok(())
    }

    /// Photo de profil d'un compte, par identifiant.
    pub fn avatar_of(&self, user_id: UserId) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        inner.users.values().find(|u| u.id == user_id)?.avatar.clone()
    }

    /// Empreintes des photos de tous les comptes, pour garnir la liste des
    /// membres sans transporter les vignettes.
    pub fn avatar_hashes(&self) -> HashMap<UserId, String> {
        let inner = self.inner.lock().unwrap();
        inner
            .users
            .values()
            .filter_map(|u| {
                ki_protocol::avatar_hash(u.avatar.as_deref()).map(|hash| (u.id, hash))
            })
            .collect()
    }

    /// Définit ou retire la photo d'un compte. Chacun ne modifie que la
    /// sienne : l'appelant a déjà été authentifié sous ce pseudo.
    pub fn set_avatar(&self, username: &str, avatar: Option<String>) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        let Some(user) = inner.users.get_mut(username) else {
            return Err("compte inconnu".into());
        };
        user.avatar = avatar;
        self.save(&inner);
        Ok(())
    }

    fn save(&self, inner: &AccountsFile) {
        match serde_json::to_string_pretty(inner) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&self.path, json) {
                    tracing::error!("sauvegarde des comptes impossible : {e}");
                }
            }
            Err(e) => tracing::error!("sérialisation des comptes impossible : {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_then_login() {
        let dir = std::env::temp_dir().join(format!("ki-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let accounts = Accounts::open(dir.to_str().unwrap()).unwrap();

        // Inconnu sans invitation : refusé.
        assert!(accounts.authenticate("alice", "secret99", None, "inv").is_err());
        // Mauvaise invitation : refusé.
        assert!(accounts.authenticate("alice", "secret99", Some("bad"), "inv").is_err());
        // Bonne invitation : compte créé, premier compte = admin.
        let alice = accounts.authenticate("alice", "secret99", Some("inv"), "inv").unwrap();
        assert!(alice.admin);
        // Deuxième compte : pas admin.
        let bob = accounts.authenticate("bob", "secret99", Some("inv"), "inv").unwrap();
        assert!(!bob.admin);
        // Reconnexion : mot de passe seul suffit.
        assert_eq!(accounts.authenticate("alice", "secret99", None, "inv").unwrap().id, alice.id);
        // Mauvais mot de passe : refusé même avec invitation.
        assert!(accounts.authenticate("alice", "wrong1", Some("inv"), "inv").is_err());

        // Persistance : rechargement depuis le fichier.
        let reloaded = Accounts::open(dir.to_str().unwrap()).unwrap();
        let again = reloaded.authenticate("alice", "secret99", None, "inv").unwrap();
        assert_eq!(again.id, alice.id);
        assert!(again.admin);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invites_bans_and_password_reset() {
        let dir = std::env::temp_dir().join(format!("ki-test-adm-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let accounts = Accounts::open(dir.to_str().unwrap()).unwrap();

        let root = accounts.authenticate("root", "rootpass", Some("inv"), "inv").unwrap();
        assert!(root.admin);

        // Invitation à usage unique : un compte, pas deux.
        let code = accounts.create_invite();
        assert!(code.starts_with("ki-"));
        assert!(accounts.authenticate("ami", "amipass", Some(&code), "inv").is_ok());
        assert!(accounts.authenticate("autre", "autrepass", Some(&code), "inv").is_err());
        assert!(accounts.invites().is_empty());

        // Reset de mot de passe : l'ancien ne marche plus, le nouveau oui.
        accounts.reset_password("root", "ami", "nouveaupass").unwrap();
        assert!(accounts.authenticate("ami", "amipass", None, "inv").is_err());
        assert!(accounts.authenticate("ami", "nouveaupass", None, "inv").is_ok());
        // Un admin ne peut pas modifier un autre admin, mais peut se modifier.
        assert!(accounts.reset_password("ami", "root", "hackpass").is_err());
        assert!(accounts.reset_password("root", "root", "rootpass2").is_ok());

        // Blocage : login refusé, déblocage le rétablit ; pas de ban d'admin.
        accounts.set_banned("root", "ami", true).unwrap();
        assert!(accounts.authenticate("ami", "nouveaupass", None, "inv").is_err());
        accounts.set_banned("root", "ami", false).unwrap();
        assert!(accounts.authenticate("ami", "nouveaupass", None, "inv").is_ok());
        assert!(accounts.set_banned("ami", "root", true).is_err());
        assert!(accounts.set_banned("root", "root", true).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }
}
