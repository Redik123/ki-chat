//! Les données VALORANT du client : ce que le serveur lui a rendu (les
//! fiches du groupe, la soirée, sa propre fiche, son journal, ses comptes
//! Riot), ce qu'on lui a déjà demandé ou dit, et ce qu'il sait faire.
//!
//! À part de la page (`valo`, ce qu'on regarde) et du veilleur
//! (`valorant`, ce que lit le client Riot de cette machine).

use std::time::{Duration, Instant};

use std::collections::HashSet;
use std::sync::Arc;

use ki_protocol::{CurseurMatchs, FicheMembre, FicheValorant, JeuEtat, JourDeJeu, MatchEsport, MonCompte, PartieSoiree};

use crate::FicheOuverte;

/// Le temps laissé au serveur pour rendre les fiches du groupe. Il les sert
/// de son cache, en quelques millisecondes : dix secondes sans rien, c'est
/// une demande perdue.
pub(crate) const REPONSE_MAX: Duration = Duration::from_secs(10);

/// Les jours que l'onglet Matchs demande au serveur : sa plus longue
/// période. Les sept jours se lisent dedans.
pub(crate) const MATCHS_JOURS: u64 = 30;
const JOUR_MS: u64 = 86_400_000;
/// Au plus tant de pages par lecture : un serveur qui dirait toujours « la
/// suite » ne ferait pas lire sans fin.
const MATCHS_PAGES_MAX: u32 = 40;

/// Ce que l'onglet Matchs a sous la main.
#[derive(Clone, Copy, Debug)]
pub enum MatchsVus<'a> {
    /// Les parties que le serveur a rendues, toutes, de la plus récente à
    /// la plus ancienne.
    DuServeur(&'a [PartieSoiree]),
    /// Le serveur les rend, et la première lecture est en route.
    EnLecture,
    /// Rien du serveur — il est d'avant, ou n'a pas répondu : les matchs
    /// du résumé des fiches, quelques-uns par membre.
    DesFiches,
}

/// Ce que l'onglet Matchs a sous la main, possédé : de quoi le passer à une
/// fenêtre à part. Voir [`MatchsVus`].
#[derive(Clone, Debug, Default)]
pub enum Matchs {
    DuServeur(Arc<Vec<PartieSoiree>>),
    EnLecture,
    #[default]
    DesFiches,
}

impl Matchs {
    pub fn vus(&self) -> MatchsVus<'_> {
        match self {
            Matchs::DuServeur(parties) => MatchsVus::DuServeur(parties),
            Matchs::EnLecture => MatchsVus::EnLecture,
            Matchs::DesFiches => MatchsVus::DesFiches,
        }
    }

    /// Les mêmes parties — la même lecture, pas une lecture égale.
    pub fn memes(&self, autre: &Matchs) -> bool {
        match (self, autre) {
            (Matchs::DuServeur(a), Matchs::DuServeur(b)) => Arc::ptr_eq(a, b),
            (Matchs::EnLecture, Matchs::EnLecture) | (Matchs::DesFiches, Matchs::DesFiches) => true,
            _ => false,
        }
    }
}

/// Une lecture en route : sa borne, la page attendue, quand sa demande est
/// partie, et ce qui est déjà arrivé.
struct Lecture {
    depuis: u64,
    apres: Option<CurseurMatchs>,
    partie: Instant,
    pages: u32,
    recues: Vec<PartieSoiree>,
    /// Les identifiants déjà reçus : une partie arrivée entre deux pages
    /// décale la liste, et la même reviendrait deux fois.
    ids: HashSet<String>,
}

/// Les parties du groupe, lues page par page chez le serveur
/// (`ClientMsg::MatchsValorant`). Ce qu'on montre est toujours une lecture
/// finie : la suivante se fait à côté, et la remplace d'un coup.
#[derive(Default)]
pub(crate) struct MatchsGroupe {
    parties: Option<Arc<Vec<PartieSoiree>>>,
    lecture: Option<Lecture>,
    /// La dernière lecture n'a pas abouti : on ne redemande pas tout seul.
    echec: bool,
    /// Ce qu'on a date peut-être : à relire quand l'onglet se montre.
    perime: bool,
}

