//! Salons persistés dans `data/channels.json` : nom, nature, ordre
//! d'affichage et restriction par rôles.
//!
//! La liste était câblée dans `AppState::new` ; la rendre modifiable depuis
//! l'interface impose de la garder sur disque. Le point sensible n'est pas la
//! sauvegarde, c'est l'**allocation des identifiants** : l'historique texte
//! vit dans `data/channel-{id}.jsonl`, si bien qu'un salon neuf portant le
//! numéro d'un ancien hériterait de ses conversations. La fuite serait
//! silencieuse et irrattrapable une fois les messages affichés.
//!
//! `next_id` ne recule donc jamais, et il est reconstruit au démarrage en
//! regardant aussi ce que **contient le dossier**, pas seulement ce que dit
//! le fichier : `channels.json` peut avoir été effacé ou restauré depuis une
//! vieille sauvegarde, alors que les journaux, eux, sont toujours là.
//!
//! Le verrou vocal (`locked`) n'apparaît pas ici : il est éphémère, vit en
//! mémoire dans `AppState`, et n'a rien à faire dans un fichier qui survit au
//! redémarrage.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Context;
use ki_protocol::{ChannelId, ChannelInfo, ChannelKind, RoleId};
use serde::{Deserialize, Serialize};

/// Longueur maximale d'un nom de salon. Il occupe une ligne de la barre
/// latérale de tout le monde : au-delà, il ne repousse pas seulement le sien.
const MAX_NAME: usize = 32;

/// Plancher de `next_id` : les six salons d'origine vont jusqu'à 103, et
/// leurs journaux existent déjà sur les serveurs en service.
const FIRST_FREE_ID: ChannelId = 104;

#[derive(Serialize, Deserialize)]
struct ChannelsFile {
    /// Prochain identifiant libre. Recalculé à la hausse au démarrage, jamais
    /// à la baisse.
    next_id: ChannelId,
    channels: Vec<StoredChannel>,
}

#[derive(Serialize, Deserialize, Clone)]
struct StoredChannel {
    id: ChannelId,
    name: String,
    #[serde(default)]
    kind: ChannelKind,
    /// Ordre d'affichage. Toujours compacté en 0..n après chaque changement :
    /// deux salons ne partagent jamais une position.
    #[serde(default)]
    position: u32,
    /// `None` = visible par tous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    allowed_roles: Option<Vec<RoleId>>,
}

impl StoredChannel {
    /// `locked` est toujours faux ici : l'état du verrou vocal est tenu
    /// ailleurs, et c'est à l'appelant de le poser avant d'envoyer la liste.
    fn info(&self) -> ChannelInfo {
        ChannelInfo {
            id: self.id,
            name: self.name.clone(),
            kind: self.kind,
            position: self.position,
            locked: false,
            allowed_roles: self.allowed_roles.clone(),
        }
    }
}

pub struct Channels {
    path: PathBuf,
    inner: Mutex<ChannelsFile>,
}

