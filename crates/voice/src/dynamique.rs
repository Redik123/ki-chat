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
    /// Le gain de rattrapage (linéaire) : ce que la compression a retiré,
    /// rendu à toute la voix.
    rattrapage: f32,
    /// Le plus petit gain appliqué depuis le dernier relevé (l'aiguille de
    /// réduction de la page Casque).
    pire: f32,
}

/// Les réglages d'un compresseur, tels que la page Casque les montre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReglagesCompresseur {
    pub seuil_db: f32,
    pub ratio: f32,
    pub genou_db: f32,
    pub attaque_ms: f32,
    pub relachement_ms: f32,
    pub rattrapage_db: f32,
}

impl ReglagesCompresseur {
    /// Les réglages derrière les choix simples (`COMPRESSION_*`) ; `None`
    /// pour aucune compression — et pour la personnalisée, qui a les siens.
    pub fn du_niveau(niveau: u8) -> Option<Self> {
        let r = |seuil_db, ratio, attaque_ms, relachement_ms| Self {
            seuil_db,
            ratio,
            genou_db: 6.0,
            attaque_ms,
            relachement_ms,
            rattrapage_db: 0.0,
        };
        match niveau {
            // Douce : ne touche qu'aux éclats, la voix ordinaire passe telle
            // quelle.
            COMPRESSION_DOUCE => Some(r(-14.0, 3.0, 5.0, 120.0)),
            // Forte : pour qui crie souvent — la voix entière est tenue.
            COMPRESSION_FORTE => Some(r(-20.0, 6.0, 3.0, 150.0)),
            _ => None,
        }
    }

    /// Ramenés dans des plages qui ne cassent rien.
    pub fn bornes(self) -> Self {
        let ok = |v: f32, min: f32, max: f32, defaut: f32| if v.is_finite() { v.clamp(min, max) } else { defaut };
        Self {
            seuil_db: ok(self.seuil_db, -60.0, 0.0, -14.0),
            ratio: ok(self.ratio, 1.0, 20.0, 3.0),
            genou_db: ok(self.genou_db, 0.0, 18.0, 6.0),
            attaque_ms: ok(self.attaque_ms, 0.1, 200.0, 5.0),
            relachement_ms: ok(self.relachement_ms, 10.0, 2000.0, 120.0),
            rattrapage_db: ok(self.rattrapage_db, 0.0, 24.0, 0.0),
        }
    }
}

