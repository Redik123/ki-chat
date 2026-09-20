//! L'export d'un clip partagé : coupe, format téléphone, titre, mixage
//! (PLAN-CLIPS.md, jalon C3).
//!
//! Le client envoie une **recette** : des bornes, un cadre, un titre, des
//! niveaux — jamais un filtre ni une ligne de commande. Chaque champ est
//! validé ici contre ce que ffprobe dit de la source, et c'est ce module qui
//! compose les arguments de ffmpeg (ligne rouge du plan : le serveur ne fait
//! tourner que ce qu'il construit lui-même). Le texte du titre ne passe même
//! pas par la ligne de commande : il est écrit dans un fichier que
//! `drawtext` lit, sans expansion.
//!
//! Le résultat est `telephone.mp4` (9:16, 1080×1920) ou `export.mp4`
//! (16:9) dans le dossier du clip, avec `export.json` qui dit où l'on en
//! est — le client le relit comme la fiche d'une vidéo, et ffmpeg lui donne
//! sa progression (`-progress pipe:1`).
//!
//! Une simple coupe (16:9, sans titre, sans changement de cadence, sur une
//! source déjà en H.264 1080p) ne réencode pas la vidéo : `-c:v copy`, coupe
//! à la trame clé qui précède — deux secondes de marge au plus, contre des
//! minutes de x264 sur un conteneur à un cœur. Tout le reste passe par x264,
//! sur un nombre de fils borné, avec un délai qui suit la durée de la sortie.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::medias::{Outils, Sonde};

/// Un export ne dépasse pas trois minutes : Reels et TikTok n'en veulent
/// pas davantage, et le serveur non plus.
pub const DUREE_MAX_MS: u64 = 180_000;
const DUREE_MIN_MS: u64 = 500;
const TITRE_MAX: usize = 80;
/// Le téléphone : 1080×1920.
const TEL_LARGEUR: u32 = 1080;
const TEL_HAUTEUR: u32 = 1920;

/// Ce que le membre demande.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recette {
    pub debut_ms: u64,
    pub fin_ms: u64,
    pub format: Format,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub titre: Option<Titre>,
    #[serde(default)]
    pub audio: Audio,
    /// Images par seconde ; 0 = celles de la source.
    #[serde(default)]
    pub cadence: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Format {
    /// 16:9, coupé, 1080p au plus.
    Original,
    /// 9:16 pour TikTok et Instagram, selon un cadre.
    Telephone { cadre: Cadre },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Cadre {
    /// Une fenêtre 9:16 de toute la hauteur, posée à `x` (pixels de la
    /// source, bord gauche) — et, pour suivre l'action, où elle finit.
    Recadre {
        x: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fin_x: Option<u32>,
    },
    /// La vidéo entière au milieu, elle-même floutée et agrandie derrière.
    FondFlou,
    /// Une fenêtre plus large que le 9:16 (de toute la hauteur, `largeur`
    /// pixels de la source, posée à `x`), serrée dans le cadre : on garde
    /// presque tout, un peu déformé.
    Resserre { x: u32, largeur: u32 },
    /// Une fenêtre 9:16 plus petite (facteur 1 à 2), posée à (x, y).
    Zoom { x: u32, y: u32, facteur: f32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Titre {
    pub texte: String,
    #[serde(default)]
    pub position: Position,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Position {
    #[default]
    Haut,
    Bas,
}

/// Les niveaux, 0 (coupé) à 2 ; 1 = tel quel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Audio {
    pub jeu: f32,
    pub micro: f32,
    pub copains: f32,
}

impl Default for Audio {
    fn default() -> Self {
        Self {
            jeu: 1.0,
            micro: 1.0,
            copains: 1.0,
        }
    }
}

/// Ce que l'export doit savoir de la source.
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub largeur: u32,
    pub hauteur: u32,
    pub duree_ms: u64,
    /// Images par seconde de la source.
    pub cadence: f32,
    /// Les pistes après le mélange, dans l'ordre, si le clip les a dites.
    pub pistes: Option<Vec<String>>,
    pub pistes_audio: u32,
    /// Le codec de la piste vidéo (« h264 ») : décide si une coupe peut se
    /// faire en copie.
    pub codec: String,
}

impl Source {
    pub fn depuis(sonde: &Sonde, pistes: Option<Vec<String>>) -> Option<Self> {
        let (codec, largeur, hauteur) = sonde.video.clone()?;
        Some(Self {
            largeur,
            hauteur,
            duree_ms: (sonde.duree_s * 1000.0) as u64,
            cadence: if sonde.cadence > 1.0 {
                sonde.cadence
            } else {
                30.0
            },
            pistes,
            pistes_audio: sonde.pistes_audio,
            codec,
        })
    }
}

/// Où en est l'export, dans `export.json`. Les quatre états et leur sens
/// sont ceux que les clients 0.1.42 connaissent ; ce qui s'est ajouté
/// depuis est facultatif et se lit avec un défaut.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Etat {
    /// `en_attente`, `en_cours`, `pret` ou `erreur`.
    pub etat: String,
    #[serde(default)]
    pub pour_cent: u8,
    /// Le fichier produit, dans le dossier du clip.
    #[serde(default)]
    pub fichier: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub duree_s: f32,
    #[serde(default)]
    pub largeur: u32,
    #[serde(default)]
    pub hauteur: u32,
    #[serde(default)]
    pub taille: u64,
    /// En attente : combien de tâches la fabrique traite avant celle-ci.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derriere: Option<u32>,
    /// Comment la vidéo est faite : « copie » (coupe sans réencodage) ou
    /// « x264 ».
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// L'instant (secondes Unix) de la dernière écriture : le client sait
    /// si l'état bouge encore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depuis: Option<u64>,
}

/// Écrit l'état. L'échec remonte, parce qu'un disque plein ou un dossier aux
/// mauvais droits laisserait le client relire un 404 sans fin : la route qui
/// accepte l'export doit pouvoir répondre 500 à la place.
pub fn ecrire_etat(dossier: &Path, etat: &Etat) -> std::io::Result<()> {
    let mut etat = etat.clone();
    etat.depuis = Some(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    );
    let json = serde_json::to_vec_pretty(&etat).map_err(std::io::Error::other)?;
    crate::store::write_atomic(&dossier.join("export.json"), &json).inspect_err(|e| {
        tracing::error!("export.json non écrit dans {} : {e}", dossier.display());
    })
}

pub fn lire_etat(dossier: &Path) -> Option<Etat> {
    let octets = std::fs::read(dossier.join("export.json")).ok()?;
    serde_json::from_slice(&octets).ok()
}

/// La police du titre : `KI_POLICE`, sinon celle de l'image Docker
/// (DejaVu Sans Bold), sinon ce qu'une machine de développement a.
pub fn police() -> Option<PathBuf> {
    let candidates: Vec<PathBuf> = std::env::var("KI_POLICE")
        .ok()
        .map(PathBuf::from)
        .into_iter()
        .chain(
            [
                "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
                "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf",
                "C:/Windows/Fonts/arialbd.ttf",
                "C:/Windows/Fonts/segoeuib.ttf",
                "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
            ]
            .iter()
            .map(PathBuf::from),
        )
        .collect();
    candidates.into_iter().find(|p| p.is_file())
}

/// Les noms que l'atelier sait produire : le 16:9 coupé, et le téléphone.
pub const EXPORTS: [&str; 2] = ["export.mp4", "telephone.mp4"];

/// Le nom du fichier produit par une recette.
pub fn nom_sortie(recette: &Recette) -> &'static str {
    match recette.format {
        Format::Original => EXPORTS[0],
        Format::Telephone { .. } => EXPORTS[1],
    }
}

