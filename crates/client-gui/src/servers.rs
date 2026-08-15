//! Carnet de serveurs : la liste persistée des serveurs connus, et la sonde
//! qui mesure leur disponibilité **avant** de s'y connecter.
//!
//! La sonde ouvre une simple poignée de main QUIC puis referme. Le serveur
//! attend un message `Auth` pendant 10 s avant de laisser tomber la
//! connexion : une connexion qui se referme aussitôt ne crée aucun état de
//! son côté, et le RTT mesuré par QUIC pendant la poignée de main est
//! exactement le ping affiché une fois connecté.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use serde::{Deserialize, Serialize};

/// Un serveur du carnet.
#[derive(Clone, Serialize, Deserialize)]
pub struct Server {
    pub id: u64,
    /// Alias local (« Chez Kevin »), libre et facultatif. Il prime sur le
    /// nom annoncé par le serveur, mais ne concerne que cette machine.
    pub name: String,
    /// Hôte, ou hôte:port (9987 par défaut).
    pub address: String,
    #[serde(default)]
    pub username: String,
    /// Mot de passe mémorisé, chiffré par le coffre natif de la plateforme
    /// (voir [`crate::secret`]). Illisible sur une autre machine ou sous un
    /// autre compte, même si le fichier est copié.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    /// Ancien champ des versions précédentes : le mot de passe y était écrit
    /// **en clair**. Il est relu une fois pour être chiffré, puis disparaît
    /// du fichier à la première sauvegarde.
    #[serde(default, rename = "password", skip_serializing_if = "Option::is_none")]
    pub legacy_password: Option<String>,
    /// Dernière connexion réussie (ms epoch), 0 = jamais.
    #[serde(default)]
    pub last_used: u64,
    /// Nom que le serveur s'est donné, mémorisé à la dernière connexion.
    /// Le client ne fait que le recopier — il ne se règle pas ici.
    #[serde(default)]
    pub server_name: String,
    /// Logo **du serveur** : vignette PNG base64, reçue à la connexion et
    /// mise en cache ici pour que le lanceur puisse l'afficher hors ligne.
    ///
    /// Volontairement non modifiable depuis le client : le logo identifie
    /// le serveur, le laisser changer localement ouvrirait la porte à des
    /// confusions entre serveurs. Seul un admin le change, côté serveur.
    #[serde(default)]
    pub icon: Option<String>,
}

impl Server {
    /// Ce qu'on affiche : l'alias local s'il existe, sinon le nom annoncé
    /// par le serveur, sinon l'adresse.
    pub fn label(&self) -> &str {
        for candidate in [self.name.trim(), self.server_name.trim()] {
            if !candidate.is_empty() {
                return candidate;
            }
        }
        self.address.trim()
    }

    /// Vrai si le libellé affiché est autre chose que l'adresse : dans ce
    /// cas la ligne peut montrer les deux.
    pub fn has_label(&self) -> bool {
        !self.name.trim().is_empty() || !self.server_name.trim().is_empty()
    }

    /// Mot de passe mémorisé, déchiffré. `None` si rien n'est mémorisé, ou
    /// si le secret vient d'ailleurs — on redemandera, simplement.
    pub fn password(&self) -> Option<String> {
        self.secret.as_deref().and_then(crate::secret::reveal)
    }

    /// Mémorise (ou oublie) le mot de passe, chiffré pour cette machine.
    /// Si le coffre refuse, rien n'est écrit : pas de repli en clair.
    pub fn set_password(&mut self, password: Option<&str>) {
        self.legacy_password = None;
        self.secret = password.and_then(|p| crate::secret::protect(p).ok());
    }

    /// Reprend un mot de passe écrit en clair par une version précédente et
    /// le met sous clé. Le champ en clair est vidé dans tous les cas — s'il
    /// n'a pas pu être chiffré, on préfère l'oublier que le laisser traîner.
    fn migrate_secret(&mut self) {
        let Some(plaintext) = self.legacy_password.take() else { return };
        if self.secret.is_none() {
            self.secret = crate::secret::protect(&plaintext).ok();
        }
    }
}