impl MatchsGroupe {
    /// Ce que l'onglet peut montrer, face à un serveur qui rend les
    /// parties (`servis`) ou non.
    pub(crate) fn sous_la_main(&self, servis: bool) -> Matchs {
        match &self.parties {
            Some(parties) if servis => Matchs::DuServeur(parties.clone()),
            None if servis && !self.echec => Matchs::EnLecture,
            _ => Matchs::DesFiches,
        }
    }

    /// Une lecture attend sa réponse.
    pub(crate) fn en_lecture(&self) -> bool {
        self.lecture.is_some()
    }

    /// L'onglet se montre : la demande à envoyer, s'il en faut une — la
    /// première, ou une relecture de ce qui a vieilli. Une lecture restée
    /// sans réponse est abandonnée, et ne repart qu'après [`Self::perimer`].
    pub(crate) fn a_demander(&mut self, maintenant_ms: u64, instant: Instant) -> Option<(u64, Option<CurseurMatchs>)> {
        if let Some(l) = &self.lecture {
            if instant.duration_since(l.partie) >= REPONSE_MAX {
                self.lecture = None;
                self.echec = true;
                self.perime = false;
            }
            return None;
        }
        let jamais = self.parties.is_none() && !self.echec;
        if !(jamais || self.perime) {
            return None;
        }
        self.perime = false;
        let depuis = maintenant_ms.saturating_sub(MATCHS_JOURS * JOUR_MS);
        self.lecture = Some(Lecture { depuis, apres: None, partie: instant, pages: 0, recues: Vec::new(), ids: HashSet::new() });
        Some((depuis, None))
    }

    /// Une page arrive. Celle qu'on n'attend pas — d'une lecture
    /// abandonnée, ou d'avant une reconnexion — est ignorée. Rend la
    /// demande de la page suivante ; à la dernière, la lecture remplace ce
    /// qu'on montrait.
    pub(crate) fn recevoir(
        &mut self,
        depuis: u64,
        apres: Option<CurseurMatchs>,
        parties: Vec<PartieSoiree>,
        suite: Option<CurseurMatchs>,
        instant: Instant,
    ) -> Option<(u64, Option<CurseurMatchs>)> {
        let l = self.lecture.as_mut().filter(|l| l.depuis == depuis && l.apres == apres)?;
        for p in parties {
            if p.id.is_empty() || l.ids.insert(p.id.clone()) {
                l.recues.push(p);
            }
        }
        l.pages += 1;
        if let Some(suite) = suite.filter(|_| l.pages < MATCHS_PAGES_MAX) {
            l.apres = Some(suite.clone());
            l.partie = instant;
            return Some((depuis, Some(suite)));
        }
        self.parties = self.lecture.take().map(|l| Arc::new(l.recues));
        self.echec = false;
        None
    }

    /// Ce qu'on a date peut-être — une fiche a changé, on a cliqué
    /// « Actualiser », la page se rouvre : à relire quand l'onglet se
    /// montre. Ce qui est à l'écran y reste en attendant.
    pub(crate) fn perimer(&mut self) {
        self.perime = true;
    }

    /// Une connexion neuve au même serveur : la lecture en route est
    /// perdue, et ce qu'on garde est à relire.
    pub(crate) fn reconnexion(&mut self) {
        self.lecture = None;
        self.echec = false;
        self.perime = true;
    }

    /// Un autre serveur : rien de ce qu'on savait ne vaut.
    pub(crate) fn oublier(&mut self) {
        *self = Self::default();
    }
}

/// Où en est la demande des fiches du groupe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chargement {
    /// Rien en vol.
    Repos,
    /// Une demande est partie, la réponse peut encore arriver.
    EnCours,
    /// Une demande est partie depuis trop longtemps : le serveur ne répond
    /// pas.
    EnRetard,
}

impl Chargement {
    /// D'après le temps écoulé depuis la demande, s'il y en a une en vol.
    pub(crate) fn depuis(demandee: Option<Duration>) -> Self {
        match demandee {
            None => Chargement::Repos,
            Some(t) if t < REPONSE_MAX => Chargement::EnCours,
            Some(_) => Chargement::EnRetard,
        }
    }
}

