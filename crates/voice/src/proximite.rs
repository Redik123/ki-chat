//! La voix d'à côté : ne garder que la voix **proche** du micro.
//!
//! Un détecteur de parole (Silero) répond « est-ce de la parole ? », jamais
//! « est-ce *ma* parole ? » : la personne assise à un mètre ouvre le micro
//! aussi sûrement que celui qui le porte, et le gain automatique remonte sa
//! voix au même niveau. Le seul indice qui les sépare, trame par trame, est
//! le **niveau brut** : une bouche à trois centimètres d'une perche arrive 20
//! à 30 dB au-dessus d'une voix à un mètre. Ce module mesure ce niveau avant
//! tout gain, apprend celui de sa propre voix, et en déduit pour chaque trame
//! si elle est « proche ».
//!
//! Ce verdict sert trois fois dans la chaîne :
//! - la **décision d'émission** exige parole ET proche ;
//! - le **gain automatique** ne s'adapte que sur une trame proche — une voix
//!   lointaine n'est plus jamais remontée ;
//! - un **expanseur** baisse les trames lointaines (profondeur réglable), ce
//!   qui couvre aussi le push-to-talk, le micro ouvert et le micro des jeux,
//!   et les 150 ms de maintien entre deux mots.
//!
//! Le niveau se mesure sur la trame telle que la carte la livre (après
//! l'annulation d'écho, avant le gain d'entrée), passée par un coupe-bas à
//! 80 Hz : la référence ne dépend alors que de la carte son, de la perche et
//! de la voix — pas des curseurs de ki-chat. Elle est donc à réapprendre
//! quand la carte change, et l'application la range par micro.
//!
//! **Apprentissage.** La référence suit le niveau des trames dont Silero est
//! sûr (p ≥ 0,85) et qui ne sont pas plus de 6 dB sous elle : un suivi de
//! quantile à petits pas, asymétrique, qui converge vers le haut de sa voix
//! (ses voyelles). La voix d'à côté, 20 dB plus bas, ne tombe jamais dans
//! cette fenêtre et ne la déplace pas — même s'il se tait vingt minutes
//! pendant qu'elle parle. Une fois **ancrée** (étalonnage, ou première
//! minute de parole), la référence ne s'éloigne plus de 6 dB de son ancre.

use crate::egaliseur::{Bande, Egaliseur, Forme, Q_NEUTRE};
use crate::SAMPLE_RATE;

/// Les réglages, tels que l'application les garde.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReglagesProximite {
    /// Rien n'est fait si faux : la chaîne d'avant, exactement.
    pub actif: bool,
    /// Combien une voix peut être sous la référence et rester « proche ».
    pub marge_db: f32,
    /// Ce que l'expanseur laisse d'une trame lointaine (0 : rien d'enlevé,
    /// -80 : coupée).
    pub profondeur_db: f32,
    /// Une trame déjà proche le reste jusqu'à ce point sous le seuil.
    pub hysterese_db: f32,
    /// Combien de trames (20 ms) une voix proche tient le verdict après sa
    /// dernière trame au-dessus du seuil : le temps d'une consonne sourde ou
    /// d'un souffle entre deux mots.
    pub maintien_trames: u32,
    /// L'ancre apprise ou étalonnée pour ce micro (niveau efficace en dBFS
    /// de sa voix), si l'application en a une.
    pub ancre_db: Option<f32>,
}

/// Les trois forces proposées à l'utilisateur, et « coupée ».
pub const PROXIMITE_OFF: u8 = 0;
pub const PROXIMITE_DOUCE: u8 = 1;
pub const PROXIMITE_NORMALE: u8 = 2;
pub const PROXIMITE_FORTE: u8 = 3;

