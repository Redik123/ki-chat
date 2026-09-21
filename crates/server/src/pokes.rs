//! Le « poke » : un signe à quelqu'un — un son et un clignotement chez lui,
//! rien d'autre. De quoi appeler celui qui traîne dans les menus sans lui
//! écrire, et sans qu'il ait à lire.
//!
//! # Deux garde-fous
//!
//! Un signe qui ne se refuse pas devient vite un harcèlement. Le module
//! borne donc ce qu'un émetteur peut faire, en deux temps :
//!
//! 1. **Les règles** ([`verdict`]) : on ne poke pas quelqu'un d'absent, en
//!    vocal (il entend déjà), en partie (on ne le dérange pas), dont le
//!    client d'avant jetterait le poke sans le montrer, ou qui a dit ne pas
//!    en vouloir. Un refus de règle ne coûte rien à l'émetteur — il ne
//!    savait pas forcément.
//! 2. **Les limites** ([`Pokes::consommer`]) : un poke par paire toutes les
//!    cinq minutes (insister ne sert à rien), et par émetteur trois d'affilée
//!    puis un toutes les 200 s (on ne poke pas tout le serveur).
//!
//! Les tables vivent dans `AppState`, pas dans le connecté : un seau rangé
//! dans `ConnectedUser` repartirait plein à chaque reconnexion, et trois
//! pokes frais par reconnexion serait justement la faille. Ménage borné
//! comme `throttle.rs` : au-delà de `MAX_ENTRIES`, ce qui est périmé s'en
//! va.

use ki_protocol::UserId;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Entre deux pokes de la même personne vers la même cible.
pub const COOLDOWN_PAIRE: Duration = Duration::from_secs(5 * 60);
/// Pokes d'affilée tolérés par émetteur…
const RAFALE: f32 = 3.0;
/// … puis un toutes les 200 s (trois en dix minutes).
const REGIME: Duration = Duration::from_secs(200);
/// Entrées au-delà desquelles on fait le ménage — sans quoi la table
/// grossirait avec chaque paire jamais vue.
const MAX_ENTRIES: usize = 4096;

/// Ce que le serveur sait de la cible au moment du poke, lu sous le verrou
/// des connectés puis relâché : les règles s'appliquent sans verrou.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cible {
    pub en_ligne: bool,
    pub en_vocal: bool,
    pub en_partie: bool,
    /// Son client a annoncé son réglage (`AccepterPokes`), donc il sait
    /// recevoir un `Poke`. Un client antérieur ne l'annonce jamais — et
    /// jette le message : sans cette règle, le jeton et le délai de paire
    /// partaient pour rien, et l'émetteur croyait avoir appelé.
    pub client_recent: bool,
    pub accepte: bool,
}

/// Les règles, dans l'ordre où elles se disent : ce que la cible fait passe
/// avant ce qu'elle veut. Le motif est la fin de la phrase, à faire
/// précéder du pseudo (« Nono est en vocal »).
pub fn verdict(cible: &Cible) -> Result<(), &'static str> {
    if !cible.en_ligne {
        return Err("est hors ligne");
    }
    if cible.en_vocal {
        return Err("est en vocal");
    }
    if cible.en_partie {
        return Err("est en partie");
    }
    if !cible.client_recent {
        return Err("a un client d'avant, qui ne reçoit pas les pokes");
    }
    if !cible.accepte {
        return Err("n'accepte pas les pokes");
    }
    Ok(())
}

/// Pourquoi une limite a refusé le poke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limite {
    /// Cette cible a déjà été pokée par cet émetteur il y a `depuis`.
    DejaPoke { depuis: Duration },
    /// L'émetteur a épuisé sa réserve.
    TropDePokes,
}

/// Le seau de l'émetteur : `RAFALE` jetons au départ, un de regagné par
/// `REGIME`. Le même principe que `state::TokenBucket`, avec l'instant en
/// paramètre — c'est ce qui le rend testable sans attendre dix minutes.
struct Seau {
    jetons: f32,
    dernier: Instant,
}

