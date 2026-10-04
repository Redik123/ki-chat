//! ki-core : le cœur du client ki-chat, sans interface.
//!
//! - [`net`] : la connexion au serveur (QUIC) et le moteur voix qu'elle
//!   alimente, pilotés depuis un fil à part.

pub mod net;