/// Ce que l'on sait de VALORANT : les fiches du groupe, la soirée, sa propre
/// fiche et son journal, ses comptes Riot, et ce que ce serveur sait faire.
#[derive(Default)]
pub(crate) struct DonneesValo {
    /// Les fiches du groupe, telles que le serveur les a rendues
    /// (`StatsValorant`), et si elles sont arrivées depuis la demande.
    pub(crate) stats: Arc<Vec<FicheMembre>>,
    pub(crate) stats_recu: bool,
    /// La demande en vol : quand elle est partie. Rendue à `None` par la
    /// réponse — ou par l'abandon, passé [`REPONSE_MAX`].
    pub(crate) stats_demandees: Option<Instant>,
    /// Les matchs d'e-sport à venir, pour l'accueil de la page.
    pub(crate) esports: Arc<Vec<MatchEsport>>,
    /// Les 168 cases jour × heure (UTC) des parties du groupe sur 30 jours.
    pub(crate) activite: Arc<Vec<u16>>,
    /// Le fil de la soirée (`ServerMsg::Soiree`), sa borne, et la dernière
    /// demande : il se relit toutes les deux minutes tant que l'accueil se
    /// montre.
    pub(crate) soiree: Arc<Vec<PartieSoiree>>,
    pub(crate) soiree_depuis: u64,
    pub(crate) soiree_demandee: Option<Instant>,
    /// Ma propre fiche, entière : « Moi » sur la carte de chaleur de
    /// l'accueil. Demandée une fois par connexion.
    pub(crate) ma_fiche: Option<Arc<FicheValorant>>,
    pub(crate) ma_fiche_demandee: bool,
    /// Mon journal des parties, tel que le serveur l'a relu, et la
    /// dernière demande.
    pub(crate) mon_journal: Option<Arc<Vec<JourDeJeu>>>,
    pub(crate) mon_journal_demande: Option<Instant>,
    /// Nos comptes Riot liés, d'après le serveur (`MesComptes`) : de quoi
    /// reconnaître, ici, celui qui est ouvert dans le client Riot.
    pub(crate) mes_comptes: Vec<MonCompte>,
    /// La fiche VALORANT ouverte au clic droit sur un membre.
    pub(crate) fiche: Option<FicheOuverte>,
    /// La fiche comparée à celle qui est ouverte (la vue Comparer du
    /// profil), demandée ou reçue.
    pub(crate) comparee: Option<FicheOuverte>,
    /// Le Riot ID en cours de saisie dans ⚙ → Jeu, et la dernière réponse
    /// du serveur à une liaison (réussie ou non, et pourquoi).
    pub(crate) riot_saisie: String,
    pub(crate) riot_message: Option<(bool, String)>,
    /// Le serveur garde les médailles (`Welcome.medailles`) : sans cette
    /// preuve, rien ne lui part.
    pub(crate) serveur_medailles: bool,
    /// Le serveur lit lui-même les médailles chez HenrikDev, et veut
    /// savoir si on les accepte (`Welcome.choix_medailles`).
    pub(crate) serveur_choix_medailles: bool,
    /// Le serveur nous dit nos comptes Riot et lit lequel est ouvert
    /// (`Welcome.comptes_joues`) : sans lui, rien à classer.
    pub(crate) serveur_comptes_joues: bool,
    /// Le serveur tient ce journal (`Welcome.journal_parties`).
    pub(crate) serveur_journal: bool,
    /// Le serveur raconte la soirée (`Welcome.soiree`).
    pub(crate) serveur_soiree: bool,
    /// Le serveur rend les parties du groupe page par page
    /// (`Welcome.matchs_groupe`), et ce qu'on en a lu.
    pub(crate) serveur_matchs: bool,
    pub(crate) matchs: MatchsGroupe,
    /// Le serveur prévient quand des fiches changent
    /// (`Welcome.suivi_valorant`), et ce qu'on lui a dit sur cette
    /// connexion : que la page se montre, ou non.
    pub(crate) serveur_suivi: bool,
    pub(crate) suivi_dit: bool,
    /// Le serveur relit le journal des parties (`Welcome.journal_lecture`),
    /// et mon journal tel qu'il l'a rendu, avec la dernière demande : il
    /// se redemande au plus toutes les cinq minutes.
    pub(crate) serveur_journal_lecture: bool,
    /// Le choix déjà dit à ce serveur depuis la connexion.
    pub(crate) choix_journal_dit: Option<bool>,
    /// Le choix déjà dit à ce serveur depuis la connexion.
    pub(crate) choix_medailles_dit: Option<bool>,
    /// L'état VALORANT du tour d'avant, pour voir une partie finir.
    pub(crate) valorant_avant: Option<JeuEtat>,
    /// Le dernier ennui des médailles consigné : on ne le répète pas.
    pub(crate) medailles_ennui: String,
}

