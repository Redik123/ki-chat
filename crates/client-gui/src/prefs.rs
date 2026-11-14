//! Les réglages de ki-chat, pris comme un tout : leur version, leur copie
//! avant chaque mise à jour, leur export et leur import.
//!
//! Les réglages vivent dans le fichier d'eframe (`app.ron`), une table de
//! clés et de valeurs en texte que chaque partie de l'appli lit et écrit
//! pour son compte. Trois choses manquaient :
//!
//! - **une version**. Rien ne disait de quel ki-chat venait ce fichier, ni
//!   dans quel format : la clé [`CLE_VERSION`] le dit, et [`migrer`] est
//!   l'endroit où une clé change de nom ou de forme ;
//! - **une copie avant mise à jour**. Une version qui lirait mal un réglage
//!   le réécrivait à sa valeur d'origine, sans retour possible :
//!   [`avant_demarrage`] garde le fichier tel qu'il était avant qu'une
//!   nouvelle version n'y touche ;
//! - **un moyen de les emporter**. Changer de PC, c'était tout refaire :
//!   serveurs, raccourcis, égaliseurs, profils de voix, volumes par personne.
//!   [`exporter`] en fait un fichier, [`lire_export`] le relit.
//!
//! Ce qui ne s'exporte pas : les mots de passe (chiffrés pour cette machine,
//! ils n'ouvriraient rien ailleurs — et un fichier qu'on s'envoie n'a pas à
//! les porter), et ce qui décrit cette machine plutôt que la personne
//! (périphériques audio, place de la fenêtre, dossier des clips).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// La version du format des réglages. À monter avec une entrée de plus dans
/// [`migrer`] quand une clé change de nom ou de forme.
pub const VERSION: u32 = 1;
/// La clé qui porte [`VERSION`].
pub const CLE_VERSION: &str = "prefs_version";
/// La clé qui dit quel ki-chat a écrit ces réglages en dernier.
pub const CLE_APPLI: &str = "prefs_appli";
/// Le carnet de serveurs : exporté sans ses mots de passe.
const CLE_CARNET: &str = "servers";

const FORMAT: &str = "ki-chat-reglages";
/// Les copies d'avant mise à jour qu'on garde.
const COPIES_GARDEES: usize = 3;
const PREFIXE_COPIE: &str = "app.avant-";

/// Ce qui décrit cette machine ou cette session, pas la personne : ni
/// exporté, ni importé.
const NON_TRANSPORTES: &[&str] = &[
    // La mémoire d'egui (place des fenêtres internes) et celle de la fenêtre.
    "egui",
    "window",
    "window_maximized",
    "fenetre",
    "fenetre_stream",
    "fenetre_valo",
    "fenetre_tactique",
    "fenetre_cartes",
    // Les périphériques et les dossiers de ce PC.
    "input_device",
    "output_device",
    "micro_jeux_sortie",
    "clips_dossier",
    // L'état d'une installation : serveur rouvert d'office, mise à jour
    // refusée, rapport de plantage déjà envoyé, avertissement déjà vu.
    "session_auto",
    "update_skipped",
    "diag_crash_envoye",
    "zone_avertie",
    CLE_APPLI,
];

/// Les champs d'un serveur du carnet qui voyagent. Tout autre champ reste
/// ici — à commencer par `secret` et l'ancien `password`.
const CHAMPS_DE_SERVEUR: &[&str] = &["id", "name", "address", "username", "last_used", "server_name", "cert_fingerprint", "icon"];

static FICHIER: OnceLock<PathBuf> = OnceLock::new();

/// Dit où eframe range les réglages de cette instance — son dossier
/// habituel, ou le profil à part du ki-chat de test.
pub fn designer(chemin: PathBuf) {
    let _ = FICHIER.set(chemin);
}

/// Le fichier des réglages de cette instance.
pub fn fichier() -> Option<&'static Path> {
    FICHIER.get().map(PathBuf::as_path)
}

/// Lit le fichier d'eframe : une table RON de clés et de valeurs.
pub fn lire(chemin: &Path) -> Result<BTreeMap<String, String>, String> {
    let texte = std::fs::read_to_string(chemin).map_err(|e| e.to_string())?;
    let table: HashMap<String, String> = ron::from_str(&texte).map_err(|e| e.to_string())?;
    Ok(table.into_iter().collect())
}