/// Un nombre pair, jamais nul : ce que les encodeurs veulent.
fn pair(v: u32) -> u32 {
    (v & !1).max(2)
}

/// La fenêtre 9:16 de toute la hauteur de la source : sa largeur.
fn largeur_fenetre(source: &Source) -> u32 {
    pair(source.hauteur * 9 / 16)
}

/// La recette contre la source : tout ce qui sort des bornes est refusé,
/// avec la raison.
pub fn valider(recette: &Recette, source: &Source, police_disponible: bool) -> Result<(), String> {
    if recette.fin_ms <= recette.debut_ms {
        return Err("la fin doit venir après le début".into());
    }
    let duree = recette.fin_ms - recette.debut_ms;
    if duree < DUREE_MIN_MS {
        return Err("moins d'une demi-seconde, ce n'est pas un clip".into());
    }
    if duree > DUREE_MAX_MS {
        return Err("trois minutes au plus".into());
    }
    if recette.fin_ms > source.duree_ms + 500 {
        return Err("la fin dépasse la vidéo".into());
    }
    if source.largeur < 64 || source.hauteur < 64 {
        return Err("source trop petite".into());
    }
    if let Format::Telephone { cadre } = &recette.format {
        let largeur = largeur_fenetre(source);
        if largeur > source.largeur {
            return Err("la source est déjà plus étroite qu'un téléphone".into());
        }
        match cadre {
            Cadre::Recadre { x, fin_x } => {
                for x in std::iter::once(x).chain(fin_x.iter()) {
                    if x + largeur > source.largeur {
                        return Err("le cadre sort de l'image".into());
                    }
                }
            }
            Cadre::FondFlou => {}
            Cadre::Resserre { x, largeur: l } => {
                if *l < largeur || *l > source.largeur {
                    return Err("la largeur resserrée sort des bornes".into());
                }
                if x + l > source.largeur {
                    return Err("le cadre sort de l'image".into());
                }
            }
            Cadre::Zoom { x, y, facteur } => {
                if !(1.0..=2.0).contains(facteur) || !facteur.is_finite() {
                    return Err("le zoom va de 1 à 2".into());
                }
                let (l, h) = (
                    pair((largeur as f32 / facteur) as u32),
                    pair((source.hauteur as f32 / facteur) as u32),
                );
                if x + l > source.largeur || y + h > source.hauteur {
                    return Err("le cadre du zoom sort de l'image".into());
                }
            }
        }
    }
    if let Some(t) = &recette.titre {
        let texte = t.texte.trim();
        if texte.is_empty() {
            return Err("titre vide".into());
        }
        if texte.chars().count() > TITRE_MAX {
            return Err(format!("titre trop long ({TITRE_MAX} caractères au plus)"));
        }
        if texte.chars().any(|c| c.is_control()) {
            return Err("titre : caractères interdits".into());
        }
        if !police_disponible {
            return Err("pas de police sur le serveur : pas de titre".into());
        }
    }
    for (nom, v) in [
        ("jeu", recette.audio.jeu),
        ("micro", recette.audio.micro),
        ("copains", recette.audio.copains),
    ] {
        if !(0.0..=2.0).contains(&v) || !v.is_finite() {
            return Err(format!("niveau « {nom} » hors bornes (0 à 2)"));
        }
    }
    if !matches!(recette.cadence, 0 | 24 | 25 | 30 | 50 | 60) {
        return Err("cadence inconnue".into());
    }
    Ok(())
}

