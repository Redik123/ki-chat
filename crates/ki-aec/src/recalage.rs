//! Le recalage du lointain sur l'écho.
//!
//! L'annulateur ne cherche l'écho que dans sa queue de filtre (200 ms dans
//! le moteur) : il faut que la voix des autres revienne au micro moins de
//! 200 ms après qu'on la lui a donnée. Sur PC, c'est le cas. Sur un
//! téléphone, non : mesuré sur un Galaxy Z Flip 6, l'écho revenait 170 à
//! 260 ms après le son joué, selon l'ouverture des flux — hors de portée,
//! et l'annulateur ne retirait rien.
//!
//! Le [`Recaleur`] mesure ce retard et retarde d'autant le lointain avant
//! de le donner à l'annulateur, moins une marge : l'écho tombe alors au
//! début de la queue, quel que soit le téléphone.
//!
//! La mesure compare les **enveloppes** (le niveau, par blocs de 2 ms, en
//! logarithme) plutôt que les signaux : la chaîne du téléphone (haut-parleur
//! qui compresse, traitement du micro) déforme l'écho au point de défaire
//! la ressemblance échantillon par échantillon, pas celle de la parole qui
//! monte et descend. Elle ne retient un retard que sur une corrélation
//! nette, et deux fois de suite : quand on parle soi-même par-dessus, ou que
//! personne ne parle en face, elle ne dit rien et le recalage ne bouge pas.

use std::collections::VecDeque;

/// Blocs de l'enveloppe : 2 ms à 48 kHz.
const BLOC_MS: f32 = 2.0;
/// Fenêtre de comparaison : 2 s.
const FENETRE_BLOCS: usize = 1000;
/// Retard le plus long qu'on cherche : 500 ms.
const RETARD_MAX_MS: f32 = 500.0;
/// L'écho est placé à ce temps-là dans la queue du filtre : de quoi absorber
/// une mesure un peu longue sans le faire passer devant le lointain.
const MARGE_MS: f32 = 40.0;
/// Une mesure toutes les 250 ms.
const PAS_BLOCS: usize = 125;
/// En dessous, la corrélation ne dit rien de sûr.
const CORRELATION_MIN: f32 = 0.45;
/// Un retard plus proche que ça de l'actuel ne vaut pas de tout refaire :
/// l'écho reste dans la queue du filtre, qui a appris son trajet — le
/// remettre à zéro pour 20 ms lui faisait tout réapprendre, et l'écho
/// revenait le temps qu'il converge.
const ECART_MIN_MS: f32 = 30.0;
/// Deux mesures sont « les mêmes » à ça près.
const ACCORD_MS: f32 = 6.0;

/// Mesure le retard de l'écho et recale le lointain dessus.
pub struct Recaleur {
    trame: usize,
    bloc: usize,
    retard_max: usize,
    marge: usize,
    /// Le lointain des dernières `retard_max` + une trame, la ligne à retard.
    lointain: VecDeque<f32>,
    /// Les blocs en cours de remplissage.
    bloc_loin: Vec<f32>,
    bloc_pres: Vec<f32>,
    /// Les enveloppes, en log : le micro sur la fenêtre, le lointain sur la
    /// fenêtre plus le retard maximal.
    env_loin: VecDeque<f32>,
    env_pres: VecDeque<f32>,
    depuis_mesure: usize,
    /// Le retard retenu, en blocs d'enveloppe.
    retard: Option<usize>,
    /// La dernière mesure, en blocs, en attente d'être confirmée.
    candidat: Option<usize>,
}

