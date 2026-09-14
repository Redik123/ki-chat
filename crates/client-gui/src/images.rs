//! Aperçu des images partagées dans le fil de discussion — et, depuis la
//! visionneuse, la fiche des vidéos.
//!
//! # Pourquoi on ne télécharge pas n'importe quelle adresse
//!
//! Une adresse écrite dans un message vient d'un autre utilisateur. Si le
//! client allait chercher tout ce qui ressemble à une image, il suffirait
//! d'écrire `http://192.168.1.1/admin?reboot=1#a.png` pour que **chaque
//! membre du salon** émette silencieusement cette requête depuis son propre
//! réseau local. Le même mécanisme sert de mouchard : une adresse contrôlée
//! par l'auteur révèle l'adresse IP et l'heure de connexion de tous ceux qui
//! affichent le message.
//!
//! On ne télécharge donc que ce qui est hébergé par **le serveur auquel on
//! est connecté** — c'est-à-dire les fichiers passés par le partage de
//! l'application. Tout le reste demeure un lien cliquable, que l'utilisateur
//! ouvre s'il le décide.

use std::collections::HashMap;
use std::io::Read as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::medias::{self, Meta};

/// Poids maximal téléchargé pour un aperçu.
const MAX_BYTES: usize = 12 * 1024 * 1024;
/// Côté maximal admis par le décodeur.
const MAX_PX: u32 = 8_000;
/// Nombre d'aperçus gardés en mémoire.
const MAX_CACHED: usize = 40;
/// Temps accordé à un téléchargement.
const TIMEOUT: Duration = Duration::from_secs(10);
/// Une animation ne dépasse pas ça, toutes images confondues : au-delà, on
/// n'en garde que la première — un GIF de 1080p sur trente secondes serait
/// un gigaoctet de textures.
const MAX_PIXELS_ANIMATION: u64 = 48_000_000;
const MAX_IMAGES_ANIMATION: usize = 400;
/// Une fiche de vidéo qui n'est pas prête se redemande à ce rythme.
const RELANCE_META: Duration = Duration::from_secs(3);

/// Extensions reconnues comme des images.
const IMAGE_SUFFIXES: [&str; 6] = [".png", ".jpg", ".jpeg", ".bmp", ".gif", ".webp"];

/// Vrai si l'adresse désigne une image, d'après son extension.
pub fn looks_like_image(url: &str) -> bool {
    // On coupe une éventuelle requête ou ancre avant de regarder la fin.
    let path = url.split(['?', '#']).next().unwrap_or(url).to_lowercase();
    IMAGE_SUFFIXES.iter().any(|suffix| path.ends_with(suffix))
}

/// Une image animée (GIF, WebP) : ses images et leur durée, en secondes.
pub struct Animation {
    pub images: Vec<(egui::TextureHandle, f32)>,
    pub total: f32,
}

impl Animation {
    /// L'image à montrer à l'instant `temps` (l'horloge de l'interface).
    pub fn image_a(&self, temps: f64) -> &egui::TextureHandle {
        let mut t = (temps % f64::from(self.total.max(0.001))) as f32;
        for (image, duree) in &self.images {
            if t < *duree {
                return image;
            }
            t -= duree;
        }
        &self.images[self.images.len() - 1].0
    }
}

#[derive(Clone)]
pub enum Preview {
    /// Téléchargement en cours.
    Loading,
    Ready(egui::TextureHandle),
    /// Animée : plusieurs images, à faire défiler.
    Anime(Arc<Animation>),
    /// Illisible, trop lourde, ou serveur injoignable.
    Failed,
}

/// Ce que le décodeur rend.
pub enum Decodee {
    Fixe(egui::ColorImage),
    /// Chaque image et sa durée, en secondes.
    Animee(Vec<(egui::ColorImage, f32)>),
}

