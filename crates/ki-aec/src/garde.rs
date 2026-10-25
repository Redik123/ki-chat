//! La garde d'écho : le seuil d'ouverture du micro suit ce que joue le
//! haut-parleur.
//!
//! Même annulé, l'écho d'un haut-parleur de téléphone poussé à fond n'est
//! pas nul : sur un Galaxy Z Flip 6 au maximum, le filtre en retirait 15 à
//! 25 dB, et ce qui restait passait encore le seuil de volume, et Silero y
//! entendait une voix — les autres entendaient leur musique revenir. Un seuil
//! fixe plus haut aurait coupé la voix de celui qui parle au téléphone.
//!
//! La [`GardeEcho`] apprend, trame par trame, la part du son joué qui revient
//! encore à l'endroit où l'on juge la parole : le rapport entre la crête de
//! la capture et celle du lointain qui vient d'être joué. Chaque seconde,
//! elle en retient le niveau sous lequel tombent 80 % des trames — pas le
//! minimum : le débruiteur écrase certaines trames d'écho presque à zéro, et
//! le minimum plaçait la garde 20 dB sous l'écho, où elle ne servait à rien.
//! Puis le plus bas de ces niveaux sur cinq secondes : une seconde où l'on
//! parle par-dessus fait monter le rapport, elle ne compte pas. Pendant que le haut-parleur joue, le
//! micro n'ouvre alors que sur une crête nettement au-dessus de cet écho
//! attendu. Rien ne joue : pas de garde, le seuil de volume seul décide.

use std::collections::VecDeque;

/// Le lointain pris en compte : ce qui a été joué sur les 200 dernières ms
/// (dix trames), le temps que l'écho revienne et que les tampons passent.
const FENETRE_LOIN: usize = 10;
/// Un niveau par seconde (50 trames de 20 ms) ; on en garde cinq.
const TRAMES_PAR_MESURE: usize = 50;
const MESURES: usize = 5;
/// Le niveau d'une seconde : celui sous lequel tombent 80 % de ses trames.
const CENTILE: f32 = 0.8;
/// En dessous, le haut-parleur ne joue rien qui puisse revenir.
const LOIN_ACTIF: f32 = 1e-3;
/// Ce qui revient du haut-parleur, une fois l'annulateur passé, ne dépasse
/// pas −20 dB du son joué (mesuré sur un Galaxy Z Flip 6 à fond : −22 à
/// −38 dB). Au-delà, c'est quelqu'un qui parle ici : qui parle longtemps
/// par-dessus la musique ne doit pas apprendre à la garde sa propre voix, et
/// se faire couper le début de la phrase suivante.
const RETOUR_MAX: f32 = 0.1;
/// La marge au-dessus de l'écho attendu : +6 dB. Une voix au téléphone, elle,
/// arrive bien au-dessus — mesuré sur un Galaxy Z Flip 6, 10 à 25 dB.
const MARGE: f32 = 2.0;

/// Apprend ce qui revient du haut-parleur, et en tire une garde.
pub struct GardeEcho {
    loin: VecDeque<f32>,
    /// Les niveaux des dernières secondes.
    mesures: VecDeque<f32>,
    /// Les rapports de la seconde en cours.
    rapports: Vec<f32>,
}

impl Default for GardeEcho {
    fn default() -> Self {
        Self::new()
    }
}

impl GardeEcho {
    pub fn new() -> Self {
        Self {
            loin: VecDeque::with_capacity(FENETRE_LOIN + 1),
            mesures: VecDeque::with_capacity(MESURES + 1),
            rapports: Vec::with_capacity(TRAMES_PAR_MESURE),
        }
    }

