//! Un stream en MP4 fragmenté, pour le donner à lire à une télé.
//!
//! Un Chromecast ne sait rien de QUIC ni de notre chiffrement : il lit une adresse
//! HTTP. Le téléphone lui sert donc le stream sous la forme la plus simple qu'il
//! connaisse, un MP4 sans fin : un en-tête qui décrit les deux pistes, puis des
//! fragments (`moof` + `mdat`) à mesure que les trames arrivent. La vidéo n'est pas
//! réencodée : c'est le H.264 du streamer, seulement réemballé.
//!
//! Le son doit être en AAC : Google ne documente pas l'Opus dans un MP4 pour ses
//! appareils vidéo.
//!
//! Tout ici est pur (ni réseau ni horloge), donc testable sans télé.

/// Les unités de temps des deux pistes, par seconde.
pub const ECHELLE_VIDEO: u32 = 90_000;
pub const ECHELLE_SON: u32 = 48_000;
/// Une trame AAC : toujours 1024 échantillons.
pub const ECHANTILLONS_AAC: u32 = 1024;

const PISTE_VIDEO: u32 = 1;
const PISTE_SON: u32 = 2;
/// AAC-LC (2), 48 kHz (indice 3), stéréo (2), sur 16 bits.
const CONFIG_AAC: [u8; 2] = [0x11, 0x90];
/// Un fragment part dès qu'il contient ça : assez peu pour rester réactif, assez pour
/// ne pas envoyer un en-tête par image.
const FRAGMENT_VIDEO: u64 = ECHELLE_VIDEO as u64 / 10;
/// Sans image (écran immobile), le son part quand même, tous les dix blocs : 213 ms.
const FRAGMENT_SON: usize = 10;

// ---------------------------------------------------------------------------
// H.264
// ---------------------------------------------------------------------------

const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;
const NAL_AUD: u8 = 9;

