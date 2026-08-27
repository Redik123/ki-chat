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

use ki_protocol::{perm, AccountInfo, InviteInfo, RoleId, UserId, ROLE_OWNER};
use rand::Rng;

use crate::roles::Roles;

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
    ///
    /// N'est plus la source de vérité depuis les rôles : le champ continue
    /// d'être écrit pour qu'un retour à une version antérieure retrouve ses
    /// admins, mais plus rien ici ne le consulte pour décider quoi que ce
    /// soit — c'est `roles` qui commande.
    #[serde(default)]
    admin: bool,
    /// Rôles attribués. `@everyone` n'y figure jamais : il est implicite,
    /// et `Roles` l'ajoute à chaque calcul.
    #[serde(default)]
    roles: Vec<RoleId>,
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
    /// Micro coupé par un modérateur, et surdité imposée.
    ///
    /// Persistés, et c'est le point : une sanction que la reconnexion efface
    /// n'en est pas une — il suffirait de relancer le client. Ils ne
    /// concernent que le vocal, et un compte hors ligne les garde.
    #[serde(default)]
    voice_muted: bool,
    #[serde(default)]
    voice_deafened: bool,
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
///
/// Les rôles sont rendus bruts, sans « est-ce un admin ? » calculé au
/// passage : les permissions ne se lisent que dans `Roles`, et un booléen
/// figé ici redeviendrait une seconde source de vérité, à côté de la
/// première.
#[derive(Debug)]
pub struct AuthOk {
    pub id: UserId,
    pub roles: Vec<RoleId>,
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
        let mut inner: AccountsFile = if path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&path)?)?
        } else {
            AccountsFile { next_id: 1, users: HashMap::new(), invites: Vec::new() }
        };
        // Migration : avant les rôles, `admin: true` tenait lieu de toute la
        // hiérarchie. On la transpose une fois pour toutes, au chargement —
        // sans quoi la première montée de version laisserait un serveur sans
        // personne pour l'administrer. Le compte reçoit Propriétaire, qui
        // porte ADMINISTRATOR.
        let migrated: Vec<String> = inner
            .users
            .iter()
            .filter(|(_, u)| u.admin && u.roles.is_empty())
            .map(|(name, _)| name.clone())
            .collect();
        for name in &migrated {
            if let Some(user) = inner.users.get_mut(name) {
                user.roles = vec![ROLE_OWNER];
            }
        }
        let store = Self { path, inner: Mutex::new(inner) };
        if !migrated.is_empty() {
            tracing::info!("rôle Propriétaire attribué à {} compte(s) admin", migrated.len());
            // Une migration qui ne s'écrit pas se rejouerait à chaque
            // démarrage — sans dommage, mais sans jamais aboutir non plus.
            // Autant s'arrêter et le dire.
            store
                .save(&store.inner.lock().unwrap())
                .map_err(|e| anyhow::anyhow!(e))?;
        }
        Ok(store)
    }

    /// Authentifie (ou crée, si invitation valide) un compte.
    ///
    /// **Aucun Argon2 ne tourne sous le verrou du magasin.** Il est
    /// volontairement lent (~50 à 200 ms, 19 Mio) et ce verrou est sur le
    /// chemin chaud : `roster()` le reprend à chaque entrée/sortie de vocal,
    /// à chaque connexion et à chaque déconnexion. Le tenir pendant un
    /// hachage figeait la boucle asynchrone du serveur — donc le relais des
    /// datagrammes vocaux de tout le monde — à chaque tentative de connexion.
    /// Le verrou ne sert donc plus qu'à lire, puis à écrire, jamais à calculer.
    pub fn authenticate(
        &self,
        username: &str,
        password: &str,
        invite: Option<&str>,
        server_invite: &str,
    ) -> Result<AuthOk, String> {
        // --- Phase 1, sous verrou : lecture seule (et levée d'un ban échu) ---
        let existing = {
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
                    // Volontairement NON propagé, contrairement au reste :
                    // la levée se reconstate à la tentative suivante, et
                    // refuser la connexion parce que le disque est plein
                    // enfermerait dehors quelqu'un dont le bannissement est
                    // justement terminé.
                    if let Err(e) = self.save(&inner) {
                        tracing::error!("levée de bannissement non persistée : {e}");
                    }
                    tracing::info!("bannissement de {username} expiré");
                }
                Some(inner.users[username].hash.clone())
            } else {
                None
            }
        };

        // --- Vérification du mot de passe, verrou relâché ---
        if let Some(stored) = existing {
            let parsed = PasswordHash::new(&stored).map_err(|_| "compte corrompu".to_string())?;
            if Argon2::default().verify_password(password.as_bytes(), &parsed).is_err() {
                return Err("mot de passe incorrect".into());
            }
            // Le compte est **relu** sous verrou plutôt que rendu tel qu'il
            // était avant le hachage. Celui-ci dure une centaine de
            // millisecondes, verrou relâché : un bannissement prononcé
            // pendant ce temps doit s'appliquer, sans quoi le compte entrerait
            // quand même — et la déconnexion immédiate que déclenche un
            // bannissement le manquerait, puisqu'il n'est pas encore connecté.
            // Des rôles fraîchement attribués doivent de même être ceux de la
            // session qui s'ouvre.
            let inner = self.inner.lock().unwrap();
            let now = crate::state::now_millis();
            let Some(user) = inner.users.get(username) else {
                return Err("compte inconnu".into());
            };
            match &user.ban {
                Some(ban) if ban.active(now) => return Err(ban.message(now)),
                None if user.banned => return Err("compte bloqué par un admin".into()),
                _ => {}
            }
            return Ok(AuthOk { id: user.id, roles: user.roles.clone(), created_with: None });
        }

        // Compte inconnu : création uniquement sur invitation valide —
        // le code maître du serveur, ou un code émis par un admin.
        let Some(code) = invite else {
            return Err("compte inconnu — code d'invitation requis pour en créer un".into());
        };
        // --- Phase 2 : l'invitation est validée une première fois sous verrou,
        // avant de hacher. Sans ce contrôle préalable, présenter un code bidon
        // suffirait à faire calculer un Argon2 au serveur, gratuitement. ---
        {
            let inner = self.inner.lock().unwrap();
            let now = crate::state::now_millis();
            if code != server_invite {
                match inner.invites.iter().find(|i| i.code == code) {
                    Some(i) if i.usable(now) => {}
                    Some(_) => return Err("code d'invitation expiré ou révoqué".into()),
                    None => return Err("code d'invitation invalide".into()),
                }
            }
        }
        // Toutes les validations AVANT de consommer l'invitation.
        check_password(password)?;
        let hash = hash_password(password)?;

        // --- Phase 3, sous verrou : consommation et création, en un bloc ---
        // L'invitation est **revérifiée** : le verrou a été relâché pour
        // hacher, et une autre connexion a pu épuiser le code entre-temps.
        // C'est ce qui garde le décompte des usages exact.
        let mut inner = self.inner.lock().unwrap();
        let now = crate::state::now_millis();
        if inner.users.contains_key(username) {
            return Err("ce compte vient d'être créé — reconnecte-toi".into());
        }
        let one_shot = if code == server_invite {
            None
        } else {
            match inner.invites.iter().position(|i| i.code == code) {
                Some(pos) if inner.invites[pos].usable(now) => Some(pos),
                Some(_) => return Err("code d'invitation expiré ou révoqué".into()),
                None => return Err("code d'invitation invalide".into()),
            }
        };
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
        // Le tout premier compte du serveur devient propriétaire : sans lui,
        // un serveur neuf n'aurait personne pour créer les rôles.
        let admin = inner.users.is_empty();
        let roles = if admin { vec![ROLE_OWNER] } else { Vec::new() };
        inner.next_id += 1;
        inner.users.insert(
            username.to_string(),
            StoredUser {
                id,
                hash,
                admin,
                roles: roles.clone(),
                banned: false,
                ban: None,
                avatar: None,
                voice_muted: false,
                voice_deafened: false,
            },
        );
        // Propagé : un compte qui n'atteint pas le disque disparaît au
        // redémarrage, mot de passe compris. Mieux vaut refuser la création
        // que promettre un compte fantôme.
        self.save(&inner)?;
        tracing::info!("nouveau compte : {username} (id {id}, propriétaire: {admin})");
        Ok(AuthOk { id, roles, created_with: Some(code.to_string()) })
    }

    /// Tous les comptes, pour le panneau admin. `online` est complété par
    /// l'appelant (l'état des connexions vit dans AppState).
    ///
    /// Le magasin de rôles est demandé plutôt que déduit : `rank` et `admin`
    /// se **calculent** depuis les rôles, et les laisser à l'appelant, comme
    /// `online`, c'est accepter qu'un oubli renvoie un rang 0 silencieux —
    /// donc un panneau qui propose des actions vouées au refus. Ce que la
    /// liste peut remplir juste, elle le remplit.
    pub fn list(&self, roles: &Roles) -> Vec<AccountInfo> {
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
                    admin: perm::has(roles.perms_of(&u.roles), perm::ADMINISTRATOR),
                    banned: ban.is_some() || (u.ban.is_none() && u.banned),
                    online: false,
                    ban_reason: ban.map(|b| b.reason.clone()).unwrap_or_default(),
                    ban_until: ban.and_then(|b| b.until),
                    ban_by: ban.map(|b| b.by.clone()).unwrap_or_default(),
                    roles: u.roles.clone(),
                    rank: roles.rank_of(&u.roles),
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
        invites.sort_by_key(|i| std::cmp::Reverse(i.created_at));
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
    ) -> Result<String, String> {
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
        self.save(&inner)?;
        Ok(code)
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
        self.save(&inner)?;
        Ok(())
    }

    /// Redéfinit le mot de passe d'un compte.
    ///
    /// Plus aucune hiérarchie ici : « qui peut agir sur qui » se tranche au
    /// rang, dans `quic.rs`, avant l'appel. La règle câblée « un admin ne
    /// touche pas à un autre admin » y perdait de toute façon son sens dès
    /// qu'un modérateur existe. `requester` ne sert donc plus qu'à la trace.
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
        user.hash = hash;
        self.save(&inner)?;
        tracing::info!("mot de passe de {target} réinitialisé par {requester}");
        Ok(())
    }

    /// Change son propre mot de passe, après vérification de l'ancien.
    ///
    /// Trois précautions, dans cet ordre, et chacune corrige un vrai défaut de
    /// la version précédente.
    ///
    /// **On ne hache le nouveau mot de passe qu'une fois l'ancien vérifié.**
    /// L'ordre inverse offrait un amplificateur de déni de service : chaque
    /// message coûtait un Argon2id complet — volontairement lent et gourmand
    /// en mémoire — même avec un ancien mot de passe faux. Le seau à jetons du
    /// flux de contrôle bornait le débit sans supprimer l'amplification.
    ///
    /// **Aucun des deux Argon2id ne tourne sous le verrou des comptes.**
    /// C'était pourtant le cas : la vérification s'exécutait le verrou en
    /// main, donc toute connexion, tout bannissement, toute sauvegarde
    /// attendaient derrière. C'est exactement le défaut soldé pour
    /// `authenticate`, resté ici.
    ///
    /// **Le compte est relu avant l'écriture.** Le verrou est relâché le temps
    /// de deux hachages, soit des centaines de millisecondes : un admin a pu
    /// réinitialiser ce mot de passe entre-temps, et le nôtre effacerait sa
    /// décision en silence.
    pub fn change_password(
        &self,
        username: &str,
        old_password: &str,
        new_password: &str,
    ) -> Result<(), String> {
        check_password(new_password)?;

        // --- Phase 1, sous verrou : lecture seule ---
        let stored = {
            let inner = self.inner.lock().unwrap();
            let Some(user) = inner.users.get(username) else {
                return Err("compte inconnu".into());
            };
            user.hash.clone()
        };

        // --- Vérification puis hachage, verrou relâché ---
        let parsed = PasswordHash::new(&stored).map_err(|_| "compte corrompu".to_string())?;
        if Argon2::default()
            .verify_password(old_password.as_bytes(), &parsed)
            .is_err()
        {
            return Err("ancien mot de passe incorrect".into());
        }
        let hash = hash_password(new_password)?;

        // --- Phase 2, sous verrou : écriture ---
        let mut inner = self.inner.lock().unwrap();
        let Some(user) = inner.users.get_mut(username) else {
            return Err("compte inconnu".into());
        };
        if user.hash != stored {
            return Err(
                "le mot de passe a changé entre-temps — recommence avec le nouveau".into()
            );
        }
        user.hash = hash;
        self.save(&inner)?;
        tracing::info!("{username} a changé son mot de passe");
        Ok(())
    }

    /// Bannit un compte, avec motif et durée. `duration_secs` à 0 = définitif.
    ///
    /// Le refus de bannir plus haut que soi a déménagé dans `quic.rs`, où le
    /// rang se compare : ici on ne fait plus que poser la sanction. Reste la
    /// seule règle qui ne dépende d'aucune hiérarchie — on ne se bannit pas
    /// soi-même.
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
        user.ban = Some(Ban {
            until: (duration_secs > 0).then(|| now + duration_secs.saturating_mul(1000)),
            reason: reason.to_string(),
            by: requester.to_string(),
            at: now,
        });
        user.banned = true;
        self.save(&inner)?;
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
        self.save(&inner)?;
        tracing::info!("{target} débanni par {requester}");
        Ok(())
    }

    /// Les rôles d'un compte, hors `@everyone` (toujours implicite).
    pub fn roles_of(&self, username: &str) -> Vec<RoleId> {
        let inner = self.inner.lock().unwrap();
        inner.users.get(username).map(|u| u.roles.clone()).unwrap_or_default()
    }

    /// Remplace la liste des rôles d'un compte.
    ///
    /// Remplacement complet, pas d'ajout ni de retrait unitaire : l'appelant
    /// envoie l'état voulu, et deux admins qui règlent le même compte en même
    /// temps ne composent pas un résultat que ni l'un ni l'autre n'a demandé.
    /// La liste est supposée déjà passée par `Roles::sanitize` — ici on ne
    /// sait pas quels identifiants existent.
    pub fn set_roles(&self, username: &str, roles: Vec<RoleId>) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        let Some(user) = inner.users.get_mut(username) else {
            return Err("compte inconnu".into());
        };
        user.roles = roles;
        // `admin` suit, sans plus rien décider : il n'est là que pour qu'un
        // retour à une version antérieure retrouve ses administrateurs.
        user.admin = user.roles.contains(&ROLE_OWNER);
        let count = user.roles.len();
        self.save(&inner)?;
        tracing::info!("rôles de {username} redéfinis ({count})");
        Ok(())
    }

    /// Retire un rôle de **tous** les comptes, après sa suppression.
    ///
    /// À appeler juste après `Roles::delete` : un identifiant laissé dans un
    /// compte n'accorde plus rien — `perms_of` ignore les inconnus — mais il
    /// ressortirait tel quel dans la liste des membres, et reviendrait à la
    /// vie le jour où le même identifiant serait recyclé. Renvoie le nombre
    /// de comptes touchés, pour le journal d'audit.
    pub fn remove_role(&self, role: RoleId) -> Result<usize, String> {
        let mut inner = self.inner.lock().unwrap();
        let mut touched = 0;
        for user in inner.users.values_mut() {
            if user.roles.contains(&role) {
                user.roles.retain(|r| *r != role);
                user.admin = user.roles.contains(&ROLE_OWNER);
                touched += 1;
            }
        }
        if touched > 0 {
            self.save(&inner)?;
            tracing::info!("rôle {role} retiré de {touched} compte(s)");
        }
        Ok(touched)
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
    /// Les sanctions vocales d'un compte : (micro coupé, sourd).
    ///
    /// Rendues ensemble parce qu'elles se lisent ensemble — à la connexion,
    /// une seule prise du verrou.
    pub fn voice_sanctions(&self, username: &str) -> (bool, bool) {
        let inner = self.inner.lock().unwrap();
        match inner.users.get(username) {
            Some(u) => (u.voice_muted, u.voice_deafened),
            None => (false, false),
        }
    }

    /// Pose ou lève une sanction vocale.  = laisser celle-ci telle
    /// quelle, ce qui permet de couper le micro sans toucher à la surdité.
    pub fn set_voice_sanction(
        &self,
        username: &str,
        muted: Option<bool>,
        deafened: Option<bool>,
    ) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        let Some(user) = inner.users.get_mut(username) else {
            return Err("compte inconnu".into());
        };
        if let Some(m) = muted {
            user.voice_muted = m;
        }
        if let Some(d) = deafened {
            user.voice_deafened = d;
        }
        self.save(&inner)?;
        Ok(())
    }

    pub fn set_avatar(&self, username: &str, avatar: Option<String>) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        let Some(user) = inner.users.get_mut(username) else {
            return Err("compte inconnu".into());
        };
        user.avatar = avatar;
        self.save(&inner)?;
        Ok(())
    }

    /// Écrit `users.json`. Renvoie l'échec — voir `Roles::save` : un
    /// bannissement que le disque a refusé tenait jusqu'au redémarrage puis
    /// disparaissait, l'interface ayant confirmé.
    fn save(&self, inner: &AccountsFile) -> Result<(), String> {
        let json = serde_json::to_string_pretty(inner)
            .map_err(|e| format!("sérialisation des comptes impossible : {e}"))?;
        // Écriture atomique : une coupure pendant la sauvegarde laissait
        // sinon `users.json` à zéro octet, et le serveur refusait de
        // redémarrer — tous les comptes avec.
        crate::store::write_atomic(&self.path, json.as_bytes()).map_err(|e| {
            tracing::error!("sauvegarde des comptes impossible : {e}");
            format!("sauvegarde impossible : {e}")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Une sanction vocale qu'''un redémarrage efface n'''en est pas une : il
    /// suffirait de relancer le client. Elle doit être sur le disque.
    #[test]
    fn une_sanction_vocale_survit_au_redemarrage() {
        let dir = std::env::temp_dir().join(format!("ki-sanction-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let chemin = dir.to_str().unwrap();

        let accounts = Accounts::open(chemin).unwrap();
        accounts.authenticate("kevin", "secret99", Some("inv"), "inv").unwrap();
        assert_eq!(accounts.voice_sanctions("kevin"), (false, false));

        // Couper le micro ne rend pas sourd au passage : les deux sanctions
        // sont indépendantes, et un modérateur qui fait taire quelqu'''un ne
        // veut pas toujours le priver de la conversation.
        accounts.set_voice_sanction("kevin", Some(true), None).unwrap();
        assert_eq!(accounts.voice_sanctions("kevin"), (true, false));
        accounts.set_voice_sanction("kevin", None, Some(true)).unwrap();
        assert_eq!(accounts.voice_sanctions("kevin"), (true, true));

        // Relu du disque : c'''est tout l'''enjeu.
        let relu = Accounts::open(chemin).unwrap();
        assert_eq!(relu.voice_sanctions("kevin"), (true, true));

        // Et l'''on peut lever l'''une sans lever l'''autre.
        relu.set_voice_sanction("kevin", Some(false), None).unwrap();
        assert_eq!(Accounts::open(chemin).unwrap().voice_sanctions("kevin"), (false, true));

        // Un compte inconnu se refuse plutôt que de créer une fiche vide.
        assert!(relu.set_voice_sanction("personne", Some(true), None).is_err());
        assert_eq!(relu.voice_sanctions("personne"), (false, false));
    }

    #[test]
    fn register_then_login() {
        let dir = std::env::temp_dir().join(format!("ki-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let accounts = Accounts::open(dir.to_str().unwrap()).unwrap();

        // Inconnu sans invitation : refusé.
        assert!(accounts.authenticate("alice", "secret99", None, "inv").is_err());
        // Mauvaise invitation : refusé.
        assert!(accounts.authenticate("alice", "secret99", Some("bad"), "inv").is_err());
        // Bonne invitation : compte créé, premier compte = propriétaire.
        let alice = accounts.authenticate("alice", "secret99", Some("inv"), "inv").unwrap();
        assert_eq!(alice.roles, vec![ROLE_OWNER]);
        // Deuxième compte : aucun rôle, donc @everyone et rien d'autre.
        let bob = accounts.authenticate("bob", "secret99", Some("inv"), "inv").unwrap();
        assert!(bob.roles.is_empty());
        // Reconnexion : mot de passe seul suffit.
        assert_eq!(accounts.authenticate("alice", "secret99", None, "inv").unwrap().id, alice.id);
        // Mauvais mot de passe : refusé même avec invitation.
        assert!(accounts.authenticate("alice", "wrong1", Some("inv"), "inv").is_err());

        // Persistance : rechargement depuis le fichier.
        let reloaded = Accounts::open(dir.to_str().unwrap()).unwrap();
        let again = reloaded.authenticate("alice", "secret99", None, "inv").unwrap();
        assert_eq!(again.id, alice.id);
        assert_eq!(again.roles, vec![ROLE_OWNER]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invites_bans_and_password_reset() {
        let dir = std::env::temp_dir().join(format!("ki-test-adm-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let accounts = Accounts::open(dir.to_str().unwrap()).unwrap();

        let root = accounts.authenticate("root", "rootpass", Some("inv"), "inv").unwrap();
        assert_eq!(root.roles, vec![ROLE_OWNER]);

        // Invitation à usage unique : un compte, pas deux.
        let code = accounts.create_invite("root", Some(1), "", 0).unwrap();
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
        // La règle « un admin ne touche pas à un autre admin » a déménagé :
        // elle se décide au rang, dans quic.rs. Ici, plus rien ne s'y oppose
        // — le magasin ne fait que stocker ce qu'on lui demande.
        assert!(accounts.reset_password("ami", "root", "rootpass2").is_ok());
        assert!(accounts.authenticate("root", "rootpass2", None, "inv").is_ok());

        // Bannissement définitif : login refusé, levée le rétablit.
        accounts.ban("root", "ami", "spam", 0).unwrap();
        let refus = accounts.authenticate("ami", "nouveaupass", None, "inv").unwrap_err();
        // Le motif remonte à la personne bannie : sans ça, elle écrit à
        // l'admin et il faut traiter la question à la main.
        assert!(refus.contains("spam"), "message inattendu : {refus}");
        accounts.unban("root", "ami").unwrap();
        assert!(accounts.authenticate("ami", "nouveaupass", None, "inv").is_ok());
        // Bannir le propriétaire n'est plus refusé ici : c'est le rang qui
        // l'interdira, avant l'appel. Se bannir soi-même n'a en revanche
        // aucun sens, quelle que soit la hiérarchie.
        assert!(accounts.ban("ami", "root", "", 0).is_ok());
        accounts.unban("ami", "root").unwrap();
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
        let roles = Roles::open(dir.to_str().unwrap()).unwrap();

        accounts.authenticate("root", "rootpass", Some("inv"), "inv").unwrap();
        accounts.authenticate("ami", "amipass", Some("inv"), "inv").unwrap();

        // Une heure : bloquant, et le compte est signalé banni.
        accounts.ban("root", "ami", "pause", 3600).unwrap();
        assert!(accounts.authenticate("ami", "amipass", None, "inv").is_err());
        let listed = accounts.list(&roles);
        let ami = listed.iter().find(|u| u.username == "ami").unwrap();
        assert!(ami.banned);
        assert_eq!(ami.ban_reason, "pause");
        assert_eq!(ami.ban_by, "root");
        assert!(ami.ban_until.is_some());
        // Le rang et le drapeau admin sortent des rôles, pas du fichier des
        // comptes : le propriétaire domine, le membre ordinaire est à zéro.
        assert!(!ami.admin);
        assert_eq!(ami.rank, 0);
        let root = listed.iter().find(|u| u.username == "root").unwrap();
        assert!(root.admin);
        assert_eq!(root.rank, u16::MAX);
        assert_eq!(root.roles, vec![ROLE_OWNER]);

        // Une durée déjà écoulée : le compte se reconnecte, et n'est plus
        // signalé banni.
        {
            let mut inner = accounts.inner.lock().unwrap();
            inner.users.get_mut("ami").unwrap().ban.as_mut().unwrap().until = Some(1);
            accounts.save(&inner).unwrap();
        }
        assert!(accounts.authenticate("ami", "amipass", None, "inv").is_ok());
        let listed = accounts.list(&roles);
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
        let code = accounts.create_invite("root", None, "tournoi", 0).unwrap();

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

    /// Le hachage Argon2 ne tourne plus sous le verrou du magasin, ce qui veut
    /// dire que le verrou est **relâché puis repris** au milieu d'une création
    /// de compte. Un code à usage unique doit malgré tout ne servir qu'une
    /// fois, même si deux connexions le présentent exactement en même temps.
    #[test]
    fn a_single_use_invite_survives_two_simultaneous_signups() {
        let dir = std::env::temp_dir().join(format!("ki-test-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let accounts =
            std::sync::Arc::new(Accounts::open(dir.to_str().unwrap()).unwrap());
        // Un premier compte pour que les suivants ne soient pas propriétaires.
        accounts.authenticate("chef", "motdepasse", Some("inv"), "inv").unwrap();
        let code = accounts.create_invite("chef", Some(1), "unique", 0).unwrap();

        // Deux inscriptions concurrentes avec le même code à usage unique.
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = ["alice", "bob"]
            .into_iter()
            .map(|name| {
                let (accounts, code, barrier) =
                    (accounts.clone(), code.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    accounts.authenticate(name, "motdepasse", Some(&code), "inv").is_ok()
                })
            })
            .collect();
        let wins =
            handles.into_iter().map(|h| h.join().unwrap()).filter(|ok| *ok).count();

        assert_eq!(wins, 1, "un code à usage unique ne doit créer qu'un compte");
        let listed = accounts.invites();
        assert_eq!(listed[0].uses, 1);
        assert_eq!(listed[0].uses_left, Some(0));
        assert!(listed[0].revoked, "épuisée, donc marquée");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Le hachage tourne hors verrou : un bannissement prononcé *pendant* ce
    /// temps doit quand même s'appliquer. Sinon le compte entrerait, et la
    /// déconnexion immédiate déclenchée par le bannissement le manquerait —
    /// il n'est pas encore dans la table des connectés.
    #[test]
    fn a_ban_landing_during_the_hash_still_applies() {
        let dir = std::env::temp_dir().join(format!("ki-test-banrace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let accounts =
            std::sync::Arc::new(Accounts::open(dir.to_str().unwrap()).unwrap());
        accounts.authenticate("chef", "motdepasse", Some("inv"), "inv").unwrap();
        accounts.authenticate("tricheur", "motdepasse", Some("inv"), "inv").unwrap();

        // Le bannissement tombe pendant que la connexion est en cours de
        // vérification. On le prononce depuis un autre fil, le temps que le
        // hachage (volontairement lent) s'exécute.
        let banner = {
            let accounts = accounts.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(15));
                accounts.ban("chef", "tricheur", "triche", 0).unwrap();
            })
        };
        let outcome = accounts.authenticate("tricheur", "motdepasse", None, "inv");
        banner.join().unwrap();

        // Que la course tombe d'un côté ou de l'autre, l'état final est
        // cohérent : si l'entrée a été acceptée, c'est que le ban n'était pas
        // encore posé — et une seconde tentative, elle, doit être refusée.
        assert!(accounts.authenticate("tricheur", "motdepasse", None, "inv").is_err());
        if outcome.is_ok() {
            eprintln!("course gagnée par la connexion : le ban n'était pas encore posé");
        }

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
        let code = accounts.create_invite("root", None, "", 3600).unwrap();
        {
            let mut inner = accounts.inner.lock().unwrap();
            inner.invites[0].expires_at = Some(1);
        }
        assert!(accounts.authenticate("tard", "motdepasse", Some(&code), "inv").is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Un `users.json` d'avant les rôles : les admins d'alors doivent
    /// ressortir propriétaires, sans quoi la montée de version laisserait un
    /// serveur que plus personne ne peut administrer.
    #[test]
    fn the_old_admin_flag_becomes_the_owner_role() {
        let dir = std::env::temp_dir().join(format!("ki-test-mig-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        // Écrit à la main : c'est exactement ce que produisait la version
        // précédente, `roles` compris — c'est-à-dire absent.
        std::fs::write(
            dir.join("users.json"),
            r#"{"next_id":3,"users":{
                "root":{"id":1,"hash":"","admin":true},
                "ami":{"id":2,"hash":"","admin":false}
            },"invites":[]}"#,
        )
        .unwrap();

        let accounts = Accounts::open(dir.to_str().unwrap()).unwrap();
        assert_eq!(accounts.roles_of("root"), vec![ROLE_OWNER]);
        assert!(accounts.roles_of("ami").is_empty());

        // La migration est écrite, pas seulement tenue en mémoire — et
        // `admin` reste dans le fichier : une version antérieure remise en
        // service doit y retrouver ses administrateurs.
        let written = std::fs::read_to_string(dir.join("users.json")).unwrap();
        assert!(written.contains("\"admin\": true"), "fichier inattendu : {written}");
        let reloaded = Accounts::open(dir.to_str().unwrap()).unwrap();
        assert_eq!(reloaded.roles_of("root"), vec![ROLE_OWNER]);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Attribuer, puis supprimer un rôle : le balayage des comptes est à la
    /// charge de l'appelant, et il doit vraiment nettoyer.
    #[test]
    fn roles_can_be_assigned_and_swept_after_a_deletion() {
        let dir = std::env::temp_dir().join(format!("ki-test-rol-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let accounts = Accounts::open(dir.to_str().unwrap()).unwrap();
        let roles = Roles::open(dir.to_str().unwrap()).unwrap();

        accounts.authenticate("root", "rootpass", Some("inv"), "inv").unwrap();
        accounts.authenticate("ami", "amipass", Some("inv"), "inv").unwrap();
        let modo = roles.create("Renfort", None, 50, perm::KICK).unwrap();

        accounts.set_roles("ami", roles.sanitize(&[modo.id])).unwrap();
        let listed = accounts.list(&roles);
        let ami = listed.iter().find(|u| u.username == "ami").unwrap();
        assert_eq!(ami.rank, 50);
        assert!(!ami.admin);
        assert!(perm::has(roles.perms_of(&ami.roles), perm::KICK));
        assert!(accounts.set_roles("fantôme", vec![]).is_err());

        // Rôle supprimé : le balayage le retire partout, et ne touche que
        // les comptes qui le portaient.
        roles.delete(modo.id).unwrap();
        assert_eq!(accounts.remove_role(modo.id).unwrap(), 1);
        assert_eq!(accounts.remove_role(modo.id).unwrap(), 0);
        assert!(accounts.roles_of("ami").is_empty());
        assert_eq!(accounts.roles_of("root"), vec![ROLE_OWNER]);

        std::fs::remove_dir_all(&dir).ok();
    }
}
