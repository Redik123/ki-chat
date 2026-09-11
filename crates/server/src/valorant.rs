//! Le compte Riot des membres et leur fiche VALORANT, par HenrikDev.
//!
//! Un membre lie son Riot ID (« Pseudo#TAG ») ; le serveur le résout par
//! l'API HenrikDev et garde une fiche à jour — rang courant et pic,
//! derniers mouvements de RR, derniers matchs résumés — que les autres
//! consultent d'un clic droit. Tout passe par ici, et par ici seulement :
//! la clé d'API ne quitte jamais le serveur, et le client ne parle jamais
//! à HenrikDev.
//!
//! **Budget.** La clé « Basic » autorise trente requêtes par minute, pour
//! tout le groupe. Un fil unique sert les demandes l'une après l'autre et
//! s'arrête de lui-même à vingt par minute glissante — les dix restantes
//! sont la marge. Ouvrir une fiche ne coûte rien : elle vient du cache.
//! Seules une liaison (quatre requêtes) et un rafraîchissement (trois)
//! touchent l'API, et les rafraîchissements s'espacent d'une demi-heure
//! par membre, en ligne seulement.
//!
//! **Ce qu'on garde.** La ligne du membre dans chaque match — jamais celles
//! des neuf autres, qui ne sont pas du serveur. Deux fichiers sous
//! `data/valorant/` : `comptes.json` (qui a lié quoi) et `fiches.json`
//! (les fiches), écrits par [`crate::store::write_atomic`].
//!
//! **Sans clé**, tout est simplement absent : `KI_HENRIK_KEY` vide et
//! pas de `data/henrik.key` → le service dit non aux liaisons, les fiches
//! déjà en cache restent lisibles, rien ne casse.
//!
//! **Le fil de jeu.** Quand un membre sort d'une partie (sa présence passe
//! d'« en jeu » à autre chose), sa fiche est relue au bout de 75 s — le
//! temps que HenrikDev voie le match — et jusqu'à trois fois. Un match qui
//! n'a jamais été annoncé l'est dans le salon choisi par l'admin : le
//! résultat, sa ligne, ses RR. Les coéquipiers du groupe sont reconnus à
//! leur puuid dans le même match : leur fiche est relue aussitôt et
//! l'annonce les attend, deux minutes au plus, pour ne faire qu'un
//! message. Ce qui a été annoncé est noté dans `fil.json`, sinon chaque
//! redémarrage rejouerait la soirée.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ki_protocol::{FicheValorant, MatchEsport, MatchResume, PointRR, RangValorant, UserId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Requêtes tolérées par minute glissante — sous les trente de la clé.
const BUDGET_PAR_MINUTE: usize = 20;
/// Points d'historique et matchs gardés par fiche.
const HISTORIQUE_MAX: usize = 10;
const MATCHS_MAX: usize = 5;
const BASE: &str = "https://api.henrikdev.xyz";
/// Par match, les autres membres liés qui y jouaient.
type CoMembres = Vec<(String, Vec<UserId>)>;
/// Le pseudo sous lequel le serveur poste le fil de jeu.
pub const PSEUDO_DU_FIL: &str = "VALORANT";
/// Les modes annoncés ; les modes d'arcade feraient du bruit pour rien.
const MODES_ANNONCES: [&str; 4] = ["Compétitif", "Non classé", "Partie rapide", "Premier"];
/// Un match fini depuis plus longtemps que ça ne s'annonce plus : après
/// une panne, le fil ne rejoue pas la soirée.
const ANNONCE_AGE_MAX: Duration = Duration::from_secs(6 * 3600);
/// Combien de temps une annonce attend les coéquipiers du groupe.
const ANNONCE_ATTENTE: Duration = Duration::from_secs(120);
/// Après une fin de partie, HenrikDev met quelques minutes à voir le
/// match : la fiche est relue après ce délai, jusqu'à `RELANCES_MAX` fois.
const RELANCE_DELAI: Duration = Duration::from_secs(75);
const RELANCES_MAX: u32 = 3;
/// Identifiants de matchs gardés par membre dans `fil.json`.
const ANNONCES_GARDEES: usize = 30;
/// Le calendrier esport se relit toutes les heures, et l'on en garde
/// autant de matchs.
const ESPORTS_AGE: Duration = Duration::from_secs(3600);
const ESPORTS_MAX: usize = 20;

/// Le compte Riot lié à un membre, tel que HenrikDev l'a résolu.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompteRiot {
    pub nom: String,
    pub tag: String,
    pub puuid: String,
    /// L'affinité HenrikDev : eu, na, ap, kr, latam, br.
    pub region: String,
    /// pc ou console.
    pub plateforme: String,
    /// Liaison, en millisecondes Unix.
    pub depuis: u64,
}

impl CompteRiot {
    pub fn riot_id(&self) -> String {
        format!("{}#{}", self.nom, self.tag)
    }
}

/// Ce que le fil HenrikDev a à faire.
enum Travail {
    Lier {
        user_id: UserId,
        nom: String,
        tag: String,
    },
    /// `relance` : la énième relecture après une fin de partie, s'il s'agit
    /// de ça — pour savoir s'il faut relire encore.
    Rafraichir {
        user_id: UserId,
        relance: Option<u32>,
    },
    /// Le calendrier esport.
    Esports,
}

/// Ce que HenrikDev a coûté depuis le démarrage — lisible dans le résumé
/// des diagnostics.
#[derive(Default)]
struct Compteurs {
    requetes: AtomicU64,
    refus_429: AtomicU64,
    erreurs: AtomicU64,
    derniere_ms: AtomicU64,
}

/// La ligne d'un membre dans un match annoncé, avec ses RR après coup
/// quand c'est du classé : la variation, et le rang qui en résulte.
#[derive(Debug, Clone, PartialEq)]
pub struct LigneAnnonce {
    pub user_id: UserId,
    pub resume: MatchResume,
    pub rr: Option<(i32, RangValorant)>,
}

/// Un match à annoncer : les lignes des membres du groupe qui y étaient.
#[derive(Debug, Clone, PartialEq)]
pub struct Annonce {
    pub match_id: String,
    pub lignes: Vec<LigneAnnonce>,
}

/// Une annonce qui attend ses coéquipiers.
struct Attente {
    depuis: Instant,
    lignes: Vec<LigneAnnonce>,
    attendus: BTreeSet<UserId>,
}

/// Le fil de jeu : ce qui a été annoncé, ce qui attend, ce qui doit être
/// relu.
#[derive(Default)]
struct Fil {
    /// Les matchs déjà annoncés (ou connus à la liaison), par membre.
    annonces: Mutex<BTreeMap<UserId, VecDeque<String>>>,
    en_attente: Mutex<BTreeMap<String, Attente>>,
    /// Relectures programmées après une fin de partie : quand, et le
    /// nombre de relectures déjà faites.
    relances: Mutex<BTreeMap<UserId, (Instant, u32)>>,
}