impl Default for ReglagesCompresseur {
    fn default() -> Self {
        Self::du_niveau(COMPRESSION_DOUCE).unwrap()
    }
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
            rattrapage: 1.0,
            pire: 1.0,
        }
    }

    /// Un compresseur réglé au chiffre près (page Casque, mode studio).
    pub fn depuis(r: &ReglagesCompresseur) -> Self {
        let r = r.bornes();
        let mut c = Self::new(r.seuil_db, r.ratio, r.genou_db, r.attaque_ms, r.relachement_ms);
        c.rattrapage = (r.rattrapage_db * DB_VERS_LN).exp();
        c
    }

    /// Le compresseur d'une voix qu'on émet, selon le réglage choisi
    /// (`COMPRESSION_*`) ; `None` pour aucune compression — la
    /// personnalisée se construit par [`Compresseur::depuis`], avec ses
    /// réglages.
    pub fn emission(niveau: u8) -> Option<Self> {
        ReglagesCompresseur::du_niveau(niveau).map(|r| Self::depuis(&r))
    }

    /// La plus forte réduction appliquée depuis le dernier relevé, en dB
    /// (0 : rien), remise à zéro.
    pub fn reduction_db(&mut self) -> f32 {
        let pire = std::mem::replace(&mut self.pire, 1.0);
        pire.max(1e-6).ln() * LN_VERS_DB
    }

    /// Le compresseur d'une voix qu'on entend (« adoucir les cris »).
    ///
    /// Au-dessus du niveau d'une voix normale : le gain automatique de chacun
    /// vise des crêtes vers -10 dBFS, et le seuil d'avant (-12, genou dès -15)
    /// tassait donc TOUTES les voix de 1 à 2 dB, avec son relâchement — de quoi
    /// épaissir une voix déjà chargée en graves. Seuls les vrais éclats
    /// passent désormais le genou (-10 dBFS).
    pub fn ecoute() -> Self {
        Self::new(-8.0, 4.0, 4.0, 3.0, 150.0)
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
        let g = self.gain_pour(self.enveloppe);
        if g < self.pire {
            self.pire = g;
        }
        x * g * self.rattrapage
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

    /// Un plafond choisi, en dBFS (de -12 à 0).
    pub fn avec_plafond(plafond_db: f32) -> Self {
        let db = if plafond_db.is_finite() { plafond_db.clamp(-12.0, 0.0) } else { -1.0 };
        Self { plafond: (db * DB_VERS_LN).exp(), ..Self::new() }
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
/// Compression réglée au chiffre près (page Casque, mode studio).
pub const COMPRESSION_PERSO: u8 = 3;

/// Les réglages de la porte de bruit — le seuil vit à part (le réglage
/// simple de l'onglet Audio, en niveau linéaire).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReglagesPorte {
    /// Ce que la porte laisse passer une fois fermée : -80 coupe net, -10
    /// ne fait que baisser le fond.
    pub profondeur_db: f32,
    pub attaque_ms: f32,
    /// Combien de temps elle reste ouverte après la dernière syllabe.
    pub maintien_ms: f32,
    pub relachement_ms: f32,
}

impl Default for ReglagesPorte {
    /// Le comportement de l'ancienne porte : ouverture immédiate, fermeture
    /// complète en quelques centaines de millisecondes.
    fn default() -> Self {
        Self { profondeur_db: -80.0, attaque_ms: 2.0, maintien_ms: 150.0, relachement_ms: 150.0 }
    }
}

/// Porte de bruit, à l'échantillon près : s'ouvre dès que la voix passe le
/// seuil, reste ouverte le temps du maintien, puis redescend jusqu'à sa
/// profondeur. Une hystérésis de 4 dB l'empêche de battre sur une voix qui
/// oscille autour du seuil.
#[derive(Clone, Debug)]
pub struct Porte {
    reglages: ReglagesPorte,
    plancher: f32,
    attaque: f32,
    relachement: f32,
    retombee_detecteur: f32,
    maintien: u32,
    maintien_restant: u32,
    detecteur: f32,
    ouverte: bool,
    gain: f32,
}

impl Porte {
    pub fn new(reglages: ReglagesPorte) -> Self {
        let mut p = Self {
            reglages,
            plancher: 0.0,
            attaque: 0.0,
            relachement: 0.0,
            retombee_detecteur: coefficient(20.0),
            maintien: 0,
            maintien_restant: 0,
            detecteur: 0.0,
            ouverte: false,
            gain: 1.0,
        };
        p.regler(reglages);
        p
    }

    pub fn regler(&mut self, r: ReglagesPorte) {
        let ok = |v: f32, min: f32, max: f32| if v.is_finite() { v.clamp(min, max) } else { min };
        self.reglages = r;
        self.plancher = (ok(r.profondeur_db, -80.0, 0.0) * DB_VERS_LN).exp();
        self.attaque = coefficient(ok(r.attaque_ms, 0.1, 100.0));
        self.relachement = coefficient(ok(r.relachement_ms, 5.0, 2000.0));
        self.maintien = (ok(r.maintien_ms, 0.0, 2000.0) / 1000.0 * SAMPLE_RATE as f32) as u32;
    }

    pub fn reglages(&self) -> ReglagesPorte {
        self.reglages
    }

    /// Le gain qu'elle applique en ce moment (1 : ouverte).
    pub fn gain(&self) -> f32 {
        self.gain
    }

    /// `seuil` en niveau linéaire ; 0 désactive la porte.
    pub fn traiter_trame(&mut self, trame: &mut [f32], seuil: f32) {
        if seuil <= 0.0 {
            self.gain = 1.0;
            self.ouverte = true;
            return;
        }
        let fermeture = seuil * 0.631; // -4 dB
        for s in trame.iter_mut() {
            let a = s.abs();
            self.detecteur = if a > self.detecteur {
                a
            } else {
                self.detecteur + (a - self.detecteur) * self.retombee_detecteur
            };
            if self.detecteur >= seuil || (self.ouverte && self.detecteur >= fermeture) {
                self.ouverte = true;
                self.maintien_restant = self.maintien;
            } else if self.maintien_restant > 0 {
                self.maintien_restant -= 1;
            } else {
                self.ouverte = false;
            }
            let cible = if self.ouverte || self.maintien_restant > 0 { 1.0 } else { self.plancher };
            let coef = if cible > self.gain { self.attaque } else { self.relachement };
            self.gain += (cible - self.gain) * coef;
            *s *= self.gain;
        }
    }
}

/// Les réglages du de-esser.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReglagesDeesser {
    /// Le centre de la bande des sifflantes (« s », « ch »).
    pub frequence: f32,
    /// Au-dessus, la bande est ramenée.
    pub seuil_db: f32,
    /// Jamais plus que cette réduction : la voix ne doit pas zozoter.
    pub reduction_max_db: f32,
}

