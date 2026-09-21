//! Le « dernier lu » de chaque membre dans chaque salon.
//!
//! C'est le serveur qui le tient, et non le client : lui seul sait ce qui a
//! été écrit pendant qu'on n'était pas là, et le repère suit d'un ordinateur
//! à l'autre. Un fichier à part, `lus.json`, plutôt que `users.json` : celui-ci
//! se réécrit en entier à chaque changement, or un repère de lecture bouge à
//! chaque clic de salon. On l'écrit donc **à retardement** — au plus une fois
//! toutes les [`DELAI_ECRITURE`], et à la déconnexion — par
//! [`crate::store::write_atomic`], comme tout état du serveur.
//!
//! Fichier absent : personne n'a rien lu, et c'est le serveur qui pose le
//! repère à la connexion ([`non_lus`]) — au niveau du dernier message du
//! salon, pas à zéro. Sans quoi le premier démarrage après la mise à jour
//! aurait montré mille non-lus à tout le monde dans chaque salon. Mais
//! cette indulgence ne vaut que pour un membre qui n'a encore **aucun**
//! repère : un membre déjà connu qui découvre un salon — créé pendant son
//! absence, ou qu'un nouveau rôle lui ouvre — y trouve tout ce qui s'y est
//! écrit, sinon « réunion demain 21 h » posté dans #annonces tout neuf
//! n'aurait fait de pastille chez personne.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ki_protocol::{ChannelId, ChatRecord, NonLuSalon, UserId};

use crate::history::History;

/// Au plus une écriture par ce laps de temps : trente personnes qui
/// changent de salon ne doivent pas faire tourner le disque.
pub const DELAI_ECRITURE: Duration = Duration::from_secs(5);

type Table = HashMap<UserId, HashMap<ChannelId, u64>>;

pub struct Lus {
    path: PathBuf,
    table: Mutex<Table>,
    /// Depuis quand il y a du neuf à écrire. `None` : le disque est à jour.
    sale_depuis: Mutex<Option<Instant>>,
}

impl Lus {
    pub fn open(data_dir: &str) -> Self {
        let path = PathBuf::from(data_dir).join("lus.json");
        let table = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Table>(&bytes) {
                Ok(t) => t,
                Err(e) => {
                    // Un fichier abîmé ne vaut pas un refus de démarrer : on
                    // repart de rien, et chacun se retrouve « à jour ».
                    tracing::warn!("lus.json illisible ({e}), repères de lecture remis à zéro");
                    Table::new()
                }
            },
            Err(_) => Table::new(),
        };
        Self {
            path,
            table: Mutex::new(table),
            sale_depuis: Mutex::new(None),
        }
    }

    /// Ce que `user` a lu, salon par salon.
    pub fn de(&self, user: UserId) -> HashMap<ChannelId, u64> {
        self.table
            .lock()
            .unwrap()
            .get(&user)
            .cloned()
            .unwrap_or_default()
    }

    /// Le repère de `user` dans `channel`, s'il en a un.
    #[cfg(test)]
    fn lu(&self, user: UserId, channel: ChannelId) -> Option<u64> {
        self.table
            .lock()
            .unwrap()
            .get(&user)
            .and_then(|s| s.get(&channel))
            .copied()
    }

    /// Pose le repère : `user` a lu `channel` jusqu'à `ts`. Un repère ne
    /// **recule** jamais — un `Lu` en retard, produit avant un autre plus
    /// récent, ne doit pas faire réapparaître des pastilles. Rend vrai si
    /// quelque chose a changé.
    pub fn marquer(&self, user: UserId, channel: ChannelId, ts: u64) -> bool {
        let change = {
            let mut table = self.table.lock().unwrap();
            let salons = table.entry(user).or_default();
            match salons.get(&channel) {
                Some(deja) if *deja >= ts => false,
                _ => {
                    salons.insert(channel, ts);
                    true
                }
            }
        };
        if change {
            let mut sale = self.sale_depuis.lock().unwrap();
            if sale.is_none() {
                *sale = Some(Instant::now());
            }
        }
        change
    }

    /// Le disque est-il en retard depuis assez longtemps pour l'écrire ?
    pub fn a_ecrire(&self, now: Instant) -> bool {
        self.sale_depuis
            .lock()
            .unwrap()
            .is_some_and(|depuis| now.duration_since(depuis) >= DELAI_ECRITURE)
    }

    /// Écrit la table si elle a changé depuis la dernière écriture. À appeler
    /// hors de la boucle asynchrone : c'est une écriture disque.
    pub fn ecrire_si_sale(&self) {
        // Le drapeau tombe **avant** l'écriture, et la table est copiée sous
        // son verrou puis relâchée : un `marquer` pendant l'écriture
        // relèvera le drapeau, et la prochaine passe l'emportera.
        if self.sale_depuis.lock().unwrap().take().is_none() {
            return;
        }
        let json = {
            let table = self.table.lock().unwrap();
            serde_json::to_vec(&*table)
        };
        match json {
            Ok(bytes) => {
                if let Err(e) = crate::store::write_atomic(&self.path, &bytes) {
                    tracing::error!("écriture de lus.json : {e}");
                    // Le neuf n'est pas sur le disque : on réessaiera.
                    *self.sale_depuis.lock().unwrap() = Some(Instant::now());
                }
            }
            Err(e) => tracing::error!("sérialisation de lus.json : {e}"),
        }
    }
}

