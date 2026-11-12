//! L'envoi de fichiers au serveur : par morceaux, repris après une coupure, puis
//! le lien posté dans le salon d'où il est parti.

use ki_protocol::{ChannelId, ClientMsg};

use crate::KiApp;
use crate::saisie::CADENCE_CHAT;

/// Une connexion perdue que l'on tente de reprendre toute seule.
///
/// Sans ça, le moindre hoquet — le serveur qui redémarre, la box qui
/// renumérote, un jeu qui sature le lien montant — renvoyait tout le monde à
/// l'écran de connexion, en pleine partie, à retrouver son salon vocal à la
/// main. Trente fois, chaque fois.
/// Le lien d'un fichier dont l'envoi est fini, en attente d'être posté :
/// le serveur qui l'a reçu, le salon d'où il est parti, l'adresse.
pub(crate) struct LienPret {
    pub(crate) base: String,
    pub(crate) salon: Option<ChannelId>,
    pub(crate) url: String,
}

/// Ce que donne l'envoi d'un fichier par morceaux.
pub(crate) enum EnvoiMorceaux {
    /// Tous les morceaux sont sur le serveur, à assembler.
    Envoye { upload: String, parts: u32 },
    /// Un serveur d'avant les morceaux (404 au premier) : à l'appelant de
    /// voir — un bloc, ou rien.
    ServeurAncien,
}

/// Remplit `tampon` autant que possible (un `read` peut rendre moins que
/// demandé sans que le fichier soit fini). Rend le nombre d'octets lus.
/// Ce qu'un 401 veut dire pendant un envoi : le jeton voix de la session
/// n'est plus celui du serveur — le membre s'est reconnecté (nouveau jeton
/// au `Welcome`) ou le serveur a redémarré. Le texte brut du serveur,
/// « jeton invalide », ne dirait pas quoi faire.
pub(crate) const SESSION_PERDUE: &str =
    "la session a été perdue pendant l'envoi — reconnecte-toi, puis réessaie";

/// Envoie un fichier par morceaux de 8 Mo (`/upload/partiel`) : chacun
/// reste sous la limite du routeur, et `progres` reçoit le pourcentage, le
/// nombre de morceaux envoyés et le total ; le serveur assemblera. Le jeton
/// est **relu à chaque morceau** (`jeton`) : un envoi de 300 Mo dure des
/// minutes sur une montée domestique, et une reconnexion pendant ce temps
/// en tire un nouveau — les morceaux suivants partent avec. Un serveur
/// d'avant les morceaux répond 404 au premier : c'est
/// [`EnvoiMorceaux::ServeurAncien`]. Un refus (401, 413, 507) remonte avec le
/// texte du serveur — ou, pour le 401, [`SESSION_PERDUE`] — et se note au
/// journal.
pub(crate) fn envoyer_morceaux(
    agent: &ureq::Agent,
    base: &str,
    jeton: &dyn Fn() -> String,
    path: &std::path::Path,
    taille: u64,
    progres: &dyn Fn(u64, u32, u32),
) -> Result<EnvoiMorceaux, String> {
    const MORCEAU: usize = 8 * 1024 * 1024;
    let mut fichier = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let upload = format!("{:016x}", empreinte_upload(path));
    let mut tampon = vec![0u8; MORCEAU];
    let (mut index, mut envoye) = (0u32, 0u64);
    let total = taille.div_ceil(MORCEAU as u64).max(1) as u32;
    loop {
        let n = lire_plein(&mut fichier, &mut tampon).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        let envoi = agent
            .post(&format!("{base}/upload/partiel?upload={upload}&index={index}"))
            .set("x-ki-token", &jeton())
            .timeout(std::time::Duration::from_secs(300))
            .send_bytes(&tampon[..n]);
        if index == 0 && matches!(&envoi, Err(ureq::Error::Status(404, _))) {
            return Ok(EnvoiMorceaux::ServeurAncien);
        }
        if let Err(e) = envoi {
            let code = match &e {
                ureq::Error::Status(code, _) => code.to_string(),
                _ => "réseau".into(),
            };
            let texte = match e {
                ureq::Error::Status(401, _) => SESSION_PERDUE.to_string(),
                autre => erreur_http(autre),
            };
            ki_voice::journal(format!(
                "envoi par morceaux : morceau {}/{total} refusé ({code}) : {texte}",
                index + 1
            ));
            return Err(texte);
        }
        index += 1;
        envoye += n as u64;
        progres(envoye * 100 / taille.max(1), index, total);
        if n < MORCEAU {
            break;
        }
    }
    Ok(EnvoiMorceaux::Envoye { upload, parts: index })
}