impl Default for ReglagesDeesser {
    fn default() -> Self {
        Self { frequence: 6_500.0, seuil_db: -30.0, reduction_max_db: 8.0 }
    }
}

/// Un passe-bande du second ordre, 0 dB au centre.
#[derive(Clone, Debug)]
struct PasseBande {
    b0: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl PasseBande {
    fn new(frequence: f32, q: f32) -> Self {
        let w0 = 2.0 * std::f32::consts::PI * frequence / SAMPLE_RATE as f32;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self { b0: alpha / a0, b2: -alpha / a0, a1: -2.0 * cos / a0, a2: (1.0 - alpha) / a0, z1: 0.0, z2: 0.0 }
    }

    #[inline]
    fn traiter(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = -self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// De-esser à bande séparée : la bande des sifflantes est isolée, suivie, et
/// **elle seule** est ramenée quand elle dépasse le seuil — le reste de la
/// voix ne bouge pas. Sans sifflante, la sortie est l'entrée, au bit près
/// ou presque (x − 0 · bande).
#[derive(Clone, Debug)]
pub struct Deesser {
    reglages: ReglagesDeesser,
    bande: PasseBande,
    seuil: f32,
    enveloppe: f32,
    attaque: f32,
    relachement: f32,
    pire_db: f32,
}

impl Deesser {
    pub fn new(r: ReglagesDeesser) -> Self {
        let frequence = if r.frequence.is_finite() { r.frequence.clamp(2_000.0, 12_000.0) } else { 6_500.0 };
        let seuil_db = if r.seuil_db.is_finite() { r.seuil_db.clamp(-60.0, 0.0) } else { -30.0 };
        Self {
            reglages: r,
            bande: PasseBande::new(frequence, 1.2),
            seuil: (seuil_db * DB_VERS_LN).exp(),
            enveloppe: 0.0,
            attaque: coefficient(1.0),
            relachement: coefficient(60.0),
            pire_db: 0.0,
        }
    }

    pub fn reglages(&self) -> ReglagesDeesser {
        self.reglages
    }

    /// La plus forte réduction depuis le dernier relevé, en dB (≤ 0).
    pub fn reduction_db(&mut self) -> f32 {
        std::mem::replace(&mut self.pire_db, 0.0)
    }

    pub fn traiter_trame(&mut self, trame: &mut [f32]) {
        let max = if self.reglages.reduction_max_db.is_finite() {
            self.reglages.reduction_max_db.clamp(0.0, 24.0)
        } else {
            8.0
        };
        for s in trame.iter_mut() {
            let b = self.bande.traiter(*s);
            let a = b.abs();
            let coef = if a > self.enveloppe { self.attaque } else { self.relachement };
            self.enveloppe += (a - self.enveloppe) * coef;
            if self.enveloppe <= self.seuil {
                continue;
            }
            // Ratio 4:1 sur la bande, borné à la réduction maximale.
            let depassement = (self.enveloppe / self.seuil).ln() * LN_VERS_DB;
            let reduction = (depassement * 0.75).min(max);
            let g = (-reduction * DB_VERS_LN).exp();
            *s -= (1.0 - g) * b;
            if -reduction < self.pire_db {
                self.pire_db = -reduction;
            }
        }
    }
}

/// Chaleur, façon lampe : une saturation douce qui arrondit les crêtes et
/// ajoute des harmoniques, sans toucher aux faibles niveaux (sa pente à
/// l'origine vaut 1). `chaleur` de 0 (rien) à 1.
#[derive(Clone, Debug)]
pub struct Saturation {
    chaleur: f32,
    pousse: f32,
}

impl Saturation {
    pub fn new(chaleur: f32) -> Self {
        let chaleur = if chaleur.is_finite() { chaleur.clamp(0.0, 1.0) } else { 0.0 };
        Self { chaleur, pousse: 1.0 + 3.0 * chaleur }
    }

    pub fn traiter_trame(&mut self, trame: &mut [f32]) {
        if self.chaleur < 0.005 {
            return;
        }
        for s in trame.iter_mut() {
            let sature = (self.pousse * *s).tanh() / self.pousse;
            *s += self.chaleur * (sature - *s);
        }
    }
}

/// Tout ce que le mode studio règle de la chaîne de sa voix, au-delà des
/// choix simples (seuil de la porte, niveau de compression, égaliseur).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReglagesStudio {
    pub porte: ReglagesPorte,
    /// `None` : pas de de-esser.
    pub deesser: Option<ReglagesDeesser>,
    /// Le compresseur de la compression personnalisée (`COMPRESSION_PERSO`).
    pub compresseur: ReglagesCompresseur,
    /// La chaleur, de 0 à 1.
    pub chaleur: f32,
    /// Le plafond du limiteur, en dBFS.
    pub plafond_db: f32,
}

impl Default for ReglagesStudio {
    /// Exactement la chaîne d'avant le mode studio.
    fn default() -> Self {
        Self {
            porte: ReglagesPorte::default(),
            deesser: None,
            compresseur: ReglagesCompresseur::default(),
            chaleur: 0.0,
            plafond_db: -1.0,
        }
    }
}

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

    fn sinus_a(frequence: f32, amplitude: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amplitude * (i as f32 * 2.0 * std::f32::consts::PI * frequence / SAMPLE_RATE as f32).sin())
            .collect()
    }

