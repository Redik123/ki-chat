//! La dynamique de la voix : compresseur et limiteur, échantillon par
//! échantillon.
//!
//! Deux usages :
//! - **à l'émission**, en fin de chaîne micro : le compresseur (réglable)
//!   contient les éclats de voix, le limiteur (toujours là) empêche tout
//!   dépassement de la pleine échelle. C'est ce qui manquait quand « ça part
//!   en couille » en parlant fort : le gain automatique ne réagissait qu'une
//!   fois par trame de 20 ms, si bien qu'une phrase forte après une phrase
//!   douce arrivait multipliée, écrêtée pendant ses 60 à 80 premières ms ;
//! - **à l'écoute**, un compresseur par personne entendue (« adoucir les
//!   cris ») : celui qui hurle dans son micro redescend, les autres ne
//!   bougent pas.
//!
//! Tout se calcule en temps réel sur le fil audio : pas d'allocation, et le
//! passage en décibels (deux fonctions transcendantes) n'a lieu qu'au-dessus
//! du genou — la voix ordinaire traverse sur le chemin rapide.

use crate::SAMPLE_RATE;

/// Coefficient d'un lissage exponentiel de constante de temps `ms`.
fn coefficient(ms: f32) -> f32 {
    if ms <= 0.0 {
        return 1.0;
    }
    1.0 - (-1.0 / (ms / 1000.0 * SAMPLE_RATE as f32)).exp()
}

/// ln(10) / 20 : décibels vers logarithme népérien d'un gain.
const DB_VERS_LN: f32 = std::f32::consts::LN_10 / 20.0;
/// 20 / ln(10) : logarithme népérien d'un niveau vers décibels.
const LN_VERS_DB: f32 = 20.0 / std::f32::consts::LN_10;

/// Compresseur à genou doux sur une enveloppe crête.
///
/// L'enveloppe suit la valeur absolue du signal : vite quand il monte
/// (`attaque`), doucement quand il retombe (`relachement`). Au-dessus du
/// seuil, chaque décibel de trop n'en laisse passer que `1 / ratio` ; le
/// genou arrondit la transition sur `genou_db`, sans quoi le passage du seuil
/// s'entend comme un pompage.
#[derive(Clone, Debug)]
pub struct Compresseur {
    seuil_db: f32,
    ratio: f32,
    genou_db: f32,
    /// Sous ce niveau (bas du genou), rien à faire : le chemin rapide.
    bas_du_genou: f32,
    attaque: f32,
    relachement: f32,
    enveloppe: f32,
}

impl Compresseur {
    pub fn new(seuil_db: f32, ratio: f32, genou_db: f32, attaque_ms: f32, relachement_ms: f32) -> Self {
        Self {
            seuil_db,
            ratio: ratio.max(1.0),
            genou_db: genou_db.max(0.0),
            bas_du_genou: ((seuil_db - genou_db / 2.0) * DB_VERS_LN).exp(),
            attaque: coefficient(attaque_ms),
            relachement: coefficient(relachement_ms),
            enveloppe: 0.0,
        }
    }

    /// Le compresseur d'une voix qu'on émet, selon le réglage choisi
    /// (`COMPRESSION_*`) ; `None` pour aucune compression.
    pub fn emission(niveau: u8) -> Option<Self> {
        match niveau {
            // Douce : ne touche qu'aux éclats, la voix ordinaire passe telle
            // quelle.
            COMPRESSION_DOUCE => Some(Self::new(-14.0, 3.0, 6.0, 5.0, 120.0)),
            // Forte : pour qui crie souvent — la voix entière est tenue.
            COMPRESSION_FORTE => Some(Self::new(-20.0, 6.0, 6.0, 3.0, 150.0)),
            _ => None,
        }
    }

    /// Le compresseur d'une voix qu'on entend (« adoucir les cris »).
    pub fn ecoute() -> Self {
        Self::new(-12.0, 4.0, 6.0, 3.0, 200.0)
    }

    /// Réduction de gain (linéaire, ≤ 1) pour un niveau d'enveloppe donné.
    fn gain_pour(&self, enveloppe: f32) -> f32 {
        if enveloppe <= self.bas_du_genou {
            return 1.0;
        }
        let depassement = enveloppe.ln() * LN_VERS_DB - self.seuil_db;
        let pente = 1.0 / self.ratio - 1.0;
        let reduction_db = if self.genou_db > 0.0 && 2.0 * depassement <= self.genou_db {
            let x = depassement + self.genou_db / 2.0;
            pente * x * x / (2.0 * self.genou_db)
        } else {
            pente * depassement
        };
        (reduction_db * DB_VERS_LN).exp()
    }

    /// Un échantillon compressé.
    #[inline]
    pub fn traiter(&mut self, x: f32) -> f32 {
        let a = x.abs();
        let coef = if a > self.enveloppe { self.attaque } else { self.relachement };
        self.enveloppe += (a - self.enveloppe) * coef;
        x * self.gain_pour(self.enveloppe)
    }

    pub fn traiter_trame(&mut self, trame: &mut [f32]) {
        for s in trame.iter_mut() {
            *s = self.traiter(*s);
        }
    }
}

/// Limiteur de crête : aucun échantillon ne dépasse le plafond.
///
/// Attaque instantanée — le gain tombe à l'échantillon même qui dépasserait
/// —, relâchement exponentiel ensuite. Sans anticipation (et donc sans
/// latence), il plie la crête qui l'a déclenché ; le compresseur en amont
/// fait que cela reste rare et bref, là où l'ancien écrêtage durait le temps
/// que le gain automatique redescende.
#[derive(Clone, Debug)]
pub struct Limiteur {
    plafond: f32,
    relachement: f32,
    gain: f32,
}