impl DonneesValo {
    /// Ce qu'il faut dire au serveur du suivi des fiches, s'il y a du neuf :
    /// la page vient de se montrer (`Some(true)`), ou de se fermer
    /// (`Some(false)`). Rien face à un serveur qui ne prévient pas, ni
    /// quand il le sait déjà.
    pub(crate) fn suivi_a_dire(&mut self, page_montree: bool) -> Option<bool> {
        if !self.serveur_suivi || self.suivi_dit == page_montree {
            return None;
        }
        self.suivi_dit = page_montree;
        Some(page_montree)
    }

    /// Une connexion neuve : le serveur dit s'il prévient, et ne sait
    /// encore rien de ce que cette connexion montre.
    pub(crate) fn suivi_a_la_connexion(&mut self, serveur_suivi: bool) {
        self.serveur_suivi = serveur_suivi;
        self.suivi_dit = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Les parties du groupe se lisent page par page : la demande part
    /// une fois, chaque page appelle la suivante, et ce qu'on montre ne
    /// change qu'à la dernière.
    #[test]
    fn les_matchs_du_groupe_se_lisent_page_par_page() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let partie = |date: u64, id: &str| PartieSoiree { id: id.into(), date, ..Default::default() };
        let curseur = |date: u64, id: &str| CurseurMatchs { date, id: id.into() };
        let maintenant = 40 * JOUR_MS;
        let mut m = MatchsGroupe::default();
        assert!(matches!(m.sous_la_main(false).vus(), MatchsVus::DesFiches), "serveur d'avant : le résumé des fiches");
        assert!(matches!(m.sous_la_main(true).vus(), MatchsVus::EnLecture), "rien encore : la lecture va partir");

        let (depuis, apres) = m.a_demander(maintenant, t0).expect("la première demande");
        assert_eq!((depuis, &apres), (10 * JOUR_MS, &None), "trente jours en arrière, depuis le début");
        assert!(m.a_demander(maintenant, t0 + s(1)).is_none(), "une seule demande en vol");
        assert!(m.en_lecture());

        // Une page d'une autre lecture : ignorée.
        assert!(m.recevoir(1, None, vec![partie(9, "x")], None, t0).is_none());
        assert!(m.en_lecture() && matches!(m.sous_la_main(true).vus(), MatchsVus::EnLecture));

        // La première page, puis la seconde, qui redonne la partie « b » :
        // une partie arrivée entre-temps a décalé la liste.
        let suite = m.recevoir(depuis, None, vec![partie(50, "a"), partie(49, "b")], Some(curseur(49, "b")), t0 + s(1));
        assert_eq!(suite, Some((depuis, Some(curseur(49, "b")))), "la page suivante se demande");
        assert!(matches!(m.sous_la_main(true).vus(), MatchsVus::EnLecture), "rien ne se montre avant la fin");
        assert!(m.recevoir(depuis, None, vec![partie(1, "vieille")], None, t0 + s(1)).is_none(), "la première page ne revient pas");
        let fin = m.recevoir(depuis, Some(curseur(49, "b")), vec![partie(49, "b"), partie(48, "c"), partie(47, ""), partie(47, "")], None, t0 + s(2));
        assert!(fin.is_none() && !m.en_lecture());
        let sous_la_main = m.sous_la_main(true);
        let MatchsVus::DuServeur(parties) = sous_la_main.vus() else { panic!() };
        let ids: Vec<&str> = parties.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c", "", ""], "tout, une fois ; deux matchs sans identifiant restent deux");

        // Rien ne se redemande tant que rien n'a vieilli.
        assert!(m.a_demander(maintenant, t0 + s(60)).is_none());
        m.perimer();
        let (depuis, _) = m.a_demander(maintenant + 5, t0 + s(61)).expect("la relecture");
        assert!(matches!(m.sous_la_main(true).vus(), MatchsVus::DuServeur(p) if p.len() == 5), "l'ancienne lecture reste à l'écran");
        // Le serveur ne répond pas : abandon, on garde ce qu'on avait, et
        // rien ne repart tout seul.
        assert!(m.a_demander(maintenant, t0 + s(61) + REPONSE_MAX).is_none());
        assert!(!m.en_lecture());
        assert!(m.a_demander(maintenant, t0 + s(120)).is_none());
        assert!(m.recevoir(depuis, None, Vec::new(), None, t0 + s(121)).is_none(), "une réponse trop tardive est ignorée");
        assert!(matches!(m.sous_la_main(true).vus(), MatchsVus::DuServeur(p) if p.len() == 5));

        // Jamais de réponse à la toute première lecture : le résumé des
        // fiches, jusqu'à ce qu'on redemande.
        let mut m = MatchsGroupe::default();
        m.a_demander(maintenant, t0);
        m.a_demander(maintenant, t0 + REPONSE_MAX);
        assert!(matches!(m.sous_la_main(true).vus(), MatchsVus::DesFiches));
        m.perimer();
        assert!(m.a_demander(maintenant, t0 + s(30)).is_some(), "« Actualiser » relance");
        // Une reconnexion pendant la lecture : elle est perdue, et repart.
        m.reconnexion();
        assert!(!m.en_lecture());
        assert!(m.a_demander(maintenant, t0 + s(31)).is_some());
        // Un serveur qui dit toujours « la suite » : la lecture finit quand même.
        let mut m = MatchsGroupe::default();
        let (depuis, mut apres) = m.a_demander(maintenant, t0).unwrap();
        let mut pages = 0u64;
        while let Some((_, suivante)) = m.recevoir(depuis, apres.clone(), vec![partie(1_000 - pages, "")], Some(curseur(1_000 - pages, "")), t0) {
            apres = suivante;
            pages += 1;
            assert!(pages < 1_000);
        }
        assert_eq!(pages + 1, u64::from(MATCHS_PAGES_MAX));
        assert!(matches!(m.sous_la_main(true).vus(), MatchsVus::DuServeur(_)));
    }