impl Channels {
    /// Charge `data/channels.json`, ou installe les six salons d'origine.
    pub fn open(data_dir: &str) -> anyhow::Result<Self> {
        let dir = PathBuf::from(data_dir);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("channels.json");

        let (mut file, fresh) = match std::fs::read_to_string(&path) {
            // Un fichier illisible n'est PAS remplacé par les salons par
            // défaut : ce serait effacer la configuration de l'admin au pire
            // moment. On refuse de démarrer, il reste le fichier à réparer.
            Ok(json) => (
                serde_json::from_str::<ChannelsFile>(&json).context("lecture de channels.json")?,
                false,
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (default_channels(), true),
            Err(e) => return Err(e).context("lecture de channels.json"),
        };

        // Trois sources, on garde la plus haute. La troisième est celle qui
        // compte vraiment : elle tient même quand le fichier a disparu.
        let from_channels = file.channels.iter().map(|c| c.id).max().unwrap_or(0);
        let from_disk = highest_logged_id(&dir);
        file.next_id = file
            .next_id
            .max(from_channels.saturating_add(1))
            .max(from_disk.saturating_add(1))
            .max(FIRST_FREE_ID);
        compact_positions(&mut file.channels);

        let channels = Self { path, inner: Mutex::new(file) };
        // La migration est écrite tout de suite : sans ça, `next_id` ne serait
        // fixé sur disque qu'à la première modification, et un redémarrage
        // entre-temps repartirait d'un fichier absent.
        if fresh {
            channels.save(&channels.inner.lock().unwrap());
        }
        Ok(channels)
    }

    /// Tous les salons, dans l'ordre d'affichage.
    pub fn list(&self) -> Vec<ChannelInfo> {
        let inner = self.inner.lock().unwrap();
        let mut out: Vec<ChannelInfo> = inner.channels.iter().map(StoredChannel::info).collect();
        out.sort_by_key(|c| c.position);
        out
    }

    pub fn get(&self, id: ChannelId) -> Option<ChannelInfo> {
        let inner = self.inner.lock().unwrap();
        inner.channels.iter().find(|c| c.id == id).map(StoredChannel::info)
    }

    /// Vrai si le salon existe **et** est de la nature attendue : on ne parle
    /// pas dans un salon textuel, on n'écrit pas dans un vocal.
    pub fn is(&self, id: ChannelId, kind: ChannelKind) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.channels.iter().any(|c| c.id == id && c.kind == kind)
    }

    /// La liste telle que **cette personne** doit la voir.
    ///
    /// Les restrictions sont gommées pour qui ne gère pas les salons : savoir
    /// qu'un salon existe est déjà une information, connaître les rôles qui y
    /// donnent accès en est une autre.
    pub fn visible_to(&self, roles: &[RoleId], manage_channels: bool) -> Vec<ChannelInfo> {
        self.list()
            .into_iter()
            .filter(|c| can_view(c, roles, manage_channels))
            .map(|mut c| {
                if !manage_channels {
                    c.allowed_roles = None;
                }
                c
            })
            .collect()
    }

    /// Crée un salon et rend sa description complète.
    pub fn create(
        &self,
        name: &str,
        kind: ChannelKind,
        allowed_roles: Option<Vec<RoleId>>,
    ) -> Result<ChannelInfo, String> {
        let name = clean_name(name)?;
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_id;
        // Incrémenté avant toute chose : même si la sauvegarde échoue plus
        // bas, ce numéro est brûlé pour la durée du processus.
        inner.next_id = id.saturating_add(1);
        let position = inner.channels.len() as u32;
        inner.channels.push(StoredChannel {
            id,
            name,
            kind,
            position,
            allowed_roles: normalize_roles(allowed_roles),
        });
        compact_positions(&mut inner.channels);
        let created = inner
            .channels
            .iter()
            .find(|c| c.id == id)
            .expect("ajouté à l'instant")
            .info();
        self.save(&inner);
        Ok(created)
    }

    /// Remplacement complet, sauf la nature du salon et son verrou.
    ///
    /// La nature est figée : basculer un salon textuel en vocal rendrait son
    /// historique inatteignable sans le supprimer, et le ferait réapparaître
    /// au retour en arrière. Créer un nouveau salon coûte moins cher qu'une
    /// conversation qu'on croyait perdue.
    pub fn edit(&self, channel: ChannelInfo) -> Result<(), String> {
        let name = clean_name(&channel.name)?;
        let mut inner = self.inner.lock().unwrap();
        let Some(existing) = inner.channels.iter_mut().find(|c| c.id == channel.id) else {
            return Err("salon inconnu".into());
        };
        if existing.kind != channel.kind {
            return Err("la nature d'un salon ne se change pas".into());
        }
        existing.name = name;
        existing.allowed_roles = normalize_roles(channel.allowed_roles);

        // Déplacement à la place demandée, et pas seulement « avec ce
        // numéro » : une position déjà occupée serait tranchée par
        // l'identifiant, et le salon n'arriverait pas là où l'admin l'a lâché.
        compact_positions(&mut inner.channels);
        let at = inner
            .channels
            .iter()
            .position(|c| c.id == channel.id)
            .expect("trouvé à l'instant");
        let moved = inner.channels.remove(at);
        let dest = (channel.position as usize).min(inner.channels.len());
        inner.channels.insert(dest, moved);
        renumber(&mut inner.channels);

        self.save(&inner);
        Ok(())
    }

