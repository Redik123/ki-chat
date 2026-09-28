//! Les bornes des écoutes HTTPS : qui a le droit de faire lire un corps de
//! requête au serveur, combien de connexions une adresse peut tenir ouvertes,
//! et combien de temps une connexion a pour dire ce qu'elle veut.
//!
//! # Authentifier avant de lire
//!
//! Un gestionnaire qui prend `body: Bytes` (ou `Json<…>`) fait lire **tout**
//! le corps à axum avant de s'exécuter — donc avant de regarder le jeton.
//! Sans compte, des envois concurrents de 25 Mo faisaient ainsi monter la
//! mémoire du serveur jusqu'à ce que le système le tue. Les extracteurs de
//! [`Session`] et de [`PlaceEnvoi`] travaillent sur les seuls en-têtes, et
//! axum les exécute **avant** l'extracteur du corps : un refus ici, et le
//! corps n'est jamais lu.
//!
//! # Tenir la porte
//!
//! axum-server construit hyper sans horloge : sans elle, hyper n'applique
//! aucun délai de lecture des en-têtes, et une connexion qui les envoie au
//! compte-gouttes reste ouverte indéfiniment. [`borner`] lui en donne une ;
//! [`Limiteur`] plafonne en plus le nombre de connexions, au total et par
//! adresse, **avant** même la poignée de main TLS.

use std::collections::HashMap;
use std::io;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;

use ki_protocol::UserId;

use crate::state::AppState;

/// Le membre d'une requête HTTP, reconnu à son jeton de session
/// (`x-ki-token`, en hexadécimal) — sans rien lire du corps.
pub struct Session {
    pub user_id: UserId,
    pub username: String,
}

impl FromRequestParts<Arc<AppState>> for Session {
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get("x-ki-token")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| u64::from_str_radix(s, 16).ok());
        match token.and_then(|t| state.user_by_voice_token(t)) {
            Some((user_id, username)) => Ok(Session { user_id, username }),
            None => Err((StatusCode::UNAUTHORIZED, "jeton invalide")),
        }
    }
}

/// Envois de fichiers lus en même temps, tout le serveur confondu. Chacun
/// tient son corps en mémoire — jusqu'à 25 Mo pour `/upload`, 8 Mo pour un
/// morceau : huit à la fois, c'est au pire 200 Mo, et jamais davantage.
pub const ENVOIS_SIMULTANES: usize = 8;

/// Une place parmi les envois en cours, prise **avant** de lire le corps et
/// rendue avec la requête. Plus de place : 503, et le client réessaie.
pub struct PlaceEnvoi {
    _permis: tokio::sync::OwnedSemaphorePermit,
}

impl FromRequestParts<Arc<AppState>> for PlaceEnvoi {
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(
        _parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        match state.envois_http.clone().try_acquire_owned() {
            Ok(permis) => Ok(PlaceEnvoi { _permis: permis }),
            Err(_) => Err((
                StatusCode::SERVICE_UNAVAILABLE,
                "trop d'envois en cours sur le serveur — réessaie dans un instant",
            )),
        }
    }
}

/// Connexions HTTPS ouvertes au plus, toutes adresses confondues.
const CONNEXIONS_MAX: usize = 1024;
/// Et d'une même adresse. Large : trente joueurs derrière une même box, un
/// navigateur par porte web et les clients ki-chat qui gardent leurs
/// connexions ouvertes. Il ne vise que les milliers de connexions lentes.
const CONNEXIONS_PAR_ADRESSE: usize = 128;
/// Le temps laissé à une connexion pour envoyer les en-têtes d'une requête —
/// y compris la suivante, sur une connexion gardée ouverte.
const DELAI_EN_TETES: Duration = Duration::from_secs(20);

#[derive(Default)]
struct Compte {
    total: usize,
    par_adresse: HashMap<IpAddr, usize>,
}

/// Le plafond des connexions, posé sous TLS : une connexion refusée ne coûte
/// ni poignée de main ni tampon. Partagé par les deux écoutes.
#[derive(Clone, Default)]
pub struct Limiteur {
    compte: Arc<Mutex<Compte>>,
}

