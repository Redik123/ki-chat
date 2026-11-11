//! Ce que l'on écrit ne se perd pas.
//!
//! Deux façons de perdre un texte existaient, toutes deux silencieuses :
//!
//! - **changer de salon** en pleine rédaction. Le champ était unique : le
//!   texte suivait dans le salon d'à côté, et une modification en cours
//!   partait vers un salon où son message n'existe pas — le serveur la
//!   refusait, le texte était déjà effacé ;
//! - **un refus du serveur**. Le champ se vide à l'envoi, avant toute
//!   réponse : cadence, message trop long, modification refusée, et le texte
//!   n'était plus nulle part.
//!
//! D'où un brouillon par salon ([`Brouillons`]) et le dernier envoi gardé
//! jusqu'à ce que le serveur le renvoie ([`Envoi`]).

use ki_protocol::{ChannelId, MsgRef, ReplyRef};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Ce qui attend dans la zone de saisie d'un salon : le texte, et ce à quoi
/// il se rapporte.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Brouillon {
    pub texte: String,
    pub reponse_a: Option<ReplyRef>,
    pub edition: Option<MsgRef>,
}

impl Brouillon {
    /// Rien à retenir : pas la peine d'en garder la trace.
    pub fn est_vide(&self) -> bool {
        self.texte.trim().is_empty() && self.reponse_a.is_none() && self.edition.is_none()
    }
}

/// Les brouillons des salons qu'on ne regarde pas. Celui du salon courant
/// vit dans la zone de saisie elle-même.
#[derive(Default)]
pub struct Brouillons {
    /// Le serveur auquel ils appartiennent : un numéro de salon ne veut rien
    /// dire ailleurs, et un brouillon ne doit jamais ressortir chez un autre.
    serveur: String,
    par_salon: HashMap<ChannelId, Brouillon>,
}

impl Brouillons {
    /// On vient d'entrer sur ce serveur. Le même qu'avant — une reprise après
    /// coupure — garde ses brouillons ; un autre les fait oublier.
    pub fn serveur(&mut self, adresse: &str) {
        if self.serveur != adresse {
            self.serveur = adresse.to_string();
            self.par_salon.clear();
        }
    }

    /// On quitte ce salon : ce qui s'y écrivait attend qu'on y revienne.
    pub fn ranger(&mut self, salon: ChannelId, brouillon: Brouillon) {
        if brouillon.est_vide() {
            self.par_salon.remove(&salon);
        } else {
            self.par_salon.insert(salon, brouillon);
        }
    }

    /// On entre dans ce salon : ce qu'on y avait laissé, ou rien.
    pub fn reprendre(&mut self, salon: ChannelId) -> Brouillon {
        self.par_salon.remove(&salon).unwrap_or_default()
    }

    /// Les salons qui n'existent plus n'ont plus de brouillon.
    pub fn garder_seulement(&mut self, existe: impl Fn(ChannelId) -> bool) {
        self.par_salon.retain(|salon, _| existe(*salon));
    }

    #[cfg(test)]
    fn nombre(&self) -> usize {
        self.par_salon.len()
    }
}

/// Une erreur du serveur n'est pas rattachée à la demande qui l'a provoquée.
/// Dans les trois secondes qui suivent un envoi resté sans écho, on la tient
/// pour sa réponse.
pub const FENETRE_REFUS: Duration = Duration::from_secs(3);

/// Une session qui tombe dans les vingt secondes d'un envoi resté sans écho :
/// le message est sans doute resté dans le tampon de la connexion morte.
pub const FENETRE_COUPURE: Duration = Duration::from_secs(20);

/// Le dernier envoi, gardé jusqu'à ce que le serveur le renvoie.
#[derive(Debug, Clone)]
pub struct Envoi {
    /// Tel que tapé : ce qu'on remet dans le champ.
    pub brut: String,
    /// Tel que le serveur le nettoiera : ce à quoi on reconnaît son écho.
    pub propre: String,
    pub quand: Instant,
    pub salon: Option<ChannelId>,
    pub reponse_a: Option<ReplyRef>,
    /// Le message modifié, si c'était une modification.
    pub edition: Option<MsgRef>,
    /// Déjà remis dans le champ après une erreur. Si l'écho arrive quand
    /// même — l'erreur parlait d'autre chose —, le champ se revide.
    pub rendu: bool,
}

impl Envoi {
    /// Un message neuf qui nous revient : c'est bien celui-là.
    pub fn est_l_echo_du_message(&self, texte: &str) -> bool {
        self.edition.is_none() && self.propre == texte
    }

    /// Une modification qui nous revient : c'est bien celle-là.
    pub fn est_l_echo_de_la_modification(&self, message: &MsgRef) -> bool {
        self.edition.as_ref() == Some(message)
    }

    /// Le serveur vient de dire une erreur : faut-il remettre ce texte dans
    /// le champ ? Oui s'il vient de partir, qu'on est toujours dans son
    /// salon, et que rien d'autre ne s'écrit déjà.
    pub fn a_rendre_apres_un_refus(&self, maintenant: Instant, salon: Option<ChannelId>, champ_libre: bool) -> bool {
        !self.rendu
            && maintenant.saturating_duration_since(self.quand) < FENETRE_REFUS
            && self.salon == salon
            && champ_libre
    }

