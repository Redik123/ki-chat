//! Le statut VALORANT, lu chez soi — jalon V1 de PLAN-VALORANT.md.
//!
//! Quand le client Riot tourne, il écrit un « lockfile »
//! (`%LocalAppData%\Riot Games\Riot Client\Config\lockfile`, une ligne
//! `nom:pid:port:motdepasse:protocole`) et sert sur `https://127.0.0.1:port`
//! une petite API locale : sa session, et la **présence** — la sienne et
//! celle de ses amis, c'est ce que le jeu affiche dans sa liste d'amis. La
//! présence VALORANT est un JSON encodé en base64 : état de session, file,
//! carte, score de la party, taille de la party, rang.
//!
//! On ne lit que la sienne, en local, en lecture seule, et on n'en garde que
//! ce que [`JeuStatut`] porte. Aucun jeton ne sort de la machine : ceux du
//! client Riot permettraient d'acheter dans la boutique. Rien de tout ça
//! n'est supporté par Riot ; le jour où le format change, le statut s'éteint
//! sans bruit — c'est prévu.
//!
//! La lecture se fait par sondage toutes les deux secondes : assez réactif
//! pour un score qui change, dérisoire pour le client Riot, et bien plus
//! simple qu'une WebSocket TLS vers un certificat auto-signé.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use eframe::egui;
use ki_protocol::{JeuEtat, JeuStatut};
use serde::Deserialize;

/// Cadence de lecture.
const PERIODE: Duration = Duration::from_secs(2);

/// Le fil qui surveille le client Riot et publie le statut aux changements.
pub struct Veilleur {
    statut: Arc<Mutex<Option<JeuStatut>>>,
    /// Incrémentée à chaque statut différent : l'interface compare.
    version: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Veilleur {
    pub fn demarrer(ctx: egui::Context) -> Self {
        let statut = Arc::new(Mutex::new(None));
        let version = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new()
            .name("ki-valorant".into())
            .spawn({
                let (statut, version, stop) = (statut.clone(), version.clone(), stop.clone());
                move || boucle(ctx, statut, version, stop)
            })
            .ok();
        Self { statut, version, stop, thread }
    }

    /// La version et le statut courant. La version ne bouge qu'aux
    /// changements : l'appelant n'envoie que ceux-là.
    pub fn releve(&self) -> (u64, Option<JeuStatut>) {
        (self.version.load(Ordering::Relaxed), self.statut.lock().unwrap().clone())
    }
}

impl Drop for Veilleur {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Ce que le lockfile dit : où joindre le client, et comment.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Lockfile {
    port: u16,
    password: String,
}

/// `nom:pid:port:motdepasse:protocole`.
fn parser_lockfile(contenu: &str) -> Option<Lockfile> {
    let mut parts = contenu.trim().split(':');
    let _nom = parts.next()?;
    let _pid = parts.next()?;
    let port: u16 = parts.next()?.parse().ok()?;
    let password = parts.next()?.to_string();
    (!password.is_empty()).then_some(Lockfile { port, password })
}

pub(crate) fn lire_lockfile() -> Option<Lockfile> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    let chemin = std::path::PathBuf::from(base)
        .join("Riot Games")
        .join("Riot Client")
        .join("Config")
        .join("lockfile");
    let contenu = std::fs::read_to_string(chemin).ok()?;
    parser_lockfile(&contenu)
}

/// Le client HTTP vers l'API locale : certificat auto-signé du client Riot
/// accepté (c'est la boucle locale, pas le réseau), mot de passe du lockfile
/// en Basic.
pub(crate) struct Client {
    agent: ureq::Agent,
    base: String,
    auth: String,
}

impl Client {
    pub(crate) fn new(lf: &Lockfile) -> Self {
        let agent = ureq::AgentBuilder::new()
            .tls_config(ki_client_quic::local_tls_config())
            .timeout(Duration::from_secs(3))
            .build();
        let auth = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("riot:{}", lf.password))
        );
        Self { agent, base: format!("https://127.0.0.1:{}", lf.port), auth }
    }

    pub(crate) fn get<T: serde::de::DeserializeOwned>(&self, chemin: &str) -> anyhow::Result<T> {
        let reponse = self
            .agent
            .get(&format!("{}{chemin}", self.base))
            .set("Authorization", &self.auth)
            .call()?;
        Ok(reponse.into_json()?)
    }
}

