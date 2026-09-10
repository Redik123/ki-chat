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
struct Lockfile {
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

fn lire_lockfile() -> Option<Lockfile> {
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
struct Client {
    agent: ureq::Agent,
    base: String,
    auth: String,
}

impl Client {
    fn new(lf: &Lockfile) -> Self {
        let agent = ureq::AgentBuilder::new()
            .tls_config(ki_client_quic::pinned_tls_config(None))
            .timeout(Duration::from_secs(3))
            .build();
        let auth = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("riot:{}", lf.password))
        );
        Self { agent, base: format!("https://127.0.0.1:{}", lf.port), auth }
    }

    fn get<T: serde::de::DeserializeOwned>(&self, chemin: &str) -> anyhow::Result<T> {
        let reponse = self
            .agent
            .get(&format!("{}{chemin}", self.base))
            .set("Authorization", &self.auth)
            .call()?;
        Ok(reponse.into_json()?)
    }
}

#[derive(Deserialize)]
struct Session {
    #[serde(default)]
    puuid: String,
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
/// facultatifs : le format n'est documenté par personne d'officiel.
#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Prive {
    session_loop_state: String,
    party_owner_match_map: String,
    match_map: String,
    party_owner_match_score_ally_team: u32,
    party_owner_match_score_enemy_team: u32,
    queue_id: String,
    party_size: u32,
    max_party_size: u32,
    party_accessibility: String,
    competitive_tier: u32,
    account_level: u32,
    provisioning_flow: String,
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
        "Range" => "Stand de tir",
        "HURM_Alley" => "Kasbah",
        "HURM_Bowl" => "Piazza",
        "HURM_Helix" => "Drift",
        "HURM_Yard" => "District",
        "HURM_HighTide" => "Glitch",
        autre => autre,
    };
    connu.to_string()
}

/// De la présence brute au statut qu'on partage. `None` : pas en jeu.
pub(crate) fn normaliser(p: &Prive) -> Option<JeuStatut> {
    let etat = match p.session_loop_state.as_str() {
        "MENUS" => JeuEtat::Menus,
        "PREGAME" => JeuEtat::PreGame,
        "INGAME" => JeuEtat::EnJeu,
        _ => return None,
    };
    let carte = if p.match_map.is_empty() { &p.party_owner_match_map } else { &p.match_map };
    let custom = p.provisioning_flow == "CustomGame" || (p.queue_id.is_empty() && etat != JeuEtat::Menus);
    Some(
        JeuStatut {
            etat,
            file: p.queue_id.clone(),
            carte: if etat == JeuEtat::Menus { String::new() } else { nom_de_carte(carte) },
            score_allie: p.party_owner_match_score_ally_team.min(99) as u8,
            score_adverse: p.party_owner_match_score_enemy_team.min(99) as u8,
            party_taille: p.party_size.min(10) as u8,
            party_max: p.max_party_size.min(10) as u8,
            party_ouverte: p.party_accessibility == "OPEN",
            rang: p.competitive_tier.min(27) as u8,
            niveau: p.account_level.min(9999),
            custom,
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
                Ok(s) if !s.puuid.is_empty() => client = Some((lf, c, s.puuid)),
                _ => {
                    // Le client démarre encore, ou refuse : on réessaie au
                    // prochain tour.
                    client = None;
                    publier(None);
                    continue;
                }
            }
        }
        let Some((_, c, puuid)) = client.as_ref() else { continue };
        let nouveau = match c.get::<Presences>("/chat/v4/presences") {
            Ok(p) => p
                .presences
                .iter()
                .find(|x| x.puuid == *puuid && x.product == "valorant")
                .and_then(|x| x.private.as_deref())
                .and_then(decoder_prive)
                .and_then(|pr| normaliser(&pr)),
            Err(_) => {
                // Le client s'est fermé ou rechargé : on repartira du lockfile.
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
        // Un état inconnu (le format a changé) : pas de statut, pas de bruit.
        let inconnu = Prive { session_loop_state: "AUTRE".into(), ..Default::default() };
        assert!(normaliser(&inconnu).is_none());
        assert!(decoder_prive("pas du base64 !!").is_none());
    }

    #[test]
    fn les_cartes_ont_leur_nom_de_joueur() {
        assert_eq!(nom_de_carte("/Game/Maps/Bonsai/Bonsai"), "Split");
        assert_eq!(nom_de_carte("/Game/Maps/Ascent/Ascent"), "Ascent");
        assert_eq!(nom_de_carte("/Game/Maps/Inconnue/Zeta"), "Zeta");
        assert_eq!(nom_de_carte(""), "");
    }
}