const KEY: &str = "servers";

/// Charge le carnet, en reprenant au besoin l'unique adresse mémorisée par
/// les versions précédentes du client.
pub fn load(storage: Option<&dyn eframe::Storage>) -> Vec<Server> {
    let Some(storage) = storage else { return Vec::new() };
    let stored = storage
        .get_string(KEY)
        .and_then(|json| serde_json::from_str::<Vec<Server>>(&json).ok());
    if let Some(mut servers) = stored {
        // Les mots de passe en clair des versions précédentes passent sous
        // clé dès le chargement.
        for server in &mut servers {
            server.migrate_secret();
        }
        return servers;
    }

    // Migration depuis l'ancien format (une seule adresse, pas de nom).
    let address = storage.get_string("url").unwrap_or_default();
    if address.trim().is_empty() || address.starts_with("ws") {
        return Vec::new();
    }
    vec![Server {
        id: 1,
        name: String::new(),
        address,
        username: storage.get_string("username").unwrap_or_default(),
        secret: None,
        legacy_password: None,
        last_used: 0,
        server_name: String::new(),
        icon: None,
    }]
}

pub fn save(storage: &mut dyn eframe::Storage, servers: &[Server]) {
    if let Ok(json) = serde_json::to_string(servers) {
        storage.set_string(KEY, json);
    }
}

/// Identifiant libre pour un nouveau serveur.
pub fn next_id(servers: &[Server]) -> u64 {
    servers.iter().map(|s| s.id).max().unwrap_or(0) + 1
}

// ---------------------------------------------------------------------
// Logos
// ---------------------------------------------------------------------

/// Côté de la vignette stockée, en pixels.
const ICON_PX: u32 = 64;
/// Rayon des coins, en fraction du côté.
const ICON_ROUNDING: f32 = 0.28;

/// Lit une image du disque et en fait une vignette carrée aux coins
/// arrondis, encodée en PNG puis en base64.
///
/// Les coins sont détourés ici plutôt qu'au rendu : egui dessine une image
/// dans un rectangle net, sans rayon — en cuisant le masque dans la
/// texture, le logo est arrondi partout où il est affiché.
pub fn encode_icon(path: &std::path::Path) -> anyhow::Result<String> {
    use base64::Engine as _;

    // Le fichier vient de l'utilisateur lui-même, mais un PNG malformé ne
    // doit pas pouvoir faire tomber l'application : le décodeur est borné.
    // Large, pour accepter une vraie photo — c'est nous qui la réduirons.
    let mut reader = image::ImageReader::open(path)?.with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(12_000);
    limits.max_image_height = Some(12_000);
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let source = reader.decode()?;
    let mut thumb = source
        .resize_to_fill(ICON_PX, ICON_PX, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    round_corners(&mut thumb);

    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(thumb)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(png))
}

/// Décode une vignette stockée (base64) vers une image egui.
pub fn decode_icon(data: &str) -> Option<egui::ColorImage> {
    decode_png(&decode_base64(data)?)
}

