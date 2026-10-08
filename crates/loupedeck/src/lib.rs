//! Pilote du Loupedeck Live, sans le logiciel officiel.
//!
//! Le Live est un port série USB (VID 0x2EC2, PID 0x0004) qui parle
//! WebSocket : une poignée de main HTTP « Upgrade », puis des trames
//! binaires 0x82. Dans chaque trame : `[longueur, commande, transaction,
//! données…]`. Côté écrans, c'est UNE dalle de 480×270 (id « M ») : une
//! bande de 60 px à gauche, la grille de 4×3 touches de 90 px au milieu,
//! une bande de 60 px à droite. Pixels en RGB565 petit-boutiste.
//!
//! Le port série lui-même passe par l'API Windows (voir `port`).

use anyhow::{bail, Context, Result};
use std::sync::Arc;
use std::time::Duration;

mod port;
use port::Port;

const BAUDS: u32 = 256_000;

mod cmd {
    pub const BOUTON: u8 = 0x00;
    pub const MOLETTE: u8 = 0x01;
    pub const COULEUR: u8 = 0x02;
    pub const LUMINOSITE: u8 = 0x09;
    pub const FRAMEBUFF: u8 = 0x10;
    pub const DESSINER: u8 = 0x0f;
    pub const TOUCHER: u8 = 0x4d;
    pub const TOUCHER_FIN: u8 = 0x6d;
}

/// Id de la dalle unique du Live.
const DALLE: [u8; 2] = [0x00, b'M'];
pub const LARGEUR: u16 = 480;
pub const HAUTEUR: u16 = 270;
pub const TOUCHE: u16 = 90;
/// Largeur des bandes de gauche et de droite, de part et d'autre de la grille.
pub const MARGE_GAUCHE: u16 = 60;

/// Une commande physique : les 6 molettes (qu'on peut aussi enfoncer)
/// et les 8 boutons ronds du bas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Controle {
    /// 0 = haut gauche, 1 = milieu gauche, 2 = bas gauche, 3-5 = à droite.
    Molette(u8),
    /// 0 (le rond tout à gauche) à 7.
    Rond(u8),
}

