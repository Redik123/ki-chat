//! Rôles du serveur : couleur de pseudo, rang et permissions, persistés dans
//! `data/roles.json`.
//!
//! Deux idées portent tout le module.
//!
//! `@everyone` est **implicite** : il n'est jamais rangé dans la liste de
//! rôles d'un compte, et il est toujours ajouté au calcul. Un compte sans
//! aucun rôle attribué peut donc quand même lire et écrire, et changer ce que
//! reçoit « tout le monde » se fait à un seul endroit, sans balayer les
//! comptes.
//!
//! Le rang est ce qui rend la hiérarchie sûre. `ADMINISTRATOR` court-circuite
//! la vérification de permission, jamais celle de rang : c'est ce qui empêche
//! un second administrateur de bannir le propriétaire. Ce module fournit les
//! rangs ; c'est `quic.rs` qui les compare avant d'agir.

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use ki_protocol::{perm, Perms, RoleId, RoleInfo, ROLE_EVERYONE, ROLE_OWNER};

/// Un nom de rôle tient dans une étiquette à côté d'un pseudo : au-delà, il
/// ne s'affiche plus, il déborde.
const MAX_ROLE_NAME: usize = 32;

/// Plafond de rang d'un rôle ordinaire. Le propriétaire occupe `u16::MAX` et
/// doit y rester seul : à rang égal on n'agit pas sur l'autre, mais un rôle
/// créé au sommet mettrait quand même toute la modération à l'arrêt entre
/// deux comptes qui le portent.
const MAX_RANK: u16 = u16::MAX - 1;

#[derive(Serialize, Deserialize)]
struct RolesFile {
    /// Jamais décrémenté, jamais réattribué : un identifiant recyclé rendrait
    /// à un rôle neuf les attributions de l'ancien, restées dans les comptes
    /// et dans les restrictions de salons.
    next_id: RoleId,
    roles: Vec<RoleInfo>,
}

/// Ce que trouve un serveur qui démarre pour la première fois.
///
/// Le modérateur n'est pas un rôle système : il est là pour être remanié,
/// renommé ou supprimé selon les habitudes du serveur. Les deux autres, si.
fn initial_roles() -> RolesFile {
    RolesFile {
        next_id: 4,
        roles: vec![
            RoleInfo {
                id: ROLE_EVERYONE,
                name: "@everyone".into(),
                color: None,
                rank: 0,
                perms: perm::DEFAULT,
                system: true,
            },
            RoleInfo {
                id: ROLE_OWNER,
                name: "Propriétaire".into(),
                color: None,
                rank: u16::MAX,
                perms: perm::ADMINISTRATOR,
                system: true,
            },
            RoleInfo {
                id: 3,
                name: "Modérateur".into(),
                color: None,
                rank: 100,
                perms: perm::KICK
                    | perm::BAN
                    | perm::CREATE_INVITE
                    | perm::MANAGE_INVITES
                    | perm::VIEW_AUDIT_LOG,
                system: false,
            },
        ],
    }
}

/// Les bits que le protocole définit réellement.
///
/// Ce qu'un client envoie en dehors est écarté à l'entrée : un bit inconnu
/// stocké aujourd'hui deviendrait une permission accordée en douce le jour
/// où une version suivante lui donne un sens.
pub fn known_perms() -> Perms {
    perm::ALL.iter().fold(0, |acc, (bit, _, _)| acc | *bit)
}

/// Ce qui ne s'accorde pas à `@everyone` : la règle est portée par le
/// protocole, pour que l'interface cesse de proposer ce qu'on refuse ici.
use ki_protocol::perm::NOT_FOR_EVERYONE;

fn clean_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("nom de rôle vide".into());
    }
    if name.chars().count() > MAX_ROLE_NAME {
        return Err("nom de rôle trop long".into());
    }
    Ok(name.to_string())
}

pub struct Roles {
    path: PathBuf,
    inner: Mutex<RolesFile>,
}