#[derive(Deserialize)]
pub(crate) struct Session {
    #[serde(default)]
    pub(crate) puuid: String,
    /// Le Riot ID de la session, en deux morceaux (« Redik », « 6162 »).
    #[serde(default)]
    pub(crate) game_name: String,
    #[serde(default)]
    pub(crate) game_tag: String,
}

/// Ce que VALORANT lui-même annonce à ses serveurs ; sans quoi la porte
/// est fermée (Cloudflare, code 1010).
const AGENT_DU_JEU: &str = "ShooterGame/13 Windows/10.0.19043.1.256.64bit";
const TIMEOUT_PD: Duration = Duration::from_secs(15);

/// De quoi parler aux serveurs de VALORANT (`pd.<région>.a.pvp.net`) au
/// nom de soi-même, prêté par son propre client Riot. Vit le temps d'une
/// lecture, sur un fil à part : les jetons ne sont ni journalisés, ni
/// gardés, ni envoyés ailleurs qu'à Riot — ils permettraient d'acheter.
pub(crate) struct Acces {
    pub(crate) puuid: String,
    /// « Pseudo#TAG » : ce que ki-chat compare au compte lié.
    pub(crate) riot_id: String,
    /// La région des serveurs de jeu (`eu`, `na`, `ap`, `kr`).
    pub(crate) region: String,
    agent: ureq::Agent,
    acces: String,
    droit: String,
    version: String,
    plateforme: String,
}

impl Acces {
    /// Le lockfile → la session → les jetons → la région → la version du
    /// client. Échoue proprement si le client Riot est fermé ou déconnecté.
    pub(crate) fn ouvrir() -> anyhow::Result<Self> {
        let lf = lire_lockfile().ok_or_else(|| anyhow::anyhow!("client Riot fermé"))?;
        let client = Client::new(&lf);
        let session: Session = client.get("/chat/v1/session")?;
        if session.puuid.is_empty() {
            anyhow::bail!("pas encore connecté au client Riot");
        }
        let jetons: serde_json::Value = client.get("/entitlements/v1/token")?;
        let acces = jetons["accessToken"].as_str().unwrap_or("").to_string();
        let droit = jetons["token"].as_str().unwrap_or("").to_string();
        if acces.is_empty() || droit.is_empty() {
            anyhow::bail!("le client Riot n'a pas de jeton (pas connecté ?)");
        }
        let region = region_du_jeu(&client).or_else(|| region_du_client(&client)).ok_or_else(|| {
            anyhow::anyhow!("région inconnue (VALORANT n'est pas lancé, et le client Riot ne la dit pas)")
        })?;
        let version: serde_json::Value = ureq::get("https://valorant-api.com/v1/version")
            .set("User-Agent", "ki-chat")
            .timeout(TIMEOUT_PD)
            .call()?
            .into_json()?;
        let version = version["data"]["riotClientVersion"].as_str().unwrap_or("").to_string();
        let plateforme = base64::engine::general_purpose::STANDARD.encode(
            r#"{"platformType":"PC","platformOS":"Windows","platformOSVersion":"10.0.19042.1.256.64bit","platformChipset":"Unknown"}"#,
        );
        let riot_id = if session.game_name.is_empty() {
            String::new()
        } else {
            format!("{}#{}", session.game_name, session.game_tag)
        };
        Ok(Self {
            puuid: session.puuid,
            riot_id,
            region,
            agent: ureq::AgentBuilder::new().timeout(TIMEOUT_PD).build(),
            acces,
            droit,
            version,
            plateforme,
        })
    }

