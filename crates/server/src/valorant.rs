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
//! Seules une liaison (six requêtes : le compte, la fiche, et deux de
//! rattrapage dans les archives de HenrikDev) et un rafraîchissement
//! (trois) touchent l'API, et les rafraîchissements s'espacent d'une
//! demi-heure par membre, en ligne seulement.
//!
//! **Ce qu'on garde.** La ligne du membre dans chaque match — jamais celles
//! des neuf autres, qui ne sont pas du serveur. Depuis 0.1.40 la fiche
//! **s'accumule** : chaque rafraîchissement ne rapporte que les cinq
//! derniers matchs, mais [`fusionner`] les range sous les anciens, jusqu'à
//! soixante matchs et cent points de RR par membre — sans une requête de
//! plus. Et de chaque match on lit les manches (`rounds[]`, `kills[]`,
//! déjà téléchargés) pour en tirer la ligne du membre manche par manche :
//! premiers sangs, KAST, multi-kills, clutchs, poses. Deux fichiers sous
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
//!
//! **Sobriété (0.1.43).** Les soirées coûtaient plus que le régime établi :
//! un 5-stack classé valait 27 requêtes au lieu de 15, un hoquet du
//! client Riot en pleine partie en valait 9 pour rien. Depuis, tout
//! rafraîchissement passe par [`Fil::pousser`], qui ne met pas deux fois
//! le même membre en file ni ne double une relance programmée ; la fin de
//! partie est **qualifiée** — pas de relance sous trois minutes de jeu ni
//! en personnalisée, trois relances seulement pour une file qui s'annonce
//! (compétitive, non classée, swiftplay, Premier), une sinon, et une
//! reprise de partie annule la relance qui n'a pas encore tiré ; un 429
//! gèle le seau pour tout le fil le temps que HenrikDev demande
//! (`Retry-After`) et ne nourrit plus de relance ; et chaque requête est
//! comptée par motif et par point d'API, les soixante dernières gardées
//! en anneau, pour que `/diag-resume` dise *pourquoi* on a appelé.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ki_protocol::{
    BilanMembre, DetailManches, FicheMembre, FicheValorant, MatchEsport, MatchResume, PointRR,
    RangValorant, ServerMsg, StatsSaison, UserId, STATS_MAX_BYTES,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Requêtes tolérées par minute glissante — sous les trente de la clé.
const BUDGET_PAR_MINUTE: usize = 20;
/// Ce qu'un rafraîchissement rapporte : le `take` sur `v2/mmr-history`
/// (l'API en rend une vingtaine) et la taille de la requête `v4/matches`
/// — cinq, parce qu'un match pèse de 300 Ko à 1 Mo sous un timeout de
/// vingt secondes, et que la fiche accumule de toute façon.
const HISTORIQUE_MAX: usize = 20;
const MATCHS_MAX: usize = 5;
/// Ce que la fiche accumule au fil des rafraîchissements.
const MATCHS_GARDES: usize = 60;
const HISTORIQUE_GARDES: usize = 100;
/// Points du résumé envoyé à la page du groupe (les matchs : `MATCHS_MAX`).
const POINTS_RESUME: usize = 10;
/// Le rattrapage à la liaison, une fois par membre : ce que HenrikDev a
/// archivé de lui (`stored-matches`, `stored-mmr-history`).
const RATTRAPAGE_MATCHS: usize = 60;
const RATTRAPAGE_POINTS: usize = 100;
/// Fenêtre d'échange du KAST : ma mort vengée dans les 5 s compte.
const ECHANGE_MS: u64 = 5_000;
/// Un jour, en millisecondes.
const JOUR_MS: u64 = 86_400_000;
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
/// match : la fiche est relue après ce délai, jusqu'à `RELANCES_MAX` fois
/// pour une file qui s'annonce, une seule fois pour les autres (le
/// combat à mort finit dans la fiche au périodique, rien à annoncer).
const RELANCE_DELAI: Duration = Duration::from_secs(75);
const RELANCES_MAX: u32 = 3;
const RELANCES_SANS_ANNONCE: u32 = 1;
/// Les files dont un match s'annonce — les mêmes que [`MODES_ANNONCES`],
/// vues du client Riot (sans le préfixe `console_`).
const FILES_ANNONCEES: [&str; 4] = ["competitive", "unrated", "swiftplay", "premier"];
/// Une partie plus courte que ça n'en est pas une : un hoquet du client
/// Riot, un remake, un plantage. Pas de relance, le périodique verra.
const PARTIE_MIN: Duration = Duration::from_secs(3 * 60);
/// Une fiche relue il y a moins d'une minute ne se relit pas pour un
/// périodique — une relance ou un co-membre passent outre (voir
/// [`relecture_inutile`]).
const FRAICHEUR: Duration = Duration::from_secs(60);
/// Après un échec, pas de nouvel essai périodique avant ce délai —
/// qu'il y ait une fiche ou non.
const ECHEC_ATTENTE: Duration = Duration::from_secs(30 * 60);
/// Un 429 sans `Retry-After` gèle le seau autant ; avec, jamais plus que
/// le plafond — l'en-tête vient du réseau, il ne commande pas le fil
/// pour l'après-midi.
const GEL_DEFAUT: Duration = Duration::from_secs(30);
const GEL_MAX: Duration = Duration::from_secs(5 * 60);
/// Les requêtes gardées en anneau pour le diagnostic, et les minutes
/// d'histoire du pic.
const JOURNAL_REQUETES: usize = 60;
const PIC_MINUTES: u64 = 60;
/// Identifiants de matchs gardés par membre dans `fil.json` — plus que
/// les soixante d'une fiche, puisque `connaitre` y verse la fiche entière.
const ANNONCES_GARDEES: usize = 80;
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
    /// `motif` : pourquoi on relit — la énième relance après une fin de
    /// partie, le périodique, un co-membre reconnu. Le fil le compte, et
    /// s'en sert pour savoir s'il faut relire encore.
    Rafraichir {
        user_id: UserId,
        motif: Motif,
    },
    /// Le calendrier esport.
    Esports,
}

/// Pourquoi une requête part : c'est ce que le diagnostic compte, et ce
/// qui départage deux demandes pour le même membre.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Motif {
    /// La demi-heure est passée.
    Periodique,
    /// La énième relecture après une fin de partie (`essai` de 0), sur
    /// `max` au plus.
    Relance { essai: u32, max: u32 },
    /// Reconnu dans le match d'un autre membre.
    CoMembre,
    /// Une liaison.
    Liaison,
    /// Le calendrier esport.
    Esports,
}

impl Motif {
    /// L'essai en cours s'il s'agit d'une relance.
    fn relance(self) -> Option<u32> {
        match self {
            Motif::Relance { essai, .. } => Some(essai),
            _ => None,
        }
    }

    /// Ce qu'une demande dit de plus qu'une autre : une relance sait
    /// qu'il y a un match à trouver, un co-membre sait qu'il y a une
    /// annonce qui attend, le périodique ne sait rien.
    fn poids(self) -> u8 {
        match self {
            Motif::Periodique => 0,
            Motif::CoMembre => 1,
            Motif::Relance { .. } => 2,
            Motif::Liaison | Motif::Esports => 3,
        }
    }

    /// Sa case dans les compteurs.
    fn indice(self) -> usize {
        match self {
            Motif::Periodique => 0,
            Motif::Relance { .. } => 1,
            Motif::CoMembre => 2,
            Motif::Liaison => 3,
            Motif::Esports => 4,
        }
    }

    const NOMS: [&'static str; 5] = ["périodique", "relance", "co-membre", "liaison", "esport"];

    fn nom(self) -> String {
        match self {
            Motif::Relance { essai, max } => format!("relance {}/{max}", essai + 1),
            autre => Self::NOMS[autre.indice()].to_string(),
        }
    }
}

/// Le point d'API HenrikDev touché, reconnu au chemin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PointApi {
    Mmr,
    History,
    Matches,
    Account,
    Stored,
    Esports,
    Autre,
}

impl PointApi {
    /// Le point d'API est le troisième segment du chemin
    /// (`/valorant/v3/mmr/…`) — jamais une sous-chaîne : le Riot ID qui
    /// suit vient d'un membre, et un membre peut s'appeler « mmr ».
    fn de(chemin: &str) -> Self {
        let segment = chemin
            .split('/')
            .nth(3)
            .and_then(|s| s.split('?').next())
            .unwrap_or("");
        match segment {
            "mmr" => PointApi::Mmr,
            "mmr-history" => PointApi::History,
            "matches" => PointApi::Matches,
            "account" => PointApi::Account,
            "stored-matches" | "stored-mmr-history" => PointApi::Stored,
            "esports" => PointApi::Esports,
            _ => PointApi::Autre,
        }
    }

    fn indice(self) -> usize {
        match self {
            PointApi::Mmr => 0,
            PointApi::History => 1,
            PointApi::Matches => 2,
            PointApi::Account => 3,
            PointApi::Stored => 4,
            PointApi::Esports => 5,
            PointApi::Autre => 6,
        }
    }

    const NOMS: [&'static str; 7] = ["mmr", "history", "matches", "account", "stored", "esports", "autre"];

    fn nom(self) -> &'static str {
        Self::NOMS[self.indice()]
    }
}

/// Une requête telle que l'anneau du diagnostic la garde.
#[derive(Debug, Clone, Copy)]
struct Requete {
    ms: u64,
    motif: Motif,
    point: PointApi,
    /// 0 : pas de réponse (réseau, délai).
    code: u16,
    duree_ms: u32,
}

/// Ce que HenrikDev a coûté depuis le démarrage — lisible dans le résumé
/// des diagnostics : les totaux, le détail par motif et par point d'API,
/// les soixante dernières requêtes, et de quoi dire le pic par minute de
/// la dernière heure.
#[derive(Default)]
struct Compteurs {
    requetes: AtomicU64,
    refus_429: AtomicU64,
    dernier_429_ms: AtomicU64,
    erreurs: AtomicU64,
    derniere_ms: AtomicU64,
    par_motif: [AtomicU64; 5],
    par_point: [AtomicU64; 7],
    journal: Mutex<VecDeque<Requete>>,
    /// Par minute Unix : combien de requêtes — soixante minutes gardées.
    par_minute: Mutex<VecDeque<(u64, u32)>>,
}

impl Compteurs {
    /// Note une requête partie, où qu'elle aboutisse.
    fn noter(&self, r: Requete) {
        self.requetes.fetch_add(1, Ordering::Relaxed);
        self.derniere_ms.store(r.ms, Ordering::Relaxed);
        self.par_motif[r.motif.indice()].fetch_add(1, Ordering::Relaxed);
        self.par_point[r.point.indice()].fetch_add(1, Ordering::Relaxed);
        // Un 404 n'est pas une erreur de HenrikDev : c'est un Riot ID mal
        // écrit.
        if r.code == 429 {
            self.refus_429.fetch_add(1, Ordering::Relaxed);
            self.dernier_429_ms.store(r.ms, Ordering::Relaxed);
        } else if r.code != 200 && r.code != 404 {
            self.erreurs.fetch_add(1, Ordering::Relaxed);
        }
        let mut journal = self.journal.lock().unwrap();
        journal.push_back(r);
        while journal.len() > JOURNAL_REQUETES {
            journal.pop_front();
        }
        let minute = r.ms / 60_000;
        let mut par_minute = self.par_minute.lock().unwrap();
        match par_minute.back_mut() {
            Some((m, n)) if *m == minute => *n = n.saturating_add(1),
            _ => par_minute.push_back((minute, 1)),
        }
        while par_minute
            .front()
            .is_some_and(|(m, _)| minute.saturating_sub(*m) >= PIC_MINUTES)
        {
            par_minute.pop_front();
        }
    }

    /// Le pic : la minute la plus chargée de la dernière heure.
    fn pic(&self, maintenant: u64) -> u32 {
        let minute = maintenant / 60_000;
        self.par_minute
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| minute.saturating_sub(*m) < PIC_MINUTES)
            .map(|(_, n)| *n)
            .max()
            .unwrap_or(0)
    }
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

/// Une relecture programmée après une fin de partie : quand, la énième,
/// et combien au plus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Relance {
    quand: Instant,
    essai: u32,
    max: u32,
}

