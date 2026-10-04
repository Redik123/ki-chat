//! ki-core : le cœur du client ki-chat, sans interface.
//!
//! - [`net`] : la connexion au serveur (QUIC) et le moteur voix qu'elle
//!   alimente, pilotés depuis un fil à part ;
//! - [`etat`] : l'état du client (salons, membres, fil, non-lus, vocal),
//!   tenu à jour d'après les messages du serveur ;
//! - [`markup`] : la mise en forme des messages et la détection des
//!   mentions ;
//! - [`apparence`] : les couleurs des pseudos et des rangs, celles du PC.

pub mod apparence;
pub mod etat;
pub mod markup;
pub mod net;
