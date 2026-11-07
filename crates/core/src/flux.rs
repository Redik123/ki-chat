//! Les trames d'un partage d'écran, sans décodeur ni encodeur : les sceller pour
//! les envoyer, les ouvrir à la réception, et les remettre en ordre.
//!
//! Utilisé par l'appli mobile, dont le matériel fait le reste. Le format est celui
//! du PC ([`ki_protocol::MediaHeader`]) : un téléphone regarde un PC, et l'inverse.
//! Les règles de [`Lecture`] sont celles du fil décodeur de `client-gui/partage.rs`.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ki_client_quic::quinn;
use ki_protocol::MediaHeader;
use tokio::sync::mpsc as tokio_mpsc;

/// Les trames reçues en attente du fil décodeur, au plus : 3 s à 30 i/s.
pub const FILE_TRAMES_MAX: usize = 90;
/// De même pour le son du stream : 3 s de datagrammes.
pub const FILE_SON_MAX: usize = 150;
/// Au-delà de tant de trames en attente derrière un trou, on cesse d'espérer.
const ATTENTE_MAX: usize = 30;
/// Plus de retard que ça à décoder derrière la lecture : on saute à la dernière
/// trame clé en attente, l'image saute d'un coup au lieu que le retard grossisse.
const RETARD_MAX_MS: f64 = 330.0;

/// Une trame déchiffrée.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trame {
    pub seq: u64,
    pub idr: bool,
    pub pts_us: u64,
    pub largeur: u16,
    pub hauteur: u16,
    pub donnees: Vec<u8>,
}

/// Déchiffre les trames d'un stream, avec la clé remise par `WatchAccepted`.
pub struct Ouvreur {
    stream_id: u32,
    cipher: XChaCha20Poly1305,
}

impl Ouvreur {
    pub fn new(stream_id: u32, cle: [u8; 32]) -> Self {
        Self { stream_id, cipher: XChaCha20Poly1305::new(&cle.into()) }
    }

    /// La trame et sa qualité (vrai : la basse). `None` : d'un autre stream,
    /// tronquée, ou altérée en route (l'en-tête est l'AAD, le tag le trahit).
    pub fn ouvrir(&self, octets: &[u8]) -> Option<(Trame, bool)> {
        let h = ki_protocol::parse_media_header(octets)?;
        if h.stream_id != self.stream_id {
            return None;
        }
        let domaine = if h.basse { ki_protocol::MEDIA_DOMAIN_VIDEO_BASSE } else { ki_protocol::MEDIA_DOMAIN_VIDEO };
        let nonce = ki_protocol::nonce_for_media(domaine, self.stream_id, h.seq);
        let (aad, scelle) = octets.split_at(ki_protocol::MEDIA_HEADER_LEN);
        let donnees = self.cipher.decrypt(&XNonce::from(nonce), Payload { msg: scelle, aad }).ok()?;
        Some((
            Trame { seq: h.seq, idr: h.idr, pts_us: h.pts_us, largeur: h.width, hauteur: h.height, donnees },
            h.basse,
        ))
    }
}

impl Ouvreur {
    /// Un datagramme de son du stream : sa séquence, sa date et l'Opus déchiffré.
    pub fn ouvrir_son(&self, datagramme: &[u8]) -> Option<(u64, u64, Vec<u8>)> {
        let h = ki_protocol::parse_audio_header(datagramme)?;
        if h.stream_id != self.stream_id {
            return None;
        }
        let nonce = ki_protocol::nonce_for_media(ki_protocol::MEDIA_DOMAIN_GAME_AUDIO, self.stream_id, h.seq);
        let (aad, scelle) = datagramme.split_at(ki_protocol::AUDIO_HEADER_LEN);
        let opus = self.cipher.decrypt(&XNonce::from(nonce), Payload { msg: scelle, aad }).ok()?;
        Some((h.seq, h.pts_us, opus))
    }
}

/// Chiffre les trames d'un stream qu'on diffuse, en qualité haute (la seule d'un
/// téléphone) : séquence jamais réinitialisée, c'est elle qui fait le nonce.
pub struct Scelleur {
    stream_id: u32,
    cipher: XChaCha20Poly1305,
    seq: u64,
    groupe: u32,
}