impl ReglagesProximite {
    /// Les réglages d'une force (`PROXIMITE_*`).
    ///
    /// Les marges se comptent depuis la référence, qui est le haut de sa
    /// voix (ses syllabes fortes), pas sa moyenne : mesuré chez drion le
    /// 03/10/2026 (MMX 300 Pro sur G8), ses voyelles normales sont 4 à 7 dB
    /// sous cette référence, sa voix douce 8 à 14 dB, la personne à côté 18 à
    /// 20 dB (12 dB quand elle parle fort).
    pub fn de_force(force: u8, ancre_db: Option<f32>) -> Self {
        //
        // Banc du 03/10/2026 sur son enregistrement (`examples/banc-voisine`) :
        // sans isolation, sa voix forte à elle partait à 41 % des trames, 8 dB
        // sous lui ; en Normale, 6 % à -46 dBFS (inaudible), sa voix normale
        // à lui inchangée (69 %), sa voix douce 68 % au lieu de 88 — le prix.
        // L'hystérésis de 8 dB et le maintien de 200 ms tiennent ses fins de
        // mots sans rien rendre à la voisine ; une marge de 15 ou 16 dB la
        // laissait repasser à 13 % et 10 dB plus fort.
        let (actif, marge_db, profondeur_db, hysterese_db, maintien_trames) = match force {
            PROXIMITE_DOUCE => (true, 18.0, -12.0, 8.0, 10),
            PROXIMITE_NORMALE => (true, 14.0, -18.0, 8.0, 10),
            PROXIMITE_FORTE => (true, 10.0, -30.0, 6.0, 8),
            _ => (false, 14.0, -18.0, 8.0, 10),
        };
        Self { actif, marge_db, profondeur_db, hysterese_db, maintien_trames, ancre_db }
    }

    /// Ramène chaque champ dans ce que le moteur sait faire.
    pub fn bornes(mut self) -> Self {
        let ok = |v: f32, min: f32, max: f32| if v.is_finite() { v.clamp(min, max) } else { min };
        self.marge_db = ok(self.marge_db, 3.0, 40.0);
        self.profondeur_db = ok(self.profondeur_db, -80.0, 0.0);
        self.hysterese_db = ok(self.hysterese_db, 0.0, 20.0);
        self.maintien_trames = self.maintien_trames.min(50);
        self.ancre_db = self.ancre_db.filter(|a| a.is_finite()).map(|a| a.clamp(-80.0, 0.0));
        self
    }
}

impl Default for ReglagesProximite {
    fn default() -> Self {
        Self::de_force(PROXIMITE_OFF, None)
    }
}

/// Ce que la trame vient de dire.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Verdict {
    /// La trame vient d'une voix proche (ou on ne sait pas encore : avant
    /// toute référence, tout est tenu pour proche).
    pub proche: bool,
    /// Et même, à coup sûr, de SA voix : à moins de [`SURE_DB`] sous la
    /// référence. C'est sur ces trames-là seulement que le gain automatique
    /// se cale — une voix d'à côté un peu forte peut passer pour proche,
    /// jamais pour sûre.
    pub sur: bool,
    /// Le niveau efficace mesuré, en dBFS.
    pub niveau_db: f32,
    /// La référence courante (sa voix), si elle existe.
    pub reference_db: Option<f32>,
}

/// Où en est l'apprentissage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Etat {
    /// Pas de référence : rien n'est filtré, on écoute.
    Ecoute,
    /// Une référence provisoire, encore libre de bouger.
    Apprentissage,
    /// Ancrée : la référence ne s'éloigne plus de 6 dB de l'ancre.
    Ancree,
}

/// Niveau sous lequel une trame n'est pas un son : le coupe-bas et le fond
/// d'une carte muette donnent -100 dB et moins.
const PLANCHER_DB: f32 = -90.0;
/// Sous la référence, jusqu'où une trame reste « sûrement sa voix ».
pub const SURE_DB: f32 = 9.0;
/// Silero doit être au moins sûr à ce point pour que la trame serve à
/// apprendre.
const P_SURE: f32 = 0.85;
/// Fenêtre d'apprentissage : seules les trames à moins de 6 dB sous la
/// référence la déplacent.
const FENETRE_DB: f32 = 6.0;
/// Pas d'apprentissage (dB par trame sûre) : montée et descente — le rapport
/// 7 : 3 fait converger vers le 70e centile des trames de la fenêtre.
const PAS_HAUT: f32 = 0.05 * 0.7;
const PAS_BAS: f32 = 0.05 * 0.3;
/// Au début, sans ancre, les pas sont dix fois plus grands pendant ce nombre
/// de trames sûres : une référence utile en deux ou trois secondes de parole.
const TRAMES_RAPIDES: u32 = 150;
/// Trames sûres après lesquelles une référence provisoire devient l'ancre.
const TRAMES_AVANT_ANCRE: u32 = 1_500;
/// La référence ne s'éloigne jamais de son ancre de plus que ça.
const LIBERTE_DB: f32 = 6.0;
/// Constantes de temps de l'expanseur.
const ATTAQUE_MS: f32 = 2.0;
const RELACHEMENT_MS: f32 = 120.0;

