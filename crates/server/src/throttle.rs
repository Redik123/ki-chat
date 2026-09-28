//! Limiteur de tentatives d'authentification.
//!
//! Deux dangers, pas un seul :
//!
//! 1. **Deviner un mot de passe** en enchaînant les essais ;
//! 2. **épuiser le serveur** — chaque essai déclenche un hachage Argon2id,
//!    volontairement coûteux en mémoire et en temps. Quelques centaines
//!    d'essais simultanés suffisent à saturer la machine, sans avoir la
//!    moindre chance de trouver le mot de passe.
//!
//! Le limiteur répond aux deux en refusant l'essai **avant** de lancer le
//! hachage : le coût d'une tentative refusée est alors une recherche dans
//! une table.
//!
//! # Ralentir plutôt que verrouiller
//!
//! Un verrouillage de compte après N échecs se retourne contre les
//! utilisateurs : n'importe qui peut bloquer le compte d'un autre en
//! échouant exprès. On applique donc un **délai croissant**, qui rend le
//! parcours exhaustif impraticable sans jamais fermer la porte à celui qui
//! s'est trompé de touche.
//!
//! Le compteur est tenu par adresse IP **et** par compte : le premier
//! attrape celui qui essaie beaucoup de comptes depuis une machine, le
//! second celui qui s'acharne sur un compte depuis plusieurs machines.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Échecs tolérés sans aucun délai — on se trompe de touche, ça arrive.
const FREE_ATTEMPTS: u32 = 5;
/// Délai après le premier échec de trop ; il double ensuite.
const BASE_DELAY: Duration = Duration::from_secs(2);
/// Plafond du délai : au-delà, inutile de punir davantage.
const MAX_DELAY: Duration = Duration::from_secs(60);
/// Sans nouvel échec pendant ce temps, l'ardoise est effacée.
const FORGET_AFTER: Duration = Duration::from_secs(15 * 60);
/// Nombre d'entrées au-delà duquel on fait le ménage. Sans cela, le
/// limiteur deviendrait lui-même un moyen d'épuiser la mémoire : il suffit
/// d'essayer des pseudos tous différents.
const MAX_ENTRIES: usize = 4096;
/// Autant d'échecs d'un coup qu'il en faut pour que le prochain essai
/// attende le délai maximal : les gratuits, puis les doublements de 2 s
/// jusqu'à 60 s (2 × 2⁵ = 64 s, plafonné).
const ECART_ECHECS: u32 = FREE_ATTEMPTS + 6;
/// Essais **en cours** tolérés d'une même adresse. Un échec ne se compte
/// qu'une fois le hachage fini : sans ce plafond, trente-deux connexions
/// lancées ensemble passaient toutes le contrôle avant que la première ait
/// échoué — trente-deux fois le débit de devinette, et autant d'Argon2.
/// Assez large pour une salle entière derrière une même box qui se
/// reconnecte d'un coup : les autres réessaient une seconde plus tard.
const EN_VOL_PAR_ADRESSE: u32 = 8;
/// Et d'un même compte, toutes adresses confondues : un vrai titulaire n'a
/// qu'un client à la fois.
const EN_VOL_PAR_COMPTE: u32 = 2;

#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Address(IpAddr),
    Account(String),
}

struct Record {
    failures: u32,
    last: Instant,
    /// Essais réservés dont le hachage n'est pas fini.
    en_vol: u32,
}

/// Pourquoi un essai est refusé avant tout hachage.
#[derive(Debug, PartialEq, Eq)]
pub enum Refus {
    /// Trop d'échecs récents : attendre ce temps-là.
    Attendre(Duration),
    /// Trop d'essais déjà en cours depuis cette adresse ou sur ce compte.
    TropEnCours,
}

impl Refus {
    /// Ce qu'on en dit au client.
    pub fn message(&self) -> String {
        match self {
            Refus::Attendre(wait) => format!(
                "trop de tentatives — réessaie dans {} s",
                wait.as_secs().max(1)
            ),
            Refus::TropEnCours => {
                "trop de connexions en cours depuis ton adresse — réessaie dans un instant".into()
            }
        }
    }
}