impl Controle {
    fn depuis_id(id: u8) -> Option<Self> {
        match id {
            0x01..=0x06 => Some(Self::Molette(id - 1)),
            0x07..=0x0e => Some(Self::Rond(id - 7)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evenement {
    Appui(Controle),
    Relache(Controle),
    /// Crans de molette : positif = sens horaire.
    Tourne { molette: u8, crans: i8 },
    /// Doigt posé ou glissé sur la dalle. `touche` = 0..12 dans la grille
    /// centrale (ligne par ligne), `None` dans les bandes latérales.
    Toucher { x: u16, y: u16, doigt: u8, touche: Option<u8> },
    Lever { x: u16, y: u16, doigt: u8, touche: Option<u8> },
}

/// Cherche le port série du Loupedeck parmi les ports présents.
pub fn trouver_port() -> Result<String> {
    port::trouver()
}

fn touche_a(x: u16, y: u16) -> Option<u8> {
    if !(MARGE_GAUCHE..MARGE_GAUCHE + 4 * TOUCHE).contains(&x) || y >= HAUTEUR {
        return None;
    }
    let col = (x - MARGE_GAUCHE) / TOUCHE;
    let ligne = y / TOUCHE;
    Some((ligne * 4 + col) as u8)
}

fn chercher(foin: &[u8], aiguille: &[u8]) -> Option<usize> {
    foin.windows(aiguille.len()).position(|w| w == aiguille)
}

/// Couleur 24 bits → RGB565.
pub fn rgb565(r: u8, g: u8, b: u8) -> u16 {
    ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3)
}

/// Moitié « écriture » : couleurs, luminosité, dessin.
pub struct Ecrivain {
    port: Arc<Port>,
    transaction: u8,
    poignee: Poignee,
}

/// Comment la poignée de main s'est passée.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Poignee {
    /// L'appareil attendait sa poignée de main, et a répondu.
    Directe,
    /// Il était resté en WebSocket (programme précédent arrêté net, ou
    /// fermé sans la trame de fermeture) et a été remis d'aplomb.
    Reprise,
}

/// La trame WebSocket « fermeture », masquée d'un masque nul : l'appareil
/// quitte le WebSocket et attend de nouveau une poignée de main. Le pilote
/// de référence (foxxyz/loupedeck) l'envoie avant de rendre le port.
const FERMETURE: [u8; 6] = [0x88, 0x80, 0x00, 0x00, 0x00, 0x00];

/// La requête qui fait passer l'appareil en WebSocket.
const POIGNEE: &[u8] = b"GET /index.html HTTP/1.1\r\n\
    Connection: Upgrade\r\n\
    Upgrade: websocket\r\n\
    Sec-WebSocket-Key: 123abc\r\n\r\n";

/// Comment le micrologiciel lit l'en-tête d'une trame : le masque
/// seulement s'il est annoncé (la norme), ou toujours. On ne le sait pas ;
/// la reprise essaie l'une puis l'autre. Sur le Live de drion (08/10), la
/// norme a suffi du premier coup : l'autre lecture reste en secours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lecture {
    Norme,
    MasqueToujours,
}

/// Les octets nuls qu'il faut encore envoyer pour que l'appareil, resté
/// en WebSocket et sur une frontière de trame quand on a ouvert le port,
/// retombe sur une frontière après avoir lu `envoyes` comme des trames.
/// Notre requête de poignée de main en fait partie : le « E » de « GET »
/// y passe pour une longueur de 69 octets, et ainsi de suite — à la fin,
/// l'appareil attend la suite d'une trame qui n'existe pas, avale ce qu'on
/// lui envoie ensuite, et ne s'en remettait qu'en étant débranché.
fn zeros_jusqu_a_une_frontiere(lecture: Lecture, envoyes: &[u8]) -> usize {
    #[derive(Clone, Copy)]
    enum Etat {
        Debut,
        Longueur,
        Etendue { reste: u8, valeur: u64 },
        Masque { reste: u8, charge: u64 },
        Charge(u64),
    }
    fn apres_longueur(lecture: Lecture, masque: bool, charge: u64) -> Etat {
        if masque || lecture == Lecture::MasqueToujours {
            Etat::Masque { reste: 4, charge }
        } else if charge == 0 {
            Etat::Debut
        } else {
            Etat::Charge(charge)
        }
    }
    fn avancer(etat: Etat, octet: u8, lecture: Lecture, masque: &mut bool) -> Etat {
        match etat {
            Etat::Debut => Etat::Longueur,
            Etat::Longueur => {
                *masque = octet & 0x80 != 0;
                match octet & 0x7f {
                    126 => Etat::Etendue { reste: 2, valeur: 0 },
                    127 => Etat::Etendue { reste: 8, valeur: 0 },
                    n => apres_longueur(lecture, *masque, u64::from(n)),
                }
            }
            Etat::Etendue { reste, valeur } => {
                let valeur = (valeur << 8) | u64::from(octet);
                if reste > 1 {
                    Etat::Etendue { reste: reste - 1, valeur }
                } else {
                    apres_longueur(lecture, *masque, valeur)
                }
            }
            Etat::Masque { reste, charge } => {
                if reste > 1 {
                    Etat::Masque { reste: reste - 1, charge }
                } else if charge == 0 {
                    Etat::Debut
                } else {
                    Etat::Charge(charge)
                }
            }
            Etat::Charge(n) if n > 1 => Etat::Charge(n - 1),
            Etat::Charge(_) => Etat::Debut,
        }
    }
    let mut masque = false;
    let mut etat = Etat::Debut;
    for &octet in envoyes {
        etat = avancer(etat, octet, lecture, &mut masque);
    }
    let mut zeros = 0;
    // Borné : une trame annoncée plus longue que tout ce qu'on envoie un
    // jour ne se complète pas à coups de zéros.
    while !matches!(etat, Etat::Debut) && zeros < 1 << 20 {
        etat = avancer(etat, 0, lecture, &mut masque);
        zeros += 1;
    }
    zeros
}

impl Drop for Ecrivain {
    /// L'appareil est rendu en attente de poignée de main : sans la trame
    /// de fermeture, il restait en WebSocket, et le prochain lancement de
    /// ki-chat le trouvait sourd.
    fn drop(&mut self) {
        let _ = self.port.ecrire(&FERMETURE);
    }
}

/// Moitié « lecture » : les événements.
pub struct Lecteur {
    port: Arc<Port>,
    tampon: Vec<u8>,
}

/// Ouvre le Loupedeck (port trouvé tout seul si `port` vaut `None`) et
/// fait la poignée de main WebSocket — en le remettant d'aplomb s'il était
/// resté en WebSocket (voir [`Poignee::Reprise`]).
pub fn ouvrir(port: Option<&str>) -> Result<(Ecrivain, Lecteur)> {
    let nom = match port {
        Some(p) => p.to_string(),
        None => trouver_port()?,
    };
    let sp = Arc::new(Port::ouvrir(&nom, BAUDS)?);
    sp.vider();
    sp.delai(Duration::from_millis(100))?;

    // Tout ce qui part avant la réponse : de quoi refaire, octet par
    // octet, la lecture qu'en a faite un appareil resté en WebSocket.
    let mut envoyes: Vec<u8> = Vec::new();
    let mut poignee = Poignee::Directe;
    let mut reste = poignee_de_main(&sp, &mut envoyes)?;
    if reste.is_none() {
        // Pas de réponse : resté en WebSocket, il a lu notre requête comme
        // des trames. On complète la dernière par des zéros, on lui dit de
        // fermer, et on recommence — selon chacune des deux lectures
        // possibles de ses en-têtes.
        for lecture in [Lecture::Norme, Lecture::MasqueToujours] {
            let zeros = vec![0u8; zeros_jusqu_a_une_frontiere(lecture, &envoyes)];
            envoyer_brut(&sp, &mut envoyes, &zeros)?;
            envoyer_brut(&sp, &mut envoyes, &FERMETURE)?;
            std::thread::sleep(Duration::from_millis(150));
            sp.vider();
            reste = poignee_de_main(&sp, &mut envoyes)?;
            if reste.is_some() {
                poignee = Poignee::Reprise;
                break;
            }
        }
    }
    let Some(reste) = reste else {
        bail!(
            "le Loupedeck ne répond plus (resté bloqué au milieu d'un envoi par un programme \
             arrêté net) : débranche-le et rebranche-le"
        );
    };

    Ok((
        Ecrivain { port: sp.clone(), transaction: 0, poignee },
        Lecteur { port: sp, tampon: reste },
    ))
}

fn envoyer_brut(sp: &Port, envoyes: &mut Vec<u8>, octets: &[u8]) -> Result<()> {
    if octets.is_empty() {
        return Ok(());
    }
    envoyes.extend_from_slice(octets);
    sp.ecrire(octets)
}

/// Envoie la requête et attend la réponse 101, 1,5 s au plus — en
/// ramassant au passage ce qu'un appareil resté en WebSocket peut encore
/// cracher. Rend ce qui suit la réponse (le début du flux WebSocket), ou
/// `None` sans réponse.
fn poignee_de_main(sp: &Port, envoyes: &mut Vec<u8>) -> Result<Option<Vec<u8>>> {
    envoyer_brut(sp, envoyes, POIGNEE)?;
    let mut recu = Vec::new();
    let limite = std::time::Instant::now() + Duration::from_millis(1500);
    let mut morceau = [0u8; 512];
    while std::time::Instant::now() < limite {
        let n = sp.lire(&mut morceau).context("poignée de main")?;
        recu.extend_from_slice(&morceau[..n]);
        if let Some(debut) = chercher(&recu, b"HTTP/1.1 101") {
            if let Some(fin) = chercher(&recu[debut..], b"\r\n\r\n") {
                return Ok(Some(recu[debut + fin + 4..].to_vec()));
            }
        }
    }
    Ok(None)
}

impl Ecrivain {
    fn envoyer(&mut self, commande: u8, donnees: &[u8]) -> Result<()> {
        self.transaction = self.transaction.wrapping_add(1).max(1);
        let mut paquet = Vec::with_capacity(3 + donnees.len());
        paquet.push((3 + donnees.len()).min(0xff) as u8);
        paquet.push(commande);
        paquet.push(self.transaction);
        paquet.extend_from_slice(donnees);

        // Trame WebSocket binaire, masquée avec un masque nul (l'appareil
        // l'exige). Au-delà de 255 octets : longueur sur 64 bits.
        let n = paquet.len();
        let mut trame = Vec::with_capacity(14 + n);
        if n > 0xff {
            trame.extend_from_slice(&[0x82, 0xff, 0, 0, 0, 0]);
            trame.extend_from_slice(&(n as u32).to_be_bytes());
            trame.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            trame.extend_from_slice(&[0x82, 0x80 + n as u8, 0, 0, 0, 0]);
        }
        trame.extend_from_slice(&paquet);
        self.port.ecrire(&trame)?;
        Ok(())
    }