/// Ce que le fil de téléchargement rend : l'adresse, et l'image **déjà
/// décodée**. `None` = illisible, trop lourde, ou serveur injoignable.
///
/// Le décodage se faisait dans `mount()`, donc sur le fil de l'interface :
/// borné à 8000 px et 64 Mio d'allocation, il pouvait figer la fenêtre
/// plusieurs secondes à l'arrivée d'une photo un peu grande. Le fil qui a
/// téléchargé, lui, n'a plus rien à faire — c'est là que ça se passe.
type Delivery = (String, Option<Decodee>);
/// Une fiche de vidéo lue : l'adresse de la fiche, et son contenu (`None`
/// = pas de fiche, ou serveur injoignable).
type LivraisonMeta = (String, Option<Meta>);

/// La fiche d'une vidéo, telle qu'on la connaît.
#[derive(Clone, Debug)]
pub enum EtatMeta {
    Chargement,
    Prete(Meta),
    /// Le serveur y travaille encore (conversion, poster).
    EnPreparation,
    Erreur(String),
    /// Pas de fiche (un lien d'avant la visionneuse, ou un fichier disparu).
    Absente,
}

#[derive(Default)]
pub struct Previews {
    cache: HashMap<String, Preview>,
    /// Ordre d'arrivée, pour évincer les plus anciennes.
    order: Vec<String>,
    incoming: Arc<Mutex<Vec<Delivery>>>,
    /// Les fiches de vidéos, par adresse de fiche, et l'heure de la lecture.
    metas: HashMap<String, (EtatMeta, Instant)>,
    incoming_metas: Arc<Mutex<Vec<LivraisonMeta>>>,
    /// Racine HTTP du serveur courant : seule origine autorisée.
    origin: Option<String>,
    /// Client HTTP épinglé sur l'empreinte du serveur. Par défaut celui de
    /// `ureq`, remplacé dès qu'on connaît l'empreinte : un aperçu ne doit pas
    /// être l'occasion de parler à quelqu'un d'autre que le serveur.
    agent: Option<ureq::Agent>,
}

impl Previews {
    /// Fixe le client HTTP à utiliser (épinglé sur l'empreinte du serveur).
    pub fn set_agent(&mut self, agent: ureq::Agent) {
        self.agent = Some(agent);
    }

    /// Fixe le serveur dont on accepte les images. Changer de serveur vide
    /// le cache : les adresses d'un autre serveur n'ont plus cours.
    pub fn set_origin(&mut self, origin: String) {
        if self.origin.as_deref() == Some(origin.as_str()) {
            return;
        }
        self.origin = Some(origin);
        self.cache.clear();
        self.order.clear();
        self.metas.clear();
    }

    /// Vrai si cette adresse est servie par notre serveur.
    pub fn is_ours(&self, url: &str) -> bool {
        self.to_pinned(url).is_some()
    }

    /// La même adresse, ramenée en TLS si elle vise notre serveur.
    /// Sert aussi à l'ouverture dans le navigateur.
    pub fn pinned_url(&self, url: &str) -> Option<String> {
        self.to_pinned(url)
    }

    /// Une image servie par notre serveur, désignée par son chemin
    /// (« /musique/vignette/… ») : l'origine est celle de la connexion.
    pub fn chez_nous(&mut self, ctx: &egui::Context, chemin: &str) -> Option<Preview> {
        let origin = self.origin.clone()?;
        self.get(ctx, &format!("{origin}{chemin}"))
    }

    /// L'URL ramenée au serveur courant, en TLS, ou `None` si elle vise
    /// ailleurs.
    ///
    /// Les liens déjà écrits dans l'historique commencent par `http://` : le
    /// partage de fichiers a longtemps été en clair. Les rejeter ferait
    /// disparaître toutes les images déjà partagées, sans un mot. On les
    /// reconnaît donc, et on les récupère en TLS — c'est le même serveur, sur
    /// le même port, et lui seul écoute désormais.
    fn to_pinned(&self, url: &str) -> Option<String> {
        let origin = self.origin.as_ref()?;
        // La barre oblique évite qu'un hôte du genre « monserveur.evil.com »
        // passe pour « monserveur ».
        let prefix = format!("{origin}/");
        if url.starts_with(&prefix) {
            return Some(url.to_string());
        }
        let clear = prefix.replacen("https://", "http://", 1);
        url.starts_with(&clear)
            .then(|| url.replacen("http://", "https://", 1))
    }

