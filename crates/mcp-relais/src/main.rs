//! Le relais MCP de test de ki-chat, que Claude Code lance à chaque
//! conversation.
//!
//! Claude Code ne liste que les outils des serveurs qui répondent au début
//! d'une conversation, et ki-chat est le plus souvent fermé à ce moment-là.
//! Le relais, lui, répond toujours : il annonce les outils, passe chaque
//! appel au ki-chat ouvert avec `--mcp` par son tube nommé, et sait l'ouvrir
//! (`lancer_ki_chat`).
//!
//! ```text
//! ki-chat-relais [--ki-chat <chemin de ki-chat.exe>]
//! ```
//!
//! Sans `--ki-chat`, c'est le `ki-chat.exe` posé à côté du relais — celui de
//! `target/debug`, le seul qui porte le serveur de test.
//!
//! Côté Claude : du MCP sur l'entrée et la sortie standard, un message JSON
//! par ligne. Côté ki-chat : une demande par ligne (`{"id", "outil",
//! "arguments"}`), une réponse par ligne (`{"id", "ok", "texte", "image"}`).

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Le tube que tient ki-chat (voir `client-gui/src/mcp.rs`).
const TUBE: &str = r"\\.\pipe\ki-chat-mcp";
const LANCER: &str = "lancer_ki_chat";
/// La version de MCP parlée quand le client n'en propose pas.
const VERSION_MCP: &str = "2025-06-18";
/// Le temps laissé à ki-chat pour s'ouvrir : une version de debug met un
/// moment à démarrer.
const DELAI_LANCEMENT: Duration = Duration::from_secs(90);
const FERME: &str = "ki-chat n'est pas ouvert avec --mcp : lancer_ki_chat l'ouvre, puis réessayer. \
    (Seule une version de debug porte le serveur de test.)";

/// Ce que Claude doit savoir avant de s'en servir.
const MODE_D_EMPLOI: &str = "ki-chat est un client de discussion vocale et écrite. Ces outils pilotent \
    une fenêtre de TEST (version de debug lancée avec --mcp), pas le ki-chat de l'utilisateur. \
    Pour voir : capture (une image, un pixel par point) et etat (du JSON). Pour agir : cliquer aux \
    coordonnées lues sur la capture, taper dans le champ qui a le focus, touche pour Enter, Escape, etc. \
    Chaque action laisse quelques images à l'interface avant la suivante : capture ou etat juste après \
    montrent son effet. Ne jamais rejoindre un salon vocal ni jouer de son sans que l'utilisateur le \
    demande : le micro et le casque sont les siens.";