    /// Comment la poignée de main s'est passée.
    pub fn poignee(&self) -> Poignee {
        self.poignee
    }

    /// Luminosité des écrans, de 0 à 10.
    pub fn luminosite(&mut self, niveau: u8) -> Result<()> {
        self.envoyer(cmd::LUMINOSITE, &[niveau.min(10)])
    }

    /// Couleur de la LED d'un bouton rond (0 à 7).
    pub fn couleur_rond(&mut self, rond: u8, r: u8, g: u8, b: u8) -> Result<()> {
        self.envoyer(cmd::COULEUR, &[0x07 + rond.min(7), r, g, b])
    }

    /// Envoie un rectangle de pixels RGB565 dans la mémoire de la dalle,
    /// sans l'afficher : [`Ecrivain::rafraichir`] montre d'un coup tout ce
    /// qui a été envoyé, sans qu'on voie les touches changer une à une.
    pub fn tampon(&mut self, x: u16, y: u16, l: u16, h: u16, pixels: &[u16]) -> Result<()> {
        if pixels.len() != l as usize * h as usize {
            bail!("taille de l'image : {} pixels pour {l}×{h}", pixels.len());
        }
        let mut donnees = Vec::with_capacity(10 + pixels.len() * 2);
        donnees.extend_from_slice(&DALLE);
        for v in [x, y, l, h] {
            donnees.extend_from_slice(&v.to_be_bytes());
        }
        for p in pixels {
            donnees.extend_from_slice(&p.to_le_bytes());
        }
        self.envoyer(cmd::FRAMEBUFF, &donnees)
    }