/// Écrit le fichier d'eframe, sans jamais le laisser à moitié écrit : à côté,
/// puis renommé.
fn ecrire(chemin: &Path, table: &BTreeMap<String, String>) -> Result<(), String> {
    let texte = ron::ser::to_string_pretty(table, ron::ser::PrettyConfig::default()).map_err(|e| e.to_string())?;
    let temporaire = chemin.with_extension("ron.tmp");
    std::fs::write(&temporaire, texte).map_err(|e| e.to_string())?;
    std::fs::rename(&temporaire, chemin).map_err(|e| {
        let _ = std::fs::remove_file(&temporaire);
        e.to_string()
    })
}

/// Fait passer une table de réglages de sa version à [`VERSION`]. Rend vrai
/// si elle a changé.
///
/// Rien à migrer pour l'instant : la version 1 est le format tel qu'il
/// était, la clé de version en plus. La première clé qui changera de nom ou
/// de forme s'ajoute ici, sous `if version < 2 { … }`.
pub fn migrer(table: &mut BTreeMap<String, String>) -> bool {
    let version: u32 = table.get(CLE_VERSION).and_then(|v| v.parse().ok()).unwrap_or(0);
    if version >= VERSION {
        return false;
    }
    table.insert(CLE_VERSION.into(), VERSION.to_string());
    true
}

/// Le nom d'une copie : la version quittée, puis un numéro d'ordre — le
/// tri par nom n'est pas le tri par âge (0.1.9 après 0.1.10), on se fie à la
/// date du fichier.
fn nom_de_copie(quittee: &str) -> String {
    let propre: String =
        quittee.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    format!("{PREFIXE_COPIE}{}.ron", if propre.is_empty() { "inconnue" } else { &propre })
}

/// Les copies d'avant mise à jour du dossier, de la plus récente à la plus
/// ancienne.
pub fn copies(dossier: &Path) -> Vec<PathBuf> {
    let mut liste: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dossier)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let nom = e.file_name().to_string_lossy().into_owned();
            nom.starts_with(PREFIXE_COPIE) && nom.ends_with(".ron")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    liste.sort_by_key(|(quand, _)| std::cmp::Reverse(*quand));
    liste.into_iter().map(|(_, chemin)| chemin).collect()
}

/// À faire avant qu'eframe n'ouvre le fichier : garder une copie des
/// réglages quand ki-chat a changé de version depuis qu'ils ont été écrits,
/// puis les migrer. Rien de tout cela n'empêche de démarrer : un échec se
/// dit au journal, et l'appli s'ouvre.
pub fn avant_demarrage(chemin: &Path, appli: &str) {
    let mut table = match lire(chemin) {
        Ok(t) => t,
        // Pas de fichier : première ouverture, rien à garder.
        Err(_) if !chemin.exists() => return,
        Err(e) => {
            tracing::warn!("réglages illisibles ({e}) : ni copie ni migration");
            return;
        }
    };
    let quittee = table.get(CLE_APPLI).cloned().unwrap_or_default();
    if quittee != appli {
        if let Some(dossier) = chemin.parent() {
            let copie = dossier.join(nom_de_copie(&quittee));
            match std::fs::copy(chemin, &copie) {
                Ok(_) => tracing::info!("réglages d'avant la {appli} gardés dans {}", copie.display()),
                Err(e) => tracing::warn!("copie des réglages impossible : {e}"),
            }
            for vieille in copies(dossier).into_iter().skip(COPIES_GARDEES) {
                let _ = std::fs::remove_file(vieille);
            }
        }
    }
    let lue: u32 = table.get(CLE_VERSION).and_then(|v| v.parse().ok()).unwrap_or(0);
    if lue > VERSION {
        tracing::warn!(
            "ces réglages ont été écrits par un ki-chat plus récent (format {lue}, celui-ci connaît {VERSION}) : \
             ce qu'il ne comprend pas reprendra sa valeur d'origine"
        );
    }
    if migrer(&mut table) {
        match ecrire(chemin, &table) {
            Ok(()) => tracing::info!("réglages passés du format {lue} au format {VERSION}"),
            Err(e) => tracing::warn!("migration des réglages non écrite : {e}"),
        }
    }
}