/// Ce que `user` n'a pas lu, salon par salon, parmi `salons` (les salons
/// textuels qu'il voit) — et le repère posé là où il n'en avait pas.
///
/// Un salon sans repère est tenu pour lu jusqu'à son dernier message
/// **seulement** si le membre n'a encore aucune table : première connexion
/// depuis que le serveur tient les lus, ou compte neuf. C'est ce qui évite
/// mille non-lus par salon pour tout le monde au premier démarrage après la
/// mise à jour. Un membre déjà connu, lui, découvre le salon avec tout ce
/// qui s'y est écrit — plafonné à ce que le cache garde, mille messages.
/// Dans les deux cas le repère est posé : ce qu'on écrira ensuite comptera,
/// même s'il n'ouvre jamais le salon.
pub fn non_lus(
    lus: &Lus,
    history: &History,
    salons: impl IntoIterator<Item = ChannelId>,
    user: UserId,
    username: &str,
) -> Vec<NonLuSalon> {
    let table = lus.de(user);
    let premiere_fois = table.is_empty();
    salons
        .into_iter()
        .map(|channel| {
            let dernier_ts = match table.get(&channel) {
                Some(lu) => *lu,
                None => {
                    let repere = if premiere_fois {
                        history.dernier_ts(channel).unwrap_or(0)
                    } else {
                        0
                    };
                    lus.marquer(user, channel, repere);
                    repere
                }
            };
            let depuis = history.depuis(channel, dernier_ts);
            // Ses propres messages ne sont pas des non-lus — après une
            // reconnexion, on retrouve sinon ce qu'on vient d'écrire en
            // pastille. Le fil de jeu et les clips (identifiant 0) comptent :
            // c'est du nouveau, qu'il n'a pas vu.
            let autres: Vec<&ChatRecord> = depuis.iter().filter(|r| r.user_id != user).collect();
            NonLuSalon {
                channel,
                dernier_ts,
                non_lus: autres.len().min(u32::MAX as usize) as u32,
                // Comme chez le client : le serveur et le bot ne nomment
                // personne, seul un membre le fait.
                mention: autres
                    .iter()
                    .filter(|r| r.user_id != 0 && r.user_id != ki_protocol::MUSIQUE_ID)
                    .any(|r| mentionne(&r.text, username)),
            }
        })
        .collect()
}

/// `texte` nomme-t-il `pseudo` par un `@` ?
///
/// L'approximation du serveur : le découpeur du client (`markup`) n'est pas
/// partagé, on en reprend les règles qui comptent pour une pastille — le
/// pseudo entier, terminé sur une frontière de mot (`@marie` ne s'accroche
/// pas à `@mariette` ni à `@marie-claire`), casse ASCII ignorée, et rien de
/// ce qui est entre accents graves ne compte. Le client, lui, fait foi à
/// l'affichage — une pastille d'accent de trop n'est pas un drame.
pub fn mentionne(texte: &str, pseudo: &str) -> bool {
    if pseudo.is_empty() {
        return false;
    }
    // Les blocs ``` d'abord — un bloc jamais refermé court jusqu'à la fin,
    // comme chez le client — puis, ligne à ligne, les portions `code`.
    let mut reste = texte;
    loop {
        let (clair, suite) = match reste.find("```") {
            Some(debut) => match reste[debut + 3..].find("```") {
                Some(fin) => (&reste[..debut], &reste[debut + 3 + fin + 3..]),
                None => (&reste[..debut], ""),
            },
            None => (reste, ""),
        };
        if clair.lines().any(|l| ligne_mentionne(l, pseudo)) {
            return true;
        }
        if suite.is_empty() {
            return false;
        }
        reste = suite;
    }
}