    /// Monte en textures les images arrivées depuis le dernier rendu.
    ///
    /// Ne fait plus que téléverser vers le GPU : le décodage a eu lieu sur le
    /// fil de téléchargement.
    pub fn mount(&mut self, ctx: &egui::Context) {
        let arrived = std::mem::take(&mut *self.incoming.lock().unwrap());
        for (url, image) in arrived {
            // Une livraison dont l'entrée a disparu du cache est une livraison
            // orpheline : on a changé de serveur pendant le téléchargement.
            // L'insérer créerait une texture que rien ne compte, donc que rien
            // n'évincera jamais — une fuite de mémoire graphique.
            if !self.cache.contains_key(&url) {
                continue;
            }
            let state = match image {
                Some(Decodee::Fixe(image)) => Preview::Ready(ctx.load_texture(
                    format!("apercu-{url}"),
                    image,
                    egui::TextureOptions::LINEAR,
                )),
                Some(Decodee::Animee(images)) => {
                    let total: f32 = images.iter().map(|(_, d)| *d).sum();
                    let images = images
                        .into_iter()
                        .enumerate()
                        .map(|(i, (image, duree))| {
                            let texture = ctx.load_texture(
                                format!("apercu-{url}-{i}"),
                                image,
                                egui::TextureOptions::LINEAR,
                            );
                            (texture, duree)
                        })
                        .collect();
                    Preview::Anime(Arc::new(Animation { images, total }))
                }
                None => Preview::Failed,
            };
            self.cache.insert(url, state);
        }
        let metas = std::mem::take(&mut *self.incoming_metas.lock().unwrap());
        for (url, meta) in metas {
            let etat = match meta {
                Some(m) if m.prete() => EtatMeta::Prete(m),
                Some(m) if m.en_erreur() => {
                    EtatMeta::Erreur(m.message.unwrap_or_else(|| "vidéo illisible".into()))
                }
                Some(_) => EtatMeta::EnPreparation,
                None => EtatMeta::Absente,
            };
            self.metas.insert(url, (etat, Instant::now()));
        }
    }

    /// État d'un aperçu, en lançant son téléchargement si besoin.
    pub fn get(&mut self, ctx: &egui::Context, url: &str) -> Option<Preview> {
        if !self.is_ours(url) {
            return None;
        }
        if let Some(state) = self.cache.get(url) {
            return Some(state.clone());
        }
        // Rien n'est mis en cache tant qu'on n'a pas de quoi télécharger :
        // marquer « en chargement » sans lancer la requête figerait l'aperçu
        // dans cet état, l'entrée en cache empêchant tout nouvel essai.
        let Some(agent) = self.agent.clone() else {
            return Some(Preview::Loading);
        };
        // Téléchargé en TLS, même si le lien du salon est resté en clair.
        let target = self.to_pinned(url)?;
        if self.order.len() >= MAX_CACHED {
            // On n'évince **jamais** un chargement en vol.
            //
            // C'était le défaut : l'aperçu est demandé pour chaque message du
            // fil, visible ou non, vingt fois par seconde. Passé une
            // quarantaine d'images, l'éviction retirait des entrées encore en
            // `Loading` ; l'image redevenait inconnue, on relançait un
            // `thread::spawn`, qui évinçait à son tour — une tempête de fils
            // et de téléchargements, et des textures que plus rien ne
            // comptait.
            let Some(pos) = self
                .order
                .iter()
                .position(|u| !matches!(self.cache.get(u), Some(Preview::Loading)))
            else {
                // Tout est en cours de chargement : on ne lance rien de plus.
                // Le nombre de fils en vol est ainsi borné par MAX_CACHED, et
                // la demande repassera à la prochaine image sans rien coûter.
                return Some(Preview::Loading);
            };
            let evincee = self.order.remove(pos);
            self.cache.remove(&evincee);
        }
        self.cache.insert(url.to_string(), Preview::Loading);
        self.order.push(url.to_string());
        fetch(
            target,
            url.to_string(),
            self.incoming.clone(),
            ctx.clone(),
            agent,
        );
        Some(Preview::Loading)
    }