    /// Affiche ce que [`Ecrivain::tampon`] a envoyé.
    pub fn rafraichir(&mut self) -> Result<()> {
        self.envoyer(cmd::DESSINER, &DALLE)
    }

    /// Dessine un rectangle de pixels RGB565 sur la dalle puis rafraîchit.
    pub fn dessiner(&mut self, x: u16, y: u16, l: u16, h: u16, pixels: &[u16]) -> Result<()> {
        self.tampon(x, y, l, h, pixels)?;
        self.rafraichir()
    }

    /// Remplit une touche de la grille (0 à 11) d'une couleur unie.
    pub fn remplir_touche(&mut self, touche: u8, r: u8, g: u8, b: u8) -> Result<()> {
        let t = touche.min(11) as u16;
        let x = MARGE_GAUCHE + (t % 4) * TOUCHE;
        let y = (t / 4) * TOUCHE;
        let pixels = vec![rgb565(r, g, b); (TOUCHE * TOUCHE) as usize];
        self.dessiner(x, y, TOUCHE, TOUCHE, &pixels)
    }

    /// Remplit toute la dalle d'une couleur.
    pub fn remplir_tout(&mut self, r: u8, g: u8, b: u8) -> Result<()> {
        let pixels = vec![rgb565(r, g, b); LARGEUR as usize * HAUTEUR as usize];
        self.dessiner(0, 0, LARGEUR, HAUTEUR, &pixels)
    }
}

impl Lecteur {
    /// Combien [`Lecteur::suivant`] attend avant de rendre la main (100 ms
    /// à l'ouverture).
    pub fn delai(&mut self, d: Duration) -> Result<()> {
        self.port.delai(d).context("délai de lecture")
    }

    /// Attend le prochain événement. `Ok(None)` si rien pendant le délai ;
    /// une erreur si l'appareil n'est plus là.
    pub fn suivant(&mut self) -> Result<Option<Evenement>> {
        loop {
            if let Some(ev) = self.extraire()? {
                return Ok(Some(ev));
            }
            let mut morceau = [0u8; 512];
            let n = self.port.lire(&mut morceau).context("lecture du Loupedeck")?;
            if n == 0 {
                return Ok(None);
            }
            if std::env::var_os("KI_LOUPEDECK_BRUT").is_some() {
                eprintln!("brut {:02x?}", &morceau[..n]);
            }
            self.tampon.extend_from_slice(&morceau[..n]);
        }
    }