fn coefficient(ms: f32) -> f32 {
    1.0 - (-1.0 / (ms / 1000.0 * SAMPLE_RATE as f32)).exp()
}

pub struct Proximite {
    reglages: ReglagesProximite,
    coupe_bas: Egaliseur,
    reference: Option<f32>,
    ancre: Option<f32>,
    /// Trames sûres vues depuis le début de l'apprentissage.
    trames_sures: u32,
    proche: bool,
    /// Trames écoulées depuis la dernière au-dessus du seuil d'ouverture.
    depuis_forte: u32,
    gain: f32,
    attaque: f32,
    relachement: f32,
    plancher: f32,
    dernier: Verdict,
}

impl Proximite {
    pub fn new(reglages: ReglagesProximite) -> Self {
        let mut p = Self {
            reglages: ReglagesProximite::default(),
            coupe_bas: Egaliseur::new(&[Bande::new(Forme::PasseHaut, 80.0, 0.0, Q_NEUTRE)]),
            reference: None,
            ancre: None,
            trames_sures: 0,
            proche: true,
            depuis_forte: u32::MAX,
            gain: 1.0,
            attaque: coefficient(ATTAQUE_MS),
            relachement: coefficient(RELACHEMENT_MS),
            plancher: 1.0,
            dernier: Verdict { proche: true, sur: true, niveau_db: PLANCHER_DB, reference_db: None },
        };
        p.regler(reglages);
        p
    }

    /// Change les réglages à chaud. Une nouvelle ancre remplace la référence
    /// (c'est un étalonnage, ou un autre micro) ; `None` ne touche à rien.
    pub fn regler(&mut self, r: ReglagesProximite) {
        let r = r.bornes();
        if r.ancre_db != self.reglages.ancre_db {
            match r.ancre_db {
                Some(a) => {
                    self.ancre = Some(a);
                    self.reference = Some(a);
                    self.trames_sures = TRAMES_AVANT_ANCRE;
                }
                None => {
                    self.ancre = None;
                    self.reference = None;
                    self.trames_sures = 0;
                }
            }
        }
        self.plancher = (r.profondeur_db * std::f32::consts::LN_10 / 20.0).exp();
        self.reglages = r;
    }

    pub fn reglages(&self) -> ReglagesProximite {
        self.reglages
    }

    pub fn etat(&self) -> Etat {
        match (self.ancre, self.reference) {
            (Some(_), _) => Etat::Ancree,
            (None, Some(_)) => Etat::Apprentissage,
            (None, None) => Etat::Ecoute,
        }
    }

    /// L'ancre que l'apprentissage vient de poser de lui-même, à ranger par
    /// l'application — `None` tant qu'il n'y en a pas, ou qu'elle vient des
    /// réglages.
    pub fn ancre_apprise(&self) -> Option<f32> {
        match (self.ancre, self.reglages.ancre_db) {
            (Some(a), None) => Some(a),
            _ => None,
        }
    }

    /// Oublie ce qui a été appris sans ancre (changement de micro).
    pub fn oublier(&mut self) {
        if self.reglages.ancre_db.is_none() {
            self.reference = None;
            self.ancre = None;
            self.trames_sures = 0;
        }
        self.coupe_bas.reinitialiser();
        self.proche = true;
        self.depuis_forte = u32::MAX;
        self.gain = 1.0;
    }