    fn requete(&self, methode: &str, chemin: &str) -> ureq::Request {
        self.agent
            .request(methode, &format!("https://pd.{}.a.pvp.net{chemin}", self.region))
            .set("User-Agent", AGENT_DU_JEU)
            .set("Authorization", &format!("Bearer {}", self.acces))
            .set("X-Riot-Entitlements-JWT", &self.droit)
            .set("X-Riot-ClientPlatform", &self.plateforme)
            .set("X-Riot-ClientVersion", &self.version)
    }

    /// `GET` sur les serveurs de jeu ; une erreur ne dit que son code HTTP.
    pub(crate) fn get(&self, chemin: &str) -> anyhow::Result<serde_json::Value> {
        let reponse = self.requete("GET", chemin).call().map_err(erreur_pd)?;
        Ok(reponse.into_json()?)
    }

    /// `POST` d'un corps JSON sur les serveurs de jeu.
    pub(crate) fn post(&self, chemin: &str, corps: &str) -> anyhow::Result<serde_json::Value> {
        let reponse = self
            .requete("POST", chemin)
            .set("Content-Type", "application/json")
            .send_string(corps)
            .map_err(erreur_pd)?;
        Ok(reponse.into_json()?)
    }
}

fn erreur_pd(e: ureq::Error) -> anyhow::Error {
    match e {
        ureq::Error::Status(code, _) => anyhow::anyhow!("refusé par Riot (HTTP {code})"),
        e => anyhow::anyhow!("serveurs de VALORANT injoignables : {e}"),
    }
}

/// La région telle que le jeu lancé l'annonce (`-ares-deployment=eu` dans
/// les arguments de la session externe) : la plus sûre.
fn region_du_jeu(client: &Client) -> Option<String> {
    let sessions: serde_json::Value = client.get("/product-session/v1/external-sessions").ok()?;
    let region = sessions
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
        .filter_map(|s| s["launchConfiguration"]["arguments"].as_array())
        .flatten()
        .filter_map(|a| a.as_str())
        .find_map(|a| a.strip_prefix("-ares-deployment="))?
        .to_ascii_lowercase();
    (!region.is_empty() && region.chars().all(|c| c.is_ascii_lowercase())).then_some(region)
}

/// Sans le jeu : la région du client Riot (`/riotclient/region-locale`),
/// ramenée à celle des serveurs de VALORANT — l'Amérique latine et le
/// Brésil jouent sur `na`, le Japon sur `ap`.
fn region_du_client(client: &Client) -> Option<String> {
    let r: serde_json::Value = client.get("/riotclient/region-locale").ok()?;
    region_de_jeu(r["region"].as_str().or_else(|| r["webRegion"].as_str())?)
}

/// La région des serveurs de VALORANT pour une région du client Riot.
fn region_de_jeu(region: &str) -> Option<String> {
    let r = region.trim().to_ascii_lowercase();
    let r = r.trim_end_matches(|c: char| c.is_ascii_digit());
    Some(
        match r {
            "eu" | "euw" | "eune" | "tr" | "ru" | "me" => "eu",
            "na" | "latam" | "la" | "lan" | "las" | "br" | "pbe" => "na",
            "ap" | "oc" | "oce" | "jp" | "sea" | "ph" | "sg" | "th" | "tw" | "vn" => "ap",
            "kr" => "kr",
            _ => return None,
        }
        .to_string(),
    )
}

#[derive(Deserialize)]
struct Presences {
    #[serde(default)]
    presences: Vec<Presence>,
}

#[derive(Deserialize)]
struct Presence {
    #[serde(default)]
    puuid: String,
    #[serde(default)]
    product: String,
    #[serde(default)]
    private: Option<String>,
}