/// Ce qu'il rapporte, à relayer aux clients.
pub enum Resultat {
    Liaison {
        user_id: UserId,
        ok: bool,
        message: String,
        riot_id: Option<String>,
    },
    Fiche {
        user_id: UserId,
    },
}

struct Etat {
    dossier: PathBuf,
    comptes: Mutex<BTreeMap<UserId, CompteRiot>>,
    fiches: Mutex<BTreeMap<UserId, FicheValorant>>,
    fil: Fil,
    compteurs: Arc<Compteurs>,
    /// Le calendrier esport : quand il a été lu (0 : jamais), et ce qu'il
    /// contient. `en_cours` évite deux lectures à la fois.
    esports: Mutex<(u64, Vec<MatchEsport>)>,
    esports_en_cours: std::sync::atomic::AtomicBool,
}

pub struct Valorant {
    etat: Arc<Etat>,
    /// `None` : pas de clé, pas de fil.
    travaux: Option<Sender<Travail>>,
    resultats: Mutex<Receiver<Resultat>>,
}

impl Valorant {
    /// Charge les comptes et fiches, cherche la clé, lance le fil si elle
    /// est là.
    pub fn open(data_dir: &str) -> Self {
        let dossier = PathBuf::from(data_dir).join("valorant");
        let comptes = lire(&dossier.join("comptes.json"));
        let fiches: BTreeMap<UserId, FicheValorant> = lire(&dossier.join("fiches.json"));
        // Sans fil.json (première fois), tout ce que les fiches contiennent
        // est réputé déjà annoncé : le fil commence aux parties à venir.
        let chemin_fil = dossier.join("fil.json");
        let annonces: BTreeMap<UserId, VecDeque<String>> = if chemin_fil.exists() {
            lire(&chemin_fil)
        } else {
            fiches
                .iter()
                .map(|(id, f)| (*id, f.matchs.iter().map(|m| m.id.clone()).collect()))
                .collect()
        };
        let etat = Arc::new(Etat {
            dossier,
            comptes: Mutex::new(comptes),
            fiches: Mutex::new(fiches),
            fil: Fil {
                annonces: Mutex::new(annonces),
                ..Default::default()
            },
            compteurs: Arc::default(),
            esports: Mutex::new((0, Vec::new())),
            esports_en_cours: std::sync::atomic::AtomicBool::new(false),
        });
        let (tx_res, rx_res) = mpsc::channel();
        let cle = cle_henrik(data_dir);
        let travaux = cle.map(|cle| {
            let (tx, rx) = mpsc::channel();
            let etat = Arc::clone(&etat);
            let travaux = tx.clone();
            std::thread::Builder::new()
                .name("ki-henrik".into())
                .spawn(move || fil(cle, etat, rx, tx_res, travaux))
                .expect("fil HenrikDev");
            tx
        });
        if travaux.is_some() {
            tracing::info!("VALORANT : clé HenrikDev trouvée, liaisons ouvertes");
        } else {
            tracing::info!("VALORANT : pas de clé HenrikDev (KI_HENRIK_KEY ou data/henrik.key), liaisons fermées");
        }
        Self {
            etat,
            travaux,
            resultats: Mutex::new(rx_res),
        }
    }

    pub fn riot_id(&self, user_id: UserId) -> Option<String> {
        self.etat
            .comptes
            .lock()
            .unwrap()
            .get(&user_id)
            .map(CompteRiot::riot_id)
    }

    pub fn rang(&self, user_id: UserId) -> Option<u8> {
        self.etat
            .fiches
            .lock()
            .unwrap()
            .get(&user_id)
            .map(|f| f.rang.tier)
    }

    pub fn fiche(&self, user_id: UserId) -> Option<FicheValorant> {
        self.etat.fiches.lock().unwrap().get(&user_id).cloned()
    }

    /// Toutes les fiches, pour la page de stats du groupe.
    pub fn toutes(&self) -> Vec<(UserId, FicheValorant)> {
        self.etat
            .fiches
            .lock()
            .unwrap()
            .iter()
            .map(|(id, f)| (*id, f.clone()))
            .collect()
    }

    /// Met la liaison en file. Refuse sans clé, ou si ce Riot ID est déjà
    /// celui d'un autre membre.
    pub fn lier(&self, user_id: UserId, nom: String, tag: String) -> Result<(), String> {
        let Some(travaux) = &self.travaux else {
            return Err("le serveur n'a pas de clé HenrikDev : demande à l'admin".into());
        };
        {
            let comptes = self.etat.comptes.lock().unwrap();
            let deja = comptes.iter().any(|(id, c)| {
                *id != user_id
                    && c.nom.eq_ignore_ascii_case(&nom)
                    && c.tag.eq_ignore_ascii_case(&tag)
            });
            if deja {
                return Err("ce compte Riot est déjà lié à un autre membre".into());
            }
        }
        travaux
            .send(Travail::Lier { user_id, nom, tag })
            .map_err(|_| "le service VALORANT est arrêté".to_string())
    }

    /// Retire compte, fiche et mémoire du fil. `false` s'il n'y avait rien.
    pub fn delier(&self, user_id: UserId) -> bool {
        let retire = self.etat.comptes.lock().unwrap().remove(&user_id).is_some();
        let fiche = self.etat.fiches.lock().unwrap().remove(&user_id).is_some();
        if self
            .etat
            .fil
            .annonces
            .lock()
            .unwrap()
            .remove(&user_id)
            .is_some()
        {
            self.etat.sauver_fil();
        }
        self.etat.fil.relances.lock().unwrap().remove(&user_id);
        if retire {
            self.etat.sauver_comptes();
        }
        if fiche {
            self.etat.sauver_fiches();
        }
        retire
    }

    /// Demande un rafraîchissement — sans clé ou sans compte, rien.
    pub fn rafraichir(&self, user_id: UserId) {
        if let Some(travaux) = &self.travaux {
            if self.etat.comptes.lock().unwrap().contains_key(&user_id) {
                let _ = travaux.send(Travail::Rafraichir {
                    user_id,
                    relance: None,
                });
            }
        }
    }

    /// Le calendrier esport tel qu'on l'a — vide tant qu'il n'a pas été lu.
    pub fn esports(&self) -> Vec<MatchEsport> {
        self.etat.esports.lock().unwrap().1.clone()
    }

    /// Relit le calendrier esport s'il a plus d'une heure (ou jamais été
    /// lu) et qu'aucune lecture n'est en cours.
    pub fn rafraichir_esports(&self) {
        let Some(travaux) = &self.travaux else { return };
        let perime = maintenant_ms().saturating_sub(self.etat.esports.lock().unwrap().0)
            > ESPORTS_AGE.as_millis() as u64;
        if perime && !self.etat.esports_en_cours.swap(true, Ordering::Relaxed) {
            let _ = travaux.send(Travail::Esports);
        }
    }