impl Scelleur {
    pub fn new(stream_id: u32, cle: [u8; 32]) -> Self {
        Self { stream_id, cipher: XChaCha20Poly1305::new(&cle.into()), seq: 0, groupe: 0 }
    }

    /// La séquence et les octets à envoyer : en-tête en clair, charge chiffrée.
    pub fn sceller(
        &mut self,
        donnees: &[u8],
        idr: bool,
        pts_us: u64,
        largeur: u16,
        hauteur: u16,
    ) -> Option<(u64, Vec<u8>)> {
        let seq = self.seq;
        self.seq += 1;
        if idr {
            self.groupe += 1;
        }
        let header = MediaHeader {
            idr,
            basse: false,
            stream_id: self.stream_id,
            seq,
            pts_us,
            group_id: self.groupe,
            width: largeur,
            height: hauteur,
        };
        let mut tete = [0u8; ki_protocol::MEDIA_HEADER_LEN];
        ki_protocol::write_media_header(&mut tete, &header);
        let nonce = ki_protocol::nonce_for_media(ki_protocol::MEDIA_DOMAIN_VIDEO, self.stream_id, seq);
        let scelle = self.cipher.encrypt(&XNonce::from(nonce), Payload { msg: donnees, aad: &tete }).ok()?;
        let mut octets = Vec::with_capacity(tete.len() + scelle.len());
        octets.extend_from_slice(&tete);
        octets.extend_from_slice(&scelle);
        Some((seq, octets))
    }
}

/// La tâche d'envoi d'une qualité : chaque trame dans SON flux QUIC
/// unidirectionnel, le plus ancien d'abord — la priorité décroît avec l'âge, en
/// saturant (cf. le relais, même arithmétique). File de deux trames : au-delà,
/// l'émetteur jette à la source.
pub fn lancer_envoi(
    rt: &tokio::runtime::Handle,
    conn: quinn::Connection,
) -> tokio_mpsc::Sender<(u64, Vec<u8>)> {
    let (tx, mut rx) = tokio_mpsc::channel::<(u64, Vec<u8>)>(2);
    rt.spawn(async move {
        while let Some((seq, bytes)) = rx.recv().await {
            let Ok(mut flux) = conn.open_uni().await else {
                return;
            };
            let _ = flux.set_priority(0i32.saturating_sub(seq.min(i32::MAX as u64) as i32));
            if flux.write_all(&bytes).await.is_err() {
                return;
            }
            let _ = flux.finish();
        }
    });
    tx
}

type Attente = BTreeMap<u64, Trame>;

/// La remise en ordre côté spectateur.
///
/// Les trames arrivent dans le désordre — un flux QUIC chacune, les petites
/// doublent les grosses. La lecture ne démarre qu'à une trame clé, puis avance
/// strictement en séquence ; une trame clé au-delà d'un trou y fait sauter tout de
/// suite. Deux qualités, deux séquences : elles ne se mélangent jamais, et le
/// serveur nous fait passer de l'une à l'autre par une trame clé.
#[derive(Default)]
pub struct Lecture {
    attentes: [Attente; 2],
    prochaine: Option<u64>,
    couche: usize,
    lu_pts: Option<u64>,
}

/// La trame clé de l'autre qualité où passer : la première plus récente que ce
/// qu'on a déjà lu. Une plus ancienne, croisée en route, ne ramène pas en arrière.
fn cle_de_bascule(autre: &Attente, lu_pts: Option<u64>) -> Option<u64> {
    autre.iter().find(|(_, t)| t.idr && lu_pts.is_none_or(|l| t.pts_us > l)).map(|(s, _)| *s)
}

/// La trame clé où sauter quand le décodage ne suit pas : la dernière en attente
/// après `next`, si décoder tout ce qui attend prendrait plus de [`RETARD_MAX_MS`].
fn saut_de_retard(attente: &Attente, next: u64, decodage_moyen_ms: f64) -> Option<u64> {
    if attente.range(next..).count() as f64 * decodage_moyen_ms <= RETARD_MAX_MS {
        return None;
    }
    attente.iter().rev().find(|(k, t)| **k > next && t.idr).map(|(s, _)| *s)
}

impl Lecture {
    /// Une trame de plus, de la qualité dite.
    pub fn recevoir(&mut self, trame: Trame, basse: bool) {
        self.attentes[usize::from(basse)].insert(trame.seq, trame);
    }