    /// La session vient de tomber : faut-il remettre ce texte dans le champ ?
    /// Seulement un message neuf — une modification remise sans son message
    /// partirait comme un message de plus.
    pub fn a_rendre_apres_une_coupure(&self, maintenant: Instant, champ_libre: bool) -> bool {
        !self.rendu
            && self.edition.is_none()
            && maintenant.saturating_duration_since(self.quand) < FENETRE_COUPURE
            && champ_libre
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brouillon(texte: &str) -> Brouillon {
        Brouillon { texte: texte.into(), ..Default::default() }
    }

    fn envoi(edition: Option<MsgRef>) -> Envoi {
        Envoi {
            brut: " salut ".into(),
            propre: "salut".into(),
            quand: Instant::now(),
            salon: Some(3),
            reponse_a: None,
            edition,
            rendu: false,
        }
    }

    #[test]
    fn chaque_salon_garde_ce_qu_on_y_ecrivait() {
        let mut b = Brouillons::default();
        b.serveur("quic://a");
        b.ranger(1, brouillon("à finir"));
        let edition = MsgRef { user_id: 7, ts: 42 };
        b.ranger(2, Brouillon { texte: "corrigé".into(), reponse_a: None, edition: Some(edition) });
        // Le salon 3 n'a rien : on y arrive les mains vides.
        assert!(b.reprendre(3).est_vide());
        // La modification revient avec son message, dans son salon.
        let repris = b.reprendre(2);
        assert_eq!((repris.texte.as_str(), repris.edition), ("corrigé", Some(edition)));
        assert_eq!(b.reprendre(1).texte, "à finir");
        // Repris, c'est la zone de saisie qui le porte : rien ne reste rangé.
        assert_eq!(b.nombre(), 0);
    }

    #[test]
    fn un_brouillon_vide_ne_laisse_pas_de_trace() {
        let mut b = Brouillons::default();
        b.ranger(1, brouillon("quelque chose"));
        // Revenu, effacé, reparti : plus rien à retenir.
        b.ranger(1, brouillon("   "));
        assert_eq!(b.nombre(), 0);
        // Une réponse sans texte en est un : on y répondait.
        let reponse = ReplyRef { user_id: 1, ts: 2, username: "max".into(), excerpt: "…".into() };
        b.ranger(1, Brouillon { texte: String::new(), reponse_a: Some(reponse), edition: None });
        assert_eq!(b.nombre(), 1);
    }

    #[test]
    fn les_brouillons_ne_passent_pas_d_un_serveur_a_l_autre() {
        let mut b = Brouillons::default();
        b.serveur("quic://a");
        b.ranger(1, brouillon("pour le serveur a"));
        // Une reprise sur le même serveur ne perd rien.
        b.serveur("quic://a");
        assert_eq!(b.nombre(), 1);
        // Un autre serveur a aussi un salon 1 : il ne doit rien y trouver.
        b.serveur("quic://b");
        assert!(b.reprendre(1).est_vide());
    }

    #[test]
    fn un_salon_supprime_emporte_son_brouillon() {
        let mut b = Brouillons::default();
        b.ranger(1, brouillon("un"));
        b.ranger(2, brouillon("deux"));
        b.garder_seulement(|salon| salon == 2);
        assert!(b.reprendre(1).est_vide());
        assert_eq!(b.reprendre(2).texte, "deux");
    }

    #[test]
    fn l_echo_se_reconnait_au_texte_nettoye_ou_au_message_modifie() {
        let message = envoi(None);
        assert!(message.est_l_echo_du_message("salut"));
        assert!(!message.est_l_echo_du_message("autre chose"));
        let cible = MsgRef { user_id: 7, ts: 42 };
        let modification = envoi(Some(cible));
        // Une modification n'est pas l'écho d'un message neuf, même texte.
        assert!(!modification.est_l_echo_du_message("salut"));
        assert!(modification.est_l_echo_de_la_modification(&cible));
        assert!(!modification.est_l_echo_de_la_modification(&MsgRef { user_id: 7, ts: 43 }));
    }

    #[test]
    fn un_refus_rend_le_texte_seulement_s_il_vient_de_partir() {
        let e = envoi(None);
        let t0 = e.quand;
        assert!(e.a_rendre_apres_un_refus(t0 + Duration::from_secs(1), Some(3), true));
        // Trop tard : l'erreur parle d'autre chose.
        assert!(!e.a_rendre_apres_un_refus(t0 + Duration::from_secs(4), Some(3), true));
        // On a changé de salon : son brouillon n'est pas le nôtre.
        assert!(!e.a_rendre_apres_un_refus(t0 + Duration::from_secs(1), Some(4), true));
        // On écrit déjà autre chose : on n'écrase rien.
        assert!(!e.a_rendre_apres_un_refus(t0 + Duration::from_secs(1), Some(3), false));
        // Déjà rendu : une seconde erreur ne le remet pas.
        let rendu = Envoi { rendu: true, ..e };
        assert!(!rendu.a_rendre_apres_un_refus(t0 + Duration::from_secs(1), Some(3), true));
    }

    #[test]
    fn une_coupure_rend_un_message_pas_une_modification() {
        let e = envoi(None);
        let t0 = e.quand;
        assert!(e.a_rendre_apres_une_coupure(t0 + Duration::from_secs(10), true));
        assert!(!e.a_rendre_apres_une_coupure(t0 + Duration::from_secs(21), true));
        assert!(!e.a_rendre_apres_une_coupure(t0 + Duration::from_secs(10), false));
        let modification = envoi(Some(MsgRef { user_id: 7, ts: 42 }));
        assert!(!modification.a_rendre_apres_une_coupure(t0 + Duration::from_secs(10), true));
    }
}