/// Un chemin dans un graphe de filtres : barres droites, deux-points
/// échappés, entre apostrophes.
fn chemin_filtre(p: &Path) -> Result<String, String> {
    let s = p.to_string_lossy().replace('\\', "/");
    if s.contains('\'') {
        return Err("chemin avec une apostrophe".into());
    }
    Ok(format!("'{}'", s.replace(':', "\\:")))
}

/// La chaîne vidéo du graphe, de `[0:v:0]` à `[v]`.
fn chaine_video(
    recette: &Recette,
    source: &Source,
    titre: Option<(&Path, &Path)>,
) -> Result<String, String> {
    let duree_s = (recette.fin_ms - recette.debut_ms) as f32 / 1000.0;
    let hauteur_sortie;
    let mut chaine = String::from("[0:v:0]");
    match &recette.format {
        Format::Original => {
            hauteur_sortie = source.hauteur.min(1080);
            chaine.push_str("scale='min(1920,iw)':'min(1080,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2");
        }
        Format::Telephone { cadre } => {
            hauteur_sortie = TEL_HAUTEUR;
            let largeur = largeur_fenetre(source);
            match cadre {
                Cadre::Recadre { x, fin_x } => {
                    let x_expr = match fin_x {
                        // La fenêtre glisse d'un bord à l'autre au fil des
                        // images : `n` compte depuis la première de la coupe.
                        Some(fx) if fx != x => {
                            let images = (duree_s * source.cadence).max(1.0);
                            format!("'{x}+({fx}-{x})*min(n/{images:.1}\\,1)'")
                        }
                        _ => x.to_string(),
                    };
                    chaine.push_str(&format!(
                        "crop={largeur}:{}:{x_expr}:0,scale={TEL_LARGEUR}:{TEL_HAUTEUR}",
                        source.hauteur
                    ));
                }
                Cadre::FondFlou => {
                    chaine.push_str(&format!(
                        "split=2[fond0][devant0];[fond0]scale={TEL_LARGEUR}:{TEL_HAUTEUR}:force_original_aspect_ratio=increase,crop={TEL_LARGEUR}:{TEL_HAUTEUR},boxblur=luma_radius=24:luma_power=2:chroma_radius=12:chroma_power=2[fond];[devant0]scale={TEL_LARGEUR}:-2[devant];[fond][devant]overlay=(W-w)/2:(H-h)/2"
                    ));
                }
                Cadre::Resserre { x, largeur: l } => {
                    // `scale` sans garder le rapport : c'est le serrage voulu.
                    chaine.push_str(&format!(
                        "crop={}:{}:{x}:0,scale={TEL_LARGEUR}:{TEL_HAUTEUR}",
                        pair(*l),
                        source.hauteur
                    ));
                }
                Cadre::Zoom { x, y, facteur } => {
                    let l = pair((largeur as f32 / facteur) as u32);
                    let h = pair((source.hauteur as f32 / facteur) as u32);
                    chaine.push_str(&format!(
                        "crop={l}:{h}:{x}:{y},scale={TEL_LARGEUR}:{TEL_HAUTEUR}"
                    ));
                }
            }
        }
    }
    if recette.cadence > 0 {
        chaine.push_str(&format!(",fps={}", recette.cadence));
    }
    if let (Some(t), Some((police, fichier))) = (&recette.titre, titre) {
        let taille = (hauteur_sortie / 26).max(18);
        let y = match t.position {
            Position::Haut => "h*0.07".to_string(),
            Position::Bas => "h-text_h-h*0.07".to_string(),
        };
        chaine.push_str(&format!(
            ",drawtext=fontfile={}:textfile={}:expansion=none:fontsize={taille}:fontcolor=white:borderw=4:bordercolor=black@0.8:x=(w-text_w)/2:y={y}",
            chemin_filtre(police)?,
            chemin_filtre(fichier)?
        ));
    }
    chaine.push_str("[v]");
    Ok(chaine)
}

