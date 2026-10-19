//! La carte tactique d'un salon vocal — V10.2 de PLAN-VALORANT-2.md.
//!
//! Une carte par salon vocal, ouverte par qui y est, vue de tout le salon.
//! Le serveur ne dessine rien : il relaie les morceaux de traits, en force
//! l'auteur — la connexion qui les envoie, jamais un nom écrit par le
//! client —, les borne, et oublie chaque trait 15 s après que son auteur a
//! levé le crayon. Rien ne va sur le disque.
//!
//! Elle suit le salon : qui arrive reçoit le plan et les traits vivants,
//! avec le temps qui leur reste ; qui part n'a plus de carte, et ses traits
//! s'éteignent chez les autres ; un salon vide efface la sienne. Un membre
//! rendu muet par un modérateur la regarde, mais n'y dessine pas : le
//! dessin est une parole.
//!
//! [`Salle`] ne connaît ni le réseau ni l'horloge : on lui passe l'instant,
//! et ses tests font vieillir les traits sans attendre.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ki_protocol::{
    plan_tactique_valide, ActionTactique, CarteTactique, ChannelId, EvenementTactique, OutilTactique,
    PointTactique, ServerMsg, UserId, TACTIQUE_POINTS_PAR_MORCEAU, TACTIQUE_POINTS_PAR_TRAIT,
    TACTIQUE_TRAITS_PAR_JOUEUR, TACTIQUE_VIE_MS,
};

use crate::state::AppState;

const VIE: Duration = Duration::from_millis(TACTIQUE_VIE_MS as u64);

/// Pourquoi un geste est refusé, dit à celui qui l'a fait.
pub type Refus = &'static str;

struct Trait {
    auteur: UserId,
    id: u32,
    outil: OutilTactique,
    points: Vec<PointTactique>,
    /// Le crayon levé : ses 15 s partent de là.
    fini: Option<Instant>,
    /// Son dernier morceau. Un trait laissé en plan — un client qui se tait
    /// sans lever le crayon — vieillit à partir de là.
    maj: Instant,
}

impl Trait {
    fn age(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.fini.unwrap_or(self.maj))
    }

    /// Tout le trait, pour qui arrive : ses points depuis le premier, et ce
    /// qui lui reste à vivre s'il est fini.
    fn entier(&self, now: Instant) -> EvenementTactique {
        EvenementTactique::Trait {
            auteur: self.auteur,
            id: self.id,
            outil: self.outil,
            depuis: 0,
            points: self.points.clone(),
            fin: self.fini.is_some(),
            reste_ms: self.fini.map(|_| VIE.saturating_sub(self.age(now)).as_millis() as u32),
        }
    }
}

/// La carte d'un salon : son plan, ses traits vivants, et ceux qui l'ont
/// reçue.
pub struct Salle {
    salon: ChannelId,
    carte: String,
    par: UserId,
    traits: Vec<Trait>,
    vus: HashSet<UserId>,
}

impl Salle {
    pub fn new(salon: ChannelId, carte: &str, par: UserId) -> Self {
        Self { salon, carte: carte.to_string(), par, traits: Vec::new(), vus: HashSet::new() }
    }

    fn vue(&self) -> CarteTactique {
        CarteTactique { salon: self.salon, carte: self.carte.clone(), par: self.par }
    }

    /// Ce que reçoit qui arrive : la carte, puis chaque trait vivant.
    fn arrivee(&self, now: Instant) -> Vec<ServerMsg> {
        let mut msgs = vec![ServerMsg::TactiqueEtat { carte: Some(self.vue()) }];
        msgs.extend(self.traits.iter().map(|t| ServerMsg::Tactique { evenement: t.entier(now) }));
        msgs
    }

    /// Les traits morts s'en vont.
    fn purger(&mut self, now: Instant) {
        self.traits.retain(|t| t.age(now) < VIE);
    }