/// Un stockage qui note les clés qu'on y écrit. Celui d'eframe ne sait pas
/// dire ce qu'il contient : pour exporter, on fait sauver l'appli à travers
/// celui-ci, et l'on sait alors tout ce qu'elle écrit — modules compris,
/// sans liste à tenir à la main.
pub struct Temoin<'a> {
    dessous: &'a mut dyn eframe::Storage,
    ecrites: std::collections::BTreeSet<String>,
}

impl<'a> Temoin<'a> {
    pub fn sur(dessous: &'a mut dyn eframe::Storage) -> Self {
        Self { dessous, ecrites: Default::default() }
    }

    /// Les clés écrites à travers lui.
    pub fn cles(self) -> std::collections::BTreeSet<String> {
        self.ecrites
    }
}

impl eframe::Storage for Temoin<'_> {
    fn get_string(&self, cle: &str) -> Option<String> {
        self.dessous.get_string(cle)
    }

    fn set_string(&mut self, cle: &str, valeur: String) {
        self.ecrites.insert(cle.to_string());
        self.dessous.set_string(cle, valeur);
    }

    fn remove_string(&mut self, cle: &str) {
        self.ecrites.remove(cle);
        self.dessous.remove_string(cle);
    }

    fn flush(&mut self) {
        self.dessous.flush();
    }
}

/// Le carnet de serveurs sans ce qui ne doit pas voyager : chaque serveur
/// réduit à [`CHAMPS_DE_SERVEUR`]. `None` : ce n'était pas un carnet.
fn carnet_sans_secrets(json: &str) -> Option<String> {
    let serveurs: Vec<serde_json::Map<String, serde_json::Value>> = serde_json::from_str(json).ok()?;
    let propres: Vec<serde_json::Map<String, serde_json::Value>> = serveurs
        .into_iter()
        .map(|s| s.into_iter().filter(|(champ, _)| CHAMPS_DE_SERVEUR.contains(&champ.as_str())).collect())
        .collect();
    serde_json::to_string(&propres).ok()
}

/// Ne garde que ce qui voyage, carnet nettoyé. Sert à l'export **et** à
/// l'import : un fichier qu'on reçoit n'est pas cru sur parole.
fn transportables(table: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    table
        .iter()
        .filter(|(cle, _)| !NON_TRANSPORTES.contains(&cle.as_str()))
        .filter_map(|(cle, valeur)| {
            if cle == CLE_CARNET {
                // Un carnet illisible ne part pas tel quel : on ne sait pas
                // ce qu'il contient.
                carnet_sans_secrets(valeur).map(|propre| (cle.clone(), propre))
            } else {
                Some((cle.clone(), valeur.clone()))
            }
        })
        .collect()
}

/// Le fichier d'export : du JSON lisible, avec de quoi savoir d'où il vient.
pub fn exporter(table: &BTreeMap<String, String>, appli: &str) -> String {
    let fichier = serde_json::json!({
        "format": FORMAT,
        "version": VERSION,
        "ki_chat": appli,
        "cles": transportables(table),
    });
    serde_json::to_string_pretty(&fichier).unwrap_or_default()
}

/// Relit un fichier d'export : les réglages qu'il porte, prêts à être posés.
pub fn lire_export(texte: &str) -> Result<BTreeMap<String, String>, String> {
    let fichier: serde_json::Value =
        serde_json::from_str(texte).map_err(|_| "ce fichier n'est pas un export de réglages ki-chat".to_string())?;
    if fichier.get("format").and_then(|f| f.as_str()) != Some(FORMAT) {
        return Err("ce fichier n'est pas un export de réglages ki-chat".into());
    }
    let version = fichier.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
    if version > u64::from(VERSION) {
        let de = fichier.get("ki_chat").and_then(|v| v.as_str()).unwrap_or("une version plus récente");
        return Err(format!("ces réglages viennent de ki-chat {de}, plus récent que celui-ci : mets à jour d'abord"));
    }
    let cles = fichier.get("cles").and_then(|c| c.as_object()).ok_or("cet export ne contient aucun réglage")?;
    let mut table: BTreeMap<String, String> =
        cles.iter().filter_map(|(cle, valeur)| Some((cle.clone(), valeur.as_str()?.to_string()))).collect();
    // Un export d'un format plus ancien passe par les mêmes migrations.
    table.insert(CLE_VERSION.into(), version.to_string());
    migrer(&mut table);
    let table = transportables(&table);
    if table.len() <= 1 {
        return Err("cet export ne contient aucun réglage".into());
    }
    Ok(table)
}

