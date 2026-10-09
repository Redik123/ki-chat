//! Les médailles VALORANT — les « goldstars » de Riot, les Accolades du
//! jeu depuis la 13.06 —, lues dans son **propre** client Riot.
//!
//! Pour soi seulement : `goldstars/v1/players/{puuid}` répondrait aussi
//! pour le puuid d'un autre, on ne le demande jamais (pas de scouting), et
//! la réponse, qui raconte chaque match avec ses dix joueurs, est réduite à
//! la ligne du membre ([`normaliser`]) avant de sortir de ce fichier. Les
//! jetons vivent dans l'accès ([`valorant::Acces`]) le temps d'une lecture.
//!
//! Quand : une vingtaine de secondes après l'arrivée de VALORANT, puis
//! après chaque partie — à 40 s, le temps que Riot compte, et à 150 s par
//! sûreté —, jamais deux fois en trente secondes. Seulement avec « Partager
//! mon activité » (c'est la présence qui dit qu'une partie finit), l'option
//! des médailles, un compte lié, un serveur qui les connaît, et si le
//! client Riot est bien ouvert sur le compte lié : c'est `main.rs` qui en
//! juge.
//!
//! Sondé en vrai le 2026-09-27 (format 131) : le catalogue ne nomme plus
//! les médailles (son `tempT` est un nombre) — on les reconnaît à leur
//! uuid —, et chaque match porte enfin son id, celui de l'historique.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use ki_protocol::{CompteMedaille, Medaille, MedailleGagnee, Medailles, MedaillesDuMatch};
use serde_json::Value;

use crate::{valo_catalogue, valorant};


/// L'« acte » tout à zéro de la réponse : la carrière.
const CARRIERE: &str = "00000000-0000-0000-0000-000000000000";

/// Les lectures prévues après une partie : le temps que Riot compte, puis
/// une seconde par sûreté.
const APRES_PARTIE: [Duration; 2] = [Duration::from_secs(40), Duration::from_secs(150)];
const AU_LANCEMENT: Duration = Duration::from_secs(20);
/// Jamais deux lectures plus près que ça.
const ECART_MIN: Duration = Duration::from_secs(30);

/// Les goldstars par leur uuid : la table est celle du protocole, que le
/// serveur partage pour les accolades de HenrikDev.
fn medaille_de(uuid: &str) -> Option<Medaille> {
    Medaille::depuis_uuid(uuid)
}

/// Une entrée d'objet JSON par sa clé, sans tenir compte de la casse : les
/// uuid de Riot sont en minuscules aujourd'hui, rien ne le garantit.
fn cle<'a>(objet: &'a Value, cle: &str) -> Option<&'a Value> {
    objet.as_object()?.iter().find(|(k, _)| k.eq_ignore_ascii_case(cle)).map(|(_, v)| v)
}

/// Les sommes d'un acte (ou de la carrière) : combien de fois, et le
/// meilleur. Les médailles inconnues de cette version sont laissées.
fn sommes(joueur: &Value, acte: &str) -> Vec<CompteMedaille> {
    let Some(a) = cle(&joueur["tempS"], acte) else { return Vec::new() };
    a["tempA"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(id, g)| {
            Some(CompteMedaille {
                medaille: medaille_de(id)?,
                fois: g["tempC"].as_u64().unwrap_or(0).min(u64::from(u32::MAX)) as u32,
                meilleur: g["tempB"].as_f64().unwrap_or(0.0) as f32,
            })
        })
        .filter(|c| c.fois > 0)
        .collect()
}

/// La réponse de `goldstars/v1/players/{puuid}` réduite au membre : ses
/// sommes de l'acte en cours et de sa carrière, et match par match ce
/// qu'il a gagné — rien des neuf autres. `acte` : l'acte en cours (uuid,
/// nom) d'après le catalogue ; sans lui, un seul acte dans la réponse est
/// réputé le bon, plusieurs ne disent rien.
pub(crate) fn normaliser(joueur: &Value, puuid: &str, acte: Option<(&str, &str)>) -> Medailles {
    let actes: Vec<&str> = joueur["tempS"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, _)| k.as_str())
        .filter(|k| !k.eq_ignore_ascii_case(CARRIERE))
        .collect();
    let (acte_id, acte_nom) = match acte {
        Some((id, nom)) => (Some(id), nom.to_string()),
        None if actes.len() == 1 => (Some(actes[0]), String::new()),
        None => (None, String::new()),
    };
    let matchs = joueur["tempM"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let mien = cle(&m["tempP"], puuid)?;
            let medailles: Vec<MedailleGagnee> = mien["tempA"]
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(id, g)| {
                    Some(MedailleGagnee {
                        medaille: medaille_de(id)?,
                        valeur: g["tempV"].as_f64().unwrap_or(0.0) as f32,
                        record: g["tempB"].as_bool().unwrap_or(false),
                    })
                })
                .collect();
            (!medailles.is_empty()).then(|| MedaillesDuMatch {
                id: m["id"].as_str().and_then(ki_protocol::uuid_valorant).unwrap_or_default(),
                debut: m["tempG"].as_u64().unwrap_or(0),
                medailles,
            })
        })
        .collect();
    Medailles {
        maj: 0,
        acte: acte_nom,
        cet_acte: acte_id.map(|id| sommes(joueur, id)).unwrap_or_default(),
        carriere: sommes(joueur, CARRIERE),
        matchs,
    }
    .nettoyer()
}

