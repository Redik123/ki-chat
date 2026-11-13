//! Les données VALORANT du client : ce que le serveur lui a rendu (les
//! fiches du groupe, la soirée, sa propre fiche, son journal, ses comptes
//! Riot), ce qu'on lui a déjà demandé ou dit, et ce qu'il sait faire.
//!
//! À part de la page (`valo`, ce qu'on regarde) et du veilleur
//! (`valorant`, ce que lit le client Riot de cette machine).

use std::time::{Duration, Instant};

use ki_protocol::{FicheMembre, FicheValorant, JeuEtat, JourDeJeu, MatchEsport, MonCompte, PartieSoiree};

use crate::FicheOuverte;

/// Le temps laissé au serveur pour rendre les fiches du groupe. Il les sert
/// de son cache, en quelques millisecondes : dix secondes sans rien, c'est
/// une demande perdue.
pub(crate) const REPONSE_MAX: Duration = Duration::from_secs(10);

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
    pub(crate) stats: Vec<FicheMembre>,
    pub(crate) stats_recu: bool,
    /// La demande en vol : quand elle est partie. Rendue à `None` par la
    /// réponse — ou par l'abandon, passé [`REPONSE_MAX`].
    pub(crate) stats_demandees: Option<Instant>,
    /// Les matchs d'e-sport à venir, pour l'accueil de la page.
    pub(crate) esports: Vec<MatchEsport>,
    /// Les 168 cases jour × heure (UTC) des parties du groupe sur 30 jours.
    pub(crate) activite: Vec<u16>,
    /// Le fil de la soirée (`ServerMsg::Soiree`), sa borne, et la dernière
    /// demande : il se relit toutes les deux minutes tant que l'accueil se
    /// montre.
    pub(crate) soiree: Vec<PartieSoiree>,
    pub(crate) soiree_depuis: u64,
    pub(crate) soiree_demandee: Option<Instant>,
    /// Ma propre fiche, entière : « Moi » sur la carte de chaleur de
    /// l'accueil. Demandée une fois par connexion.
    pub(crate) ma_fiche: Option<FicheValorant>,
    pub(crate) ma_fiche_demandee: bool,
    /// Mon journal des parties, tel que le serveur l'a relu, et la
    /// dernière demande.
    pub(crate) mon_journal: Option<Vec<JourDeJeu>>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn une_demande_sans_reponse_finit_en_retard() {
        assert_eq!(Chargement::depuis(None), Chargement::Repos);
        assert_eq!(Chargement::depuis(Some(Duration::from_millis(200))), Chargement::EnCours);
        assert_eq!(Chargement::depuis(Some(REPONSE_MAX - Duration::from_millis(1))), Chargement::EnCours);
        assert_eq!(Chargement::depuis(Some(REPONSE_MAX)), Chargement::EnRetard);
        assert_eq!(Chargement::depuis(Some(Duration::from_secs(120))), Chargement::EnRetard);
    }
}