impl Seau {
    fn neuf(now: Instant) -> Self {
        Self { jetons: RAFALE, dernier: now }
    }

    fn prendre(&mut self, now: Instant) -> bool {
        let regagne = now.duration_since(self.dernier).as_secs_f32() / REGIME.as_secs_f32();
        self.jetons = (self.jetons + regagne).min(RAFALE);
        self.dernier = now;
        if self.jetons < 1.0 {
            return false;
        }
        self.jetons -= 1.0;
        true
    }

    /// Plein à nouveau : plus rien à retenir de lui.
    fn oubliable(&self, now: Instant) -> bool {
        now.duration_since(self.dernier) >= REGIME.mul_f32(RAFALE)
    }
}

#[derive(Default)]
struct Tables {
    /// (émetteur, cible) -> dernier poke parti.
    paires: HashMap<(UserId, UserId), Instant>,
    emetteurs: HashMap<UserId, Seau>,
}

#[derive(Default)]
pub struct Pokes {
    tables: Mutex<Tables>,
}

impl Pokes {
    /// Consomme les limites pour un poke de `de` vers `vers` — à n'appeler
    /// qu'après un [`verdict`] favorable : un refus de règle ne doit pas
    /// coûter de jeton. La paire est vérifiée avant le seau, pour la même
    /// raison : insister sur la même cible ne vide pas la réserve.
    pub fn consommer(&self, de: UserId, vers: UserId) -> Result<(), Limite> {
        self.consommer_at(de, vers, Instant::now())
    }

    fn consommer_at(&self, de: UserId, vers: UserId, now: Instant) -> Result<(), Limite> {
        let mut t = self.tables.lock().unwrap();
        if let Some(dernier) = t.paires.get(&(de, vers)) {
            let depuis = now.duration_since(*dernier);
            if depuis < COOLDOWN_PAIRE {
                return Err(Limite::DejaPoke { depuis });
            }
        }
        if t.paires.len() >= MAX_ENTRIES {
            t.paires.retain(|_, d| now.duration_since(*d) < COOLDOWN_PAIRE);
        }
        if t.emetteurs.len() >= MAX_ENTRIES {
            t.emetteurs.retain(|_, s| !s.oubliable(now));
        }
        let seau = t.emetteurs.entry(de).or_insert_with(|| Seau::neuf(now));
        if !seau.prendre(now) {
            return Err(Limite::TropDePokes);
        }
        t.paires.insert((de, vers), now);
        Ok(())
    }
}