    /// Le dernier verdict rendu.
    pub fn dernier(&self) -> Verdict {
        self.dernier
    }

    /// Le seuil au-dessus duquel une trame ouvre, en dBFS, s'il y en a un.
    pub fn seuil_db(&self) -> Option<f32> {
        self.reference.map(|r| r - self.reglages.marge_db)
    }

    /// Mesure le niveau d'une trame **brute** (avant tout gain) et dit si
    /// elle est proche. `p_parole` est la dernière probabilité connue de
    /// Silero (`None` : il ne tourne pas), qui ne sert qu'à apprendre.
    pub fn mesurer(&mut self, brute: &[f32], p_parole: Option<f32>) -> Verdict {
        // Le coupe-bas sur une copie : la trame, elle, continue sa route.
        let mut energie = 0f32;
        let mut copie = [0f32; crate::FRAME_SAMPLES];
        let n = brute.len().min(copie.len());
        copie[..n].copy_from_slice(&brute[..n]);
        self.coupe_bas.traiter_trame(&mut copie[..n]);
        for s in &copie[..n] {
            if s.is_finite() {
                energie += s * s;
            }
        }
        let niveau_db = (10.0 * (energie / n.max(1) as f32).max(1e-12).log10()).max(PLANCHER_DB);

        if !self.reglages.actif {
            self.proche = true;
            self.dernier = Verdict { proche: true, sur: true, niveau_db, reference_db: self.reference };
            return self.dernier;
        }

        // Apprentissage, sur les trames dont la parole est sûre.
        if p_parole.is_some_and(|p| p >= P_SURE) && niveau_db > PLANCHER_DB + 10.0 {
            match self.reference {
                None => {
                    self.reference = Some(niveau_db);
                    self.trames_sures = 1;
                }
                Some(r) if niveau_db >= r - FENETRE_DB => {
                    let rapide = self.ancre.is_none() && self.trames_sures < TRAMES_RAPIDES;
                    let facteur = if rapide { 10.0 } else { 1.0 };
                    let mut nouvelle =
                        if niveau_db > r { r + PAS_HAUT * facteur } else { r - PAS_BAS * facteur };
                    if let Some(a) = self.ancre {
                        nouvelle = nouvelle.clamp(a - LIBERTE_DB, a + LIBERTE_DB);
                    }
                    self.reference = Some(nouvelle);
                    self.trames_sures = self.trames_sures.saturating_add(1);
                    if self.ancre.is_none() && self.trames_sures >= TRAMES_AVANT_ANCRE {
                        self.ancre = Some(nouvelle);
                    }
                }
                Some(_) => {}
            }
        }

        // Le verdict.
        let (proche, sur) = match self.reference {
            None => (true, true),
            Some(r) => {
                let haut = r - self.reglages.marge_db;
                let bas = haut - self.reglages.hysterese_db;
                let proche = if niveau_db >= haut {
                    self.depuis_forte = 0;
                    true
                } else {
                    self.depuis_forte = self.depuis_forte.saturating_add(1);
                    self.proche && (niveau_db >= bas || self.depuis_forte <= self.reglages.maintien_trames)
                };
                (proche, proche && niveau_db >= r - SURE_DB)
            }
        };
        self.proche = proche;
        self.dernier = Verdict { proche, sur, niveau_db, reference_db: self.reference };
        self.dernier
    }

    /// L'expanseur : glisse la trame vers sa profondeur si le dernier verdict
    /// la dit lointaine, la rouvre sinon. À appeler sur la trame traitée,
    /// après le débruitage.
    pub fn attenuer(&mut self, trame: &mut [f32]) {
        if !self.reglages.actif {
            self.gain = 1.0;
            return;
        }
        let cible = if self.proche { 1.0 } else { self.plancher };
        if (cible - self.gain).abs() < 1e-6 && (cible - 1.0).abs() < 1e-6 {
            return;
        }
        for s in trame.iter_mut() {
            let coef = if cible > self.gain { self.attaque } else { self.relachement };
            self.gain += (cible - self.gain) * coef;
            *s *= self.gain;
        }
    }