    /// Un nouveau plan : tous les traits s'effacent.
    fn choisir(&mut self, carte: &str, par: UserId) -> EvenementTactique {
        self.carte = carte.to_string();
        self.par = par;
        self.traits.clear();
        EvenementTactique::Plan { carte: self.carte.clone(), par }
    }

    /// Un morceau du trait `id` de `auteur`. Rend ce qu'il faut dire au
    /// salon : le morceau tel qu'il est gardé, et le retrait du plus ancien
    /// des traits de l'auteur quand celui-ci en fait un de trop.
    ///
    /// Un morceau pour un trait déjà fini n'a plus d'effet ; au-delà de
    /// [`TACTIQUE_POINTS_PAR_TRAIT`], la suite est ignorée.
    fn tracer(
        &mut self,
        auteur: UserId,
        id: u32,
        outil: OutilTactique,
        mut points: Vec<PointTactique>,
        fin: bool,
        now: Instant,
    ) -> Result<Vec<EvenementTactique>, Refus> {
        if points.len() > TACTIQUE_POINTS_PAR_MORCEAU {
            return Err("un morceau de trait trop long");
        }
        self.purger(now);
        let mut evenements = Vec::new();
        let i = match self.traits.iter().position(|t| t.auteur == auteur && t.id == id) {
            Some(i) if self.traits[i].fini.is_some() => return Ok(evenements),
            Some(i) => i,
            None => {
                self.traits.push(Trait { auteur, id, outil, points: Vec::new(), fini: None, maj: now });
                let siens: Vec<usize> = (0..self.traits.len()).filter(|&k| self.traits[k].auteur == auteur).collect();
                if siens.len() > TACTIQUE_TRAITS_PAR_JOUEUR {
                    let vieux = self.traits.remove(siens[0]);
                    evenements.push(EvenementTactique::Retire { auteur, id: vieux.id });
                }
                self.traits.len() - 1
            }
        };
        let t = &mut self.traits[i];
        let depuis = match t.outil {
            // Une zone, c'est son centre et son bord : chaque morceau les
            // remplace.
            OutilTactique::Zone if !points.is_empty() => {
                points.truncate(2);
                t.points.clear();
                0
            }
            OutilTactique::Zone => t.points.len(),
            _ => {
                points.truncate(TACTIQUE_POINTS_PAR_TRAIT.saturating_sub(t.points.len()));
                t.points.len()
            }
        };
        t.points.extend_from_slice(&points);
        t.maj = now;
        if fin {
            t.fini = Some(now);
        }
        if !points.is_empty() || fin {
            evenements.insert(
                0,
                EvenementTactique::Trait {
                    auteur,
                    id,
                    outil: t.outil,
                    depuis: depuis as u32,
                    points,
                    fin,
                    reste_ms: None,
                },
            );
        }
        Ok(evenements)
    }

    /// Retire le trait `id` de `auteur`, s'il est encore là.
    fn annuler(&mut self, auteur: UserId, id: u32) -> Option<EvenementTactique> {
        let i = self.traits.iter().position(|t| t.auteur == auteur && t.id == id)?;
        self.traits.remove(i);
        Some(EvenementTactique::Retire { auteur, id })
    }

    /// Retire tous les traits de `auteur`, s'il en a.
    fn effacer(&mut self, auteur: UserId) -> Option<EvenementTactique> {
        let avant = self.traits.len();
        self.traits.retain(|t| t.auteur != auteur);
        (self.traits.len() < avant).then_some(EvenementTactique::RetireTout { auteur })
    }