    fn db(x: f32) -> f32 {
        20.0 * x.max(1e-9).log10()
    }

    /// La porte : transparente sur la voix, encore ouverte pendant son
    /// maintien, puis fermée jusqu'à sa profondeur — et transparente sans
    /// seuil.
    #[test]
    fn la_porte_s_ouvre_se_tient_et_se_ferme() {
        let r = ReglagesPorte { profondeur_db: -40.0, attaque_ms: 2.0, maintien_ms: 100.0, relachement_ms: 50.0 };
        let mut p = Porte::new(r);
        let seuil = 0.05;
        let mut voix = sinus(0.3, 4_800);
        let entree = voix.clone();
        p.traiter_trame(&mut voix, seuil);
        assert!((crete(&voix[2_400..]) - crete(&entree[2_400..])).abs() < 0.01);
        // 50 ms de fond : toujours ouverte (maintien de 100 ms).
        let mut fond = sinus(0.01, 2_400);
        p.traiter_trame(&mut fond, seuil);
        assert!(p.gain() > 0.99, "fermée trop tôt : {}", p.gain());
        // Une seconde de fond : fermée à -40 dB.
        let mut fond = sinus(0.01, 48_000);
        p.traiter_trame(&mut fond, seuil);
        assert!((p.gain() - 0.01).abs() < 0.002, "gain {}", p.gain());
        let mut x = sinus(0.01, 960);
        let e = x.clone();
        p.traiter_trame(&mut x, 0.0);
        assert_eq!(x, e);
    }

    /// Une voix passée au-dessus du seuil puis retombée juste en dessous ne
    /// fait pas battre la porte : l'hystérésis la tient ouverte.
    #[test]
    fn la_porte_ne_bat_pas_autour_du_seuil() {
        let mut p = Porte::new(ReglagesPorte { maintien_ms: 0.0, ..Default::default() });
        let seuil = 0.1;
        let mut x = sinus(0.12, 960);
        p.traiter_trame(&mut x, seuil);
        let mut x = sinus(0.09, 9_600);
        p.traiter_trame(&mut x, seuil);
        assert!(p.gain() > 0.99, "gain {}", p.gain());
    }