/// Les unités NAL d'une trame en Annex B, sans leurs start codes.
fn nals(trame: &[u8]) -> Vec<&[u8]> {
    let mut debuts = Vec::new();
    let mut i = 0;
    while i + 3 <= trame.len() {
        if trame[i..].starts_with(&[0, 0, 1]) {
            // Le start code long compte le zéro d'avant.
            let avant = if i > 0 && trame[i - 1] == 0 { i - 1 } else { i };
            debuts.push((avant, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    debuts
        .iter()
        .enumerate()
        .map(|(n, &(_, nal))| &trame[nal..debuts.get(n + 1).map_or(trame.len(), |d| d.0)])
        .collect()
}

fn genre(nal: &[u8]) -> u8 {
    nal.first().map_or(0, |o| o & 0x1f)
}

/// Le SPS et le PPS d'une trame clé, sans start code.
fn parametres(trame: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let liste = nals(trame);
    let sps = liste.iter().find(|n| genre(n) == NAL_SPS)?;
    let pps = liste.iter().find(|n| genre(n) == NAL_PPS)?;
    Some((sps.to_vec(), pps.to_vec()))
}

/// Une trame en Annex B, telle qu'un MP4 la veut : chaque NAL précédée de sa longueur.
/// Les paramètres sont dans l'en-tête, et un délimiteur n'a rien à faire ici.
fn en_longueurs(trame: &[u8]) -> Vec<u8> {
    let mut sortie = Vec::with_capacity(trame.len() + 16);
    for nal in nals(trame) {
        if matches!(genre(nal), NAL_SPS | NAL_PPS | NAL_AUD) || nal.is_empty() {
            continue;
        }
        sortie.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        sortie.extend_from_slice(nal);
    }
    sortie
}

// ---------------------------------------------------------------------------
// Les boîtes
// ---------------------------------------------------------------------------

fn boite(sortie: &mut Vec<u8>, nom: &[u8; 4], corps: impl FnOnce(&mut Vec<u8>)) {
    let debut = sortie.len();
    sortie.extend_from_slice(&[0; 4]);
    sortie.extend_from_slice(nom);
    corps(sortie);
    let taille = (sortie.len() - debut) as u32;
    sortie[debut..debut + 4].copy_from_slice(&taille.to_be_bytes());
}

fn pleine(sortie: &mut Vec<u8>, nom: &[u8; 4], version: u8, drapeaux: u32, corps: impl FnOnce(&mut Vec<u8>)) {
    boite(sortie, nom, |s| {
        s.push(version);
        s.extend_from_slice(&drapeaux.to_be_bytes()[1..]);
        corps(s);
    });
}

fn u16_(s: &mut Vec<u8>, v: u16) {
    s.extend_from_slice(&v.to_be_bytes());
}

fn u32_(s: &mut Vec<u8>, v: u32) {
    s.extend_from_slice(&v.to_be_bytes());
}

fn zeros(s: &mut Vec<u8>, n: usize) {
    s.resize(s.len() + n, 0);
}

/// La matrice identité d'un `mvhd` ou d'un `tkhd`.
fn matrice(s: &mut Vec<u8>) {
    for v in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        u32_(s, v);
    }
}

fn avc_c(s: &mut Vec<u8>, sps: &[u8], pps: &[u8]) {
    boite(s, b"avcC", |s| {
        s.push(1);
        // Le profil, ses contraintes et le niveau : les trois octets après l'en-tête.
        s.extend_from_slice(sps.get(1..4).unwrap_or(&[66, 0, 31]));
        s.push(0xff); // des longueurs sur 4 octets
        s.push(0xe1); // un SPS
        u16_(s, sps.len() as u16);
        s.extend_from_slice(sps);
        s.push(1); // un PPS
        u16_(s, pps.len() as u16);
        s.extend_from_slice(pps);
    });
}

fn esds(s: &mut Vec<u8>) {
    pleine(s, b"esds", 0, 0, |s| {
        // ES_Descriptor > DecoderConfig (AAC, audio) > la configuration, puis SLConfig.
        s.extend_from_slice(&[0x03, 25, 0, PISTE_SON as u8, 0]);
        s.extend_from_slice(&[0x04, 17, 0x40, 0x15, 0, 0, 0]);
        u32_(s, 128_000);
        u32_(s, 128_000);
        s.extend_from_slice(&[0x05, CONFIG_AAC.len() as u8]);
        s.extend_from_slice(&CONFIG_AAC);
        s.extend_from_slice(&[0x06, 1, 2]);
    });
}

/// Ce qui ne change pas d'une piste à l'autre dans un `stbl` : aucun échantillon,
/// ils sont tous dans les fragments.
fn tables_vides(s: &mut Vec<u8>) {
    for nom in [b"stts", b"stsc", b"stco"] {
        pleine(s, nom, 0, 0, |s| u32_(s, 0));
    }
    pleine(s, b"stsz", 0, 0, |s| zeros(s, 8));
}

fn dinf(s: &mut Vec<u8>) {
    boite(s, b"dinf", |s| {
        pleine(s, b"dref", 0, 0, |s| {
            u32_(s, 1);
            pleine(s, b"url ", 0, 1, |_| {});
        });
    });
}

fn hdlr(s: &mut Vec<u8>, genre: &[u8; 4], nom: &str) {
    pleine(s, b"hdlr", 0, 0, |s| {
        u32_(s, 0);
        s.extend_from_slice(genre);
        zeros(s, 12);
        s.extend_from_slice(nom.as_bytes());
        s.push(0);
    });
}

fn tkhd(s: &mut Vec<u8>, piste: u32, son: bool, largeur: u16, hauteur: u16) {
    // Drapeaux : activée, dans le film.
    pleine(s, b"tkhd", 0, 3, |s| {
        zeros(s, 8); // création, modification
        u32_(s, piste);
        zeros(s, 4 + 4 + 8); // réservé, durée, réservé
        zeros(s, 4); // couche, groupe
        u16_(s, if son { 0x0100 } else { 0 }); // volume
        zeros(s, 2);
        matrice(s);
        u32_(s, u32::from(largeur) << 16);
        u32_(s, u32::from(hauteur) << 16);
    });
}

fn mdhd(s: &mut Vec<u8>, echelle: u32) {
    pleine(s, b"mdhd", 0, 0, |s| {
        zeros(s, 8);
        u32_(s, echelle);
        u32_(s, 0);
        u16_(s, 0x55c4); // langue : « und »
        zeros(s, 2);
    });
}

/// `ftyp` et `moov` : ce qui ouvre le flux, et décrit les deux pistes.
pub fn entete(sps: &[u8], pps: &[u8], largeur: u16, hauteur: u16) -> Vec<u8> {
    let mut s = Vec::with_capacity(1024);
    boite(&mut s, b"ftyp", |s| {
        s.extend_from_slice(b"iso5");
        u32_(s, 512);
        for marque in [b"iso5", b"iso6", b"mp41"] {
            s.extend_from_slice(marque);
        }
    });
    boite(&mut s, b"moov", |s| {
        pleine(s, b"mvhd", 0, 0, |s| {
            zeros(s, 8);
            u32_(s, 1000);
            u32_(s, 0);
            u32_(s, 0x0001_0000); // vitesse
            u16_(s, 0x0100); // volume
            zeros(s, 2 + 8);
            matrice(s);
            zeros(s, 24);
            u32_(s, PISTE_SON + 1);
        });
        boite(s, b"trak", |s| {
            tkhd(s, PISTE_VIDEO, false, largeur, hauteur);
            boite(s, b"mdia", |s| {
                mdhd(s, ECHELLE_VIDEO);
                hdlr(s, b"vide", "ki-chat");
                boite(s, b"minf", |s| {
                    pleine(s, b"vmhd", 0, 1, |s| zeros(s, 8));
                    dinf(s);
                    boite(s, b"stbl", |s| {
                        pleine(s, b"stsd", 0, 0, |s| {
                            u32_(s, 1);
                            boite(s, b"avc1", |s| {
                                zeros(s, 6);
                                u16_(s, 1); // la référence de données
                                zeros(s, 16);
                                u16_(s, largeur);
                                u16_(s, hauteur);
                                u32_(s, 0x0048_0000); // 72 dpi
                                u32_(s, 0x0048_0000);
                                zeros(s, 4);
                                u16_(s, 1); // une image par échantillon
                                zeros(s, 32); // le nom du compresseur
                                u16_(s, 0x0018);
                                u16_(s, 0xffff);
                                avc_c(s, sps, pps);
                            });
                        });
                        tables_vides(s);
                    });
                });
            });
        });
        boite(s, b"trak", |s| {
            tkhd(s, PISTE_SON, true, 0, 0);
            boite(s, b"mdia", |s| {
                mdhd(s, ECHELLE_SON);
                hdlr(s, b"soun", "ki-chat");
                boite(s, b"minf", |s| {
                    pleine(s, b"smhd", 0, 0, |s| zeros(s, 4));
                    dinf(s);
                    boite(s, b"stbl", |s| {
                        pleine(s, b"stsd", 0, 0, |s| {
                            u32_(s, 1);
                            boite(s, b"mp4a", |s| {
                                zeros(s, 6);
                                u16_(s, 1);
                                zeros(s, 8);
                                u16_(s, 2); // stéréo
                                u16_(s, 16);
                                zeros(s, 4);
                                u32_(s, ECHELLE_SON << 16);
                                esds(s);
                            });
                        });
                        tables_vides(s);
                    });
                });
            });
        });
        boite(s, b"mvex", |s| {
            for (piste, duree, drapeaux) in [(PISTE_VIDEO, 0, 0), (PISTE_SON, ECHANTILLONS_AAC, DRAPEAUX_CLE)] {
                pleine(s, b"trex", 0, 0, |s| {
                    u32_(s, piste);
                    u32_(s, 1);
                    u32_(s, duree);
                    u32_(s, 0);
                    u32_(s, drapeaux);
                });
            }
        });
    });
    s
}

/// Un échantillon indépendant : trame clé, ou son.
const DRAPEAUX_CLE: u32 = 0x0200_0000;
/// Dépend d'autres images, et n'est pas un point d'entrée.
const DRAPEAUX_SUITE: u32 = 0x0101_0000;

/// Une image dans un fragment, en unités de la piste vidéo.
struct Image {
    temps: u64,
    duree: u32,
    cle: bool,
    donnees: Vec<u8>,
}

/// Un fragment : `numero`, les images, les trames de son qui commencent à `son_temps`.
fn fragment(numero: u32, images: &[Image], son_temps: u64, sons: &[Vec<u8>]) -> Vec<u8> {
    let taille_video: usize = images.iter().map(|i| i.donnees.len()).sum();
    let taille_son: usize = sons.iter().map(Vec::len).sum();
    let mut s = Vec::with_capacity(taille_video + taille_son + 256 + images.len() * 12 + sons.len() * 4);
    // Les décalages dépendent de la taille du `moof` : on les note, et on revient.
    let (mut place_video, mut place_son) = (None, None);
    boite(&mut s, b"moof", |s| {
        pleine(s, b"mfhd", 0, 0, |s| u32_(s, numero));
        if let Some(premiere) = images.first() {
            boite(s, b"traf", |s| {
                // Les décalages partent du début du `moof`.
                pleine(s, b"tfhd", 0, 0x02_0000, |s| u32_(s, PISTE_VIDEO));
                pleine(s, b"tfdt", 1, 0, |s| s.extend_from_slice(&premiere.temps.to_be_bytes()));
                // Décalage, durée, taille et drapeaux de chaque échantillon.
                pleine(s, b"trun", 0, 0x00_0701, |s| {
                    u32_(s, images.len() as u32);
                    place_video = Some(s.len());
                    u32_(s, 0);
                    for i in images {
                        u32_(s, i.duree);
                        u32_(s, i.donnees.len() as u32);
                        u32_(s, if i.cle { DRAPEAUX_CLE } else { DRAPEAUX_SUITE });
                    }
                });
            });
        }
        if !sons.is_empty() {
            boite(s, b"traf", |s| {
                pleine(s, b"tfhd", 0, 0x02_0000, |s| u32_(s, PISTE_SON));
                pleine(s, b"tfdt", 1, 0, |s| s.extend_from_slice(&son_temps.to_be_bytes()));
                // Décalage et taille : la durée et les drapeaux sont ceux de `trex`.
                pleine(s, b"trun", 0, 0x00_0201, |s| {
                    u32_(s, sons.len() as u32);
                    place_son = Some(s.len());
                    u32_(s, 0);
                    for son in sons {
                        u32_(s, son.len() as u32);
                    }
                });
            });
        }
    });
    let donnees = s.len() as u32 + 8;
    if let Some(p) = place_video {
        s[p..p + 4].copy_from_slice(&donnees.to_be_bytes());
    }
    if let Some(p) = place_son {
        s[p..p + 4].copy_from_slice(&(donnees + taille_video as u32).to_be_bytes());
    }
    boite(&mut s, b"mdat", |s| {
        for i in images {
            s.extend_from_slice(&i.donnees);
        }
        for son in sons {
            s.extend_from_slice(son);
        }
    });
    s
}

// ---------------------------------------------------------------------------
// Le flux
// ---------------------------------------------------------------------------

/// Ce que rend une trame donnée au flux.
#[derive(Debug, PartialEq, Eq)]
pub enum Sortie {
    /// Rien à envoyer pour l'instant.
    Rien,
    /// Des octets à envoyer au lecteur.
    Octets(Vec<u8>),
    /// Les paramètres vidéo ont changé (autre résolution) : un MP4 ne le permet pas
    /// en cours de route. Il faut que le lecteur recharge, et le flux repart de zéro.
    Recharger,
}

/// Une image reçue, qui attend de connaître sa durée : celle-ci vient de la suivante.
struct EnAttente {
    pts_us: u64,
    cle: bool,
    donnees: Vec<u8>,
}

/// Fabrique le MP4 d'un lecteur, de la première trame clé jusqu'à ce qu'il parte.
#[derive(Default)]
pub struct Flux {
    /// L'horodatage du temps zéro : la première trame clé.
    base_us: Option<u64>,
    parametres: Option<(Vec<u8>, Vec<u8>)>,
    dimensions: (u16, u16),
    numero: u32,
    attente: Option<EnAttente>,
    images: Vec<Image>,
    sons: Vec<Vec<u8>>,
    /// Le temps de la prochaine trame de son, en unités de la piste.
    son_temps: Option<u64>,
    /// Le temps de la première trame de `sons`.
    sons_debut: u64,
}

fn en_video(us: u64) -> u64 {
    us.saturating_mul(u64::from(ECHELLE_VIDEO)) / 1_000_000
}

fn en_son(us: u64) -> u64 {
    us.saturating_mul(u64::from(ECHELLE_SON)) / 1_000_000
}

impl Flux {
    /// Le flux a-t-il commencé ? Sinon, il attend une trame clé.
    pub fn commence(&self) -> bool {
        self.base_us.is_some()
    }

    /// Une trame en Annex B, dans l'ordre de lecture.
    pub fn image(&mut self, annexb: &[u8], cle: bool, pts_us: u64, largeur: u16, hauteur: u16) -> Sortie {
        let mut entete_a_envoyer = None;
        if cle {
            let nouveaux = parametres(annexb);
            if self.base_us.is_some()
                && ((nouveaux.is_some() && nouveaux != self.parametres) || (largeur, hauteur) != self.dimensions)
            {
                *self = Self::default();
                return Sortie::Recharger;
            }
            if self.base_us.is_none() {
                // Sans ses paramètres, cette trame clé ne vaut rien : on attend la suivante.
                let Some((sps, pps)) = nouveaux else { return Sortie::Rien };
                entete_a_envoyer = Some(entete(&sps, &pps, largeur, hauteur));
                self.parametres = Some((sps, pps));
                self.dimensions = (largeur, hauteur);
                self.base_us = Some(pts_us);
            }
        }
        let Some(base) = self.base_us else { return Sortie::Rien };
        let pts_us = pts_us.max(base);
        if let Some(a) = self.attente.take() {
            let temps = en_video(a.pts_us - base);
            // Une image a toujours une durée, même si les horodatages ne bougent pas.
            let duree = en_video(pts_us - base).saturating_sub(temps).clamp(1, u64::from(u32::MAX)) as u32;
            self.images.push(Image { temps, duree, cle: a.cle, donnees: a.donnees });
        }
        self.attente = Some(EnAttente { pts_us, cle, donnees: en_longueurs(annexb) });
        let video: u64 = self.images.iter().map(|i| u64::from(i.duree)).sum();
        let suite = (video >= FRAGMENT_VIDEO).then(|| self.vider());
        match (entete_a_envoyer, suite) {
            (None, None) => Sortie::Rien,
            (Some(mut e), Some(f)) => {
                e.extend_from_slice(&f);
                Sortie::Octets(e)
            }
            (Some(o), None) | (None, Some(o)) => Sortie::Octets(o),
        }
    }

    /// Une trame AAC, datée sur la même horloge que les images. Les trames doivent se
    /// suivre sans trou : c'est à l'appelant de combler les silences.
    pub fn son(&mut self, aac: Vec<u8>, pts_us: u64) -> Sortie {
        let Some(base) = self.base_us else { return Sortie::Rien };
        if pts_us < base {
            return Sortie::Rien;
        }
        let temps = *self.son_temps.get_or_insert_with(|| en_son(pts_us - base));
        if self.sons.is_empty() {
            self.sons_debut = temps;
        }
        self.sons.push(aac);
        self.son_temps = Some(temps + u64::from(ECHANTILLONS_AAC));
        if self.sons.len() >= FRAGMENT_SON {
            Sortie::Octets(self.vider())
        } else {
            Sortie::Rien
        }
    }

    fn vider(&mut self) -> Vec<u8> {
        self.numero += 1;
        let f = fragment(self.numero, &self.images, self.sons_debut, &self.sons);
        self.images.clear();
        self.sons.clear();
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: [u8; 6] = [0x67, 0x64, 0x00, 0x2a, 0xac, 0xd9];
    const PPS: [u8; 4] = [0x68, 0xeb, 0xe3, 0xcb];

    fn annexb(nals: &[&[u8]]) -> Vec<u8> {
        let mut s = Vec::new();
        for n in nals {
            s.extend_from_slice(&[0, 0, 0, 1]);
            s.extend_from_slice(n);
        }
        s
    }

    fn cle() -> Vec<u8> {
        annexb(&[&[0x09, 0xf0], &SPS, &PPS, &[0x65, 1, 2, 3, 4]])
    }

    fn suite(n: u8) -> Vec<u8> {
        annexb(&[&[0x41, n, n, n]])
    }

    /// Les boîtes de `octets`, à ce niveau : (nom, position du corps, fin).
    fn boites(octets: &[u8]) -> Vec<(String, usize, usize)> {
        let mut sortie = Vec::new();
        let mut i = 0;
        while i + 8 <= octets.len() {
            let taille = u32::from_be_bytes(octets[i..i + 4].try_into().unwrap()) as usize;
            assert!(taille >= 8 && i + taille <= octets.len(), "boîte mal formée en {i} : {taille}");
            sortie.push((String::from_utf8_lossy(&octets[i + 4..i + 8]).to_string(), i + 8, i + taille));
            i += taille;
        }
        assert_eq!(i, octets.len(), "des octets en trop");
        sortie
    }

    fn corps<'a>(octets: &'a [u8], nom: &str) -> &'a [u8] {
        let (_, debut, fin) = boites(octets).into_iter().find(|(n, _, _)| n == nom).unwrap_or_else(|| panic!("pas de {nom}"));
        &octets[debut..fin]
    }

    fn u32_a(octets: &[u8], i: usize) -> u32 {
        u32::from_be_bytes(octets[i..i + 4].try_into().unwrap())
    }

    #[test]
    fn les_start_codes_deviennent_des_longueurs() {
        let t = annexb(&[&[0x09, 0xf0], &SPS, &PPS, &[0x65, 1, 2], &[0x41, 9]]);
        assert_eq!(en_longueurs(&t), [&[0, 0, 0, 3, 0x65, 1, 2][..], &[0, 0, 0, 2, 0x41, 9]].concat());
    }

    #[test]
    fn les_start_codes_courts_aussi() {
        let t = [0, 0, 1, 0x65, 1, 0, 0, 1, 0x41, 2];
        assert_eq!(en_longueurs(&t), [0, 0, 0, 2, 0x65, 1, 0, 0, 0, 2, 0x41, 2]);
    }

    #[test]
    fn l_entete_decrit_les_deux_pistes() {
        let e = entete(&SPS, &PPS, 1920, 1080);
        let noms: Vec<_> = boites(&e).into_iter().map(|b| b.0).collect();
        assert_eq!(noms, ["ftyp", "moov"]);
        let moov = corps(&e, "moov");
        let noms: Vec<_> = boites(moov).into_iter().map(|b| b.0).collect();
        assert_eq!(noms, ["mvhd", "trak", "trak", "mvex"]);
        // Le SPS et le PPS sont bien dans la description de la vidéo.
        let cherche = |motif: &[u8]| e.windows(motif.len()).any(|w| w == motif);
        assert!(cherche(&[1, 0x64, 0x00, 0x2a, 0xff, 0xe1, 0, 6, 0x67, 0x64]), "avcC");
        assert!(cherche(&[1, 0, 4, 0x68, 0xeb, 0xe3, 0xcb]), "PPS");
        assert!(cherche(&CONFIG_AAC), "la configuration AAC");
    }

    #[test]
    fn la_taille_des_descripteurs_du_son_est_juste() {
        // Une taille fausse dans `esds` et le lecteur refuse toute la piste.
        let mut s = Vec::new();
        esds(&mut s);
        let d = &s[12..];
        assert_eq!(d[0], 0x03);
        assert_eq!(d[1] as usize, d.len() - 2, "ES_Descriptor");
        assert_eq!(d[5], 0x04);
        assert_eq!(d[6] as usize, 2 + 13 + 2, "DecoderConfig");
        assert_eq!(d[7 + 13], 0x05);
    }

    #[test]
    fn un_fragment_pointe_sur_les_bons_octets() {
        let images = [
            Image { temps: 9000, duree: 3000, cle: true, donnees: vec![1, 2, 3] },
            Image { temps: 12000, duree: 3000, cle: false, donnees: vec![4, 5] },
        ];
        let sons = [vec![7, 7, 7, 7], vec![8, 8]];
        let f = fragment(5, &images, 4800, &sons);
        let noms: Vec<_> = boites(&f).into_iter().map(|b| b.0).collect();
        assert_eq!(noms, ["moof", "mdat"]);
        assert_eq!(corps(&f, "mdat"), [1, 2, 3, 4, 5, 7, 7, 7, 7, 8, 8]);

        let moof = corps(&f, "moof");
        assert_eq!(u32_a(corps(moof, "mfhd"), 4), 5);
        let trafs: Vec<_> = boites(moof).into_iter().filter(|b| b.0 == "traf").collect();
        assert_eq!(trafs.len(), 2);

        let video = &moof[trafs[0].1..trafs[0].2];
        assert_eq!(u32_a(corps(video, "tfhd"), 4), PISTE_VIDEO);
        assert_eq!(u64::from_be_bytes(corps(video, "tfdt")[4..12].try_into().unwrap()), 9000);
        let trun = corps(video, "trun");
        assert_eq!(u32_a(trun, 4), 2, "deux images");
        let decalage = u32_a(trun, 8) as usize;
        assert_eq!(&f[decalage..decalage + 3], [1, 2, 3], "la vidéo commence là");
        assert_eq!(u32_a(trun, 12), 3000);
        assert_eq!(u32_a(trun, 16), 3);
        assert_eq!(u32_a(trun, 20), DRAPEAUX_CLE);
        assert_eq!(u32_a(trun, 32), DRAPEAUX_SUITE);

        let son = &moof[trafs[1].1..trafs[1].2];
        assert_eq!(u32_a(corps(son, "tfhd"), 4), PISTE_SON);
        assert_eq!(u64::from_be_bytes(corps(son, "tfdt")[4..12].try_into().unwrap()), 4800);
        let trun = corps(son, "trun");
        let decalage = u32_a(trun, 8) as usize;
        assert_eq!(&f[decalage..decalage + 4], [7, 7, 7, 7], "le son commence là");
        assert_eq!(u32_a(trun, 12), 4);
        assert_eq!(u32_a(trun, 16), 2);
    }

    #[test]
    fn un_fragment_sans_image_ne_decrit_que_le_son() {
        let f = fragment(1, &[], 0, &[vec![1]]);
        let trafs = boites(corps(&f, "moof")).into_iter().filter(|b| b.0 == "traf").count();
        assert_eq!(trafs, 1);
    }

    fn octets(s: Sortie) -> Vec<u8> {
        match s {
            Sortie::Octets(o) => o,
            autre => panic!("attendu des octets, reçu {autre:?}"),
        }
    }

    #[test]
    fn le_flux_attend_une_trame_cle() {
        let mut f = Flux::default();
        assert_eq!(f.image(&suite(1), false, 1000, 640, 360), Sortie::Rien);
        assert_eq!(f.son(vec![1], 1000), Sortie::Rien);
        assert!(!f.commence());
        let o = octets(f.image(&cle(), true, 5000, 640, 360));
        assert!(f.commence());
        let noms: Vec<_> = boites(&o).into_iter().map(|b| b.0).collect();
        assert_eq!(noms, ["ftyp", "moov"]);
    }

    #[test]
    fn une_trame_cle_sans_parametres_ne_fait_pas_commencer() {
        let mut f = Flux::default();
        let sans = annexb(&[&[0x65, 1, 2]]);
        assert_eq!(f.image(&sans, true, 1000, 640, 360), Sortie::Rien);
        assert!(!f.commence());
    }

    #[test]
    fn le_temps_part_de_zero_et_les_durees_viennent_des_horodatages() {
        let mut f = Flux::default();
        let base = 7_000_000;
        octets(f.image(&cle(), true, base, 640, 360));
        let mut sortie = Vec::new();
        for i in 1..10u64 {
            if let Sortie::Octets(o) = f.image(&suite(i as u8), false, base + i * 33_333, 640, 360) {
                sortie = o;
                break;
            }
        }
        let moof = corps(&sortie, "moof");
        let traf = corps(moof, "traf");
        assert_eq!(u64::from_be_bytes(corps(traf, "tfdt")[4..12].try_into().unwrap()), 0);
        let trun = corps(traf, "trun");
        let n = u32_a(trun, 4) as usize;
        assert!(n >= 3, "{n} images");
        // 33,333 ms à 90 kHz : 2999 ou 3000, sans cumuler d'erreur.
        let somme: u64 = (0..n).map(|i| u64::from(u32_a(trun, 12 + i * 12))).sum();
        assert_eq!(somme, en_video(n as u64 * 33_333));
    }

    #[test]
    fn le_son_seul_continue_quand_l_image_est_immobile() {
        let mut f = Flux::default();
        octets(f.image(&cle(), true, 0, 640, 360));
        let mut temps = 0u64;
        let mut fragments = Vec::new();
        for i in 0..FRAGMENT_SON as u64 * 2 {
            if let Sortie::Octets(o) = f.son(vec![i as u8], i * 21_333) {
                fragments.push(o);
            }
        }
        assert_eq!(fragments.len(), 2);
        for o in fragments {
            let traf = corps(corps(&o, "moof"), "traf");
            assert_eq!(u32_a(corps(traf, "tfhd"), 4), PISTE_SON);
            assert_eq!(u64::from_be_bytes(corps(traf, "tfdt")[4..12].try_into().unwrap()), temps);
            temps += FRAGMENT_SON as u64 * u64::from(ECHANTILLONS_AAC);
        }
    }

    #[test]
    fn le_son_commence_a_sa_date_et_ignore_ce_qui_precede_le_debut() {
        let mut f = Flux::default();
        octets(f.image(&cle(), true, 1_000_000, 640, 360));
        assert_eq!(f.son(vec![9], 999_000), Sortie::Rien);
        for i in 0..FRAGMENT_SON as u64 - 1 {
            assert_eq!(f.son(vec![1], 1_100_000 + i * 21_333), Sortie::Rien);
        }
        let o = octets(f.son(vec![1], 1_400_000));
        let traf = corps(corps(&o, "moof"), "traf");
        // 100 ms après le début : 4800 échantillons.
        assert_eq!(u64::from_be_bytes(corps(traf, "tfdt")[4..12].try_into().unwrap()), 4800);
    }

    #[test]
    fn une_autre_resolution_demande_de_recharger_puis_repart() {
        let mut f = Flux::default();
        octets(f.image(&cle(), true, 0, 1920, 1080));
        assert_ne!(f.image(&cle(), true, 2_000_000, 1920, 1080), Sortie::Recharger, "mêmes paramètres : on continue");
        assert_eq!(f.image(&cle(), true, 4_000_000, 1280, 720), Sortie::Recharger);
        assert!(!f.commence());
        let o = octets(f.image(&cle(), true, 4_033_000, 1280, 720));
        assert_eq!(boites(&o)[0].0, "ftyp");
    }

    #[test]
    fn un_autre_sps_a_la_meme_taille_demande_aussi_de_recharger() {
        let mut f = Flux::default();
        octets(f.image(&cle(), true, 0, 640, 360));
        let autre = annexb(&[&[0x67, 0x42, 0, 0x1f], &PPS, &[0x65, 1]]);
        assert_eq!(f.image(&autre, true, 1000, 640, 360), Sortie::Recharger);
    }

    #[test]
    fn une_trame_cle_sans_parametres_en_route_ne_coupe_pas_le_flux() {
        let mut f = Flux::default();
        octets(f.image(&cle(), true, 0, 640, 360));
        assert_eq!(f.image(&annexb(&[&[0x65, 1]]), true, 1000, 640, 360), Sortie::Rien);
        assert!(f.commence());
    }

    #[test]
    fn un_horodatage_qui_recule_ne_casse_pas_les_durees() {
        let mut f = Flux::default();
        octets(f.image(&cle(), true, 1_000_000, 640, 360));
        let _ = f.image(&suite(1), false, 900_000, 640, 360);
        let mut sortie = None;
        for i in 2..20u64 {
            if let Sortie::Octets(o) = f.image(&suite(i as u8), false, 1_000_000 + i * 33_333, 640, 360) {
                sortie = Some(o);
                break;
            }
        }
        let o = sortie.expect("un fragment");
        let trun = corps(corps(corps(&o, "moof"), "traf"), "trun");
        for i in 0..u32_a(trun, 4) as usize {
            assert!(u32_a(trun, 12 + i * 12) >= 1, "une durée nulle ou négative");
        }
    }
}