fn outils() -> Value {
    let point = |quoi: &str| {
        json!({
            "type": "object",
            "properties": {
                "x": { "type": "number", "description": "Abscisse en points, lue sur la capture" },
                "y": { "type": "number", "description": "Ordonnée en points, lue sur la capture" },
            },
            "required": ["x", "y"],
            "description": quoi,
        })
    };
    let vide = json!({ "type": "object", "properties": {} });
    let mut cliquer = point("Où cliquer");
    cliquer["properties"]["bouton"] =
        json!({ "type": "string", "enum": ["gauche", "droit", "milieu"], "description": "Par défaut : gauche" });
    cliquer["properties"]["double"] = json!({ "type": "boolean", "description": "Double clic" });
    cliquer["properties"]["ctrl"] = json!({ "type": "boolean" });
    cliquer["properties"]["maj"] = json!({ "type": "boolean" });
    let mut defiler = point("Où poser la souris avant de faire tourner la molette");
    defiler["properties"]["dy"] = json!({
        "type": "number",
        "description": "Points de défilement : négatif descend dans la page, positif remonte. Par défaut -120"
    });
    json!([
        {
            "name": "etat",
            "description": "L'état de ki-chat en JSON : connexion, salon courant, salons et non-lus, derniers messages, \
                contenu du champ de saisie (texte, réponse ou modification en cours), vocal, fenêtres ouvertes, \
                bandeaux d'erreur, taille de la fenêtre et rythme d'affichage.",
            "inputSchema": vide,
        },
        {
            "name": "capture",
            "description": "Une image de la fenêtre de ki-chat telle qu'il la dessine, à un pixel par point : les \
                coordonnées qu'on y lit sont celles de cliquer, survoler et defiler.",
            "inputSchema": vide,
        },
        {
            "name": "cliquer",
            "description": "Clique dans la fenêtre, comme une souris : tout ce qui se clique se teste.",
            "inputSchema": cliquer,
        },
        {
            "name": "survoler",
            "description": "Amène la souris à un endroit sans cliquer, pour faire apparaître une bulle ou un survol.",
            "inputSchema": point("Où amener la souris"),
        },
        {
            "name": "defiler",
            "description": "Fait tourner la molette au-dessus d'un endroit de la fenêtre.",
            "inputSchema": defiler,
        },
        {
            "name": "taper",
            "description": "Tape du texte dans le champ qui a le focus (cliquer dedans d'abord). N'envoie rien : \
                c'est touche Enter qui valide.",
            "inputSchema": {
                "type": "object",
                "properties": { "texte": { "type": "string" } },
                "required": ["texte"],
            },
        },
        {
            "name": "touche",
            "description": "Appuie sur une touche : Enter, Escape, Tab, Backspace, Delete, ArrowUp, ArrowDown, \
                ArrowLeft, ArrowRight, Home, End, PageUp, PageDown, F1 à F12, ou une lettre (A à Z) avec ctrl, maj ou alt.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "touche": { "type": "string" },
                    "ctrl": { "type": "boolean" },
                    "maj": { "type": "boolean" },
                    "alt": { "type": "boolean" },
                },
                "required": ["touche"],
            },
        },
        {
            "name": "aller_salon",
            "description": "Ouvre un salon textuel, par son nom ou son numéro (voir etat).",
            "inputSchema": {
                "type": "object",
                "properties": { "salon": { "description": "Nom ou numéro du salon", "type": ["string", "integer"] } },
                "required": ["salon"],
            },
        },
        {
            "name": "journal",
            "description": "Les dernières lignes du journal de ce ki-chat (ki-chat.log), avec son chemin.",
            "inputSchema": {
                "type": "object",
                "properties": { "lignes": { "type": "integer", "description": "Par défaut 60, au plus 500" } },
            },
        },
        {
            "name": "fermer_ki_chat",
            "description": "Ferme le ki-chat de test, proprement. À faire en fin d'essai.",
            "inputSchema": vide,
        },
        {
            "name": LANCER,
            "description": "Ouvre le ki-chat de test (version de debug, --mcp, à côté de celui de l'utilisateur) et \
                attend qu'il réponde. Une fenêtre s'ouvre sur l'écran de l'utilisateur : le prévenir.",
            "inputSchema": vide,
        },
    ])
}

/// Pourquoi ki-chat n'a pas répondu.
enum Echec {
    /// Personne ne tient le tube : ki-chat est fermé, ou lancé sans `--mcp`.
    Ferme,
    Autre(String),
}

/// Passe un appel d'outil à ki-chat et rend sa réponse.
fn appeler(outil: &str, arguments: &Value) -> Result<Value, Echec> {
    // Le tube ne sert qu'un client à la fois : occupé (231), on patiente.
    let debut = Instant::now();
    let tube = loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(TUBE) {
            Ok(t) => break t,
            Err(e) if e.raw_os_error() == Some(231) && debut.elapsed() < Duration::from_secs(10) => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Echec::Ferme),
            Err(e) => return Err(Echec::Autre(format!("le tube de ki-chat ne s'ouvre pas : {e}"))),
        }
    };
    let mut ligne = json!({ "id": 1, "outil": outil, "arguments": arguments }).to_string();
    ligne.push('\n');
    (&tube).write_all(ligne.as_bytes()).map_err(|e| Echec::Autre(format!("écriture vers ki-chat : {e}")))?;
    let mut reponse = String::new();
    BufReader::new(&tube).read_line(&mut reponse).map_err(|e| Echec::Autre(format!("lecture depuis ki-chat : {e}")))?;
    if reponse.trim().is_empty() {
        return Err(Echec::Autre("ki-chat a refermé le tube sans répondre".into()));
    }
    serde_json::from_str(&reponse).map_err(|e| Echec::Autre(format!("réponse de ki-chat illisible : {e}")))
}