    /// Le gain que l'expanseur applique en ce moment (1 : rien).
    pub fn gain(&self) -> f32 {
        self.gain
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FRAME_SAMPLES;

    fn trame(amplitude: f32, phase: &mut f32) -> [f32; FRAME_SAMPLES] {
        let mut f = [0f32; FRAME_SAMPLES];
        for s in f.iter_mut() {
            *phase += 2.0 * std::f32::consts::PI * 220.0 / SAMPLE_RATE as f32;
            *s = amplitude * phase.sin();
        }
        f
    }

    fn db(x: f32) -> f32 {
        20.0 * x.max(1e-9).log10()
    }

    /// Lui parle (sûr, fort) : la référence s'établit près de son niveau et
    /// la trame est proche. Elle parle 25 dB plus bas : lointaine, et la
    /// référence n'a pas bougé.
    #[test]
    fn la_voix_d_a_cote_est_lointaine_et_n_apprend_rien() {
        let mut p = Proximite::new(ReglagesProximite::de_force(PROXIMITE_NORMALE, None));
        let mut phase = 0f32;
        for _ in 0..150 {
            let v = p.mesurer(&trame(0.3, &mut phase), Some(0.99));
            assert!(v.proche);
        }
        let r = p.dernier().reference_db.expect("référence");
        // Un sinus à 0,3 a un niveau efficace de -13,5 dBFS environ.
        assert!((r - db(0.3 / 2f32.sqrt())).abs() < 1.5, "référence {r}");
        assert_eq!(p.etat(), Etat::Apprentissage);
        // Elle : 25 dB plus bas, Silero sûr quand même.
        let elle = 0.3 * 10f32.powf(-25.0 / 20.0);
        let mut lointaines = 0;
        for _ in 0..100 {
            if !p.mesurer(&trame(elle, &mut phase), Some(0.99)).proche {
                lointaines += 1;
            }
        }
        assert!(lointaines >= 90, "{lointaines} trames lointaines sur 100");
        let r2 = p.dernier().reference_db.unwrap();
        assert!((r2 - r).abs() < 0.01, "la référence a bougé : {r} -> {r2}");
    }

    /// Sa voix douce, 8 dB sous sa référence, reste proche (marge 14) mais
    /// n'est plus « sûre » à 12 dB ; à 24 dB dessous elle n'est plus proche
    /// — après le maintien.
    #[test]
    fn la_marge_et_le_maintien_tiennent_les_consonnes() {
        let mut p = Proximite::new(ReglagesProximite::de_force(PROXIMITE_NORMALE, None));
        let maintien = p.reglages().maintien_trames as usize;
        let mut phase = 0f32;
        for _ in 0..150 {
            let v = p.mesurer(&trame(0.3, &mut phase), Some(0.99));
            assert!(v.proche && v.sur);
        }
        let douce = 0.3 * 10f32.powf(-8.0 / 20.0);
        for _ in 0..20 {
            let v = p.mesurer(&trame(douce, &mut phase), Some(0.5));
            assert!(v.proche && v.sur, "{v:?}");
        }
        let plus_douce = 0.3 * 10f32.powf(-12.0 / 20.0);
        for _ in 0..20 {
            let v = p.mesurer(&trame(plus_douce, &mut phase), Some(0.5));
            assert!(v.proche && !v.sur, "{v:?}");
        }
        let lointaine = 0.3 * 10f32.powf(-24.0 / 20.0);
        let mut verdicts = Vec::new();
        for _ in 0..20 {
            verdicts.push(p.mesurer(&trame(lointaine, &mut phase), Some(0.5)).proche);
        }
        // Les premières tiennent (le maintien), puis plus.
        assert!(verdicts[..maintien].iter().all(|&v| v));
        assert!(verdicts[maintien + 1..].iter().all(|&v| !v), "{verdicts:?}");
    }

    /// L'expanseur baisse une trame lointaine à sa profondeur et rouvre
    /// vite ; coupé, il ne touche à rien.
    #[test]
    fn l_expanseur_baisse_et_rouvre() {
        let mut p = Proximite::new(ReglagesProximite::de_force(PROXIMITE_NORMALE, None));
        let mut phase = 0f32;
        for _ in 0..150 {
            p.mesurer(&trame(0.3, &mut phase), Some(0.99));
        }
        let elle = 0.3 * 10f32.powf(-25.0 / 20.0);
        let mut derniere = [0f32; FRAME_SAMPLES];
        // Une bonne seconde : le relâchement de 120 ms est largement passé.
        for _ in 0..60 {
            let mut t = trame(elle, &mut phase);
            p.mesurer(&t, Some(0.99));
            p.attenuer(&mut t);
            derniere = t;
        }
        let crete = derniere.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!((db(crete / elle) + 18.0).abs() < 1.0, "atténuation {} dB", db(crete / elle));
        // Il reprend : l'attaque de 2 ms a rouvert avant la fin de la trame.
        let mut t = trame(0.3, &mut phase);
        p.mesurer(&t, Some(0.99));
        p.attenuer(&mut t);
        assert!(p.gain() > 0.99, "gain {}", p.gain());
        // Coupé : rien ne bouge.
        p.regler(ReglagesProximite::de_force(PROXIMITE_OFF, None));
        let mut t = trame(elle, &mut phase);
        let avant = t;
        p.mesurer(&t, Some(0.99));
        p.attenuer(&mut t);
        assert_eq!(t, avant);
    }

    /// Ancrée, la référence ne suit pas une voix qui monte de 15 dB : elle
    /// s'arrête à 6 dB de l'ancre.
    #[test]
    fn l_ancre_borne_la_derive() {
        let ancre = -20.0;
        let mut p = Proximite::new(ReglagesProximite::de_force(PROXIMITE_NORMALE, Some(ancre)));
        assert_eq!(p.etat(), Etat::Ancree);
        let mut phase = 0f32;
        let fort = 10f32.powf((ancre + 15.0) / 20.0) * 2f32.sqrt();
        for _ in 0..3000 {
            p.mesurer(&trame(fort, &mut phase), Some(0.99));
        }
        let r = p.dernier().reference_db.unwrap();
        assert!((r - (ancre + LIBERTE_DB)).abs() < 0.1, "référence {r}");
        assert!(p.ancre_apprise().is_none());
    }

    /// Sans ancre, l'apprentissage en pose une après assez de parole sûre,
    /// et la publie une fois pour que l'application la range.
    #[test]
    fn l_apprentissage_pose_une_ancre() {
        let mut p = Proximite::new(ReglagesProximite::de_force(PROXIMITE_NORMALE, None));
        let mut phase = 0f32;
        for _ in 0..TRAMES_AVANT_ANCRE + 10 {
            p.mesurer(&trame(0.2, &mut phase), Some(0.99));
        }
        let a = p.ancre_apprise().expect("ancre");
        assert!((a - db(0.2 / 2f32.sqrt())).abs() < 1.5, "ancre {a}");
        // L'application la range et la renvoie : plus rien à publier.
        let mut r = p.reglages();
        r.ancre_db = Some(a);
        p.regler(r);
        assert!(p.ancre_apprise().is_none());
        assert_eq!(p.etat(), Etat::Ancree);
    }

    /// Le silence et le bruit (Silero pas sûr) n'apprennent rien, et sans
    /// référence tout est proche.
    #[test]
    fn sans_parole_sure_rien_n_est_appris() {
        let mut p = Proximite::new(ReglagesProximite::de_force(PROXIMITE_FORTE, None));
        let mut phase = 0f32;
        for _ in 0..200 {
            let v = p.mesurer(&trame(0.1, &mut phase), Some(0.3));
            assert!(v.proche);
            assert!(v.reference_db.is_none());
        }
        assert_eq!(p.etat(), Etat::Ecoute);
        // Une trame de zéros sur un détecteur neuf : le plancher, et proche.
        let mut neuf = Proximite::new(ReglagesProximite::de_force(PROXIMITE_FORTE, None));
        let v = neuf.mesurer(&[0f32; FRAME_SAMPLES], None);
        assert!(v.proche);
        assert_eq!(v.niveau_db, PLANCHER_DB);
    }
}