    /// Une trame : la crête du lointain qui vient d'être joué (recalé, s'il
    /// l'est) et celle de la capture là où l'on juge la parole. Rend la crête
    /// que la capture doit passer pour ouvrir le micro — 0 quand rien ne joue,
    /// ou tant que l'écho n'est pas encore mesuré.
    pub fn trame(&mut self, crete_loin: f32, crete_capture: f32) -> f32 {
        self.loin.push_back(crete_loin.abs());
        while self.loin.len() > FENETRE_LOIN {
            self.loin.pop_front();
        }
        let loin = self.loin.iter().copied().fold(0f32, f32::max);
        if loin < LOIN_ACTIF {
            return 0.0;
        }
        self.rapports.push(crete_capture.abs() / loin);
        if self.rapports.len() >= TRAMES_PAR_MESURE {
            self.rapports.sort_by(f32::total_cmp);
            let rang = ((self.rapports.len() as f32 * CENTILE) as usize).min(self.rapports.len() - 1);
            self.mesures.push_back(self.rapports[rang]);
            while self.mesures.len() > MESURES {
                self.mesures.pop_front();
            }
            self.rapports.clear();
        }
        match self.mesures.iter().copied().reduce(f32::min) {
            Some(retour) => loin * retour.min(RETOUR_MAX) * MARGE,
            None => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un écho qui revient à −20 dB : passé la première seconde, la garde se
    /// place au-dessus de lui ; une voix qui arrive par-dessus la passe.
    #[test]
    fn la_garde_se_place_au_dessus_de_l_echo() {
        let mut g = GardeEcho::new();
        let mut x: u32 = 1;
        let mut hasard = || {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            (x >> 8) as f32 / (1u32 << 24) as f32
        };
        for n in 0..200 {
            let loin = 0.3 + 0.2 * hasard();
            // L'écho : un dixième du lointain, qui fluctue de ±3 dB.
            let echo = loin * 0.1 * (0.7 + 0.7 * hasard());
            let garde = g.trame(loin, echo);
            if n < 49 {
                assert_eq!(garde, 0.0, "pas de garde avant la première mesure");
            } else if n >= 100 {
                // Au-dessus de l'écho de chaque trame…
                assert!(garde > echo, "trame {n} : garde {garde} sous l'écho {echo}");
                // …et sous une voix au téléphone, à la moitié de la pleine échelle.
                assert!(garde < 0.5, "trame {n} : garde {garde} trop haute pour une voix");
            }
        }
    }

    /// Le débruiteur écrase des trames d'écho à presque rien : elles ne
    /// tirent pas la garde sous l'écho qui, lui, passe.
    #[test]
    fn des_trames_ecrasees_ne_tirent_pas_la_garde_vers_le_bas() {
        let mut g = GardeEcho::new();
        let mut garde = 0.0;
        for n in 0..200 {
            // Une trame sur trois écrasée, les autres à −20 dB du lointain.
            let echo = if n % 3 == 0 { 0.0001 } else { 0.04 };
            garde = g.trame(0.4, echo);
        }
        assert!(garde > 0.04, "garde {garde} sous l'écho qui passe");
    }

    /// Quand on parle par-dessus, le rapport monte : il ne compte pas, et la
    /// garde ne grimpe pas jusqu'à couper la voix.
    #[test]
    fn parler_par_dessus_ne_fait_pas_monter_la_garde() {
        let mut g = GardeEcho::new();
        for _ in 0..60 {
            g.trame(0.4, 0.04);
        }
        let avant = g.trame(0.4, 0.04);
        for _ in 0..40 {
            g.trame(0.4, 0.6);
        }
        let apres = g.trame(0.4, 0.04);
        assert!(apres <= avant * 1.01, "la garde a monté : {avant} → {apres}");
    }

    /// Parler longtemps par-dessus : la garde n'apprend pas cette voix-là, et
    /// reste sous elle.
    #[test]
    fn parler_longtemps_par_dessus_ne_coupe_pas_la_voix() {
        let mut g = GardeEcho::new();
        let mut garde = 0.0;
        for _ in 0..400 {
            garde = g.trame(0.3, 0.45);
        }
        assert!(garde < 0.45 * 0.5, "garde {garde} au niveau de la voix");
    }

    /// Rien ne joue : pas de garde, le seuil de volume seul décide.
    #[test]
    fn sans_rien_au_haut_parleur_pas_de_garde() {
        let mut g = GardeEcho::new();
        for _ in 0..60 {
            g.trame(0.4, 0.04);
        }
        for _ in 0..FENETRE_LOIN {
            g.trame(0.0, 0.3);
        }
        assert_eq!(g.trame(0.0, 0.3), 0.0);
    }
}