/// Le JSON de la présence VALORANT, une fois décodé. Tous les champs sont
/// facultatifs : le format n'est documenté par personne d'officiel, et il a
/// déjà changé — les clients de 2026 (13.x) rangent l'essentiel dans des
/// blocs (`matchPresenceData`, `partyPresenceData`, `playerPresenceData`),
/// les anciens mettaient tout à plat. On lit les deux : le bloc d'abord, le
/// champ à plat en repli.
#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Prive {
    // --- À plat (anciens clients) ---
    session_loop_state: String,
    party_owner_match_map: String,
    match_map: String,
    party_owner_match_score_ally_team: u32,
    party_owner_match_score_enemy_team: u32,
    queue_id: String,
    party_size: u32,
    max_party_size: u32,
    party_accessibility: String,
    party_state: String,
    competitive_tier: u32,
    account_level: u32,
    provisioning_flow: String,
    // --- En blocs (clients 13.x) ---
    match_presence_data: MatchPresence,
    party_presence_data: PartyPresence,
    player_presence_data: PlayerPresence,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct MatchPresence {
    session_loop_state: String,
    match_map: String,
    queue_id: String,
    provisioning_flow: String,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct PartyPresence {
    party_owner_match_map: String,
    /// `DEFAULT` au repos, `MATCHMAKING` en file d'attente.
    party_state: String,
    party_accessibility: String,
    /// « Looking for more » : la party cherche du monde.
    party_lfm: bool,
    party_size: u32,
    max_party_size: u32,
    custom_game_name: String,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct PlayerPresence {
    account_level: u32,
    competitive_tier: u32,
}

/// Le premier texte non vide, ou vide.
fn premier<'a>(candidats: &[&'a str]) -> &'a str {
    candidats.iter().copied().find(|s| !s.is_empty()).unwrap_or("")
}

pub(crate) fn decoder_prive(b64: &str) -> Option<Prive> {
    let octets = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
    serde_json::from_slice(&octets).ok()
}

/// Le nom d'affichage d'une carte depuis son chemin interne
/// (`/Game/Maps/Bonsai/Bonsai` → « Split »). Les noms internes sont ceux
/// du développement, pas ceux des joueurs.
pub(crate) fn nom_de_carte(chemin: &str) -> String {
    let interne = chemin.rsplit('/').next().unwrap_or("").trim();
    let connu = match interne {
        "" => "",
        "Ascent" => "Ascent",
        "Bonsai" => "Split",
        "Duality" => "Bind",
        "Triad" => "Haven",
        "Port" => "Icebox",
        "Foxtrot" => "Breeze",
        "Canyon" => "Fracture",
        "Pitt" => "Pearl",
        "Jam" => "Lotus",
        "Juliett" => "Sunset",
        "Infinity" => "Abyss",
        "Rook" => "Corrode",
        "Plummet" => "Summit",
        "Range" | "RangeV2" => "Stand de tir",
        "NPEV2" => "Entraînement de base",
        // Les cartes du team deathmatch, d'après valorant-api.com (mapUrl).
        "HURM_Alley" => "District",
        "HURM_Bowl" => "Kasbah",
        "HURM_Helix" => "Drift",
        "HURM_Yard" => "Piazza",
        "HURM_HighTide" => "Glitch",
        // Les arènes du battle royale Gauntlet (13.06).
        "AbilityDraft" => "Gauntlet",
        "Skirmish_A" => "Escarmouche A",
        "Skirmish_B" => "Escarmouche B",
        "Skirmish_C" => "Escarmouche C",
        "Skirmish_D" => "Escarmouche D",
        "Skirmish_E" => "Escarmouche E",
        autre => autre,
    };
    connu.to_string()
}

