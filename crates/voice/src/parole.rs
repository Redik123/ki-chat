//! La décision de prise de parole : la trame part-elle vers les autres ?
//!
//! Trois modes. **Ouvert** : tout part. **Seuil** : le niveau de la trame
//! passe un seuil d'amplitude. **Neuronal** : Silero juge que c'est de la
//! parole — avec une hystérésis (on ouvre à `sens`, on ne referme qu'à
//! `sens - 0,15`, comme l'enveloppe de référence de Silero) pour ne pas
//! hacher deux syllabes. Dans tous les cas le verdict de proximité
//! ([`crate::proximite`]) a un droit de veto : une parole lointaine n'est
//! pas une prise de parole. Puis un maintien : la trame part encore quelques
//! dixièmes de seconde après la dernière voix, le temps d'une fin de mot.
//!
//! Tout est compté en trames de 20 ms, sans horloge : c'est ce qui rend la
//! décision rejouable sur un enregistrement, et testable.

/// Ce que la trame a dit d'elle-même.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Parole {
    /// Micro ouvert : toute trame est une prise de parole.
    Toujours,
    /// Seuil d'amplitude : `niveau` (crête, avant le gain automatique)
    /// contre `seuil`.
    Seuil { niveau: f32, seuil: f32 },
    /// Silero : probabilité `p` contre la sensibilité `sens`.
    Neuronale { p: f32, sens: f32 },
}

/// Sous la sensibilité, de combien la probabilité doit retomber pour que le
/// micro se referme.
pub const HYSTERESE: f32 = 0.15;

#[derive(Clone, Debug, Default)]
pub struct Decision {
    /// Hystérésis neuronale : la parole est en cours.
    ouvert: bool,
    /// Trames depuis la dernière prise de parole (`None` : jamais).
    depuis_voix: Option<u32>,
}

impl Decision {
    pub fn new() -> Self {
        Self::default()
    }

    /// Le seuil de fermeture pour une sensibilité donnée.
    pub fn seuil_fermeture(sens: f32) -> f32 {
        (sens - HYSTERESE).max(0.05)
    }

    /// Une trame de plus. Rend vrai si elle doit partir : prise de parole
    /// proche en cours, ou dans le maintien de la dernière.
    pub fn trame(&mut self, parole: Parole, proche: bool, maintien_trames: u32) -> bool {
        // Micro ouvert : la proximité n'a pas de veto — tout part, l'expanseur
        // a déjà baissé ce qui est loin.
        let parle = match parole {
            Parole::Toujours => {
                self.ouvert = true;
                self.depuis_voix = Some(0);
                return true;
            }
            Parole::Seuil { niveau, seuil } => {
                self.ouvert = niveau >= seuil;
                self.ouvert
            }
            Parole::Neuronale { p, sens } => {
                self.ouvert =
                    if self.ouvert { p >= Self::seuil_fermeture(sens) } else { p >= sens };
                self.ouvert
            }
        };
        if parle && proche {
            self.depuis_voix = Some(0);
            return true;
        }
        match self.depuis_voix.as_mut() {
            Some(n) => {
                *n = n.saturating_add(1);
                *n <= maintien_trames
            }
            None => false,
        }
    }

    /// La parole est en cours (hystérésis), proximité mise à part.
    pub fn parle(&self) -> bool {
        self.ouvert
    }

    /// Au réarmement du micro, au changement de mode : rien d'avant ne
    /// compte — ni une parole en cours, ni un maintien.
    pub fn reinitialiser(&mut self) {
        self.ouvert = false;
        self.depuis_voix = None;
    }
}

/// Maintien en ms → en trames de 20 ms, arrondi au-dessus.
pub fn maintien_trames(ms: u32) -> u32 {
    ms.div_ceil(20)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l_hysterese_ouvre_haut_et_referme_bas() {
        let mut d = Decision::new();
        let n = |p| Parole::Neuronale { p, sens: 0.5 };
        assert!(!d.trame(n(0.45), true, 0));
        assert!(d.trame(n(0.55), true, 0));
        // Entre les deux seuils : toujours ouvert.
        assert!(d.trame(n(0.40), true, 0));
        assert!(d.parle());
        // Sous le seuil de fermeture : fermé, et plus de maintien.
        assert!(!d.trame(n(0.30), true, 0));
        assert!(!d.parle());
        assert!(!d.trame(n(0.40), true, 0));
    }

    #[test]
    fn le_maintien_tient_le_nombre_de_trames_demande() {
        let mut d = Decision::new();
        assert!(d.trame(Parole::Toujours, true, 3));
        for _ in 0..3 {
            assert!(d.trame(Parole::Seuil { niveau: 0.0, seuil: 0.1 }, true, 3));
        }
        assert!(!d.trame(Parole::Seuil { niveau: 0.0, seuil: 0.1 }, true, 3));
    }

    /// La voix d'à côté : Silero dit « parole », la proximité dit non → la
    /// trame ne part pas, et ne réarme pas le maintien.
    #[test]
    fn une_parole_lointaine_ne_part_pas() {
        let mut d = Decision::new();
        let n = Parole::Neuronale { p: 0.99, sens: 0.5 };
        for _ in 0..10 {
            assert!(!d.trame(n, false, 7));
        }
        assert!(d.parle(), "Silero la voit bien comme de la parole");
        // Lui parle : ça part. Elle enchaîne : le maintien expire, puis rien.
        assert!(d.trame(n, true, 7));
        let mut partis = 0;
        for _ in 0..20 {
            partis += d.trame(n, false, 7) as u32;
        }
        assert_eq!(partis, 7);
    }

    #[test]
    fn la_reinitialisation_oublie_tout() {
        let mut d = Decision::new();
        assert!(d.trame(Parole::Toujours, true, 50));
        d.reinitialiser();
        assert!(!d.parle());
        assert!(!d.trame(Parole::Neuronale { p: 0.4, sens: 0.5 }, true, 50));
    }

    #[test]
    fn le_maintien_se_compte_en_trames() {
        assert_eq!(maintien_trames(400), 20);
        assert_eq!(maintien_trames(150), 8);
        assert_eq!(maintien_trames(0), 0);
    }
}