impl Roles {
    /// Charge `data/roles.json`, ou pose les rôles du premier démarrage.
    ///
    /// Un fichier illisible arrête le serveur au lieu de retomber sur les
    /// rôles par défaut : repartir de zéro rendrait tout le monde membre
    /// ordinaire, silencieusement, et le prochain enregistrement écraserait
    /// ce qu'il restait à réparer.
    pub fn open(data_dir: &str) -> anyhow::Result<Self> {
        let path = PathBuf::from(data_dir).join("roles.json");
        let (inner, fresh) = match std::fs::read_to_string(&path) {
            Ok(json) => (
                serde_json::from_str(&json).context("lecture de roles.json")?,
                false,
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (initial_roles(), true),
            Err(e) => return Err(e).context("lecture de roles.json"),
        };
        let store = Self { path, inner: Mutex::new(inner) };
        // Le fichier est écrit dès le premier démarrage : sans ça, un admin
        // qui cherche à comprendre d'où sortent ces trois rôles ne trouve
        // rien dans `data/`.
        if fresh {
            store.save(&store.inner.lock().unwrap());
        }
        Ok(store)
    }

    /// Tous les rôles, du plus haut rang au plus bas — l'ordre d'affichage
    /// est celui de l'autorité. À rang égal l'identifiant tranche, sinon la
    /// liste changerait d'ordre d'un envoi à l'autre.
    pub fn list(&self) -> Vec<RoleInfo> {
        let inner = self.inner.lock().unwrap();
        let mut roles = inner.roles.clone();
        roles.sort_by(|a, b| b.rank.cmp(&a.rank).then(a.id.cmp(&b.id)));
        roles
    }

    pub fn get(&self, id: RoleId) -> Option<RoleInfo> {
        let inner = self.inner.lock().unwrap();
        inner.roles.iter().find(|r| r.id == id).cloned()
    }

    /// Permissions effectives : l'union des rôles portés, plus `@everyone`.
    ///
    /// Les identifiants inconnus sont ignorés plutôt que refusés — un compte
    /// qui garde la trace d'un rôle supprimé doit continuer à se connecter.
    pub fn perms_of(&self, roles: &[RoleId]) -> Perms {
        let inner = self.inner.lock().unwrap();
        Self::held(&inner, roles).fold(0, |acc, r| acc | r.perms)
    }

    /// Rang le plus élevé. `@everyone` étant à 0, un compte sans rôle est au
    /// plancher : tout le monde peut agir sur lui, lui sur personne.
    pub fn rank_of(&self, roles: &[RoleId]) -> u16 {
        let inner = self.inner.lock().unwrap();
        Self::held(&inner, roles).map(|r| r.rank).max().unwrap_or(0)
    }

    /// Couleur du pseudo : celle du rôle le mieux classé **qui en porte
    /// une**. Un rôle sans couleur ne masque pas celui d'en dessous, sinon
    /// donner un rôle de tri à quelqu'un lui effacerait sa couleur.
    pub fn color_of(&self, roles: &[RoleId]) -> Option<u32> {
        let inner = self.inner.lock().unwrap();
        Self::held(&inner, roles)
            .filter(|r| r.color.is_some())
            .max_by_key(|r| (r.rank, r.id))
            .and_then(|r| r.color)
    }

    /// Nettoie une liste avant de l'attribuer à un compte : les identifiants
    /// inconnus et les doublons disparaissent, et `@everyone` n'y entre
    /// jamais — il est implicite, l'y écrire donnerait l'illusion qu'on peut
    /// le retirer à quelqu'un.
    pub fn sanitize(&self, roles: &[RoleId]) -> Vec<RoleId> {
        let inner = self.inner.lock().unwrap();
        let mut kept: Vec<RoleId> = Vec::new();
        for id in roles {
            if *id != ROLE_EVERYONE
                && !kept.contains(id)
                && inner.roles.iter().any(|r| r.id == *id)
            {
                kept.push(*id);
            }
        }
        kept
    }

    /// Les rôles réellement portés : ceux de la liste, plus `@everyone`.
    fn held<'a>(
        inner: &'a RolesFile,
        roles: &'a [RoleId],
    ) -> impl Iterator<Item = &'a RoleInfo> + 'a {
        inner
            .roles
            .iter()
            .filter(move |r| r.id == ROLE_EVERYONE || roles.contains(&r.id))
    }

    /// Crée un rôle. Le rang et les permissions viennent de l'appelant :
    /// c'est `quic.rs` qui vérifie qu'il ne s'accorde pas plus que ce qu'il
    /// possède déjà.
    pub fn create(
        &self,
        name: &str,
        color: Option<u32>,
        rank: u16,
        perms: Perms,
    ) -> Result<RoleInfo, String> {
        let name = clean_name(name)?;
        let mut inner = self.inner.lock().unwrap();
        let role = RoleInfo {
            id: inner.next_id,
            name,
            color,
            rank: rank.min(MAX_RANK),
            perms: perms & known_perms(),
            // Un rôle créé n'est jamais système : sinon n'importe qui pouvant
            // gérer les rôles se fabriquerait un rôle indestructible.
            system: false,
        };
        inner.next_id += 1;
        inner.roles.push(role.clone());
        self.save(&inner);
        tracing::info!("rôle créé : {} (id {}, rang {})", role.name, role.id, role.rank);
        Ok(role)
    }