    /// Retire le salon de la liste et **archive** son journal.
    ///
    /// Le `.jsonl` n'est jamais effacé : une suppression par mégarde reste
    /// rattrapable à la main, et le fichier archivé continue de porter le
    /// numéro du salon, donc de le retenir au prochain démarrage.
    pub fn delete(&self, data_dir: &str, id: ChannelId) -> Result<ChannelInfo, String> {
        let removed = {
            let mut inner = self.inner.lock().unwrap();
            let Some(at) = inner.channels.iter().position(|c| c.id == id) else {
                return Err("salon inconnu".into());
            };
            let removed = inner.channels.remove(at).info();
            compact_positions(&mut inner.channels);
            self.save(&inner);
            removed
        };
        archive_log(Path::new(data_dir), id);
        Ok(removed)
    }

    /// Applique un nouvel ordre d'affichage.
    ///
    /// Exige une permutation exacte : une liste tronquée, envoyée par un
    /// client d'une autre version ou par une interface qui filtre les salons
    /// invisibles, effacerait de la liste tout ce qu'elle ne mentionne pas.
    pub fn reorder(&self, order: &[ChannelId]) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        // Tout est vérifié avant que quoi que ce soit ne bouge : un ordre
        // appliqué à moitié laisserait deux salons sur la même position,
        // c'est-à-dire un affichage que plus personne ne contrôle.
        if order.len() != inner.channels.len() {
            return Err("l'ordre doit citer tous les salons, une seule fois chacun".into());
        }
        for (rank, id) in order.iter().enumerate() {
            // Un doublon fait forcément un manquant en face, puisque les
            // longueurs sont égales : le refuser ici suffit.
            if order[..rank].contains(id) {
                return Err("l'ordre doit citer tous les salons, une seule fois chacun".into());
            }
            if !inner.channels.iter().any(|c| c.id == *id) {
                return Err("l'ordre cite un salon inconnu".into());
            }
        }
        for (rank, id) in order.iter().enumerate() {
            if let Some(channel) = inner.channels.iter_mut().find(|c| c.id == *id) {
                channel.position = rank as u32;
            }
        }
        compact_positions(&mut inner.channels);
        self.save(&inner);
        Ok(())
    }

    /// Retire un rôle supprimé de toutes les restrictions. Rend vrai si
    /// quelque chose a changé, de quoi décider s'il faut repousser la liste.
    ///
    /// Sans ce nettoyage, un salon resterait réservé à un rôle que plus
    /// personne ne peut porter : invisible pour tous, sauf pour qui gère les
    /// salons.
    pub fn forget_role(&self, role: RoleId) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let mut changed = false;
        for channel in inner.channels.iter_mut() {
            let Some(roles) = &mut channel.allowed_roles else { continue };
            let before = roles.len();
            roles.retain(|r| *r != role);
            changed |= roles.len() != before;
        }
        if changed {
            self.save(&inner);
        }
        changed
    }

    fn save(&self, inner: &ChannelsFile) {
        match serde_json::to_string_pretty(inner) {
            Ok(json) => {
                // Écriture atomique : une coupure de courant pendant la
                // sauvegarde laisserait sinon un `channels.json` vide, donc
                // un serveur qui refuse de démarrer.
                if let Err(e) = crate::store::write_atomic(&self.path, json.as_bytes()) {
                    tracing::error!("sauvegarde des salons impossible : {e}");
                }
            }
            Err(e) => tracing::error!("sérialisation des salons impossible : {e}"),
        }
    }
}

