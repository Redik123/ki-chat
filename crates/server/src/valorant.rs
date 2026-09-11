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

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ki_protocol::{FicheValorant, MatchResume, PointRR, RangValorant, UserId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Requêtes tolérées par minute glissante — sous les trente de la clé.
const BUDGET_PAR_MINUTE: usize = 20;
/// Points d'historique et matchs gardés par fiche.
const HISTORIQUE_MAX: usize = 10;
const MATCHS_MAX: usize = 5;
const BASE: &str = "https://api.henrikdev.xyz";

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
    Lier { user_id: UserId, nom: String, tag: String },
    Rafraichir { user_id: UserId },
}

/// Ce qu'il rapporte, à relayer aux clients.
pub enum Resultat {
    Liaison { user_id: UserId, ok: bool, message: String, riot_id: Option<String> },
    Fiche { user_id: UserId },
}

struct Etat {
    dossier: PathBuf,
    comptes: Mutex<BTreeMap<UserId, CompteRiot>>,
    fiches: Mutex<BTreeMap<UserId, FicheValorant>>,
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
        let fiches = lire(&dossier.join("fiches.json"));
        let etat = Arc::new(Etat {
            dossier,
            comptes: Mutex::new(comptes),
            fiches: Mutex::new(fiches),
        });
        let (tx_res, rx_res) = mpsc::channel();
        let cle = cle_henrik(data_dir);
        let travaux = cle.map(|cle| {
            let (tx, rx) = mpsc::channel();
            let etat = Arc::clone(&etat);
            std::thread::Builder::new()
                .name("ki-henrik".into())
                .spawn(move || fil(cle, etat, rx, tx_res))
                .expect("fil HenrikDev");
            tx
        });
        if travaux.is_some() {
            tracing::info!("VALORANT : clé HenrikDev trouvée, liaisons ouvertes");
        } else {
            tracing::info!("VALORANT : pas de clé HenrikDev (KI_HENRIK_KEY ou data/henrik.key), liaisons fermées");
        }
        Self { etat, travaux, resultats: Mutex::new(rx_res) }
    }

    pub fn riot_id(&self, user_id: UserId) -> Option<String> {
        self.etat.comptes.lock().unwrap().get(&user_id).map(CompteRiot::riot_id)
    }

    pub fn rang(&self, user_id: UserId) -> Option<u8> {
        self.etat.fiches.lock().unwrap().get(&user_id).map(|f| f.rang.tier)
    }

    pub fn fiche(&self, user_id: UserId) -> Option<FicheValorant> {
        self.etat.fiches.lock().unwrap().get(&user_id).cloned()
    }

    /// Toutes les fiches, pour la page de stats du groupe.
    pub fn toutes(&self) -> Vec<(UserId, FicheValorant)> {
        self.etat.fiches.lock().unwrap().iter().map(|(id, f)| (*id, f.clone())).collect()
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
                *id != user_id && c.nom.eq_ignore_ascii_case(&nom) && c.tag.eq_ignore_ascii_case(&tag)
            });
            if deja {
                return Err("ce compte Riot est déjà lié à un autre membre".into());
            }
        }
        travaux
            .send(Travail::Lier { user_id, nom, tag })
            .map_err(|_| "le service VALORANT est arrêté".to_string())
    }

    /// Retire compte et fiche. `false` s'il n'y avait rien.
    pub fn delier(&self, user_id: UserId) -> bool {
        let retire = self.etat.comptes.lock().unwrap().remove(&user_id).is_some();
        let fiche = self.etat.fiches.lock().unwrap().remove(&user_id).is_some();
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
                let _ = travaux.send(Travail::Rafraichir { user_id });
            }
        }
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
}

fn lire<T: serde::de::DeserializeOwned>(chemin: &std::path::Path) -> BTreeMap<UserId, T> {
    match std::fs::read_to_string(chemin) {
        Ok(texte) => serde_json::from_str(&texte).unwrap_or_else(|e| {
            tracing::warn!("VALORANT : {} illisible ({e}), reparti de zéro", chemin.display());
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
                tracing::warn!("VALORANT : écriture de {} impossible : {e}", chemin.display());
            }
        }
        Err(e) => tracing::warn!("VALORANT : sérialisation impossible : {e}"),
    }
}