    /// Remplace un rôle par la version fournie.
    ///
    /// D'un rôle système, ni le nom ni le rang ne bougent. Les permissions,
    /// elles, se distinguent :
    ///
    /// - Celles du **propriétaire** sont verrouillées. Lui ôter
    ///   `ADMINISTRATOR`, c'est laisser le serveur sans personne pour le lui
    ///   rendre.
    /// - Celles d'**`@everyone`** se règlent, et c'est indispensable : les
    ///   permissions sont une union, si bien que ce que `@everyone` accorde
    ///   est accordé à tout le monde, définitivement. Tant qu'il était figé,
    ///   retirer à quelqu'un le droit d'écrire, de parler ou de partager était
    ///   impossible — les cases correspondantes de l'éditeur de rôles ne
    ///   faisaient donc rien. On règle ici ce que reçoit tout le monde, puis
    ///   l'on rend le droit par un rôle à qui doit l'avoir.
    pub fn edit(&self, role: RoleInfo) -> Result<(), String> {
        let name = clean_name(&role.name)?;
        let mut inner = self.inner.lock().unwrap();
        let Some(current) = inner.roles.iter_mut().find(|r| r.id == role.id) else {
            return Err("rôle inconnu".into());
        };
        if current.system {
            if name != current.name {
                return Err("un rôle du serveur ne se renomme pas".into());
            }
            if role.rank != current.rank {
                return Err("le rang d'un rôle du serveur ne change pas".into());
            }
            if current.id == ROLE_OWNER {
                if role.perms & known_perms() != current.perms & known_perms() {
                    return Err(
                        "les permissions du rôle Propriétaire ne changent pas".into()
                    );
                }
            } else {
                let wanted = role.perms & known_perms();
                if wanted & NOT_FOR_EVERYONE != 0 {
                    return Err(
                        "ces permissions ne s'accordent pas à tout le monde".into()
                    );
                }
                current.perms = wanted;
            }
            current.color = role.color;
        } else {
            current.name = name;
            current.color = role.color;
            current.rank = role.rank.min(MAX_RANK);
            current.perms = role.perms & known_perms();
        }
        self.save(&inner);
        Ok(())
    }

    /// Supprime un rôle et le renvoie, pour que l'appelant puisse le nommer
    /// dans le journal d'audit.
    ///
    /// **L'appelant doit ensuite balayer les comptes** (`Accounts::
    /// remove_role`) et les restrictions de salons : ce module ne connaît que
    /// les rôles, il ne peut pas retirer ce qu'il ignore. Un identifiant
    /// oublié dans un compte reste sans effet — `perms_of` ignore les
    /// inconnus — mais il ressort tel quel dans la liste des membres.
    pub fn delete(&self, id: RoleId) -> Result<RoleInfo, String> {
        let mut inner = self.inner.lock().unwrap();
        let Some(pos) = inner.roles.iter().position(|r| r.id == id) else {
            return Err("rôle inconnu".into());
        };
        if inner.roles[pos].system {
            return Err("un rôle du serveur ne se supprime pas".into());
        }
        let removed = inner.roles.remove(pos);
        // `next_id` ne recule pas : l'identifiant libéré reste mort.
        self.save(&inner);
        tracing::info!("rôle supprimé : {} (id {})", removed.name, removed.id);
        Ok(removed)
    }