/// Un essai réservé : il compte parmi les « en cours » jusqu'à son verdict.
/// Tombé sans verdict — erreur, délai dépassé —, il rend sa place sans
/// compter ni comme échec ni comme réussite.
pub struct Essai<'a> {
    throttle: &'a Throttle,
    ip: IpAddr,
    compte: String,
    rendu: bool,
}

impl Essai<'_> {
    /// Le mot de passe était bon : l'ardoise est effacée.
    pub fn reussi(mut self) {
        self.rendre();
        self.throttle.record_success(self.ip, &self.compte);
    }

    /// Le mot de passe était faux : le prochain essai sera plus lent.
    pub fn echoue(mut self) {
        self.rendre();
        self.throttle.record_failure(self.ip, &self.compte);
    }

    fn rendre(&mut self) {
        if std::mem::replace(&mut self.rendu, true) {
            return;
        }
        let mut records = self.throttle.records.lock().unwrap();
        for key in [Key::Address(self.ip), Key::Account(self.compte.clone())] {
            if let Some(record) = records.get_mut(&key) {
                record.en_vol = record.en_vol.saturating_sub(1);
            }
        }
    }
}

impl Drop for Essai<'_> {
    fn drop(&mut self) {
        self.rendre();
    }
}

#[derive(Default)]
pub struct Throttle {
    records: Mutex<HashMap<Key, Record>>,
}

impl Throttle {
    /// Autorise ou non une tentative. En cas de refus, renvoie le temps
    /// restant à patienter.
    pub fn check(&self, ip: IpAddr, username: &str) -> Result<(), Duration> {
        self.check_at(ip, username, Instant::now())
    }

