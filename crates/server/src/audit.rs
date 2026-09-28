//! Journal d'audit administratif : `data/audit.jsonl`.
//!
//! Même patron que [`crate::history`] — append-only, les N dernières entrées
//! gardées en mémoire pour les consultations, et l'écriture sur un **fil
//! dédié**. Ce qui y entre est tout ce qu'un administrateur pourrait avoir à
//! justifier plus tard : qui a banni qui et pourquoi, quel code d'invitation
//! a servi à créer quel compte, qui a changé l'identité du serveur.
//!
//! Le fichier est volontairement lisible à la main : une entrée par ligne,
//! un verbe stable non traduit, de quoi le lire avec `grep` sur un serveur
//! sans y déployer d'outil.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Mutex;

use ki_protocol::AuditRecord;

/// Entrées gardées en mémoire, donc consultables sans relire le fichier.
const MEM_CAP: usize = 500;

/// Au-delà, le fichier est mis de côté et un neuf le remplace. Sans cela il
/// grandirait indéfiniment sur un serveur de longue vie.
const ROTATE_BYTES: u64 = 8 * 1024 * 1024;

/// Archives conservées. Le journal est une pièce à conviction, donc on ne
/// tronque rien — mais garder toutes les archives revient au même problème
/// avec un pas de huit mégaoctets : un disque plein ne fait pas que perdre le
/// journal, il emporte l'historique, les sauvegardes de comptes, et le
/// serveur avec.
const ARCHIVES_GARDEES: usize = 5;

struct Inner {
    recent: VecDeque<AuditRecord>,
}

/// Ce que le fil d'écriture reçoit.
enum Ordre {
    Ecrire(AuditRecord),
    /// Répondre quand tout ce qui précède est écrit : voir
    /// [`Audit::attendre_ecritures`].
    Barriere(Sender<()>),
}

/// Journal d'audit.
///
/// L'écriture part sur un fil dédié. Elle était synchrone, sous mutex,
/// appelée depuis `handle_msg` — lui-même synchrone et exécuté sur un ouvrier
/// tokio : chaque bannissement, chaque invitation, chaque changement de rôle
/// bloquait donc un ouvrier le temps d'un `writeln!` sur le disque du VPS.
/// Le même ouvrier porte des tâches de relais vocal.
pub struct Audit {
    inner: Mutex<Inner>,
    /// `Option` pour pouvoir être lâché au `Drop` : c'est la fermeture du
    /// canal qui dit au fil d'écriture de finir sa file et de s'arrêter.
    writes: Option<Sender<Ordre>>,
    writer: Option<std::thread::JoinHandle<()>>,
}

impl Audit {
    pub fn open(data_dir: &str) -> anyhow::Result<Self> {
        let dir = PathBuf::from(data_dir);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("audit.jsonl");
        rotate_if_large(&path)?;

        let mut recent = VecDeque::with_capacity(MEM_CAP);
        if path.exists() {
            let reader = BufReader::new(File::open(&path)?);
            for line in reader.lines() {
                let Ok(line) = line else { continue };
                if let Ok(rec) = serde_json::from_str::<AuditRecord>(&line) {
                    if recent.len() == MEM_CAP {
                        recent.pop_front();
                    }
                    recent.push_back(rec);
                }
            }
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let ecrit = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

        let (writes, rx) = std::sync::mpsc::channel();
        let writer = std::thread::Builder::new()
            .name("ki-audit".into())
            .spawn(move || writer_loop(file, path, ecrit, rx))?;

        Ok(Self {
            inner: Mutex::new(Inner { recent }),
            writes: Some(writes),
            writer: Some(writer),
        })
    }

    /// Consigne une action. Ne renvoie rien et n'échoue jamais bruyamment :
    /// un journal en panne ne doit pas faire échouer la modération qu'il
    /// décrit. L'échec part dans les traces, depuis le fil d'écriture.
    ///
    /// Ne touche pas au disque. La mémoire glissante est mise à jour ici —
    /// c'est elle que le panneau d'administration consulte, elle doit donc
    /// être à jour immédiatement — et la ligne part sur le fil dédié.
    pub fn record(&self, action: &str, actor: &str, target: &str, detail: &str) {
        let rec = AuditRecord {
            ts: crate::state::now_millis(),
            action: action.to_string(),
            actor: actor.to_string(),
            target: target.to_string(),
            detail: tronquer(detail, DETAIL_MAX),
        };
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.recent.len() == MEM_CAP {
                inner.recent.pop_front();
            }
            inner.recent.push_back(rec.clone());
        }
        if let Some(writes) = &self.writes {
            let _ = writes.send(Ordre::Ecrire(rec));
        }
    }