    /// Le salon compte désormais `presents` : le partant n'a plus de carte
    /// et ses traits s'éteignent chez les autres, le nouveau venu reçoit la
    /// carte et ce qui y vit. Faux : le salon s'est vidé, la carte s'efface.
    fn suivre(&mut self, presents: HashSet<UserId>, now: Instant, envois: &mut Envois) -> bool {
        self.purger(now);
        for &id in self.vus.difference(&presents) {
            envois.push((id, ServerMsg::TactiqueEtat { carte: None }));
        }
        if presents.is_empty() {
            return false;
        }
        // Le retrait, à ceux qui restent : le nouveau venu ne les a pas.
        let restent: HashSet<UserId> = presents.intersection(&self.vus).copied().collect();
        let partis: HashSet<UserId> = self.traits.iter().map(|t| t.auteur).filter(|a| !presents.contains(a)).collect();
        for auteur in partis {
            if let Some(evenement) = self.effacer(auteur) {
                a_tous(envois, &restent, None, &evenement);
            }
        }
        for &id in presents.difference(&self.vus) {
            envois.extend(self.arrivee(now).into_iter().map(|m| (id, m)));
        }
        self.vus = presents;
        true
    }
}

/// Des messages préparés sous le verrou des cartes, envoyés après : on ne
/// tient jamais celui des connectés en même temps.
type Envois = Vec<(UserId, ServerMsg)>;

/// Les cartes, une par salon vocal.
#[derive(Default)]
pub struct Tactiques {
    salles: Mutex<HashMap<ChannelId, Salle>>,
}

/// Les « plus de carte » d'abord : qui passe d'un salon à carte à un autre
/// doit recevoir la fin de l'ancienne avant la nouvelle.
fn envoyer(state: &AppState, mut envois: Envois) {
    envois.sort_by_key(|(_, msg)| !matches!(msg, ServerMsg::TactiqueEtat { carte: None }));
    for (id, msg) in envois {
        state.send_to(id, &msg);
    }
}

/// À chacun de `qui`, sauf `sauf`.
fn a_tous(envois: &mut Envois, qui: &HashSet<UserId>, sauf: Option<UserId>, evenement: &EvenementTactique) {
    for &id in qui.iter().filter(|&&id| Some(id) != sauf) {
        envois.push((id, ServerMsg::Tactique { evenement: evenement.clone() }));
    }
}

/// Un geste sur la carte du salon vocal de `user_id`.
pub fn agir(state: &AppState, user_id: UserId, action: ActionTactique) -> Result<(), Refus> {
    let (salon, muet) = {
        let users = state.users.lock().unwrap();
        users.get(&user_id).and_then(|u| Some((u.voice?, u.force_muted)))
    }
    .ok_or("rejoins un salon vocal pour ouvrir la carte tactique")?;
    if muet {
        match action {
            // Un refus par trait, au crayon levé, et pas un par morceau.
            ActionTactique::Trait { fin: false, .. } => return Ok(()),
            ActionTactique::Trait { .. } | ActionTactique::Choisir { .. } => {
                return Err("un modérateur t'a rendu muet : tu regardes la carte, sans y dessiner")
            }
            _ => {}
        }
    }
    // Qui est dans le salon, lu avant de prendre le verrou des cartes — et
    // seulement pour ouvrir : un morceau de trait n'en a pas besoin.
    let presents = matches!(action, ActionTactique::Ouvrir { .. })
        .then(|| crate::cartes::occupants(state).remove(&salon).unwrap_or_default());
    let now = Instant::now();
    let mut envois = Vec::new();
    {
        let mut salles = state.tactique.salles.lock().unwrap();
        if let ActionTactique::Ouvrir { carte } = &action {
            let carte = plan_tactique_valide(carte).ok_or("ce nom de carte ne va pas")?;
            match salles.get_mut(&salon) {
                // Déjà ouverte : je la reçois, rien ne change.
                Some(salle) => {
                    salle.purger(now);
                    salle.vus.insert(user_id);
                    envois.extend(salle.arrivee(now).into_iter().map(|m| (user_id, m)));
                }
                None => {
                    tracing::info!("tactique : carte ouverte dans le salon {salon} ({carte})");
                    let mut salle = Salle::new(salon, carte, user_id);
                    salle.vus = presents.unwrap_or_default();
                    salle.vus.insert(user_id);
                    for &id in &salle.vus {
                        envois.extend(salle.arrivee(now).into_iter().map(|m| (id, m)));
                    }
                    salles.insert(salon, salle);
                }
            }
        } else {
            let salle = salles.get_mut(&salon).ok_or("personne n'a ouvert la carte tactique dans ton salon")?;
            match action {
                ActionTactique::Ouvrir { .. } => {}
                ActionTactique::Choisir { carte } => {
                    let carte = plan_tactique_valide(&carte).ok_or("ce nom de carte ne va pas")?;
                    let evenement = salle.choisir(carte, user_id);
                    a_tous(&mut envois, &salle.vus, None, &evenement);
                }
                ActionTactique::Trait { id, outil, points, fin } => {
                    for evenement in salle.tracer(user_id, id, outil, points, fin, now)? {
                        // Son auteur a déjà son trait ; un retrait, lui, se dit à tous.
                        let sauf = matches!(evenement, EvenementTactique::Trait { .. }).then_some(user_id);
                        a_tous(&mut envois, &salle.vus, sauf, &evenement);
                    }
                }
                ActionTactique::Annuler { id } => {
                    if let Some(evenement) = salle.annuler(user_id, id) {
                        a_tous(&mut envois, &salle.vus, None, &evenement);
                    }
                }
                ActionTactique::EffacerLesMiens => {
                    if let Some(evenement) = salle.effacer(user_id) {
                        a_tous(&mut envois, &salle.vus, None, &evenement);
                    }
                }
            }
        }
    }
    envoyer(state, envois);
    Ok(())
}