    /// Contrôle **et réserve** l'essai d'un seul geste, sous le même verrou.
    ///
    /// Le contrôle seul ne faisait que lire : l'échec n'était compté qu'après
    /// le hachage, si bien que des essais lancés ensemble passaient tous le
    /// contrôle avant que le premier ait échoué. Ici l'essai compte parmi
    /// les « en cours » dès qu'il est admis, et le plafond de ceux-ci borne
    /// la concurrence par adresse comme par compte.
    pub fn reserver(&self, ip: IpAddr, username: &str) -> Result<Essai<'_>, Refus> {
        self.reserver_at(ip, username, Instant::now())
    }

    fn reserver_at(&self, ip: IpAddr, username: &str, now: Instant) -> Result<Essai<'_>, Refus> {
        let mut records = self.records.lock().unwrap();
        let keys = [Key::Address(ip), Key::Account(username.to_string())];
        let wait = keys
            .iter()
            .filter_map(|key| records.get(key).map(|record| remaining(record, now)))
            .max()
            .unwrap_or(Duration::ZERO);
        if !wait.is_zero() {
            return Err(Refus::Attendre(wait));
        }
        let en_vol = |key: &Key| records.get(key).map_or(0, |r| r.en_vol);
        if en_vol(&keys[0]) >= EN_VOL_PAR_ADRESSE || en_vol(&keys[1]) >= EN_VOL_PAR_COMPTE {
            return Err(Refus::TropEnCours);
        }
        if records.len() >= MAX_ENTRIES {
            records.retain(|_, record| {
                record.en_vol > 0 || now.duration_since(record.last) < FORGET_AFTER
            });
        }
        for key in keys {
            records
                .entry(key)
                .or_insert(Record {
                    failures: 0,
                    last: now,
                    en_vol: 0,
                })
                .en_vol += 1;
        }
        Ok(Essai {
            throttle: self,
            ip,
            compte: username.to_string(),
            rendu: false,
        })
    }

    /// Enregistre un échec : le prochain essai sera plus lent.
    pub fn record_failure(&self, ip: IpAddr, username: &str) {
        self.record_failure_at(ip, username, Instant::now());
    }

    /// Tient à l'écart : autant d'échecs d'un coup qu'il en faut pour que
    /// le prochain essai attende le délai maximal, puis la pente ordinaire
    /// — l'ardoise s'efface toujours après un quart d'heure de calme. C'est
    /// ce que fait une porte web d'une adresse qu'on vient de refuser ou
    /// d'expulser : elle peut revenir, pas dans la seconde.
    pub fn ecarter(&self, ip: IpAddr, username: &str) {
        let now = Instant::now();
        for _ in 0..ECART_ECHECS {
            self.record_failure_at(ip, username, now);
        }
    }

    /// Efface l'ardoise après une authentification réussie. Une ligne qui a
    /// encore des essais en cours reste, remise à zéro : l'effacer perdrait
    /// leur compte, et le plafond de concurrence avec.
    pub fn record_success(&self, ip: IpAddr, username: &str) {
        let mut records = self.records.lock().unwrap();
        for key in [Key::Address(ip), Key::Account(username.to_string())] {
            match records.get_mut(&key) {
                Some(record) if record.en_vol > 0 => record.failures = 0,
                Some(_) => {
                    records.remove(&key);
                }
                None => {}
            }
        }
    }

    fn check_at(&self, ip: IpAddr, username: &str, now: Instant) -> Result<(), Duration> {
        let records = self.records.lock().unwrap();
        let keys = [Key::Address(ip), Key::Account(username.to_string())];
        // Le plus sévère des deux compteurs l'emporte.
        let wait = keys
            .iter()
            .filter_map(|key| records.get(key).map(|record| remaining(record, now)))
            .max()
            .unwrap_or(Duration::ZERO);
        if wait.is_zero() {
            Ok(())
        } else {
            Err(wait)
        }
    }

    fn record_failure_at(&self, ip: IpAddr, username: &str, now: Instant) {
        let mut records = self.records.lock().unwrap();
        if records.len() >= MAX_ENTRIES {
            records.retain(|_, record| {
                record.en_vol > 0 || now.duration_since(record.last) < FORGET_AFTER
            });
        }
        for key in [Key::Address(ip), Key::Account(username.to_string())] {
            let record = records.entry(key).or_insert(Record {
                failures: 0,
                last: now,
                en_vol: 0,
            });
            // Une ardoise oubliée repart de zéro.
            if now.duration_since(record.last) >= FORGET_AFTER {
                record.failures = 0;
            }
            record.failures = record.failures.saturating_add(1);
            record.last = now;
        }
    }
}

/// Temps restant à patienter pour un compteur donné.
fn remaining(record: &Record, now: Instant) -> Duration {
    let since = now.duration_since(record.last);
    if since >= FORGET_AFTER {
        return Duration::ZERO;
    }
    required_gap(record.failures).saturating_sub(since)
}