    /// Le suivi des fiches se dit une fois par changement : à l'ouverture
    /// de la page, à sa fermeture, et de nouveau après une reconnexion —
    /// le serveur n'en garde rien d'une connexion à l'autre. Jamais à un
    /// serveur qui ne l'a pas promis.
    #[test]
    fn le_suivi_se_dit_une_fois_par_changement() {
        let mut d = DonneesValo::default();
        assert_eq!(d.suivi_a_dire(true), None, "serveur d'avant : rien ne lui part");
        d.suivi_a_la_connexion(true);
        assert_eq!(d.suivi_a_dire(false), None, "la page n'était pas montrée");
        assert_eq!(d.suivi_a_dire(true), Some(true));
        assert_eq!(d.suivi_a_dire(true), None, "déjà dit");
        assert_eq!(d.suivi_a_dire(false), Some(false));
        assert_eq!(d.suivi_a_dire(false), None);
        // La page ouverte pendant une reconnexion : à redire.
        assert_eq!(d.suivi_a_dire(true), Some(true));
        d.suivi_a_la_connexion(true);
        assert_eq!(d.suivi_a_dire(true), Some(true), "la connexion neuve ne le savait pas");
        d.suivi_a_la_connexion(false);
        assert_eq!(d.suivi_a_dire(true), None, "ce serveur-ci ne prévient pas");
    }

    #[test]
    fn une_demande_sans_reponse_finit_en_retard() {
        assert_eq!(Chargement::depuis(None), Chargement::Repos);
        assert_eq!(Chargement::depuis(Some(Duration::from_millis(200))), Chargement::EnCours);
        assert_eq!(Chargement::depuis(Some(REPONSE_MAX - Duration::from_millis(1))), Chargement::EnCours);
        assert_eq!(Chargement::depuis(Some(REPONSE_MAX)), Chargement::EnRetard);
        assert_eq!(Chargement::depuis(Some(Duration::from_secs(120))), Chargement::EnRetard);
    }
}