/// Ce qu'un import pose réellement, étant donné ce qui existe déjà ici : le
/// carnet importé ne remplace jamais un carnet qui a des serveurs — il
/// emporterait leurs mots de passe mémorisés.
pub fn a_poser(importes: BTreeMap<String, String>, carnet_vide: bool) -> BTreeMap<String, String> {
    importes.into_iter().filter(|(cle, _)| cle != CLE_CARNET || carnet_vide).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(paires: &[(&str, &str)]) -> BTreeMap<String, String> {
        paires.iter().map(|(c, v)| (c.to_string(), v.to_string())).collect()
    }

    fn banc(nom: &str) -> PathBuf {
        let dossier = std::env::temp_dir().join(format!("ki-prefs-{nom}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dossier);
        std::fs::create_dir_all(&dossier).unwrap();
        dossier
    }

    const CARNET: &str = r#"[{"id":7,"name":"Chez Kevin","address":"exemple.fr:9987","username":"max","secret":"AQAAANCMnd8=","password":"en-clair","last_used":12,"server_name":"BAWS","cert_fingerprint":"a5:f6","icon":null}]"#;

    #[test]
    fn l_export_n_emporte_ni_mot_de_passe_ni_ce_qui_decrit_la_machine() {
        let reglages = table(&[
            ("sfx_volume", "0.4"),
            ("ptt_key", "alt_gauche"),
            ("egaliseur", "100:2.5"),
            ("servers", CARNET),
            ("input_device", "Micro (Sound Blaster G8)"),
            ("window", "(pos: (10, 20))"),
            ("session_auto", "7"),
            ("egui", "…mémoire…"),
        ]);
        let json = exporter(&reglages, "0.1.70");
        // Ni le mot de passe chiffré, ni l'ancien en clair.
        assert!(!json.contains("AQAAANCMnd8") && !json.contains("en-clair") && !json.contains("secret"), "{json}");
        // Ni le micro de ce PC, ni la place de la fenêtre, ni le serveur rouvert d'office.
        assert!(!json.contains("Sound Blaster") && !json.contains("session_auto") && !json.contains("mémoire"), "{json}");
        // Mais bien les réglages, et le serveur avec son empreinte.
        let relu = lire_export(&json).unwrap();
        assert_eq!(relu.get("sfx_volume").map(String::as_str), Some("0.4"));
        assert_eq!(relu.get("ptt_key").map(String::as_str), Some("alt_gauche"));
        let carnet: serde_json::Value = serde_json::from_str(&relu["servers"]).unwrap();
        assert_eq!(carnet[0]["address"], "exemple.fr:9987");
        assert_eq!(carnet[0]["cert_fingerprint"], "a5:f6");
        assert!(carnet[0].get("secret").is_none() && carnet[0].get("password").is_none());
    }

    #[test]
    fn un_export_trafique_ne_fait_pas_entrer_ce_qui_ne_voyage_pas() {
        // Écrit à la main, avec ce que l'export n'aurait jamais mis.
        let trafique = serde_json::json!({
            "format": FORMAT, "version": VERSION, "ki_chat": "0.1.70",
            "cles": { "sfx_volume": "0.9", "session_auto": "3", "servers": CARNET, "output_device": "ailleurs" },
        })
        .to_string();
        let relu = lire_export(&trafique).unwrap();
        assert!(relu.contains_key("sfx_volume"));
        assert!(!relu.contains_key("session_auto") && !relu.contains_key("output_device"));
        // Le carnet importé est nettoyé lui aussi.
        assert!(!relu["servers"].contains("secret") && !relu["servers"].contains("en-clair"));
    }

    #[test]
    fn un_fichier_quelconque_ou_trop_recent_est_refuse_avec_sa_raison() {
        assert!(lire_export("pas du json").unwrap_err().contains("n'est pas un export"));
        assert!(lire_export(r#"{"format":"autre chose","cles":{}}"#).unwrap_err().contains("n'est pas un export"));
        let futur = serde_json::json!({ "format": FORMAT, "version": VERSION + 1, "ki_chat": "0.9.0", "cles": { "a": "b" } });
        let refus = lire_export(&futur.to_string()).unwrap_err();
        assert!(refus.contains("0.9.0") && refus.contains("mets à jour"), "{refus}");
        let vide = serde_json::json!({ "format": FORMAT, "version": VERSION, "cles": {} });
        assert!(lire_export(&vide.to_string()).unwrap_err().contains("aucun réglage"));
    }

    #[test]
    fn le_carnet_importe_ne_remplace_pas_un_carnet_qui_a_des_serveurs() {
        let importes = table(&[("sfx_volume", "0.4"), ("servers", "[]")]);
        // PC neuf : le carnet arrive avec le reste.
        assert!(a_poser(importes.clone(), true).contains_key("servers"));
        // PC qui a déjà ses serveurs et leurs mots de passe : on n'y touche pas.
        let poses = a_poser(importes, false);
        assert!(!poses.contains_key("servers") && poses.contains_key("sfx_volume"));
    }

    /// Un stockage en mémoire, pour les tests.
    #[derive(Default)]
    struct EnMemoire(BTreeMap<String, String>);

    impl eframe::Storage for EnMemoire {
        fn get_string(&self, cle: &str) -> Option<String> {
            self.0.get(cle).cloned()
        }
        fn set_string(&mut self, cle: &str, valeur: String) {
            self.0.insert(cle.to_string(), valeur);
        }
        fn remove_string(&mut self, cle: &str) {
            self.0.remove(cle);
        }
        fn flush(&mut self) {}
    }

    #[test]
    fn le_temoin_note_ce_qui_s_ecrit_et_laisse_passer() {
        use eframe::Storage;
        let mut dessous = EnMemoire::default();
        dessous.set_string("deja_la", "1".into());
        let mut temoin = Temoin::sur(&mut dessous);
        temoin.set_string("sfx_volume", "0.4".into());
        temoin.set_string("regard_volume", "0.5".into());
        temoin.set_string("sfx_volume", "0.6".into());
        // Il lit à travers, comme si de rien n'était.
        assert_eq!(temoin.get_string("deja_la").as_deref(), Some("1"));
        // Ce qui a été écrit, une fois chacun — pas ce qui était déjà là.
        let cles: Vec<String> = temoin.cles().into_iter().collect();
        assert_eq!(cles, ["regard_volume", "sfx_volume"]);
        // Et tout est arrivé dessous, à sa dernière valeur.
        assert_eq!(dessous.get_string("sfx_volume").as_deref(), Some("0.6"));
    }

    #[test]
    fn la_migration_pose_la_version_une_seule_fois() {
        let mut anciens = table(&[("sfx_volume", "0.4")]);
        assert!(migrer(&mut anciens));
        assert_eq!(anciens.get(CLE_VERSION).map(String::as_str), Some("1"));
        assert_eq!(anciens.get("sfx_volume").map(String::as_str), Some("0.4"));
        // Déjà à jour, ou plus récent : on ne touche à rien.
        assert!(!migrer(&mut anciens));
        let mut futurs = table(&[(CLE_VERSION, "9")]);
        assert!(!migrer(&mut futurs));
        assert_eq!(futurs[CLE_VERSION], "9");
    }

    #[test]
    fn le_fichier_d_eframe_se_relit_apres_ecriture() {
        let dossier = banc("aller-retour");
        let chemin = dossier.join("app.ron");
        let reglages = table(&[("sfx_volume", "0.4"), ("servers", CARNET), ("texte", "avec \"guillemets\" et\nligne")]);
        ecrire(&chemin, &reglages).unwrap();
        assert_eq!(lire(&chemin).unwrap(), reglages);
        // Rien ne traîne à côté.
        assert!(!dossier.join("app.ron.tmp").exists());
        std::fs::remove_dir_all(&dossier).unwrap();
    }

    #[test]
    fn une_nouvelle_version_garde_les_reglages_d_avant() {
        let dossier = banc("copie");
        let chemin = dossier.join("app.ron");
        // Première ouverture : pas de fichier, rien à faire, rien de créé.
        avant_demarrage(&chemin, "0.1.70");
        assert!(!chemin.exists() && copies(&dossier).is_empty());

        // Des réglages d'avant que la clé d'appli existe.
        ecrire(&chemin, &table(&[("sfx_volume", "0.4")])).unwrap();
        avant_demarrage(&chemin, "0.1.70");
        let gardees = copies(&dossier);
        assert_eq!(gardees.len(), 1);
        assert!(gardees[0].ends_with("app.avant-inconnue.ron"));
        // La copie est le fichier d'avant, sans la clé de version…
        assert!(!lire(&gardees[0]).unwrap().contains_key(CLE_VERSION));
        // … et le fichier courant est migré, réglages intacts.
        let courant = lire(&chemin).unwrap();
        assert_eq!(courant[CLE_VERSION], "1");
        assert_eq!(courant["sfx_volume"], "0.4");

        // ki-chat sauve en quittant ; même version au lancement suivant : rien de plus.
        let mut sauve = courant.clone();
        sauve.insert(CLE_APPLI.into(), "0.1.70".into());
        ecrire(&chemin, &sauve).unwrap();
        avant_demarrage(&chemin, "0.1.70");
        assert_eq!(copies(&dossier).len(), 1);

        // Mise à jour : la copie porte le nom de la version quittée.
        avant_demarrage(&chemin, "0.2.0");
        assert!(copies(&dossier).iter().any(|c| c.ends_with("app.avant-0.1.70.ron")));
        std::fs::remove_dir_all(&dossier).unwrap();
    }

    #[test]
    fn un_fichier_abime_n_empeche_pas_de_demarrer() {
        let dossier = banc("abime");
        let chemin = dossier.join("app.ron");
        std::fs::write(&chemin, "{ ceci n'est pas du RON").unwrap();
        // Ni panique, ni copie d'un fichier qu'on ne comprend pas, ni réécriture.
        avant_demarrage(&chemin, "0.1.70");
        assert!(copies(&dossier).is_empty());
        assert_eq!(std::fs::read_to_string(&chemin).unwrap(), "{ ceci n'est pas du RON");
        std::fs::remove_dir_all(&dossier).unwrap();
    }

    /// Le défaut que ce module ne peut pas empêcher par un type, il le
    /// surveille : une clé que `KiApp::new` relit doit être écrite quelque
    /// part. Un réglage lu mais jamais sauvé revient à sa valeur d'origine à
    /// chaque lancement, sans rien dire.
    #[test]
    fn ce_qui_se_relit_au_demarrage_est_sauve_quelque_part() {
        fn cles(source: &str, appel: &str) -> std::collections::BTreeSet<String> {
            source
                .split(appel)
                .skip(1)
                .filter_map(|suite| {
                    let suite = suite.trim_start().strip_prefix('"')?;
                    let fin = suite.find('"')?;
                    Some(suite[..fin].to_string())
                })
                .filter(|cle| !cle.is_empty() && cle.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
                .collect()
        }
        let principal = include_str!("main.rs");
        let modules = [
            principal,
            include_str!("audio.rs"),
            include_str!("reglages.rs"),
            include_str!("clips.rs"),
            include_str!("soundboard.rs"),
            include_str!("valo.rs"),
            include_str!("partage.rs"),
            include_str!("overlay.rs"),
            include_str!("servers.rs"),
        ];
        let audio = include_str!("audio.rs");
        let mut lues = cles(principal, "get(");
        lues.extend(cles(audio, "get("));
        lues.extend(cles(include_str!("reglages.rs"), "get("));
        let ecrites: std::collections::BTreeSet<String> = modules.iter().flat_map(|m| cles(m, "set_string(")).collect();
        assert!(lues.len() > 50, "la lecture des réglages a changé de forme : ce test ne voit plus rien");
        // Relues pour reprendre l'état d'une version d'avant, plus jamais écrites.
        let heritees = ["clip"];
        let oubliees: Vec<&String> = lues.iter().filter(|c| !ecrites.contains(*c) && !heritees.contains(&c.as_str())).collect();
        assert!(oubliees.is_empty(), "relues au démarrage mais jamais sauvées : {oubliees:?}");
    }
}