/// La clé : la variable d'environnement d'abord, le fichier ensuite.
/// Jamais dans le binaire ni dans le dépôt.
fn cle_henrik(data_dir: &str) -> Option<String> {
    let env = std::env::var("KI_HENRIK_KEY").ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    env.or_else(|| {
        std::fs::read_to_string(PathBuf::from(data_dir).join("henrik.key"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

pub fn maintenant_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
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
        Self { passees: VecDeque::with_capacity(BUDGET_PAR_MINUTE) }
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
}

impl Api {
    fn get(&mut self, chemin: &str) -> Result<Value, Erreur> {
        let url = format!("{BASE}{chemin}");
        let mut essais = 0;
        loop {
            self.seau.prendre();
            essais += 1;
            match self.agent.get(&url).set("Authorization", &self.cle).call() {
                Ok(reponse) => {
                    return reponse.into_json::<Value>().map_err(|e| Erreur::Autre(e.to_string()));
                }
                Err(ureq::Error::Status(404, _)) => return Err(Erreur::Introuvable),
                Err(ureq::Error::Status(429, _)) if essais < 2 => {
                    tracing::warn!("VALORANT : HenrikDev renvoie 429, pause de trente secondes");
                    std::thread::sleep(Duration::from_secs(30));
                }
                Err(ureq::Error::Status(429, _)) => return Err(Erreur::Limite),
                Err(ureq::Error::Status(code, reponse)) => {
                    let detail = reponse
                        .into_json::<Value>()
                        .ok()
                        .and_then(|v| v["errors"][0]["message"].as_str().map(str::to_string))
                        .unwrap_or_default();
                    return Err(Erreur::Autre(format!("HTTP {code} {detail}").trim().to_string()));
                }
                Err(e) => return Err(Erreur::Autre(e.to_string())),
            }
        }
    }
}

fn fil(cle: String, etat: Arc<Etat>, rx: Receiver<Travail>, tx: Sender<Resultat>) {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("ki-chat-server/", env!("CARGO_PKG_VERSION")))
        .build();
    let mut api = Api { agent, cle, seau: Seau::new() };
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
                let _ = tx.send(Resultat::Liaison { user_id, ok, message, riot_id });
            }
            Travail::Rafraichir { user_id } => {
                let compte = etat.comptes.lock().unwrap().get(&user_id).cloned();
                let Some(compte) = compte else { continue };
                match construire(&mut api, &compte, None) {
                    Ok(fiche) => {
                        etat.fiches.lock().unwrap().insert(user_id, fiche);
                        etat.sauver_fiches();
                        let _ = tx.send(Resultat::Fiche { user_id });
                    }
                    Err(e) => {
                        tracing::warn!("VALORANT : fiche de {} non rafraîchie : {}", compte.riot_id(), e.message());
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
fn lier(api: &mut Api, etat: &Etat, user_id: UserId, nom: &str, tag: &str) -> Result<FicheValorant, Erreur> {
    let compte = api.get(&format!("/valorant/v2/account/{}/{}", enc(nom), enc(tag)))?;
    let d = &compte["data"];
    let puuid = d["puuid"].as_str().ok_or(Erreur::Introuvable)?.to_string();
    let region = d["region"].as_str().unwrap_or("eu").to_string();
    let plateformes: Vec<&str> = d["platforms"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let plateforme = if plateformes.iter().any(|p| p.eq_ignore_ascii_case("pc")) || plateformes.is_empty() {
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
    let fiche = construire(api, &compte, Some(niveau))?;
    etat.comptes.lock().unwrap().insert(user_id, compte);
    etat.fiches.lock().unwrap().insert(user_id, fiche.clone());
    etat.sauver_comptes();
    etat.sauver_fiches();
    Ok(fiche)
}

/// Trois requêtes : rang, historique de RR, derniers matchs.
fn construire(api: &mut Api, compte: &CompteRiot, niveau: Option<u32>) -> Result<FicheValorant, Erreur> {
    let suffixe = format!("{}/{}/{}/{}", compte.region, compte.plateforme, enc(&compte.nom), enc(&compte.tag));
    let mmr = api.get(&format!("/valorant/v3/mmr/{suffixe}"))?;
    let historique = api.get(&format!("/valorant/v2/mmr-history/{suffixe}")).unwrap_or(Value::Null);
    let matchs = api.get(&format!("/valorant/v4/matches/{suffixe}?size={MATCHS_MAX}")).unwrap_or(Value::Null);

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
        .map(|liste| liste.iter().filter_map(|m| resumer_match(m, &compte.puuid)).take(MATCHS_MAX).collect())
        .unwrap_or_default();
    if niveau.is_none() {
        // Le niveau de compte vient avec chaque match : le plus récent.
        if let Some(n) = matchs["data"]
            .as_array()
            .and_then(|l| l.first())
            .and_then(|m| m["players"].as_array())
            .and_then(|ps| ps.iter().find(|p| p["puuid"].as_str() == Some(&compte.puuid)))
            .and_then(|p| p["account_level"].as_u64())
        {
            fiche.niveau = n as u32;
        }
    }
    Ok(fiche)
}

/// La ligne du membre dans un match — et rien des autres joueurs.
fn resumer_match(m: &Value, puuid: &str) -> Option<MatchResume> {
    let meta = &m["metadata"];
    let joueur = m["players"].as_array()?.iter().find(|p| p["puuid"].as_str() == Some(puuid))?;
    let equipe = joueur["team_id"].as_str().unwrap_or("");
    let camp = m["teams"].as_array().and_then(|ts| ts.iter().find(|t| t["team_id"].as_str() == Some(equipe)));
    let (gagnees, perdues) = camp
        .map(|t| (t["rounds"]["won"].as_u64().unwrap_or(0) as u8, t["rounds"]["lost"].as_u64().unwrap_or(0) as u8))
        .unwrap_or((0, 0));
    let gagne = match camp.and_then(|t| t["won"].as_bool()) {
        Some(true) => Some(true),
        _ if gagnees == perdues => None,
        Some(false) => Some(false),
        None => None,
    };
    let stats = &joueur["stats"];
    let tetes = stats["headshots"].as_u64().unwrap_or(0);
    let tirs = tetes + stats["bodyshots"].as_u64().unwrap_or(0) + stats["legshots"].as_u64().unwrap_or(0);
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
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(octet as char),
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
    let (y, m) = if mois <= 2 { (an - 1, mois + 9) } else { (an, mois - 3) };
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
        let _ = std::fs::remove_dir_all(dir);
    }
}