impl Recaleur {
    /// Pour des trames de `trame` échantillons à `frequence` Hz.
    pub fn new(trame: usize, frequence: u32) -> Self {
        let par_ms = frequence as f32 / 1000.0;
        let bloc = (BLOC_MS * par_ms).round().max(1.0) as usize;
        let retard_max = (RETARD_MAX_MS * par_ms) as usize;
        Self {
            trame,
            bloc,
            retard_max,
            marge: (MARGE_MS * par_ms) as usize,
            lointain: VecDeque::from(vec![0.0; retard_max + trame]),
            bloc_loin: Vec::with_capacity(bloc),
            bloc_pres: Vec::with_capacity(bloc),
            env_loin: VecDeque::new(),
            env_pres: VecDeque::new(),
            depuis_mesure: 0,
            retard: None,
            candidat: None,
        }
    }

    /// Le retard retenu, en millisecondes, une fois mesuré.
    pub fn retard_ms(&self) -> Option<f32> {
        self.retard.map(|r| r as f32 * BLOC_MS)
    }

    /// Pour chaque trame de capture : `pres` (le micro, avant annulation) et
    /// `loin` (ce qui vient d'être joué). Rend dans `sortie` le lointain à
    /// donner à l'annulateur, recalé. Vrai si le recalage vient de changer :
    /// le filtre de l'annulateur a appris l'ancien, il est à refaire.
    pub fn recaler(&mut self, pres: &[f32], loin: &[f32], sortie: &mut [f32]) -> bool {
        debug_assert_eq!(pres.len(), self.trame);
        debug_assert_eq!(loin.len(), self.trame);
        debug_assert_eq!(sortie.len(), self.trame);
        self.lointain.extend(loin.iter().copied());
        while self.lointain.len() > self.retard_max + self.trame {
            self.lointain.pop_front();
        }
        let mut change = false;
        for (&p, &l) in pres.iter().zip(loin) {
            self.bloc_pres.push(p);
            self.bloc_loin.push(l);
            if self.bloc_pres.len() == self.bloc {
                let e = |b: &[f32]| (b.iter().map(|x| x * x).sum::<f32>() / b.len() as f32).sqrt();
                self.env_pres.push_back((e(&self.bloc_pres) + 1e-5).log10());
                self.env_loin.push_back((e(&self.bloc_loin) + 1e-5).log10());
                self.bloc_pres.clear();
                self.bloc_loin.clear();
                let lag_max = self.retard_max / self.bloc;
                while self.env_pres.len() > FENETRE_BLOCS {
                    self.env_pres.pop_front();
                }
                while self.env_loin.len() > FENETRE_BLOCS + lag_max {
                    self.env_loin.pop_front();
                }
                self.depuis_mesure += 1;
                if self.depuis_mesure >= PAS_BLOCS && self.env_loin.len() == FENETRE_BLOCS + lag_max {
                    self.depuis_mesure = 0;
                    change |= self.mesurer(lag_max);
                }
            }
        }
        // La ligne à retard : l'écho placé `marge` après le début de la queue.
        let decalage = self.retard.map_or(0, |r| (r * self.bloc).saturating_sub(self.marge));
        let fin = self.lointain.len() - decalage;
        for (s, x) in sortie.iter_mut().zip(self.lointain.range(fin - self.trame..fin)) {
            *s = *x;
        }
        change
    }