/// Décode le base64 d'une vignette vers les octets PNG bruts — ce qu'on
/// range dans le cache disque, un tiers plus compact que le base64.
pub fn decode_base64(data: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

/// Décode des octets PNG vers une image egui.
///
/// Deux garde-fous avant de laisser travailler le décodeur : l'en-tête est
/// contrôlé (c'est bien un petit PNG), puis le décodeur lui-même est borné.
/// Une vignette hostile ne peut donc pas réclamer des gigaoctets — ce qui
/// compte d'autant plus que le binaire de production est compilé en
/// `panic = "abort"` : un plantage du décodeur ne serait pas rattrapable.
pub fn decode_png(bytes: &[u8]) -> Option<egui::ColorImage> {
    ki_protocol::check_png(bytes).ok()?;

    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(ki_protocol::MAX_THUMBNAIL_PX);
    limits.max_image_height = Some(ki_protocol::MAX_THUMBNAIL_PX);
    limits.max_alloc = Some(4 * 1024 * 1024);
    reader.limits(limits);

    let rgba = reader.decode().ok()?.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}

/// Applique un masque de coins arrondis sur le canal alpha.
fn round_corners(image: &mut image::RgbaImage) {
    let (w, h) = (image.width() as f32, image.height() as f32);
    let radius = w.min(h) * ICON_ROUNDING;
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let coverage = rounded_coverage(x as f32 + 0.5, y as f32 + 0.5, w, h, radius);
        pixel[3] = (pixel[3] as f32 * coverage).round() as u8;
    }
}

/// Couverture antialiasée d'un pixel par un rectangle arrondi 0,0..w,h.
fn rounded_coverage(x: f32, y: f32, w: f32, h: f32, radius: f32) -> f32 {
    let (hw, hh) = (w / 2.0, h / 2.0);
    let radius = radius.min(hw).min(hh);
    let dx = (x - hw).abs() - (hw - radius);
    let dy = (y - hh).abs() - (hh - radius);
    let outside = dx.max(0.0).hypot(dy.max(0.0));
    let inside = dx.max(dy).min(0.0);
    (0.5 - (outside + inside - radius)).clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------
// Sonde
// ---------------------------------------------------------------------

/// État de joignabilité d'un serveur.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Jamais testé.
    Unknown,
    Probing,
    Online { ping_ms: u32 },
    Offline,
}

/// Temps accordé à une poignée de main de test.
const TIMEOUT: Duration = Duration::from_secs(4);
/// Période de rafraîchissement automatique, tant qu'on est sur le lanceur.
const PERIOD: Duration = Duration::from_secs(20);

#[derive(Default)]
pub struct Probes {
    state: Arc<Mutex<HashMap<u64, Reach>>>,
    last_sweep: Mutex<Option<Instant>>,
}

impl Probes {
    pub fn reach(&self, id: u64) -> Reach {
        self.state.lock().unwrap().get(&id).copied().unwrap_or(Reach::Unknown)
    }

    /// Teste tous les serveurs, au plus une fois par `PERIOD`.
    /// `force` court-circuite le délai (bouton « actualiser »).
    pub fn sweep(&self, servers: &[Server], ctx: &egui::Context, force: bool) {
        {
            let mut last = self.last_sweep.lock().unwrap();
            if !force && last.is_some_and(|t| t.elapsed() < PERIOD) {
                return;
            }
            *last = Some(Instant::now());
        }
        for server in servers {
            self.probe(server, ctx);
        }
    }

    /// Relance la mesure d'un serveur — sans effet si elle est déjà en cours.
    pub fn probe(&self, server: &Server, ctx: &egui::Context) {
        let address = server.address.trim().to_string();
        if address.is_empty() {
            return;
        }
        let id = server.id;
        {
            let mut state = self.state.lock().unwrap();
            if state.get(&id) == Some(&Reach::Probing) {
                return;
            }
            state.insert(id, Reach::Probing);
        }

        let state = self.state.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let reach = measure(&address);
            state.lock().unwrap().insert(id, reach);
            ctx.request_repaint();
        });
    }

    /// Oublie un serveur supprimé.
    pub fn forget(&self, id: u64) {
        self.state.lock().unwrap().remove(&id);
    }
}