/// Délai imposé entre deux essais, selon le nombre d'échecs accumulés.
fn required_gap(failures: u32) -> Duration {
    let Some(over) = failures.checked_sub(FREE_ATTEMPTS).filter(|n| *n > 0) else {
        return Duration::ZERO;
    };
    // 2 s, 4 s, 8 s… plafonnées. `checked_mul` évite tout débordement pour
    // un compteur qui aurait beaucoup grimpé.
    BASE_DELAY
        .checked_mul(1u32.checked_shl(over - 1).unwrap_or(u32::MAX))
        .unwrap_or(MAX_DELAY)
        .min(MAX_DELAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IP: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 7));
    const OTHER_IP: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(198, 51, 100, 9));

    #[test]
    fn a_few_mistakes_cost_nothing() {
        let throttle = Throttle::default();
        let start = Instant::now();
        for _ in 0..FREE_ATTEMPTS {
            assert!(throttle.check_at(IP, "redik", start).is_ok());
            throttle.record_failure_at(IP, "redik", start);
        }
        // Le dernier essai gratuit vient d'être consommé : ça se durcit.
        assert!(throttle.check_at(IP, "redik", start).is_ok());
        throttle.record_failure_at(IP, "redik", start);
        assert!(throttle.check_at(IP, "redik", start).is_err());
    }

    #[test]
    fn the_delay_grows_then_stops_growing() {
        assert_eq!(required_gap(0), Duration::ZERO);
        assert_eq!(required_gap(FREE_ATTEMPTS), Duration::ZERO);
        assert_eq!(required_gap(FREE_ATTEMPTS + 1), BASE_DELAY);
        assert_eq!(required_gap(FREE_ATTEMPTS + 2), BASE_DELAY * 2);
        assert_eq!(required_gap(FREE_ATTEMPTS + 3), BASE_DELAY * 4);
        // Plafonné, et sans débordement même pour un compteur absurde.
        assert_eq!(required_gap(FREE_ATTEMPTS + 40), MAX_DELAY);
        assert_eq!(required_gap(u32::MAX), MAX_DELAY);
    }

    /// Tenu à l'écart, on attend tout de suite le délai maximal — et pas
    /// davantage : la table n'a rien d'infini à retenir.
    #[test]
    fn ecarter_impose_le_delai_maximal_d_un_coup() {
        // La porte web n'a pas de compte : l'adresse sert deux fois de clé.
        let throttle = Throttle::default();
        let start = Instant::now();
        assert!(throttle.check_at(IP, &IP.to_string(), start).is_ok());
        throttle.ecarter(IP, &IP.to_string());
        let attente = throttle.check_at(IP, &IP.to_string(), Instant::now()).unwrap_err();
        assert!(attente > MAX_DELAY - Duration::from_secs(1), "{attente:?}");
        assert!(attente <= MAX_DELAY);
        assert_eq!(required_gap(ECART_ECHECS), MAX_DELAY);
        assert!(required_gap(ECART_ECHECS - 1) < MAX_DELAY, "pas un échec de trop");
        // Une autre adresse n'en sait rien.
        assert!(throttle.check_at(OTHER_IP, &OTHER_IP.to_string(), Instant::now()).is_ok());
    }

    #[test]
    fn waiting_long_enough_opens_the_door_again() {
        let throttle = Throttle::default();
        let start = Instant::now();
        for _ in 0..FREE_ATTEMPTS + 1 {
            throttle.record_failure_at(IP, "redik", start);
        }
        assert!(throttle.check_at(IP, "redik", start).is_err());
        // Juste avant l'échéance, c'est encore non.
        assert!(throttle
            .check_at(IP, "redik", start + BASE_DELAY / 2)
            .is_err());
        // Après, on peut réessayer.
        assert!(throttle.check_at(IP, "redik", start + BASE_DELAY).is_ok());
    }

    #[test]
    fn a_successful_login_wipes_the_slate() {
        let throttle = Throttle::default();
        let start = Instant::now();
        for _ in 0..FREE_ATTEMPTS + 3 {
            throttle.record_failure_at(IP, "redik", start);
        }
        assert!(throttle.check_at(IP, "redik", start).is_err());
        throttle.record_success(IP, "redik");
        assert!(throttle.check_at(IP, "redik", start).is_ok());
    }

    #[test]
    fn one_machine_cannot_hide_behind_many_accounts() {
        let throttle = Throttle::default();
        let start = Instant::now();
        // Chaque essai vise un compte différent : le compteur par compte ne
        // verrait rien, c'est celui par adresse qui doit arrêter la série.
        for n in 0..FREE_ATTEMPTS + 1 {
            throttle.record_failure_at(IP, &format!("victime{n}"), start);
        }
        assert!(throttle.check_at(IP, "encore-une-autre", start).is_err());
        // Une autre machine n'est pas punie pour autant.
        assert!(throttle
            .check_at(OTHER_IP, "encore-une-autre", start)
            .is_ok());
    }

    #[test]
    fn one_account_cannot_be_hammered_from_many_machines() {
        let throttle = Throttle::default();
        let start = Instant::now();
        for n in 0..FREE_ATTEMPTS + 1 {
            let ip = IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, n as u8));
            throttle.record_failure_at(ip, "redik", start);
        }
        // Depuis une machine encore jamais vue, le compte reste protégé.
        assert!(throttle.check_at(OTHER_IP, "redik", start).is_err());
        // Mais un autre compte depuis cette machine n'a rien à se reprocher.
        assert!(throttle.check_at(OTHER_IP, "alice", start).is_ok());
    }

    /// Des essais lancés ensemble ne passent plus tous le contrôle : au-delà
    /// du plafond d'essais en cours, l'adresse attend que les premiers aient
    /// rendu leur verdict.
    #[test]
    fn des_essais_simultanes_sont_plafonnes_par_adresse() {
        let throttle = Throttle::default();
        let now = Instant::now();
        let essais: Vec<_> = (0..EN_VOL_PAR_ADRESSE)
            .map(|n| throttle.reserver_at(IP, &format!("compte{n}"), now).unwrap())
            .collect();
        assert_eq!(
            throttle.reserver_at(IP, "encore", now).err(),
            Some(Refus::TropEnCours)
        );
        // Une autre adresse n'en pâtit pas.
        assert!(throttle.reserver_at(OTHER_IP, "encore", now).is_ok());
        // Un verdict rend la place.
        let mut essais = essais.into_iter();
        essais.next().unwrap().echoue();
        assert!(throttle.reserver_at(IP, "encore", now).is_ok());
    }

    /// Sur un même compte, depuis des adresses différentes : deux en cours.
    #[test]
    fn des_essais_simultanes_sont_plafonnes_par_compte() {
        let throttle = Throttle::default();
        let now = Instant::now();
        let a = throttle.reserver_at(IP, "redik", now).unwrap();
        let _b = throttle.reserver_at(OTHER_IP, "redik", now).unwrap();
        let troisieme = IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 1));
        assert_eq!(
            throttle.reserver_at(troisieme, "redik", now).err(),
            Some(Refus::TropEnCours)
        );
        // Un essai qui tombe sans verdict rend aussi sa place.
        drop(a);
        assert!(throttle.reserver_at(troisieme, "redik", now).is_ok());
    }

    /// Les échecs des essais réservés comptent comme les autres : passé les
    /// essais gratuits, l'adresse attend.
    #[test]
    fn les_echecs_des_essais_reserves_ralentissent_la_suite() {
        let throttle = Throttle::default();
        let now = Instant::now();
        for _ in 0..FREE_ATTEMPTS + 1 {
            throttle.reserver_at(IP, "redik", now).unwrap().echoue();
        }
        assert!(matches!(
            throttle.reserver(IP, "redik").err(),
            Some(Refus::Attendre(_))
        ));
        // Une réussite efface l'ardoise sans perdre les essais encore en
        // cours sur la même adresse.
        let throttle = Throttle::default();
        let en_cours = throttle.reserver_at(IP, "alice", now).unwrap();
        throttle.reserver_at(IP, "redik", now).unwrap().reussi();
        let records = throttle.records.lock().unwrap();
        assert_eq!(records.get(&Key::Address(IP)).map(|r| r.en_vol), Some(1));
        drop(records);
        drop(en_cours);
        let records = throttle.records.lock().unwrap();
        assert_eq!(records.get(&Key::Address(IP)).map(|r| r.en_vol), Some(0));
    }

    #[test]
    fn the_table_cannot_be_grown_without_bound() {
        // Le limiteur ne doit pas devenir lui-même un moyen d'épuiser la
        // mémoire : des pseudos tous différents ne doivent pas l'enfler.
        let throttle = Throttle::default();
        let start = Instant::now();
        let old = start - FORGET_AFTER - Duration::from_secs(1);
        for n in 0..MAX_ENTRIES {
            throttle.record_failure_at(IP, &format!("compte{n}"), old);
        }
        let before = throttle.records.lock().unwrap().len();
        assert!(before >= MAX_ENTRIES, "prémisse du test : {before}");

        // Un échec de plus, une fois les anciens périmés : ménage fait.
        throttle.record_failure_at(IP, "declencheur", start);
        let after = throttle.records.lock().unwrap().len();
        assert!(after < before, "table non purgée : {before} -> {after}");
    }
}
