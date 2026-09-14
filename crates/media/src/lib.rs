//! Le décodeur de la visionneuse — voir PLAN-CLIPS.md, jalon C0.
//!
//! Un fichier vidéo entre (le MP4 H.264/AAC que le serveur fabrique, ou un
//! clip enregistré par ki-chat), des images RGBA et du son mono 48 kHz en
//! sortent, horodatés. Rien d'autre : la lecture, l'horloge et l'affichage
//! sont l'affaire du client ; ici on ne fait que tirer.
//!
//! Sous Windows, c'est Media Foundation qui démultiplexe et décode — présent
//! dans chaque Windows, rien à livrer. Ailleurs, `ouvrir` refuse en le disant ;
//! le chemin portable (openh264 + symphonia) est prévu au jalon C4.
//!
//! Le son sort en **mono** : la sortie du moteur vocal l'est encore, et c'est
//! par elle que la visionneuse joue (même volume général, même annulateur
//! d'écho). La stéréo viendra avec celle du moteur.

use std::path::Path;

#[cfg(windows)]
mod mf;
pub mod pixels;
pub mod son;

/// Fréquence du son rendu, celle du moteur vocal.
pub const CADENCE: u32 = 48_000;

/// Ce qu'on sait d'un fichier une fois ouvert.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Infos {
    pub duree_ms: u64,
    pub largeur: u32,
    pub hauteur: u32,
    /// Images par seconde annoncées (0.0 si le fichier ne le dit pas).
    pub fps: f32,
    /// Le fichier a une piste audio que l'on sait lire.
    pub audio: bool,
    /// Le fichier a une piste vidéo que l'on sait lire.
    pub video: bool,
}

/// Une image décodée : RGBA serré (4 octets par pixel, pas de remplissage).
pub struct Image {
    pub pts_ms: u64,
    pub largeur: u32,
    pub hauteur: u32,
    pub rgba: Vec<u8>,
}

/// Ce que `suivant` rend.
pub enum Paquet {
    Image(Image),
    /// Du son mono 48 kHz, horodaté à son premier échantillon.
    Audio {
        pts_ms: u64,
        mono: Vec<f32>,
    },
    /// Plus rien sur ce flux.
    Fin,
}

/// Le flux que l'on tire. Les deux se lisent indépendamment : le lecteur
/// garde pour chacun sa propre file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flux {
    Video,
    Audio,
}

/// Un fichier ouvert. Ni `Send` ni `Sync` : le fil qui ouvre est celui qui
/// lit, du début à la fin — c'est ainsi que le fil de lecture du client
/// travaille, et cela dispense de toute question sur l'appartement COM.
pub trait Lecteur {
    fn infos(&self) -> &Infos;

    /// Se place à `ms`. Les paquets suivants partent de la trame clé qui
    /// précède, **avec leur vrai horodatage** : à l'appelant de jeter ce qui
    /// est avant la cible (le son comme l'image).
    fn chercher(&mut self, ms: u64) -> anyhow::Result<()>;

    /// Le prochain paquet du flux demandé, ou `Paquet::Fin`.
    fn suivant(&mut self, flux: Flux) -> anyhow::Result<Paquet>;
}

/// Ouvre un fichier. L'erreur dit pourquoi, en français, pour l'interface.
pub fn ouvrir(chemin: &Path) -> anyhow::Result<Box<dyn Lecteur>> {
    #[cfg(windows)]
    {
        mf::ouvrir(chemin)
    }
    #[cfg(not(windows))]
    {
        let _ = chemin;
        anyhow::bail!(
            "lecture vidéo indisponible sur {} (Windows seulement pour l'instant)",
            std::env::consts::OS
        )
    }
}

