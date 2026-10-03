//! Le gain automatique de sa voix : une crête de parole constante, et une
//! adaptation qui ne se fait que sur **sa** voix (verdict de proximité).

/// Gain automatique : vise une crête de parole constante — et ne s'ajuste
/// que sur la **voix**. L'ancien seuil absolu prenait le bruit de fond d'un
/// micro un peu chargé (clavier, souffle que RNNoise laisse passer) pour de
/// la parole et le gonflait ×8 entre les phrases : le « souffle montant »
/// entendu sur le terrain. Un plancher de bruit glissant sépare désormais
/// les deux : il suit la crête vers le bas sans délai (une respiration
/// suffit), remonte lentement, et la voix doit le dominer nettement.
///
/// Trois conséquences voulues : le bruit stable n'est plus jamais amplifié ;
/// pendant les silences le gain reflue vers le neutre au lieu de rester
/// gonflé (le premier mot n'écrête plus) ; et la remontée est d'autant plus
/// vive que le gain est loin du compte — après un cri, une phrase douce
/// retrouve son niveau en quelques trames, pas en une demi-seconde.
///
/// Le gain glisse d'un bout à l'autre de la trame au lieu de sauter d'une
/// trame à la suivante : ces marches de 20 ms s'entendaient comme un grain.
/// Et il n'écrête plus lui-même : une attaque sur-amplifiée (une phrase forte
/// après une douce, le gain encore haut) passait par un écrêtage doux jusqu'à
/// ce que le gain redescende — 60 à 80 ms de saturation à chaque éclat. Le
/// compresseur et le limiteur de fin de chaîne (`dynamique`) la prennent
/// désormais, à l'échantillon près.
pub struct Agc {
    gain: f32,
    /// Plancher de bruit estimé (crête des moments les plus calmes).
    floor: f32,
    /// Le gain atteint sur la dernière trame **sûre** (sa voix, à coup sûr) :
    /// une trame seulement proche ne peut pas monter plus de 6 dB au-dessus.
    gain_sur: f32,
}

/// Sur quoi le gain peut s'adapter pour la trame en cours — le verdict de
/// proximité, traduit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adaptation {
    /// Sa voix, à coup sûr : le gain vise la cible et retient où il en est.
    Sure,
    /// Proche, mais nettement sous sa voix (une fin de mot, une voix douce —
    /// ou une voix d'à côté un peu forte) : le gain peut suivre, mais pas
    /// plus de 6 dB au-dessus de celui de sa voix sûre. C'est ce qui empêche
    /// de remonter la voisine au niveau de la sienne dès qu'elle hausse le
    /// ton.
    Proche,
    /// Lointaine, ou rien : le gain ne s'adapte pas et reflue vers le neutre.
    Non,
}

impl Agc {
    /// En-dessous, silence absolu : ni voix, ni bruit exploitable.
    const GATE: f32 = 0.015;
    /// La voix doit dominer le plancher de bruit d'au moins ce facteur.
    const VOICE_OVER_FLOOR: f32 = 3.0;
    /// Le plancher ne descend jamais sous GATE/3 : sans ce garde-fou, un
    /// vrai silence l'écrasait à zéro et le premier bruit venu redevenait
    /// « de la voix ».
    const FLOOR_MIN: f32 = Self::GATE / 3.0;
    /// Crête qu'aucune trame ne dépasse à la sortie du gain automatique.
    const CRETE_MAX: f32 = 0.9;

    pub fn new() -> Self {
        Self { gain: 1.0, floor: Self::FLOOR_MIN, gain_sur: 1.0 }
    }

    /// Le gain en vigueur (1 : neutre).
    pub fn gain(&self) -> f32 {
        self.gain
    }