/// La réponse brute de Riot. Ne sort pas de ce module.
pub(crate) struct Brut {
    pub(crate) puuid: String,
    pub(crate) riot_id: String,
    /// Le catalogue des médailles : la sonde seulement le demande.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) catalogue: Value,
    pub(crate) joueur: Value,
}

/// Toute la lecture, sur un fil à part : les jetons vivent dans l'accès et
/// disparaissent avec lui.
pub(crate) fn lire_brut(avec_catalogue: bool) -> anyhow::Result<Brut> {
    let acces = valorant::Acces::ouvrir()?;
    let catalogue = if avec_catalogue { acces.get("/goldstars/v1/goldstars").unwrap_or(Value::Null) } else { Value::Null };
    let joueur = acces.get(&format!("/goldstars/v1/players/{}", acces.puuid))?;
    Ok(Brut { puuid: acces.puuid.clone(), riot_id: acces.riot_id.clone(), catalogue, joueur })
}

/// Ce qu'une lecture rapporte : le compte ouvert dans le client Riot
/// (pour le comparer au compte lié) et ses médailles.
pub struct Lecture {
    pub riot_id: String,
    pub medailles: Medailles,
}

fn lire() -> anyhow::Result<Lecture> {
    let brut = lire_brut(false)?;
    // Le catalogue dit l'acte en cours : quelques secondes pour venir,
    // s'il n'est pas encore là — on est sur un fil à part.
    let mut index = valo_catalogue::index();
    for _ in 0..20 {
        if index.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
        index = valo_catalogue::index();
    }
    let maintenant = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let acte = index.as_deref().and_then(|i| i.acte_en_cours(maintenant));
    let medailles = normaliser(&brut.joueur, &brut.puuid, acte);
    Ok(Lecture { riot_id: brut.riot_id, medailles })
}

enum Etat {
    Repos,
    EnCours,
    Fini(Result<Lecture, String>),
}

/// Les lectures prévues, celle en cours, et ce qu'elle a donné.
pub struct Lecteur {
    etat: Arc<Mutex<Etat>>,
    prevues: Vec<Instant>,
    derniere: Option<Instant>,
}

impl Default for Lecteur {
    fn default() -> Self {
        Self::new()
    }
}

impl Lecteur {
    pub fn new() -> Self {
        Self { etat: Arc::new(Mutex::new(Etat::Repos)), prevues: Vec::new(), derniere: None }
    }

    /// VALORANT vient d'arriver : une lecture dans vingt secondes.
    pub fn au_lancement(&mut self) {
        self.prevoir(AU_LANCEMENT);
    }

    /// Une partie vient de finir : deux lectures.
    pub fn apres_partie(&mut self) {
        for delai in APRES_PARTIE {
            self.prevoir(delai);
        }
    }

    /// Plus rien de prévu : l'option est coupée, ou la connexion perdue.
    pub fn oublier(&mut self) {
        self.prevues.clear();
    }

    fn prevoir(&mut self, delai: Duration) {
        self.prevues.push(Instant::now() + delai);
        self.prevues.sort();
        // Une partie enchaînée derrière une autre ne cumule pas sans fin.
        self.prevues.truncate(4);
    }