    /// Attend que tout ce qui a été consigné jusqu'ici soit dans le fichier,
    /// au plus `au_plus`. Pour l'arrêt sur signal : le `Drop`, qui vide la
    /// file, ne court jamais quand le processus quitte par `exit` — la
    /// dernière action d'un modérateur avant un redéploiement se perdait.
    pub fn attendre_ecritures(&self, au_plus: std::time::Duration) {
        let (fait, attente) = std::sync::mpsc::channel();
        if let Some(writes) = &self.writes {
            let _ = writes.send(Ordre::Barriere(fait));
        }
        let _ = attente.recv_timeout(au_plus);
    }

    /// Les `limit` dernières entrées, de la plus récente à la plus ancienne —
    /// autant qu'il en tient dans une ligne de contrôle. Cinq cents entrées
    /// au détail long ne tenaient pas dans `MAX_LINE` : la réponse était
    /// jetée en silence, et le panneau du journal restait vide.
    pub fn recent(&self, limit: usize) -> Vec<AuditRecord> {
        let inner = self.inner.lock().unwrap();
        let mut place = REPONSE_MAX;
        inner
            .recent
            .iter()
            .rev()
            .take(limit)
            .take_while(|r| {
                // Une borne haute de ce que l'entrée pèse en JSON : chaque
                // caractère au pire échappé, plus l'enveloppe.
                let poids = 2 * (r.action.len() + r.actor.len() + r.target.len() + r.detail.len()) + 96;
                match place.checked_sub(poids) {
                    Some(reste) => {
                        place = reste;
                        true
                    }
                    None => false,
                }
            })
            .cloned()
            .collect()
    }
}

/// Caractères gardés du détail d'une entrée. Le détail recopiait tel quel ce
/// que l'action portait — une adresse de morceau, un nom de playlist, jusqu'à
/// 160 Kio : trois cents messages suffisaient alors à faire tourner, donc à
/// effacer, tout le journal, juste après le geste qu'on voulait cacher.
const DETAIL_MAX: usize = 300;

/// Ce que la réponse au panneau peut peser, en laissant de la marge à
/// l'enveloppe du message sous `MAX_LINE`.
const REPONSE_MAX: usize = ki_protocol::MAX_LINE - 16 * 1024;

/// `texte` ramené à `max` caractères, avec une ellipse s'il a été coupé.
fn tronquer(texte: &str, max: usize) -> String {
    match texte.char_indices().nth(max) {
        Some((coupe, _)) => format!("{}…", &texte[..coupe]),
        None => texte.to_string(),
    }
}