    fn save(&self, inner: &RolesFile) {
        match serde_json::to_string_pretty(inner) {
            Ok(json) => {
                // Écriture atomique : une coupure en pleine sauvegarde
                // laisserait sinon un `roles.json` à zéro octet, et le
                // serveur refuserait de redémarrer.
                if let Err(e) = crate::store::write_atomic(&self.path, json.as_bytes()) {
                    tracing::error!("sauvegarde des rôles impossible : {e}");
                }
            }
            Err(e) => tracing::error!("sérialisation des rôles impossible : {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dossier de travail jetable, propre à chaque test.
    fn scratch(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("ki-chat-roles-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    #[test]
    fn first_start_creates_the_three_roles_and_keeps_them() {
        let dir = scratch("initial");
        let roles = Roles::open(&dir).unwrap();
        let listed = roles.list();
        // Rang décroissant : le propriétaire d'abord, @everyone en dernier.
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].id, ROLE_OWNER);
        assert_eq!(listed[1].name, "Modérateur");
        assert_eq!(listed[2].id, ROLE_EVERYONE);
        assert_eq!(listed[2].perms, perm::DEFAULT);
        assert!(listed[0].system && listed[2].system);
        // Le modérateur est fait pour être remanié : rien ne le protège.
        assert!(!listed[1].system);

        // Le fichier est écrit dès le départ, et relu tel quel.
        let reread = Roles::open(&dir).unwrap();
        assert_eq!(reread.list(), listed);
    }

    #[test]
    fn perms_are_the_union_of_the_roles_plus_everyone() {
        let roles = Roles::open(&scratch("perms")).unwrap();
        // Sans aucun rôle, @everyone s'applique quand même : sinon un membre
        // ordinaire ne pourrait ni lire ni écrire.
        assert_eq!(roles.perms_of(&[]), perm::DEFAULT);

        let a = roles.create("Recruteur", None, 10, perm::CREATE_INVITE).unwrap();
        let b = roles.create("Archiviste", None, 20, perm::VIEW_AUDIT_LOG).unwrap();
        let held = roles.perms_of(&[a.id, b.id]);
        assert!(perm::has(held, perm::CREATE_INVITE));
        assert!(perm::has(held, perm::VIEW_AUDIT_LOG));
        // L'union n'oublie pas @everyone au passage.
        assert!(perm::has(held, perm::SEND_MESSAGE));
        assert!(!perm::has(held, perm::BAN));

        // ADMINISTRATOR ouvre tout, sans que le bit soit posé.
        assert!(perm::has(roles.perms_of(&[ROLE_OWNER]), perm::BAN));
        // Un rôle supprimé — ou jamais existé — n'empêche pas de se connecter.
        assert_eq!(roles.perms_of(&[9999]), perm::DEFAULT);
        assert_eq!(roles.rank_of(&[9999]), 0);
    }

    #[test]
    fn colour_comes_from_the_highest_role_that_has_one() {
        let roles = Roles::open(&scratch("couleur")).unwrap();
        let bas = roles.create("Vétéran", Some(0x00_ff_00), 10, 0).unwrap();
        // Plus haut placé, mais sans couleur : il ne doit rien masquer.
        let haut = roles.create("Renfort", None, 50, 0).unwrap();

        assert_eq!(roles.color_of(&[bas.id, haut.id]), Some(0x00_ff_00));
        assert_eq!(roles.rank_of(&[bas.id, haut.id]), 50);
        // Personne n'impose de couleur par défaut.
        assert_eq!(roles.color_of(&[]), None);

        // Un rôle coloré au-dessus reprend la main.
        let chef = roles.create("Chef", Some(0xff_00_00), 90, 0).unwrap();
        assert_eq!(roles.color_of(&[bas.id, haut.id, chef.id]), Some(0xff_00_00));
    }

    #[test]
    fn system_roles_refuse_deletion_renaming_and_rank_changes() {
        let roles = Roles::open(&scratch("systeme")).unwrap();
        assert!(roles.delete(ROLE_OWNER).is_err());
        assert!(roles.delete(ROLE_EVERYONE).is_err());

        let mut owner = roles.get(ROLE_OWNER).unwrap();
        owner.name = "Chef suprême".into();
        assert!(roles.edit(owner.clone()).is_err());

        // Le retrait d'ADMINISTRATOR est le cas qui rendrait le serveur
        // ingouvernable : plus personne pour le rendre au propriétaire.
        let mut owner = roles.get(ROLE_OWNER).unwrap();
        owner.perms = perm::DEFAULT;
        assert!(roles.edit(owner).is_err());

        let mut owner = roles.get(ROLE_OWNER).unwrap();
        owner.rank = 5;
        assert!(roles.edit(owner).is_err());

        // Seule la couleur passe.
        let mut owner = roles.get(ROLE_OWNER).unwrap();
        owner.color = Some(0xff_a5_00);
        roles.edit(owner).unwrap();
        assert_eq!(roles.get(ROLE_OWNER).unwrap().color, Some(0xff_a5_00));
        assert_eq!(roles.get(ROLE_OWNER).unwrap().perms, perm::ADMINISTRATOR);
    }

    /// Les permissions d'`@everyone`, elles, doivent se régler : c'est le seul
    /// moyen de retirer un droit à tout le monde, puisque les permissions sont
    /// une union. Sans cela, les cases « Écrire », « Rejoindre le vocal » et
    /// « Partager des fichiers » de l'éditeur ne peuvent rien produire.
    #[test]
    fn everyone_permissions_can_be_tightened_unlike_the_owners() {
        let roles = Roles::open(&scratch("everyone")).unwrap();
        assert!(perm::has(roles.perms_of(&[]), perm::SEND_MESSAGE));

        // On retire le droit d'écrire à tout le monde.
        let mut everyone = roles.get(ROLE_EVERYONE).unwrap();
        everyone.perms = perm::DEFAULT & !perm::SEND_MESSAGE;
        roles.edit(everyone).unwrap();
        assert!(!perm::has(roles.perms_of(&[]), perm::SEND_MESSAGE));
        // Le reste de `DEFAULT` n'a pas bougé.
        assert!(perm::has(roles.perms_of(&[]), perm::CONNECT_VOICE));

        // ...puis on le rend par un rôle, à qui doit l'avoir.
        let parle = roles.create("Membre", None, 10, perm::SEND_MESSAGE).unwrap();
        assert!(perm::has(roles.perms_of(&[parle.id]), perm::SEND_MESSAGE));

        // En revanche, on n'accorde pas à tout le monde ce qui ne sert qu'à
        // distinguer une autorité d'une autre : le serveur se retrouverait à
        // plat, et personne ne pourrait revenir en arrière — @everyone est au
        // rang zéro, et l'on n'édite qu'un rôle strictement sous son rang.
        for interdit in [perm::ADMINISTRATOR, perm::BAN, perm::KICK, perm::MANAGE_ROLES] {
            let mut everyone = roles.get(ROLE_EVERYONE).unwrap();
            everyone.perms = perm::DEFAULT | interdit;
            assert!(
                roles.edit(everyone).is_err(),
                "{interdit:#x} n'aurait pas dû être accordé à tout le monde"
            );
        }
        // ...et le réglage précédent n'a pas bougé au passage.
        assert!(!perm::has(roles.perms_of(&[]), perm::SEND_MESSAGE));
        assert!(!perm::has(roles.perms_of(&[]), perm::ADMINISTRATOR));

        // Le nom et le rang restent verrouillés, eux.
        let mut everyone = roles.get(ROLE_EVERYONE).unwrap();
        everyone.name = "@tout-le-monde".into();
        assert!(roles.edit(everyone).is_err());
        let mut everyone = roles.get(ROLE_EVERYONE).unwrap();
        everyone.rank = 50;
        assert!(roles.edit(everyone).is_err());
        // Et il reste indestructible.
        assert!(roles.delete(ROLE_EVERYONE).is_err());
    }

    #[test]
    fn next_id_never_goes_back_after_a_deletion() {
        let dir = scratch("ids");
        let first = {
            let roles = Roles::open(&dir).unwrap();
            let first = roles.create("Éphémère", None, 5, 0).unwrap();
            assert_eq!(first.id, 4);
            let deleted = roles.delete(first.id).unwrap();
            assert_eq!(deleted.name, "Éphémère");
            // L'identifiant libéré reste mort : le suivant passe outre.
            assert_eq!(roles.create("Suivant", None, 5, 0).unwrap().id, 5);
            first
        };
        // Et le compteur survit au redémarrage, sinon un rôle recréé
        // hériterait des attributions de l'ancien.
        let roles = Roles::open(&dir).unwrap();
        assert_eq!(roles.create("Encore", None, 5, 0).unwrap().id, 6);
        assert!(roles.get(first.id).is_none());
    }

    #[test]
    fn assigned_roles_are_cleaned_before_being_stored() {
        let roles = Roles::open(&scratch("sanitize")).unwrap();
        let r = roles.create("Recruteur", None, 10, 0).unwrap();
        // @everyone jamais stocké, doublons et inconnus écartés.
        assert_eq!(
            roles.sanitize(&[ROLE_EVERYONE, r.id, r.id, 9999, ROLE_OWNER]),
            vec![r.id, ROLE_OWNER]
        );
    }

    #[test]
    fn unknown_permission_bits_are_dropped_on_the_way_in() {
        let roles = Roles::open(&scratch("bits")).unwrap();
        // Un bit sans signification aujourd'hui en aurait peut-être une
        // demain : le stocker reviendrait à accorder une permission à retard.
        let r = roles.create("Bricolé", None, 10, perm::KICK | 1 << 40).unwrap();
        assert_eq!(r.perms, perm::KICK);
        // Un nom vide ou démesuré n'a pas sa place non plus.
        assert!(roles.create("   ", None, 10, 0).is_err());
        assert!(roles.create(&"x".repeat(MAX_ROLE_NAME + 1), None, 10, 0).is_err());
        // Le propriétaire reste seul au sommet.
        assert_eq!(roles.create("Ambitieux", None, u16::MAX, 0).unwrap().rank, MAX_RANK);
    }
}