    /// À chaque tour : rend la lecture qui vient de finir, ou lance celle
    /// qui est due. Rien ne bloque.
    pub fn tick(&mut self, ctx: &egui::Context) -> Option<Result<Lecture, String>> {
        {
            let mut etat = self.etat.lock().unwrap_or_else(|e| e.into_inner());
            match std::mem::replace(&mut *etat, Etat::Repos) {
                Etat::Fini(resultat) => return Some(resultat),
                Etat::EnCours => {
                    *etat = Etat::EnCours;
                    return None;
                }
                Etat::Repos => {}
            }
        }
        let maintenant = Instant::now();
        // Une lecture due attend son tour si la précédente est trop proche.
        if !self.prevues.iter().any(|t| *t <= maintenant) || self.derniere.is_some_and(|d| d.elapsed() < ECART_MIN) {
            return None;
        }
        self.prevues.retain(|t| *t > maintenant);
        self.derniere = Some(maintenant);
        *self.etat.lock().unwrap_or_else(|e| e.into_inner()) = Etat::EnCours;
        let (etat, ctx) = (Arc::clone(&self.etat), ctx.clone());
        let lance = std::thread::Builder::new().name("ki-medailles".into()).spawn(move || {
            let resultat = lire().map_err(|e| format!("{e:#}"));
            *etat.lock().unwrap_or_else(|e| e.into_inner()) = Etat::Fini(resultat);
            ctx.request_repaint();
        });
        if let Err(e) = lance {
            *self.etat.lock().unwrap_or_else(|e| e.into_inner()) = Etat::Fini(Err(format!("fil non lancé : {e}")));
        }
        None
    }
}