    /// Découpe les trames 0x82 complètes du tampon. Les messages qu'on ne
    /// connaît pas (réponses de version, accusés de dessin…) sont sautés.
    fn extraire(&mut self) -> Result<Option<Evenement>> {
        loop {
            let Some(debut) = self.tampon.iter().position(|&o| o == 0x82) else {
                self.tampon.clear();
                return Ok(None);
            };
            self.tampon.drain(..debut);
            if self.tampon.len() < 2 {
                return Ok(None);
            }
            let (entete, longueur) = match self.tampon[1] & 0x7f {
                126 => {
                    if self.tampon.len() < 4 {
                        return Ok(None);
                    }
                    (4, u16::from_be_bytes([self.tampon[2], self.tampon[3]]) as usize)
                }
                n => (2, n as usize),
            };
            if self.tampon.len() < entete + longueur {
                return Ok(None);
            }
            let msg: Vec<u8> = self.tampon[entete..entete + longueur].to_vec();
            self.tampon.drain(..entete + longueur);
            if let Some(ev) = decoder(&msg) {
                return Ok(Some(ev));
            }
        }
    }
}

fn decoder(msg: &[u8]) -> Option<Evenement> {
    if msg.len() < 3 {
        return None;
    }
    let d = &msg[3..];
    match msg[1] {
        cmd::BOUTON if d.len() >= 2 => {
            let c = Controle::depuis_id(d[0])?;
            Some(if d[1] == 0 { Evenement::Appui(c) } else { Evenement::Relache(c) })
        }
        cmd::MOLETTE if d.len() >= 2 => match Controle::depuis_id(d[0])? {
            Controle::Molette(m) => Some(Evenement::Tourne { molette: m, crans: d[1] as i8 }),
            _ => None,
        },
        c @ (cmd::TOUCHER | cmd::TOUCHER_FIN) if d.len() >= 6 => {
            let x = u16::from_be_bytes([d[1], d[2]]);
            let y = u16::from_be_bytes([d[3], d[4]]);
            let (doigt, touche) = (d[5], touche_a(x, y));
            Some(if c == cmd::TOUCHER {
                Evenement::Toucher { x, y, doigt, touche }
            } else {
                Evenement::Lever { x, y, doigt, touche }
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grille() {
        assert_eq!(touche_a(10, 10), None);
        assert_eq!(touche_a(60, 0), Some(0));
        assert_eq!(touche_a(419, 269), Some(11));
        assert_eq!(touche_a(150, 95), Some(5));
        assert_eq!(touche_a(420, 10), None);
    }

    /// Resté en WebSocket, l'appareil lit notre requête comme des trames et
    /// attend, à la fin, la suite d'une charge qui n'existe pas : 78 octets
    /// selon la norme.
    #[test]
    fn la_poignee_de_main_lue_comme_des_trames() {
        assert_eq!(zeros_jusqu_a_une_frontiere(Lecture::Norme, POIGNEE), 78);
        assert_eq!(zeros_jusqu_a_une_frontiere(Lecture::MasqueToujours, POIGNEE), 84);
        // Une fois complétée, on est sur une frontière : rien à ajouter.
        let mut complete = POIGNEE.to_vec();
        complete.extend(std::iter::repeat_n(0, 78));
        assert_eq!(zeros_jusqu_a_une_frontiere(Lecture::Norme, &complete), 0);
    }

    /// Nos propres trames se lisent jusqu'au bout, petites ou étendues.
    #[test]
    fn nos_trames_tombent_sur_une_frontiere() {
        let petite = [0x82, 0x80 + 4, 0, 0, 0, 0, 4, 0x09, 1, 10];
        assert_eq!(zeros_jusqu_a_une_frontiere(Lecture::Norme, &petite), 0);
        let mut etendue = vec![0x82, 0xff, 0, 0, 0, 0];
        etendue.extend_from_slice(&300u32.to_be_bytes());
        etendue.extend_from_slice(&[0, 0, 0, 0]);
        etendue.extend(std::iter::repeat_n(7, 300));
        assert_eq!(zeros_jusqu_a_une_frontiere(Lecture::Norme, &etendue), 0);
        // Coupée au milieu de sa charge : il manque le reste, exactement.
        assert_eq!(zeros_jusqu_a_une_frontiere(Lecture::Norme, &etendue[..100]), 214);
        assert_eq!(zeros_jusqu_a_une_frontiere(Lecture::Norme, &FERMETURE), 0);
    }

    #[test]
    fn decodage() {
        assert_eq!(
            decoder(&[5, 0x00, 1, 0x07, 0x00]),
            Some(Evenement::Appui(Controle::Rond(0)))
        );
        assert_eq!(
            decoder(&[5, 0x01, 1, 0x02, 0xff]),
            Some(Evenement::Tourne { molette: 1, crans: -1 })
        );
    }
}