/// La chaîne son du graphe, jusqu'à `[son]` — `None` : muet.
fn chaine_audio(recette: &Recette, source: &Source) -> Option<String> {
    let Some(pistes) = &source.pistes else {
        // Pistes inconnues : le mélange tel quel, s'il y en a un.
        return (source.pistes_audio > 0).then(|| "[0:a:0]volume=1[son]".to_string());
    };
    let niveau = |nom: &str| match nom {
        "jeu" => recette.audio.jeu,
        "micro" => recette.audio.micro,
        "copains" => recette.audio.copains,
        _ => 0.0,
    };
    let gardees: Vec<(u32, f32)> = pistes
        .iter()
        .enumerate()
        .map(|(i, nom)| (i as u32 + 1, niveau(nom)))
        .filter(|(_, v)| *v > 0.001)
        .collect();
    match gardees.len() {
        0 => None,
        1 => Some(format!(
            "[0:a:{}]volume={:.3}[son]",
            gardees[0].0, gardees[0].1
        )),
        n => {
            let mut s = String::new();
            for (k, (i, v)) in gardees.iter().enumerate() {
                s.push_str(&format!("[0:a:{i}]volume={v:.3}[a{k}];"));
            }
            for k in 0..n {
                s.push_str(&format!("[a{k}]"));
            }
            s.push_str(&format!("amix=inputs={n}:normalize=0[son]"));
            Some(s)
        }
    }
}

/// Vrai si la recette n'est qu'une coupe d'une source déjà bonne : ni
/// titre, ni cadence imposée, ni format téléphone, du H.264 qui tient dans
/// le 1080p. Alors la vidéo se copie au lieu de se réencoder.
pub fn coupe_en_copie(recette: &Recette, source: &Source) -> bool {
    recette.format == Format::Original
        && recette.titre.is_none()
        && recette.cadence == 0
        && source.codec == "h264"
        && source.largeur <= 1920
        && source.hauteur <= 1080
}

/// Les arguments de ffmpeg pour une recette validée : ceux d'avant
/// l'entrée, ceux d'après (jusqu'au nom de sortie exclu).
pub fn composer(
    recette: &Recette,
    source: &Source,
    titre: Option<(&Path, &Path)>,
) -> Result<(Vec<String>, Vec<String>), String> {
    let debut_s = recette.debut_ms as f64 / 1000.0;
    let duree_s = (recette.fin_ms - recette.debut_ms) as f64 / 1000.0;
    let avant = vec!["-ss".to_string(), format!("{debut_s:.3}")];
    if coupe_en_copie(recette, source) {
        // La coupe seule : `-ss` avant l'entrée saute à la trame clé qui
        // précède, la vidéo se copie, le son se compose comme d'habitude
        // (c'est le pas cher). `-avoid_negative_ts` remet la première
        // image à zéro, sinon un lecteur attend le début manquant.
        let mut apres: Vec<String> = vec!["-t".into(), format!("{duree_s:.3}")];
        match chaine_audio(recette, source) {
            Some(a) => apres.extend(
                [
                    "-filter_complex", &a, "-map", "0:v:0", "-map", "[son]", "-c:a", "aac",
                    "-b:a", "160k", "-ar", "48000", "-ac", "2",
                ]
                .map(String::from),
            ),
            None => apres.extend(["-map", "0:v:0", "-an"].map(String::from)),
        }
        apres.extend(
            [
                "-c:v", "copy", "-avoid_negative_ts", "make_zero", "-movflags", "+faststart",
                "-progress", "pipe:1", "-nostats",
            ]
            .map(String::from),
        );
        return Ok((avant, apres));
    }
    let video = chaine_video(recette, source, titre)?;
    let audio = chaine_audio(recette, source);
    let graphe = match &audio {
        Some(a) => format!("{video};{a}"),
        None => video,
    };
    let mut apres: Vec<String> = vec![
        "-t".into(),
        format!("{duree_s:.3}"),
        "-filter_complex".into(),
        graphe,
        "-map".into(),
        "[v]".into(),
    ];
    match audio {
        Some(_) => apres.extend(
            [
                "-map", "[son]", "-c:a", "aac", "-b:a", "160k", "-ar", "48000", "-ac", "2",
            ]
            .map(String::from),
        ),
        None => apres.push("-an".into()),
    }
    // x264 `superfast` : un cran plus vite que `veryfast` pour un export
    // qu'on regarde sur un téléphone, et des fils bornés à ce que le
    // conteneur a vraiment (voir `medias::fils_ffmpeg`).
    let fils = crate::medias::fils_ffmpeg().to_string();
    apres.extend(
        [
            "-c:v",
            "libx264",
            "-preset",
            "superfast",
            "-crf",
            "22",
            "-profile:v",
            "high",
            "-pix_fmt",
            "yuv420p",
            "-threads",
            &fils,
            "-filter_threads",
            &fils,
            "-movflags",
            "+faststart",
            "-progress",
            "pipe:1",
            "-nostats",
        ]
        .map(String::from),
    );
    Ok((avant, apres))
}