impl Drop for Audit {
    /// Lâcher l'émetteur ferme le canal ; le fil écrit ce qui reste en file
    /// avant de s'arrêter. Sans cette attente, la dernière action d'un
    /// administrateur — juste avant un arrêt propre — n'atteindrait jamais le
    /// fichier, ce qu'un journal d'audit ne peut pas se permettre.
    fn drop(&mut self) {
        self.writes.take();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

/// Le fil d'écriture : sérialise, écrit, et fait tourner le fichier quand il
/// a trop grossi.
fn writer_loop(mut file: File, path: PathBuf, mut ecrit: u64, rx: Receiver<Ordre>) {
    while let Ok(ordre) = rx.recv() {
        let rec = match ordre {
            Ordre::Ecrire(rec) => rec,
            // Tout ce qui précède la barrière est écrit : on le dit.
            Ordre::Barriere(fait) => {
                let _ = fait.send(());
                continue;
            }
        };
        let line = match serde_json::to_string(&rec) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("sérialisation d'une entrée d'audit impossible : {e}");
                continue;
            }
        };
        if let Err(e) = writeln!(file, "{line}") {
            tracing::error!("écriture du journal d'audit impossible : {e}");
            continue;
        }
        ecrit += line.len() as u64 + 1;

        // La taille est vérifiée **ici**, et plus seulement au démarrage. Un
        // serveur qui ne redémarre pas — c'est le but — ne passait jamais par
        // la rotation : le journal grandissait sans borne jusqu'à remplir le
        // disque.
        if ecrit >= ROTATE_BYTES {
            ecrit = 0;
            if let Err(e) = rotate(&path) {
                // Ne pas réessayer à chaque ligne : le disque est
                // probablement plein, et l'on inonderait les traces.
                tracing::error!("rotation du journal d'audit impossible : {e}");
                continue;
            }
            match OpenOptions::new().create(true).append(true).open(&path) {
                Ok(neuf) => {
                    file = neuf;
                    purger_archives(&path);
                }
                // L'archive est en place mais le fichier neuf ne s'ouvre pas :
                // le descripteur courant pointe désormais sur l'archive, et
                // l'on continue d'y écrire. Rien n'est perdu, et le prochain
                // démarrage repart propre.
                Err(e) => tracing::error!("réouverture du journal d'audit impossible : {e}"),
            }
        }
    }
}

/// Met le journal de côté. On garde le fichier plutôt que de le tronquer :
/// c'est une pièce à conviction, pas un cache.
fn rotate(path: &Path) -> std::io::Result<()> {
    let stamp = crate::state::now_millis();
    std::fs::rename(path, path.with_file_name(format!("audit-{stamp}.jsonl")))
}

/// Ne garde que les [`ARCHIVES_GARDEES`] archives les plus récentes.
///
/// L'horodatage est en millisecondes et de largeur fixe pour les siècles à
/// venir : l'ordre alphabétique des noms est donc l'ordre chronologique. On
/// n'interroge pas le système de fichiers sur les dates, qu'une copie de
/// volume rendrait de toute façon fausses.
fn purger_archives(path: &Path) {
    let Some(dir) = path.parent() else { return };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut archives: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("audit-") && n.ends_with(".jsonl"))
        })
        .collect();
    if archives.len() <= ARCHIVES_GARDEES {
        return;
    }
    archives.sort();
    for vieille in &archives[..archives.len() - ARCHIVES_GARDEES] {
        if let Err(e) = std::fs::remove_file(vieille) {
            tracing::warn!("archive d'audit {} non effacée : {e}", vieille.display());
        }
    }
}

