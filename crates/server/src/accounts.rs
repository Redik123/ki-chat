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
    /// Un compte bloqué ne peut plus se connecter. Conservé pour rester
    /// lisible par une version antérieure : recalculé depuis `ban`.
    #[serde(default)]
    banned: bool,
    /// Détail du bannissement en cours. `None` = pas banni.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ban: Option<Ban>,
    /// Photo de profil : vignette PNG encodée en base64, choisie par le
    /// titulaire du compte lui-même.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    avatar: Option<String>,
}

/// Bannissement en cours sur un compte.
#[derive(Serialize, Deserialize, Clone)]
struct Ban {
    /// Fin du bannissement en ms Unix. `None` = définitif.
    #[serde(default)]
    until: Option<u64>,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    by: String,
    #[serde(default)]
    at: u64,
}

impl Ban {
    /// Un bannissement à durée dépassée ne bloque plus rien : il est levé
    /// paresseusement à la prochaine tentative de connexion.
    fn active(&self, now: u64) -> bool {
        self.until.is_none_or(|until| until > now)
    }

    /// Ce que voit la personne bannie. On lui dit le motif et le temps
    /// restant : un simple « accès refusé » ne fait qu'engendrer un message
    /// à l'admin, qu'il faudra traiter à la main.
    fn message(&self, now: u64) -> String {
        let delay = match self.until {
            None => " définitivement".to_string(),
            Some(until) => {
                let minutes = until.saturating_sub(now) / 60_000;
                match minutes {
                    0 => " pour moins d'une minute".to_string(),
                    1..=90 => format!(" pour encore {minutes} min"),
                    _ => format!(" pour encore {} h", minutes / 60),
                }
            }
        };
        let reason = if self.reason.is_empty() {
            String::new()
        } else {
            format!(" : {}", self.reason)
        };
        format!("compte banni{delay}{reason}")
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct Invite {
    code: String,
    /// `None` = illimité, c'est-à-dire un lien permanent. Un fichier écrit
    /// par une version antérieure porte un entier nu, que serde lit comme
    /// `Some(n)` : aucune migration à écrire.
    #[serde(default = "one_use")]
    uses_left: Option<u32>,
    /// Compteur cumulatif, jamais décrémenté : c'est lui qu'on affiche.
    #[serde(default)]
    uses: u32,
    #[serde(default)]
    label: String,
    #[serde(default)]
    created_by: String,
    #[serde(default)]
    created_at: u64,
    /// Expiration en ms Unix. `None` = jamais.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<u64>,
    /// Une invitation révoquée ou épuisée est **marquée**, pas supprimée :
    /// le journal d'audit doit pouvoir renvoyer à un code encore lisible.
    #[serde(default)]
    revoked: bool,
}

fn one_use() -> Option<u32> {
    Some(1)
}

impl Invite {
    /// Utilisable : ni révoquée, ni expirée, ni épuisée.
    fn usable(&self, now: u64) -> bool {
        !self.revoked
            && self.expires_at.is_none_or(|at| at > now)
            && self.uses_left.is_none_or(|left| left > 0)
    }
}

/// Résultat d'une authentification réussie.
#[derive(Debug)]
pub struct AuthOk {
    pub id: UserId,
    pub admin: bool,
    /// Renseigné uniquement quand cette authentification vient de **créer**
    /// le compte : le code d'invitation consommé, pour le journal d'audit.
    pub created_with: Option<String>,
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
        let now = crate::state::now_millis();

        if let Some(user) = inner.users.get(username) {
            // Un bannissement à durée dépassée se lève ici, à la première
            // tentative de connexion : pas de tâche de fond à faire tourner
            // pour ça, et la levée est constatée au seul moment où elle
            // change quelque chose.
            let expired = match &user.ban {
                Some(ban) if !ban.active(now) => true,
                Some(ban) => return Err(ban.message(now)),
                // `banned` sans `ban` : compte bloqué par une version
                // antérieure, sans motif ni durée. Définitif.
                None if user.banned => return Err("compte bloqué par un admin".into()),
                None => false,
            };
            if expired {
                if let Some(user) = inner.users.get_mut(username) {
                    user.ban = None;
                    user.banned = false;
                }
                self.save(&inner);
                tracing::info!("bannissement de {username} expiré");
            }

            let user = &inner.users[username];
            let parsed =
                PasswordHash::new(&user.hash).map_err(|_| "compte corrompu".to_string())?;
            return match Argon2::default().verify_password(password.as_bytes(), &parsed) {
                Ok(()) => Ok(AuthOk { id: user.id, admin: user.admin, created_with: None }),
                Err(_) => Err("mot de passe incorrect".into()),
            };
        }

        // Compte inconnu : création uniquement sur invitation valide —
        // le code maître du serveur, ou un code émis par un admin.
        let Some(code) = invite else {
            return Err("compte inconnu — code d'invitation requis pour en créer un".into());
        };
        let one_shot = if code == server_invite {
            None
        } else {
            match inner.invites.iter().position(|i| i.code == code) {
                Some(pos) if inner.invites[pos].usable(now) => Some(pos),
                Some(_) => return Err("code d'invitation expiré ou révoqué".into()),
                None => return Err("code d'invitation invalide".into()),
            }
        };
        // Toutes les validations AVANT de consommer l'invitation.
        check_password(password)?;
        let hash = hash_password(password)?;
        if let Some(pos) = one_shot {
            let used = &mut inner.invites[pos];
            used.uses = used.uses.saturating_add(1);
            // `saturating_sub` : sur un `uses_left` déjà à zéro — fichier
            // édité à la main, incohérence — une soustraction nue paniquerait.
            if let Some(left) = used.uses_left.as_mut() {
                *left = left.saturating_sub(1);
                // Épuisée : marquée, pas retirée de la liste. Le journal
                // d'audit doit rester résoluble vers un code existant.
                if *left == 0 {
                    used.revoked = true;
                }
            }
        }
        let id = inner.next_id;
        // Le tout premier compte du serveur devient admin.
        let admin = inner.users.is_empty();
        inner.next_id += 1;
        inner.users.insert(
            username.to_string(),
            StoredUser { id, hash, admin, banned: false, ban: None, avatar: None },
        );
        self.save(&inner);
        tracing::info!("nouveau compte : {username} (id {id}, admin: {admin})");
        Ok(AuthOk { id, admin, created_with: Some(code.to_string()) })
    }

    /// Tous les comptes, pour le panneau admin. `online` est complété par
    /// l'appelant (l'état des connexions vit dans AppState).
    pub fn list(&self) -> Vec<AccountInfo> {
        let now = crate::state::now_millis();
        let inner = self.inner.lock().unwrap();
        let mut users: Vec<AccountInfo> = inner
            .users
            .iter()
            .map(|(name, u)| {
                let ban = u.ban.as_ref().filter(|b| b.active(now));
                AccountInfo {
                    username: name.clone(),
                    user_id: u.id,
                    admin: u.admin,
                    banned: ban.is_some() || (u.ban.is_none() && u.banned),
                    online: false,
                    ban_reason: ban.map(|b| b.reason.clone()).unwrap_or_default(),
                    ban_until: ban.and_then(|b| b.until),
                    ban_by: ban.map(|b| b.by.clone()).unwrap_or_default(),
                }
            })
            .collect();
        users.sort_by_key(|u| u.user_id);
        users
    }

    /// Les invitations, la plus récente d'abord. Les révoquées et les
    /// épuisées restent de la partie : c'est l'historique des accès.
    pub fn invites(&self) -> Vec<InviteInfo> {
        let inner = self.inner.lock().unwrap();
        let mut invites: Vec<InviteInfo> = inner
            .invites
            .iter()
            .map(|i| InviteInfo {
                code: i.code.clone(),
                uses_left: i.uses_left,
                uses: i.uses,
                label: i.label.clone(),
                created_by: i.created_by.clone(),
                created_at: i.created_at,
                expires_at: i.expires_at,
                revoked: i.revoked,
            })
            .collect();
        invites.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        invites
    }

    /// Génère un code d'invitation lisible et sans ambiguïté.
    ///
    /// `uses` à `None` produit un lien permanent ; `ttl_secs` à 0, un lien
    /// sans expiration. Le code est tracé dans `data/users.json` avec son
    /// auteur et sa date : c'est ce qui rend un lien permanent acceptable.
    pub fn create_invite(
        &self,
        by: &str,
        uses: Option<u32>,
        label: &str,
        ttl_secs: u64,
    ) -> String {
        const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
        let mut rng = rand::rng();
        let code: String = (0..10)
            .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
            .collect();
        let code = format!("ki-{code}");
        let now = crate::state::now_millis();
        let mut inner = self.inner.lock().unwrap();
        inner.invites.push(Invite {
            code: code.clone(),
            // Zéro usage n'aurait pas de sens : c'est un code mort-né.
            uses_left: uses.map(|n| n.max(1)),
            uses: 0,
            label: label.to_string(),
            created_by: by.to_string(),
            created_at: now,
            expires_at: (ttl_secs > 0).then(|| now + ttl_secs.saturating_mul(1000)),
            revoked: false,
        });
        self.save(&inner);
        code
    }

    /// Révoque un code. Il reste listé, marqué, mais ne crée plus de compte.
    pub fn revoke_invite(&self, code: &str) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        let Some(invite) = inner.invites.iter_mut().find(|i| i.code == code) else {
            return Err("code d'invitation inconnu".into());
        };
        if invite.revoked {
            return Err("code déjà révoqué".into());
        }
        invite.revoked = true;
        self.save(&inner);
        Ok(())
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

    /// Bannit un compte, avec motif et durée. Les admins ne peuvent pas
    /// l'être. `duration_secs` à 0 = définitif.
    pub fn ban(
        &self,
        requester: &str,
        target: &str,
        reason: &str,
        duration_secs: u64,
    ) -> Result<(), String> {
        if requester == target {
            return Err("impossible de se bannir soi-même".into());
        }
        let now = crate::state::now_millis();
        let mut inner = self.inner.lock().unwrap();
        let Some(user) = inner.users.get_mut(target) else {
            return Err("compte inconnu".into());
        };
        if user.admin {
            return Err("impossible de bannir un admin".into());
        }
        user.ban = Some(Ban {
            until: (duration_secs > 0).then(|| now + duration_secs.saturating_mul(1000)),
            reason: reason.to_string(),
            by: requester.to_string(),
            at: now,
        });
        user.banned = true;
        self.save(&inner);
        tracing::info!("{target} banni par {requester} ({duration_secs} s) : {reason}");
        Ok(())
    }

    /// Lève un bannissement, définitif ou non.
    pub fn unban(&self, requester: &str, target: &str) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        let Some(user) = inner.users.get_mut(target) else {
            return Err("compte inconnu".into());
        };
        if user.ban.is_none() && !user.banned {
            return Err("ce compte n'est pas banni".into());
        }
        user.ban = None;
        user.banned = false;
        self.save(&inner);
        tracing::info!("{target} débanni par {requester}");
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
                // Écriture atomique : une coupure pendant la sauvegarde
                // laissait sinon `users.json` à zéro octet, et le serveur
                // refusait de redémarrer — tous les comptes avec.
                if let Err(e) = crate::store::write_atomic(&self.path, json.as_bytes()) {
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
        let code = accounts.create_invite("root", Some(1), "", 0);
        assert!(code.starts_with("ki-"));
        let ami = accounts.authenticate("ami", "amipass", Some(&code), "inv").unwrap();
        // Le code consommé remonte à l'appelant : c'est ce qui permet de
        // consigner « ce compte est né de ce lien ».
        assert_eq!(ami.created_with.as_deref(), Some(code.as_str()));
        assert!(accounts.authenticate("autre", "autrepass", Some(&code), "inv").is_err());
        // Épuisée mais **conservée** : le journal d'audit doit pouvoir
        // renvoyer à un code encore listé.
        let listed = accounts.invites();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].revoked);
        assert_eq!(listed[0].uses, 1);
        assert_eq!(listed[0].created_by, "root");