/// Un délai en minutes, tel qu'on le dit : jamais « il y a 0 min ».
pub fn minutes(d: Duration) -> u64 {
    (d.as_secs() / 60).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const JOIGNABLE: Cible =
        Cible { en_ligne: true, en_vocal: false, en_partie: false, client_recent: true, accepte: true };

    #[test]
    fn les_regles_se_disent_dans_l_ordre() {
        assert_eq!(verdict(&JOIGNABLE), Ok(()));
        // Hors ligne prime sur tout : le reste ne veut rien dire.
        assert_eq!(
            verdict(&Cible { en_ligne: false, en_vocal: true, en_partie: true, client_recent: false, accepte: false }),
            Err("est hors ligne")
        );
        assert_eq!(
            verdict(&Cible { en_vocal: true, en_partie: true, client_recent: false, accepte: false, ..JOIGNABLE }),
            Err("est en vocal")
        );
        assert_eq!(
            verdict(&Cible { en_partie: true, client_recent: false, accepte: false, ..JOIGNABLE }),
            Err("est en partie")
        );
        // Un client d'avant jetterait le poke : refusé avant de consommer
        // quoi que ce soit, et quel que soit le réglage qu'on lui prête.
        assert_eq!(
            verdict(&Cible { client_recent: false, ..JOIGNABLE }),
            Err("a un client d'avant, qui ne reçoit pas les pokes")
        );
        assert_eq!(
            verdict(&Cible { client_recent: false, accepte: false, ..JOIGNABLE }),
            Err("a un client d'avant, qui ne reçoit pas les pokes")
        );
        assert_eq!(verdict(&Cible { accepte: false, ..JOIGNABLE }), Err("n'accepte pas les pokes"));
    }

    #[test]
    fn la_meme_cible_attend_cinq_minutes_sans_vider_la_reserve() {
        let pokes = Pokes::default();
        let t0 = Instant::now();
        assert_eq!(pokes.consommer_at(1, 2, t0), Ok(()));
        // Insister : refusé avec le délai, et sans coûter de jeton…
        let bientot = t0 + Duration::from_secs(90);
        assert_eq!(pokes.consommer_at(1, 2, bientot), Err(Limite::DejaPoke { depuis: Duration::from_secs(90) }));
        assert_eq!(pokes.consommer_at(1, 2, bientot), Err(Limite::DejaPoke { depuis: Duration::from_secs(90) }));
        // … la preuve : deux autres cibles passent encore (rafale de trois).
        assert_eq!(pokes.consommer_at(1, 3, bientot), Ok(()));
        assert_eq!(pokes.consommer_at(1, 4, bientot), Ok(()));
        // Une autre cible n'est pas gênée par la paire (1, 2).
        assert_eq!(pokes.consommer_at(5, 2, bientot), Ok(()));
        // Cinq minutes plus tard, la paire est libre — mais la réserve de 1
        // (trois pris entre t0 et t0+90 s, 210 s écoulés depuis le dernier)
        // n'a regagné qu'un jeton et demi : celui-ci passe, le suivant non.
        let apres = t0 + COOLDOWN_PAIRE;
        assert_eq!(pokes.consommer_at(1, 2, apres), Ok(()));
        assert_eq!(pokes.consommer_at(1, 6, apres), Err(Limite::TropDePokes));
    }

    #[test]
    fn trois_d_affilee_puis_un_toutes_les_200_s() {
        let pokes = Pokes::default();
        let t0 = Instant::now();
        for cible in 10..13 {
            assert_eq!(pokes.consommer_at(1, cible, t0), Ok(()));
        }
        assert_eq!(pokes.consommer_at(1, 13, t0), Err(Limite::TropDePokes));
        // Juste avant les 200 s : toujours non.
        assert_eq!(pokes.consommer_at(1, 13, t0 + REGIME - Duration::from_secs(1)), Err(Limite::TropDePokes));
        // Après : un seul.
        assert_eq!(pokes.consommer_at(1, 13, t0 + REGIME), Ok(()));
        assert_eq!(pokes.consommer_at(1, 14, t0 + REGIME), Err(Limite::TropDePokes));
        // Un autre émetteur a sa propre réserve.
        assert_eq!(pokes.consommer_at(2, 13, t0), Ok(()));
    }

    #[test]
    fn les_tables_ne_grossissent_pas_sans_fin() {
        let pokes = Pokes::default();
        let t0 = Instant::now();
        // Des paires toutes différentes, d'émetteurs tous différents : ni
        // le délai par paire ni le seau ne les arrêtent.
        for n in 0..MAX_ENTRIES as UserId {
            assert_eq!(pokes.consommer_at(n, n + 1_000_000, t0), Ok(()));
        }
        let avant = pokes.tables.lock().unwrap().paires.len();
        assert!(avant >= MAX_ENTRIES, "prémisse du test : {avant}");
        // Une fois tout périmé, le poke suivant fait le ménage.
        let tard = t0 + REGIME.mul_f32(RAFALE) + Duration::from_secs(1);
        assert_eq!(pokes.consommer_at(1, 2, tard), Ok(()));
        let t = pokes.tables.lock().unwrap();
        assert!(t.paires.len() < avant, "paires non purgées : {avant} -> {}", t.paires.len());
        assert!(t.emetteurs.len() < avant, "seaux non purgés : {avant} -> {}", t.emetteurs.len());
    }

    #[test]
    fn un_delai_se_dit_en_minutes_entieres_jamais_zero() {
        assert_eq!(minutes(Duration::from_secs(5)), 1);
        assert_eq!(minutes(Duration::from_secs(60)), 1);
        assert_eq!(minutes(Duration::from_secs(179)), 2);
    }
}