    /// L'état du service en une ligne, pour le résumé des diagnostics.
    pub fn compteurs_texte(&self) -> String {
        let c = &self.etat.compteurs;
        let derniere = match c.derniere_ms.load(Ordering::Relaxed) {
            0 => "jamais".to_string(),
            t => format!("il y a {} min", maintenant_ms().saturating_sub(t) / 60_000),
        };
        let attente = self.etat.fil.en_attente.lock().unwrap().len();
        format!(
            "VALORANT : clé HenrikDev {} · {} membres liés, {} fiches · requêtes depuis le démarrage : {} \
             (refus 429 : {}, erreurs : {}), dernière {} · annonces en attente : {}",
            if self.travaux.is_some() { "présente" } else { "absente" },
            self.etat.comptes.lock().unwrap().len(),
            self.etat.fiches.lock().unwrap().len(),
            c.requetes.load(Ordering::Relaxed),
            c.refus_429.load(Ordering::Relaxed),
            c.erreurs.load(Ordering::Relaxed),
            derniere,
            attente,
        )
    }

    /// Le membre sort d'une partie : sa fiche sera relue dans 75 s, puis
    /// encore si le match n'y est pas.
    pub fn fin_de_partie(&self, user_id: UserId) {
        if self.travaux.is_none() || !self.etat.comptes.lock().unwrap().contains_key(&user_id) {
            return;
        }
        self.etat
            .fil
            .relances
            .lock()
            .unwrap()
            .insert(user_id, (Instant::now() + RELANCE_DELAI, 0));
    }

    /// À appeler régulièrement : lance les relectures dues et rend les
    /// annonces prêtes — complètes, ou qui ont assez attendu.
    pub fn tick(&self) -> Vec<Annonce> {
        let Some(travaux) = &self.travaux else {
            return Vec::new();
        };
        let maintenant = Instant::now();
        let dues: Vec<(UserId, u32)> = {
            let mut relances = self.etat.fil.relances.lock().unwrap();
            let dues: Vec<_> = relances
                .iter()
                .filter(|(_, (quand, _))| *quand <= maintenant)
                .map(|(id, (_, n))| (*id, *n))
                .collect();
            for (id, _) in &dues {
                relances.remove(id);
            }
            dues
        };
        for (user_id, essais) in dues {
            let _ = travaux.send(Travail::Rafraichir {
                user_id,
                relance: Some(essais),
            });
        }
        self.etat.fil.pretes()
    }

    /// Parmi `en_ligne`, les membres liés dont la fiche a plus de
    /// `age_max`.
    pub fn a_rafraichir(&self, en_ligne: &[UserId], age_max: Duration) -> Vec<UserId> {
        if self.travaux.is_none() {
            return Vec::new();
        }
        let maintenant = maintenant_ms();
        let comptes = self.etat.comptes.lock().unwrap();
        let fiches = self.etat.fiches.lock().unwrap();
        en_ligne
            .iter()
            .copied()
            .filter(|id| comptes.contains_key(id))
            .filter(|id| {
                fiches
                    .get(id)
                    .is_none_or(|f| maintenant.saturating_sub(f.maj) > age_max.as_millis() as u64)
            })
            .collect()
    }

    /// Ce que le fil a fini depuis la dernière fois.
    pub fn resultats(&self) -> Vec<Resultat> {
        let rx = self.resultats.lock().unwrap();
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }
}

impl Etat {
    fn sauver_comptes(&self) {
        let comptes = self.comptes.lock().unwrap().clone();
        ecrire(&self.dossier.join("comptes.json"), &comptes);
    }

    fn sauver_fiches(&self) {
        let fiches = self.fiches.lock().unwrap().clone();
        ecrire(&self.dossier.join("fiches.json"), &fiches);
    }

    fn sauver_fil(&self) {
        let annonces = self.fil.annonces.lock().unwrap().clone();
        ecrire(&self.dossier.join("fil.json"), &annonces);
    }

    /// Les puuid des membres liés — pour reconnaître les coéquipiers du
    /// groupe dans un match.
    fn lies(&self) -> Vec<(UserId, String)> {
        self.comptes
            .lock()
            .unwrap()
            .iter()
            .map(|(id, c)| (*id, c.puuid.clone()))
            .collect()
    }
}

impl Fil {
    /// Tout ce que la fiche contient est réputé connu : à la liaison, le
    /// fil commence aux parties à venir.
    fn connaitre(&self, user_id: UserId, fiche: &FicheValorant) {
        let ids = fiche.matchs.iter().map(|m| m.id.clone()).collect();
        self.annonces.lock().unwrap().insert(user_id, ids);
    }

    /// Les matchs de la fiche jamais vus : notés, et mis en attente
    /// d'annonce s'ils s'annoncent (mode, âge). Les coéquipiers du groupe
    /// (`co` : match → membres) sont relus aussitôt et attendus. Rend le
    /// nombre de matchs nouveaux, annoncés ou non.
    fn nouveaux(
        &self,
        user_id: UserId,
        fiche: &FicheValorant,
        co: &[(String, Vec<UserId>)],
        travaux: &Sender<Travail>,
    ) -> usize {
        let mut n = 0;
        let maintenant = maintenant_ms();
        let mut annonces = self.annonces.lock().unwrap();
        let vus = annonces.entry(user_id).or_default();
        // Du plus ancien au plus récent : les annonces sortent dans l'ordre.
        for m in fiche.matchs.iter().rev() {
            if m.id.is_empty() || vus.contains(&m.id) {
                continue;
            }
            vus.push_back(m.id.clone());
            while vus.len() > ANNONCES_GARDEES {
                vus.pop_front();
            }
            n += 1;
            let fin = m.date + m.duree_s as u64 * 1000;
            let trop_vieux = maintenant.saturating_sub(fin) > ANNONCE_AGE_MAX.as_millis() as u64;
            if trop_vieux || !MODES_ANNONCES.contains(&m.mode.as_str()) {
                continue;
            }
            let rr = fiche
                .historique_rr
                .iter()
                .find(|p| p.match_id == m.id)
                .map(|p| {
                    (
                        p.delta,
                        RangValorant {
                            tier: p.tier,
                            rr: p.rr,
                            ..Default::default()
                        },
                    )
                });
            let autres = co
                .iter()
                .find(|(id, _)| *id == m.id)
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            let mut attente = self.en_attente.lock().unwrap();
            let e = attente.entry(m.id.clone()).or_insert_with(|| Attente {
                depuis: Instant::now(),
                lignes: Vec::new(),
                attendus: BTreeSet::new(),
            });
            e.attendus.remove(&user_id);
            if !e.lignes.iter().any(|l| l.user_id == user_id) {
                e.lignes.push(LigneAnnonce {
                    user_id,
                    resume: m.clone(),
                    rr,
                });
            }
            for autre in autres {
                if !e.lignes.iter().any(|l| l.user_id == autre) && e.attendus.insert(autre) {
                    let _ = travaux.send(Travail::Rafraichir {
                        user_id: autre,
                        relance: None,
                    });
                }
            }
        }
        n
    }