        // Reset de mot de passe : l'ancien ne marche plus, le nouveau oui.
        accounts.reset_password("root", "ami", "nouveaupass").unwrap();
        assert!(accounts.authenticate("ami", "amipass", None, "inv").is_err());
        assert!(accounts.authenticate("ami", "nouveaupass", None, "inv").is_ok());
        // Un admin ne peut pas modifier un autre admin, mais peut se modifier.
        assert!(accounts.reset_password("ami", "root", "hackpass").is_err());
        assert!(accounts.reset_password("root", "root", "rootpass2").is_ok());

        // Bannissement définitif : login refusé, levée le rétablit.
        accounts.ban("root", "ami", "spam", 0).unwrap();
        let refus = accounts.authenticate("ami", "nouveaupass", None, "inv").unwrap_err();
        // Le motif remonte à la personne bannie : sans ça, elle écrit à
        // l'admin et il faut traiter la question à la main.
        assert!(refus.contains("spam"), "message inattendu : {refus}");
        accounts.unban("root", "ami").unwrap();
        assert!(accounts.authenticate("ami", "nouveaupass", None, "inv").is_ok());
        // Ni un admin, ni soi-même.
        assert!(accounts.ban("ami", "root", "", 0).is_err());
        assert!(accounts.ban("root", "root", "", 0).is_err());
        // Débannir qui n'est pas banni n'a pas de sens silencieux.
        assert!(accounts.unban("root", "ami").is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Un bannissement à durée se lève tout seul, sans tâche de fond : la
    /// prochaine tentative de connexion le constate.
    #[test]
    fn temporary_ban_expires_on_its_own() {
        let dir = std::env::temp_dir().join(format!("ki-test-ban-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let accounts = Accounts::open(dir.to_str().unwrap()).unwrap();

        accounts.authenticate("root", "rootpass", Some("inv"), "inv").unwrap();
        accounts.authenticate("ami", "amipass", Some("inv"), "inv").unwrap();

        // Une heure : bloquant, et le compte est signalé banni.
        accounts.ban("root", "ami", "pause", 3600).unwrap();
        assert!(accounts.authenticate("ami", "amipass", None, "inv").is_err());
        let listed = accounts.list();
        let ami = listed.iter().find(|u| u.username == "ami").unwrap();
        assert!(ami.banned);
        assert_eq!(ami.ban_reason, "pause");
        assert_eq!(ami.ban_by, "root");
        assert!(ami.ban_until.is_some());

        // Une durée déjà écoulée : le compte se reconnecte, et n'est plus
        // signalé banni.
        {
            let mut inner = accounts.inner.lock().unwrap();
            inner.users.get_mut("ami").unwrap().ban.as_mut().unwrap().until = Some(1);
            accounts.save(&inner);
        }
        assert!(accounts.authenticate("ami", "amipass", None, "inv").is_ok());
        let listed = accounts.list();
        assert!(!listed.iter().find(|u| u.username == "ami").unwrap().banned);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Un lien permanent ne s'épuise pas, mais se révoque.
    #[test]
    fn permanent_invite_never_runs_out() {
        let dir = std::env::temp_dir().join(format!("ki-test-inv-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let accounts = Accounts::open(dir.to_str().unwrap()).unwrap();

        accounts.authenticate("root", "rootpass", Some("inv"), "inv").unwrap();
        let code = accounts.create_invite("root", None, "tournoi", 0);

        for name in ["a", "b", "c"] {
            assert!(accounts.authenticate(name, "motdepasse", Some(&code), "inv").is_ok());
        }
        let listed = accounts.invites();
        assert_eq!(listed[0].uses, 3);
        assert_eq!(listed[0].uses_left, None);
        assert_eq!(listed[0].label, "tournoi");
        assert!(!listed[0].revoked);

        // Révoqué : plus aucun compte, mais le code reste listé.
        accounts.revoke_invite(&code).unwrap();
        assert!(accounts.authenticate("d", "motdepasse", Some(&code), "inv").is_err());
        assert!(accounts.revoke_invite(&code).is_err());
        assert!(accounts.invites()[0].revoked);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Une invitation expirée ne crée plus de compte.
    #[test]
    fn expired_invite_is_refused() {
        let dir = std::env::temp_dir().join(format!("ki-test-exp-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let accounts = Accounts::open(dir.to_str().unwrap()).unwrap();

        accounts.authenticate("root", "rootpass", Some("inv"), "inv").unwrap();
        let code = accounts.create_invite("root", None, "", 3600);
        {
            let mut inner = accounts.inner.lock().unwrap();
            inner.invites[0].expires_at = Some(1);
        }
        assert!(accounts.authenticate("tard", "motdepasse", Some(&code), "inv").is_err());

        std::fs::remove_dir_all(&dir).ok();
    }
}