    /// Une mesure : la corrélation des enveloppes à chaque retard. Vrai si
    /// le retard retenu change.
    fn mesurer(&mut self, lag_max: usize) -> bool {
        let pres: Vec<f32> = self.env_pres.iter().copied().collect();
        let loin: Vec<f32> = self.env_loin.iter().copied().collect();
        let n = pres.len();
        let moyenne = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
        let mp = moyenne(&pres);
        let vp: f32 = pres.iter().map(|x| (x - mp) * (x - mp)).sum();
        // Le micro à peu près plat : rien à reconnaître.
        if vp / (n as f32) < 0.01 {
            return false;
        }
        let mut meilleur = (f32::MIN, 0usize);
        for lag in 0..=lag_max {
            // Le lointain joué `lag` blocs avant le micro.
            let debut = lag_max - lag;
            let l = &loin[debut..debut + n];
            let ml = moyenne(l);
            let vl: f32 = l.iter().map(|x| (x - ml) * (x - ml)).sum();
            // Personne ne parle en face sur cette fenêtre.
            if vl / (n as f32) < 0.02 {
                continue;
            }
            let c: f32 = pres.iter().zip(l).map(|(p, x)| (p - mp) * (x - ml)).sum::<f32>() / (vp * vl).sqrt();
            if c > meilleur.0 {
                meilleur = (c, lag);
            }
        }
        if meilleur.0 < CORRELATION_MIN {
            self.candidat = None;
            return false;
        }
        let lag = meilleur.1;
        // Collé au bord de la recherche : le vrai pic est au-delà, ou ce
        // n'en est pas un. Mesuré sur un téléphone, un « 500 ms » de ce genre
        // décalait tout pour rien.
        if lag <= 1 || lag + 1 >= lag_max {
            self.candidat = None;
            return false;
        }
        let blocs = |ms: f32| (ms / BLOC_MS).round() as usize;
        // Confirmée par la mesure d'avant ? Sinon, elle attend la suivante.
        let confirme = self.candidat.is_some_and(|c| c.abs_diff(lag) <= blocs(ACCORD_MS));
        self.candidat = Some(lag);
        if !confirme {
            return false;
        }
        match self.retard {
            Some(r) if r.abs_diff(lag) < blocs(ECART_MIN_MS) => false,
            _ => {
                self.retard = Some(lag);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Aec;

    const TRAME: usize = 960;
    const RATE: u32 = 48_000;

    /// Une « parole » de synthèse : du bruit dans la bande de la voix, haché
    /// en syllabes de durées et de niveaux tirés au sort — pas un rythme
    /// régulier, qui donnerait une corrélation à chaque période.
    struct Parole {
        x: u32,
        /// Le multiplicateur et l'incrément du générateur. Deux graines d'un
        /// même générateur donnent la même suite, décalée : deux « voix »
        /// indépendantes en demandent deux différents.
        mul: u32,
        inc: u32,
        reste: usize,
        niveau: f32,
        lp: f32,
        hp: (f32, f32),
    }
    impl Parole {
        fn new(graine: u32) -> Self {
            Self { x: graine, mul: 1664525, inc: 1013904223, reste: 0, niveau: 0.0, lp: 0.0, hp: (0.0, 0.0) }
        }
        fn autre(graine: u32) -> Self {
            Self { mul: 22695477, inc: 1, ..Self::new(graine) }
        }
        fn hasard(&mut self) -> f32 {
            self.x = self.x.wrapping_mul(self.mul).wrapping_add(self.inc);
            (self.x >> 8) as f32 / (1u32 << 24) as f32
        }
        fn trame(&mut self) -> Vec<f32> {
            (0..TRAME)
                .map(|_| {
                    if self.reste == 0 {
                        self.reste = 2400 + (self.hasard() * 12_000.0) as usize;
                        self.niveau = if self.hasard() < 0.3 { 0.0 } else { 0.05 + self.hasard() * 0.3 };
                    }
                    self.reste -= 1;
                    let n = self.hasard() * 2.0 - 1.0;
                    let hp = 0.96 * (self.hp.1 + n - self.hp.0);
                    self.hp = (n, hp);
                    self.lp += 0.3 * (hp - self.lp);
                    self.lp * self.niveau * 3.0
                })
                .collect()
        }
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / x.len().max(1) as f64).sqrt()
    }

    /// Un écho qui revient 230 ms après le son, comme sur un téléphone, et
    /// légèrement écrasé par un haut-parleur qui compresse.
    fn echo(retard: usize, trames: usize, graine: u32) -> (Vec<Vec<f32>>, Vec<Vec<f32>>) {
        let mut parole = Parole::new(graine);
        let mut ligne = vec![0f32; retard];
        let (mut loins, mut pres) = (Vec::new(), Vec::new());
        for _ in 0..trames {
            let loin = parole.trame();
            ligne.extend_from_slice(&loin);
            let p: Vec<f32> = ligne.drain(..TRAME).map(|s| (s * 1.5).tanh() * 0.4).collect();
            loins.push(loin);
            pres.push(p);
        }
        (loins, pres)
    }

    #[test]
    fn le_retard_d_un_echo_tardif_est_mesure() {
        let retard = 230 * 48;
        let (loins, pres) = echo(retard, 500, 0xC0FFEE);
        let mut r = Recaleur::new(TRAME, RATE);
        let mut sortie = vec![0f32; TRAME];
        for (l, p) in loins.iter().zip(&pres) {
            r.recaler(p, l, &mut sortie);
        }
        let mesure = r.retard_ms().expect("retard mesuré");
        assert!((mesure - 230.0).abs() <= 4.0, "retard mesuré {mesure} ms au lieu de 230");
    }

    /// Le but : avec le recalage, l'annulateur à queue de 200 ms retire un
    /// écho de 230 ms ; sans, il n'y arrive pas.
    #[test]
    fn recale_l_annulateur_retire_l_echo_tardif() {
        let retard = 230 * 48;
        let (loins, pres) = echo(retard, 750, 0xBADA55);
        let mesurer = |recaler: bool| -> f64 {
            let mut aec = Aec::new(TRAME, 9600, RATE).unwrap();
            let mut r = Recaleur::new(TRAME, RATE);
            let mut cale = vec![0f32; TRAME];
            let (mut avant, mut apres) = (0f64, 0f64);
            for (n, (l, p)) in loins.iter().zip(&pres).enumerate() {
                let mut trame = p.clone();
                if recaler {
                    if r.recaler(p, l, &mut cale) {
                        aec = Aec::new(TRAME, 9600, RATE).unwrap();
                    }
                    aec.process(&mut trame, &cale);
                } else {
                    aec.process(&mut trame, l);
                }
                if n >= 550 {
                    avant += rms(p);
                    apres += rms(&trame);
                }
            }
            20.0 * (avant / apres.max(1e-12)).log10()
        };
        let sans = mesurer(false);
        let avec = mesurer(true);
        assert!(avec > 12.0, "recalé, l'annulateur ne retire que {avec:.1} dB");
        assert!(avec > sans + 8.0, "recalé {avec:.1} dB, sans recalage {sans:.1} dB : pas de gain");
    }

    /// Personne ne parle en face, ou l'on parle seul : aucune mesure, et le
    /// lointain passe tel quel (un recalage inventé décalerait tout).
    #[test]
    fn sans_echo_rien_n_est_recale() {
        let mut r = Recaleur::new(TRAME, RATE);
        let mut voix = Parole::new(0x1234);
        let silence = vec![0f32; TRAME];
        let mut sortie = vec![1f32; TRAME];
        for _ in 0..400 {
            let v = voix.trame();
            assert!(!r.recaler(&v, &silence, &mut sortie));
        }
        assert_eq!(r.retard_ms(), None);
        assert!(sortie.iter().all(|&s| s == 0.0));

        // Et deux paroles sans rapport : la sienne au micro, une autre en face.
        let mut r = Recaleur::new(TRAME, RATE);
        let mut autre = Parole::autre(0x9876);
        for _ in 0..400 {
            let (v, a) = (voix.trame(), autre.trame());
            r.recaler(&v, &a, &mut sortie);
        }
        assert_eq!(r.retard_ms(), None);
    }

    /// Sans recalage, la sortie est le lointain du moment, à l'échantillon.
    #[test]
    fn avant_toute_mesure_le_lointain_passe_tel_quel() {
        let mut r = Recaleur::new(TRAME, RATE);
        let mut parole = Parole::new(7);
        let silence = vec![0f32; TRAME];
        let mut sortie = vec![0f32; TRAME];
        for _ in 0..5 {
            let l = parole.trame();
            r.recaler(&silence, &l, &mut sortie);
            assert_eq!(sortie, l);
        }
    }
}