    /// Les annonces complètes, ou qui ont attendu assez longtemps.
    fn pretes(&self) -> Vec<Annonce> {
        let mut attente = self.en_attente.lock().unwrap();
        let mures: Vec<String> = attente
            .iter()
            .filter(|(_, a)| a.attendus.is_empty() || a.depuis.elapsed() >= ANNONCE_ATTENTE)
            .map(|(id, _)| id.clone())
            .collect();
        mures
            .into_iter()
            .filter_map(|id| {
                attente.remove(&id).map(|a| Annonce {
                    match_id: id,
                    lignes: a.lignes,
                })
            })
            .filter(|a| !a.lignes.is_empty())
            .collect()
    }
}

/// Le texte d'une annonce : le résultat en tête, puis une ligne par membre
/// du groupe, du meilleur score au moins bon, avec ses RR en classé. Si
/// le groupe était des deux côtés, chaque ligne dit de quel côté.
pub fn composer(a: &Annonce, pseudo: impl Fn(UserId) -> String) -> String {
    let mut lignes: Vec<&LigneAnnonce> = a.lignes.iter().collect();
    lignes.sort_by_key(|l| std::cmp::Reverse(l.resume.score));
    let Some(premier) = lignes.first().map(|l| &l.resume) else {
        return String::new();
    };
    let issue = |m: &MatchResume| -> String {
        let (g, p) = m.manches;
        match m.gagne {
            Some(true) => format!("victoire {g}-{p}"),
            Some(false) => format!("défaite {g}-{p}"),
            None => format!("égalité {g}-{p}"),
        }
    };
    let meme_camp = lignes
        .iter()
        .all(|l| l.resume.gagne == premier.gagne && l.resume.manches == premier.manches);
    let mut texte = if meme_camp {
        let emoji = match premier.gagne {
            Some(true) => "🏆",
            Some(false) => "💀",
            None => "🤝",
        };
        let mut i = issue(premier);
        if let Some(c) = i.get_mut(0..1) {
            c.make_ascii_uppercase();
        }
        format!("{emoji} {i} sur {} · {}", premier.carte, premier.mode)
    } else {
        format!(
            "⚔️ {} · {} — le groupe des deux côtés",
            premier.carte, premier.mode
        )
    };
    for l in lignes {
        let m = &l.resume;
        texte.push_str(&format!(
            "\n{} — {} {}/{}/{}",
            pseudo(l.user_id),
            m.agent,
            m.kills,
            m.deaths,
            m.assists
        ));
        if !meme_camp {
            texte.push_str(&format!(" · {}", issue(m)));
        }
        if let Some((delta, rang)) = &l.rr {
            let signe = if *delta >= 0 { "+" } else { "" };
            texte.push_str(&format!(
                " · {signe}{delta} RR ({}, {} RR)",
                ki_protocol::nom_de_rang(rang.tier),
                rang.rr
            ));
        }
    }
    texte
}

fn lire<T: serde::de::DeserializeOwned>(chemin: &std::path::Path) -> BTreeMap<UserId, T> {
    match std::fs::read_to_string(chemin) {
        Ok(texte) => serde_json::from_str(&texte).unwrap_or_else(|e| {
            tracing::warn!(
                "VALORANT : {} illisible ({e}), reparti de zéro",
                chemin.display()
            );
            BTreeMap::new()
        }),
        Err(_) => BTreeMap::new(),
    }
}

fn ecrire<T: Serialize>(chemin: &std::path::Path, valeur: &BTreeMap<UserId, T>) {
    if let Some(parent) = chemin.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_vec_pretty(valeur) {
        Ok(octets) => {
            if let Err(e) = crate::store::write_atomic(chemin, &octets) {
                tracing::warn!(
                    "VALORANT : écriture de {} impossible : {e}",
                    chemin.display()
                );
            }
        }
        Err(e) => tracing::warn!("VALORANT : sérialisation impossible : {e}"),
    }
}