/// Le fil de jeu : ce qui a été annoncé, ce qui attend, ce qui doit être
/// relu — et ce qui l'est déjà, pour ne pas le demander deux fois.
///
/// Verrous, dans cet ordre quand il en faut plusieurs : `annonces`,
/// `en_attente`, `en_file`, `relances`.
#[derive(Default)]
struct Fil {
    /// Les matchs déjà annoncés (ou connus à la liaison), par membre.
    annonces: Mutex<BTreeMap<UserId, VecDeque<String>>>,
    en_attente: Mutex<BTreeMap<String, Attente>>,
    /// Relectures programmées après une fin de partie.
    relances: Mutex<BTreeMap<UserId, Relance>>,
    /// Les rafraîchissements en file, par membre, avec le motif le plus
    /// informatif reçu : c'est ce que le fil lit en commençant.
    en_file: Mutex<BTreeMap<UserId, Motif>>,
    /// Les liaisons en file : un double clic n'en fait pas deux.
    liaisons_en_file: Mutex<BTreeSet<UserId>>,
    /// Le dernier échec par membre, en millisecondes Unix : pas de nouvel
    /// essai périodique avant [`ECHEC_ATTENTE`], fiche ou pas.
    derniere_tentative: Mutex<BTreeMap<UserId, u64>>,
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
    /// Le dernier récap hebdo posté, en millisecondes Unix (0 : jamais) ;
    /// `data/valorant/recap.json`.
    dernier_recap: Mutex<u64>,
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
        let dernier_recap = std::fs::read_to_string(dossier.join("recap.json"))
            .ok()
            .and_then(|t| t.trim().parse().ok())
            .unwrap_or(0);
        let etat = Arc::new(Etat {
            dossier,
            comptes: Mutex::new(comptes),
            fiches: Mutex::new(fiches),
            fil: Fil {
                annonces: Mutex::new(annonces),
                ..Default::default()
            },
            dernier_recap: Mutex::new(dernier_recap),
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

    /// Les fiches allégées et leur bilan, pour la page du groupe :
    /// `n_matchs` matchs et `n_points` points par membre, et les agrégats
    /// calculés à l'envoi sous le verrou — [`FicheValorant::resume`] ne
    /// clone que ce qui part, la fiche entière ne quitte pas la table.
    pub fn resumes(&self, n_matchs: usize, n_points: usize) -> Vec<(UserId, FicheValorant, BilanMembre)> {
        let maintenant = maintenant_ms();
        self.etat
            .fiches
            .lock()
            .unwrap()
            .iter()
            .map(|(id, f)| (*id, f.resume(n_matchs, n_points), bilan_membre(f, maintenant)))
            .collect()
    }

    /// Les 168 cases jour × heure (UTC) des parties commencées sur trente
    /// jours, tous membres et modes — vide si personne n'a joué.
    pub fn activite(&self) -> Vec<u16> {
        let fiches = self.etat.fiches.lock().unwrap();
        activite_de(fiches.values(), maintenant_ms())
    }

    /// Met la liaison en file. Refuse sans clé, si ce Riot ID est déjà
    /// celui d'un autre membre, ou si une liaison de ce membre attend
    /// déjà : six requêtes par clic, pas par double clic.
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
        if !self.etat.fil.liaisons_en_file.lock().unwrap().insert(user_id) {
            return Err("ta liaison est déjà en cours : attends sa réponse".into());
        }
        travaux.send(Travail::Lier { user_id, nom, tag }).map_err(|_| {
            self.etat.fil.liaisons_en_file.lock().unwrap().remove(&user_id);
            "le service VALORANT est arrêté".to_string()
        })
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
        self.etat.fil.derniere_tentative.lock().unwrap().remove(&user_id);
        if retire {
            self.etat.sauver_comptes();
        }
        if fiche {
            self.etat.sauver_fiches();
        }
        retire
    }

    /// Demande un rafraîchissement périodique — sans clé ou sans compte,
    /// rien ; déjà en file ou en relance, rien non plus.
    pub fn rafraichir(&self, user_id: UserId) {
        if let Some(travaux) = &self.travaux {
            if self.etat.comptes.lock().unwrap().contains_key(&user_id) {
                self.etat.fil.pousser(user_id, Motif::Periodique, travaux);
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

    /// L'état du service en une ligne, pour le résumé des diagnostics et
    /// le tableau de bord : les totaux, puis le pic par minute de la
    /// dernière heure, le détail par motif et par point d'API, et le
    /// dernier 429.
    pub fn compteurs_texte(&self) -> String {
        let c = &self.etat.compteurs;
        let maintenant = maintenant_ms();
        let il_y_a = |t: u64| match t {
            0 => "jamais".to_string(),
            t => format!("il y a {} min", maintenant.saturating_sub(t) / 60_000),
        };
        let attente = self.etat.fil.en_attente.lock().unwrap().len();
        let en_file = self.etat.fil.en_file.lock().unwrap().len();
        let relances = self.etat.fil.relances.lock().unwrap().len();
        let motifs: Vec<String> = Motif::NOMS
            .iter()
            .zip(c.par_motif.iter())
            .map(|(nom, n)| format!("{nom} {}", n.load(Ordering::Relaxed)))
            .collect();
        let points: Vec<String> = PointApi::NOMS
            .iter()
            .zip(c.par_point.iter())
            .map(|(nom, n)| (nom, n.load(Ordering::Relaxed)))
            .filter(|(_, n)| *n > 0)
            .map(|(nom, n)| format!("{nom} {n}"))
            .collect();
        format!(
            "VALORANT : clé HenrikDev {} · {} membres liés, {} fiches · requêtes depuis le démarrage : {} \
             (refus 429 : {}, dernier {}, erreurs : {}), dernière {} · pic sur une minute (dernière heure) : {} \
             · motifs : {} · points d'API : {} · en file : {}, relances programmées : {}, annonces en attente : {}",
            if self.travaux.is_some() { "présente" } else { "absente" },
            self.etat.comptes.lock().unwrap().len(),
            self.etat.fiches.lock().unwrap().len(),
            c.requetes.load(Ordering::Relaxed),
            c.refus_429.load(Ordering::Relaxed),
            il_y_a(c.dernier_429_ms.load(Ordering::Relaxed)),
            c.erreurs.load(Ordering::Relaxed),
            il_y_a(c.derniere_ms.load(Ordering::Relaxed)),
            c.pic(maintenant),
            motifs.join(", "),
            if points.is_empty() { "aucun".to_string() } else { points.join(", ") },
            en_file,
            relances,
            attente,
        )
    }

    /// Les soixante dernières requêtes, une par ligne, de la plus ancienne
    /// à la plus récente : l'heure, le motif, le point d'API, le code et
    /// la durée — pour expliquer un 429 après coup.
    pub fn journal_texte(&self) -> String {
        let journal = self.etat.compteurs.journal.lock().unwrap();
        if journal.is_empty() {
            return "HenrikDev : aucune requête depuis le démarrage".to_string();
        }
        let mut texte = format!("HenrikDev : les {} dernières requêtes", journal.len());
        for r in journal.iter() {
            let heure = chrono::DateTime::from_timestamp_millis(r.ms as i64)
                .map(|d| d.format("%H:%M:%S").to_string())
                .unwrap_or_else(|| "?".into());
            let code = if r.code == 0 { "—".to_string() } else { r.code.to_string() };
            texte.push_str(&format!(
                "\n  {heure} UTC · {} · {} · {code} · {} ms",
                r.motif.nom(),
                r.point.nom(),
                r.duree_ms
            ));
        }
        texte
    }

    /// Le membre sort d'une partie — `ancien` : ce qu'il jouait, `duree` :
    /// depuis quand. Sa fiche sera relue dans 75 s, puis encore si le
    /// match n'y est pas — jamais pour une partie de moins de trois
    /// minutes ni une personnalisée, trois fois pour une file qui
    /// s'annonce, une fois sinon (voir [`relances_pour`]).
    pub fn fin_de_partie(&self, user_id: UserId, ancien: &ki_protocol::JeuStatut, duree: Duration) {
        if self.travaux.is_none() || !self.etat.comptes.lock().unwrap().contains_key(&user_id) {
            return;
        }
        let max = relances_pour(ancien, duree);
        if max == 0 {
            tracing::debug!(
                "VALORANT : fin de partie de {user_id} sans relance ({} après {} s)",
                ancien.libelle_file(),
                duree.as_secs()
            );
            return;
        }
        self.etat.fil.relances.lock().unwrap().insert(
            user_id,
            Relance {
                quand: Instant::now() + RELANCE_DELAI,
                essai: 0,
                max,
            },
        );
    }

    /// Le membre est (de nouveau) en partie : la relance qui n'a pas
    /// encore tiré n'a plus lieu d'être — c'était un hoquet du client
    /// Riot, ou le match suivant s'est enchaîné et sa fin la relancera.
    pub fn reprise_de_partie(&self, user_id: UserId) {
        let mut relances = self.etat.fil.relances.lock().unwrap();
        if relances.get(&user_id).is_some_and(|r| r.essai == 0) {
            relances.remove(&user_id);
        }
    }

    /// À appeler régulièrement : lance les relectures dues et rend les
    /// annonces prêtes — complètes, ou qui ont assez attendu.
    pub fn tick(&self) -> Vec<Annonce> {
        let Some(travaux) = &self.travaux else {
            return Vec::new();
        };
        let maintenant = Instant::now();
        let dues: Vec<(UserId, Relance)> = {
            let mut relances = self.etat.fil.relances.lock().unwrap();
            let dues: Vec<_> = relances
                .iter()
                .filter(|(_, r)| r.quand <= maintenant)
                .map(|(id, r)| (*id, *r))
                .collect();
            for (id, _) in &dues {
                relances.remove(id);
            }
            dues
        };
        for (user_id, r) in dues {
            self.etat.fil.pousser(
                user_id,
                Motif::Relance {
                    essai: r.essai,
                    max: r.max,
                },
                travaux,
            );
        }
        self.etat.fil.pretes()
    }

    /// Le récap de la semaine, s'il est l'heure (dimanche soir) et qu'il
    /// n'est pas déjà parti cette semaine — marqué comme posté dès qu'il
    /// est rendu, même vide : une semaine sans match ne se redit pas.
    pub fn recap_hebdo(&self) -> Option<Recap> {
        self.travaux.as_ref()?;
        let maintenant = maintenant_ms();
        if !heure_du_recap(maintenant) {
            return None;
        }
        let mut dernier = self.etat.dernier_recap.lock().unwrap();
        if maintenant.saturating_sub(*dernier) < 6 * 86_400_000 {
            return None;
        }
        *dernier = maintenant;
        let _ = crate::store::write_atomic(
            &self.etat.dossier.join("recap.json"),
            maintenant.to_string().as_bytes(),
        );
        let fiches = self.etat.fiches.lock().unwrap();
        recap_de(&fiches, maintenant.saturating_sub(7 * 86_400_000), maintenant)
    }

    /// Parmi `en_ligne`, les membres liés dont la fiche a plus de
    /// `age_max` — ou qui n'en ont pas — et dont le dernier échec, s'il y
    /// en a un, a plus de [`ECHEC_ATTENTE`] : un membre sans fiche que
    /// HenrikDev refuse ne se redemande pas chaque minute.
    pub fn a_rafraichir(&self, en_ligne: &[UserId], age_max: Duration) -> Vec<UserId> {
        if self.travaux.is_none() {
            return Vec::new();
        }
        let maintenant = maintenant_ms();
        let comptes = self.etat.comptes.lock().unwrap();
        let fiches = self.etat.fiches.lock().unwrap();
        let tentatives = self.etat.fil.derniere_tentative.lock().unwrap();
        en_ligne
            .iter()
            .copied()
            .filter(|id| comptes.contains_key(id))
            .filter(|id| {
                fiches
                    .get(id)
                    .is_none_or(|f| maintenant.saturating_sub(f.maj) > age_max.as_millis() as u64)
            })
            .filter(|id| {
                tentatives.get(id).is_none_or(|t| {
                    maintenant.saturating_sub(*t) > ECHEC_ATTENTE.as_millis() as u64
                })
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

/// Combien de relances une fin de partie mérite : aucune sous
/// [`PARTIE_MIN`] (hoquet du client Riot, remake, plantage) ni en
/// personnalisée ; [`RELANCES_MAX`] pour une file qui s'annonce (les
/// variantes console comprises) ; une seule sinon — le match finira dans
/// la fiche, il n'y a rien à annoncer, pas la peine d'insister.
fn relances_pour(ancien: &ki_protocol::JeuStatut, duree: Duration) -> u32 {
    if duree < PARTIE_MIN || ancien.custom {
        return 0;
    }
    let file = ancien.file.strip_prefix("console_").unwrap_or(&ancien.file);
    if FILES_ANNONCEES.iter().any(|f| f.eq_ignore_ascii_case(file)) {
        RELANCES_MAX
    } else {
        RELANCES_SANS_ANNONCE
    }
}

/// Ce qu'un changement de statut de jeu dit au fil : voir
/// [`transition_de_jeu`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TransitionJeu {
    /// Il sort d'une partie de VALORANT : ce qu'il jouait, et depuis
    /// combien de temps ([`Duration::MAX`] quand on ne sait pas).
    pub fin: Option<(ki_protocol::JeuStatut, Duration)>,
    /// Il (re)vient en partie : une relance qui n'a pas encore tiré n'a
    /// plus lieu d'être.
    pub reprise: bool,
    /// L'heure d'entrée en partie à garder sur la session.
    pub en_jeu_depuis: Option<Instant>,
}

/// Lit une transition de statut (`ancien` → `nouveau`) avec l'heure
/// d'entrée en partie connue (`depuis`), et dit au fil ce qu'elle vaut.
///
/// Toute la finesse est dans l'heure d'entrée. Elle n'est posée que sur
/// une **vraie** entrée, du menu (ou de la sélection d'agent) à la
/// partie. Elle n'est **pas** effacée sur une présence perdue (`None`) :
/// le client Riot hoquette — lockfile, session, présences en erreur —
/// et revient quelques secondes plus tard ; s'il revient en partie,
/// c'est la même partie, et sa vraie fin se mesure depuis l'entrée
/// d'origine, pas depuis le hoquet. Seul un menu observé l'efface. Et la
/// première présence d'une session (`None` → en jeu sans heure d'entrée :
/// reconnexion, second client, relance après un plantage) ne compte ni
/// comme entrée ni comme reprise — on ne sait ni depuis quand ni s'il
/// s'agit d'un match nouveau ; la fin sera réputée assez longue.
///
/// Sans cela, un hoquet ou une reconnexion dans les trois dernières
/// minutes d'une classée en ferait une partie « trop courte » pour une
/// relance, et l'annonce attendrait le périodique.
pub fn transition_de_jeu(
    ancien: Option<ki_protocol::JeuStatut>,
    nouveau: &Option<ki_protocol::JeuStatut>,
    depuis: Option<Instant>,
    maintenant: Instant,
) -> TransitionJeu {
    let en_jeu = |j: Option<&ki_protocol::JeuStatut>| {
        j.is_some_and(|j| j.est_valorant() && j.etat == ki_protocol::JeuEtat::EnJeu)
    };
    let (avant, apres) = (en_jeu(ancien.as_ref()), en_jeu(nouveau.as_ref()));
    // Un menu observé (ou un autre jeu) clôt la partie ; une présence
    // perdue peut n'être qu'un hoquet, l'heure d'entrée reste.
    let garde = if !apres && nouveau.is_some() { None } else { depuis };
    if avant && !apres {
        let duree = depuis.map_or(Duration::MAX, |d| maintenant.saturating_duration_since(d));
        TransitionJeu {
            fin: ancien.map(|a| (a, duree)),
            reprise: false,
            en_jeu_depuis: garde,
        }
    } else if !avant && apres {
        match (ancien.is_some(), depuis) {
            // Du menu à la partie : une vraie entrée.
            (true, _) => TransitionJeu {
                fin: None,
                reprise: true,
                en_jeu_depuis: Some(maintenant),
            },
            // Présence perdue puis revenue : le hoquet, l'heure d'origine
            // reste.
            (false, Some(d)) => TransitionJeu {
                fin: None,
                reprise: true,
                en_jeu_depuis: Some(d),
            },
            // Première présence d'une session : on ne sait rien.
            (false, None) => TransitionJeu::default(),
        }
    } else {
        TransitionJeu {
            fin: None,
            reprise: false,
            en_jeu_depuis: garde,
        }
    }
}

impl Fil {
    /// La seule porte vers un `Travail::Rafraichir`. Un membre déjà en
    /// file ne l'est pas deux fois : le motif le plus informatif reste (le
    /// fil le relit en commençant, voir [`Fil::prendre`]). Un membre dont
    /// la relance est programmée n'a rien à faire en file non plus : elle
    /// viendra. Rend `true` si un travail est parti.
    fn pousser(&self, user_id: UserId, motif: Motif, travaux: &Sender<Travail>) -> bool {
        let mut en_file = self.en_file.lock().unwrap();
        if let Some(existant) = en_file.get_mut(&user_id) {
            if motif.poids() > existant.poids() {
                *existant = motif;
            }
            return false;
        }
        if motif.relance().is_none() && self.relances.lock().unwrap().contains_key(&user_id) {
            return false;
        }
        en_file.insert(user_id, motif);
        if travaux.send(Travail::Rafraichir { user_id, motif }).is_err() {
            en_file.remove(&user_id);
            return false;
        }
        true
    }

    /// Le fil commence un rafraîchissement : l'entrée sort de la file, et
    /// c'est le motif qu'elle porte — fusionné depuis — qui compte.
    fn prendre(&self, user_id: UserId, motif: Motif) -> Motif {
        self.en_file.lock().unwrap().remove(&user_id).unwrap_or(motif)
    }

    /// Tout ce que la fiche contient est réputé connu : à la liaison, le
    /// fil commence aux parties à venir.
    fn connaitre(&self, user_id: UserId, fiche: &FicheValorant) {
        let ids = fiche.matchs.iter().map(|m| m.id.clone()).collect();
        self.annonces.lock().unwrap().insert(user_id, ids);
    }

    /// Les matchs de la fiche jamais vus : notés, et mis en attente
    /// d'annonce s'ils s'annoncent (mode, âge). Les coéquipiers du groupe
    /// (`co` : match → membres) sont attendus, et relus aussitôt — sauf
    /// s'ils sont déjà en file ou en relance, auquel cas leur lecture
    /// vient de toute façon. Rend le nombre de matchs nouveaux, annoncés
    /// ou non.
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
                    self.pousser(autre, Motif::CoMembre, travaux);
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

// ---------------------------------------------------------------------
// Le récap de la semaine
// ---------------------------------------------------------------------

/// Un membre dans le récap : où il en est, ce qu'il a gagné, ce qu'il a
/// joué, et son meilleur match.
#[derive(Debug, Clone, PartialEq)]
pub struct LigneRecap {
    pub user_id: UserId,
    pub rang: RangValorant,
    pub rr_gagnes: i32,
    pub matchs: u32,
    pub victoires: u32,
    pub defaites: u32,
    pub meilleur: Option<MatchResume>,
}

/// Le récap hebdo du fil de jeu : dimanche soir, ce que le groupe a joué
/// depuis dimanche dernier — du plus grand gain de RR au plus petit.
#[derive(Debug, Clone, PartialEq)]
pub struct Recap {
    pub depuis_ms: u64,
    pub jusqu_a_ms: u64,
    pub lignes: Vec<LigneRecap>,
    /// Les membres liés qui n'ont pas joué.
    pub sans_match: Vec<UserId>,
}

/// Le récap d'après les fiches, sur `[depuis_ms, jusqu_a_ms[`. `None` si
/// personne n'a joué : rien à dire.
pub fn recap_de(fiches: &BTreeMap<UserId, FicheValorant>, depuis_ms: u64, jusqu_a_ms: u64) -> Option<Recap> {
    let dans = |date: u64| date >= depuis_ms && date < jusqu_a_ms;
    let mut lignes = Vec::new();
    let mut sans_match = Vec::new();
    for (id, f) in fiches {
        let matchs: Vec<&MatchResume> = f.matchs.iter().filter(|m| dans(m.date)).collect();
        if matchs.is_empty() {
            sans_match.push(*id);
            continue;
        }
        let rr_gagnes: i32 = f.historique_rr.iter().filter(|p| dans(p.date)).map(|p| p.delta).sum();
        lignes.push(LigneRecap {
            user_id: *id,
            rang: f.rang.clone(),
            rr_gagnes,
            matchs: matchs.len() as u32,
            victoires: matchs.iter().filter(|m| m.gagne == Some(true)).count() as u32,
            defaites: matchs.iter().filter(|m| m.gagne == Some(false)).count() as u32,
            meilleur: matchs.iter().max_by_key(|m| (m.kills, m.score)).map(|m| (*m).clone()),
        });
    }
    if lignes.is_empty() {
        return None;
    }
    lignes.sort_by(|a, b| b.rr_gagnes.cmp(&a.rr_gagnes).then(b.matchs.cmp(&a.matchs)));
    Some(Recap { depuis_ms, jusqu_a_ms, lignes, sans_match })
}

/// Le texte du récap : un titre daté, une ligne par membre qui a joué, et
/// ceux qui n'ont pas joué en bas.
pub fn composer_recap(r: &Recap, pseudo: impl Fn(UserId) -> String) -> String {
    let mut texte = format!(
        "📅 La semaine du groupe, du {} au {}",
        jour(r.depuis_ms),
        jour(r.jusqu_a_ms.saturating_sub(1))
    );
    for (i, l) in r.lignes.iter().enumerate() {
        let signe = if l.rr_gagnes >= 0 { "+" } else { "" };
        let pluriel = if l.matchs > 1 { "s" } else { "" };
        texte.push_str(&format!(
            "\n{}. {} — {} {} RR · {signe}{} RR · {} match{pluriel} ({} V / {} D)",
            i + 1,
            pseudo(l.user_id),
            ki_protocol::nom_de_rang(l.rang.tier),
            l.rang.rr,
            l.rr_gagnes,
            l.matchs,
            l.victoires,
            l.defaites
        ));
        if let Some(m) = &l.meilleur {
            texte.push_str(&format!(
                " · meilleur : {} {}/{}/{} sur {}",
                m.agent, m.kills, m.deaths, m.assists, m.carte
            ));
            // Depuis que les manches se lisent, un ace se dit.
            if m.manches_detail.as_ref().is_some_and(|d| d.aces > 0) {
                texte.push_str(" (avec un ace)");
            }
        }
    }
    if !r.sans_match.is_empty() {
        let noms: Vec<String> = r.sans_match.iter().map(|id| pseudo(*id)).collect();
        texte.push_str(&format!("\nPas de match cette semaine : {}", noms.join(", ")));
    }
    texte
}

/// « 8 septembre », depuis des millisecondes Unix. En UTC : à une heure
/// près, la soirée est la même à Paris.
fn jour(ms: u64) -> String {
    use chrono::Datelike;
    const MOIS: [&str; 12] = [
        "janvier", "février", "mars", "avril", "mai", "juin",
        "juillet", "août", "septembre", "octobre", "novembre", "décembre",
    ];
    match chrono::DateTime::from_timestamp_millis(ms as i64) {
        Some(d) => format!("{} {}", d.day(), MOIS[(d.month0() as usize).min(11)]),
        None => "?".into(),
    }
}

/// Dimanche, à partir de 19 h 30 UTC — 21 h 30 à Paris l'été, 20 h 30
/// l'hiver : le moment où la semaine se raconte.
pub fn heure_du_recap(ms: u64) -> bool {
    use chrono::{Datelike, Timelike, Weekday};
    let Some(d) = chrono::DateTime::from_timestamp_millis(ms as i64) else {
        return false;
    };
    d.weekday() == Weekday::Sun && (d.hour() > 19 || (d.hour() == 19 && d.minute() >= 30))
}

// ---------------------------------------------------------------------
// La page du groupe
// ---------------------------------------------------------------------

/// Ce que la page du groupe reçoit d'un membre sans porter ses soixante
/// matchs : les bilans à sept et trente jours, la forme, la série, les
/// agents, cartes et duos du mois — tout par les formules de
/// `ki-protocol`, classé seulement. Calculé à l'envoi, jamais stocké.
fn bilan_membre(f: &FicheValorant, maintenant: u64) -> BilanMembre {
    let sept = maintenant.saturating_sub(7 * JOUR_MS);
    let trente = maintenant.saturating_sub(30 * JOUR_MS);
    let compte = |(nom, b): (String, ki_protocol::Bilan)| (nom, b.matchs, b.victoires);
    BilanMembre {
        sept_jours: f.bilan(sept, u64::MAX, true),
        trente_jours: f.bilan(trente, u64::MAX, true),
        serie: f.serie(),
        forme: f.forme(10),
        agents: f.par_agent(trente, u64::MAX, true).into_iter().take(3).map(compte).collect(),
        cartes: f.par_carte(trente, u64::MAX, true).into_iter().take(5).map(compte).collect(),
        duos: f.duos(trente, u64::MAX).into_iter().take(5).collect(),
    }
}

/// Les 168 cases `[jour UTC 0 = lundi … 6][heure 0..24]` des parties
/// commencées depuis trente jours, tous membres et modes. Le jour se
/// calcule sans calendrier : le 1er janvier 1970 était un jeudi, soit le
/// jour 3 d'une semaine qui commence le lundi. Vide si tout est à zéro.
fn activite_de<'a>(fiches: impl Iterator<Item = &'a FicheValorant>, maintenant: u64) -> Vec<u16> {
    let depuis = maintenant.saturating_sub(30 * JOUR_MS);
    let mut cases = vec![0u16; 7 * 24];
    for m in fiches.flat_map(|f| f.matchs.iter()) {
        if m.date < depuis {
            continue;
        }
        let jour = ((m.date / JOUR_MS + 3) % 7) as usize;
        let heure = ((m.date / 3_600_000) % 24) as usize;
        if let Some(c) = cases.get_mut(jour * 24 + heure) {
            *c = c.saturating_add(1);
        }
    }
    if cases.iter().all(|c| *c == 0) {
        Vec::new()
    } else {
        cases
    }
}

/// Le message de la page du groupe, sous le budget d'une ligne : cinq
/// matchs et dix points par membre ; si la ligne dépasse
/// [`STATS_MAX_BYTES`], on allège tout le monde d'un cran — jamais un
/// membre de moins. `resumes(n_matchs, n_points)` rend les fiches au
/// palier demandé. Si même à zéro match ça ne tient pas, le message part
/// sans esports ni activité : les fiches d'abord.
pub fn message_stats(
    resumes: impl Fn(usize, usize) -> Vec<FicheMembre>,
    esports: Vec<MatchEsport>,
    activite: Vec<u16>,
) -> ServerMsg {
    const PALIERS: [(usize, usize); 4] = [(MATCHS_MAX, POINTS_RESUME), (3, 6), (1, 3), (0, 0)];
    for (n_m, n_p) in PALIERS {
        let candidat = ServerMsg::StatsValorant {
            fiches: resumes(n_m, n_p),
            esports: esports.clone(),
            activite: activite.clone(),
        };
        if let Ok(octets) = serde_json::to_vec(&candidat) {
            if octets.len() <= STATS_MAX_BYTES {
                tracing::debug!(
                    "VALORANT : page du groupe en {} octets, {n_m} matchs et {n_p} points par membre",
                    octets.len()
                );
                return candidat;
            }
        }
    }
    tracing::warn!("VALORANT : la page du groupe dépasse le budget même sans match : envoyée sans esports ni activité");
    ServerMsg::StatsValorant {
        fiches: resumes(0, 0),
        esports: Vec::new(),
        activite: Vec::new(),
    }
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
/// Et quand HenrikDev a dit 429, le seau est **gelé** jusqu'à l'heure
/// qu'il a donnée : tout le fil attend, pas seulement la requête refusée.
struct Seau {
    passees: VecDeque<Instant>,
    gel_jusqua: Option<Instant>,
}

impl Seau {
    fn new() -> Self {
        Self {
            passees: VecDeque::with_capacity(BUDGET_PAR_MINUTE),
            gel_jusqua: None,
        }
    }

    fn prendre(&mut self) {
        if let Some(fin) = self.gel_jusqua.take() {
            let reste = fin.saturating_duration_since(Instant::now());
            if !reste.is_zero() {
                std::thread::sleep(reste);
            }
        }
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

    /// Gèle le seau pour `duree` — plafonnée, l'en-tête vient du réseau.
    /// Un gel plus long déjà posé reste.
    fn geler(&mut self, duree: Duration) {
        let fin = Instant::now() + duree.min(GEL_MAX);
        if self.gel_jusqua.is_none_or(|f| f < fin) {
            self.gel_jusqua = Some(fin);
        }
    }

    /// Gelé, et jusqu'à quand.
    fn gele(&self) -> Option<Duration> {
        self.gel_jusqua
            .map(|f| f.saturating_duration_since(Instant::now()))
            .filter(|d| !d.is_zero())
    }
}

/// `Retry-After` d'un 429 : des secondes, ou une date HTTP (RFC 2822).
/// `None` s'il manque ou ne se lit pas.
fn lire_retry_after(valeur: Option<&str>) -> Option<Duration> {
    let v = valeur?.trim();
    if let Ok(s) = v.parse::<u64>() {
        return Some(Duration::from_secs(s));
    }
    let date = chrono::DateTime::parse_from_rfc2822(v).ok()?;
    let reste = date.timestamp_millis().saturating_sub(chrono::Utc::now().timestamp_millis());
    Some(Duration::from_millis(reste.max(0) as u64))
}

enum Erreur {
    Introuvable,
    /// Trop de requêtes : le seau est gelé, on ne réessaie pas.
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
    /// Pourquoi les prochaines requêtes partent — posé par le fil avant
    /// chaque travail, lu par le journal.
    motif: Motif,
}

impl Api {
    /// Une requête, une seule : un 429 gèle le seau le temps que
    /// HenrikDev demande (`Retry-After`, sinon [`GEL_DEFAUT`]) et rend
    /// `Limite` — le travail en cours s'arrête là, le suivant attendra la
    /// fin du gel avant de partir.
    fn get(&mut self, chemin: &str) -> Result<Value, Erreur> {
        let url = format!("{BASE}{chemin}");
        self.seau.prendre();
        let point = PointApi::de(chemin);
        let depart = Instant::now();
        let ms = maintenant_ms();
        let reponse = self.agent.get(&url).set("Authorization", &self.cle).call();
        let code = match &reponse {
            Ok(_) => 200,
            Err(ureq::Error::Status(code, _)) => *code,
            Err(_) => 0,
        };
        self.compteurs.noter(Requete {
            ms,
            motif: self.motif,
            point,
            code,
            duree_ms: depart.elapsed().as_millis().min(u128::from(u32::MAX)) as u32,
        });
        match reponse {
            Ok(reponse) => reponse
                .into_json::<Value>()
                .map_err(|e| Erreur::Autre(e.to_string())),
            Err(ureq::Error::Status(404, _)) => Err(Erreur::Introuvable),
            Err(ureq::Error::Status(429, reponse)) => {
                let gel = lire_retry_after(reponse.header("Retry-After")).unwrap_or(GEL_DEFAUT);
                self.seau.geler(gel);
                tracing::warn!(
                    "VALORANT : HenrikDev renvoie 429 ({}, {}) — fil gelé {} s",
                    self.motif.nom(),
                    point.nom(),
                    self.seau.gele().unwrap_or_default().as_secs()
                );
                Err(Erreur::Limite)
            }
            Err(ureq::Error::Status(code, reponse)) => {
                let detail = reponse
                    .into_json::<Value>()
                    .ok()
                    .and_then(|v| v["errors"][0]["message"].as_str().map(str::to_string))
                    .unwrap_or_default();
                Err(Erreur::Autre(
                    format!("HTTP {code} {detail}").trim().to_string(),
                ))
            }
            Err(e) => Err(Erreur::Autre(e.to_string())),
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
        motif: Motif::Periodique,
    };
    for travail in rx {
        match travail {
            Travail::Lier { user_id, nom, tag } => {
                api.motif = Motif::Liaison;
                let resultat = lier(&mut api, &etat, user_id, &nom, &tag);
                // La place en file se rend une fois répondu : pendant les
                // six requêtes, un second clic est refusé.
                etat.fil.liaisons_en_file.lock().unwrap().remove(&user_id);
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
                api.motif = Motif::Esports;
                // Le calendrier complet d'abord ; s'il tombe (HenrikDev a
                // renvoyé 500 sur l'ensemble un soir de septembre 2026), la
                // seule région qui nous intéresse, puis l'international.
                // Même raté, il est daté de maintenant : pas de nouvel essai
                // avant une heure — et une seule ligne de journal pour les
                // trois essais. Un 429 arrête tout : chaque variante de
                // plus attendrait la fin du gel pour se faire refuser à son
                // tour, et les relances derrière dériveraient d'autant.
                let mut matchs = None;
                let mut echecs = Vec::new();
                let mut sature = false;
                for filtre in ["", "?region=emea", "?region=international"] {
                    match api.get(&format!("/valorant/v1/esports/schedule{filtre}")) {
                        Ok(v) => {
                            matchs = Some(calendrier_esport(&v, maintenant_ms()));
                            break;
                        }
                        Err(Erreur::Limite) => {
                            echecs.push(Erreur::Limite.message());
                            sature = true;
                            break;
                        }
                        Err(e) => echecs.push(e.message()),
                    }
                }
                // La source officielle en panne, VLR prend le relais : les
                // événements à venir ou en cours, puis leurs matchs.
                if matchs.is_none() && !sature {
                    match calendrier_vlr(&mut api, maintenant_ms()) {
                        Ok(liste) if !liste.is_empty() => {
                            tracing::info!("VALORANT : calendrier esport lu chez VLR ({} matchs)", liste.len());
                            matchs = Some(liste);
                        }
                        Ok(_) => echecs.push("VLR : rien à venir".into()),
                        Err(e) => echecs.push(format!("VLR : {}", e.message())),
                    }
                }
                if matchs.is_none() {
                    echecs.dedup();
                    tracing::warn!(
                        "VALORANT : calendrier esport illisible chez HenrikDev ({}) — nouvel essai dans une heure",
                        echecs.join(" ; ")
                    );
                }
                let matchs = matchs.unwrap_or_else(|| etat.esports.lock().unwrap().1.clone());
                *etat.esports.lock().unwrap() = (maintenant_ms(), matchs);
                etat.esports_en_cours.store(false, Ordering::Relaxed);
            }
            Travail::Rafraichir { user_id, motif } => {
                // Le motif qui compte est celui de la file, fusionné depuis
                // l'envoi : un périodique devenu relance entre-temps relit
                // en relance.
                let motif = etat.fil.prendre(user_id, motif);
                let compte = etat.comptes.lock().unwrap().get(&user_id).cloned();
                let Some(compte) = compte else { continue };
                let maj = etat.fiches.lock().unwrap().get(&user_id).map(|f| f.maj);
                if relecture_inutile(motif, maj, maintenant_ms()) {
                    tracing::debug!(
                        "VALORANT : fiche de {} fraîche, relecture {} sautée",
                        compte.riot_id(),
                        motif.nom()
                    );
                    continue;
                }
                api.motif = motif;
                let lies = etat.lies();
                let resultat = construire(&mut api, &compte, None, &lies);
                if let Some(r) = apres_lecture(&etat, user_id, &compte, motif, resultat, &travaux) {
                    let _ = tx.send(r);
                }
            }
        }
    }
}

/// Fraîcheur : une fiche relue il y a moins de [`FRAICHEUR`] ne se relit
/// pas pour un périodique — la lecture qui vient de finir a déjà vu ce
/// qu'il y avait à voir. Tout autre motif passe : une relance attend un
/// match précis, un co-membre porte l'information qu'une annonce
/// l'attend (et s'il venait d'être relu *avant* que le match soit indexé,
/// c'est justement lui qu'il faut relire — sinon l'annonce sort sans sa
/// ligne, et son périodique la redit trente minutes plus tard). La
/// tempête du 5-stack, elle, est retenue en amont par [`Fil::pousser`].
fn relecture_inutile(motif: Motif, maj: Option<u64>, maintenant: u64) -> bool {
    motif == Motif::Periodique
        && maj.is_some_and(|maj| maintenant.saturating_sub(maj) < FRAICHEUR.as_millis() as u64)
}

/// Ce que le fil fait d'une lecture, réussie ou non. Réussie : l'échec
/// antérieur s'oublie, les matchs nouveaux partent vers l'annonce, la
/// fiche se fusionne et s'écrit, et une relance sans rien de neuf se
/// reprogramme tant que la fin de partie l'a jugé utile. Ratée : l'échec
/// est noté, fiche ou pas, pour que `a_rafraichir` n'y revienne pas
/// chaque minute — et **pas** de relance : sur un 429, elle relancerait
/// précisément quand HenrikDev sature ; le périodique reprendra la fin de
/// partie. Rend ce qu'il y a à relayer aux clients.
fn apres_lecture(
    etat: &Etat,
    user_id: UserId,
    compte: &CompteRiot,
    motif: Motif,
    resultat: Result<(FicheValorant, CoMembres), Erreur>,
    travaux: &Sender<Travail>,
) -> Option<Resultat> {
    match resultat {
        Ok((fiche, co)) => {
            etat.fil.derniere_tentative.lock().unwrap().remove(&user_id);
            // `nouveaux` se juge sur la fiche fraîche : avec soixante
            // matchs accumulés, d'anciens ids redeviendraient « nouveaux »
            // et la relance de fin de partie (nouveaux == 0) ne partirait
            // plus.
            let nouveaux = etat.fil.nouveaux(user_id, &fiche, &co, travaux);
            if nouveaux > 0 {
                etat.sauver_fil();
            }
            {
                let mut fiches = etat.fiches.lock().unwrap();
                let fiche = match fiches.remove(&user_id) {
                    Some(ancienne) => fusionner(ancienne, fiche),
                    None => fiche,
                };
                fiches.insert(user_id, fiche);
            }
            etat.sauver_fiches();
            // Rien de neuf après une fin de partie : HenrikDev n'a pas
            // encore le match, on relira.
            if let Motif::Relance { essai, max } = motif {
                if nouveaux == 0 && essai + 1 < max {
                    etat.fil.relances.lock().unwrap().insert(
                        user_id,
                        Relance {
                            quand: Instant::now() + RELANCE_DELAI,
                            essai: essai + 1,
                            max,
                        },
                    );
                }
            }
            Some(Resultat::Fiche { user_id })
        }
        Err(e) => {
            tracing::warn!(
                "VALORANT : fiche de {} non rafraîchie ({}) : {}",
                compte.riot_id(),
                motif.nom(),
                e.message()
            );
            etat.fil
                .derniere_tentative
                .lock()
                .unwrap()
                .insert(user_id, maintenant_ms());
            None
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
    // Si le membre relie le même compte (ou se renomme : même puuid), sa
    // fiche accumulée reste ; un autre compte repart de zéro. Ça se lit
    // avant d'écrire le nouveau compte — et avant le rattrapage, parce
    // que les trois couches s'empilent dans un ordre précis (voir
    // `empiler_a_la_liaison`). L'ancienne fiche reste en place le temps
    // des deux requêtes d'archive : rien ne disparaît pendant qu'on
    // attend HenrikDev.
    let ancien_puuid = etat.comptes.lock().unwrap().get(&user_id).map(|c| c.puuid.clone());
    let ancienne = etat.fiches.lock().unwrap().get(&user_id).map(|f| (ancien_puuid.unwrap_or_default(), f.clone()));
    let archive = rattraper(api, &compte);
    let fiche = empiler_a_la_liaison(ancienne, &compte.puuid, fiche, archive);
    etat.comptes.lock().unwrap().insert(user_id, compte.clone());
    etat.fiches.lock().unwrap().insert(user_id, fiche.clone());
    // Ses matchs d'avant la liaison — rattrapés compris — ne s'annoncent
    // pas.
    etat.fil.connaitre(user_id, &fiche);
    etat.sauver_comptes();
    etat.sauver_fiches();
    etat.sauver_fil();
    Ok(fiche)
}

/// À la liaison, l'ancienne fiche n'est gardée que si c'est le même
/// compte Riot — au puuid, pas au Riot ID, pour qu'un joueur qui se
/// renomme garde son historique. Sinon la neuve remplace tout.
fn fusion_a_la_liaison(
    ancienne: Option<(String, FicheValorant)>,
    puuid: &str,
    neuve: FicheValorant,
) -> FicheValorant {
    match ancienne {
        Some((ancien_puuid, ancienne)) if ancien_puuid == puuid => fusionner(ancienne, neuve),
        _ => neuve,
    }
}

/// Les trois couches d'une liaison, de la plus forte à la plus faible :
/// la fiche fraîche des trois requêtes classiques (elle porte le détail
/// des manches des cinq derniers matchs), puis la fiche accumulée sur le
/// disque si c'est le même compte (ses matchs ont été résumés en leur
/// temps avec leurs manches, leurs co-membres, leur party et leur durée),
/// et tout en bas l'archive de HenrikDev, qui ne sait rien de tout ça —
/// elle ne fait que combler les trous. Empilée au-dessus de la fiche
/// accumulée, l'archive écraserait cinquante matchs détaillés par leur
/// version muette à chaque re-liaison : c'est l'ordre qui protège.
fn empiler_a_la_liaison(
    ancienne: Option<(String, FicheValorant)>,
    puuid: &str,
    fraiche: FicheValorant,
    archive: FicheValorant,
) -> FicheValorant {
    let fiche = fusion_a_la_liaison(ancienne, puuid, fraiche);
    if archive.matchs.is_empty() && archive.historique_rr.is_empty() {
        return fiche;
    }
    fusionner(archive, fiche)
}

/// Le rattrapage, à la liaison seulement : les archives de HenrikDev —
/// soixante matchs (`v1/stored-matches`, sans plateforme dans le chemin)
/// et cent points de RR (`v2/stored-mmr-history`) — rendues comme une
/// fiche qui n'a que ça, à ranger sous les autres par
/// [`empiler_a_la_liaison`]. Deux requêtes tolérées : une archive muette
/// rend une fiche vide, qui ne change rien.
fn rattraper(api: &mut Api, compte: &CompteRiot) -> FicheValorant {
    let matchs = api
        .get(&format!(
            "/valorant/v1/stored-matches/{}/{}/{}?size={RATTRAPAGE_MATCHS}",
            compte.region,
            enc(&compte.nom),
            enc(&compte.tag)
        ))
        .unwrap_or(Value::Null);
    let points = api
        .get(&format!(
            "/valorant/v2/stored-mmr-history/{}/{}/{}/{}?size={RATTRAPAGE_POINTS}",
            compte.region,
            compte.plateforme,
            enc(&compte.nom),
            enc(&compte.tag)
        ))
        .unwrap_or(Value::Null);
    let archive = fiche_archivee(&matchs, &points);
    if !archive.matchs.is_empty() || !archive.historique_rr.is_empty() {
        tracing::info!(
            "VALORANT : {} rattrapé — {} matchs et {} points archivés",
            compte.riot_id(),
            archive.matchs.len(),
            archive.historique_rr.len()
        );
    }
    archive
}

/// Les deux réponses d'archive réduites à une fiche qui n'a que des
/// matchs et des points — tout le reste à zéro, pour passer sous
/// [`fusionner`] comme une « ancienne » fiche.
fn fiche_archivee(matchs: &Value, points: &Value) -> FicheValorant {
    FicheValorant {
        matchs: matchs["data"]
            .as_array()
            .map(|l| l.iter().filter_map(resumer_match_stocke).take(RATTRAPAGE_MATCHS).collect())
            .unwrap_or_default(),
        historique_rr: points["data"]
            .as_array()
            .map(|l| l.iter().take(RATTRAPAGE_POINTS).map(point_rr).collect())
            .unwrap_or_default(),
        ..Default::default()
    }
}

/// La fiche fusionne l'ancienne et la neuve : les scalaires de la neuve
/// (sauf un niveau à zéro ou des saisons vides, qui gardent l'ancien),
/// l'union des matchs par `id` — la neuve gagne, elle porte les champs
/// de 0.1.40 — et l'union des points, par `match_id` ou, s'il manque,
/// par `(date, tier, rr)`. Tout est trié du plus récent au plus ancien et
/// plafonné à [`MATCHS_GARDES`] et [`HISTORIQUE_GARDES`]. Une neuve sans
/// match (un `v4/matches` en 429) laisse les anciens en place ; idem pour
/// l'historique.
fn fusionner(ancienne: FicheValorant, neuve: FicheValorant) -> FicheValorant {
    let mut fiche = neuve;
    if fiche.niveau == 0 {
        fiche.niveau = ancienne.niveau;
    }
    if fiche.saisons.is_empty() {
        fiche.saisons = ancienne.saisons;
    }
    // Un match d'id vide n'est comparable à rien : il reste des deux côtés.
    let connus: BTreeSet<String> = fiche
        .matchs
        .iter()
        .filter(|m| !m.id.is_empty())
        .map(|m| m.id.clone())
        .collect();
    fiche
        .matchs
        .extend(ancienne.matchs.into_iter().filter(|m| m.id.is_empty() || !connus.contains(&m.id)));
    fiche.matchs.sort_by_key(|m| std::cmp::Reverse(m.date));
    fiche.matchs.truncate(MATCHS_GARDES);

    let cle = |p: &PointRR| -> (String, u64, u8, u16) {
        if p.match_id.is_empty() {
            (String::new(), p.date, p.tier, p.rr)
        } else {
            (p.match_id.clone(), 0, 0, 0)
        }
    };
    let mut cles: BTreeSet<(String, u64, u8, u16)> = fiche.historique_rr.iter().map(cle).collect();
    fiche
        .historique_rr
        .extend(ancienne.historique_rr.into_iter().filter(|p| cles.insert(cle(p))));
    fiche.historique_rr.sort_by_key(|p| std::cmp::Reverse(p.date));
    fiche.historique_rr.truncate(HISTORIQUE_GARDES);
    fiche
}

/// Trois requêtes : rang, historique de RR, derniers matchs. Rend aussi,
/// par match, les autres membres liés (`lies` : membre → puuid) qui y
/// jouaient — reconnus à leur puuid, sans rien garder des neuf autres.
/// Sans le rang, pas de fiche ; sans l'historique ou les matchs, une
/// fiche qui en manque (la fusion gardera les anciens) — sauf en 429, qui
/// est une erreur franche : une fiche sans match sous 429 passerait pour
/// « rien de neuf » et nourrirait la relance au moment où HenrikDev
/// sature.
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
    let tolere = |r: Result<Value, Erreur>| match r {
        Ok(v) => Ok(v),
        Err(Erreur::Limite) => Err(Erreur::Limite),
        Err(_) => Ok(Value::Null),
    };
    let mmr = api.get(&format!("/valorant/v3/mmr/{suffixe}"))?;
    let historique = tolere(api.get(&format!("/valorant/v2/mmr-history/{suffixe}")))?;
    let matchs = tolere(api.get(&format!("/valorant/v4/matches/{suffixe}?size={MATCHS_MAX}")))?;

    let mut fiche = FicheValorant {
        riot_id: compte.riot_id(),
        region: compte.region.clone(),
        plateforme: compte.plateforme.clone(),
        niveau: niveau.unwrap_or(0),
        maj: maintenant_ms(),
        ..Default::default()
    };
    // Les actes joués, dans l'ordre de l'API (du plus ancien au plus
    // récent) ; une entrée sans `season.short` ne dit pas quel acte, on
    // la saute. Le dernier donne la saison du rang courant.
    fiche.saisons = mmr["data"]["seasonal"]
        .as_array()
        .map(|l| {
            l.iter()
                .filter_map(|s| {
                    let saison = s["season"]["short"].as_str().filter(|s| !s.is_empty())?;
                    Some(StatsSaison {
                        saison: saison.to_string(),
                        victoires: u16_sature(s["wins"].as_u64().unwrap_or(0)),
                        parties: u16_sature(s["games"].as_u64().unwrap_or(0)),
                        tier_fin: u8_sature(s["end_tier"]["id"].as_u64().unwrap_or(0)),
                        rr_fin: u16_sature(s["end_rr"].as_u64().unwrap_or(0)),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let courant = &mmr["data"]["current"];
    fiche.rang = RangValorant {
        tier: courant["tier"]["id"].as_u64().unwrap_or(0) as u8,
        rr: courant["rr"].as_u64().unwrap_or(0) as u16,
        delta: courant["last_change"].as_i64().unwrap_or(0) as i32,
        elo: courant["elo"].as_u64().unwrap_or(0) as u32,
        saison: fiche.saisons.last().map(|s| s.saison.clone()).unwrap_or_default(),
        placements_restants: u8_sature(courant["games_needed_for_rating"].as_u64().unwrap_or(0)),
        boucliers: u8_sature(courant["rank_protection_shields"].as_u64().unwrap_or(0)),
        classement: u32_sature(courant["leaderboard_placement"]["rank"].as_u64().unwrap_or(0)),
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
                ..Default::default()
            });
        }
    }
    fiche.historique_rr = historique["data"]["history"]
        .as_array()
        .map(|h| h.iter().take(HISTORIQUE_MAX).map(point_rr).collect())
        .unwrap_or_default();
    fiche.matchs = matchs["data"]
        .as_array()
        .map(|liste| {
            liste
                .iter()
                .filter_map(|m| resumer_match(m, &compte.puuid, lies))
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

/// Le calendrier par VLR (esports v2 de HenrikDev) : les événements en
/// cours ou à venir, puis les matchs des cinq premiers — six requêtes au
/// plus, une fois par heure, seulement quand la source officielle tombe.
fn calendrier_vlr(api: &mut Api, maintenant: u64) -> Result<Vec<MatchEsport>, Erreur> {
    let evenements = api.get("/valorant/v2/esports/vlr/events?type=upcoming")?;
    let mut candidats: Vec<(u64, String, String, bool)> = evenements["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let statut = e["status"].as_str().unwrap_or("");
            if statut != "ongoing" && statut != "upcoming" {
                return None;
            }
            Some((
                e["id"].as_u64()?,
                e["title"].as_str().unwrap_or("").to_string(),
                region_vlr(e["region"].as_str().unwrap_or("")),
                statut == "ongoing",
            ))
        })
        .collect();
    // Les événements en cours d'abord : c'est là que sont les matchs du soir.
    candidats.sort_by_key(|(_, _, _, en_cours)| !en_cours);
    candidats.truncate(5);
    let mut matchs = Vec::new();
    let mut date_illisible: Option<String> = None;
    for (id, titre, region, _) in candidats {
        let reponse = api.get(&format!("/valorant/v2/esports/vlr/events/{id}/matches"))?;
        for m in reponse["data"].as_array().into_iter().flatten() {
            let brut = m["date"].as_str().unwrap_or("");
            let date = iso_vers_ms(brut);
            if date == 0 {
                if !brut.is_empty() {
                    date_illisible.get_or_insert_with(|| brut.to_string());
                }
                continue;
            }
            if date + 3 * 3_600_000 < maintenant {
                continue;
            }
            let equipes: Vec<String> = m["teams"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| t["name"].as_str().filter(|n| !n.is_empty() && *n != "TBD").map(str::to_string))
                .take(2)
                .collect();
            if equipes.len() < 2 {
                continue;
            }
            let commence = m["teams"].as_array().is_some_and(|ts| ts.iter().any(|t| t["score"].as_u64().is_some()));
            matchs.push(MatchEsport {
                date,
                ligue: titre.clone(),
                region: region.clone(),
                tournoi: m["series"].as_str().unwrap_or("").to_string(),
                equipes,
                etat: if commence && date <= maintenant { "inProgress" } else { "unstarted" }.to_string(),
                format: String::new(),
            });
        }
    }
    if matchs.is_empty() {
        if let Some(d) = date_illisible {
            tracing::warn!("VALORANT : VLR date un match « {d} », un format qu'on ne lit pas");
        }
    }
    matchs.sort_by_key(|m| m.date);
    matchs.truncate(ESPORTS_MAX);
    Ok(matchs)
}

/// Les régions de VLR, en clair.
fn region_vlr(r: &str) -> String {
    match r {
        "europe" => "EMEA",
        "north_america" => "Amériques",
        "asia_pacific" => "Pacifique",
        "brazil" => "Brésil",
        "korea" => "Corée",
        "japan" => "Japon",
        "latin_america" => "Amérique latine",
        "oceania" => "Océanie",
        "mena" => "MENA",
        "gc" => "Game Changers",
        "collegiate" => "Universitaire",
        "" => "",
        autre => autre,
    }
    .to_string()
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

/// Un point de RR tel que `v2/mmr-history` et `v2/stored-mmr-history` le
/// donnent — la même forme aux deux endroits.
fn point_rr(p: &Value) -> PointRR {
    PointRR {
        match_id: p["match_id"].as_str().unwrap_or("").to_string(),
        date: iso_vers_ms(p["date"].as_str().unwrap_or("")),
        tier: u8_sature(p["tier"]["id"].as_u64().unwrap_or(0)),
        rr: u16_sature(p["rr"].as_u64().unwrap_or(0)),
        delta: p["last_change"].as_i64().unwrap_or(0).clamp(i32::MIN as i64, i32::MAX as i64) as i32,
        carte: p["map"]["name"].as_str().unwrap_or("").to_string(),
        saison: p["season"]["short"].as_str().unwrap_or("").to_string(),
        protege: p["was_derank_protected"].as_bool().unwrap_or(false),
    }
}

/// Des nombres qui viennent du réseau : ils entrent dans le type prévu en
/// butant sur son plafond, jamais en repartant de zéro.
fn u8_sature(v: u64) -> u8 {
    v.min(u64::from(u8::MAX)) as u8
}

fn u16_sature(v: u64) -> u16 {
    v.min(u64::from(u16::MAX)) as u16
}

fn u32_sature(v: u64) -> u32 {
    v.min(u64::from(u32::MAX)) as u32
}

/// La ligne du membre dans un match — et rien des autres joueurs, sinon
/// ce qu'on en déduit : l'effectif de sa party, et les autres membres du
/// groupe (`lies` : membre → puuid) reconnus dans son camp ou en face,
/// notés par leur `UserId`. Un match pas fini (`is_completed == false`)
/// n'est pas résumé.
fn resumer_match(m: &Value, puuid: &str, lies: &[(UserId, String)]) -> Option<MatchResume> {
    let meta = &m["metadata"];
    if meta["is_completed"].as_bool() == Some(false) {
        return None;
    }
    let joueurs = m["players"].as_array()?;
    let joueur = joueurs.iter().find(|p| p["puuid"].as_str() == Some(puuid))?;
    let equipe = joueur["team_id"].as_str().unwrap_or("");
    let camp = m["teams"]
        .as_array()
        .and_then(|ts| ts.iter().find(|t| t["team_id"].as_str() == Some(equipe)));
    let (gagnees, perdues) = camp
        .map(|t| {
            (
                u8_sature(t["rounds"]["won"].as_u64().unwrap_or(0)),
                u8_sature(t["rounds"]["lost"].as_u64().unwrap_or(0)),
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
    // Sa party : combien de joueurs portent le même `party_id`, lui
    // compris. Un effectif, jamais une identité.
    let party = joueur["party_id"]
        .as_str()
        .filter(|p| !p.is_empty())
        .map(|mien| joueurs.iter().filter(|p| p["party_id"].as_str() == Some(mien)).count())
        .map(|n| n.min(usize::from(u8::MAX)) as u8)
        .unwrap_or(0);
    let mut avec = Vec::new();
    let mut contre = Vec::new();
    for (id, autre) in lies.iter().filter(|(_, autre)| autre != puuid) {
        let Some(p) = joueurs.iter().find(|p| p["puuid"].as_str() == Some(autre.as_str())) else {
            continue;
        };
        if p["team_id"].as_str().is_some_and(|t| t.eq_ignore_ascii_case(equipe)) {
            avec.push(*id);
        } else {
            contre.push(*id);
        }
    }
    Some(MatchResume {
        id: meta["match_id"].as_str().unwrap_or("").to_string(),
        date: iso_vers_ms(meta["started_at"].as_str().unwrap_or("")),
        carte: meta["map"]["name"].as_str().unwrap_or("").to_string(),
        mode: mode_en_francais(&mode),
        agent: joueur["agent"]["name"].as_str().unwrap_or("").to_string(),
        kills: u16_sature(stats["kills"].as_u64().unwrap_or(0)),
        deaths: u16_sature(stats["deaths"].as_u64().unwrap_or(0)),
        assists: u16_sature(stats["assists"].as_u64().unwrap_or(0)),
        score: u32_sature(stats["score"].as_u64().unwrap_or(0)),
        tete_pct: (tetes * 100).checked_div(tirs).unwrap_or(0).min(100) as u8,
        manches: (gagnees, perdues),
        gagne,
        tier: u8_sature(joueur["tier"]["id"].as_u64().unwrap_or(0)),
        duree_s: u32_sature(meta["game_length_in_ms"].as_u64().unwrap_or(0) / 1000),
        saison: meta["season"]["short"].as_str().unwrap_or("").to_string(),
        degats: u32_sature(stats["damage"]["dealt"].as_u64().unwrap_or(0)),
        degats_recus: u32_sature(stats["damage"]["received"].as_u64().unwrap_or(0)),
        tetes: u16_sature(tetes),
        tirs: u16_sature(tirs),
        party,
        avec,
        contre,
        manches_detail: detailler_manches(m, puuid, equipe),
    })
}

/// Un match archivé (`v1/stored-matches`) réduit à la ligne du membre :
/// la même forme, moins ce que l'archive ne dit pas — durée, party,
/// coéquipiers, manches. Les manches gagnées se lisent de `teams
/// {blue, red}` selon le camp de `stats.team` ; un camp inconnu donne
/// (0, 0) et pas de résultat. `None` sans `meta.id` ni `stats`.
fn resumer_match_stocke(m: &Value) -> Option<MatchResume> {
    let meta = &m["meta"];
    let id = meta["id"].as_str().filter(|s| !s.is_empty())?;
    let stats = &m["stats"];
    stats.as_object()?;
    let bleu = u8_sature(m["teams"]["blue"].as_u64().unwrap_or(0));
    let rouge = u8_sature(m["teams"]["red"].as_u64().unwrap_or(0));
    let camp = stats["team"].as_str().unwrap_or("");
    let manches = if camp.eq_ignore_ascii_case("blue") {
        (bleu, rouge)
    } else if camp.eq_ignore_ascii_case("red") {
        (rouge, bleu)
    } else {
        (0, 0)
    };
    let gagne = match manches {
        (0, 0) => None,
        (mien, autre) if mien == autre => None,
        (mien, autre) => Some(mien > autre),
    };
    let tetes = stats["shots"]["head"].as_u64().unwrap_or(0);
    let tirs = tetes
        + stats["shots"]["body"].as_u64().unwrap_or(0)
        + stats["shots"]["leg"].as_u64().unwrap_or(0);
    Some(MatchResume {
        id: id.to_string(),
        date: iso_vers_ms(meta["started_at"].as_str().unwrap_or("")),
        carte: meta["map"]["name"].as_str().unwrap_or("").to_string(),
        mode: mode_en_francais(meta["mode"].as_str().unwrap_or("")),
        agent: stats["character"]["name"].as_str().unwrap_or("").to_string(),
        kills: u16_sature(stats["kills"].as_u64().unwrap_or(0)),
        deaths: u16_sature(stats["deaths"].as_u64().unwrap_or(0)),
        assists: u16_sature(stats["assists"].as_u64().unwrap_or(0)),
        score: u32_sature(stats["score"].as_u64().unwrap_or(0)),
        tete_pct: (tetes * 100).checked_div(tirs).unwrap_or(0).min(100) as u8,
        manches,
        gagne,
        tier: u8_sature(stats["tier"].as_u64().unwrap_or(0)),
        duree_s: 0,
        saison: meta["season"]["short"].as_str().unwrap_or("").to_string(),
        degats: u32_sature(stats["damage"]["made"].as_u64().unwrap_or(0)),
        degats_recus: u32_sature(stats["damage"]["received"].as_u64().unwrap_or(0)),
        tetes: u16_sature(tetes),
        tirs: u16_sature(tirs),
        party: 0,
        avec: Vec::new(),
        contre: Vec::new(),
        manches_detail: None,
    })
}

/// Les modes sans manches, tels que `queue.id` les nomme : un combat à
/// mort n'a ni camp ni premier sang.
const MODES_SANS_MANCHES: [&str; 3] = ["deathmatch", "team deathmatch", "hurm"];

/// Le puuid d'un objet joueur (`killer`, `victim`, `assistants[]`,
/// `plant.player`…) — vide s'il n'y en a pas.
fn puuid_de(v: &Value) -> &str {
    v["puuid"].as_str().unwrap_or("")
}

/// Ce que les manches racontent du membre — sa ligne seulement, rien
/// des neuf autres. Lu dans `rounds[]` et `kills[]`, déjà téléchargés.
///
/// `None` si le mode n'a pas de manches, si `rounds[]` manque ou est
/// vide, si `kills[]` manque (un tableau vide passe : un match sans kill
/// est théorique mais valide), ou s'il y a moins de deux manches. Les
/// camps se comparent sans tenir compte de la casse — le format exact de
/// `winning_team` et `killer.team` reste à voir à l'exécution ; en cas
/// d'écart, `deroule` reste vide et les clutchs à zéro, sans panique.
/// Rien ne suppose cinq joueurs par camp : l'effectif se compte.
fn detailler_manches(m: &Value, moi: &str, mon_camp: &str) -> Option<DetailManches> {
    if moi.is_empty() {
        return None;
    }
    let queue = &m["metadata"]["queue"];
    let sans_manches = ["id", "name", "mode_type"].iter().any(|c| {
        queue[*c]
            .as_str()
            .is_some_and(|mode| MODES_SANS_MANCHES.contains(&mode.to_ascii_lowercase().as_str()))
    });
    if sans_manches {
        return None;
    }
    let rounds = m["rounds"].as_array().filter(|r| !r.is_empty())?;
    let kills = m["kills"].as_array()?;
    let meme_camp = |a: &str, b: &str| !a.is_empty() && a.eq_ignore_ascii_case(b);

    // Qui est de quel camp, et combien ils sont — jetés à la sortie.
    let mut camp_de: BTreeMap<&str, &str> = BTreeMap::new();
    let mut effectif: Vec<(&str, u8)> = Vec::new();
    for p in m["players"].as_array().into_iter().flatten() {
        let (Some(puuid), Some(camp)) = (p["puuid"].as_str(), p["team_id"].as_str()) else {
            continue;
        };
        camp_de.insert(puuid, camp);
        match effectif.iter_mut().find(|(c, _)| meme_camp(c, camp)) {
            Some((_, n)) => *n = n.saturating_add(1),
            None => effectif.push((camp, 1)),
        }
    }
    // Les kills par manche, dans l'ordre du temps.
    let mut par_manche: BTreeMap<u64, Vec<&Value>> = BTreeMap::new();
    for k in kills {
        if let Some(r) = k["round"].as_u64() {
            par_manche.entry(r).or_default().push(k);
        }
    }
    for liste in par_manche.values_mut() {
        liste.sort_by_key(|k| k["time_in_round_in_ms"].as_u64().unwrap_or(0));
    }
    let temps = |k: &Value| k["time_in_round_in_ms"].as_u64().unwrap_or(0);

    let mut d = DetailManches {
        manches: rounds.len().min(usize::from(u8::MAX)) as u8,
        ..Default::default()
    };
    let vide = Vec::new();
    for (i, r) in rounds.iter().enumerate() {
        let numero = r["id"].as_u64().unwrap_or(i as u64);
        let kills = par_manche.get(&numero).unwrap_or(&vide);

        // Gagnée, perdue — ou rien, si `winning_team` ou mon camp ne
        // nomme aucun camp connu : on n'invente pas une défaite.
        let vainqueur = r["winning_team"].as_str().unwrap_or("");
        let camp_connu = |camp: &str| effectif.iter().any(|(c, _)| meme_camp(c, camp));
        let gagnee = if meme_camp(vainqueur, mon_camp) {
            Some(true)
        } else if camp_connu(vainqueur) && camp_connu(mon_camp) {
            Some(false)
        } else {
            None
        };
        match gagnee {
            Some(true) => d.deroule.push('V'),
            Some(false) => d.deroule.push('D'),
            None => {}
        }

        let mes_kills = kills.iter().filter(|k| puuid_de(&k["killer"]) == moi).count();
        match mes_kills {
            3 => d.triples = d.triples.saturating_add(1),
            4 => d.quadruples = d.quadruples.saturating_add(1),
            n if n >= 5 => d.aces = d.aces.saturating_add(1),
            _ => {}
        }
        if let Some(premier) = kills.first() {
            if puuid_de(&premier["killer"]) == moi {
                d.premiers_sangs = d.premiers_sangs.saturating_add(1);
            }
            if puuid_de(&premier["victim"]) == moi {
                d.premieres_morts = d.premieres_morts.saturating_add(1);
            }
        }

        // KAST : kill, assist, survie, ou échangé — mon tueur tombe sous
        // un des miens dans les cinq secondes.
        let ma_mort = kills.iter().find(|k| puuid_de(&k["victim"]) == moi);
        let assiste = kills.iter().any(|k| {
            k["assistants"]
                .as_array()
                .is_some_and(|a| a.iter().any(|x| puuid_de(x) == moi))
        });
        let echange = ma_mort.is_some_and(|k1| {
            let t1 = temps(k1);
            let tueur = puuid_de(&k1["killer"]);
            // Une chute mortelle (tueur = moi) n'a personne à venger.
            !tueur.is_empty()
                && tueur != moi
                && kills.iter().any(|k2| {
                    let t2 = temps(k2);
                    let vengeur = &k2["killer"];
                    let des_miens = meme_camp(vengeur["team"].as_str().unwrap_or(""), mon_camp)
                        || camp_de.get(puuid_de(vengeur)).is_some_and(|c| meme_camp(c, mon_camp));
                    puuid_de(&k2["victim"]) == tueur && des_miens && t2 >= t1 && t2 <= t1 + ECHANGE_MS
                })
        });
        if mes_kills > 0 || assiste || ma_mort.is_none() || echange {
            d.kast = d.kast.saturating_add(1);
        }

        // Clutch : on rejoue les kills ; au premier instant où je suis le
        // dernier des miens face à au moins un adversaire, c'est une
        // tentative — gagnée si la manche l'est, à X l'effectif d'en face.
        let mut vivants: Vec<(&str, u8)> = effectif.clone();
        let mut moi_vivant = true;
        let mut tente = false;
        for k in kills {
            let victime = puuid_de(&k["victim"]);
            if victime == moi {
                moi_vivant = false;
            }
            if let Some(camp) = camp_de.get(victime) {
                if let Some((_, n)) = vivants.iter_mut().find(|(c, _)| meme_camp(c, camp)) {
                    *n = n.saturating_sub(1);
                }
            }
            if tente || !moi_vivant {
                continue;
            }
            let miens = vivants.iter().find(|(c, _)| meme_camp(c, mon_camp)).map_or(0, |(_, n)| *n);
            let adverses: u8 = vivants
                .iter()
                .filter(|(c, _)| !meme_camp(c, mon_camp))
                .fold(0u8, |acc, (_, n)| acc.saturating_add(*n));
            if miens == 1 && adverses >= 1 {
                tente = true;
                d.clutchs_tentes = d.clutchs_tentes.saturating_add(1);
                if gagnee == Some(true) {
                    d.clutchs = d.clutchs.saturating_add(1);
                    d.meilleur_clutch = d.meilleur_clutch.max(adverses);
                }
            }
        }

        if puuid_de(&r["plant"]["player"]) == moi {
            d.poses = d.poses.saturating_add(1);
        }
        if puuid_de(&r["defuse"]["player"]) == moi {
            d.desamorcages = d.desamorcages.saturating_add(1);
        }
    }
    if d.manches < 2 {
        return None;
    }
    Some(d)
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

    #[test]
    fn le_recap_compte_la_semaine_et_classe_par_rr() {
        let jour = 86_400_000u64;
        let maintenant = 1_800_000_000_000u64;
        let (depuis, jusqu_a) = (maintenant - 7 * jour, maintenant);
        let m = |id: &str, date: u64, kills: u16, gagne: Option<bool>| MatchResume {
            id: id.into(),
            date,
            carte: "Ascent".into(),
            mode: "Compétitif".into(),
            agent: "Jett".into(),
            kills,
            deaths: 10,
            assists: 3,
            score: u32::from(kills) * 200,
            tete_pct: 20,
            manches: (13, 9),
            gagne,
            tier: 15,
            duree_s: 2400,
            ..Default::default()
        };
        let p = |date: u64, delta: i32| PointRR {
            match_id: String::new(),
            carte: "Ascent".into(),
            date,
            tier: 15,
            rr: 40,
            delta,
            ..Default::default()
        };
        let mut fiches = BTreeMap::new();
        fiches.insert(
            1,
            FicheValorant {
                rang: RangValorant { tier: 15, rr: 40, ..Default::default() },
                matchs: vec![
                    m("a", maintenant - jour, 25, Some(true)),
                    m("b", maintenant - 2 * jour, 12, Some(false)),
                    // Trop vieux : hors de la semaine.
                    m("c", maintenant - 9 * jour, 40, Some(true)),
                ],
                historique_rr: vec![p(maintenant - jour, 22), p(maintenant - 2 * jour, -15), p(maintenant - 9 * jour, 30)],
                ..Default::default()
            },
        );
        fiches.insert(
            2,
            FicheValorant {
                rang: RangValorant { tier: 12, rr: 70, ..Default::default() },
                matchs: vec![m("d", maintenant - 3 * jour, 18, Some(true))],
                historique_rr: vec![p(maintenant - 3 * jour, 19)],
                ..Default::default()
            },
        );
        fiches.insert(3, FicheValorant::default());
        let r = recap_de(&fiches, depuis, jusqu_a).expect("quelqu'un a joué");
        assert_eq!(r.lignes.len(), 2);
        // Le membre 2 a gagné 19, le membre 1 seulement 7 : 2 en tête.
        assert_eq!(r.lignes[0].user_id, 2);
        assert_eq!(r.lignes[1].user_id, 1);
        assert_eq!(r.lignes[1].rr_gagnes, 7);
        assert_eq!((r.lignes[1].matchs, r.lignes[1].victoires, r.lignes[1].defaites), (2, 1, 1));
        assert_eq!(r.lignes[1].meilleur.as_ref().map(|m| m.kills), Some(25));
        assert_eq!(r.sans_match, vec![3]);
        let texte = composer_recap(&r, |id| format!("m{id}"));
        assert!(texte.starts_with("📅 La semaine du groupe, du "), "{texte}");
        assert!(texte.contains("1. m2 — "), "{texte}");
        assert!(texte.contains("+19 RR · 1 match (1 V / 0 D)"), "{texte}");
        assert!(texte.contains("2. m1 — "), "{texte}");
        assert!(texte.contains("+7 RR · 2 matchs (1 V / 1 D) · meilleur : Jett 25/10/3 sur Ascent"), "{texte}");
        assert!(texte.ends_with("Pas de match cette semaine : m3"), "{texte}");
        // Personne n'a joué : rien à dire.
        assert!(recap_de(&fiches, maintenant - 20 * jour, maintenant - 15 * jour).is_none());
    }

    #[test]
    fn le_recap_part_le_dimanche_soir() {
        let ms = |y, mo, d, h, mi| {
            chrono::NaiveDate::from_ymd_opt(y, mo, d)
                .unwrap()
                .and_hms_opt(h, mi, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis() as u64
        };
        // Le 13 septembre 2026 est un dimanche.
        assert!(heure_du_recap(ms(2026, 9, 13, 19, 30)));
        assert!(heure_du_recap(ms(2026, 9, 13, 23, 59)));
        assert!(!heure_du_recap(ms(2026, 9, 13, 19, 29)));
        assert!(!heure_du_recap(ms(2026, 9, 14, 20, 0)));
        assert!(!heure_du_recap(ms(2026, 9, 12, 20, 0)));
        assert_eq!(jour(ms(2026, 9, 8, 12, 0)), "8 septembre");
    }

    /// Les dates HenrikDev tombent juste, à la seconde.
    #[test]
    fn les_dates_iso_se_convertissent() {
        assert_eq!(iso_vers_ms("1970-01-01T00:00:00.000Z"), 0);
        assert_eq!(iso_vers_ms("2000-03-01T00:00:00Z"), 951_868_800_000);
        assert_eq!(iso_vers_ms("2026-09-03T21:12:33.000Z"), 1_788_469_953_000);
        assert_eq!(iso_vers_ms("n'importe quoi"), 0);
    }

    /// On ne garde que la ligne du membre — pas les neuf autres. Les
    /// membres du groupe reconnus dans le match n'y sont que par leur
    /// `UserId` : ni pseudo, ni puuid.
    #[test]
    fn un_match_se_resume_a_la_ligne_du_membre() {
        let m = serde_json::json!({
            "metadata": {
                "match_id": "abc", "map": {"name": "Ascent"}, "game_length_in_ms": 2_400_000,
                "started_at": "2026-09-03T21:12:33.000Z", "queue": {"id": "competitive", "name": "Competitive"},
                "season": {"short": "e9a2"}, "is_completed": true
            },
            "players": [
                {"puuid": "moi", "name": "Redik", "team_id": "Red", "party_id": "p1", "agent": {"name": "Jett"}, "tier": {"id": 14},
                 "stats": {"score": 5000, "kills": 20, "deaths": 12, "assists": 4, "headshots": 30, "bodyshots": 60, "legshots": 10,
                           "damage": {"dealt": 4212, "received": 3980}}},
                {"puuid": "copain", "name": "Nono", "team_id": "Red", "party_id": "p1", "agent": {"name": "Omen"}, "stats": {"kills": 9}},
                {"puuid": "autre", "name": "Inconnu", "team_id": "Blue", "party_id": "p2", "agent": {"name": "Sage"}, "stats": {"kills": 3}}
            ],
            "teams": [
                {"team_id": "Red", "rounds": {"won": 13, "lost": 9}, "won": true},
                {"team_id": "Blue", "rounds": {"won": 9, "lost": 13}, "won": false}
            ]
        });
        let lies = vec![(1u64, "moi".to_string()), (2, "copain".to_string()), (3, "autre".to_string())];
        let r = resumer_match(&m, "moi", &lies).unwrap();
        assert_eq!((r.kills, r.deaths, r.assists), (20, 12, 4));
        assert_eq!(r.manches, (13, 9));
        assert_eq!(r.gagne, Some(true));
        assert_eq!(r.tete_pct, 30);
        assert_eq!((r.tetes, r.tirs), (30, 100));
        assert_eq!(r.mode, "Compétitif");
        assert_eq!(r.agent, "Jett");
        assert_eq!(r.duree_s, 2400);
        assert_eq!(r.saison, "e9a2");
        assert_eq!((r.degats, r.degats_recus), (4212, 3980));
        assert_eq!(r.party, 2);
        assert_eq!(r.avec, vec![2]);
        assert_eq!(r.contre, vec![3]);
        // Sans `rounds` ni `kills`, pas de détail — et pas de panique.
        assert!(r.manches_detail.is_none());
        assert!(resumer_match(&m, "inconnu", &lies).is_none());
        let json = serde_json::to_string(&r).unwrap();
        for interdit in ["Sage", "Omen", "autre", "copain", "Nono", "Inconnu", "p1", "p2"] {
            assert!(!json.contains(interdit), "{interdit} dans {json}");
        }
        // Un match pas fini ne se résume pas.
        let mut en_cours = m.clone();
        en_cours["metadata"]["is_completed"] = serde_json::json!(false);
        assert!(resumer_match(&en_cours, "moi", &lies).is_none());
    }

    /// Un kill de la fixture v4 : qui, qui, quand, avec l'aide de qui.
    fn kill(round: u64, t: u64, tueur: (&str, &str), victime: (&str, &str), assistants: &[&str]) -> Value {
        serde_json::json!({
            "round": round,
            "time_in_round_in_ms": t,
            "time_in_match_in_ms": round * 100_000 + t,
            "killer": {"puuid": tueur.0, "name": tueur.0, "tag": "EUW", "team": tueur.1},
            "victim": {"puuid": victime.0, "name": victime.0, "tag": "EUW", "team": victime.1},
            "assistants": assistants.iter().map(|a| serde_json::json!({"puuid": a, "name": a, "tag": "EUW", "team": "Red"})).collect::<Vec<_>>(),
            "weapon": {"id": null, "name": null, "type": null}
        })
    }

    /// Un match v4 à trois manches, cinq contre cinq, moi en Red :
    /// manche 0, je fais le premier sang puis un triple ; manche 1, je
    /// meurs le premier et r2 me venge à `vengeance_ms` ; manche 2, je me
    /// retrouve seul contre deux après avoir posé le spike, et la manche
    /// est gagnée. Les kills sont donnés dans le désordre : c'est au
    /// lecteur de les trier.
    fn match_a_trois_manches(vengeance_ms: u64) -> Value {
        let joueur = |puuid: &str, camp: &str| {
            serde_json::json!({
                "puuid": puuid, "name": puuid, "tag": "EUW", "team_id": camp, "party_id": puuid,
                "agent": {"name": "Jett"}, "tier": {"id": 14}, "account_level": 100,
                "stats": {"score": 4000, "kills": 5, "deaths": 1, "assists": 0, "headshots": 5, "bodyshots": 10, "legshots": 0,
                          "damage": {"dealt": 900, "received": 400}}
            })
        };
        let players: Vec<Value> = ["moi", "r2", "r3", "r4", "r5"]
            .iter()
            .map(|p| joueur(p, "Red"))
            .chain(["b1", "b2", "b3", "b4", "b5"].iter().map(|p| joueur(p, "Blue")))
            .collect();
        let kills = vec![
            // Manche 0 : premier sang et triple.
            kill(0, 15_000, ("moi", "Red"), ("b3", "Blue"), &[]),
            kill(0, 5_000, ("moi", "Red"), ("b1", "Blue"), &["r2"]),
            kill(0, 10_000, ("moi", "Red"), ("b2", "Blue"), &[]),
            // Manche 1 : je meurs le premier, r2 me venge.
            kill(1, 3_000, ("b1", "Blue"), ("moi", "Red"), &[]),
            kill(1, 3_000 + vengeance_ms, ("r2", "Red"), ("b1", "Blue"), &[]),
            kill(1, 40_000, ("b2", "Blue"), ("r2", "Red"), &[]),
            // Manche 2 : trois des miens tombent, j'en reprends deux, r5
            // en prend un puis tombe : je suis seul contre b4 et b5.
            kill(2, 2_000, ("b1", "Blue"), ("r2", "Red"), &[]),
            kill(2, 4_000, ("b2", "Blue"), ("r3", "Red"), &[]),
            kill(2, 6_000, ("b3", "Blue"), ("r4", "Red"), &[]),
            kill(2, 8_000, ("moi", "Red"), ("b1", "Blue"), &[]),
            kill(2, 9_000, ("moi", "Red"), ("b2", "Blue"), &[]),
            kill(2, 10_000, ("r5", "Red"), ("b3", "Blue"), &[]),
            kill(2, 11_000, ("b4", "Blue"), ("r5", "Red"), &[]),
        ];
        serde_json::json!({
            "metadata": {
                "match_id": "m3", "map": {"id": "x", "name": "Bind"}, "game_length_in_ms": 900_000,
                "started_at": "2026-09-03T21:12:33.000Z", "queue": {"id": "competitive", "name": "Competitive", "mode_type": "Standard"},
                "season": {"id": "s", "short": "e9a2"}, "is_completed": true, "platform": "pc"
            },
            "players": players,
            "teams": [
                {"team_id": "Red", "rounds": {"won": 2, "lost": 1}, "won": true},
                {"team_id": "Blue", "rounds": {"won": 1, "lost": 2}, "won": false}
            ],
            "rounds": [
                {"id": 0, "result": "Eliminated", "winning_team": "Red", "plant": null, "defuse": null},
                {"id": 1, "result": "Eliminated", "winning_team": "Blue", "plant": null, "defuse": null},
                {"id": 2, "result": "Detonated", "winning_team": "Red",
                 "plant": {"player": {"puuid": "moi", "name": "moi", "tag": "EUW", "team": "Red"}, "site": "A", "round_time_in_ms": 7_000},
                 "defuse": null}
            ],
            "kills": kills
        })
    }

    /// Les manches se lisent : premier sang, première mort, triple, KAST
    /// par échange, clutch 1v2 gagné, pose.
    #[test]
    fn les_manches_donnent_les_premiers_sangs_et_les_clutchs() {
        let m = match_a_trois_manches(3_000);
        let d = detailler_manches(&m, "moi", "Red").expect("trois manches");
        assert_eq!(d.manches, 3);
        assert_eq!(d.premiers_sangs, 1);
        assert_eq!(d.premieres_morts, 1);
        assert_eq!((d.triples, d.quadruples, d.aces), (1, 0, 0));
        assert_eq!(d.kast, 3);
        assert_eq!((d.clutchs_tentes, d.clutchs, d.meilleur_clutch), (1, 1, 2));
        assert_eq!((d.poses, d.desamorcages), (1, 0));
        assert_eq!(d.deroule, "VDV");
        // La casse des camps ne compte pas ; un camp inconnu ne dit rien
        // des manches, mais ne fait pas tomber le reste.
        let d2 = detailler_manches(&m, "moi", "red").expect("trois manches");
        assert_eq!(d2, d);
        let d3 = detailler_manches(&m, "moi", "Team A").expect("trois manches");
        assert_eq!(d3.deroule, "");
        assert_eq!((d3.clutchs, d3.premiers_sangs, d3.triples), (0, 1, 1));
        // Par `resumer_match`, le détail voyage avec le résumé — sans un
        // seul nom des neuf autres.
        let r = resumer_match(&m, "moi", &[]).unwrap();
        assert_eq!(r.manches_detail.as_ref(), Some(&d));
        assert_eq!(r.party, 1);
        let json = serde_json::to_string(&r).unwrap();
        for autre in ["r2", "r3", "b1", "b5"] {
            assert!(!json.contains(&format!("\"{autre}\"")), "{autre} dans {json}");
        }
        // Un JSON sans `rounds`, avec `rounds` vide, ou sans `kills` : rien.
        let mut sans = m.clone();
        sans["rounds"] = serde_json::json!([]);
        assert!(detailler_manches(&sans, "moi", "Red").is_none());
        let mut sans = m.clone();
        sans.as_object_mut().unwrap().remove("kills");
        assert!(detailler_manches(&sans, "moi", "Red").is_none());
        // Une seule manche ne raconte rien non plus.
        let mut une = m.clone();
        une["rounds"].as_array_mut().unwrap().truncate(1);
        assert!(detailler_manches(&une, "moi", "Red").is_none());
        // Des kills sans `round`, des joueurs sans camp : toujours pas de
        // panique.
        let bizarre = serde_json::json!({
            "metadata": {"queue": {"id": "competitive"}},
            "players": [{"puuid": "moi"}, {"team_id": "Blue"}],
            "rounds": [{"winning_team": 3}, {}],
            "kills": [{"killer": 1, "victim": null}, {"round": "deux"}]
        });
        let d = detailler_manches(&bizarre, "moi", "Red").expect("deux manches");
        assert_eq!(d.manches, 2);
        assert_eq!(d.kast, 2, "survivre compte");
        assert_eq!(d.deroule, "");
    }

    /// Ma mort vengée six secondes plus tard n'est plus un échange : la
    /// manche 1 sort du KAST.
    #[test]
    fn un_echange_trop_tardif_ne_compte_pas() {
        let m = match_a_trois_manches(6_000);
        let d = detailler_manches(&m, "moi", "Red").expect("trois manches");
        assert_eq!(d.kast, 2);
        // À cinq secondes pile, ça passe encore.
        let m = match_a_trois_manches(ECHANGE_MS);
        assert_eq!(detailler_manches(&m, "moi", "Red").unwrap().kast, 3);
    }

    /// Un combat à mort n'a pas de manches : pas de détail, mais le match
    /// se résume quand même.
    #[test]
    fn un_combat_a_mort_n_a_pas_de_manches() {
        let mut m = match_a_trois_manches(3_000);
        m["metadata"]["queue"] = serde_json::json!({"id": "deathmatch", "name": "Deathmatch", "mode_type": "Deathmatch"});
        assert!(detailler_manches(&m, "moi", "Red").is_none());
        let r = resumer_match(&m, "moi", &[]).expect("résumé quand même");
        assert_eq!(r.mode, "Combat à mort");
        assert!(r.manches_detail.is_none());
        // Le TDM aussi, sous ses deux noms.
        m["metadata"]["queue"] = serde_json::json!({"id": "hurm", "name": "Team Deathmatch"});
        assert!(detailler_manches(&m, "moi", "Red").is_none());
    }

    /// Un match de test pour les fusions : `id`, date, et un détail ou non.
    fn resume(id: &str, date: u64, detail: bool) -> MatchResume {
        MatchResume {
            id: id.into(),
            date,
            carte: "Ascent".into(),
            mode: "Compétitif".into(),
            agent: "Jett".into(),
            kills: 20,
            deaths: 10,
            assists: 4,
            score: 5000,
            manches: (13, 9),
            gagne: Some(true),
            tier: 15,
            duree_s: 2400,
            manches_detail: detail.then(|| DetailManches { manches: 22, kast: 17, ..Default::default() }),
            ..Default::default()
        }
    }

    fn point(match_id: &str, date: u64, tier: u8, rr: u16, delta: i32) -> PointRR {
        PointRR {
            match_id: match_id.into(),
            date,
            tier,
            rr,
            delta,
            carte: "Ascent".into(),
            ..Default::default()
        }
    }

    /// La fiche fusionne sans doublon, du plus récent au plus ancien, la
    /// neuve gagnant sur l'ancienne ; le plafond tient ; une neuve vide
    /// (HenrikDev en 429) laisse l'ancien en place.
    #[test]
    fn la_fiche_accumule_ses_matchs_sans_doublon() {
        let ancienne = FicheValorant {
            niveau: 120,
            saisons: vec![StatsSaison { saison: "e9a1".into(), ..Default::default() }],
            matchs: ["a", "b", "c", "d", "e"]
                .iter()
                .enumerate()
                .map(|(i, id)| resume(id, 1000 + i as u64, false))
                .collect(),
            historique_rr: vec![point("a", 1000, 15, 40, 18), point("", 900, 15, 22, -20), point("z", 800, 14, 90, 15)],
            ..Default::default()
        };
        let neuve = FicheValorant {
            niveau: 0,
            maj: 99,
            matchs: ["c", "d", "e", "f", "g"]
                .iter()
                .enumerate()
                .map(|(i, id)| resume(id, 1002 + i as u64, true))
                .collect(),
            historique_rr: vec![point("g", 1006, 15, 58, 18), point("", 900, 15, 22, -20), point("a", 1000, 15, 40, 18)],
            ..Default::default()
        };
        let f = fusionner(ancienne.clone(), neuve.clone());
        let ids: Vec<&str> = f.matchs.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["g", "f", "e", "d", "c", "b", "a"]);
        assert!(f.matchs.iter().take(5).all(|m| m.manches_detail.is_some()), "la neuve porte le détail");
        assert!(f.matchs.iter().skip(5).all(|m| m.manches_detail.is_none()));
        assert_eq!(f.maj, 99);
        assert_eq!(f.niveau, 120, "un niveau à zéro garde l'ancien");
        assert_eq!(f.saisons.len(), 1, "des saisons vides gardent les anciennes");
        // L'historique : « a » et le point sans id sont les mêmes des deux
        // côtés, « g » et « z » s'ajoutent.
        let points: Vec<(&str, u64)> = f.historique_rr.iter().map(|p| (p.match_id.as_str(), p.date)).collect();
        assert_eq!(points, vec![("g", 1006), ("a", 1000), ("", 900), ("z", 800)]);
        // Sans match ni point dans la neuve, l'ancien reste.
        let vide = FicheValorant { niveau: 130, ..Default::default() };
        let f = fusionner(ancienne.clone(), vide);
        assert_eq!(f.matchs.len(), 5);
        assert_eq!(f.historique_rr.len(), 3);
        assert_eq!(f.niveau, 130);
        // Le plafond : soixante-dix matchs, il en reste soixante, les
        // plus récents ; les matchs sans id restent des deux côtés.
        let grosse = FicheValorant {
            matchs: (0..65).map(|i| resume(&format!("m{i}"), 10_000 + i, false)).chain([resume("", 5, false)]).collect(),
            historique_rr: (0..90).map(|i| point(&format!("m{i}"), 10_000 + i, 15, 40, 1)).collect(),
            ..Default::default()
        };
        let neuve = FicheValorant {
            matchs: (63..68).map(|i| resume(&format!("m{i}"), 10_000 + i, true)).chain([resume("", 6, true)]).collect(),
            historique_rr: (85..105).map(|i| point(&format!("m{i}"), 10_000 + i, 15, 40, 1)).collect(),
            ..Default::default()
        };
        let f = fusionner(grosse, neuve);
        assert_eq!(f.matchs.len(), MATCHS_GARDES);
        assert_eq!(f.matchs[0].id, "m67");
        assert!(f.matchs.iter().all(|m| m.date >= 10_008), "les plus récents restent");
        assert_eq!(f.historique_rr.len(), HISTORIQUE_GARDES);
        assert_eq!(f.historique_rr[0].match_id, "m104");
        let ids: BTreeSet<&str> = f.historique_rr.iter().map(|p| p.match_id.as_str()).collect();
        assert_eq!(ids.len(), HISTORIQUE_GARDES, "pas de doublon");
        // Deux matchs sans id ne se confondent pas, même à la fusion.
        let f = fusionner(
            FicheValorant { matchs: vec![resume("", 5, false)], ..Default::default() },
            FicheValorant { matchs: vec![resume("", 6, true)], ..Default::default() },
        );
        assert_eq!(f.matchs.len(), 2);
    }

    /// À la liaison, on garde l'ancienne fiche pour le même compte Riot
    /// (au puuid) et on repart de zéro pour un autre.
    #[test]
    fn la_liaison_ne_fusionne_que_le_meme_compte() {
        let ancienne = FicheValorant { matchs: vec![resume("a", 1, false)], ..Default::default() };
        let neuve = FicheValorant { matchs: vec![resume("b", 2, true)], ..Default::default() };
        let f = fusion_a_la_liaison(Some(("puuid-1".into(), ancienne.clone())), "puuid-1", neuve.clone());
        assert_eq!(f.matchs.len(), 2);
        let f = fusion_a_la_liaison(Some(("puuid-1".into(), ancienne.clone())), "puuid-2", neuve.clone());
        assert_eq!(f.matchs.len(), 1);
        assert_eq!(f.matchs[0].id, "b");
        let f = fusion_a_la_liaison(None, "puuid-1", neuve.clone());
        assert_eq!(f, neuve);
    }

    /// Nono relie le même compte un mois plus tard : l'archive de
    /// HenrikDev connaît ses vieux matchs, mais sans manches ni
    /// co-membres — elle passe dessous, et ce qu'on avait détaillé reste
    /// détaillé. Un autre compte, lui, ne garde rien de l'ancienne fiche.
    #[test]
    fn la_re_liaison_garde_le_detail_des_matchs_accumules() {
        let mut x = resume("x", 1_000, true);
        x.avec = vec![2];
        x.party = 3;
        x.duree_s = 2_460;
        let ancienne = FicheValorant {
            matchs: vec![x.clone()],
            historique_rr: vec![point("x", 1_000, 15, 40, 18)],
            ..Default::default()
        };
        let fraiche = FicheValorant {
            riot_id: "NouveauNom#TAG".into(),
            matchs: vec![resume("y", 2_000, true)],
            historique_rr: vec![point("y", 2_000, 15, 58, 18)],
            ..Default::default()
        };
        // L'archive : « x » et « z », résumés comme `resumer_match_stocke`
        // les rend — ni détail, ni co-membre, ni party, ni durée.
        let archive = FicheValorant {
            matchs: vec![
                MatchResume { party: 0, duree_s: 0, manches_detail: None, ..resume("x", 1_000, false) },
                MatchResume { party: 0, duree_s: 0, ..resume("z", 500, false) },
            ],
            historique_rr: vec![point("x", 1_000, 15, 40, 18), point("z", 500, 15, 22, -20)],
            ..Default::default()
        };
        let f = empiler_a_la_liaison(Some(("puuid-1".into(), ancienne.clone())), "puuid-1", fraiche.clone(), archive.clone());
        let ids: Vec<&str> = f.matchs.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["y", "x", "z"], "la fraîche en tête, l'archive comble « z »");
        assert_eq!(f.matchs[1], x, "« x » garde son détail, ses co-membres, sa party et sa durée");
        assert!(f.matchs[0].manches_detail.is_some());
        assert!(f.matchs[2].manches_detail.is_none());
        assert_eq!(f.riot_id, "NouveauNom#TAG", "le nouveau nom, même puuid");
        let points: Vec<&str> = f.historique_rr.iter().map(|p| p.match_id.as_str()).collect();
        assert_eq!(points, vec!["y", "x", "z"]);
        // Un autre compte : l'ancienne fiche est oubliée, l'archive reste.
        let f = empiler_a_la_liaison(Some(("puuid-1".into(), ancienne.clone())), "puuid-2", fraiche.clone(), archive.clone());
        let ids: Vec<&str> = f.matchs.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["y", "x", "z"]);
        assert!(f.matchs[1].avec.is_empty() && f.matchs[1].manches_detail.is_none(), "« x » vient de l'archive");
        // Une archive muette ne change rien à la fusion classique.
        let f = empiler_a_la_liaison(Some(("puuid-1".into(), ancienne.clone())), "puuid-1", fraiche.clone(), FicheValorant::default());
        assert_eq!(f, fusion_a_la_liaison(Some(("puuid-1".into(), ancienne)), "puuid-1", fraiche.clone()));
        // Et sans rien d'ancien, la fraîche passe sur l'archive.
        let f = empiler_a_la_liaison(None, "puuid-1", fraiche, archive);
        assert_eq!(f.matchs.len(), 3);
        assert!(f.matchs[1].manches_detail.is_none());
    }

    /// Une fiche écrite par un serveur 0.1.39 se relit avec les défauts,
    /// et se fusionne sous une fiche neuve.
    #[test]
    fn une_fiche_d_avant_se_relit_et_se_fusionne() {
        let brut = r#"{
          "7": {
            "riot_id": "Redik#6162", "region": "eu", "plateforme": "pc", "niveau": 212,
            "rang": {"tier": 15, "rr": 40, "delta": 18, "elo": 1240, "saison": ""},
            "pic": {"tier": 16, "rr": 12, "delta": 0, "elo": 0, "saison": "E9A1"},
            "historique_rr": [
              {"match_id": "m1", "date": 1788469953000, "tier": 15, "rr": 40, "delta": 18, "carte": "Ascent"}
            ],
            "matchs": [
              {"id": "m1", "date": 1788469953000, "carte": "Ascent", "mode": "Compétitif", "agent": "Jett",
               "kills": 20, "deaths": 10, "assists": 4, "score": 5000, "tete_pct": 25, "manches": [13, 9],
               "gagne": true, "tier": 15, "duree_s": 2400}
            ],
            "maj": 1788470000000
          }
        }"#;
        let fiches: BTreeMap<UserId, FicheValorant> = serde_json::from_str(brut).expect("une fiche d'avant se relit");
        let ancienne = fiches.get(&7).expect("le membre 7").clone();
        assert_eq!(ancienne.matchs[0].manches_detail, None);
        assert_eq!(ancienne.matchs[0].party, 0);
        assert!(ancienne.saisons.is_empty());
        assert_eq!(ancienne.rang.boucliers, 0);
        let neuve = FicheValorant {
            riot_id: "Redik#6162".into(),
            niveau: 213,
            matchs: vec![resume("m2", 1_788_480_000_000, true), resume("m1", 1_788_469_953_000, true)],
            historique_rr: vec![point("m2", 1_788_480_000_000, 15, 58, 18), point("m1", 1_788_469_953_000, 15, 40, 18)],
            ..Default::default()
        };
        let f = fusionner(ancienne, neuve);
        assert_eq!(f.matchs.len(), 2);
        assert!(f.matchs.iter().all(|m| m.manches_detail.is_some()));
        assert_eq!(f.historique_rr.len(), 2);
        assert_eq!(f.niveau, 213);
        // Et une fiche V5 se réécrit et se relit sans perte.
        let json = serde_json::to_string(&BTreeMap::from([(7u64, f.clone())])).unwrap();
        let relue: BTreeMap<UserId, FicheValorant> = serde_json::from_str(&json).unwrap();
        assert_eq!(relue.get(&7), Some(&f));
    }

    /// Un match archivé se lit à la ligne du membre, les manches selon
    /// son camp.
    #[test]
    fn un_match_stocke_se_resume() {
        let stocke = |camp: &str, bleu: u8, rouge: u8| {
            serde_json::json!({
                "meta": {
                    "id": "arch-1", "map": {"id": "x", "name": "Haven"}, "mode": "competitive",
                    "season": {"id": "s", "short": "e8a3"}, "started_at": "2026-08-01T20:00:00.000Z",
                    "version": "release-11.00", "region": "eu", "cluster": null
                },
                "stats": {
                    "puuid": "moi", "team": camp, "character": {"id": "c", "name": "Reyna"},
                    "kills": 22, "deaths": 14, "assists": 3, "score": 5400, "level": 200, "tier": 14,
                    "damage": {"made": 3800, "received": 3100}, "shots": {"head": 20, "body": 60, "leg": 20},
                    "name": "Redik", "tag": "6162"
                },
                "teams": {"blue": bleu, "red": rouge}
            })
        };
        let r = resumer_match_stocke(&stocke("Blue", 13, 9)).expect("un match");
        assert_eq!(r.id, "arch-1");
        assert_eq!(r.manches, (13, 9));
        assert_eq!(r.gagne, Some(true));
        assert_eq!(r.mode, "Compétitif");
        assert_eq!(r.agent, "Reyna");
        assert_eq!(r.carte, "Haven");
        assert_eq!(r.saison, "e8a3");
        assert_eq!((r.kills, r.deaths, r.assists, r.score), (22, 14, 3, 5400));
        assert_eq!((r.tetes, r.tirs, r.tete_pct), (20, 100, 20));
        assert_eq!((r.degats, r.degats_recus), (3800, 3100));
        assert_eq!(r.tier, 14);
        assert_eq!(r.date, iso_vers_ms("2026-08-01T20:00:00.000Z"));
        assert_eq!((r.duree_s, r.party), (0, 0));
        assert!(r.avec.is_empty() && r.contre.is_empty() && r.manches_detail.is_none());
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("Redik") && !json.contains("6162") && !json.contains("moi"));
        let r = resumer_match_stocke(&stocke("red", 13, 9)).expect("un match");
        assert_eq!((r.manches, r.gagne), ((9, 13), Some(false)));
        let r = resumer_match_stocke(&stocke("Blue", 11, 11)).expect("un match");
        assert_eq!(r.gagne, None);
        let r = resumer_match_stocke(&stocke("Green", 13, 9)).expect("un match");
        assert_eq!((r.manches, r.gagne), ((0, 0), None));
        // Sans `meta.id` ou sans `stats` : rien.
        let mut sans = stocke("Blue", 13, 9);
        sans["meta"]["id"] = serde_json::json!("");
        assert!(resumer_match_stocke(&sans).is_none());
        let mut sans = stocke("Blue", 13, 9);
        sans["stats"] = Value::Null;
        assert!(resumer_match_stocke(&sans).is_none());
    }

    /// Les deux archives de la liaison donnent une fiche de matchs et de
    /// points, que la fiche fraîche recouvre.
    #[test]
    fn la_liaison_rattrape_l_historique() {
        let matchs = serde_json::json!({"status": 200, "results": {"total": 2, "returned": 2}, "data": [
            {"meta": {"id": "m9", "map": {"name": "Bind"}, "mode": "unrated", "season": {"short": "e9a2"}, "started_at": "2026-09-02T20:00:00Z"},
             "stats": {"puuid": "moi", "team": "Red", "character": {"name": "Sage"}, "kills": 10, "deaths": 10, "assists": 10, "score": 3000,
                       "tier": 0, "damage": {"made": 2000, "received": 2000}, "shots": {"head": 5, "body": 20, "leg": 0}},
             "teams": {"blue": 13, "red": 4}},
            {"meta": {"id": "", "map": {"name": "?"}}, "stats": {}, "teams": {}},
            {"meta": {"id": "m8", "map": {"name": "Ascent"}, "mode": "competitive", "season": {"short": "e9a2"}, "started_at": "2026-09-01T20:00:00Z"},
             "stats": {"puuid": "moi", "team": "Blue", "character": {"name": "Jett"}, "kills": 25, "deaths": 12, "assists": 2, "score": 6000,
                       "tier": 15, "damage": {"made": 4500, "received": 3000}, "shots": {"head": 30, "body": 50, "leg": 20}},
             "teams": {"blue": 13, "red": 9}}
        ]});
        let points = serde_json::json!({"status": 200, "results": {"total": 1, "returned": 1}, "data": [
            {"match_id": "m8", "date": "2026-09-01T20:40:00Z", "tier": {"id": 15, "name": "Or 3"}, "rr": 40, "last_change": 18,
             "map": {"name": "Ascent"}, "season": {"short": "e9a2"}, "was_derank_protected": true, "elo": 1240, "refunded_rr": 0}
        ]});
        let archive = fiche_archivee(&matchs, &points);
        assert_eq!(archive.matchs.len(), 2, "l'entrée sans id est ignorée");
        assert_eq!(archive.matchs[0].id, "m9");
        assert_eq!((archive.matchs[0].manches, archive.matchs[0].gagne), ((4, 13), Some(false)));
        assert_eq!(archive.historique_rr.len(), 1);
        assert!(archive.historique_rr[0].protege);
        assert_eq!(archive.historique_rr[0].saison, "e9a2");
        assert_eq!(archive.historique_rr[0].delta, 18);
        // La fiche fraîche gagne sur « m8 » : elle porte le détail.
        let fraiche = FicheValorant {
            riot_id: "Redik#6162".into(),
            niveau: 200,
            matchs: vec![resume("m10", iso_vers_ms("2026-09-03T20:00:00Z"), true), resume("m8", iso_vers_ms("2026-09-01T20:00:00Z"), true)],
            historique_rr: vec![point("m8", iso_vers_ms("2026-09-01T20:40:00Z"), 15, 40, 18)],
            ..Default::default()
        };
        let f = fusionner(archive, fraiche);
        let ids: Vec<&str> = f.matchs.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["m10", "m9", "m8"]);
        assert!(f.matchs[2].manches_detail.is_some());
        assert_eq!(f.historique_rr.len(), 1);
        assert_eq!(f.niveau, 200);
        assert_eq!(f.riot_id, "Redik#6162");
        // Des archives muettes donnent une fiche vide.
        let vide = fiche_archivee(&Value::Null, &Value::Null);
        assert!(vide.matchs.is_empty() && vide.historique_rr.is_empty());
    }

    /// Le bilan d'un membre se calcule à l'envoi, classé seulement, sur
    /// sept et trente jours.
    #[test]
    fn le_bilan_d_un_membre_se_calcule_a_l_envoi() {
        let maintenant = 1_800_000_000_000u64;
        let jour = JOUR_MS;
        let mut recent = resume("a", maintenant - jour, true);
        recent.avec = vec![2];
        let mut perdu = resume("b", maintenant - 10 * jour, true);
        perdu.gagne = Some(false);
        perdu.manches = (9, 13);
        perdu.agent = "Reyna".into();
        perdu.avec = vec![2, 3];
        let mut non_classe = resume("c", maintenant - 2 * jour, false);
        non_classe.mode = "Non classé".into();
        let vieux = resume("d", maintenant - 40 * jour, false);
        let f = FicheValorant {
            matchs: vec![recent, non_classe, perdu, vieux],
            historique_rr: vec![point("a", maintenant - jour, 15, 58, 18), point("b", maintenant - 10 * jour, 15, 40, -16)],
            ..Default::default()
        };
        let b = bilan_membre(&f, maintenant);
        assert_eq!((b.sept_jours.matchs, b.sept_jours.victoires, b.sept_jours.rr), (1, 1, 18));
        assert_eq!((b.trente_jours.matchs, b.trente_jours.defaites, b.trente_jours.rr), (2, 1, 2));
        // La forme et la série ne connaissent pas de fenêtre : le vieux
        // classé « d » y est.
        assert_eq!(b.forme, vec![1, -1, 1]);
        assert_eq!(b.serie, 1);
        assert_eq!(b.agents, vec![("Jett".to_string(), 1, 1), ("Reyna".to_string(), 1, 0)]);
        assert_eq!(b.cartes, vec![("Ascent".to_string(), 2, 1)]);
        assert_eq!(b.duos, vec![(2, 2, 1), (3, 1, 0)]);
        assert_eq!(bilan_membre(&FicheValorant::default(), maintenant), BilanMembre::default());
    }

    /// Deux matchs connus tombent dans les bonnes cases jour × heure ; un
    /// match trop vieux n'y est pas ; sans match, rien.
    #[test]
    fn l_activite_compte_les_heures_utc() {
        let maintenant = iso_vers_ms("2026-09-15T12:00:00Z");
        // Le 14 septembre 2026 est un lundi, le 13 un dimanche.
        let f1 = FicheValorant {
            matchs: vec![
                resume("a", iso_vers_ms("2026-09-14T20:30:00Z"), false),
                resume("b", iso_vers_ms("2026-09-13T01:15:00Z"), false),
                resume("c", iso_vers_ms("2026-07-01T20:30:00Z"), false),
            ],
            ..Default::default()
        };
        let f2 = FicheValorant {
            matchs: vec![resume("d", iso_vers_ms("2026-09-14T20:59:59Z"), false)],
            ..Default::default()
        };
        let cases = activite_de([&f1, &f2].into_iter(), maintenant);
        assert_eq!(cases.len(), 168);
        assert_eq!(cases[20], 2, "lundi 20 h");
        assert_eq!(cases[6 * 24 + 1], 1, "dimanche 1 h");
        assert_eq!(cases.iter().map(|c| u32::from(*c)).sum::<u32>(), 3);
        // Le 1er janvier 1970 est un jeudi : la case 3 × 24.
        let f3 = FicheValorant { matchs: vec![resume("e", 0, false)], ..Default::default() };
        assert!(activite_de([&f3].into_iter(), maintenant).is_empty(), "trop vieux");
        assert_eq!(activite_de([&f3].into_iter(), 10 * JOUR_MS)[3 * 24], 1);
        assert!(activite_de(std::iter::empty(), maintenant).is_empty());
    }

    /// Quarante fiches pleines — soixante matchs détaillés, cent points —
    /// tiennent dans une ligne : le serveur descend d'un palier ; trois
    /// fiches passent au premier.
    #[test]
    fn la_page_du_groupe_tient_dans_une_ligne() {
        let maintenant = 1_800_000_000_000u64;
        let pleine = |n: u64| FicheValorant {
            riot_id: format!("JoueurNumeroDouze{n}#EUW{n}"),
            region: "eu".into(),
            plateforme: "pc".into(),
            niveau: 212,
            rang: RangValorant { tier: 15, rr: 40, delta: 18, elo: 1240, saison: "e9a2".into(), boucliers: 1, ..Default::default() },
            pic: Some(RangValorant { tier: 16, rr: 12, saison: "e9a1".into(), ..Default::default() }),
            matchs: (0..60u64)
                .map(|i| {
                    let mut m = resume(&format!("0123abcd-4567-89ef-0123-456789abc{n:02}{i:02}"), maintenant - i * 3_600_000, true);
                    m.saison = "e9a2".into();
                    m.agent = "Chamber".into();
                    m.carte = "Fracture".into();
                    m.degats = 4212;
                    m.degats_recus = 3980;
                    m.tetes = 30;
                    m.tirs = 100;
                    m.party = 3;
                    m.avec = vec![(n + 1) % 40, (n + 2) % 40];
                    m.contre = vec![(n + 3) % 40];
                    m.manches_detail = Some(DetailManches {
                        manches: 24, kast: 18, premiers_sangs: 3, premieres_morts: 2, triples: 2, quadruples: 1, aces: 0,
                        clutchs_tentes: 2, clutchs: 1, meilleur_clutch: 2, poses: 4, desamorcages: 1,
                        deroule: "VDVVDVDVVVDDVDVVDVDVVDVV".into(),
                    });
                    m
                })
                .collect(),
            historique_rr: (0..100u64)
                .map(|i| {
                    let mut p = point(&format!("0123abcd-4567-89ef-0123-456789abc{n:02}{i:02}"), maintenant - i * 3_600_000, 15, 40, 18);
                    p.saison = "e9a2".into();
                    p.protege = i % 7 == 0;
                    p
                })
                .collect(),
            maj: maintenant,
            saisons: (0..12).map(|i| StatsSaison { saison: format!("e{}a{}", i / 3 + 6, i % 3 + 1), victoires: 40, parties: 80, tier_fin: 15, rr_fin: 40 }).collect(),
        };
        let fiches: Vec<(UserId, FicheValorant)> = (0..40).map(|n| (n, pleine(n))).collect();
        let resumes = |n_m: usize, n_p: usize| -> Vec<FicheMembre> {
            fiches
                .iter()
                .map(|(id, f)| FicheMembre {
                    user_id: *id,
                    username: format!("membre-numero-{id}"),
                    fiche: f.resume(n_m, n_p),
                    bilan: Some(bilan_membre(f, maintenant)),
                })
                .collect()
        };
        let esports = vec![
            MatchEsport {
                date: maintenant,
                ligue: "VCT EMEA".into(),
                region: "EMEA".into(),
                tournoi: "Kickoff".into(),
                equipes: vec!["FNC".into(), "TH".into()],
                etat: "unstarted".into(),
                format: "BO3".into(),
            };
            20
        ];
        let activite = vec![3u16; 168];
        let msg = message_stats(resumes, esports, activite);
        let octets = serde_json::to_vec(&msg).unwrap();
        assert!(octets.len() <= STATS_MAX_BYTES, "{} octets", octets.len());
        assert!(octets.len() <= ki_protocol::MAX_LINE);
        let ServerMsg::StatsValorant { fiches: envoyees, esports, activite } = &msg else {
            panic!("pas le bon message");
        };
        assert_eq!(envoyees.len(), 40, "jamais un membre de moins");
        assert!(envoyees.iter().all(|f| f.fiche.matchs.len() <= 3 && f.fiche.historique_rr.len() <= 6), "palier (3, 6) au plus");
        assert!(envoyees.iter().all(|f| f.bilan.is_some() && f.fiche.saisons.is_empty()));
        assert!(envoyees.iter().all(|f| f.fiche.matchs.iter().all(|m| m.manches_detail.is_none() && !m.id.is_empty())));
        assert_eq!(esports.len(), 20);
        assert_eq!(activite.len(), 168);
        // Trois fiches : le premier palier, cinq matchs et dix points.
        let trois: Vec<(UserId, FicheValorant)> = fiches.iter().take(3).cloned().collect();
        let msg = message_stats(
            |n_m, n_p| {
                trois
                    .iter()
                    .map(|(id, f)| FicheMembre { user_id: *id, username: "x".into(), fiche: f.resume(n_m, n_p), bilan: None })
                    .collect()
            },
            Vec::new(),
            Vec::new(),
        );
        let ServerMsg::StatsValorant { fiches: envoyees, .. } = &msg else {
            panic!("pas le bon message");
        };
        assert!(envoyees.iter().all(|f| f.fiche.matchs.len() == MATCHS_MAX && f.fiche.historique_rr.len() == POINTS_RESUME));
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
                motif: Motif::CoMembre
            })
        ));
        assert!(rx.try_recv().is_err(), "un seul travail pour le coéquipier");
        assert!(fil.pretes().is_empty(), "l'annonce attend le coéquipier");
        // Le coéquipier vient d'être relu — juste avant que HenrikDev
        // indexe le match : sa fiche est fraîche, et pourtant le fil le
        // relit, sinon l'annonce sortirait sans sa ligne. Seul le
        // périodique se laisse arrêter par la fraîcheur.
        let maintenant = maintenant_ms();
        assert!(!relecture_inutile(Motif::CoMembre, Some(maintenant), maintenant));
        assert!(!relecture_inutile(Motif::Relance { essai: 0, max: 3 }, Some(maintenant), maintenant));
        assert!(relecture_inutile(Motif::Periodique, Some(maintenant), maintenant));
        assert!(!relecture_inutile(Motif::Periodique, Some(maintenant - 61_000), maintenant));
        assert!(!relecture_inutile(Motif::Periodique, None, maintenant));
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
        v.fin_de_partie(1, &partie("competitive", false), Duration::from_secs(1800));
        v.reprise_de_partie(1);
        v.rafraichir(1);
        assert!(v.tick().is_empty());
        v.rafraichir_esports();
        assert!(v.esports().is_empty());
        assert!(v.compteurs_texte().contains("clé HenrikDev absente"));
        assert!(v.journal_texte().contains("aucune requête"));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn partie(file: &str, custom: bool) -> ki_protocol::JeuStatut {
        ki_protocol::JeuStatut {
            etat: ki_protocol::JeuEtat::EnJeu,
            file: file.into(),
            custom,
            ..Default::default()
        }
    }

    /// Un service avec une clé (bidon) mais dont le fil ne tourne pas :
    /// les travaux s'entassent dans `rx`, où le test les lit — sans
    /// jamais toucher HenrikDev.
    fn service_a_l_arret(nom: &str) -> (Valorant, Receiver<Travail>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("ki-valorant-{nom}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (tx, rx) = mpsc::channel();
        let etat = Arc::new(Etat {
            dossier: dir.clone(),
            comptes: Mutex::new(BTreeMap::new()),
            fiches: Mutex::new(BTreeMap::new()),
            fil: Fil::default(),
            dernier_recap: Mutex::new(0),
            compteurs: Arc::default(),
            esports: Mutex::new((0, Vec::new())),
            esports_en_cours: std::sync::atomic::AtomicBool::new(false),
        });
        let v = Valorant {
            etat,
            travaux: Some(tx),
            resultats: Mutex::new(mpsc::channel().1),
        };
        (v, rx, dir)
    }

    fn compte(user_id: UserId, v: &Valorant) {
        v.etat.comptes.lock().unwrap().insert(
            user_id,
            CompteRiot {
                nom: format!("m{user_id}"),
                tag: "KI".into(),
                puuid: format!("puuid-{user_id}"),
                region: "eu".into(),
                plateforme: "pc".into(),
                depuis: 0,
            },
        );
    }

    fn travaux_en_file(rx: &Receiver<Travail>) -> Vec<(UserId, Motif)> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|t| match t {
                Travail::Rafraichir { user_id, motif } => Some((user_id, motif)),
                _ => None,
            })
            .collect()
    }

    /// Le périodique, le co-membre et la relance visent le même membre :
    /// un seul travail part, et c'est le motif le plus informatif que le
    /// fil lit en le prenant. Une relance programmée vaut une file : le
    /// périodique n'y ajoute rien.
    #[test]
    fn la_file_ne_double_pas_un_membre_deja_en_attente() {
        let (v, rx, _dir) = service_a_l_arret("file");
        compte(1, &v);
        compte(2, &v);
        let travaux = v.travaux.as_ref().unwrap();
        v.rafraichir(1);
        v.rafraichir(1);
        assert!(!v.etat.fil.pousser(1, Motif::CoMembre, travaux));
        assert!(!v.etat.fil.pousser(1, Motif::Relance { essai: 0, max: 3 }, travaux));
        // Un seul travail, et la file porte la relance.
        assert_eq!(travaux_en_file(&rx), vec![(1, Motif::Periodique)]);
        assert_eq!(
            v.etat.fil.prendre(1, Motif::Periodique),
            Motif::Relance { essai: 0, max: 3 }
        );
        // Sorti de la file, il y revient — le motif d'origine quand rien n'a
        // été fusionné.
        v.rafraichir(1);
        assert_eq!(travaux_en_file(&rx), vec![(1, Motif::Periodique)]);
        assert_eq!(v.etat.fil.prendre(1, Motif::Periodique), Motif::Periodique);
        // Une relance programmée : ni le périodique ni un co-membre ne
        // passent, la relance viendra ; elle-même passe quand elle est due.
        v.fin_de_partie(2, &partie("competitive", false), Duration::from_secs(1800));
        v.rafraichir(2);
        assert!(!v.etat.fil.pousser(2, Motif::CoMembre, travaux));
        assert!(travaux_en_file(&rx).is_empty());
        v.etat.fil.relances.lock().unwrap().get_mut(&2).unwrap().quand = Instant::now();
        assert!(v.tick().is_empty());
        assert_eq!(travaux_en_file(&rx), vec![(2, Motif::Relance { essai: 0, max: 3 })]);
        assert!(v.etat.fil.relances.lock().unwrap().is_empty());
        assert!(v.compteurs_texte().contains("en file : 1, relances programmées : 0"));
    }

    /// Le membre revient en partie avant que la relance ait tiré : c'était
    /// un hoquet du client Riot, la relance tombe. Une relance qui a déjà
    /// tiré reste.
    #[test]
    fn une_reprise_de_partie_annule_la_relance() {
        let (v, rx, _dir) = service_a_l_arret("reprise");
        compte(1, &v);
        v.fin_de_partie(1, &partie("competitive", false), Duration::from_secs(1800));
        assert_eq!(v.etat.fil.relances.lock().unwrap().len(), 1);
        v.reprise_de_partie(1);
        assert!(v.etat.fil.relances.lock().unwrap().is_empty());
        assert!(v.tick().is_empty());
        assert!(travaux_en_file(&rx).is_empty(), "zéro requête pour un hoquet");
        // Déjà tirée une fois : la suivante reste programmée.
        v.etat.fil.relances.lock().unwrap().insert(
            1,
            Relance {
                quand: Instant::now() + RELANCE_DELAI,
                essai: 1,
                max: 3,
            },
        );
        v.reprise_de_partie(1);
        assert_eq!(v.etat.fil.relances.lock().unwrap().len(), 1);
    }

    fn menu() -> ki_protocol::JeuStatut {
        ki_protocol::JeuStatut {
            etat: ki_protocol::JeuEtat::Menus,
            ..Default::default()
        }
    }

    /// Une classée de quarante minutes, un hoquet du client Riot à la
    /// trente-huitième : la partie se mesure depuis l'entrée d'origine,
    /// pas depuis le retour du hoquet, et sa vraie fin vaut trois
    /// relances. Une session qui reprend en pleine partie ne sait pas
    /// depuis quand : sa fin est réputée assez longue.
    #[test]
    fn un_hoquet_ou_une_reconnexion_ne_raccourcit_pas_la_partie() {
        let t0 = Instant::now();
        let min = |n: u64| Duration::from_secs(n * 60);
        let classee = partie("competitive", false);
        // Du menu à la partie : l'heure d'entrée se pose, la relance du
        // match d'avant tombe.
        let t = transition_de_jeu(Some(menu()), &Some(classee.clone()), None, t0);
        assert_eq!(
            t,
            TransitionJeu {
                fin: None,
                reprise: true,
                en_jeu_depuis: Some(t0)
            }
        );
        // Le hoquet : présence perdue à 38 min — une fin, mesurée à 38
        // min, mais l'heure d'entrée reste.
        let t = transition_de_jeu(Some(classee.clone()), &None, Some(t0), t0 + min(38));
        assert_eq!(t.fin, Some((classee.clone(), min(38))));
        assert!(!t.reprise);
        assert_eq!(t.en_jeu_depuis, Some(t0));
        // Retour cinq secondes plus tard : une reprise, l'heure d'origine
        // toujours.
        let t = transition_de_jeu(None, &Some(classee.clone()), Some(t0), t0 + min(38) + Duration::from_secs(5));
        assert_eq!(
            t,
            TransitionJeu {
                fin: None,
                reprise: true,
                en_jeu_depuis: Some(t0)
            }
        );
        // La vraie fin, au menu, deux minutes plus tard : quarante minutes,
        // trois relances ; le menu efface l'heure d'entrée.
        let t = transition_de_jeu(Some(classee.clone()), &Some(menu()), Some(t0), t0 + min(40));
        let (ancien, duree) = t.fin.clone().expect("une fin de partie");
        assert_eq!((duree, t.en_jeu_depuis), (min(40), None));
        assert_eq!(relances_pour(&ancien, duree), 3);
        // Et le service enchaîne ces trois transitions comme il faut : une
        // relance après le hoquet, annulée au retour, puis la vraie.
        let (v, _rx, _dir) = service_a_l_arret("hoquet");
        compte(1, &v);
        v.fin_de_partie(1, &classee, min(38));
        assert_eq!(v.etat.fil.relances.lock().unwrap().len(), 1);
        v.reprise_de_partie(1);
        assert!(v.etat.fil.relances.lock().unwrap().is_empty());
        v.fin_de_partie(1, &ancien, duree);
        assert_eq!(v.etat.fil.relances.lock().unwrap().get(&1).map(|r| r.max), Some(3));
        // Une session neuve qui trouve son membre en partie : ni entrée ni
        // reprise — la relance du match d'avant, s'il y en a une, reste.
        let t = transition_de_jeu(None, &Some(classee.clone()), None, t0);
        assert_eq!(t, TransitionJeu::default());
        // Sa fin deux minutes plus tard est réputée assez longue.
        let t = transition_de_jeu(Some(classee.clone()), &Some(menu()), None, t0 + min(2));
        let (ancien, duree) = t.fin.expect("une fin de partie");
        assert_eq!(duree, Duration::MAX);
        assert_eq!(relances_pour(&ancien, duree), 3);
        // Un menu observé après une présence perdue clôt la partie ; un
        // score qui bouge en partie ne touche à rien.
        let t = transition_de_jeu(None, &Some(menu()), Some(t0), t0);
        assert_eq!(t, TransitionJeu::default());
        let mut score = classee.clone();
        score.score_allie = 5;
        let t = transition_de_jeu(Some(classee.clone()), &Some(score), Some(t0), t0 + min(10));
        assert_eq!(
            t,
            TransitionJeu {
                fin: None,
                reprise: false,
                en_jeu_depuis: Some(t0)
            }
        );
        // Une partie vraiment courte, elle, reste sans relance.
        let t = transition_de_jeu(Some(classee.clone()), &Some(menu()), Some(t0), t0 + min(2));
        let (ancien, duree) = t.fin.expect("une fin de partie");
        assert_eq!(relances_pour(&ancien, duree), 0);
    }

    /// Trois relances pour une file qui s'annonce (console comprise), une
    /// pour un combat à mort, aucune en personnalisée ou sous trois
    /// minutes.
    #[test]
    fn la_relance_depend_de_la_file() {
        let longue = Duration::from_secs(40 * 60);
        assert_eq!(relances_pour(&partie("competitive", false), longue), 3);
        assert_eq!(relances_pour(&partie("console_unrated", false), longue), 3);
        assert_eq!(relances_pour(&partie("swiftplay", false), Duration::from_secs(181)), 3);
        assert_eq!(relances_pour(&partie("premier", false), longue), 3);
        assert_eq!(relances_pour(&partie("deathmatch", false), longue), 1);
        assert_eq!(relances_pour(&partie("hurm", false), longue), 1);
        assert_eq!(relances_pour(&partie("", false), longue), 1);
        assert_eq!(relances_pour(&partie("", true), longue), 0);
        assert_eq!(relances_pour(&partie("competitive", false), Duration::from_secs(179)), 0);
        assert_eq!(relances_pour(&partie("competitive", false), Duration::ZERO), 0);
        // Et le service en tient compte : un combat à mort ne se relit
        // qu'une fois, un hoquet pas du tout.
        let (v, _rx, _dir) = service_a_l_arret("relance");
        compte(1, &v);
        compte(2, &v);
        v.fin_de_partie(1, &partie("deathmatch", false), longue);
        v.fin_de_partie(2, &partie("competitive", false), Duration::from_secs(10));
        let relances = v.etat.fil.relances.lock().unwrap();
        assert_eq!(relances.get(&1).map(|r| r.max), Some(1));
        assert!(!relances.contains_key(&2));
    }

    /// Un 429 gèle le seau — le temps que `Retry-After` demande, sous
    /// plafond, trente secondes sans en-tête — et se compte avec son
    /// heure. Et un rafraîchissement qui échoue ne reprogramme rien.
    #[test]
    fn un_429_gele_le_seau_et_n_engendre_pas_de_relance() {
        let mut seau = Seau::new();
        assert!(seau.gele().is_none());
        seau.geler(Duration::from_secs(12));
        let gel = seau.gele().expect("gelé");
        assert!(gel > Duration::from_secs(10) && gel <= Duration::from_secs(12));
        // Un gel plus court ne raccourcit pas ; un gel démesuré est plafonné.
        seau.geler(Duration::from_secs(1));
        assert!(seau.gele().unwrap() > Duration::from_secs(10));
        seau.geler(Duration::from_secs(3600));
        assert!(seau.gele().unwrap() <= GEL_MAX);
        assert_eq!(lire_retry_after(Some("7")), Some(Duration::from_secs(7)));
        assert_eq!(lire_retry_after(Some(" 30 ")), Some(Duration::from_secs(30)));
        assert_eq!(lire_retry_after(None), None);
        assert_eq!(lire_retry_after(Some("n'importe quoi")), None);
        // Une date passée : plus rien à attendre, mais l'en-tête se lit.
        assert_eq!(
            lire_retry_after(Some("Wed, 21 Oct 2015 07:28:00 GMT")),
            Some(Duration::ZERO)
        );
        let dans_une_heure = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc2822();
        assert!(lire_retry_after(Some(&dans_une_heure)).unwrap() > Duration::from_secs(3500));
        // Le compteur note le 429 et son heure.
        let c = Compteurs::default();
        let ms = maintenant_ms();
        c.noter(Requete {
            ms,
            motif: Motif::Relance { essai: 0, max: 3 },
            point: PointApi::Matches,
            code: 429,
            duree_ms: 40,
        });
        assert_eq!(c.refus_429.load(Ordering::Relaxed), 1);
        assert_eq!(c.dernier_429_ms.load(Ordering::Relaxed), ms);
        assert_eq!(c.erreurs.load(Ordering::Relaxed), 0);
        // Le fil, lui, ne reprogramme pas de relance sur un échec : la
        // première relance revient en 429, rien n'est reprogrammé, l'échec
        // est noté (le membre sort du périodique), rien à relayer.
        let (v, rx, dir) = service_a_l_arret("gel");
        compte(1, &v);
        let travaux = v.travaux.as_ref().unwrap();
        let c1 = v.etat.comptes.lock().unwrap()[&1].clone();
        let relance = Motif::Relance { essai: 0, max: 3 };
        assert!(apres_lecture(&v.etat, 1, &c1, relance, Err(Erreur::Limite), travaux).is_none());
        assert!(v.etat.fil.relances.lock().unwrap().is_empty(), "pas de relance sur un 429");
        assert!(v.etat.fil.derniere_tentative.lock().unwrap().contains_key(&1));
        assert!(v.a_rafraichir(&[1], Duration::from_secs(0)).is_empty());
        assert!(v.etat.fiches.lock().unwrap().is_empty(), "pas de fiche datée à vide");
        // Un succès sans match nouveau, lui, reprogramme la relance
        // suivante, oublie l'échec, écrit la fiche et la relaie.
        let fiche = FicheValorant {
            riot_id: c1.riot_id(),
            maj: ms,
            ..Default::default()
        };
        let r = apres_lecture(&v.etat, 1, &c1, relance, Ok((fiche, Vec::new())), travaux);
        assert!(matches!(r, Some(Resultat::Fiche { user_id: 1 })));
        assert_eq!(
            v.etat.fil.relances.lock().unwrap().get(&1).map(|r| (r.essai, r.max)),
            Some((1, 3))
        );
        assert!(!v.etat.fil.derniere_tentative.lock().unwrap().contains_key(&1));
        assert_eq!(v.etat.fiches.lock().unwrap().get(&1).map(|f| f.maj), Some(ms));
        // La dernière relance sans rien de neuf n'en reprogramme pas.
        v.etat.fil.relances.lock().unwrap().clear();
        let fiche = FicheValorant {
            riot_id: c1.riot_id(),
            ..Default::default()
        };
        let dernier = Motif::Relance { essai: 2, max: 3 };
        assert!(apres_lecture(&v.etat, 1, &c1, dernier, Ok((fiche, Vec::new())), travaux).is_some());
        assert!(v.etat.fil.relances.lock().unwrap().is_empty());
        assert!(travaux_en_file(&rx).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Un membre sans fiche que HenrikDev refuse n'est pas redemandé
    /// chaque minute : l'échec est noté, trente minutes de silence, puis
    /// on réessaie ; la déliaison efface tout.
    #[test]
    fn un_echec_sans_fiche_n_est_pas_retente_avant_trente_minutes() {
        let (v, _rx, _dir) = service_a_l_arret("echec");
        compte(1, &v);
        compte(2, &v);
        // Sans fiche ni échec : à relire.
        assert_eq!(v.a_rafraichir(&[1, 2], Duration::from_secs(1800)), vec![1, 2]);
        // Le périodique de 1 échoue, sans fiche : l'échec se note quand
        // même, et 1 sort du périodique.
        let travaux = v.travaux.as_ref().unwrap();
        let c1 = v.etat.comptes.lock().unwrap()[&1].clone();
        let echec = Err(Erreur::Autre("HTTP 500".into()));
        assert!(apres_lecture(&v.etat, 1, &c1, Motif::Periodique, echec, travaux).is_none());
        assert!(v.etat.fiches.lock().unwrap().get(&1).is_none(), "toujours sans fiche");
        assert_eq!(v.a_rafraichir(&[1, 2], Duration::from_secs(1800)), vec![2]);
        // Dix minutes plus tard, toujours pas ; trente et une, oui.
        let maintenant = maintenant_ms();
        {
            let mut t = v.etat.fil.derniere_tentative.lock().unwrap();
            t.insert(1, maintenant.saturating_sub(10 * 60_000));
            t.insert(2, maintenant.saturating_sub(31 * 60_000));
        }
        assert_eq!(v.a_rafraichir(&[1, 2], Duration::from_secs(1800)), vec![2]);
        // Hors ligne : jamais.
        assert!(v.a_rafraichir(&[], Duration::from_secs(1800)).is_empty());
        assert!(v.delier(1));
        assert!(!v.etat.fil.derniere_tentative.lock().unwrap().contains_key(&1));
        let _ = std::fs::remove_dir_all(_dir);
    }

    /// Chaque requête se compte par motif et par point d'API, les
    /// dernières restent en anneau, et le pic par minute se lit.
    #[test]
    fn le_motif_se_compte() {
        assert_eq!(PointApi::de("/valorant/v3/mmr/eu/pc/a/b"), PointApi::Mmr);
        assert_eq!(PointApi::de("/valorant/v2/mmr-history/eu/pc/a/b"), PointApi::History);
        assert_eq!(PointApi::de("/valorant/v4/matches/eu/pc/a/b?size=5"), PointApi::Matches);
        assert_eq!(PointApi::de("/valorant/v2/account/a/b"), PointApi::Account);
        assert_eq!(PointApi::de("/valorant/v1/stored-matches/eu/a/b"), PointApi::Stored);
        assert_eq!(PointApi::de("/valorant/v2/stored-mmr-history/eu/pc/a/b"), PointApi::Stored);
        assert_eq!(PointApi::de("/valorant/v1/esports/schedule"), PointApi::Esports);
        assert_eq!(PointApi::de("/valorant/v2/esports/vlr/events/1/matches"), PointApi::Esports);
        assert_eq!(PointApi::de("/valorant/v2/esports/vlr/events?type=upcoming"), PointApi::Esports);
        // Un membre au nom piégé ne déplace pas le compteur : c'est le
        // segment qui compte, pas ce qu'on trouve sur le chemin.
        assert_eq!(PointApi::de("/valorant/v4/matches/eu/pc/mmr/KI?size=5"), PointApi::Matches);
        assert_eq!(PointApi::de("/valorant/v2/mmr-history/eu/pc/stored-x/KI"), PointApi::History);
        assert_eq!(PointApi::de("/valorant/v3/mmr/eu/pc/esports/account"), PointApi::Mmr);
        assert_eq!(PointApi::de("/valorant/v9/inconnu/eu"), PointApi::Autre);
        assert_eq!(PointApi::de(""), PointApi::Autre);
        let c = Compteurs::default();
        let ms = maintenant_ms();
        let req = |motif, point, code, ms| Requete { ms, motif, point, code, duree_ms: 10 };
        for _ in 0..3 {
            c.noter(req(Motif::Periodique, PointApi::Mmr, 200, ms));
        }
        c.noter(req(Motif::Relance { essai: 1, max: 3 }, PointApi::Matches, 200, ms + 60_000));
        c.noter(req(Motif::CoMembre, PointApi::History, 500, ms + 60_000));
        c.noter(req(Motif::Liaison, PointApi::Account, 404, ms + 60_000));
        c.noter(req(Motif::Esports, PointApi::Esports, 0, ms + 60_000));
        assert_eq!(c.requetes.load(Ordering::Relaxed), 7);
        let motifs: Vec<u64> = c.par_motif.iter().map(|n| n.load(Ordering::Relaxed)).collect();
        assert_eq!(motifs, vec![3, 1, 1, 1, 1]);
        let points: Vec<u64> = c.par_point.iter().map(|n| n.load(Ordering::Relaxed)).collect();
        assert_eq!(points, vec![3, 1, 1, 1, 0, 1, 0]);
        // Le 404 n'est pas une erreur ; le 500 et le réseau muet le sont.
        assert_eq!(c.erreurs.load(Ordering::Relaxed), 2);
        assert_eq!(c.pic(ms + 60_000), 4);
        // Une heure plus tard, le pic est retombé.
        assert_eq!(c.pic(ms + 61 * 60_000 + 60_000), 0);
        // L'anneau garde les soixante dernières.
        for i in 0..70u64 {
            c.noter(req(Motif::Periodique, PointApi::Mmr, 200, ms + 120_000 + i));
        }
        let journal = c.journal.lock().unwrap();
        assert_eq!(journal.len(), JOURNAL_REQUETES);
        assert_eq!(journal.back().map(|r| r.ms), Some(ms + 120_000 + 69));
        drop(journal);
        assert_eq!(Motif::Relance { essai: 1, max: 3 }.nom(), "relance 2/3");
        assert_eq!(Motif::CoMembre.nom(), "co-membre");
        let (v, _rx, _dir) = service_a_l_arret("motif");
        v.etat.compteurs.noter(req(Motif::Liaison, PointApi::Account, 200, ms));
        let texte = v.compteurs_texte();
        assert!(texte.contains("liaison 1"), "{texte}");
        assert!(texte.contains("points d'API : account 1"), "{texte}");
        let journal = v.journal_texte();
        assert!(journal.contains("liaison · account · 200 · 10 ms"), "{journal}");
    }
}