pub(crate) fn lire_plein(fichier: &mut std::fs::File, tampon: &mut [u8]) -> std::io::Result<usize> {
    use std::io::Read as _;
    let mut total = 0;
    while total < tampon.len() {
        let n = fichier.read(&mut tampon[total..])?;
        if n == 0 {
            break;
        }
        total += n;
    }
    Ok(total)
}

/// Un identifiant de téléversement : le chemin et l'instant, hachés. Il ne
/// sert qu'à retrouver ses morceaux, dans le dossier du serveur qui est le
/// nôtre — pas besoin d'aléa cryptographique.
pub(crate) fn empreinte_upload(path: &std::path::Path) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    for b in path.to_string_lossy().bytes().chain(nanos.to_le_bytes()) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Le message d'une erreur HTTP : ce que le serveur a répondu, s'il a dit
/// quelque chose (« fichier trop gros », « espace saturé »), sinon l'erreur.
pub(crate) fn erreur_http(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, reponse) => {
            let texte = reponse.into_string().unwrap_or_default();
            let texte = texte.trim();
            if texte.is_empty() {
                format!("le serveur répond {code}")
            } else {
                texte.chars().take(200).collect()
            }
        }
        autre => autre.to_string(),
    }
}

impl KiApp {
    /// Sélection d'un fichier puis upload, dans un thread (le dialogue
    /// natif et l'envoi ne doivent pas bloquer l'UI).
    /// Poste les liens des envois finis, un à la fois au rythme du chat, par
    /// la connexion en cours. Le fil d'envoi les postait lui-même, par la
    /// connexion de son départ : après une reprise, dans une connexion
    /// morte — perdus, le bandeau s'effaçant comme si tout allait bien.
    pub(crate) fn envoyer_liens_prets(&mut self) {
        if !self.session.welcomed || self.dernier_envoi.is_some_and(|t| t.elapsed() < CADENCE_CHAT) {
            return;
        }
        let pret = {
            let mut prets = self.liens_prets.lock().unwrap();
            if prets.is_empty() {
                return;
            }
            prets.remove(0)
        };
        if pret.base != self.http_base() {
            // Fini sur un serveur qu'on a quitté depuis : son lien ne
            // désigne rien ici, et ses numéros de salon non plus.
            self.info = Some("un envoi a fini sur un autre serveur — son lien n'est pas partagé ici".into());
            return;
        }
        self.dernier_envoi = Some(std::time::Instant::now());
        self.send(ClientMsg::Chat { text: pret.url, reply_to: None, salon: pret.salon });
    }

    pub(crate) fn start_upload(&self) {
        self.televerser(Vec::new());
    }