/// Rotation au démarrage. Reste utile malgré celle du fil d'écriture : un
/// journal hérité d'une version qui ne pivotait qu'ici peut dépasser la borne
/// dès l'ouverture.
fn rotate_if_large(path: &Path) -> anyhow::Result<()> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(());
    };
    if meta.len() < ROTATE_BYTES {
        return Ok(());
    }
    if let Err(e) = rotate(path) {
        tracing::error!("rotation du journal d'audit impossible : {e}");
    }
    purger_archives(path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(nom: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ki-audit-{}-{nom}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Les entrées ressortent de la plus récente à la plus ancienne, et
    /// survivent à une réouverture du fichier.
    ///
    /// L'écriture étant partie sur un fil, ce test vérifie aussi que le
    /// `Drop` attend bien qu'elle ait eu lieu : sans cette attente, la
    /// réouverture ci-dessous ne trouverait rien.
    #[test]
    fn records_are_persisted_newest_first() {
        let dir = scratch("ordre");
        let path = dir.to_str().unwrap();

        {
            let audit = Audit::open(path).unwrap();
            audit.record("invite.create", "kevin", "", "ki-abc");
            audit.record("member.ban", "kevin", "marie", "spam");
        }

        let audit = Audit::open(path).unwrap();
        let records = audit.recent(10);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].action, "member.ban");
        assert_eq!(records[0].target, "marie");
        assert_eq!(records[0].detail, "spam");
        assert_eq!(records[1].action, "invite.create");

        // La borne est respectée, et c'est bien le plus récent qui reste.
        let one = audit.recent(1);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].action, "member.ban");

        drop(audit);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Une action consignée est lisible tout de suite, sans attendre que le
    /// fil d'écriture ait fait son travail : le panneau d'administration
    /// affiche la mémoire glissante, pas le fichier.
    #[test]
    fn une_action_est_visible_avant_d_avoir_touche_le_disque() {
        let dir = scratch("immediat");
        let audit = Audit::open(dir.to_str().unwrap()).unwrap();
        audit.record("member.kick", "kevin", "marie", "");
        assert_eq!(audit.recent(1)[0].action, "member.kick");
        drop(audit);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Les archives ne s'accumulent pas sans fin. Un disque plein fait tomber
    /// bien plus que le journal.
    #[test]
    fn les_vieilles_archives_sont_purgees() {
        let dir = scratch("purge");
        let journal = dir.join("audit.jsonl");
        std::fs::write(&journal, b"").unwrap();
        // Horodatages de largeur fixe : l'ordre alphabétique est l'ordre du
        // temps, ce dont dépend la purge.
        for i in 0..ARCHIVES_GARDEES + 3 {
            std::fs::write(dir.join(format!("audit-17000000000{i:02}.jsonl")), b"x").unwrap();
        }
        // Un fichier qui n'est pas une archive ne doit pas être emporté.
        std::fs::write(dir.join("users.json"), b"{}").unwrap();

        purger_archives(&journal);

        let mut restantes: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("audit-"))
            .collect();
        restantes.sort();

        // Ce sont les plus récentes qui restent, et exactement celles-là.
        // La comparaison porte sur l'ensemble entier : chercher une
        // sous-chaîne se ferait piéger par le préfixe commun de
        // l'horodatage, qui contient déjà les motifs qu'on croit tester.
        let attendues: Vec<String> = (3..ARCHIVES_GARDEES + 3)
            .map(|i| format!("audit-17000000000{i:02}.jsonl"))
            .collect();
        assert_eq!(restantes, attendues);
        assert!(
            dir.join("users.json").exists(),
            "seules les archives sont visées"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Un détail démesuré — une adresse de morceau de 160 Kio — ne passe
    /// plus tel quel : il est coupé, sans couper un caractère en deux.
    #[test]
    fn un_detail_demesure_est_tronque() {
        let dir = scratch("tronque");
        let audit = Audit::open(dir.to_str().unwrap()).unwrap();
        audit.record("musique.ajouter", "kevin", "", &"é".repeat(160 * 1024));
        let detail = audit.recent(1).remove(0).detail;
        assert_eq!(detail.chars().count(), DETAIL_MAX + 1);
        assert!(detail.ends_with('…'));
        drop(audit);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Cinq cents entrées au détail maximal tiennent toujours dans une ligne
    /// de contrôle : le panneau du journal ne peut plus rester vide.
    #[test]
    fn la_reponse_au_panneau_tient_dans_une_ligne() {
        let dir = scratch("ligne");
        let audit = Audit::open(dir.to_str().unwrap()).unwrap();
        for _ in 0..MEM_CAP {
            audit.record("musique.ajouter", "kevin", "marie", &"\"é".repeat(400));
        }
        let records = audit.recent(MEM_CAP);
        assert!(!records.is_empty());
        let ligne = crate::state::encode(&ki_protocol::ServerMsg::AuditLog { records });
        assert!(ligne.is_some(), "la réponse doit tenir sous MAX_LINE");
        drop(audit);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