    /// Lit-on la qualité basse ? Le serveur nous y a mis, la connexion ne suivait pas.
    pub fn basse(&self) -> bool {
        self.couche == 1
    }

    /// Attend-on une trame clé pour (re)démarrer ?
    pub fn attend_une_cle(&self) -> bool {
        self.prochaine.is_none()
    }

    /// Repart de zéro : le décodeur a disparu, seule une trame clé peut relancer.
    pub fn repartir(&mut self) {
        self.attentes[0].clear();
        self.attentes[1].clear();
        self.prochaine = None;
    }

    /// Ce qui est prêt à partir au décodeur, dans l'ordre.
    pub fn a_decoder(&mut self, decodage_moyen_ms: f64) -> Vec<Trame> {
        let autre = 1 - self.couche;
        if let Some(s) = cle_de_bascule(&self.attentes[autre], self.lu_pts) {
            self.couche = autre;
            self.attentes[1 - autre].clear();
            self.attentes[autre].retain(|k, _| *k >= s);
            self.prochaine = Some(s);
        } else if self.attentes[autre].len() > 2 * ATTENTE_MAX {
            self.attentes[autre].clear();
        }
        let attente = &mut self.attentes[self.couche];

        // Le départ : la première trame clé vue. Ce qui la précède est indécodable.
        if self.prochaine.is_none() {
            if let Some(s) = attente.iter().find(|(_, t)| t.idr).map(|(s, _)| *s) {
                attente.retain(|k, _| *k >= s);
                self.prochaine = Some(s);
            } else if attente.len() > 2 * ATTENTE_MAX {
                attente.clear();
            }
        }
        if let Some(s) = self.prochaine.and_then(|next| saut_de_retard(attente, next, decodage_moyen_ms)) {
            attente.retain(|k, _| *k >= s);
            self.prochaine = Some(s);
        }

        let mut sortie = Vec::new();
        let Some(mut next) = self.prochaine else { return sortie };
        loop {
            // Tout ce qui est contigu part au décodeur.
            while let Some(trame) = attente.remove(&next) {
                self.lu_pts = Some(trame.pts_us);
                sortie.push(trame);
                next = next.wrapping_add(1);
            }
            // Une trame clé au-delà d'un trou : on y saute, elle remet le décodeur
            // à neuf et les trames manquantes d'avant ne serviraient à rien.
            match attente.iter().find(|(k, t)| **k > next && t.idr).map(|(s, _)| *s) {
                Some(s) => {
                    attente.retain(|k, _| *k >= s);
                    next = s;
                }
                None => break,
            }
        }
        // Un trou qui s'éternise sans trame clé en réserve : table rase, la
        // prochaine trame clé relancera la lecture.
        if attente.len() > ATTENTE_MAX {
            attente.clear();
            self.prochaine = None;
        } else {
            self.prochaine = Some(next);
        }
        sortie
    }
}