/// Fait l'export dans le dossier du clip : `source.mp4` → le fichier de la
/// recette, `export.json` tenu à jour en chemin. Sur le pool bloquant.
pub fn executer(outils: &Outils, dossier: &Path, recette: &Recette) -> Result<Etat, String> {
    let sortie = nom_sortie(recette);
    let mut etat = Etat {
        etat: "en_cours".into(),
        fichier: Some(sortie.to_string()),
        ..Default::default()
    };
    ecrire_etat(dossier, &etat).map_err(|e| format!("export.json : {e}"))?;
    let resultat = (|| -> Result<(), String> {
        let source_chemin = dossier.join("source.mp4");
        let sonde = crate::medias::sonder(outils, &source_chemin)?;
        let pistes = crate::medias::lire_meta(dossier).and_then(|m| m.pistes);
        let source = Source::depuis(&sonde, pistes).ok_or("source sans image")?;
        let police = police();
        valider(recette, &source, police.is_some())?;
        etat.mode = Some(if coupe_en_copie(recette, &source) { "copie" } else { "x264" }.into());
        let _ = ecrire_etat(dossier, &etat);
        // Le titre, dans un fichier que drawtext lit tel quel.
        let fichier_titre = dossier.join("titre.txt");
        let titre = match (&recette.titre, &police) {
            (Some(t), Some(p)) => {
                std::fs::write(&fichier_titre, t.texte.trim()).map_err(|e| e.to_string())?;
                Some((p.as_path(), fichier_titre.as_path()))
            }
            _ => None,
        };
        let (avant, apres) = composer(recette, &source, titre)?;
        let chemin_sortie = dossier.join(sortie);
        let _ = std::fs::remove_file(&chemin_sortie);
        let mut cmd = crate::medias::commande_ffmpeg(outils);
        cmd.args(["-y", "-nostdin", "-hide_banner", "-loglevel", "error"])
            .args(&avant)
            .arg("-i")
            .arg(&source_chemin)
            .args(&apres)
            .arg(&chemin_sortie);
        let duree_us = (recette.fin_ms - recette.debut_ms) * 1000;
        let delai = crate::medias::delai_pour(duree_us as f32 / 1_000_000.0);
        lancer_avec_progression(&mut cmd, duree_us, delai, |pc| {
            etat.pour_cent = pc;
            let _ = ecrire_etat(dossier, &etat);
        })?;
        let apres = crate::medias::sonder(outils, &chemin_sortie)?;
        let (_, l, h) = apres.video.ok_or("l'export n'a pas produit d'image")?;
        etat.largeur = l;
        etat.hauteur = h;
        etat.duree_s = apres.duree_s;
        etat.taille = std::fs::metadata(&chemin_sortie)
            .map(|m| m.len())
            .unwrap_or(0);
        Ok(())
    })();
    match resultat {
        Ok(()) => {
            etat.etat = "pret".into();
            etat.pour_cent = 100;
            etat.message = None;
            ecrire_etat(dossier, &etat).map_err(|e| format!("export.json : {e}"))?;
            Ok(etat)
        }
        Err(e) => {
            // Un fichier à moitié écrit (ffmpeg tué au délai) n'est pas
            // un export : il ne doit pas se servir ni se partager.
            let _ = std::fs::remove_file(dossier.join(sortie));
            etat.etat = "erreur".into();
            etat.message = Some(e.chars().take(200).collect());
            let _ = ecrire_etat(dossier, &etat);
            Err(e)
        }
    }
}

