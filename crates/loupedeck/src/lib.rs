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
}

/// Moitié « lecture » : les événements.
pub struct Lecteur {
    port: Arc<Port>,
    tampon: Vec<u8>,
}

/// Ouvre le Loupedeck (port trouvé tout seul si `port` vaut `None`) et
/// fait la poignée de main WebSocket.
pub fn ouvrir(port: Option<&str>) -> Result<(Ecrivain, Lecteur)> {
    let nom = match port {
        Some(p) => p.to_string(),
        None => trouver_port()?,
    };
    let sp = Arc::new(Port::ouvrir(&nom, BAUDS)?);

    sp.vider();
    sp.ecrire(
        b"GET /index.html HTTP/1.1\r\n\
          Connection: Upgrade\r\n\
          Upgrade: websocket\r\n\
          Sec-WebSocket-Key: 123abc\r\n\r\n",
    )?;
    // Un appareil resté en mode WebSocket (programme précédent fermé sans
    // le réinitialiser) ignore la poignée de main et peut encore cracher
    // des trames : on ramasse tout pendant 1,5 s au plus, on cherche la
    // réponse 101, et sans elle on continue quand même.
    sp.delai(Duration::from_millis(100))?;
    let mut recu = Vec::new();
    let limite = std::time::Instant::now() + Duration::from_millis(1500);
    let mut reste = Vec::new();
    let mut accepte = false;
    let mut morceau = [0u8; 512];
    while std::time::Instant::now() < limite {
        let n = sp.lire(&mut morceau).context("poignée de main")?;
        recu.extend_from_slice(&morceau[..n]);
        if let Some(debut) = chercher(&recu, b"HTTP/1.1 101") {
            if let Some(fin) = chercher(&recu[debut..], b"\r\n\r\n") {
                reste = recu[debut + fin + 4..].to_vec();
                accepte = true;
                break;
            }
        }
    }
    if !accepte {
        eprintln!("loupedeck : pas de réponse 101 à la poignée de main, déjà connecté ?");
    }

    Ok((
        Ecrivain { port: sp.clone(), transaction: 0 },
        Lecteur { port: sp, tampon: reste },
    ))
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