/// La première image d'un fichier — pour une vignette, un poster.
pub fn premiere_image(chemin: &Path) -> anyhow::Result<Image> {
    let mut lecteur = ouvrir(chemin)?;
    if !lecteur.infos().video {
        anyhow::bail!("aucune image dans {}", chemin.display());
    }
    loop {
        match lecteur.suivant(Flux::Video)? {
            Paquet::Image(image) => return Ok(image),
            Paquet::Fin => anyhow::bail!("aucune image dans {}", chemin.display()),
            Paquet::Audio { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    /// Fabrique un fichier d'essai avec le ffmpeg de la machine, ou rend
    /// `None` s'il n'y en a pas — le test est alors sauté, pas cassé : ce
    /// crate ne livre pas ffmpeg, il n'en a besoin que pour se prouver.
    fn fabriquer(nom: &str, args: &[&str]) -> Option<PathBuf> {
        // Deux tests qui veulent le même fichier au même moment le
        // fabriqueraient deux fois, l'un par-dessus l'autre : un verrou, et
        // une écriture sous un nom provisoire renommée à la fin.
        static VERROU: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _garde = VERROU.lock().unwrap_or_else(|e| e.into_inner());
        let dossier = std::env::temp_dir().join("ki-media-essais");
        std::fs::create_dir_all(&dossier).ok()?;
        let chemin = dossier.join(nom);
        if chemin.exists() {
            return Some(chemin);
        }
        let provisoire = dossier.join(format!("{nom}.{}.part", std::process::id()));
        let statut = Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error"])
            .args(args)
            .arg("-f")
            .arg("mp4")
            .arg(&provisoire)
            .status()
            .ok()?;
        if !statut.success() {
            let _ = std::fs::remove_file(&provisoire);
            return None;
        }
        std::fs::rename(&provisoire, &chemin).ok()?;
        Some(chemin)
    }

    fn paysage() -> Option<PathBuf> {
        fabriquer(
            "paysage.mp4",
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=1280x720:rate=30",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=44100",
                "-t",
                "3",
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
                "-movflags",
                "+faststart",
            ],
        )
    }

    fn muet() -> Option<PathBuf> {
        fabriquer(
            "muet.mp4",
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=640x360:rate=25",
                "-t",
                "2",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-an",
            ],
        )
    }

    #[test]
    #[cfg(not(windows))]
    fn ailleurs_que_windows_on_le_dit() {
        let e = ouvrir(Path::new("rien.mp4")).err().expect("doit refuser");
        assert!(e.to_string().contains("indisponible"), "{e}");
    }

    #[test]
    #[cfg(windows)]
    fn un_mp4_donne_ses_images_et_son_son() {
        let Some(chemin) = paysage() else {
            eprintln!("ffmpeg absent : test sauté");
            return;
        };
        let mut l = ouvrir(&chemin).expect("ouverture");
        let infos = l.infos().clone();
        assert_eq!((infos.largeur, infos.hauteur), (1280, 720));
        assert!(infos.audio && infos.video, "{infos:?}");
        assert!(
            (2_900..=3_100).contains(&infos.duree_ms),
            "durée {}",
            infos.duree_ms
        );
        assert!((infos.fps - 30.0).abs() < 0.5, "fps {}", infos.fps);

        let mut images = 0u32;
        let mut premiere: Option<Image> = None;
        loop {
            match l.suivant(Flux::Video).expect("image") {
                Paquet::Image(i) => {
                    assert_eq!(i.rgba.len(), 1280 * 720 * 4);
                    if premiere.is_none() {
                        premiere = Some(i);
                    }
                    images += 1;
                }
                Paquet::Fin => break,
                Paquet::Audio { .. } => panic!("du son sur le flux vidéo"),
            }
        }
        assert!(
            (85..=95).contains(&images),
            "{images} images pour 3 s à 30 i/s"
        );
        // La mire n'est pas noire : la conversion a bien produit des couleurs.
        let p = premiere.expect("au moins une image");
        assert!(p.rgba.iter().any(|&v| v > 40));
        assert!(p.rgba.as_chunks::<4>().0.iter().all(|px| px[3] == 255));

        let mut echantillons = 0usize;
        loop {
            match l.suivant(Flux::Audio).expect("son") {
                Paquet::Audio { mono, .. } => echantillons += mono.len(),
                Paquet::Fin => break,
                Paquet::Image(_) => panic!("une image sur le flux audio"),
            }
        }
        // 3 s à 48 kHz, rééchantillonné depuis 44,1 kHz : à 5 % près (amorce
        // et fin de l'encodeur AAC).
        let attendu = 3 * CADENCE as usize;
        assert!(
            (attendu * 95 / 100..=attendu * 105 / 100).contains(&echantillons),
            "{echantillons} échantillons"
        );
    }

    #[test]
    #[cfg(windows)]
    fn chercher_saute_dans_le_fichier() {
        let Some(chemin) = paysage() else {
            eprintln!("ffmpeg absent : test sauté");
            return;
        };
        let mut l = ouvrir(&chemin).expect("ouverture");
        l.chercher(2_000).expect("recherche");
        // Ce qui vient part de la trame clé d'avant la cible, jamais du
        // début — et atteint la cible.
        let mut premier = None;
        let mut atteint = false;
        for _ in 0..200 {
            match l.suivant(Flux::Video).expect("image") {
                Paquet::Image(i) => {
                    premier.get_or_insert(i.pts_ms);
                    if i.pts_ms >= 2_000 {
                        atteint = true;
                        break;
                    }
                }
                Paquet::Fin => break,
                Paquet::Audio { .. } => {}
            }
        }
        let premier = premier.expect("au moins une image après la recherche");
        assert!(premier <= 2_000, "première image à {premier} ms");
        assert!(atteint, "la cible n'est jamais atteinte");
        // Le son aussi repart de là, pas de zéro.
        let mut pts = None;
        for _ in 0..50 {
            if let Paquet::Audio { pts_ms, .. } = l.suivant(Flux::Audio).expect("son") {
                pts = Some(pts_ms);
                break;
            }
        }
        assert!(pts.is_some_and(|p| p >= 1_000), "son à {pts:?} ms");
    }

    #[test]
    #[cfg(windows)]
    fn un_fichier_muet_a_une_premiere_image_et_pas_de_son() {
        let Some(chemin) = muet() else {
            eprintln!("ffmpeg absent : test sauté");
            return;
        };
        let image = premiere_image(&chemin).expect("première image");
        assert_eq!((image.largeur, image.hauteur), (640, 360));
        let mut l = ouvrir(&chemin).expect("ouverture");
        assert!(!l.infos().audio);
        assert!(matches!(
            l.suivant(Flux::Audio).expect("flux audio"),
            Paquet::Fin
        ));
    }

    #[test]
    #[cfg(windows)]
    fn un_fichier_qui_n_existe_pas_refuse_proprement() {
        let e = ouvrir(Path::new("Z:/nulle/part/rien.mp4"))
            .err()
            .expect("doit refuser");
        assert!(!e.to_string().is_empty());
    }
}