/// Qui a le droit de voir ce salon.
///
/// La permission de gérer les salons ouvre tout, y compris ce qui ne lui est
/// pas destiné : c'est la porte de secours. Sans elle, un mauvais clic sur
/// les rôles autorisés rend le salon invisible à tout le monde, y compris à
/// celui qui pourrait corriger l'erreur.
pub fn can_view(channel: &ChannelInfo, roles: &[RoleId], manage_channels: bool) -> bool {
    match &channel.allowed_roles {
        None => true,
        Some(allowed) => manage_channels || allowed.iter().any(|r| roles.contains(r)),
    }
}

/// Les six salons câblés jusqu'ici. Les identifiants sont repris tels quels :
/// l'historique déjà écrit sur disque porte ces numéros, et le retrouver est
/// tout l'intérêt de la migration.
fn default_channels() -> ChannelsFile {
    use ChannelKind::{Text, Voice};
    let wired = [
        (1, "général", Text),
        (2, "gaming", Text),
        (3, "afk", Text),
        (101, "Général", Voice),
        (102, "Gaming", Voice),
        (103, "AFK", Voice),
    ];
    ChannelsFile {
        next_id: FIRST_FREE_ID,
        channels: wired
            .iter()
            .enumerate()
            .map(|(position, (id, name, kind))| StoredChannel {
                id: *id,
                name: (*name).to_string(),
                kind: *kind,
                position: position as u32,
                allowed_roles: None,
            })
            .collect(),
    }
}

/// Le plus grand identifiant dont un journal traîne dans `data`, archives
/// comprises. 0 si le dossier n'en contient aucun.
///
/// C'est le garde-fou qui survit à la perte de `channels.json` : tant qu'un
/// `channel-N.jsonl` existe — même renommé en `.deleted-…` — le numéro N ne
/// peut plus être réattribué.
fn highest_logged_id(dir: &Path) -> ChannelId {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut highest = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // `channel-12.jsonl` comme `channel-12.deleted-1700000000000.jsonl` :
        // on ne lit que ce qui précède le premier point.
        if !name.ends_with(".jsonl") {
            continue;
        }
        let Some(rest) = name.strip_prefix("channel-") else { continue };
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        // Le nombre doit aller jusqu'au séparateur, sinon `channel-12bis` se
        // ferait lire comme le salon 12.
        if digits.is_empty() || !rest[digits.len()..].starts_with('.') {
            continue;
        }
        if let Ok(id) = digits.parse::<ChannelId>() {
            highest = highest.max(id);
        }
    }
    highest
}

/// Renomme le journal d'un salon supprimé au lieu de l'effacer.
fn archive_log(dir: &Path, id: ChannelId) {
    let live = dir.join(format!("channel-{id}.jsonl"));
    if !live.exists() {
        return;
    }
    let archived = dir.join(format!("channel-{id}.deleted-{}.jsonl", crate::state::now_millis()));
    // Un échec ne fait pas échouer la suppression : le salon a disparu de la
    // liste, et le journal resté en place garde de toute façon son numéro
    // réservé. On le signale, c'est tout.
    if let Err(e) = std::fs::rename(&live, &archived) {
        tracing::error!("archivage du journal du salon {id} impossible : {e}");
    }
}

/// Positions compactées en 0..n, dans l'ordre courant. Les égalités sont
/// tranchées par l'identifiant : sans ça, deux salons de même position
/// s'échangeraient de place à chaque relecture du fichier.
fn compact_positions(channels: &mut [StoredChannel]) {
    channels.sort_by_key(|c| (c.position, c.id));
    renumber(channels);
}

/// Numérote les positions selon l'ordre du vecteur, sans le retrier.
fn renumber(channels: &mut [StoredChannel]) {
    for (rank, channel) in channels.iter_mut().enumerate() {
        channel.position = rank as u32;
    }
}

/// Doublons retirés. `Some(liste vide)` est conservé tel quel : il veut dire
/// « personne, sauf ceux qui gèrent les salons », ce qui est un réglage
/// délibéré et non l'absence de restriction.
fn normalize_roles(roles: Option<Vec<RoleId>>) -> Option<Vec<RoleId>> {
    roles.map(|mut roles| {
        roles.sort_unstable();
        roles.dedup();
        roles
    })
}