    /// Envoie `fichiers` dans le salon, l'un après l'autre, sur un fil ;
    /// sans fichier, le dialogue natif en demande un. C'est le même chemin
    /// pour le trombone et pour un fichier lâché sur la fenêtre.
    pub(crate) fn televerser(&self, fichiers: Vec<std::path::PathBuf>) {
        if self.session.conn.is_none() {
            return;
        }
        let base = self.http_base();
        let agent = self.http_agent();
        let jeton = self.jeton_http.clone();
        let status = self.upload_status.clone();
        // Le salon d'où l'on envoie : c'est là que le lien partira, même si
        // l'on en a changé pendant les minutes de l'envoi.
        let salon = self.session.current;
        let prets = self.liens_prets.clone();
        std::thread::spawn(move || {
            // Relu à chaque requête : un gros fichier part par morceaux
            // pendant des minutes, et une reconnexion entre-temps change
            // le jeton.
            let base_jeton = base.clone();
            let token_hex = move || jeton.lock().unwrap().pour(&base_jeton);
            let fichiers: Vec<std::path::PathBuf> = if fichiers.is_empty() {
                match rfd::FileDialog::new().pick_file() {
                    Some(p) => vec![p],
                    None => return,
                }
            } else {
                // Dix à la fois au plus : au-delà, ce n'est plus un partage.
                fichiers.into_iter().take(10).collect()
            };
            let total = fichiers.len();
            for (rang, path) in fichiers.into_iter().enumerate() {
                // Un message toutes les 1,5 s : le serveur tient cette
                // cadence, et chaque fichier en est un.
                if rang > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(1600));
                }
                let _ = total;
                let name: String = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "fichier".into())
                    .chars()
                    .map(|c| {
                        if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect();
                *status.lock().unwrap() = Some(format!("envoi de {name}…"));
                let result = (|| -> Result<String, String> {
                    const MAX: u64 = 512 * 1024 * 1024;
                    let taille = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
                    if taille == 0 {
                        return Err("fichier vide".into());
                    }
                    if taille > MAX {
                        return Err("fichier trop gros (512 Mo max)".into());
                    }
                    let progres = {
                        let (status, name) = (status.clone(), name.clone());
                        move |pc: u64, _: u32, _: u32| {
                            *status.lock().unwrap() = Some(format!("envoi de {name}… {pc} %"))
                        }
                    };
                    let (upload, parts) =
                        match envoyer_morceaux(&agent, &base, &token_hex, &path, taille, &progres)? {
                            EnvoiMorceaux::Envoye { upload, parts } => (upload, parts),
                            // Un serveur d'avant les morceaux : le fichier d'un
                            // bloc, comme avant, s'il tient dans sa limite — le
                            // temps qu'il soit mis à jour.
                            EnvoiMorceaux::ServeurAncien => {
                                if taille > 25 * 1024 * 1024 {
                                    return Err("le serveur n'accepte pas encore les gros fichiers \
                                                (25 Mo max avant sa mise à jour)"
                                        .into());
                                }
                                let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                                let resp = agent
                                    .post(&format!("{base}/upload?name={name}"))
                                    .set("x-ki-token", &token_hex())
                                    .timeout(std::time::Duration::from_secs(300))
                                    .send_bytes(&bytes)
                                    .map_err(erreur_http)?;
                                let json: serde_json::Value =
                                    resp.into_json().map_err(|e| e.to_string())?;
                                let file_path =
                                    json["url"].as_str().ok_or("réponse invalide")?.to_string();
                                return Ok(format!("{base}{file_path}"));
                            }
                        };
                    let resp = agent
                        .post(&format!("{base}/upload/fin?upload={upload}&name={name}&parts={parts}"))
                        .set("x-ki-token", &token_hex())
                        .timeout(std::time::Duration::from_secs(300))
                        .send_string("")
                        .map_err(erreur_http)?;
                    let json: serde_json::Value = resp.into_json().map_err(|e| e.to_string())?;
                    let file_path = json["url"].as_str().ok_or("réponse invalide")?.to_string();
                    Ok(format!("{base}{file_path}"))
                })();
                match result {
                    Ok(url) => {
                        // Posté par l'interface, par la connexion du moment
                        // (voir `envoyer_liens_prets`).
                        prets.lock().unwrap().push(LienPret { base: base.clone(), salon, url });
                        *status.lock().unwrap() = None;
                    }
                    Err(e) => {
                        *status.lock().unwrap() = Some(format!("échec de l'envoi : {e}"));
                        // Le suivant attend : l'erreur doit se lire.
                        std::thread::sleep(std::time::Duration::from_secs(2));
                    }
                }
            }
        });
    }
}