/// De la présence brute au statut qu'on partage. `None` : pas en jeu.
pub(crate) fn normaliser(p: &Prive) -> Option<JeuStatut> {
    let m = &p.match_presence_data;
    let pa = &p.party_presence_data;
    let pl = &p.player_presence_data;
    let etat = match premier(&[&m.session_loop_state, &p.session_loop_state]) {
        "MENUS" => JeuEtat::Menus,
        "PREGAME" => JeuEtat::PreGame,
        "INGAME" => JeuEtat::EnJeu,
        _ => return None,
    };
    let carte = premier(&[&m.match_map, &p.match_map, &pa.party_owner_match_map, &p.party_owner_match_map]);
    let file = premier(&[&m.queue_id, &p.queue_id]);
    let flux = premier(&[&m.provisioning_flow, &p.provisioning_flow]);
    // Au menu, la file annoncée n'est que le mode sélectionné : on ne la
    // dit qu'en file d'attente réelle.
    let en_file = premier(&[&pa.party_state, &p.party_state]) == "MATCHMAKING";
    let custom = flux == "CustomGame"
        || !pa.custom_game_name.is_empty()
        || (file.is_empty() && etat != JeuEtat::Menus);
    let (taille, max) = if pa.party_size > 0 {
        (pa.party_size, pa.max_party_size)
    } else {
        (p.party_size, p.max_party_size)
    };
    let acces = premier(&[&pa.party_accessibility, &p.party_accessibility]);
    let (rang, niveau) = if pl.competitive_tier > 0 || pl.account_level > 0 {
        (pl.competitive_tier, pl.account_level)
    } else {
        (p.competitive_tier, p.account_level)
    };
    Some(
        JeuStatut {
            etat,
            file: if etat == JeuEtat::Menus && !en_file { String::new() } else { file.to_string() },
            carte: if etat == JeuEtat::Menus { String::new() } else { nom_de_carte(carte) },
            score_allie: p.party_owner_match_score_ally_team.min(99) as u8,
            score_adverse: p.party_owner_match_score_enemy_team.min(99) as u8,
            party_taille: taille.min(10) as u8,
            party_max: max.min(10) as u8,
            party_ouverte: acces == "OPEN" || pa.party_lfm,
            rang: rang.min(27) as u8,
            niveau: niveau.min(9999),
            custom,
            nom: String::new(),
        }
        .nettoyer(),
    )
}