/// La clé : la variable d'environnement d'abord, le fichier ensuite.
/// Jamais dans le binaire ni dans le dépôt.
fn cle_henrik(data_dir: &str) -> Option<String> {
    let env = std::env::var("KI_HENRIK_KEY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    env.or_else(|| {
        std::fs::read_to_string(PathBuf::from(data_dir).join("henrik.key"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

pub fn maintenant_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Le fil HenrikDev
// ---------------------------------------------------------------------------

/// Vingt requêtes par minute glissante : on note l'heure de chacune, et
/// quand la file est pleine on attend que la plus vieille ait une minute.
struct Seau {
    passees: VecDeque<Instant>,
}

impl Seau {
    fn new() -> Self {
        Self {
            passees: VecDeque::with_capacity(BUDGET_PAR_MINUTE),
        }
    }

    fn prendre(&mut self) {
        let fenetre = Duration::from_secs(60);
        while self.passees.front().is_some_and(|t| t.elapsed() >= fenetre) {
            self.passees.pop_front();
        }
        if self.passees.len() >= BUDGET_PAR_MINUTE {
            if let Some(plus_vieille) = self.passees.front() {
                let attente = fenetre.saturating_sub(plus_vieille.elapsed());
                std::thread::sleep(attente);
            }
            self.passees.pop_front();
        }
        self.passees.push_back(Instant::now());
    }
}

enum Erreur {
    Introuvable,
    /// Trop de requêtes, même après une reprise.
    Limite,
    Autre(String),
}

impl Erreur {
    fn message(&self) -> String {
        match self {
            Erreur::Introuvable => "compte Riot introuvable — vérifie le pseudo et le tag".into(),
            Erreur::Limite => "HenrikDev est saturé, réessaie dans une minute".into(),
            Erreur::Autre(e) => format!("HenrikDev ne répond pas : {e}"),
        }
    }
}

struct Api {
    agent: ureq::Agent,
    cle: String,
    seau: Seau,
    compteurs: Arc<Compteurs>,
}

impl Api {
    fn get(&mut self, chemin: &str) -> Result<Value, Erreur> {
        let url = format!("{BASE}{chemin}");
        let mut essais = 0;
        loop {
            self.seau.prendre();
            essais += 1;
            self.compteurs.requetes.fetch_add(1, Ordering::Relaxed);
            self.compteurs
                .derniere_ms
                .store(maintenant_ms(), Ordering::Relaxed);
            match self.agent.get(&url).set("Authorization", &self.cle).call() {
                Ok(reponse) => {
                    return reponse
                        .into_json::<Value>()
                        .map_err(|e| Erreur::Autre(e.to_string()));
                }
                Err(ureq::Error::Status(404, _)) => return Err(Erreur::Introuvable),
                Err(ureq::Error::Status(429, _)) if essais < 2 => {
                    self.compteurs.refus_429.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!("VALORANT : HenrikDev renvoie 429, pause de trente secondes");
                    std::thread::sleep(Duration::from_secs(30));
                }
                Err(ureq::Error::Status(429, _)) => {
                    self.compteurs.refus_429.fetch_add(1, Ordering::Relaxed);
                    return Err(Erreur::Limite);
                }
                Err(ureq::Error::Status(code, reponse)) => {
                    self.compteurs.erreurs.fetch_add(1, Ordering::Relaxed);
                    let detail = reponse
                        .into_json::<Value>()
                        .ok()
                        .and_then(|v| v["errors"][0]["message"].as_str().map(str::to_string))
                        .unwrap_or_default();
                    return Err(Erreur::Autre(
                        format!("HTTP {code} {detail}").trim().to_string(),
                    ));
                }
                Err(e) => {
                    self.compteurs.erreurs.fetch_add(1, Ordering::Relaxed);
                    return Err(Erreur::Autre(e.to_string()));
                }
            }
        }
    }
}

fn fil(
    cle: String,
    etat: Arc<Etat>,
    rx: Receiver<Travail>,
    tx: Sender<Resultat>,
    travaux: Sender<Travail>,
) {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("ki-chat-server/", env!("CARGO_PKG_VERSION")))
        .build();
    let mut api = Api {
        agent,
        cle,
        seau: Seau::new(),
        compteurs: Arc::clone(&etat.compteurs),
    };
    for travail in rx {
        match travail {
            Travail::Lier { user_id, nom, tag } => {
                let resultat = lier(&mut api, &etat, user_id, &nom, &tag);
                let (ok, message, riot_id) = match resultat {
                    Ok(fiche) => {
                        let m = format!(
                            "compte lié : {} · {} · {}",
                            fiche.riot_id,
                            fiche.region.to_uppercase(),
                            resume_rang(&fiche.rang)
                        );
                        (true, m, Some(fiche.riot_id))
                    }
                    Err(e) => (false, e.message(), None),
                };
                let _ = tx.send(Resultat::Liaison {
                    user_id,
                    ok,
                    message,
                    riot_id,
                });
            }
            Travail::Esports => {
                // Le calendrier complet d'abord ; s'il tombe (HenrikDev a
                // renvoyé 500 sur l'ensemble un soir de septembre 2026), la
                // seule région qui nous intéresse, puis l'international.
                // Même raté, il est daté de maintenant : pas de nouvel essai
                // avant une heure.
                let mut matchs = None;
                for filtre in ["", "?region=emea", "?region=international"] {
                    match api.get(&format!("/valorant/v1/esports/schedule{filtre}")) {
                        Ok(v) => {
                            matchs = Some(calendrier_esport(&v, maintenant_ms()));
                            break;
                        }
                        Err(e) => tracing::warn!(
                            "VALORANT : calendrier esport illisible ({}) : {}",
                            if filtre.is_empty() { "complet" } else { filtre },
                            e.message()
                        ),
                    }
                }
                let matchs = matchs.unwrap_or_else(|| etat.esports.lock().unwrap().1.clone());
                *etat.esports.lock().unwrap() = (maintenant_ms(), matchs);
                etat.esports_en_cours.store(false, Ordering::Relaxed);
            }
            Travail::Rafraichir { user_id, relance } => {
                let compte = etat.comptes.lock().unwrap().get(&user_id).cloned();
                let Some(compte) = compte else { continue };
                let lies = etat.lies();
                match construire(&mut api, &compte, None, &lies) {
                    Ok((fiche, co)) => {
                        let nouveaux = etat.fil.nouveaux(user_id, &fiche, &co, &travaux);
                        if nouveaux > 0 {
                            etat.sauver_fil();
                        }
                        etat.fiches.lock().unwrap().insert(user_id, fiche);
                        etat.sauver_fiches();
                        // Rien de neuf après une fin de partie : HenrikDev
                        // n'a pas encore le match, on relira.
                        if let Some(essais) = relance {
                            if nouveaux == 0 && essais + 1 < RELANCES_MAX {
                                etat.fil
                                    .relances
                                    .lock()
                                    .unwrap()
                                    .insert(user_id, (Instant::now() + RELANCE_DELAI, essais + 1));
                            }
                        }
                        let _ = tx.send(Resultat::Fiche { user_id });
                    }
                    Err(e) => {
                        tracing::warn!(
                            "VALORANT : fiche de {} non rafraîchie : {}",
                            compte.riot_id(),
                            e.message()
                        );
                        // Qu'on ne réessaie pas à chaque minute : la fiche
                        // est datée de maintenant même si elle n'a pas changé.
                        if let Some(f) = etat.fiches.lock().unwrap().get_mut(&user_id) {
                            f.maj = maintenant_ms();
                        }
                    }
                }
            }
        }
    }
}

fn resume_rang(r: &RangValorant) -> String {
    if r.tier == 0 {
        ki_protocol::nom_de_rang(0)
    } else {
        format!("{} {} RR", ki_protocol::nom_de_rang(r.tier), r.rr)
    }
}

/// Résout le compte, construit la fiche, enregistre les deux.
fn lier(
    api: &mut Api,
    etat: &Etat,
    user_id: UserId,
    nom: &str,
    tag: &str,
) -> Result<FicheValorant, Erreur> {
    let compte = api.get(&format!("/valorant/v2/account/{}/{}", enc(nom), enc(tag)))?;
    let d = &compte["data"];
    let puuid = d["puuid"].as_str().ok_or(Erreur::Introuvable)?.to_string();
    let region = d["region"].as_str().unwrap_or("eu").to_string();
    let plateformes: Vec<&str> = d["platforms"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let plateforme =
        if plateformes.iter().any(|p| p.eq_ignore_ascii_case("pc")) || plateformes.is_empty() {
            "pc"
        } else {
            "console"
        }
        .to_string();
    let compte = CompteRiot {
        nom: d["name"].as_str().unwrap_or(nom).to_string(),
        tag: d["tag"].as_str().unwrap_or(tag).to_string(),
        puuid,
        region,
        plateforme,
        depuis: maintenant_ms(),
    };
    let niveau = d["account_level"].as_u64().unwrap_or(0) as u32;
    let lies = etat.lies();
    let (fiche, _) = construire(api, &compte, Some(niveau), &lies)?;
    etat.comptes.lock().unwrap().insert(user_id, compte);
    etat.fiches.lock().unwrap().insert(user_id, fiche.clone());
    // Ses matchs d'avant la liaison ne s'annoncent pas.
    etat.fil.connaitre(user_id, &fiche);
    etat.sauver_comptes();
    etat.sauver_fiches();
    etat.sauver_fil();
    Ok(fiche)
}

/// Trois requêtes : rang, historique de RR, derniers matchs. Rend aussi,
/// par match, les autres membres liés (`lies` : membre → puuid) qui y
/// jouaient — reconnus à leur puuid, sans rien garder des neuf autres.
fn construire(
    api: &mut Api,
    compte: &CompteRiot,
    niveau: Option<u32>,
    lies: &[(UserId, String)],
) -> Result<(FicheValorant, CoMembres), Erreur> {
    let suffixe = format!(
        "{}/{}/{}/{}",
        compte.region,
        compte.plateforme,
        enc(&compte.nom),
        enc(&compte.tag)
    );
    let mmr = api.get(&format!("/valorant/v3/mmr/{suffixe}"))?;
    let historique = api
        .get(&format!("/valorant/v2/mmr-history/{suffixe}"))
        .unwrap_or(Value::Null);
    let matchs = api
        .get(&format!("/valorant/v4/matches/{suffixe}?size={MATCHS_MAX}"))
        .unwrap_or(Value::Null);

    let mut fiche = FicheValorant {
        riot_id: compte.riot_id(),
        region: compte.region.clone(),
        plateforme: compte.plateforme.clone(),
        niveau: niveau.unwrap_or(0),
        maj: maintenant_ms(),
        ..Default::default()
    };
    let courant = &mmr["data"]["current"];
    fiche.rang = RangValorant {
        tier: courant["tier"]["id"].as_u64().unwrap_or(0) as u8,
        rr: courant["rr"].as_u64().unwrap_or(0) as u16,
        delta: courant["last_change"].as_i64().unwrap_or(0) as i32,
        elo: courant["elo"].as_u64().unwrap_or(0) as u32,
        saison: String::new(),
    };
    let pic = &mmr["data"]["peak"];
    if let Some(tier) = pic["tier"]["id"].as_u64() {
        if tier > 0 {
            fiche.pic = Some(RangValorant {
                tier: tier as u8,
                rr: pic["rr"].as_u64().unwrap_or(0) as u16,
                delta: 0,
                elo: 0,
                saison: pic["season"]["short"].as_str().unwrap_or("").to_string(),
            });
        }
    }
    fiche.historique_rr = historique["data"]["history"]
        .as_array()
        .map(|h| {
            h.iter()
                .take(HISTORIQUE_MAX)
                .map(|p| PointRR {
                    match_id: p["match_id"].as_str().unwrap_or("").to_string(),
                    date: iso_vers_ms(p["date"].as_str().unwrap_or("")),
                    tier: p["tier"]["id"].as_u64().unwrap_or(0) as u8,
                    rr: p["rr"].as_u64().unwrap_or(0) as u16,
                    delta: p["last_change"].as_i64().unwrap_or(0) as i32,
                    carte: p["map"]["name"].as_str().unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    fiche.matchs = matchs["data"]
        .as_array()
        .map(|liste| {
            liste
                .iter()
                .filter_map(|m| resumer_match(m, &compte.puuid))
                .take(MATCHS_MAX)
                .collect()
        })
        .unwrap_or_default();
    let co_membres: CoMembres = matchs["data"]
        .as_array()
        .map(|liste| {
            liste
                .iter()
                .filter_map(|m| co_membres(m, &compte.puuid, lies))
                .collect()
        })
        .unwrap_or_default();
    if niveau.is_none() {
        // Le niveau de compte vient avec chaque match : le plus récent.
        if let Some(n) = matchs["data"]
            .as_array()
            .and_then(|l| l.first())
            .and_then(|m| m["players"].as_array())
            .and_then(|ps| {
                ps.iter()
                    .find(|p| p["puuid"].as_str() == Some(&compte.puuid))
            })
            .and_then(|p| p["account_level"].as_u64())
        {
            fiche.niveau = n as u32;
        }
    }
    Ok((fiche, co_membres))
}

/// Le calendrier esport de HenrikDev réduit à ce qu'on montre : les
/// matchs à venir ou en cours, du plus proche au plus lointain, vingt au
/// plus. Un match fini, ou daté d'il y a plus de trois heures, n'y est pas.
pub fn calendrier_esport(v: &Value, maintenant: u64) -> Vec<MatchEsport> {
    let mut matchs: Vec<MatchEsport> = v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let etat = m["state"].as_str().unwrap_or("").to_string();
            if etat != "unstarted" && etat != "inProgress" {
                return None;
            }
            let date = iso_vers_ms(m["date"].as_str().unwrap_or(""));
            if date == 0 || date + 3 * 3_600_000 < maintenant {
                return None;
            }
            let equipes: Vec<String> = m["match"]["teams"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| {
                    t["code"]
                        .as_str()
                        .filter(|c| !c.is_empty())
                        .or_else(|| t["name"].as_str())
                        .map(str::to_string)
                })
                .take(2)
                .collect();
            if equipes.len() < 2 {
                return None;
            }
            let format = match (
                m["match"]["game_type"]["type"].as_str(),
                m["match"]["game_type"]["count"].as_u64(),
            ) {
                (Some("bestOf"), Some(n)) if n > 0 => format!("BO{n}"),
                (Some("playAll"), Some(n)) if n > 0 => format!("{n} cartes"),
                _ => String::new(),
            };
            Some(MatchEsport {
                date,
                ligue: m["league"]["name"].as_str().unwrap_or("").to_string(),
                region: m["league"]["region"].as_str().unwrap_or("").to_string(),
                tournoi: m["tournament"]["name"].as_str().unwrap_or("").to_string(),
                equipes,
                etat,
                format,
            })
        })
        .collect();
    matchs.sort_by_key(|m| m.date);
    matchs.truncate(ESPORTS_MAX);
    matchs
}

/// Les autres membres liés qui jouaient ce match, reconnus à leur puuid.
/// `None` s'il n'y en a pas.
fn co_membres(m: &Value, moi: &str, lies: &[(UserId, String)]) -> Option<(String, Vec<UserId>)> {
    let id = m["metadata"]["match_id"].as_str()?.to_string();
    let puuids: Vec<&str> = m["players"]
        .as_array()?
        .iter()
        .filter_map(|p| p["puuid"].as_str())
        .collect();
    let autres: Vec<UserId> = lies
        .iter()
        .filter(|(_, puuid)| puuid != moi && puuids.contains(&puuid.as_str()))
        .map(|(id, _)| *id)
        .collect();
    (!autres.is_empty()).then_some((id, autres))
}

/// La ligne du membre dans un match — et rien des autres joueurs.
fn resumer_match(m: &Value, puuid: &str) -> Option<MatchResume> {
    let meta = &m["metadata"];
    let joueur = m["players"]
        .as_array()?
        .iter()
        .find(|p| p["puuid"].as_str() == Some(puuid))?;
    let equipe = joueur["team_id"].as_str().unwrap_or("");
    let camp = m["teams"]
        .as_array()
        .and_then(|ts| ts.iter().find(|t| t["team_id"].as_str() == Some(equipe)));
    let (gagnees, perdues) = camp
        .map(|t| {
            (
                t["rounds"]["won"].as_u64().unwrap_or(0) as u8,
                t["rounds"]["lost"].as_u64().unwrap_or(0) as u8,
            )
        })
        .unwrap_or((0, 0));
    let gagne = match camp.and_then(|t| t["won"].as_bool()) {
        Some(true) => Some(true),
        _ if gagnees == perdues => None,
        Some(false) => Some(false),
        None => None,
    };
    let stats = &joueur["stats"];
    let tetes = stats["headshots"].as_u64().unwrap_or(0);
    let tirs =
        tetes + stats["bodyshots"].as_u64().unwrap_or(0) + stats["legshots"].as_u64().unwrap_or(0);
    let mode = meta["queue"]["name"]
        .as_str()
        .or_else(|| meta["queue"]["mode_type"].as_str())
        .or_else(|| meta["queue"]["id"].as_str())
        .unwrap_or("")
        .to_string();
    Some(MatchResume {
        id: meta["match_id"].as_str().unwrap_or("").to_string(),
        date: iso_vers_ms(meta["started_at"].as_str().unwrap_or("")),
        carte: meta["map"]["name"].as_str().unwrap_or("").to_string(),
        mode: mode_en_francais(&mode),
        agent: joueur["agent"]["name"].as_str().unwrap_or("").to_string(),
        kills: stats["kills"].as_u64().unwrap_or(0) as u16,
        deaths: stats["deaths"].as_u64().unwrap_or(0) as u16,
        assists: stats["assists"].as_u64().unwrap_or(0) as u16,
        score: stats["score"].as_u64().unwrap_or(0) as u32,
        tete_pct: (tetes * 100).checked_div(tirs).unwrap_or(0) as u8,
        manches: (gagnees, perdues),
        gagne,
        tier: joueur["tier"]["id"].as_u64().unwrap_or(0) as u8,
        duree_s: (meta["game_length_in_ms"].as_u64().unwrap_or(0) / 1000) as u32,
    })
}

fn mode_en_francais(mode: &str) -> String {
    match mode.to_ascii_lowercase().as_str() {
        "competitive" => "Compétitif".into(),
        "unrated" => "Non classé".into(),
        "swiftplay" => "Partie rapide".into(),
        "spikerush" | "spike rush" => "Spike Rush".into(),
        "deathmatch" => "Combat à mort".into(),
        "team deathmatch" | "hurm" => "Combat à mort par équipe".into(),
        "escalation" => "Escalade".into(),
        "replication" => "Réplication".into(),
        "premier" => "Premier".into(),
        "custom" | "custom game" => "Personnalisée".into(),
        _ => mode.to_string(),
    }
}

/// Le pseudo dans l'URL : les espaces et l'accentué passent en pour-cent.
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for octet in s.bytes() {
        match octet {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(octet as char)
            }
            _ => out.push_str(&format!("%{octet:02X}")),
        }
    }
    out
}

/// « 2026-09-03T21:12:33.000Z » → millisecondes Unix (0 si illisible).
/// HenrikDev date tout en UTC, on ne lit pas de décalage.
pub fn iso_vers_ms(s: &str) -> u64 {
    let s = s.trim();
    let nombre = |a: usize, b: usize| -> Option<i64> { s.get(a..b)?.parse::<i64>().ok() };
    let (Some(an), Some(mois), Some(jour)) = (nombre(0, 4), nombre(5, 7), nombre(8, 10)) else {
        return 0;
    };
    let heure = nombre(11, 13).unwrap_or(0);
    let minute = nombre(14, 16).unwrap_or(0);
    let seconde = nombre(17, 19).unwrap_or(0);
    // Jours depuis l'époque, par l'algorithme civil de Howard Hinnant.
    let (y, m) = if mois <= 2 {
        (an - 1, mois + 9)
    } else {
        (an, mois - 3)
    };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + jour - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let jours = era * 146_097 + doe - 719_468;
    let secondes = jours * 86_400 + heure * 3600 + minute * 60 + seconde;
    if secondes < 0 {
        0
    } else {
        secondes as u64 * 1000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Les dates HenrikDev tombent juste, à la seconde.
    #[test]
    fn les_dates_iso_se_convertissent() {
        assert_eq!(iso_vers_ms("1970-01-01T00:00:00.000Z"), 0);
        assert_eq!(iso_vers_ms("2000-03-01T00:00:00Z"), 951_868_800_000);
        assert_eq!(iso_vers_ms("2026-09-03T21:12:33.000Z"), 1_788_469_953_000);
        assert_eq!(iso_vers_ms("n'importe quoi"), 0);
    }

    /// On ne garde que la ligne du membre — pas les neuf autres.
    #[test]
    fn un_match_se_resume_a_la_ligne_du_membre() {
        let m = serde_json::json!({
            "metadata": {
                "match_id": "abc", "map": {"name": "Ascent"}, "game_length_in_ms": 2_400_000,
                "started_at": "2026-09-03T21:12:33.000Z", "queue": {"id": "competitive", "name": "Competitive"}
            },
            "players": [
                {"puuid": "moi", "team_id": "Red", "agent": {"name": "Jett"}, "tier": {"id": 14},
                 "stats": {"score": 5000, "kills": 20, "deaths": 12, "assists": 4, "headshots": 30, "bodyshots": 60, "legshots": 10}},
                {"puuid": "autre", "team_id": "Blue", "agent": {"name": "Sage"}, "stats": {"kills": 3}}
            ],
            "teams": [
                {"team_id": "Red", "rounds": {"won": 13, "lost": 9}, "won": true},
                {"team_id": "Blue", "rounds": {"won": 9, "lost": 13}, "won": false}
            ]
        });
        let r = resumer_match(&m, "moi").unwrap();
        assert_eq!((r.kills, r.deaths, r.assists), (20, 12, 4));
        assert_eq!(r.manches, (13, 9));
        assert_eq!(r.gagne, Some(true));
        assert_eq!(r.tete_pct, 30);
        assert_eq!(r.mode, "Compétitif");
        assert_eq!(r.agent, "Jett");
        assert_eq!(r.duree_s, 2400);
        assert!(resumer_match(&m, "inconnu").is_none());
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("Sage") && !json.contains("autre"));
    }

    /// Les pseudos avec espace ou accent passent dans l'URL.
    #[test]
    fn les_pseudos_s_encodent() {
        assert_eq!(enc("Jean Michel"), "Jean%20Michel");
        assert_eq!(enc("Élan"), "%C3%89lan");
        assert_eq!(enc("Redik"), "Redik");
    }

    /// Le seau laisse passer vingt requêtes d'un coup et note chacune.
    #[test]
    fn le_seau_compte_les_requetes() {
        let mut seau = Seau::new();
        for _ in 0..BUDGET_PAR_MINUTE {
            seau.prendre();
        }
        assert_eq!(seau.passees.len(), BUDGET_PAR_MINUTE);
    }

    fn ligne(
        user_id: UserId,
        agent: &str,
        kda: (u16, u16, u16),
        score: u32,
        gagne: Option<bool>,
        rr: Option<(i32, u8, u16)>,
    ) -> LigneAnnonce {
        LigneAnnonce {
            user_id,
            resume: MatchResume {
                id: "m1".into(),
                carte: "Ascent".into(),
                mode: "Compétitif".into(),
                agent: agent.into(),
                kills: kda.0,
                deaths: kda.1,
                assists: kda.2,
                score,
                manches: if gagne == Some(true) {
                    (13, 9)
                } else {
                    (9, 13)
                },
                gagne,
                ..Default::default()
            },
            rr: rr.map(|(d, t, r)| {
                (
                    d,
                    RangValorant {
                        tier: t,
                        rr: r,
                        ..Default::default()
                    },
                )
            }),
        }
    }

    /// L'annonce d'une victoire à deux : le résultat en tête, le meilleur
    /// score d'abord, les RR en classé.
    #[test]
    fn une_annonce_se_compose() {
        let a = Annonce {
            match_id: "m1".into(),
            lignes: vec![
                ligne(2, "Sage", (12, 14, 9), 3000, Some(true), Some((16, 15, 12))),
                ligne(1, "Jett", (24, 12, 6), 6000, Some(true), Some((18, 14, 57))),
            ],
        };
        let texte = composer(&a, |id| {
            if id == 1 {
                "Jerem".into()
            } else {
                "Redik".into()
            }
        });
        assert_eq!(
            texte,
            "🏆 Victoire 13-9 sur Ascent · Compétitif\nJerem — Jett 24/12/6 · +18 RR (Or 3, 57 RR)\nRedik — Sage 12/14/9 · +16 RR (Platine 1, 12 RR)"
        );
        // Des deux côtés : chaque ligne dit son issue.
        let b = Annonce {
            match_id: "m1".into(),
            lignes: vec![
                ligne(1, "Jett", (24, 12, 6), 6000, Some(true), None),
                ligne(2, "Sage", (12, 14, 9), 3000, Some(false), None),
            ],
        };
        let texte = composer(&b, |id| format!("j{id}"));
        assert!(texte.starts_with("⚔️ Ascent · Compétitif — le groupe des deux côtés"));
        assert!(texte.contains("j1 — Jett 24/12/6 · victoire 13-9"));
        assert!(texte.contains("j2 — Sage 12/14/9 · défaite 9-13"));
    }

    /// Un match connu ne s'annonce pas deux fois ; un mode d'arcade ou un
    /// vieux match est noté sans être annoncé ; un coéquipier du groupe est
    /// relu et attendu, et l'annonce part quand il est là.
    #[test]
    fn le_fil_annonce_une_fois_et_attend_les_coequipiers() {
        let fil = Fil::default();
        let (tx, rx) = mpsc::channel();
        let recent = maintenant_ms() - 600_000;
        let m = |id: &str, mode: &str, date: u64| MatchResume {
            id: id.into(),
            mode: mode.into(),
            date,
            carte: "Bind".into(),
            manches: (13, 5),
            gagne: Some(true),
            ..Default::default()
        };
        let fiche = FicheValorant {
            matchs: vec![
                m("neuf", "Compétitif", recent),
                m("dm", "Combat à mort", recent),
                m("vieux", "Compétitif", recent - 7 * 3_600_000),
            ],
            historique_rr: vec![PointRR {
                match_id: "neuf".into(),
                delta: 21,
                tier: 12,
                rr: 40,
                ..Default::default()
            }],
            ..Default::default()
        };
        let co = vec![("neuf".to_string(), vec![2u64])];
        assert_eq!(fil.nouveaux(1, &fiche, &co, &tx), 3);
        // Le coéquipier 2 a été relu, et l'annonce l'attend.
        assert!(matches!(
            rx.try_recv(),
            Ok(Travail::Rafraichir {
                user_id: 2,
                relance: None
            })
        ));
        assert!(fil.pretes().is_empty(), "l'annonce attend le coéquipier");
        // Rien de neuf à la seconde lecture.
        assert_eq!(fil.nouveaux(1, &fiche, &co, &tx), 0);
        // Le coéquipier arrive : l'annonce est prête, à deux lignes, avec
        // les RR du premier.
        let fiche2 = FicheValorant {
            matchs: vec![m("neuf", "Compétitif", recent)],
            ..Default::default()
        };
        assert_eq!(fil.nouveaux(2, &fiche2, &[], &tx), 1);
        let pretes = fil.pretes();
        assert_eq!(pretes.len(), 1);
        assert_eq!(pretes[0].lignes.len(), 2);
        assert_eq!(pretes[0].lignes[0].rr.as_ref().map(|(d, _)| *d), Some(21));
        assert!(fil.pretes().is_empty());
    }

    /// Le calendrier ne garde que l'à-venir et l'en-cours, dans l'ordre,
    /// avec les codes d'équipe et le format.
    #[test]
    fn le_calendrier_esport_se_reduit() {
        let maintenant = 1_800_000_000_000u64;
        let v = serde_json::json!({ "data": [
            { "date": "2027-01-20T18:00:00.000Z", "state": "completed", "league": {"name": "VCT EMEA"}, "match": {"teams": [{"code": "FNC"}, {"code": "TH"}]} },
            { "date": "2027-01-21T18:00:00.000Z", "state": "unstarted", "league": {"name": "VCT EMEA", "region": "EMEA"},
              "tournament": {"name": "Kickoff"}, "match": {"game_type": {"type": "bestOf", "count": 3}, "teams": [{"code": "FNC", "name": "Fnatic"}, {"code": "", "name": "Team Heretics"}]} },
            { "date": "2027-01-21T15:00:00.000Z", "state": "inProgress", "league": {"name": "VCT Pacific"}, "match": {"game_type": {"type": "playAll", "count": 2}, "teams": [{"code": "PRX"}, {"code": "DRX"}]} }
        ]});
        let c = calendrier_esport(&v, maintenant);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].equipes, vec!["PRX", "DRX"]);
        assert_eq!(c[0].format, "2 cartes");
        assert_eq!(c[1].equipes, vec!["FNC", "Team Heretics"]);
        assert_eq!(
            (
                c[1].format.as_str(),
                c[1].region.as_str(),
                c[1].tournoi.as_str()
            ),
            ("BO3", "EMEA", "Kickoff")
        );
    }

    /// Sans clé, le service reste ouvert en lecture et ferme les liaisons.
    #[test]
    fn sans_cle_rien_ne_casse() {
        let dir = std::env::temp_dir().join(format!("ki-valorant-{}", std::process::id()));
        std::env::remove_var("KI_HENRIK_KEY");
        let v = Valorant::open(dir.to_str().unwrap());
        assert!(v.lier(1, "Redik".into(), "6162".into()).is_err());
        assert!(v.riot_id(1).is_none() && v.rang(1).is_none() && v.fiche(1).is_none());
        assert!(!v.delier(1));
        assert!(v.a_rafraichir(&[1, 2], Duration::from_secs(1)).is_empty());
        assert!(v.resultats().is_empty());
        v.fin_de_partie(1);
        assert!(v.tick().is_empty());
        v.rafraichir_esports();
        assert!(v.esports().is_empty());
        assert!(v.compteurs_texte().contains("clé HenrikDev absente"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