    /// La fiche d'une vidéo de notre serveur, en la demandant si besoin —
    /// et en la redemandant tant qu'elle n'est pas prête. `None` : la vidéo
    /// ne vient pas de notre serveur.
    pub fn meta(&mut self, ctx: &egui::Context, url_video: &str) -> Option<EtatMeta> {
        let url = medias::url_meta(url_video)?;
        let target = self.to_pinned(&url)?;
        if let Some((etat, depuis)) = self.metas.get(&url) {
            let definitif = matches!(etat, EtatMeta::Prete(_) | EtatMeta::Erreur(_));
            if definitif || depuis.elapsed() < RELANCE_META {
                return Some(etat.clone());
            }
            if matches!(etat, EtatMeta::Chargement) {
                return Some(etat.clone());
            }
        }
        let Some(agent) = self.agent.clone() else {
            return Some(EtatMeta::Chargement);
        };
        let ancien = self.metas.get(&url).map(|(e, _)| e.clone());
        self.metas
            .insert(url.clone(), (EtatMeta::Chargement, Instant::now()));
        let slot = self.incoming_metas.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let meta = (|| -> Option<Meta> {
                let reponse = agent.get(&target).timeout(TIMEOUT).call().ok()?;
                let mut bytes = Vec::new();
                reponse
                    .into_reader()
                    .take(64 * 1024)
                    .read_to_end(&mut bytes)
                    .ok()?;
                serde_json::from_slice(&bytes).ok()
            })();
            slot.lock().unwrap().push((url, meta));
            ctx.request_repaint();
        });
        // Le temps de la réponse, on montre ce qu'on savait.
        Some(ancien.unwrap_or(EtatMeta::Chargement))
    }
}

/// Télécharge une image, en bornant son poids.
///
/// `target` est l'adresse réellement interrogée, toujours en TLS ; `url` est
/// celle qui figure dans le salon, et qui sert de clé de cache. Les deux ne
/// coïncident pas pour un lien d'avant le passage en TLS.
fn fetch(
    target: String,
    url: String,
    slot: Arc<Mutex<Vec<Delivery>>>,
    ctx: egui::Context,
    agent: ureq::Agent,
) {
    std::thread::spawn(move || {
        let octets = (|| -> Result<Vec<u8>, String> {
            // Agent épinglé sur l'empreinte du serveur : un aperçu ne doit
            // pas être l'occasion de parler à quelqu'un d'autre.
            let response = agent
                .get(&target)
                .timeout(TIMEOUT)
                .call()
                .map_err(|e| e.to_string())?;
            let mut bytes = Vec::new();
            response
                .into_reader()
                .take(MAX_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > MAX_BYTES {
                return Err("image trop lourde".into());
            }
            Ok(bytes)
        })();
        // Le décodage a lieu ICI, et plus dans `mount()` : ce fil a fini son
        // téléchargement et ne fait plus rien, là où le fil de l'interface a
        // une image à peindre dans les seize millisecondes.
        let image = octets.ok().and_then(|bytes| decoder(&bytes));
        slot.lock().unwrap().push((url, image));
        ctx.request_repaint();
    });
}

fn limites() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_PX);
    limits.max_image_height = Some(MAX_PX);
    limits.max_alloc = Some(64 * 1024 * 1024);
    limits
}

