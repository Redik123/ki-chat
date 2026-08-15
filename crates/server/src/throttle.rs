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

#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Address(IpAddr),
    Account(String),
}

struct Record {
    failures: u32,
    last: Instant,
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

    /// Enregistre un échec : le prochain essai sera plus lent.
    pub fn record_failure(&self, ip: IpAddr, username: &str) {
        self.record_failure_at(ip, username, Instant::now());
    }

    /// Efface l'ardoise après une authentification réussie.
    pub fn record_success(&self, ip: IpAddr, username: &str) {
        let mut records = self.records.lock().unwrap();
        records.remove(&Key::Address(ip));
        records.remove(&Key::Account(username.to_string()));
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
            records.retain(|_, record| now.duration_since(record.last) < FORGET_AFTER);
        }
        for key in [Key::Address(ip), Key::Account(username.to_string())] {
            let record = records.entry(key).or_insert(Record { failures: 0, last: now });
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

    #[test]
    fn waiting_long_enough_opens_the_door_again() {
        let throttle = Throttle::default();
        let start = Instant::now();
        for _ in 0..FREE_ATTEMPTS + 1 {
            throttle.record_failure_at(IP, "redik", start);
        }
        assert!(throttle.check_at(IP, "redik", start).is_err());
        // Juste avant l'échéance, c'est encore non.
        assert!(throttle.check_at(IP, "redik", start + BASE_DELAY / 2).is_err());
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
        assert!(throttle.check_at(OTHER_IP, "encore-une-autre", start).is_ok());
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
