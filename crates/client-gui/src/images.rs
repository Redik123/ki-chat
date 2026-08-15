//! Aperçu des images partagées dans le fil de discussion.
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
use std::time::Duration;

use eframe::egui;

/// Poids maximal téléchargé pour un aperçu.
const MAX_BYTES: usize = 12 * 1024 * 1024;
/// Côté maximal admis par le décodeur.
const MAX_PX: u32 = 8_000;
/// Nombre d'aperçus gardés en mémoire.
const MAX_CACHED: usize = 40;
/// Temps accordé à un téléchargement.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Extensions reconnues comme des images.
const IMAGE_SUFFIXES: [&str; 5] = [".png", ".jpg", ".jpeg", ".bmp", ".gif"];

/// Vrai si l'adresse désigne une image, d'après son extension.
pub fn looks_like_image(url: &str) -> bool {
    // On coupe une éventuelle requête ou ancre avant de regarder la fin.
    let path = url.split(['?', '#']).next().unwrap_or(url).to_lowercase();
    IMAGE_SUFFIXES.iter().any(|suffix| path.ends_with(suffix))
}

#[derive(Clone)]
pub enum Preview {
    /// Téléchargement en cours.
    Loading,
    Ready(egui::TextureHandle),
    /// Illisible, trop lourde, ou serveur injoignable.
    Failed,
}

type Delivery = (String, Result<Vec<u8>, String>);

#[derive(Default)]
pub struct Previews {
    cache: HashMap<String, Preview>,
    /// Ordre d'arrivée, pour évincer les plus anciennes.
    order: Vec<String>,
    incoming: Arc<Mutex<Vec<Delivery>>>,
    /// Racine HTTP du serveur courant : seule origine autorisée.
    origin: Option<String>,
}

impl Previews {
    /// Fixe le serveur dont on accepte les images. Changer de serveur vide
    /// le cache : les adresses d'un autre serveur n'ont plus cours.
    pub fn set_origin(&mut self, origin: String) {
        if self.origin.as_deref() == Some(origin.as_str()) {
            return;
        }
        self.origin = Some(origin);
        self.cache.clear();
        self.order.clear();
    }

    /// Vrai si cette adresse est servie par notre serveur.
    fn is_ours(&self, url: &str) -> bool {
        let Some(origin) = &self.origin else { return false };
        // La barre oblique évite qu'un hôte du genre « monserveur.evil.com »
        // passe pour « monserveur ».
        url.starts_with(&format!("{origin}/"))
    }

    /// Monte en textures les images arrivées depuis le dernier rendu.
    pub fn mount(&mut self, ctx: &egui::Context) {
        let arrived = std::mem::take(&mut *self.incoming.lock().unwrap());
        for (url, outcome) in arrived {
            let state = match outcome.ok().and_then(|bytes| decode(&bytes)) {
                Some(image) => {
                    let texture = ctx.load_texture(
                        format!("apercu-{url}"),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    Preview::Ready(texture)
                }
                None => Preview::Failed,
            };
            self.cache.insert(url, state);
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
        if self.order.len() >= MAX_CACHED {
            if let Some(oldest) = self.order.first().cloned() {
                self.order.remove(0);
                self.cache.remove(&oldest);
            }
        }
        self.cache.insert(url.to_string(), Preview::Loading);
        self.order.push(url.to_string());
        fetch(url.to_string(), self.incoming.clone(), ctx.clone());
        Some(Preview::Loading)
    }
}

/// Télécharge une image, en bornant son poids.
fn fetch(url: String, slot: Arc<Mutex<Vec<Delivery>>>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let outcome = (|| -> Result<Vec<u8>, String> {
            let response = ureq::get(&url).timeout(TIMEOUT).call().map_err(|e| e.to_string())?;
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
        slot.lock().unwrap().push((url, outcome));
        ctx.request_repaint();
    });
}

/// Décode une image téléchargée, décodeur borné.
///
/// Le format n'est pas imposé — le partage de fichiers accepte ce que
/// l'utilisateur y met — mais les dimensions et l'allocation le sont : une
/// image de quelques kilo-octets peut sinon en réclamer plusieurs
/// gigaoctets au décodage.
fn decode(bytes: &[u8]) -> Option<egui::ColorImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_PX);
    limits.max_image_height = Some(MAX_PX);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);

    let rgba = reader.decode().ok()?.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_extensions_are_recognised() {
        assert!(looks_like_image("http://s/files/ab/photo.png"));
        assert!(looks_like_image("http://s/files/ab/PHOTO.JPG"));
        assert!(looks_like_image("http://s/files/ab/a.jpeg?v=2"));
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
        previews.cache.insert("http://a:8080/x.png".into(), Preview::Failed);
        previews.order.push("http://a:8080/x.png".into());

        // Même serveur : le cache reste.
        previews.set_origin("http://a:8080".into());
        assert_eq!(previews.cache.len(), 1);

        // Serveur différent : on repart à zéro.
        previews.set_origin("http://b:8080".into());
        assert!(previews.cache.is_empty());
        assert!(previews.order.is_empty());
    }
}