impl Limiteur {
    fn prendre(&self, ip: IpAddr) -> Option<Place> {
        let mut compte = self.compte.lock().unwrap();
        let de_l_adresse = compte.par_adresse.get(&ip).copied().unwrap_or(0);
        if compte.total >= CONNEXIONS_MAX || de_l_adresse >= CONNEXIONS_PAR_ADRESSE {
            return None;
        }
        compte.total += 1;
        compte.par_adresse.insert(ip, de_l_adresse + 1);
        Some(Place {
            compte: self.compte.clone(),
            ip,
        })
    }
}

/// Une connexion comptée : rendue quand la connexion se ferme.
pub struct Place {
    compte: Arc<Mutex<Compte>>,
    ip: IpAddr,
}

impl Drop for Place {
    fn drop(&mut self) {
        let mut compte = self.compte.lock().unwrap();
        compte.total = compte.total.saturating_sub(1);
        if let Some(n) = compte.par_adresse.get_mut(&self.ip) {
            *n = n.saturating_sub(1);
            // L'adresse disparaît avec sa dernière connexion : la table ne
            // garde pas une ligne par visiteur passé.
            if *n == 0 {
                compte.par_adresse.remove(&self.ip);
            }
        }
    }
}

/// Le flux d'une connexion admise, avec sa place : tout est délégué.
pub struct AvecPlace {
    flux: TcpStream,
    _place: Place,
}

impl AsyncRead for AvecPlace {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.flux).poll_read(cx, buf)
    }
}

impl AsyncWrite for AvecPlace {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.flux).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.flux).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.flux).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.flux).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.flux.is_write_vectored()
    }
}

impl<S> axum_server::accept::Accept<TcpStream, S> for Limiteur {
    type Stream = AvecPlace;
    type Service = S;
    type Future = std::future::Ready<io::Result<(AvecPlace, S)>>;

    fn accept(&self, flux: TcpStream, service: S) -> Self::Future {
        let ip = match flux.peer_addr() {
            Ok(adresse) => adresse.ip(),
            Err(e) => return std::future::ready(Err(e)),
        };
        std::future::ready(match self.prendre(ip) {
            Some(place) => Ok((AvecPlace { flux, _place: place }, service)),
            None => {
                tracing::debug!("connexion HTTPS refusée : trop de connexions ({ip})");
                Err(io::Error::other("trop de connexions"))
            }
        })
    }
}

/// Donne une horloge à hyper, donc un délai de lecture des en-têtes
/// ([`DELAI_EN_TETES`]) qui vaut aussi pour l'attente de la requête
/// suivante sur une connexion gardée ouverte. L'horloge **avant** le délai :
/// hyper panique sur un délai sans horloge.
pub fn borner(builder: &mut hyper_util::server::conn::auto::Builder<hyper_util::rt::TokioExecutor>) {
    builder
        .http1()
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(DELAI_EN_TETES);
    builder.http2().timer(hyper_util::rt::TokioTimer::new());
}

#[cfg(test)]
mod tests {
    use super::*;

    const IP: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 7));

    /// Une adresse ne tient pas plus de connexions que son plafond, et une
    /// connexion fermée rend sa place.
    #[test]
    fn une_adresse_est_plafonnee_et_recupere_ses_places() {
        let limiteur = Limiteur::default();
        let places: Vec<Place> = (0..CONNEXIONS_PAR_ADRESSE)
            .map(|_| limiteur.prendre(IP).expect("sous le plafond"))
            .collect();
        assert!(limiteur.prendre(IP).is_none());
        let autre = IpAddr::V4(std::net::Ipv4Addr::new(198, 51, 100, 9));
        assert!(limiteur.prendre(autre).is_some(), "une autre adresse passe");
        drop(places);
        assert!(limiteur.prendre(IP).is_some());
        // Rien ne traîne dans la table une fois tout rendu.
        assert!(limiteur.compte.lock().unwrap().par_adresse.is_empty());
    }
}