    /// Le de-esser ramène une sifflante forte d'à peu près sa réduction
    /// maximale, et laisse une voix grave du même niveau intacte.
    #[test]
    fn le_deesser_ne_calme_que_les_sifflantes() {
        let r = ReglagesDeesser { frequence: 6_500.0, seuil_db: -30.0, reduction_max_db: 8.0 };
        let mut d = Deesser::new(r);
        let mut s = sinus_a(6_500.0, 0.3, 48_000);
        d.traiter_trame(&mut s);
        let sifflante = db(crete(&s[24_000..]) / 0.3);
        assert!((-9.0..-6.0).contains(&sifflante), "sifflante à {sifflante:.1} dB");
        assert!(d.reduction_db() < -6.0);
        let mut d = Deesser::new(r);
        let mut v = sinus(0.3, 48_000);
        d.traiter_trame(&mut v);
        let grave = db(crete(&v[24_000..]) / 0.3);
        assert!(grave.abs() < 0.5, "voix grave à {grave:.1} dB");
    }

    /// La chaleur arrondit les crêtes et ne touche pas aux faibles niveaux ;
    /// à zéro, elle ne fait rien du tout.
    #[test]
    fn la_chaleur_arrondit_les_cretes_sans_toucher_au_bas() {
        let mut x = sinus(0.5, 4_800);
        let e = x.clone();
        Saturation::new(0.0).traiter_trame(&mut x);
        assert_eq!(x, e);
        let mut s = Saturation::new(1.0);
        let mut fort = sinus(0.5, 4_800);
        s.traiter_trame(&mut fort);
        assert!(crete(&fort) < 0.45);
        let mut faible = sinus(0.01, 4_800);
        s.traiter_trame(&mut faible);
        assert!((crete(&faible) - 0.01).abs() < 0.0005);
    }

    /// Le compresseur personnalisé rend par le rattrapage ce qu'il prend, et
    /// son aiguille dit ce qu'il a retiré.
    #[test]
    fn le_compresseur_perso_rattrape_et_se_mesure() {
        let r = ReglagesCompresseur { rattrapage_db: 6.0, ..Default::default() };
        let mut c = Compresseur::depuis(&r);
        let mut faible = sinus(0.05, 9_600);
        c.traiter_trame(&mut faible);
        assert!((crete(&faible[4_800..]) / 0.05 - 2.0).abs() < 0.05, "+6 dB de rattrapage");
        assert!(c.reduction_db().abs() < 0.01);
        let mut fort = sinus(0.9, 48_000);
        c.traiter_trame(&mut fort);
        assert!(c.reduction_db() < -5.0);
        // Les choix simples sont les mêmes compresseurs qu'avant.
        assert_eq!(ReglagesCompresseur::du_niveau(COMPRESSION_PERSO), None);
        assert!(Compresseur::emission(COMPRESSION_FORTE).is_some());
    }

    /// « Adoucir les cris » ne touche plus aux voix normales — celles que le
    /// gain automatique de chacun pose vers -10 dBFS —, seulement aux éclats.
    #[test]
    fn adoucir_les_cris_laisse_les_voix_normales() {
        let mut c = Compresseur::ecoute();
        let mut normale = sinus(0.3, 48_000); // -10,5 dBFS
        c.traiter_trame(&mut normale);
        assert!(db(crete(&normale[24_000..]) / 0.3).abs() < 0.3, "voix normale tassée");
        let mut c = Compresseur::ecoute();
        let mut cri = sinus(0.8, 48_000); // -2 dBFS
        c.traiter_trame(&mut cri);
        assert!(db(crete(&cri[24_000..]) / 0.8) < -3.0, "cri pas adouci");
    }

    #[test]
    fn le_plafond_du_limiteur_se_regle() {
        let mut l = Limiteur::avec_plafond(-6.0);
        let mut s = sinus(0.9, 4_800);
        l.traiter_trame(&mut s);
        assert!(crete(&s) <= 0.502);
        // Par défaut, rien ne change : -1 dBFS.
        assert_eq!(ReglagesStudio::default().plafond_db, -1.0);
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