/// Qui est où a peut-être changé : appelé à chaque reconstruction des
/// routes vocales, comme [`crate::cartes::accorder`]. Le nouveau venu
/// reçoit la carte et ses traits, le partant apprend qu'il n'en a plus,
/// ses traits s'éteignent chez les autres, et un salon vide efface la
/// sienne.
pub fn accorder(state: &AppState) {
    let mut salons = crate::cartes::occupants(state);
    let now = Instant::now();
    let mut envois = Vec::new();
    {
        let mut salles = state.tactique.salles.lock().unwrap();
        salles.retain(|salon, salle| {
            let garde = salle.suivre(salons.remove(salon).unwrap_or_default(), now, &mut envois);
            if !garde {
                tracing::info!("tactique : le salon {salon} s'est vidé, sa carte s'efface");
            }
            garde
        });
    }
    envoyer(state, envois);
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: UserId = 1;
    const B: UserId = 2;

    fn salle() -> Salle {
        Salle::new(7, "Ascent", A)
    }

    fn points(n: usize) -> Vec<PointTactique> {
        (0..n).map(|k| [k as u16, (k * 2) as u16]).collect()
    }

    fn seuls_traits(evenements: &[EvenementTactique]) -> Vec<(u32, usize, usize, bool)> {
        evenements
            .iter()
            .filter_map(|e| match e {
                EvenementTactique::Trait { id, depuis, points, fin, .. } => Some((*id, *depuis as usize, points.len(), *fin)),
                _ => None,
            })
            .collect()
    }

    /// Un trait s'écrit par morceaux, chacun à sa place ; une zone remplace
    /// son centre et son bord ; un morceau pour un trait fini ne fait rien.
    #[test]
    fn un_trait_se_trace_par_morceaux() {
        let now = Instant::now();
        let mut s = salle();
        let e = s.tracer(A, 1, OutilTactique::Fleche, points(5), false, now).unwrap();
        assert_eq!(seuls_traits(&e), vec![(1, 0, 5, false)]);
        let e = s.tracer(A, 1, OutilTactique::Crayon, points(3), true, now).unwrap();
        assert_eq!(seuls_traits(&e), vec![(1, 5, 3, true)]);
        let EvenementTactique::Trait { outil, .. } = &e[0] else { panic!() };
        assert_eq!(*outil, OutilTactique::Fleche, "l'outil est celui du premier morceau");
        assert!(s.tracer(A, 1, OutilTactique::Fleche, points(2), false, now).unwrap().is_empty(), "fini, il ne bouge plus");
        assert_eq!(s.traits[0].points.len(), 8);

        s.tracer(B, 1, OutilTactique::Zone, points(2), false, now).unwrap();
        let e = s.tracer(B, 1, OutilTactique::Zone, points(4), true, now).unwrap();
        assert_eq!(seuls_traits(&e), vec![(1, 0, 2, true)], "une zone garde deux points, toujours depuis le premier");
        assert_eq!(s.traits.len(), 2, "le même numéro chez deux auteurs, deux traits");
    }

    /// Les bornes : un morceau trop long est refusé entier, un trait trop
    /// long s'arrête, et au-delà de trente traits le plus ancien de l'auteur
    /// s'en va — et le dit.
    #[test]
    fn les_bornes_tiennent() {
        let now = Instant::now();
        let mut s = salle();
        assert!(s.tracer(A, 1, OutilTactique::Crayon, points(TACTIQUE_POINTS_PAR_MORCEAU + 1), false, now).is_err());
        assert!(s.traits.is_empty());
        for _ in 0..TACTIQUE_POINTS_PAR_TRAIT / TACTIQUE_POINTS_PAR_MORCEAU + 2 {
            s.tracer(A, 1, OutilTactique::Crayon, points(TACTIQUE_POINTS_PAR_MORCEAU), false, now).unwrap();
        }
        assert_eq!(s.traits[0].points.len(), TACTIQUE_POINTS_PAR_TRAIT);
        assert!(s.tracer(A, 1, OutilTactique::Crayon, points(10), false, now).unwrap().is_empty(), "plein : plus rien à dire");
        let fin = s.tracer(A, 1, OutilTactique::Crayon, points(10), true, now).unwrap();
        assert_eq!(seuls_traits(&fin), vec![(1, TACTIQUE_POINTS_PAR_TRAIT, 0, true)], "mais le crayon se lève");

        s.tracer(B, 1, OutilTactique::Fleche, points(1), true, now).unwrap();
        for id in 2..=TACTIQUE_TRAITS_PAR_JOUEUR as u32 {
            s.tracer(A, id, OutilTactique::Fleche, points(1), true, now).unwrap();
        }
        let e = s.tracer(A, 99, OutilTactique::Fleche, points(1), false, now).unwrap();
        assert_eq!(e.last(), Some(&EvenementTactique::Retire { auteur: A, id: 1 }));
        assert_eq!(s.traits.iter().filter(|t| t.auteur == A).count(), TACTIQUE_TRAITS_PAR_JOUEUR);
        assert_eq!(s.traits.iter().filter(|t| t.auteur == B).count(), 1, "les siens ne paient pas pour les miens");
    }

    /// Un trait fini vit 15 s ; un trait laissé en plan aussi, depuis son
    /// dernier morceau ; qui arrive reçoit les vivants avec leur reste.
    #[test]
    fn les_traits_s_eteignent_et_se_rejouent() {
        let t0 = Instant::now();
        let mut s = salle();
        s.tracer(A, 1, OutilTactique::Fleche, points(3), true, t0).unwrap();
        s.tracer(B, 1, OutilTactique::Crayon, points(2), false, t0).unwrap();
        s.tracer(B, 2, OutilTactique::Crayon, points(2), false, t0 + Duration::from_secs(10)).unwrap();
        let arrivee = s.arrivee(t0 + Duration::from_secs(4));
        let ServerMsg::TactiqueEtat { carte: Some(carte) } = &arrivee[0] else { panic!() };
        assert_eq!(*carte, CarteTactique { salon: 7, carte: "Ascent".into(), par: A });
        let ServerMsg::Tactique { evenement: EvenementTactique::Trait { points, fin, reste_ms, depuis, .. } } = &arrivee[1] else {
            panic!()
        };
        assert_eq!((points.len(), *fin, *reste_ms, *depuis), (3, true, Some(11_000), 0));
        let ServerMsg::Tactique { evenement: EvenementTactique::Trait { fin, reste_ms, .. } } = &arrivee[2] else { panic!() };
        assert_eq!((*fin, *reste_ms), (false, None), "en cours : pas de compte à rebours");

        s.purger(t0 + VIE);
        let restent: Vec<(UserId, u32)> = s.traits.iter().map(|t| (t.auteur, t.id)).collect();
        assert_eq!(restent, vec![(B, 2)], "le fini et le laissé en plan s'en vont, le récent reste");
    }

    /// Annuler, effacer les siens et changer de plan ne touchent que ce
    /// qu'ils disent, et ne se disent que s'il y avait quelque chose.
    #[test]
    fn les_gestes_se_tiennent() {
        let now = Instant::now();
        let mut s = salle();
        s.tracer(A, 1, OutilTactique::Fleche, points(2), true, now).unwrap();
        s.tracer(A, 2, OutilTactique::Fleche, points(2), true, now).unwrap();
        s.tracer(B, 1, OutilTactique::Fleche, points(2), true, now).unwrap();
        assert_eq!(s.annuler(B, 2), None);
        assert_eq!(s.annuler(A, 1), Some(EvenementTactique::Retire { auteur: A, id: 1 }));
        assert_eq!(s.effacer(A), Some(EvenementTactique::RetireTout { auteur: A }));
        assert_eq!(s.effacer(A), None);
        assert_eq!(s.traits.len(), 1);
        assert_eq!(s.choisir("Lotus", B), EvenementTactique::Plan { carte: "Lotus".into(), par: B });
        assert!(s.traits.is_empty());
        assert_eq!(s.vue().carte, "Lotus");
    }

    /// La carte suit le salon : qui arrive reçoit la carte puis les traits,
    /// qui part n'a plus de carte et ses traits s'éteignent chez les
    /// autres, et un salon vide l'efface.
    #[test]
    fn la_carte_suit_le_salon() {
        const C: UserId = 3;
        let now = Instant::now();
        let mut s = salle();
        let mut envois = Vec::new();
        assert!(s.suivre(HashSet::from([A, B]), now, &mut envois));
        assert_eq!(envois.len(), 2, "la carte à chacun, sans trait encore");
        s.tracer(A, 1, OutilTactique::Fleche, points(4), true, now).unwrap();
        s.tracer(B, 1, OutilTactique::Zone, points(2), false, now).unwrap();

        let mut envois = Vec::new();
        assert!(s.suivre(HashSet::from([B, C]), now, &mut envois));
        let pour = |qui: UserId| envois.iter().filter(|(id, _)| *id == qui).map(|(_, m)| m.clone()).collect::<Vec<_>>();
        assert!(matches!(pour(A)[..], [ServerMsg::TactiqueEtat { carte: None }]), "le partant n'a plus de carte");
        assert!(matches!(
            pour(B)[..],
            [ServerMsg::Tactique { evenement: EvenementTactique::RetireTout { auteur: A } }]
        ));
        let arrivee = pour(C);
        assert!(matches!(arrivee[0], ServerMsg::TactiqueEtat { carte: Some(_) }));
        let traits: Vec<UserId> = arrivee
            .iter()
            .filter_map(|m| match m {
                ServerMsg::Tactique { evenement: EvenementTactique::Trait { auteur, .. } } => Some(*auteur),
                _ => None,
            })
            .collect();
        assert_eq!(traits, vec![B], "les traits du partant ne lui arrivent pas");

        let mut envois = Vec::new();
        assert!(!s.suivre(HashSet::new(), now, &mut envois), "un salon vide efface sa carte");
        assert_eq!(envois.len(), 2);
        assert!(envois.iter().all(|(_, m)| matches!(m, ServerMsg::TactiqueEtat { carte: None })));
    }
}