/// Nom acceptable pour la barre latérale de tout le monde, ou la raison du
/// refus.
///
/// `safe_display` retire déjà les caractères de contrôle et les commandes
/// bidirectionnelles Unicode — celles qui font lire à l'écran autre chose que
/// ce qui est écrit. Il laisse en revanche passer sauts de ligne et
/// tabulations, sans objet sur une ligne de liste.
fn clean_name(name: &str) -> Result<String, String> {
    let flat: String =
        name.chars().map(|c| if c == '\n' || c == '\t' { ' ' } else { c }).collect();
    // Une borne au-delà du maximum : le nom trop long est refusé, pas tronqué
    // en douce, sinon l'admin ne voit pas ce qu'il a réellement créé.
    let cleaned = ki_protocol::safe_display(&flat, MAX_NAME + 1);
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return Err("nom de salon vide".into());
    }
    if cleaned.chars().count() > MAX_NAME {
        return Err(format!("nom de salon trop long ({MAX_NAME} caractères maximum)"));
    }
    Ok(cleaned.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dossier de travail jetable, propre à chaque test.
    fn scratch(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("ki-chat-channels-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn log_path(dir: &str, name: &str) -> PathBuf {
        PathBuf::from(dir).join(name)
    }

    /// Premier démarrage : les six salons câblés arrivent dans l'ordre, et le
    /// fichier est écrit sans attendre une première modification.
    #[test]
    fn the_first_start_installs_the_six_wired_channels() {
        let dir = scratch("migration");
        let channels = Channels::open(&dir).unwrap();
        let list = channels.list();
        let seen: Vec<(ChannelId, &str, u32)> =
            list.iter().map(|c| (c.id, c.name.as_str(), c.position)).collect();
        assert_eq!(
            seen,
            vec![
                (1, "général", 0),
                (2, "gaming", 1),
                (3, "afk", 2),
                (101, "Général", 3),
                (102, "Gaming", 4),
                (103, "AFK", 5),
            ]
        );
        assert!(list.iter().all(|c| c.allowed_roles.is_none()));
        assert!(channels.is(1, ChannelKind::Text));
        assert!(channels.is(101, ChannelKind::Voice));
        // La nature compte : écrire dans un vocal n'est pas permis.
        assert!(!channels.is(101, ChannelKind::Text));
        assert!(!channels.is(999, ChannelKind::Text));

        // Le fichier existe déjà, et un salon créé après redémarrage part
        // bien de 104.
        assert!(log_path(&dir, "channels.json").exists());
        let after_restart = Channels::open(&dir).unwrap();
        let neuf = after_restart.create("neuf", ChannelKind::Text, None).unwrap();
        assert_eq!(neuf.id, 104);
    }

    /// LE test : un journal traîne dans `data`, `channels.json` l'ignore
    /// complètement (sauvegarde ancienne, fichier effacé). Réattribuer son
    /// numéro donnerait au salon suivant les conversations de l'ancien.
    #[test]
    fn next_id_never_goes_back_when_a_log_outlives_channels_json() {
        let dir = scratch("monotone");
        // Un fichier qui ne connaît que les salons d'origine, et se croit à
        // 104 — exactement ce qu'aurait rendu une restauration.
        std::fs::write(
            log_path(&dir, "channels.json"),
            serde_json::to_string(&default_channels()).unwrap(),
        )
        .unwrap();
        // Deux journaux plus récents : un vivant, un archivé.
        std::fs::write(log_path(&dir, "channel-150.jsonl"), "{}\n").unwrap();
        std::fs::write(log_path(&dir, "channel-207.deleted-1700000000000.jsonl"), "{}\n").unwrap();
        // Pièges : ni l'un ni l'autre ne désigne un salon.
        std::fs::write(log_path(&dir, "channel-900bis.jsonl"), "").unwrap();
        std::fs::write(log_path(&dir, "channel-800.txt"), "").unwrap();

        let channels = Channels::open(&dir).unwrap();
        let created = channels.create("suite", ChannelKind::Text, None).unwrap();
        assert_eq!(created.id, 208, "l'archive du 207 réserve encore son numéro");

        // Et l'effacement pur et simple de channels.json ne rouvre pas le
        // trou : le dossier suffit à retenir les numéros.
        std::fs::remove_file(log_path(&dir, "channels.json")).unwrap();
        let repartie = Channels::open(&dir).unwrap();
        let created = repartie.create("suite", ChannelKind::Text, None).unwrap();
        assert_eq!(created.id, 208);
    }

    /// Supprimer un salon ne doit pas détruire ses messages : le journal est
    /// renommé, donc récupérable à la main, et son numéro reste réservé.
    #[test]
    fn deleting_a_channel_archives_its_log_instead_of_erasing_it() {
        let dir = scratch("archive");
        let channels = Channels::open(&dir).unwrap();
        let salon = channels.create("éphémère", ChannelKind::Text, None).unwrap();
        std::fs::write(log_path(&dir, &format!("channel-{}.jsonl", salon.id)), "{}\n").unwrap();

        let removed = channels.delete(&dir, salon.id).unwrap();
        assert_eq!(removed.id, salon.id);
        assert!(channels.get(salon.id).is_none());
        assert!(!log_path(&dir, &format!("channel-{}.jsonl", salon.id)).exists());

        // Un fichier, un seul, et il porte encore le numéro du salon.
        let archives: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(&format!("channel-{}.deleted-", salon.id)))
            .collect();
        assert_eq!(archives.len(), 1, "le journal doit être renommé, pas effacé");
        assert!(archives[0].ends_with(".jsonl"), "sinon le scan ne le verrait plus");
        assert_eq!(std::fs::read_to_string(log_path(&dir, &archives[0])).unwrap(), "{}\n");

        // Le salon suivant ne récupère pas le numéro libéré.
        let suivant = channels.create("suivant", ChannelKind::Text, None).unwrap();
        assert!(suivant.id > salon.id);
        // Y compris après redémarrage, fichier effacé compris.
        std::fs::remove_file(log_path(&dir, "channels.json")).unwrap();
        let apres = Channels::open(&dir).unwrap();
        assert!(apres.create("encore", ChannelKind::Text, None).unwrap().id > salon.id);

        // Supprimer deux fois, ou un salon inconnu, se refuse proprement.
        assert!(channels.delete(&dir, salon.id).is_err());
    }

    /// Une liste tronquée ferait disparaître les salons qu'elle ne cite pas.
    #[test]
    fn reorder_refuses_anything_but_an_exact_permutation() {
        let dir = scratch("ordre");
        let channels = Channels::open(&dir).unwrap();
        let ids: Vec<ChannelId> = channels.list().iter().map(|c| c.id).collect();

        // Tronquée : refusée, et l'ordre en place n'a pas bougé.
        assert!(channels.reorder(&ids[..3]).is_err());
        // Doublon en place d'un manquant : refusé aussi.
        let mut doublon = ids.clone();
        doublon[0] = doublon[1];
        assert!(channels.reorder(&doublon).is_err());
        // Bonne longueur mais un intrus : refusé.
        let mut intrus = ids.clone();
        intrus[0] = 4242;
        assert!(channels.reorder(&intrus).is_err());
        assert_eq!(channels.list().iter().map(|c| c.id).collect::<Vec<_>>(), ids);

        // Permutation exacte : acceptée, et l'ordre suit.
        let mut inverse = ids.clone();
        inverse.reverse();
        channels.reorder(&inverse).unwrap();
        assert_eq!(channels.list().iter().map(|c| c.id).collect::<Vec<_>>(), inverse);
        // Les positions restent compactes, sans trou ni égalité.
        assert_eq!(
            channels.list().iter().map(|c| c.position).collect::<Vec<_>>(),
            (0..ids.len() as u32).collect::<Vec<_>>()
        );

        // Et l'ordre survit au redémarrage.
        let apres = Channels::open(&dir).unwrap();
        assert_eq!(apres.list().iter().map(|c| c.id).collect::<Vec<_>>(), inverse);
    }

    /// Un salon réservé disparaît de la liste de qui n'a pas le rôle, mais
    /// jamais de celle de qui peut le corriger.
    #[test]
    fn a_restricted_channel_hides_from_outsiders_but_never_from_managers() {
        let dir = scratch("visibilite");
        let channels = Channels::open(&dir).unwrap();
        let prive = channels.create("staff", ChannelKind::Text, Some(vec![7])).unwrap();

        // Sans le rôle : le salon n'existe pas.
        let vu = channels.visible_to(&[3], false);
        assert!(!vu.iter().any(|c| c.id == prive.id));
        // Avec le rôle : il apparaît, mais la composition de la restriction
        // ne lui est pas envoyée.
        let vu = channels.visible_to(&[3, 7], false);
        let trouve = vu.iter().find(|c| c.id == prive.id).expect("le rôle 7 y donne accès");
        assert!(trouve.allowed_roles.is_none(), "un membre n'a pas à connaître les rôles listés");
        // Qui gère les salons voit tout, restriction comprise : c'est la
        // porte de secours contre le salon rendu orphelin par mégarde.
        let vu = channels.visible_to(&[], true);
        let trouve = vu.iter().find(|c| c.id == prive.id).expect("porte de secours");
        assert_eq!(trouve.allowed_roles.as_deref(), Some(&[7][..]));
        // Les salons ouverts restent visibles de tous.
        assert!(channels.visible_to(&[], false).iter().any(|c| c.id == 1));

        // Le rôle supprimé s'efface partout ; le salon devient alors le
        // domaine des seuls gestionnaires, pas celui de tout le monde.
        assert!(channels.forget_role(7));
        assert!(!channels.forget_role(7), "rien à refaire au second passage");
        assert_eq!(channels.get(prive.id).unwrap().allowed_roles.as_deref(), Some(&[][..]));
        assert!(!channels.visible_to(&[7], false).iter().any(|c| c.id == prive.id));
        assert!(channels.visible_to(&[], true).iter().any(|c| c.id == prive.id));
    }

    /// Le nom part dans la barre latérale de tout le monde : vide, trop long
    /// ou truqué, il est refusé plutôt que rafistolé en silence.
    #[test]
    fn names_are_cleaned_bounded_and_never_empty() {
        let dir = scratch("noms");
        let channels = Channels::open(&dir).unwrap();

        assert!(channels.create("   ", ChannelKind::Text, None).is_err());
        assert!(channels.create(&"x".repeat(MAX_NAME + 1), ChannelKind::Text, None).is_err());
        // Les commandes bidirectionnelles disparaissent : elles font lire à
        // l'écran autre chose que ce qui est écrit.
        let propre = channels.create("  sa\u{202e}lon\n2  ", ChannelKind::Text, None).unwrap();
        assert_eq!(propre.name, "salon 2");

        // Les mêmes règles à l'édition, et la nature ne se change pas.
        let mut modifie = propre.clone();
        modifie.name = String::new();
        assert!(channels.edit(modifie).is_err());
        let mut modifie = propre.clone();
        modifie.kind = ChannelKind::Voice;
        assert!(channels.edit(modifie).is_err());
        // Un identifiant inconnu ne crée pas de salon au passage.
        let mut fantome = propre.clone();
        fantome.id = 4242;
        assert!(channels.edit(fantome).is_err());

        // Édition acceptée : nom nettoyé, rôles dédoublonnés, ordre compacté.
        let mut modifie = propre.clone();
        modifie.name = " renommé ".into();
        modifie.allowed_roles = Some(vec![5, 5, 2]);
        modifie.position = 0;
        channels.edit(modifie).unwrap();
        let relu = channels.get(propre.id).unwrap();
        assert_eq!(relu.name, "renommé");
        assert_eq!(relu.allowed_roles.as_deref(), Some(&[2, 5][..]));
        assert_eq!(relu.position, 0);
        assert_eq!(channels.list()[0].id, propre.id);
    }
}