fn texte(t: impl Into<String>, erreur: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": t.into() }], "isError": erreur })
}

/// La réponse de ki-chat, mise en forme pour MCP : l'image d'abord quand il
/// y en a une.
fn en_resultat(reponse: &Value) -> Value {
    let ok = reponse.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let t = reponse.get("texte").and_then(Value::as_str).unwrap_or_default();
    let mut contenu = Vec::new();
    if let Some(image) = reponse.get("image").and_then(Value::as_str) {
        contenu.push(json!({ "type": "image", "data": image, "mimeType": "image/png" }));
    }
    contenu.push(json!({ "type": "text", "text": t }));
    json!({ "content": contenu, "isError": !ok })
}

/// Le ki-chat à ouvrir : celui donné par `--ki-chat`, sinon celui posé à
/// côté du relais.
fn chemin_de_ki_chat(args: &[String]) -> Option<PathBuf> {
    if let Some(i) = args.iter().position(|a| a == "--ki-chat") {
        return args.get(i + 1).map(PathBuf::from);
    }
    let voisin = std::env::current_exe().ok()?.parent()?.join(if cfg!(windows) { "ki-chat.exe" } else { "ki-chat" });
    voisin.is_file().then_some(voisin)
}

fn lancer(args: &[String]) -> Value {
    if appeler("etat", &Value::Null).is_ok() {
        return texte("ki-chat de test est déjà ouvert.", false);
    }
    let Some(exe) = chemin_de_ki_chat(args) else {
        return texte(
            "ki-chat.exe introuvable à côté du relais : compiler d'abord (cargo build -p ki-client-gui), \
             ou donner --ki-chat <chemin> au relais.",
            true,
        );
    };
    let mut commande = std::process::Command::new(&exe);
    commande
        .args(["--mcp", "--nouvelle-instance"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Un programme à part : il continue quand Claude Code arrête le relais.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        commande.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    let lance = commande.spawn();
    if let Err(e) = lance {
        return texte(format!("{} ne se lance pas : {e}", exe.display()), true);
    }
    let debut = Instant::now();
    while debut.elapsed() < DELAI_LANCEMENT {
        std::thread::sleep(Duration::from_millis(500));
        if appeler("etat", &Value::Null).is_ok() {
            return texte(format!("ki-chat de test ouvert ({}).", exe.display()), false);
        }
    }
    texte(
        format!(
            "{} est lancé mais ne répond pas sur son tube : est-ce bien une version de debug ?",
            exe.display()
        ),
        true,
    )
}

/// Répond à une requête MCP. `None` : rien à répondre (une notification).
fn repondre(message: &Value, args: &[String]) -> Option<Value> {
    let id = message.get("id")?.clone();
    let methode = message.get("method").and_then(Value::as_str)?;
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let resultat = match methode {
        "initialize" => Ok(json!({
            "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or(VERSION_MCP),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "ki-chat", "version": env!("CARGO_PKG_VERSION") },
            "instructions": MODE_D_EMPLOI,
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": outils() })),
        "tools/call" => {
            let nom = params.get("name").and_then(Value::as_str).unwrap_or_default();
            let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            Ok(if nom == LANCER {
                lancer(args)
            } else if !outils().as_array().is_some_and(|o| o.iter().any(|t| t["name"] == nom)) {
                texte(format!("outil inconnu : {nom}"), true)
            } else {
                match appeler(nom, &arguments) {
                    Ok(reponse) => en_resultat(&reponse),
                    Err(Echec::Ferme) => texte(FERME, true),
                    Err(Echec::Autre(e)) => texte(e, true),
                }
            })
        }
        autre => Err(json!({ "code": -32601, "message": format!("méthode inconnue : {autre}") })),
    };
    Some(match resultat {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
    })
}

/// Garde pour le relais ses tubes vers Claude Code. Windows donne aux
/// programmes qu'on lance toutes les poignées héritables : sans ça, le
/// ki-chat ouvert par `lancer_ki_chat` tiendrait la sortie du relais ouverte,
/// et Claude Code attendrait la fin d'une réponse jusqu'à sa fermeture.
#[cfg(windows)]
fn garder_les_tubes() {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(laquelle: u32) -> *mut c_void;
        fn SetHandleInformation(poignee: *mut c_void, masque: u32, drapeaux: u32) -> i32;
    }
    const ENTREE: u32 = 0xFFFF_FFF6;
    const SORTIE: u32 = 0xFFFF_FFF5;
    const ERREURS: u32 = 0xFFFF_FFF4;
    const HERITABLE: u32 = 1;

    for laquelle in [ENTREE, SORTIE, ERREURS] {
        // SAFETY : deux appels simples de kernel32 ; une poignée invalide
        // est seulement refusée.
        unsafe {
            let poignee = GetStdHandle(laquelle);
            if !poignee.is_null() {
                SetHandleInformation(poignee, HERITABLE, 0);
            }
        }
    }
}

fn main() {
    #[cfg(windows)]
    garder_les_tubes();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let entree = std::io::stdin();
    let mut sortie = std::io::stdout();
    for ligne in entree.lock().lines() {
        let Ok(ligne) = ligne else { break };
        if ligne.trim().is_empty() {
            continue;
        }
        let reponse = match serde_json::from_str::<Value>(&ligne) {
            Ok(message) => repondre(&message, &args),
            Err(_) => Some(json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32700, "message": "JSON illisible" },
            })),
        };
        if let Some(r) = reponse {
            if writeln!(sortie, "{r}").is_err() || sortie.flush().is_err() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_outils_ont_un_nom_une_description_et_un_schema() {
        let o = outils();
        let liste = o.as_array().unwrap();
        assert!(liste.len() >= 9);
        for outil in liste {
            assert!(outil["name"].as_str().is_some_and(|n| !n.is_empty()), "{outil}");
            assert!(outil["description"].as_str().is_some_and(|d| d.len() > 20), "{outil}");
            assert_eq!(outil["inputSchema"]["type"], "object", "{outil}");
        }
        // Les outils à coordonnées les exigent.
        let cliquer = liste.iter().find(|t| t["name"] == "cliquer").unwrap();
        assert_eq!(cliquer["inputSchema"]["required"], json!(["x", "y"]));
        assert!(cliquer["inputSchema"]["properties"]["bouton"].is_object());
    }

    #[test]
    fn la_poignee_de_main_annonce_les_outils() {
        let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-03-26" } });
        let r = repondre(&init, &[]).unwrap();
        // On parle la version que le client propose.
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert!(r["result"]["capabilities"]["tools"].is_object());
        // Une notification n'attend pas de réponse.
        assert!(repondre(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }), &[]).is_none());
        let liste = repondre(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }), &[]).unwrap();
        assert!(liste["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == LANCER));
        // Une méthode inconnue est une erreur JSON-RPC, pas un silence.
        let inconnue = repondre(&json!({ "jsonrpc": "2.0", "id": 3, "method": "resources/list" }), &[]).unwrap();
        assert_eq!(inconnue["error"]["code"], -32601);
    }

    #[test]
    fn une_capture_rend_l_image_puis_le_texte() {
        let r = en_resultat(&json!({ "id": 1, "ok": true, "texte": "Capture de 1120×720", "image": "QUJD" }));
        assert_eq!(r["isError"], false);
        assert_eq!(r["content"][0], json!({ "type": "image", "data": "QUJD", "mimeType": "image/png" }));
        assert_eq!(r["content"][1]["text"], "Capture de 1120×720");
        // Un refus de ki-chat est une erreur d'outil, avec sa raison.
        let refus = en_resultat(&json!({ "id": 1, "ok": false, "texte": "salon introuvable", "image": null }));
        assert_eq!(refus["isError"], true);
        assert_eq!(refus["content"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn un_outil_inconnu_ne_part_pas_vers_ki_chat() {
        let appel = json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "effacer_tout" } });
        let r = repondre(&appel, &[]).unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("outil inconnu"));
    }

    #[test]
    fn le_chemin_de_ki_chat_se_donne_en_argument() {
        let args = vec!["--ki-chat".to_string(), "D:/x/ki-chat.exe".to_string()];
        assert_eq!(chemin_de_ki_chat(&args), Some(PathBuf::from("D:/x/ki-chat.exe")));
    }
}