fn boucle(
    ctx: egui::Context,
    statut: Arc<Mutex<Option<JeuStatut>>>,
    version: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
) {
    let publier = |nouveau: Option<JeuStatut>| {
        let mut courant = statut.lock().unwrap();
        if *courant != nouveau {
            *courant = nouveau;
            version.fetch_add(1, Ordering::Relaxed);
            ctx.request_repaint();
        }
    };
    // Le client courant : le lockfile qui l'a ouvert, la connexion, et le
    // PUUID de la personne — pour reconnaître sa propre présence parmi
    // celles de ses amis.
    let mut client: Option<(Lockfile, Client, String)> = None;
    // Le dernier ennui consigné : on ne le répète pas à chaque tour, mais un
    // ennui différent se dit — c'est ce qui fera comprendre, à distance,
    // pourquoi le statut reste vide.
    let mut dernier_ennui = String::new();
    let mut ennui = |quoi: String| {
        if quoi != dernier_ennui {
            ki_voice::journal(format!("Valorant : {quoi}"));
            dernier_ennui = quoi;
        }
    };

    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(PERIODE);
        let Some(lf) = lire_lockfile() else {
            // Client Riot fermé.
            client = None;
            publier(None);
            continue;
        };
        if client.as_ref().is_none_or(|(ouvert, _, _)| *ouvert != lf) {
            let c = Client::new(&lf);
            match c.get::<Session>("/chat/v1/session") {
                Ok(s) if !s.puuid.is_empty() => {
                    ennui("client Riot joint, session lue".into());
                    client = Some((lf, c, s.puuid));
                }
                Ok(_) => {
                    ennui("session sans identifiant (le client démarre ?)".into());
                    client = None;
                    publier(None);
                    continue;
                }
                Err(e) => {
                    // Le client démarre encore, ou refuse : on réessaie au
                    // prochain tour.
                    ennui(format!("session locale injoignable : {e}"));
                    client = None;
                    publier(None);
                    continue;
                }
            }
        }
        let Some((_, c, puuid)) = client.as_ref() else { continue };
        let nouveau = match c.get::<Presences>("/chat/v4/presences") {
            Ok(p) => {
                let mienne = p.presences.iter().find(|x| x.puuid == *puuid && x.product == "valorant");
                match mienne.and_then(|x| x.private.as_deref()) {
                    None => None,
                    Some(prive) => match decoder_prive(prive) {
                        Some(pr) => normaliser(&pr),
                        None => {
                            ennui("présence illisible (le format a changé ?)".into());
                            None
                        }
                    },
                }
            }
            Err(e) => {
                // Le client s'est fermé ou rechargé : on repartira du lockfile.
                ennui(format!("présences injoignables : {e}"));
                client = None;
                None
            }
        };
        publier(nouveau);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Le lockfile a cinq champs séparés par des deux-points ; il faut le
    /// port et le mot de passe, le reste ne nous regarde pas.
    #[test]
    fn le_lockfile_se_lit() {
        let lf = parser_lockfile("Riot Client:1234:56789:abcDEF:https\n").unwrap();
        assert_eq!((lf.port, lf.password.as_str()), (56789, "abcDEF"));
        assert!(parser_lockfile("Riot Client:1234").is_none());
        assert!(parser_lockfile("Riot Client:1234:port:mdp:https").is_none());
    }

    /// Une présence telle que le client la fabrique — en partie compétitive
    /// sur Ascent, 7-5, en party de trois — devient une ligne lisible.
    #[test]
    fn la_presence_se_normalise() {
        let json = r#"{"isValid":true,"sessionLoopState":"INGAME","partyOwnerSessionLoopState":"INGAME",
            "partyOwnerMatchMap":"/Game/Maps/Ascent/Ascent","matchMap":"/Game/Maps/Ascent/Ascent",
            "partyOwnerMatchScoreAllyTeam":7,"partyOwnerMatchScoreEnemyTeam":5,"queueId":"competitive",
            "partySize":3,"maxPartySize":5,"partyAccessibility":"OPEN","competitiveTier":15,
            "accountLevel":120,"provisioningFlow":"Matchmaking","isIdle":false,"customGameName":""}"#;
        let b64 = base64::engine::general_purpose::STANDARD.encode(json);
        let prive = decoder_prive(&b64).expect("décodage");
        let statut = normaliser(&prive).expect("en jeu");
        assert_eq!(statut.etat, JeuEtat::EnJeu);
        assert_eq!(statut.ligne(), "compétitive · Ascent · 7-5 · party 3/5");
        assert!(statut.party_ouverte && statut.rang == 15 && !statut.custom);

        // Au menu, sans file : la carte n'a pas de sens, et rien d'autre.
        let menu = Prive { session_loop_state: "MENUS".into(), ..Default::default() };
        assert_eq!(normaliser(&menu).unwrap().ligne(), "Valorant · au menu");

        // Le format des clients 13.x (2026) : tout en blocs, et au menu la
        // file n'est que le mode sélectionné — pas une attente.
        let json = r#"{"isIdle":false,"isValid":true,"maxPartySize":5,
            "partyOwnerMatchScoreAllyTeam":7,"partyOwnerMatchScoreEnemyTeam":8,"partySize":2,
            "provisioningFlow":"Matchmaking","queueId":"console_competitive",
            "matchPresenceData":{"gameScoreType":"Rounds","matchMap":"/Game/Maps/Infinity/Infinity",
              "provisioningFlow":"Matchmaking","queueId":"console_competitive","sessionLoopState":"INGAME"},
            "partyPresenceData":{"customGameName":"","isPartyOwner":true,"maxPartySize":5,
              "partyAccessibility":"CLOSED","partyLFM":false,"partyOwnerMatchMap":"/Game/Maps/Infinity/Infinity",
              "partyOwnerSessionLoopState":"INGAME","partySize":2,"partyState":"DEFAULT"},
            "playerPresenceData":{"accountLevel":82,"competitiveTier":10,"platformOverride":"playstation"},
            "premierPresenceData":{"division":0}}"#;
        let prive = decoder_prive(&base64::engine::general_purpose::STANDARD.encode(json)).unwrap();
        let s = normaliser(&prive).unwrap();
        assert_eq!(s.ligne(), "compétitive (console) · Abyss · 7-8 · party 2/5");
        assert_eq!((s.rang, s.niveau), (10, 82));

        let json = r#"{"isValid":true,"queueId":"unrated","partySize":1,"maxPartySize":5,
            "matchPresenceData":{"matchMap":"","provisioningFlow":"Invalid","queueId":"unrated","sessionLoopState":"MENUS"},
            "partyPresenceData":{"partyState":"DEFAULT","partySize":1,"maxPartySize":5,"partyAccessibility":"CLOSED"},
            "playerPresenceData":{"accountLevel":210,"competitiveTier":15}}"#;
        let prive = decoder_prive(&base64::engine::general_purpose::STANDARD.encode(json)).unwrap();
        assert_eq!(normaliser(&prive).unwrap().ligne(), "Valorant · au menu");
        let json = json.replace("\"partyState\":\"DEFAULT\"", "\"partyState\":\"MATCHMAKING\"");
        let prive = decoder_prive(&base64::engine::general_purpose::STANDARD.encode(json)).unwrap();
        assert_eq!(normaliser(&prive).unwrap().ligne(), "Valorant · en file non classée");
        // Un état inconnu (le format a changé) : pas de statut, pas de bruit.
        let inconnu = Prive { session_loop_state: "AUTRE".into(), ..Default::default() };
        assert!(normaliser(&inconnu).is_none());
        assert!(decoder_prive("pas du base64 !!").is_none());
    }

    /// Les régions du client Riot se ramènent à celles des serveurs de
    /// VALORANT ; une inconnue ne se devine pas.
    #[test]
    fn la_region_du_client_se_ramene_a_celle_du_jeu() {
        for (client, jeu) in [("EUW", "eu"), ("eu", "eu"), ("NA1", "na"), ("BR", "na"), ("LA2", "na"), ("KR", "kr"), ("JP1", "ap"), ("oce", "ap")] {
            assert_eq!(region_de_jeu(client).as_deref(), Some(jeu), "{client}");
        }
        assert_eq!(region_de_jeu("mars"), None);
        assert_eq!(region_de_jeu(""), None);
    }

    /// Contre le vrai client Riot (ouvert) — ignorée :
    /// `cargo test -p ki-client-gui -- --ignored --nocapture sonde_region`.
    /// Ce que le jeu et le client disent de la région ; rien d'autre.
    #[test]
    #[ignore]
    fn sonde_region() {
        let lf = lire_lockfile().expect("client Riot ouvert");
        let client = Client::new(&lf);
        println!("région du jeu : {:?}", region_du_jeu(&client));
        println!("région du client : {:?}", region_du_client(&client));
        let brut: serde_json::Value = client.get("/riotclient/region-locale").unwrap_or_default();
        println!("region-locale : region={} webRegion={}", brut["region"], brut["webRegion"]);
    }

    #[test]
    fn les_cartes_ont_leur_nom_de_joueur() {
        assert_eq!(nom_de_carte("/Game/Maps/Bonsai/Bonsai"), "Split");
        assert_eq!(nom_de_carte("/Game/Maps/Ascent/Ascent"), "Ascent");
        assert_eq!(nom_de_carte("/Game/Maps/Inconnue/Zeta"), "Zeta");
        assert_eq!(nom_de_carte(""), "");
        // Les trois cartes du TDM qui étaient permutées, et celles de la
        // 13.06 — d'après les `mapUrl` de valorant-api.com.
        assert_eq!(nom_de_carte("/Game/Maps/HURM/HURM_Alley/HURM_Alley"), "District");
        assert_eq!(nom_de_carte("/Game/Maps/HURM/HURM_Bowl/HURM_Bowl"), "Kasbah");
        assert_eq!(nom_de_carte("/Game/Maps/HURM/HURM_Yard/HURM_Yard"), "Piazza");
        assert_eq!(nom_de_carte("/Game/Maps/Plummet/Plummet"), "Summit");
        assert_eq!(nom_de_carte("/Game/Maps/AbilityDraft/AbilityDraft"), "Gauntlet");
        assert_eq!(nom_de_carte("/Game/Maps/PovegliaV2/RangeV2"), "Stand de tir");
        assert_eq!(nom_de_carte("/Game/Maps/Duel/Duel_1/Skirmish_A"), "Escarmouche A");
    }
}