/// Lance ffmpeg et suit `-progress pipe:1` : `out_time_us=…` à chaque
/// seconde environ, `progress=end` à la fin. `avancer` reçoit le pourcentage
/// quand il change ; passé `delai`, ffmpeg est tué.
fn lancer_avec_progression(
    cmd: &mut Command,
    duree_us: u64,
    delai: Duration,
    mut avancer: impl FnMut(u8),
) -> Result<(), String> {
    let mut enfant = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("lancement impossible : {e}"))?;
    let sortie = enfant.stdout.take().expect("stdout");
    let mut erreur = enfant.stderr.take().expect("stderr");
    let (tx, rx) = std::sync::mpsc::channel::<u64>();
    let lecteur = std::thread::spawn(move || {
        for ligne in BufReader::new(sortie).lines().map_while(Result::ok) {
            // `out_time_us` (ffmpeg récent) ou `out_time_ms` (qui, malgré
            // son nom, est en microsecondes) : les deux disent la même chose.
            if let Some(v) = ligne
                .strip_prefix("out_time_us=")
                .or_else(|| ligne.strip_prefix("out_time_ms="))
            {
                if let Ok(us) = v.trim().parse::<i64>() {
                    let _ = tx.send(us.max(0) as u64);
                }
            }
        }
    });
    let lecteur_err = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = erreur.read_to_end(&mut buf);
        buf
    });
    let debut = Instant::now();
    let mut dernier: u8 = 0;
    let statut = loop {
        while let Ok(us) = rx.try_recv() {
            let pc = ((us * 100) / duree_us.max(1)).min(99) as u8;
            if pc != dernier {
                dernier = pc;
                avancer(pc);
            }
        }
        match enfant.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if debut.elapsed() > delai => {
                let _ = enfant.kill();
                let _ = enfant.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => break None,
        }
    };
    let _ = lecteur.join();
    let erreur = lecteur_err.join().unwrap_or_default();
    match statut {
        Some(s) if s.success() => Ok(()),
        Some(_) => {
            let texte = String::from_utf8_lossy(&erreur);
            let derniere = texte
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("ffmpeg a échoué");
            Err(format!(
                "ffmpeg : {}",
                derniere.chars().take(160).collect::<String>()
            ))
        }
        None => Err("délai dépassé".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> Source {
        Source {
            largeur: 1920,
            hauteur: 1080,
            duree_ms: 30_000,
            cadence: 60.0,
            pistes: Some(vec!["jeu".into(), "micro".into(), "copains".into()]),
            pistes_audio: 4,
            codec: "h264".into(),
        }
    }

    fn recette(format: Format) -> Recette {
        Recette {
            debut_ms: 1_000,
            fin_ms: 11_000,
            format,
            titre: None,
            audio: Audio::default(),
            cadence: 0,
        }
    }

    #[test]
    fn la_recette_se_lit_en_json() {
        let json = r#"{"debut_ms":1000,"fin_ms":5000,"format":{"type":"telephone","cadre":{"type":"recadre","x":420,"fin_x":900}},"titre":{"texte":"ACE","position":"bas"},"audio":{"jeu":1.0,"micro":0.8,"copains":0.0},"cadence":30}"#;
        let r: Recette = serde_json::from_str(json).unwrap();
        assert_eq!(
            r.format,
            Format::Telephone {
                cadre: Cadre::Recadre {
                    x: 420,
                    fin_x: Some(900)
                }
            }
        );
        assert_eq!(r.titre.as_ref().map(|t| t.position), Some(Position::Bas));
        assert_eq!(r.cadence, 30);
        let r: Recette =
            serde_json::from_str(r#"{"debut_ms":0,"fin_ms":5000,"format":{"type":"original"}}"#)
                .unwrap();
        assert_eq!(r.format, Format::Original);
        assert_eq!(r.audio, Audio::default());
        assert_eq!(nom_sortie(&r), "export.mp4");
    }

    #[test]
    fn une_recette_hors_bornes_est_refusee() {
        let s = source();
        let ok = recette(Format::Original);
        assert!(valider(&ok, &s, false).is_ok());
        let mut r = ok.clone();
        r.fin_ms = r.debut_ms;
        assert!(valider(&r, &s, false).is_err());
        r.fin_ms = r.debut_ms + 200;
        assert!(valider(&r, &s, false).is_err());
        r.fin_ms = r.debut_ms + DUREE_MAX_MS + 1;
        assert!(valider(&r, &s, false).is_err());
        let mut r = ok.clone();
        r.fin_ms = 40_000;
        assert!(valider(&r, &s, false).is_err(), "la fin dépasse la vidéo");
        // Le cadre : 1080 de haut → 606 de large (pair), x jusqu'à 1314.
        let cadre = |x, fin_x| {
            recette(Format::Telephone {
                cadre: Cadre::Recadre { x, fin_x },
            })
        };
        assert!(valider(&cadre(1314, None), &s, false).is_ok());
        assert!(valider(&cadre(1315, None), &s, false).is_err());
        assert!(valider(&cadre(0, Some(2000)), &s, false).is_err());
        let zoom = |x, y, facteur| {
            recette(Format::Telephone {
                cadre: Cadre::Zoom { x, y, facteur },
            })
        };
        assert!(valider(&zoom(0, 0, 1.5), &s, false).is_ok());
        assert!(valider(&zoom(0, 0, 2.5), &s, false).is_err());
        assert!(valider(&zoom(1700, 0, 1.5), &s, false).is_err());
        // Le titre : borné, sans contrôle, et seulement avec une police.
        let mut r = ok.clone();
        r.titre = Some(Titre {
            texte: "ACE".into(),
            position: Position::Haut,
        });
        assert!(valider(&r, &s, true).is_ok());
        assert!(valider(&r, &s, false).is_err());
        r.titre = Some(Titre {
            texte: "a\u{7}".into(),
            position: Position::Haut,
        });
        assert!(valider(&r, &s, true).is_err());
        r.titre = Some(Titre {
            texte: "x".repeat(81),
            position: Position::Haut,
        });
        assert!(valider(&r, &s, true).is_err());
        // Les niveaux et la cadence.
        let mut r = ok.clone();
        r.audio.micro = 2.5;
        assert!(valider(&r, &s, false).is_err());
        r.audio.micro = f32::NAN;
        assert!(valider(&r, &s, false).is_err());
        let mut r = ok;
        r.cadence = 45;
        assert!(valider(&r, &s, false).is_err());
    }

    #[test]
    fn la_ligne_ffmpeg_se_compose_pour_chaque_cadre() {
        // Une source HEVC : la coupe 16:9 elle-même doit repasser par x264.
        let mut s = source();
        s.codec = "hevc".into();
        let (avant, apres) = composer(&recette(Format::Original), &s, None).unwrap();
        assert_eq!(avant, ["-ss", "1.000"]);
        assert_eq!(&apres[..2], ["-t", "10.000"]);
        let graphe = &apres[3];
        assert!(graphe.starts_with("[0:v:0]scale="), "{graphe}");
        // Trois pistes à 1 : un mélange des trois.
        assert!(
            graphe.contains("amix=inputs=3:normalize=0[son]"),
            "{graphe}"
        );
        assert!(apres.contains(&"[son]".to_string()));
        assert!(apres.contains(&"libx264".to_string()));
        assert!(apres.contains(&"-threads".to_string()));

        let glisse = recette(Format::Telephone {
            cadre: Cadre::Recadre {
                x: 100,
                fin_x: Some(700),
            },
        });
        let (_, apres) = composer(&glisse, &s, None).unwrap();
        let graphe = &apres[3];
        assert!(
            graphe.contains("crop=606:1080:'100+(700-100)*min(n/600.0\\,1)':0,scale=1080:1920"),
            "{graphe}"
        );

        let flou = recette(Format::Telephone {
            cadre: Cadre::FondFlou,
        });
        let (_, apres) = composer(&flou, &s, None).unwrap();
        assert!(apres[3].contains("boxblur") && apres[3].contains("overlay=(W-w)/2:(H-h)/2"));

        let mut zoom = recette(Format::Telephone {
            cadre: Cadre::Zoom {
                x: 300,
                y: 100,
                facteur: 1.5,
            },
        });
        zoom.cadence = 30;
        zoom.audio = Audio {
            jeu: 1.0,
            micro: 0.0,
            copains: 0.0,
        };
        let (_, apres) = composer(&zoom, &s, None).unwrap();
        let graphe = &apres[3];
        assert!(
            graphe.contains("crop=404:720:300:100,scale=1080:1920,fps=30"),
            "{graphe}"
        );
        // Une seule piste gardée : pas de mélange, un simple volume.
        assert!(graphe.ends_with("[0:a:1]volume=1.000[son]"), "{graphe}");

        // Tout coupé : muet.
        let mut muet = recette(Format::Original);
        muet.audio = Audio {
            jeu: 0.0,
            micro: 0.0,
            copains: 0.0,
        };
        let (_, apres) = composer(&muet, &s, None).unwrap();
        assert!(apres.contains(&"-an".to_string()) && !apres[3].contains("[son]"));

        // Pistes inconnues : le mélange tel quel.
        let mut inconnue = s.clone();
        inconnue.pistes = None;
        let (_, apres) = composer(&recette(Format::Original), &inconnue, None).unwrap();
        assert!(apres[3].ends_with("[0:a:0]volume=1[son]"));

        // Le titre : police et texte par leurs fichiers, deux-points échappés.
        let mut titre = recette(Format::Telephone {
            cadre: Cadre::FondFlou,
        });
        titre.titre = Some(Titre {
            texte: "ACE".into(),
            position: Position::Bas,
        });
        let (_, apres) = composer(
            &titre,
            &s,
            Some((
                Path::new("C:\\Fonts\\a.ttf"),
                Path::new("/data/clips/x/titre.txt"),
            )),
        )
        .unwrap();
        let graphe = &apres[3];
        assert!(graphe.contains("drawtext=fontfile='C\\:/Fonts/a.ttf':textfile='/data/clips/x/titre.txt':expansion=none"), "{graphe}");
        assert!(graphe.contains("y=h-text_h-h*0.07"));
    }

    /// La coupe seule d'un clip H.264 1080p ne réencode pas : la vidéo se
    /// copie, le son se compose ; un titre, une cadence, un téléphone ou
    /// une source qui n'est pas du H.264 ramènent x264.
    #[test]
    fn une_coupe_seule_se_fait_en_copie() {
        let s = source();
        let simple = recette(Format::Original);
        assert!(coupe_en_copie(&simple, &s));
        let (avant, apres) = composer(&simple, &s, None).unwrap();
        assert_eq!(avant, ["-ss", "1.000"]);
        assert_eq!(&apres[..2], ["-t", "10.000"]);
        let copie = apres.windows(2).any(|w| w == ["-c:v", "copy"]);
        assert!(copie, "{apres:?}");
        assert!(!apres.contains(&"libx264".to_string()));
        assert!(apres.contains(&"make_zero".to_string()));
        // Le son passe quand même par le mélange demandé.
        assert!(apres.iter().any(|a| a.contains("amix=inputs=3")), "{apres:?}");
        // Muet : ni graphe ni piste.
        let mut muet = simple.clone();
        muet.audio = Audio { jeu: 0.0, micro: 0.0, copains: 0.0 };
        let (_, apres) = composer(&muet, &s, None).unwrap();
        assert!(apres.contains(&"-an".to_string()) && !apres.contains(&"-filter_complex".to_string()));

        let mut titre = simple.clone();
        titre.titre = Some(Titre { texte: "ACE".into(), position: Position::Haut });
        assert!(!coupe_en_copie(&titre, &s));
        let mut cadence = simple.clone();
        cadence.cadence = 30;
        assert!(!coupe_en_copie(&cadence, &s));
        let tel = recette(Format::Telephone { cadre: Cadre::FondFlou });
        assert!(!coupe_en_copie(&tel, &s));
        let mut hevc = s.clone();
        hevc.codec = "hevc".into();
        assert!(!coupe_en_copie(&simple, &hevc));
        let mut grande = s.clone();
        grande.largeur = 2560;
        grande.hauteur = 1440;
        assert!(!coupe_en_copie(&simple, &grande));
    }

    /// Ce que le client lit dans `export.json` : les états et les champs
    /// de 0.1.42 gardent leur sens, les nouveaux sont facultatifs.
    #[test]
    fn l_etat_d_export_reste_lisible_par_les_clients_d_avant() {
        // Un état d'avant, sans les champs nouveaux.
        let ancien: Etat = serde_json::from_str(
            r#"{"etat":"en_cours","pour_cent":42,"fichier":"telephone.mp4","message":null,"duree_s":0.0,"largeur":0,"hauteur":0,"taille":0}"#,
        )
        .unwrap();
        assert_eq!(ancien.etat, "en_cours");
        assert_eq!(ancien.pour_cent, 42);
        assert_eq!(ancien.derriere, None);
        assert_eq!(ancien.mode, None);
        // Un état d'aujourd'hui : les nouveaux champs ne s'écrivent que
        // s'ils sont posés, et ce qu'un client 0.1.42 lit ne change pas.
        let dossier = std::env::temp_dir().join(format!("ki-export-etat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dossier);
        std::fs::create_dir_all(&dossier).unwrap();
        ecrire_etat(
            &dossier,
            &Etat {
                etat: "en_attente".into(),
                fichier: Some("export.mp4".into()),
                derriere: Some(2),
                ..Default::default()
            },
        )
        .unwrap();
        let texte = std::fs::read_to_string(dossier.join("export.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&texte).unwrap();
        assert_eq!(v["etat"], "en_attente");
        assert_eq!(v["fichier"], "export.mp4");
        assert_eq!(v["pour_cent"], 0);
        assert_eq!(v["derriere"], 2);
        assert!(v["depuis"].as_u64().is_some_and(|t| t > 1_700_000_000));
        assert!(v.get("mode").is_none());
        let relu = lire_etat(&dossier).unwrap();
        assert_eq!(relu.derriere, Some(2));
        // Un dossier qu'on ne peut pas écrire : l'erreur remonte.
        assert!(ecrire_etat(&dossier.join("absent"), &Etat::default()).is_err());
        let _ = std::fs::remove_dir_all(&dossier);
    }

    #[test]
    fn le_cadre_resserre_serre_une_fenetre_plus_large() {
        let s = source();
        let serre = |x, largeur| {
            recette(Format::Telephone {
                cadre: Cadre::Resserre { x, largeur },
            })
        };
        // Entre la fenêtre 9:16 (606) et toute la largeur, sans sortir.
        assert!(valider(&serre(300, 1200), &s, false).is_ok());
        assert!(valider(&serre(0, 1920), &s, false).is_ok());
        assert!(valider(&serre(0, 600), &s, false).is_err());
        assert!(valider(&serre(800, 1200), &s, false).is_err());
        assert!(valider(&serre(0, 2000), &s, false).is_err());
        let (_, apres) = composer(&serre(300, 1201), &s, None).unwrap();
        assert!(
            apres[3].contains("crop=1200:1080:300:0,scale=1080:1920"),
            "{}",
            apres[3]
        );
        let r: Recette = serde_json::from_str(
            r#"{"debut_ms":0,"fin_ms":5000,"format":{"type":"telephone","cadre":{"type":"resserre","x":300,"largeur":1200}}}"#,
        )
        .unwrap();
        assert_eq!(
            r.format,
            Format::Telephone {
                cadre: Cadre::Resserre {
                    x: 300,
                    largeur: 1200
                }
            }
        );
    }

    #[test]
    fn un_export_telephone_sort_en_1080x1920_avec_une_piste() {
        let Some(outils) = crate::medias::detecter() else {
            eprintln!("ffmpeg absent : test sauté");
            return;
        };
        let dossier = std::env::temp_dir().join(format!("ki-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dossier);
        std::fs::create_dir_all(&dossier).unwrap();
        let statut = Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error"])
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=1280x720:rate=30",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=660:sample_rate=48000",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=880:sample_rate=48000",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1100:sample_rate=48000",
                "-t",
                "3",
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-map",
                "2:a",
                "-map",
                "3:a",
                "-map",
                "4:a",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-ac",
                "2",
            ])
            .arg(dossier.join("source.mp4"))
            .status();
        if !statut.map(|s| s.success()).unwrap_or(false) {
            eprintln!("libx264 absent : test sauté");
            return;
        }
        crate::medias::ecrire_meta(
            &dossier,
            &crate::medias::Meta {
                etat: "pret".into(),
                clip: true,
                pistes: Some(vec!["jeu".into(), "micro".into(), "copains".into()]),
                ..Default::default()
            },
        )
        .unwrap();
        let recette = Recette {
            debut_ms: 500,
            fin_ms: 2_000,
            format: Format::Telephone {
                cadre: Cadre::FondFlou,
            },
            titre: police().map(|_| Titre {
                texte: "ACE de rédik".into(),
                position: Position::Haut,
            }),
            audio: Audio {
                jeu: 1.0,
                micro: 0.7,
                copains: 0.0,
            },
            cadence: 30,
        };
        let etat = executer(&outils, &dossier, &recette).expect("export");
        assert_eq!(etat.etat, "pret");
        assert_eq!((etat.largeur, etat.hauteur), (1080, 1920));
        assert!(
            (1.3..=1.7).contains(&etat.duree_s),
            "durée {}",
            etat.duree_s
        );
        let sonde = crate::medias::sonder(&outils, &dossier.join("telephone.mp4")).unwrap();
        assert_eq!(sonde.pistes_audio, 1);
        assert_eq!(sonde.video.as_ref().map(|v| v.0.as_str()), Some("h264"));
        let relu = lire_etat(&dossier).unwrap();
        assert_eq!(relu.fichier.as_deref(), Some("telephone.mp4"));
        assert_eq!(relu.pour_cent, 100);
        // Une recette hors bornes est refusée, et l'état le dit.
        let mut mauvaise = recette;
        mauvaise.fin_ms = 60_000;
        assert!(executer(&outils, &dossier, &mauvaise).is_err());
        assert_eq!(lire_etat(&dossier).unwrap().etat, "erreur");
        let _ = std::fs::remove_dir_all(&dossier);
    }
}