impl Limiteur {
    /// -1 dBFS : la marge que gardent les encodeurs pour leurs propres
    /// dépassements entre échantillons.
    pub const PLAFOND: f32 = 0.891;

    pub fn new() -> Self {
        Self { plafond: Self::PLAFOND, relachement: coefficient(60.0), gain: 1.0 }
    }

    #[inline]
    pub fn traiter(&mut self, x: f32) -> f32 {
        let a = x.abs();
        let voulu = if a > self.plafond { self.plafond / a } else { 1.0 };
        if voulu < self.gain {
            self.gain = voulu;
        } else {
            self.gain += (voulu - self.gain) * self.relachement;
        }
        x * self.gain
    }

    pub fn traiter_trame(&mut self, trame: &mut [f32]) {
        for s in trame.iter_mut() {
            *s = self.traiter(*s);
        }
    }
}

impl Default for Limiteur {
    fn default() -> Self {
        Self::new()
    }
}

/// Pas de compression à l'émission.
pub const COMPRESSION_AUCUNE: u8 = 0;
/// Compression douce (réglage par défaut) : les éclats seulement.
pub const COMPRESSION_DOUCE: u8 = 1;
/// Compression forte : toute la voix tenue.
pub const COMPRESSION_FORTE: u8 = 2;

/// Le micro a-t-il saturé dans cette trame, **avant** tout traitement ?
///
/// Un signal écrêté par la carte son (niveau Windows trop haut, micro trop
/// sensible) arrive plafonné à la pleine échelle : plusieurs échantillons
/// collés au maximum. Aucun traitement ne rend ce qui a été coupé — il faut
/// baisser le niveau du micro à la source, et c'est ce que l'interface dira.
pub fn trame_saturee(trame: &[f32]) -> bool {
    trame.iter().filter(|s| s.abs() >= 0.985).take(3).count() >= 3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sinus(amplitude: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amplitude * (i as f32 * 2.0 * std::f32::consts::PI * 220.0 / SAMPLE_RATE as f32).sin())
            .collect()
    }

    fn crete(x: &[f32]) -> f32 {
        x.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    /// Une voix sous le seuil traverse sans une retouche.
    #[test]
    fn sous_le_seuil_rien_ne_bouge() {
        let mut c = Compresseur::emission(COMPRESSION_DOUCE).unwrap();
        let entree = sinus(0.05, 9600);
        let mut sortie = entree.clone();
        c.traiter_trame(&mut sortie);
        assert_eq!(entree, sortie);
    }

    /// Un cri à -1 dBFS ressort nettement plus bas, conforme au ratio :
    /// 13 dB au-dessus du seuil de -14 dBFS, divisés par 3.
    #[test]
    fn un_cri_redescend_selon_le_ratio() {
        let mut c = Compresseur::emission(COMPRESSION_DOUCE).unwrap();
        let mut signal = sinus(0.89, 48_000);
        c.traiter_trame(&mut signal);
        // Régime établi : la dernière demi-seconde.
        let sortie_db = 20.0 * crete(&signal[24_000..]).log10();
        let attendu = -14.0 + 13.0 / 3.0;
        assert!((sortie_db - attendu).abs() < 1.5, "{sortie_db:.1} dB au lieu de {attendu:.1}");
    }

    /// La forte tient plus que la douce.
    #[test]
    fn la_forte_tient_plus_que_la_douce() {
        let mesure = |niveau| {
            let mut c = Compresseur::emission(niveau).unwrap();
            let mut s = sinus(0.7, 48_000);
            c.traiter_trame(&mut s);
            crete(&s[24_000..])
        };
        assert!(mesure(COMPRESSION_FORTE) < mesure(COMPRESSION_DOUCE) * 0.8);
    }

    /// Après un cri, la voix normale retrouve son niveau : le relâchement
    /// rend le gain en quelques centaines de millisecondes.
    #[test]
    fn apres_le_cri_la_voix_revient() {
        let mut c = Compresseur::emission(COMPRESSION_DOUCE).unwrap();
        let mut cri = sinus(0.89, 9_600);
        c.traiter_trame(&mut cri);
        let mut calme = sinus(0.1, 48_000);
        c.traiter_trame(&mut calme);
        assert!(crete(&calme[38_400..]) > 0.098, "le gain n'est pas revenu");
    }

    /// Quoi qu'on lui donne, le limiteur ne dépasse jamais son plafond.
    #[test]
    fn le_limiteur_ne_depasse_jamais() {
        let mut l = Limiteur::new();
        for amplitude in [0.5, 0.95, 1.5, 4.0, 16.0] {
            let mut s = sinus(amplitude, 4_800);
            l.traiter_trame(&mut s);
            assert!(crete(&s) <= Limiteur::PLAFOND + 1e-6, "{amplitude} : {}", crete(&s));
        }
    }

    /// Un signal sous le plafond traverse le limiteur intact.
    #[test]
    fn le_limiteur_laisse_passer_ce_qui_tient() {
        let mut l = Limiteur::new();
        let entree = sinus(0.5, 4_800);
        let mut sortie = entree.clone();
        l.traiter_trame(&mut sortie);
        assert_eq!(entree, sortie);
    }

    #[test]
    fn la_saturation_se_voit_avant_traitement() {
        let mut trame = sinus(0.3, 960);
        assert!(!trame_saturee(&trame));
        // Une crête écrêtée par la carte son : plusieurs échantillons collés
        // à la pleine échelle.
        for s in trame[100..110].iter_mut() {
            *s = 1.0;
        }
        assert!(trame_saturee(&trame));
    }
}