/// Le même Riot ID, à la casse et aux espaces près.
pub fn meme_riot_id(a: &str, b: &str) -> bool {
    let norme = |s: &str| s.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect::<String>();
    !a.trim().is_empty() && norme(a) == norme(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOI: &str = "11111111-2222-3333-4444-555555555555";
    const AUTRE: &str = "99999999-8888-7777-6666-555555555555";
    const ACTE: &str = "8102cd81-43a0-d0d7-bd59-47b8fe9bed1b";

    /// Une réponse de la forme relevée le 2026-09-27 (format 131), avec un
    /// autre joueur et une médaille qu'on ne connaît pas.
    fn reponse() -> Value {
        serde_json::json!({
            "puuid": MOI,
            "version": 131,
            "tempS": {
                CARRIERE: {"tempS": CARRIERE, "tempA": {
                    "745b27f0-4bc2-13a4-4ee7-77be323155c2": {"id": "745b27f0-4bc2-13a4-4ee7-77be323155c2", "tempC": 2, "tempB": 482.81036},
                    "bfe96c47-44d0-e473-585d-749146d2d05e": {"id": "bfe96c47-44d0-e473-585d-749146d2d05e", "tempC": 5, "tempB": 6},
                    "ffffffff-0000-0000-0000-000000000000": {"id": "ffffffff-0000-0000-0000-000000000000", "tempC": 9, "tempB": 1}
                }},
                ACTE: {"tempS": ACTE, "tempA": {
                    "745b27f0-4bc2-13a4-4ee7-77be323155c2": {"id": "745b27f0-4bc2-13a4-4ee7-77be323155c2", "tempC": 1, "tempB": 349.5}
                }}
            },
            "tempM": [
                {"id": "7865C7EA-77D7-4EE5-A06C-916D5DC2BF1B", "tempG": 1_790_200_000_000u64, "tempP": {
                    MOI: {"tempA": {
                        "745b27f0-4bc2-13a4-4ee7-77be323155c2": {"id": "745b27f0-4bc2-13a4-4ee7-77be323155c2", "tempV": 349.49786, "tempB": true},
                        "6dc31cfd-41da-895f-9246-a8b63558cdb8": {"id": "6dc31cfd-41da-895f-9246-a8b63558cdb8", "tempV": 185.22728, "tempB": false}
                    }},
                    AUTRE: {"tempA": {
                        "1c926cba-48cb-8aeb-c68d-a1ba2d012784": {"id": "1c926cba-48cb-8aeb-c68d-a1ba2d012784", "tempV": 30, "tempB": true}
                    }}
                }},
                {"id": "ab19801b-115d-4f88-a429-54ff660e7eeb", "tempG": 1_790_100_000_000u64, "tempP": {
                    MOI: {"tempA": {}},
                    AUTRE: {"tempA": {"bfe96c47-44d0-e473-585d-749146d2d05e": {"tempV": 3, "tempB": true}}}
                }}
            ]
        })
    }

    /// La réponse se réduit au membre : ses sommes, ses matchs médaillés ;
    /// rien de l'autre joueur, rien de la médaille inconnue, et l'id du
    /// match en minuscules, comme l'historique.
    #[test]
    fn la_reponse_se_reduit_au_membre() {
        let m = normaliser(&reponse(), MOI, Some((ACTE, "V26 · ACTE V")));
        assert_eq!(m.acte, "V26 · ACTE V");
        assert_eq!(m.fois(Medaille::Mvp, false), 1);
        assert_eq!(m.fois(Medaille::Mvp, true), 2);
        assert_eq!(m.fois(Medaille::PremiersSangs, true), 5);
        assert_eq!(m.carriere.len(), 2, "la médaille inconnue est laissée");
        assert_eq!(m.matchs.len(), 1, "un match sans médaille pour moi ne compte pas");
        let x = &m.matchs[0];
        assert_eq!(x.id, "7865c7ea-77d7-4ee5-a06c-916d5dc2bf1b");
        assert_eq!(x.medailles.iter().map(|g| g.medaille).collect::<Vec<_>>(), [Medaille::Mvp, Medaille::Degats]);
        assert!(x.medailles[0].record && !x.medailles[1].record);
        let json = serde_json::to_string(&m).unwrap();
        for interdit in [AUTRE, "top_frag", MOI] {
            assert!(!json.contains(interdit), "{interdit} dans {json}");
        }
    }

    /// Sans catalogue, un seul acte dans la réponse est le bon ; deux ne
    /// disent rien, et la carrière reste.
    #[test]
    fn sans_catalogue_l_acte_se_devine_s_il_est_seul() {
        let m = normaliser(&reponse(), MOI, None);
        assert_eq!((m.acte.as_str(), m.fois(Medaille::Mvp, false)), ("", 1));
        let mut deux = reponse();
        deux["tempS"]["d816f426-48ea-f052-117f-9697a155b319"] = serde_json::json!({"tempA": {}});
        let m = normaliser(&deux, MOI, None);
        assert!(m.cet_acte.is_empty());
        assert_eq!(m.fois(Medaille::Mvp, true), 2);
        // Une réponse vide ou inattendue ne panique pas.
        assert_eq!(normaliser(&Value::Null, MOI, None), Medailles::default());
    }

    #[test]
    fn le_riot_id_se_compare_sans_la_casse() {
        assert!(meme_riot_id("Redik#6162", "redik # 6162"));
        assert!(!meme_riot_id("Redik#6162", "Redik#6163"));
        assert!(!meme_riot_id("", ""));
    }

    /// Les lectures prévues se rangent, ne s'accumulent pas, et le lecteur
    /// ne lance rien avant l'heure.
    #[test]
    fn les_lectures_se_prevoient() {
        let ctx = egui::Context::default();
        let mut l = Lecteur::new();
        assert!(l.tick(&ctx).is_none(), "rien de prévu");
        for _ in 0..5 {
            l.apres_partie();
        }
        assert_eq!(l.prevues.len(), 4);
        assert!(l.tick(&ctx).is_none(), "rien d'échu");
        assert!(matches!(*l.etat.lock().unwrap(), Etat::Repos));
        l.oublier();
        assert!(l.prevues.is_empty());
    }

    /// La forme d'une valeur JSON sans ses données : les clés et les
    /// types, jusqu'à `profondeur`.
    fn forme(v: &Value, profondeur: usize) -> String {
        match v {
            Value::Object(m) if profondeur > 0 => {
                let cles: Vec<String> = m.iter().take(6).map(|(k, v)| format!("{k}: {}", forme(v, profondeur - 1))).collect();
                format!("{{{}{}}}", cles.join(", "), if m.len() > 6 { format!(", … ({} clés)", m.len()) } else { String::new() })
            }
            Value::Object(m) => format!("{{{} clés}}", m.len()),
            Value::Array(a) => match a.first() {
                Some(premier) if profondeur > 0 => format!("[{} × {}]", a.len(), forme(premier, profondeur - 1)),
                _ => format!("[{}]", a.len()),
            },
            Value::String(_) => "texte".into(),
            Value::Number(_) => "nombre".into(),
            Value::Bool(_) => "booléen".into(),
            Value::Null => "null".into(),
        }
    }

    /// La sonde, contre le vrai client Riot (VALORANT ouvert) — ignorée :
    /// `cargo test -p ki-client-gui -- --ignored --nocapture sonde_medailles`.
    /// N'imprime ni jeton, ni puuid, ni rien des autres joueurs : la forme
    /// de la réponse, le catalogue (des données du jeu), la ligne du joueur,
    /// et ce que ki-chat en garde.
    #[test]
    #[ignore]
    fn sonde_medailles() {
        let brut = lire_brut(true).expect("lecture");
        println!("compte : {}", brut.riot_id);
        println!("catalogue — forme : {}", forme(&brut.catalogue, 3));
        let j = &brut.joueur;
        println!("joueur — version : {}", j["version"]);
        for (k, v) in j.as_object().into_iter().flatten().filter(|(k, _)| *k != "puuid") {
            println!("  {k} : {}", forme(v, 2));
        }
        let gardees = normaliser(j, &brut.puuid, None);
        println!("gardé : {}", serde_json::to_string_pretty(&gardees).unwrap());
    }
}