    /// `adapter` : la trame est bien SA voix (verdict de proximité) ; sinon
    /// le gain ne s'adapte pas — une voix lointaine n'est jamais remontée.
    pub fn process(&mut self, frame: &mut [f32], target: f32, adaptation: Adaptation) {
        let mut avant = self.gain;
        let peak = frame.iter().fold(0f32, |m, s| m.max(s.abs()));
        // Plancher : tombe immédiatement sur une trame calme, remonte
        // lentement (~2 % par trame) — il s'établit en une seconde ou deux,
        // et une seule respiration le remet en place.
        if peak < self.floor {
            self.floor = peak.max(Self::FLOOR_MIN);
        } else {
            self.floor = (self.floor * 1.02).min(0.25);
        }
        let voiced =
            adaptation != Adaptation::Non && peak >= Self::GATE && peak >= self.floor * Self::VOICE_OVER_FLOOR;
        if voiced {
            let mut desired = (target / peak).clamp(0.2, 8.0);
            if adaptation == Adaptation::Proche {
                desired = desired.min(self.gain_sur * 2.0);
            }
            // Baisse vite (anti-saturation) ; monte vite quand on est loin
            // du compte, doucement près de lui (anti-pompage).
            let rate = if desired < self.gain {
                0.5
            } else if desired > self.gain * 2.0 {
                0.2
            } else {
                0.02
            };
            self.gain += (desired - self.gain) * rate;
            if adaptation == Adaptation::Sure {
                self.gain_sur = self.gain;
            }
        } else {
            // Bruit ou silence : retour progressif au neutre.
            self.gain += (1.0 - self.gain) * 0.005;
        }
        // Attaque instantanée contre la saturation : la trame entière est là,
        // sa crête est connue avant d'appliquer quoi que ce soit. Le gain n'y
        // dépasse jamais ce qui la porterait au-delà de 90 % de la pleine
        // échelle — la phrase forte qui suit une douce n'arrive plus
        // multipliée par le gain d'avant.
        if peak > 0.0 {
            let plafond = Self::CRETE_MAX / peak;
            self.gain = self.gain.min(plafond);
            avant = avant.min(plafond);
        }
        if (avant - 1.0).abs() > 0.001 || (self.gain - 1.0).abs() > 0.001 {
            let n = frame.len().max(1) as f32;
            let pas = (self.gain - avant) / n;
            for (i, s) in frame.iter_mut().enumerate() {
                *s *= avant + pas * (i as f32 + 1.0);
            }
        }
    }
}

impl Default for Agc {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FRAME_SAMPLES;

    /// Sa voix fixe le gain ; une voix seulement « proche », 25 dB plus bas
    /// et vivante (des salves, des respirations), ne gagne pas plus de 6 dB
    /// au-dessus de ce gain-là — alors qu'elle serait remontée bien plus si
    /// on la tenait pour sûre.
    #[test]
    fn une_trame_proche_ne_monte_pas_plus_de_six_db() {
        let salves = |agc: &mut Agc, amplitude: f32, adaptation: Adaptation| {
            let mut crete = 0f32;
            for _ in 0..12 {
                for _ in 0..20 {
                    let mut f = [amplitude; FRAME_SAMPLES];
                    agc.process(&mut f, 0.30, adaptation);
                    crete = crete.max(f[FRAME_SAMPLES - 1].abs());
                }
                for _ in 0..5 {
                    let mut f = [0.001f32; FRAME_SAMPLES];
                    agc.process(&mut f, 0.30, adaptation);
                }
            }
            crete
        };
        let mut agc = Agc::new();
        salves(&mut agc, 0.3, Adaptation::Sure);
        let gain_voix = agc.gain();
        let faible = 0.3 * 10f32.powf(-25.0 / 20.0);
        let crete = salves(&mut agc, faible, Adaptation::Proche);
        assert!(agc.gain() <= gain_voix * 2.0 + 1e-3, "gain {} pour une voix sûre à {gain_voix}", agc.gain());
        assert!(crete <= faible * gain_voix * 2.0 + 1e-3, "crête {crete}");
        // La même voix tenue pour sûre : remontée bien plus haut.
        let mut agc2 = Agc::new();
        let crete2 = salves(&mut agc2, faible, Adaptation::Sure);
        assert!(crete2 > crete * 2.0, "sûre {crete2} contre proche {crete}");
    }
}