/// Décode une image téléchargée, décodeur borné.
///
/// Le format n'est pas imposé — le partage de fichiers accepte ce que
/// l'utilisateur y met — mais les dimensions et l'allocation le sont : une
/// image de quelques kilo-octets peut sinon en réclamer plusieurs
/// gigaoctets au décodage.
pub(crate) fn decode(bytes: &[u8]) -> Option<egui::ColorImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    reader.limits(limites());
    let rgba = reader.decode().ok()?.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(
        size,
        rgba.as_raw(),
    ))
}

/// Décode une image, animée si elle l'est (GIF, WebP) et si elle tient dans
/// les bornes — sinon sa première image seule.
pub(crate) fn decoder(bytes: &[u8]) -> Option<Decodee> {
    let format = image::guess_format(bytes).ok();
    if matches!(
        format,
        Some(image::ImageFormat::Gif | image::ImageFormat::WebP)
    ) {
        if let Some(images) = decoder_animation(bytes, format?) {
            if images.len() > 1 {
                return Some(Decodee::Animee(images));
            }
            if let Some((image, _)) = images.into_iter().next() {
                return Some(Decodee::Fixe(image));
            }
        }
    }
    decode(bytes).map(Decodee::Fixe)
}

fn decoder_animation(
    bytes: &[u8],
    format: image::ImageFormat,
) -> Option<Vec<(egui::ColorImage, f32)>> {
    use image::AnimationDecoder as _;
    let cursor = std::io::Cursor::new(bytes);
    let frames = match format {
        image::ImageFormat::Gif => {
            let mut d = image::codecs::gif::GifDecoder::new(cursor).ok()?;
            image::ImageDecoder::set_limits(&mut d, limites()).ok()?;
            d.into_frames()
        }
        image::ImageFormat::WebP => {
            let mut d = image::codecs::webp::WebPDecoder::new(cursor).ok()?;
            image::ImageDecoder::set_limits(&mut d, limites()).ok()?;
            if !d.has_animation() {
                return None;
            }
            d.into_frames()
        }
        _ => return None,
    };
    let mut images = Vec::new();
    let mut pixels: u64 = 0;
    for frame in frames {
        let frame = frame.ok()?;
        let (num, den) = frame.delay().numer_denom_ms();
        // Une image sans délai déclaré défile à dix par seconde, comme dans
        // les navigateurs.
        let mut duree = if den == 0 {
            0.1
        } else {
            num as f32 / den as f32 / 1000.0
        };
        if duree < 0.02 {
            duree = 0.1;
        }
        let rgba = frame.into_buffer();
        let size = [rgba.width() as usize, rgba.height() as usize];
        pixels += (size[0] * size[1]) as u64;
        if pixels > MAX_PIXELS_ANIMATION || images.len() >= MAX_IMAGES_ANIMATION {
            break;
        }
        images.push((
            egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()),
            duree,
        ));
    }
    (!images.is_empty()).then_some(images)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_extensions_are_recognised() {
        assert!(looks_like_image("http://s/files/ab/photo.png"));
        assert!(looks_like_image("http://s/files/ab/PHOTO.JPG"));
        assert!(looks_like_image("http://s/files/ab/a.jpeg?v=2"));
        assert!(looks_like_image("http://s/files/ab/anim.webp"));
        assert!(!looks_like_image("http://s/files/ab/notes.txt"));
        assert!(!looks_like_image("http://s/files/ab/archive.zip"));
        // Une extension glissée dans l'ancre ne fait pas une image.
        assert!(!looks_like_image("http://192.168.1.1/admin#a.png"));
    }

    #[test]
    fn only_our_own_server_is_fetched() {
        let mut previews = Previews::default();
        // Sans serveur connu, rien n'est téléchargeable.
        assert!(!previews.is_ours("http://127.0.0.1:8080/files/ab/a.png"));

        previews.set_origin("http://127.0.0.1:8080".into());
        assert!(previews.is_ours("http://127.0.0.1:8080/files/ab/a.png"));

        // Autre hôte, autre port, ou hôte qui commence pareil : refusés.
        assert!(!previews.is_ours("http://192.168.1.1/admin.png"));
        assert!(!previews.is_ours("http://127.0.0.1:9999/files/ab/a.png"));
        assert!(!previews.is_ours("http://127.0.0.1:8080.evil.com/a.png"));
        assert!(!previews.is_ours("https://127.0.0.1:8080/files/ab/a.png"));
    }

    #[test]
    fn changing_server_forgets_the_previous_previews() {
        let mut previews = Previews::default();
        previews.set_origin("http://a:8080".into());
        previews
            .cache
            .insert("http://a:8080/x.png".into(), Preview::Failed);
        previews.order.push("http://a:8080/x.png".into());

        // Même serveur : le cache reste.
        previews.set_origin("http://a:8080".into());
        assert_eq!(previews.cache.len(), 1);

        // Serveur différent : on repart à zéro.
        previews.set_origin("http://b:8080".into());
        assert!(previews.cache.is_empty());
        assert!(previews.order.is_empty());
    }

    #[test]
    fn a_video_meta_is_only_asked_for_our_server() {
        let mut previews = Previews::default();
        let ctx = egui::Context::default();
        assert!(previews
            .meta(&ctx, "https://x:8080/files/ab/clip.mp4")
            .is_none());
        previews.set_origin("https://x:8080".into());
        // Sans agent : « en chargement », sans rien lancer.
        assert!(matches!(
            previews.meta(&ctx, "https://x:8080/files/ab/clip.mp4"),
            Some(EtatMeta::Chargement)
        ));
        assert!(previews
            .meta(&ctx, "https://ailleurs:8080/files/ab/clip.mp4")
            .is_none());
    }

    /// Un GIF de deux images (2×2 px), écrit à la main : assez pour prouver
    /// que l'animation est reconnue et que les durées sont lues.
    #[test]
    fn an_animated_gif_yields_its_frames() {
        let mut gif = Vec::new();
        {
            let mut enc = gif_encoder(&mut gif);
            for couleur in [[255u8, 0, 0, 255], [0, 0, 255, 255]] {
                let pixels = couleur.repeat(4);
                let tampon = image::RgbaImage::from_raw(2, 2, pixels).unwrap();
                let frame = image::Frame::from_parts(
                    tampon,
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(200, 1),
                );
                enc.encode_frame(frame).unwrap();
            }
        }
        match decoder(&gif) {
            Some(Decodee::Animee(images)) => {
                assert_eq!(images.len(), 2);
                assert!((images[0].1 - 0.2).abs() < 1e-3);
                assert_eq!(images[0].0.pixels[0], egui::Color32::from_rgb(255, 0, 0));
                assert_eq!(images[1].0.pixels[0], egui::Color32::from_rgb(0, 0, 255));
            }
            _ => panic!("animation attendue"),
        }
    }

    fn gif_encoder(sortie: &mut Vec<u8>) -> image::codecs::gif::GifEncoder<&mut Vec<u8>> {
        let mut enc = image::codecs::gif::GifEncoder::new(sortie);
        enc.set_repeat(image::codecs::gif::Repeat::Infinite)
            .unwrap();
        enc
    }

    #[test]
    fn the_animation_clock_wraps_around() {
        let ctx = egui::Context::default();
        let img = |c: egui::Color32| egui::ColorImage::new([1, 1], vec![c]);
        let a = ctx.load_texture("a", img(egui::Color32::RED), egui::TextureOptions::LINEAR);
        let b = ctx.load_texture("b", img(egui::Color32::BLUE), egui::TextureOptions::LINEAR);
        let anim = Animation {
            images: vec![(a.clone(), 0.5), (b.clone(), 0.25)],
            total: 0.75,
        };
        assert_eq!(anim.image_a(0.1).id(), a.id());
        assert_eq!(anim.image_a(0.6).id(), b.id());
        assert_eq!(anim.image_a(0.8).id(), a.id());
    }
}