fn ligne_mentionne(ligne: &str, pseudo: &str) -> bool {
    // Retire les portions `code` fermées ; un accent grave orphelin reste
    // du texte, comme chez le client.
    let mut reste = ligne;
    loop {
        let (clair, suite) = match reste.find('`') {
            Some(debut) => match reste[debut + 1..].find('`') {
                Some(fin) if fin > 0 => (&reste[..debut], &reste[debut + 1 + fin + 1..]),
                _ => (reste, ""),
            },
            None => (reste, ""),
        };
        if texte_mentionne(clair, pseudo) {
            return true;
        }
        if suite.is_empty() {
            return false;
        }
        reste = suite;
    }
}

fn texte_mentionne(texte: &str, pseudo: &str) -> bool {
    let mut depuis = 0;
    while let Some(pos) = texte[depuis..].find('@') {
        let apres = &texte[depuis + pos + 1..];
        if let Some(candidat) = apres.get(..pseudo.len()) {
            let fin_nette = apres[pseudo.len()..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_' && c != '-');
            if candidat.eq_ignore_ascii_case(pseudo) && fin_nette {
                return true;
            }
        }
        depuis += pos + 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dossier(nom: &str) -> String {
        let dir = std::env::temp_dir().join(format!("ki-lus-{nom}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    /// Un repère avance, ne recule pas, et se relit depuis le fichier.
    #[test]
    fn un_repere_avance_sans_reculer_et_survit_au_redemarrage() {
        let dir = dossier("repere");
        let lus = Lus::open(&dir);
        assert!(lus.de(1).is_empty(), "fichier absent : rien de lu");
        assert!(lus.marquer(1, 10, 500));
        assert!(!lus.marquer(1, 10, 400), "un Lu en retard ne recule pas");
        assert!(!lus.marquer(1, 10, 500), "le même repère n'est pas un changement");
        assert!(lus.marquer(1, 11, 7));
        assert!(lus.marquer(2, 10, 1));
        assert_eq!(lus.lu(1, 10), Some(500));
        assert_eq!(lus.de(1), HashMap::from([(10, 500), (11, 7)]));

        lus.ecrire_si_sale();
        let relu = Lus::open(&dir);
        assert_eq!(relu.de(1), HashMap::from([(10, 500), (11, 7)]));
        assert_eq!(relu.lu(2, 10), Some(1));
        assert_eq!(relu.lu(3, 10), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Le disque n'est touché qu'à retardement, et seulement s'il y a du
    /// neuf : trente changements de salon, une écriture.
    #[test]
    fn l_ecriture_attend_son_delai_et_ne_se_repete_pas_pour_rien() {
        let dir = dossier("delai");
        let lus = Lus::open(&dir);
        let t0 = Instant::now();
        assert!(!lus.a_ecrire(t0), "rien à écrire au départ");
        lus.marquer(1, 10, 5);
        lus.marquer(1, 10, 6);
        assert!(!lus.a_ecrire(t0), "tout juste marqué : trop tôt");
        assert!(lus.a_ecrire(t0 + DELAI_ECRITURE + Duration::from_millis(1)));
        lus.ecrire_si_sale();
        assert!(!lus.a_ecrire(t0 + Duration::from_secs(3600)), "écrit : plus rien à faire");
        let json = std::fs::read_to_string(PathBuf::from(&dir).join("lus.json")).unwrap();
        assert!(json.contains("\"10\":6"), "{json}");
        // Un fichier abîmé ne bloque pas le démarrage.
        std::fs::write(PathBuf::from(&dir).join("lus.json"), b"{pas du json").unwrap();
        assert!(Lus::open(&dir).de(1).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn salon(id: ChannelId) -> ki_protocol::ChannelInfo {
        ki_protocol::ChannelInfo {
            id,
            name: format!("salon-{id}"),
            kind: ki_protocol::ChannelKind::Text,
            position: 0,
            locked: false,
            allowed_roles: None,
        }
    }

    fn dit(user_id: UserId, ts: u64, text: &str) -> ChatRecord {
        ChatRecord {
            user_id,
            username: format!("u{user_id}"),
            text: text.into(),
            ts,
            ..Default::default()
        }
    }

    /// Un salon né pendant l'absence compte pour qui a déjà des repères ;
    /// seule la toute première table est posée « à jour ».
    #[test]
    fn un_salon_decouvert_compte_sauf_a_la_premiere_table() {
        let dir = dossier("decouverte");
        let lus = Lus::open(&dir);
        let history = History::open(&dir, &[salon(1), salon(2)]).unwrap();
        for ts in [10, 20, 30] {
            history.append(1, &dit(9, ts, "dans #général"));
        }
        // Première connexion de 5 : aucune table. Tout est tenu pour lu, et
        // le repère est posé au dernier message.
        let premiere = non_lus(&lus, &history, [1, 2], 5, "cinq");
        assert_eq!(premiere[0], NonLuSalon { channel: 1, dernier_ts: 30, non_lus: 0, mention: false });
        assert_eq!(premiere[1], NonLuSalon { channel: 2, dernier_ts: 0, non_lus: 0, mention: false });
        assert_eq!(lus.lu(5, 1), Some(30));
        assert_eq!(lus.lu(5, 2), Some(0), "un salon vide a son repère aussi");

        // Pendant qu'il est parti : l'admin crée #annonces et y poste,
        // 5 lui-même y écrit aussi (il ne compte pas), et 9 le nomme.
        history.open_channel(&dir, 3).unwrap();
        history.append(3, &dit(1, 100, "réunion demain 21 h"));
        history.append(3, &dit(5, 110, "ok"));
        history.append(3, &dit(9, 120, "@cinq tu viens ?"));
        history.append(1, &dit(9, 40, "encore un"));
        let retour = non_lus(&lus, &history, [1, 2, 3], 5, "cinq");
        assert_eq!(retour[0], NonLuSalon { channel: 1, dernier_ts: 30, non_lus: 1, mention: false });
        assert_eq!(
            retour[2],
            NonLuSalon { channel: 3, dernier_ts: 0, non_lus: 2, mention: true },
            "un salon inconnu de sa table : tout ce que les autres y ont écrit"
        );
        assert_eq!(lus.lu(5, 3), Some(0), "le repère est posé : la suite comptera");
        // Et il se relit tel quel à la connexion suivante.
        assert_eq!(non_lus(&lus, &history, [3], 5, "cinq")[0].non_lus, 2);
        // Un autre membre jamais vu, lui, arrive « à jour » partout.
        let neuf = non_lus(&lus, &history, [1, 3], 6, "six");
        assert!(neuf.iter().all(|s| s.non_lus == 0), "{neuf:?}");
        assert_eq!(lus.lu(6, 3), Some(120));
        drop(history);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// La reconnaissance d'une mention, avec les règles du client.
    #[test]
    fn une_mention_est_un_pseudo_entier_hors_du_code() {
        assert!(mentionne("@léa tu viens ?", "léa"));
        assert!(mentionne("salut @Lea !", "lea"));
        assert!(mentionne("(@lea)", "lea"));
        assert!(mentionne("hé\n@lea", "lea"));
        assert!(!mentionne("@leanne", "lea"), "un autre pseudo qui commence pareil");
        assert!(!mentionne("@lea-b", "lea"), "un pseudo composé");
        assert!(!mentionne("lea", "lea"), "sans arobase, ce n'est pas une mention");
        assert!(!mentionne("`@lea`", "lea"), "du code littéral");
        assert!(!mentionne("```\n@lea\n```", "lea"), "un bloc de code");
        assert!(!mentionne("```\n@lea", "lea"), "un bloc jamais refermé");
        assert!(mentionne("```x``` @lea", "lea"), "après le bloc, du texte");
        assert!(mentionne("`x` @lea `y`", "lea"));
        assert!(mentionne("` @lea", "lea"), "un accent grave orphelin est du texte");
        assert!(!mentionne("@kevin @paul", "lea"));
        assert!(!mentionne("@", "lea"));
        assert!(!mentionne("@lea", ""));
        // Le tronçon comparé peut tomber au milieu d'un caractère : pas de
        // panique, pas de mention.
        assert!(!mentionne("@é", "a"));
    }
}