/// Poignée de main QUIC de test, puis fermeture immédiate.
pub(crate) fn measure(address: &str) -> Reach {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
    let Ok(runtime) = runtime else { return Reach::Offline };
    runtime.block_on(async {
        let connect = ki_client_quic::QuicClient::connect(address);
        match tokio::time::timeout(TIMEOUT, connect).await {
            Ok(Ok(client)) => {
                let rtt = client.conn.rtt();
                client.conn.close(0u32.into(), b"probe");
                // Laisse partir le paquet de fermeture avant de tout jeter,
                // sinon le serveur garde la connexion jusqu'à son délai.
                tokio::time::sleep(Duration::from_millis(60)).await;
                Reach::Online { ping_ms: rtt.as_millis().min(9_999) as u32 }
            }
            _ => Reach::Offline,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeStorage(HashMap<String, String>);

    impl FakeStorage {
        fn with(pairs: &[(&str, &str)]) -> Self {
            let mut map = HashMap::new();
            for (k, v) in pairs {
                map.insert((*k).to_string(), (*v).to_string());
            }
            Self(map)
        }
    }

    impl eframe::Storage for FakeStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.to_owned(), value);
        }
        fn flush(&mut self) {}
    }

    #[test]
    fn migrates_the_single_address_of_older_clients() {
        let storage = FakeStorage::with(&[("url", "chez-kevin.fr"), ("username", "redik")]);
        let book = load(Some(&storage));
        assert_eq!(book.len(), 1);
        assert_eq!(book[0].address, "chez-kevin.fr");
        assert_eq!(book[0].username, "redik");
        // On ne devine pas un mot de passe qui n'a jamais été mémorisé.
        assert!(book[0].secret.is_none());
    }

    #[test]
    fn ignores_the_obsolete_websocket_address() {
        let storage = FakeStorage::with(&[("url", "ws://vieux-serveur/ws")]);
        assert!(load(Some(&storage)).is_empty());
    }

    #[test]
    fn round_trip_keeps_every_field() {
        let mut storage = FakeStorage::default();
        let book = vec![Server {
            id: 7,
            name: "Chez Kévin".into(),
            address: "kevin.fr:9987".into(),
            username: "redik".into(),
            secret: None,
            legacy_password: None,
            last_used: 42,
            server_name: "Chez Kévin (officiel)".into(),
            icon: None,
        }];
        save(&mut storage, &book);
        let read = load(Some(&storage));
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].id, 7);
        assert_eq!(read[0].name, "Chez Kévin");
        assert_eq!(read[0].server_name, "Chez Kévin (officiel)");

        assert_eq!(read[0].last_used, 42);
    }

    /// Un fichier écrit par une version précédente contient le mot de passe
    /// en clair : il doit être mis sous clé au chargement, et le champ en
    /// clair doit disparaître à la sauvegarde suivante.
    #[test]
    fn a_plaintext_password_is_sealed_on_load() {
        let legacy = r#"[{"id":1,"name":"","address":"127.0.0.1",
                          "username":"redik","password":"tres-secret",
                          "last_used":7}]"#;
        let mut storage = FakeStorage::with(&[("servers", legacy)]);

        let book = load(Some(&storage));
        assert_eq!(book.len(), 1);
        // Le champ en clair est vidé quoi qu'il arrive.
        assert!(book[0].legacy_password.is_none());

        if crate::secret::available() {
            // Chiffré, et relisible sur cette machine.
            assert!(book[0].secret.is_some());
            assert_eq!(book[0].password().as_deref(), Some("tres-secret"));
        }

        // Après réécriture, le mot de passe en clair n'est plus nulle part.
        save(&mut storage, &book);
        let written = eframe::Storage::get_string(&storage, "servers").unwrap();
        assert!(!written.contains("tres-secret"));
        assert!(!written.contains("\"password\""));
    }

    #[test]
    fn a_secret_from_another_machine_is_simply_forgotten() {
        let foreign = r#"[{"id":1,"name":"","address":"127.0.0.1",
                           "username":"redik","secret":"keychain:AAAA"}]"#;
        let storage = FakeStorage::with(&[("servers", foreign)]);
        let book = load(Some(&storage));
        // Le blob est conservé tel quel, mais illisible ici : on redemande.
        assert!(book[0].password().is_none());
    }

    #[test]
    fn label_falls_back_to_the_address() {
        let mut server = Server {
            id: 1,
            name: "  ".into(),
            address: "127.0.0.1".into(),
            username: String::new(),
            secret: None,
            legacy_password: None,
            last_used: 0,
            server_name: String::new(),
            icon: None,
        };
        assert_eq!(server.label(), "127.0.0.1");
        assert!(!server.has_label());

        // Le nom annoncé par le serveur prend le relais de l'adresse...
        server.server_name = "Le serveur de Kévin".into();
        assert_eq!(server.label(), "Le serveur de Kévin");
        assert!(server.has_label());

        // ...et l'alias local prime sur les deux.
        server.name = "Chez moi".into();
        assert_eq!(server.label(), "Chez moi");
    }

    fn stub(id: u64) -> Server {
        Server {
            id,
            name: String::new(),
            address: "127.0.0.1".into(),
            username: String::new(),
            secret: None,
            legacy_password: None,
            last_used: 0,
            server_name: String::new(),
            icon: None,
        }
    }

    #[test]
    fn next_id_never_collides() {
        assert_eq!(next_id(&[]), 1);
        assert_eq!(next_id(&[stub(3), stub(9)]), 10);
    }

    #[test]
    fn icon_survives_a_round_trip_and_gets_rounded_corners() {
        // Une image rouge opaque de 200×120 : après vignettage elle doit
        // devenir un carré de 64×64 aux coins transparents.
        let source = image::RgbaImage::from_pixel(200, 120, image::Rgba([220, 40, 40, 255]));
        let path = std::env::temp_dir().join("ki-chat-test-icon.png");
        source.save(&path).expect("écriture de l'image de test");

        let encoded = encode_icon(&path).expect("encodage");
        let decoded = decode_icon(&encoded).expect("décodage");
        assert_eq!(decoded.size, [ICON_PX as usize, ICON_PX as usize]);

        // Le centre reste opaque, le coin est détouré.
        let at = |x: usize, y: usize| decoded.pixels[y * ICON_PX as usize + x];
        assert_eq!(at(32, 32).a(), 255);
        assert_eq!(at(0, 0).a(), 0);

        let _ = std::fs::remove_file(&path);
    }

    /// Garde-fou croisé : ce que produit notre encodeur doit passer la
    /// validation stricte du protocole. Si l'un des deux bouge, ce test
    /// casse avant que les vignettes ne soient refusées en production.
    #[test]
    fn our_own_thumbnails_pass_the_strict_validation() {
        let source = image::RgbaImage::from_pixel(300, 180, image::Rgba([12, 210, 106, 255]));
        let path = std::env::temp_dir().join("ki-chat-test-strict.png");
        source.save(&path).expect("écriture de l'image de test");

        let encoded = encode_icon(&path).expect("encodage");
        // Telle qu'elle voyagera sur le réseau, en base64.
        ki_protocol::check_thumbnail(&encoded).expect("vignette refusée par le protocole");
        // Et telle qu'elle sera rangée dans le cache disque.
        let png = decode_base64(&encoded).expect("base64");
        ki_protocol::check_png(&png).expect("PNG refusé par le protocole");
        assert!(decode_png(&png).is_some(), "vignette illisible après contrôle");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_corrupt_icon_is_ignored_rather_than_fatal() {
        assert!(decode_icon("pas du base64 !").is_none());
        assert!(decode_icon("AAAA").is_none());
    }

    /// La sonde doit conclure « injoignable » plutôt que rester bloquée :
    /// 10.255.255.1 est une adresse privée non routée.
    #[test]
    fn unreachable_address_reports_offline() {
        let started = Instant::now();
        assert!(matches!(measure("10.255.255.1:9987"), Reach::Offline));
        assert!(started.elapsed() < TIMEOUT + Duration::from_secs(2));
    }
}