/// Le SPS et le PPS d'une trame clé en Annex B, start codes compris : ce qu'un
/// décodeur Android veut pour se configurer (`csd-0`, `csd-1`).
pub fn parametres_h264(trame: &[u8]) -> Option<(&[u8], &[u8])> {
    let mut debuts = Vec::new();
    let mut i = 0;
    while i + 3 <= trame.len() {
        if trame[i..].starts_with(&[0, 0, 1]) {
            // Le start code long compte le zéro d'avant.
            let debut = if i > 0 && trame[i - 1] == 0 { i - 1 } else { i };
            debuts.push((debut, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let (mut sps, mut pps) = (None, None);
    for (n, &(debut, nal)) in debuts.iter().enumerate() {
        let fin = debuts.get(n + 1).map_or(trame.len(), |d| d.0);
        match trame.get(nal).map(|o| o & 0x1f) {
            Some(7) if sps.is_none() => sps = Some(&trame[debut..fin]),
            Some(8) if pps.is_none() => pps = Some(&trame[debut..fin]),
            _ => {}
        }
    }
    Some((sps?, pps?))
}

/// Un écart qui saute de ça ne vient pas du réseau : celui qui diffuse a relancé son
/// encodeur et ses horodatages. On réapprend.
const SAUT_D_HORLOGE_US: i64 = 2_000_000;
/// Le plus petit écart se cherche sur deux fenêtres de cette durée : assez long pour
/// ne pas oublier le meilleur trajet, assez court pour suivre si le réseau change.
const FENETRE_REPERE: Duration = Duration::from_secs(5);
/// Le décalage retenu rejoint le repère à ce pas, à chaque lecture : un à-coup de
/// quelques millisecondes ne doit pas décaler d'un coup toutes les images.
const RATTRAPAGE_US: i64 = 100;
/// Au-delà, autant y aller d'un coup : le réseau a vraiment changé.
const RATTRAPAGE_MAX_US: i64 = 100_000;

/// Où en est l'horloge de celui qui diffuse par rapport à la nôtre : le plus petit
/// (arrivée − horodatage) récent, celui de la trame qui a eu le meilleur trajet.
pub struct Repere {
    courant: Option<i64>,
    passe: Option<i64>,
    debut: Instant,
    decalage: Option<i64>,
}

impl Default for Repere {
    fn default() -> Self {
        Self { courant: None, passe: None, debut: Instant::now(), decalage: None }
    }
}

impl Repere {
    /// L'écart (arrivée − horodatage) d'une trame qui vient d'arriver.
    pub fn observer(&mut self, ecart: i64) {
        if self.base().is_some_and(|b| (ecart - b).abs() > SAUT_D_HORLOGE_US) {
            *self = Self::default();
        }
        self.courant = Some(self.courant.map_or(ecart, |c| c.min(ecart)));
        if self.debut.elapsed() >= FENETRE_REPERE {
            self.passe = self.courant.take();
            self.debut = Instant::now();
        }
    }

    fn base(&self) -> Option<i64> {
        match (self.courant, self.passe) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Ce qu'il faut ajouter à un horodatage pour le lire sur notre horloge.
    pub fn decalage(&mut self) -> Option<i64> {
        let voulu = self.base()?;
        let d = match self.decalage {
            Some(d) if (voulu - d).abs() <= RATTRAPAGE_MAX_US => d + (voulu - d).clamp(-RATTRAPAGE_US, RATTRAPAGE_US),
            _ => voulu,
        };
        self.decalage = Some(d);
        Some(d)
    }
}

/// Remet d'aplomb le rythme des horodatages, pour un affichage régulier.
///
/// Une vidéo à 30 images/s capturée sur un PC arrive avec des horodatages qui
/// tremblent de quelques millisecondes. C'est peu, mais chaque image s'affiche au
/// balayage qui suit son heure : si cette heure tombe près d'un balayage, elle tombe
/// tantôt avant, tantôt après, et les images durent 16, 33 ou 50 ms au lieu de 33.
/// Mesuré sur un Flip 6 : deux images sur cinq. On suit donc le rythme moyen, et les
/// horodatages ne le corrigent que doucement. Un rythme qui n'en est pas un (un
/// bureau immobile, un jeu qui rame, une trame perdue) passe tel quel.
#[derive(Default)]
pub struct Lissage {
    /// Les derniers horodatages, à intervalles censés être réguliers.
    fenetre: std::collections::VecDeque<i64>,
    hors_rythme: u32,
}

/// La droite se trace sur tant d'images : deux secondes à 30 images/s, de quoi moyenner
/// le tremblement sans ignorer un rythme qui change.
const LISSAGE_FENETRE: usize = 64;
/// En dessous, on ne prétend pas connaître le rythme.
const LISSAGE_MIN: usize = 8;
/// L'écart le plus grand qu'on remet à sa place sans le dire.
const LISSAGE_ACCROC_MAX_US: f64 = 25_000.0;

impl Lissage {
    /// L'horodatage à utiliser pour cette trame, dans l'ordre de lecture.
    pub fn lisser(&mut self, pts: i64) -> i64 {
        if let Some((fin, pas)) = self.droite() {
            let prevu = fin + pas;
            let ecart = (pts as f64 - prevu).abs();
            if ecart > pas / 3.0 {
                // À contretemps. Trois de suite : le rythme a changé, ou une trame s'est
                // perdue et tout est décalé.
                self.hors_rythme += 1;
                if self.hors_rythme >= 3 {
                    self.hors_rythme = 0;
                    self.fenetre.clear();
                    self.fenetre.push_back(pts);
                    return pts;
                }
                // Une image isolée : sa place reste marquée, pour ne pas fausser le compte,
                // et elle s'affiche à sa place. C'est ce qu'on voit le plus : une image
                // qui dure 16 ms, et la suivante 50. Au-delà de quelques balayages, c'est
                // un trou ou un vrai retard : on ne le cache pas.
                self.pousser(prevu.round() as i64);
                return if ecart <= LISSAGE_ACCROC_MAX_US { prevu.round() as i64 } else { pts };
            }
        }
        self.hors_rythme = 0;
        self.pousser(pts);
        // On ne garde que la fin régulière de la fenêtre : pas de droite sur un bureau
        // qui change au hasard.
        while self.fenetre.len() >= LISSAGE_MIN {
            if let Some((fin, _)) = self.droite().filter(|(fin, pas)| self.reguliere(*fin, *pas)) {
                return fin.round() as i64;
            }
            self.fenetre.pop_front();
        }
        pts
    }

    /// Tous les points de la fenêtre sont-ils près de la droite ?
    fn reguliere(&self, fin: f64, pas: f64) -> bool {
        let n = self.fenetre.len();
        self.fenetre.iter().enumerate().all(|(i, p)| {
            let attendu = fin - pas * (n - 1 - i) as f64;
            // Plus large que pour accepter une image (pas / 3) : la droite bouge un peu à
            // chaque image, et une image acceptée de justesse ne doit pas vider la fenêtre.
            (*p as f64 - attendu).abs() <= pas / 2.0
        })
    }

    fn pousser(&mut self, pts: i64) {
        self.fenetre.push_back(pts);
        if self.fenetre.len() > LISSAGE_FENETRE {
            self.fenetre.pop_front();
        }
    }

    /// Où la droite passe à la dernière image, et son pas entre deux images : les
    /// moindres carrés sur toute la fenêtre. Un pas pris aux deux bouts ne vaudrait
    /// rien : à lui seul, le tremblement de la dernière image déplacerait la fin de la
    /// droite de la moitié de ce tremblement.
    fn droite(&self) -> Option<(f64, f64)> {
        let n = self.fenetre.len();
        if n < LISSAGE_MIN {
            return None;
        }
        let premier = *self.fenetre.front()?;
        let milieu = (n - 1) as f64 / 2.0;
        let (mut somme, mut moments, mut carres) = (0.0, 0.0, 0.0);
        for (i, p) in self.fenetre.iter().enumerate() {
            let (x, y) = (i as f64 - milieu, (*p - premier) as f64);
            somme += y;
            moments += x * y;
            carres += x * x;
        }
        let pas = moments / carres;
        if pas <= 0.0 {
            return None;
        }
        Some((premier as f64 + somme / n as f64 + pas * milieu, pas))
    }
}

#[cfg(test)]
mod tests {
    /// Un tremblement pseudo-aléatoire, de ±`amplitude` µs. Surtout pas un motif qui se
    /// répète : un motif de 7 images s'annule tout seul sur 64, et il a caché ici qu'un
    /// premier calcul de la droite ne lissait presque rien.
    fn tremblement(i: usize, amplitude: i64) -> i64 {
        let mut x = (i as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        x ^= x >> 29;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 32;
        (x % (2 * amplitude as u64 + 1)) as i64 - amplitude
    }

    /// Les horodatages d'une vidéo à 30 images/s, avec un tremblement de quelques
    /// millisecondes.
    fn horodatages_qui_tremblent(n: usize) -> Vec<i64> {
        (0..n).map(|i| 1_000_000 + i as i64 * 33_333 + tremblement(i, 4000)).collect()
    }

    #[test]
    fn le_lissage_rend_le_rythme_regulier() {
        let mut l = Lissage::default();
        let sortie: Vec<i64> = horodatages_qui_tremblent(300).into_iter().map(|p| l.lisser(p)).collect();
        // Le temps de trouver le rythme, puis ça ne bouge presque plus.
        for paire in sortie[120..].windows(2) {
            let pas = paire[1] - paire[0];
            assert!((pas - 33_333).abs() < 1500, "pas de {pas} µs");
        }
    }

    #[test]
    fn le_lissage_ne_derive_pas_des_horodatages() {
        let mut l = Lissage::default();
        for p in horodatages_qui_tremblent(2000) {
            let s = l.lisser(p);
            assert!((s - p).abs() < 17_000, "{s} contre {p}");
        }
    }

    #[test]
    fn une_trame_perdue_recale_sur_l_horodatage() {
        let mut l = Lissage::default();
        let mut p = 0;
        for _ in 0..100 {
            p += 33_333;
            l.lisser(p);
        }
        p += 5 * 33_333;
        assert_eq!(l.lisser(p), p);
        // Et le rythme reprend : la trame suivante est de nouveau lissée, pas perdue.
        p += 33_333;
        assert!((l.lisser(p) - p).abs() < 1000);
    }

    #[test]
    fn un_rythme_qui_change_est_adopte() {
        let mut l = Lissage::default();
        let mut p = 0;
        for _ in 0..100 {
            p += 33_333;
            l.lisser(p);
        }
        for _ in 0..10 {
            p += 16_667;
            l.lisser(p);
        }
        p += 16_667;
        assert!((l.lisser(p) - p).abs() < 1000);
    }

    #[test]
    fn une_vraie_video_capturee_sur_un_pc_devient_reguliere() {
        // Mesuré le 07/11 sur un stream réel : ±3 ms de tremblement, et une image sur
        // quinze environ en avance de 15 ms (des écarts de 17,8 puis 44,7 ms).
        let mut l = Lissage::default();
        let sortie: Vec<i64> = (0..600usize)
            .map(|i| {
                let accroc = if i % 15 == 14 { -15_500 } else { 0 };
                l.lisser(1_000_000 + i as i64 * 33_333 + tremblement(i, 3000) + accroc)
            })
            .collect();
        let pas = &sortie[120..];
        for p in pas.windows(2) {
            assert!((p[1] - p[0] - 33_333).abs() < 1500, "pas de {} µs", p[1] - p[0]);
        }
    }

    #[test]
    fn une_image_hors_rythme_ne_casse_pas_la_droite() {
        let mut l = Lissage::default();
        let mut p = 0;
        for _ in 0..100 {
            p += 33_333;
            l.lisser(p);
        }
        // Une seule image en retard de 15 ms : elle s'affiche à sa place…
        p += 33_333;
        assert!((l.lisser(p + 15_000) - p).abs() < 1000);
        // … et la suivante aussi.
        p += 33_333;
        assert!((l.lisser(p) - p).abs() < 1000);
    }

    #[test]
    fn un_bureau_qui_change_au_hasard_reste_proche_de_ses_horodatages() {
        // Un rythme qui n'en est pas un, assez long pour remplir la fenêtre.
        let mut l = Lissage::default();
        let mut p = 0i64;
        let mut x = 12_345u64;
        for _ in 0..2000 {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            p += 1000 + ((x >> 40) % 400_000) as i64;
            let s = l.lisser(p);
            assert!((s - p).abs() < 140_000, "{s} contre {p}");
        }
    }

    #[test]
    fn un_rythme_irregulier_passe_tel_quel() {
        let mut l = Lissage::default();
        let mut p = 0;
        for pas in [33_000, 200_000, 16_000, 90_000, 500_000, 40_000, 1_000_000, 16_000] {
            p += pas;
            assert_eq!(l.lisser(p), p);
        }
    }

    #[test]
    fn le_repere_retient_le_meilleur_trajet() {
        let mut r = Repere::default();
        assert_eq!(r.decalage(), None);
        for e in [50_000, 42_000, 61_000, 45_000] {
            r.observer(e);
        }
        // La toute première lecture : directement le repère.
        assert_eq!(r.decalage(), Some(42_000));
    }

    #[test]
    fn le_decalage_rejoint_le_repere_en_douceur() {
        let mut r = Repere::default();
        r.observer(50_000);
        assert_eq!(r.decalage(), Some(50_000));
        r.observer(48_000);
        assert_eq!(r.decalage(), Some(50_000 - RATTRAPAGE_US));
        for _ in 0..100 {
            r.decalage();
        }
        assert_eq!(r.decalage(), Some(48_000));
    }

    #[test]
    fn un_saut_d_horloge_fait_tout_reapprendre() {
        let mut r = Repere::default();
        r.observer(50_000);
        r.decalage();
        r.observer(50_000 + SAUT_D_HORLOGE_US + 1);
        assert_eq!(r.decalage(), Some(50_000 + SAUT_D_HORLOGE_US + 1));
    }

    use super::*;

    const CLE: [u8; 32] = [7; 32];

    fn trame(seq: u64, idr: bool) -> Trame {
        Trame { seq, idr, pts_us: seq * 33_333, largeur: 1280, hauteur: 720, donnees: vec![seq as u8] }
    }

    fn lues(l: &mut Lecture) -> Vec<u64> {
        l.a_decoder(1.0).into_iter().map(|t| t.seq).collect()
    }

    #[test]
    fn une_trame_scellee_s_ouvre_de_l_autre_cote() {
        let mut s = Scelleur::new(9, CLE);
        let (seq, octets) = s.sceller(b"nal", true, 1234, 1280, 720).unwrap();
        assert_eq!(seq, 0);
        let (t, basse) = Ouvreur::new(9, CLE).ouvrir(&octets).unwrap();
        assert!(!basse);
        assert_eq!(t, Trame { seq: 0, idr: true, pts_us: 1234, largeur: 1280, hauteur: 720, donnees: b"nal".to_vec() });
        // La séquence avance : c'est elle qui fait le nonce.
        assert_eq!(s.sceller(b"nal", false, 1, 1280, 720).unwrap().0, 1);
    }

    #[test]
    fn ce_qui_n_est_pas_a_nous_ne_s_ouvre_pas() {
        let (_, octets) = Scelleur::new(9, CLE).sceller(b"nal", true, 1, 2, 2).unwrap();
        assert!(Ouvreur::new(8, CLE).ouvrir(&octets).is_none(), "autre stream");
        assert!(Ouvreur::new(9, [1; 32]).ouvrir(&octets).is_none(), "autre clé");
        let mut altere = octets.clone();
        altere[16] ^= 1;
        assert!(Ouvreur::new(9, CLE).ouvrir(&altere).is_none(), "en-tête altéré");
        assert!(Ouvreur::new(9, CLE).ouvrir(&octets[..10]).is_none(), "tronquée");
    }

    #[test]
    fn un_datagramme_de_son_s_ouvre_et_un_altere_non() {
        let mut tete = [0u8; ki_protocol::AUDIO_HEADER_LEN];
        ki_protocol::write_audio_header(&mut tete, &ki_protocol::AudioHeader { stream_id: 9, seq: 4, pts_us: 8 });
        let nonce = ki_protocol::nonce_for_media(ki_protocol::MEDIA_DOMAIN_GAME_AUDIO, 9, 4);
        let scelle = XChaCha20Poly1305::new(&CLE.into())
            .encrypt(&XNonce::from(nonce), Payload { msg: b"opus", aad: &tete })
            .unwrap();
        let mut datagramme = tete.to_vec();
        datagramme.extend_from_slice(&scelle);
        assert_eq!(Ouvreur::new(9, CLE).ouvrir_son(&datagramme), Some((4, 8, b"opus".to_vec())));
        assert_eq!(Ouvreur::new(8, CLE).ouvrir_son(&datagramme), None);
        datagramme[10] ^= 1;
        assert_eq!(Ouvreur::new(9, CLE).ouvrir_son(&datagramme), None);
    }

    /// Le format doit rester celui du PC : même en-tête, même nonce.
    #[test]
    fn une_trame_scellee_a_l_en_tete_du_pc() {
        let (_, octets) = Scelleur::new(9, CLE).sceller(b"nal", true, 1234, 1280, 720).unwrap();
        let h = ki_protocol::parse_media_header(&octets).unwrap();
        assert!(h.idr && !h.basse);
        assert_eq!((h.stream_id, h.seq, h.pts_us, h.group_id, h.width, h.height), (9, 0, 1234, 1, 1280, 720));
        let nonce = ki_protocol::nonce_for_media(ki_protocol::MEDIA_DOMAIN_VIDEO, 9, 0);
        let (aad, scelle) = octets.split_at(ki_protocol::MEDIA_HEADER_LEN);
        let clair = XChaCha20Poly1305::new(&CLE.into())
            .decrypt(&XNonce::from(nonce), Payload { msg: scelle, aad })
            .unwrap();
        assert_eq!(clair, b"nal");
    }

    #[test]
    fn la_lecture_demarre_a_une_trame_cle_et_remet_en_ordre() {
        let mut l = Lecture::default();
        // Rien d'indécodable avant la première trame clé.
        l.recevoir(trame(3, false), false);
        assert!(lues(&mut l).is_empty());
        assert!(l.attend_une_cle());
        // La 6 double la 5.
        l.recevoir(trame(4, true), false);
        l.recevoir(trame(6, false), false);
        assert_eq!(lues(&mut l), [4]);
        l.recevoir(trame(5, false), false);
        assert_eq!(lues(&mut l), [5, 6]);
        assert!(!l.attend_une_cle());
    }

    #[test]
    fn une_trame_cle_au_dela_d_un_trou_y_fait_sauter() {
        let mut l = Lecture::default();
        l.recevoir(trame(1, true), false);
        assert_eq!(lues(&mut l), [1]);
        // La 2 ne viendra jamais.
        l.recevoir(trame(3, false), false);
        assert!(lues(&mut l).is_empty());
        l.recevoir(trame(10, true), false);
        l.recevoir(trame(11, false), false);
        assert_eq!(lues(&mut l), [10, 11]);
    }

    #[test]
    fn un_trou_qui_s_eternise_fait_table_rase() {
        let mut l = Lecture::default();
        l.recevoir(trame(1, true), false);
        assert_eq!(lues(&mut l), [1]);
        for seq in 3..(3 + ATTENTE_MAX as u64 + 1) {
            l.recevoir(trame(seq, false), false);
        }
        assert!(lues(&mut l).is_empty());
        assert!(l.attend_une_cle());
    }

    #[test]
    fn le_serveur_nous_fait_changer_de_qualite_par_une_trame_cle() {
        let mut l = Lecture::default();
        l.recevoir(trame(100, true), false);
        assert_eq!(lues(&mut l), [100]);
        assert!(!l.basse());
        // Une trame clé basse plus ancienne que ce qu'on a lu ne ramène pas en arrière.
        l.recevoir(trame(1, true), true);
        assert!(lues(&mut l).is_empty());
        assert!(!l.basse());
        // Une plus récente, si.
        l.recevoir(trame(200, true), true);
        l.recevoir(trame(201, false), true);
        assert_eq!(lues(&mut l), [200, 201]);
        assert!(l.basse());
    }

    #[test]
    fn un_decodage_en_retard_saute_a_la_derniere_trame_cle() {
        let mut l = Lecture::default();
        l.recevoir(trame(1, true), false);
        l.recevoir(trame(2, false), false);
        l.recevoir(trame(3, true), false);
        l.recevoir(trame(4, false), false);
        // 4 trames à 100 ms chacune : 400 ms de retard, on saute à la 3.
        let lu: Vec<u64> = l.a_decoder(100.0).into_iter().map(|t| t.seq).collect();
        assert_eq!(lu, [3, 4]);
    }

    #[test]
    fn repartir_attend_une_nouvelle_trame_cle() {
        let mut l = Lecture::default();
        l.recevoir(trame(1, true), false);
        assert_eq!(lues(&mut l), [1]);
        l.repartir();
        assert!(l.attend_une_cle());
        l.recevoir(trame(2, false), false);
        assert!(lues(&mut l).is_empty());
        l.recevoir(trame(5, true), false);
        assert_eq!(lues(&mut l), [5]);
    }

    #[test]
    fn le_sps_et_le_pps_se_trouvent_quels_que_soient_les_start_codes() {
        let trame = [
            0, 0, 0, 1, 0x67, 1, 2, // SPS, start code long
            0, 0, 1, 0x68, 3, // PPS, start code court
            0, 0, 0, 1, 0x65, 9, 9, // IDR
        ];
        let (sps, pps) = parametres_h264(&trame).unwrap();
        assert_eq!(sps, [0, 0, 0, 1, 0x67, 1, 2]);
        assert_eq!(pps, [0, 0, 1, 0x68, 3]);
        assert!(parametres_h264(&[0, 0, 1, 0x65, 1]).is_none(), "sans paramètres");
        assert!(parametres_h264(&[]).is_none());
    }
}
