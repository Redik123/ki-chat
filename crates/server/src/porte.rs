//! Les portes web : un lien vers un salon textuel temporaire, pour faire
//! entrer des inconnus sans compte ni installation.
//!
//! Un membre qui peut créer des invitations ouvre une porte (`salon1`) : le
//! serveur crée un salon textuel daté, et le lien `https://<serveur>/salon1`
//! (ou `/s/salon1`) mène à une page qui demande un nom. La page ouvre une
//! WebSocket, dit `hello`, et attend ; l'hôte de la porte — ou n'importe
//! quel connecté qui peut expulser — accepte ou refuse. Une fois entré,
//! l'invité lit et écrit dans ce salon-là, et **rien d'autre** : il ne reçoit
//! ni la liste des salons, ni celle des membres, ni une ligne d'un autre
//! salon. À la fermeture, le salon est effacé, pas archivé.
//!
//! # Pourquoi un pont, et pas un connecté de plus
//!
//! La table des connectés (`AppState::users`) exige une connexion QUIC : la
//! voix, la déconnexion, les flux, tout la lit. Y mettre un navigateur aurait
//! demandé de rendre tout cela optionnel. Les invités vivent donc ici, dans
//! une table à part, reliée au reste par trois points seulement : la fin de
//! `AppState::broadcast` (qui leur fait suivre la ligne du salon), le roster
//! (qui les liste, marqués `invite`), et `poster_membre` (qui écrit en leur
//! nom). Leurs identifiants sont pris dans une plage réservée
//! ([`ki_protocol::INVITE_ID_BASE`]) : aucun compte ne les porte.
//!
//! # Ce qui borne
//!
//! Cinq portes, vingt invités et cinq demandes par porte, une demande par
//! adresse à la fois, un limiteur par adresse sur les demandes — une
//! adresse refusée ou expulsée attend tout de suite son délai le plus
//! long —, un sas par adresse sur les WebSockets jusqu'à ce que la demande
//! soit posée, une origine vérifiée à la poignée de main, un budget
//! d'écriture plus serré que celui d'un membre, un budget sur les entrées
//! en vocal, une file d'envoi bornée (un invité qui ne lit plus est
//! déconnecté), chaque envoi borné dans le temps, un ping toutes les vingt
//! secondes et une fermeture après une minute de silence. Une porte sans
//! invité ferme au bout de dix minutes, et au plus tard deux heures après
//! son ouverture ; supprimer son salon, c'est la fermer. Tout est dans
//! l'audit.
//!
//! # La voix
//!
//! Un invité n'a ni QUIC ni la clé de session : sa voix passe par la même
//! WebSocket, en trames binaires, et c'est le serveur qui chiffre et
//! déchiffre à sa place. L'hôte de la porte (ou qui peut expulser) l'amène
//! dans le salon vocal où il se trouve lui-même ([`vocal`]) ; dès lors,
//! chaque trame Opus montante est emballée exactement comme un client le
//! ferait — en-tête voix à son identifiant, XChaCha20-Poly1305 sous la clé
//! du serveur, nonce dérivé de (identifiant, compteur) — et part vers les
//! pairs du salon par les routes voix ordinaires ; les membres l'entendent
//! sans rien savoir de la porte. Dans l'autre sens, le relais des
//! datagrammes ([`Portes::relayer`], appelé par `voice_task` et par le bot
//! musique) déchiffre pour lui ce que le salon reçoit, et le lui pousse en
//! clair, précédé de l'identifiant du locuteur pour qu'il mixe par voix. Il
//! n'entend jamais un autre salon : la table d'écoute est par salon, et
//! c'est le salon de l'émetteur qui la choisit.
//!
//! Être amené dans un vocal n'est qu'une autorisation : la page y entre
//! quand l'invité clique « Rejoindre » — elle dit alors `vocal actif:true`,
//! et `actif:false` quand il le quitte. C'est ce mot qui le met dans la
//! table d'écoute, dans le roster des membres et dans la liste des
//! occupants : un membre qui le voit dans le vocal sait qu'il y entend —
//! et, réciproquement, on n'entend que qui s'y voit : hors de ce mot, ses
//! trames tombent.
//!
//! Un invité ne reçoit jamais la liste des membres ; pour nommer qui parle,
//! le serveur lui dit qui occupe **son** salon vocal — membres, bot musique,
//! autres invités — à l'entrée (`porte_vocal`), puis à chaque changement
//! (`porte_vocal_occupants`, voir [`annoncer_occupants`]). Rien d'un autre
//! salon.
//!
//! Les identifiants d'invités sont espacés de [`INVITE_ID_PAS`] : la page
//! les lit en doubles JavaScript, où seuls ceux-là s'écrivent exactement.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use bytes::Bytes;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use futures_util::{SinkExt, StreamExt};
use ki_protocol::{
    parse_voice_packet, slug_valide, write_voice_header, ChannelId, ChannelKind, DemandeWeb,
    InviteWeb, Member, ServerMsg, TableauPorte, UserId, INVITE_ID_BASE, INVITE_ID_PAS, INVITE_SUFFIXE,
    MAX_USERNAME, PORTES_MAX, PORTE_DEMANDES_MAX, PORTE_INVITES_MAX, PORTE_SLUG_MAX,
    PORTE_SLUG_MIN, PORTE_TELECHARGEMENT, PORTE_TTL_MAX_SECS, PORTE_VIDE_SECS, VOICE_HEADER_LEN,
    VOICE_MAX_PACKET,
};
use rand::Rng;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use crate::state::{encode, now_millis, AppState, JetonSas, Line, TokenBucket};
use crate::throttle::Throttle;

/// La page, sa feuille et son script, embarqués : un binaire, rien à
/// déployer à côté. La feuille et le script sont servis à part
/// (`/s/porte.css`, `/s/porte.js`) : c'est ce qui permet une politique de
/// sécurité de contenu sans `unsafe-inline`. Un bloc `<script>` ou
/// `<style>` en ligne resterait accepté — il est reconnu à son empreinte —
/// mais un gestionnaire d'événement en attribut (`onclick=`) ne le serait
/// pas. `{{serveur}}` est remplacé au service par le nom du serveur.
const PAGE: &str = include_str!("porte.html");
const FEUILLE: &str = include_str!("porte.css");
const SCRIPT: &str = include_str!("porte.js");

/// Le premier message (`hello`) doit arriver dans ce délai, sinon la
/// WebSocket est fermée : un onglet ouvert « pour voir » ne coûte rien.
const HELLO_DELAI: Duration = Duration::from_secs(30);
/// Une demande sans réponse est retirée au bout de cinq minutes.
const DEMANDE_ATTENTE: Duration = Duration::from_secs(5 * 60);
/// Un ping serveur toutes les vingt secondes…
const PING_TOUTES: Duration = Duration::from_secs(20);
/// …et la fermeture après une minute sans rien recevoir — ni pong, ni
/// message. axum et tungstenite n'ont aucun délai d'inactivité par défaut,
/// contrairement à QUIC.
const SILENCE_MAX: Duration = Duration::from_secs(60);
/// Taille maximale d'une trame reçue d'un invité : de quoi porter un
/// message de `MAX_CHAT_TEXT` caractères en UTF-8, pas plus.
const TRAME_MAX: usize = 16 * 1024;
/// Profondeur de la file d'envoi vers un invité. Bien moins que celle d'un
/// membre : il ne reçoit que les messages d'un salon, jamais un roster ni
/// une photo.
const FILE_INVITE: usize = 128;
/// Le pseudo sous lequel le serveur parle dans un salon de porte.
const PSEUDO_PORTE: &str = "Porte";
/// L'invitation offerte à un invité vaut sept jours.
const INVITATION_TTL_SECS: u64 = 7 * 24 * 60 * 60;
/// L'historique servi à l'entrée.
const HISTORIQUE_A_L_ENTREE: usize = 50;

/// La version des trames binaires échangées avec la page. Montante :
/// `[version][compteur u64 LE][Opus]` ; descendante :
/// `[version][locuteur u64 LE][compteur u64 LE][Opus]` — l'identifiant du
/// locuteur sert à la page à mixer par voix et à montrer qui parle.
const VOCAL_VERSION: u8 = 1;
/// Une trame Opus de 20 ms à 48 kHz mono ne dépasse pas cela ; au-delà,
/// ce n'est pas du son.
const OPUS_MAX: usize = 400;
/// L'en-tête d'une trame montante : version et compteur.
const MONTANTE_EN_TETE: usize = 1 + 8;
/// L'en-tête d'une trame descendante : version, locuteur, compteur.
const DESCENDANTE_EN_TETE: usize = 1 + 8 + 8;
/// La file audio vers une page : un peu plus d'une seconde de trames. Une
/// page qui ne suit plus perd du son, pas sa session — la voix se rattrape,
/// un message non.
const FILE_AUDIO: usize = 64;

const PERMISSION_REFUSEE: &str = "tu n'as pas cette permission";

// ---------------------------------------------------------------------
// La table
// ---------------------------------------------------------------------

/// Les portes ouvertes, qui attend derrière et qui est entré.
///
/// Un seul verrou, jamais tenu pendant qu'on appelle le reste du serveur :
/// les méthodes rendent ce qu'il faut (files d'envoi, noms, salon) et c'est
/// l'appelant, verrou relâché, qui prévient qui doit l'être.
#[derive(Default)]
pub struct Portes {
    inner: Mutex<Inner>,
    /// Les demandes par adresse : cinq gratuites, puis un délai qui double.
    /// Le limiteur d'authentification, tel quel, avec l'adresse pour seule
    /// clé — celui du serveur est partagé avec les connexions QUIC, et un
    /// insistant à la porte ne doit pas ralentir les membres qui se
    /// connectent.
    throttle: Throttle,
    /// Qui écoute quel salon vocal : recalculée à chaque entrée ou sortie
    /// d'un invité, lue sous un verrou partagé à chaque datagramme voix du
    /// serveur — comme la table de routage des membres, et pour la même
    /// raison : sur le chemin chaud, une lecture, jamais `inner`.
    ecoutes: RwLock<HashMap<ChannelId, Vec<Ecoute>>>,
    /// Le chiffre de session, posé à la première trame à déchiffrer : la
    /// clé vit dans `AppState`, qui nous contient.
    chiffre: OnceLock<XChaCha20Poly1305>,
    /// Les occupants de chaque salon vocal écouté, tels qu'annoncés en
    /// dernier aux invités qui l'écoutent : on ne leur répète pas une liste
    /// qui n'a pas bougé. Jamais tenu avec un autre verrou de la table.
    derniers_occupants: Mutex<HashMap<ChannelId, Vec<Occupant>>>,
}

/// Quelqu'un dans un salon vocal, vu d'un invité : de quoi nommer qui
/// parle, rien de plus.
#[derive(Clone, PartialEq, Eq)]
struct Occupant {
    id: UserId,
    nom: String,
}

/// Un invité à l'écoute d'un salon vocal : à qui pousser le son en clair.
#[derive(Clone)]
struct Ecoute {
    invite_id: UserId,
    audio: mpsc::Sender<Bytes>,
}

#[derive(Default)]
struct Inner {
    portes: HashMap<String, Porte>,
    /// Qui est à quelle porte, demandes et invités confondus.
    ou: HashMap<UserId, String>,
    /// Rang du prochain invité dans la plage réservée.
    prochain_invite: u64,
    prochaine_demande: u64,
}

struct Porte {
    slug: String,
    salon: ChannelId,
    nom_salon: String,
    hote: UserId,
    hote_nom: String,
    /// Fermeture au plus tard (ms Unix).
    expire_le: u64,
    /// Depuis quand il n'y a plus d'invité : posé à l'ouverture, puis au
    /// départ du dernier. Dix minutes plus tard, la porte ferme.
    vide_depuis: Instant,
    demandes: Vec<Demande>,
    invites: Vec<Invite>,
}

struct Demande {
    id: u64,
    /// Réservé dès la demande : c'est l'identité de la session, de la
    /// première trame à la dernière.
    invite_id: UserId,
    /// Sans le suffixe : c'est ainsi que l'hôte le lit dans la bannière.
    nom: String,
    ip: IpAddr,
    /// L'hôte par lequel sa page nous a joints (l'en-tête `Host`, sans le
    /// port) : c'est l'adresse qu'il sait taper, celle qu'on lui redonne
    /// pour ki-chat quand l'admin n'en a pas fixé une.
    hote: String,
    depuis: u64,
    arrivee: Instant,
    tx: mpsc::Sender<Line>,
    /// Sa file audio, ouverte avec la session : elle ne sert qu'une fois
    /// entré et en vocal, mais c'est la session qui en tient l'autre bout.
    audio: mpsc::Sender<Bytes>,
}

struct Invite {
    invite_id: UserId,
    /// Avec le suffixe « (web) » : c'est ainsi qu'il signe.
    nom: String,
    ip: IpAddr,
    /// L'hôte par lequel sa page nous a joints — voir `Demande::hote`.
    hote: String,
    depuis: u64,
    /// Le salon vocal où on l'a amené, s'il y est autorisé.
    vocal: Option<ChannelId>,
    /// Sa page y est entrée (`vocal actif:true`) : on lui pousse le son, et
    /// les membres le voient dans le vocal. Retombe quand elle le quitte,
    /// et quand on l'en sort.
    ecoute: bool,
    /// `None` = sa file a saturé, ou sa page s'est fermée : la session se
    /// termine, et la tâche WebSocket viendra le retirer.
    tx: Option<mpsc::Sender<Line>>,
    audio: mpsc::Sender<Bytes>,
    /// Trois messages d'un coup, puis un toutes les trois secondes.
    budget: TokenBucket,
    /// Sa voix : cinquante trames de 20 ms par seconde, le double en
    /// rafale — le budget d'un membre.
    budget_voix: TokenBucket,
    /// Entrer dans le vocal : chaque entrée rediffuse le roster complet à
    /// tous les connectés. Quatre d'un coup — cliquer, se raviser,
    /// recliquer —, puis une par seconde : une page qui bascule en boucle
    /// ne fait plus rien bouger. Sortir n'est jamais compté — retenir
    /// quelqu'un dans un vocal qu'il a quitté n'aurait pas de sens, et
    /// deux sorties de suite ne changent rien.
    budget_vocal: TokenBucket,
    /// Le compteur de ses paquets voix part d'un tirage, comme chez un
    /// client : c'est un nonce, et la clé du serveur ne change qu'au
    /// redémarrage. La page ajoute le sien par-dessus, strictement
    /// croissant — un trou chez elle reste un trou chez qui l'écoute.
    base_compteur: u64,
    dernier_compteur: Option<u64>,
}

/// De quoi ouvrir une porte.
struct Ouverture {
    slug: String,
    salon: ChannelId,
    nom_salon: String,
    hote: UserId,
    hote_nom: String,
    expire_le: u64,
}

/// Ce qu'une réponse à une demande rend à qui doit prévenir l'intéressé.
struct Reponse {
    salon: ChannelId,
    nom_salon: String,
    /// Avec le suffixe si accepté, sans s'il est refusé.
    nom: String,
    ip: IpAddr,
    tx: mpsc::Sender<Line>,
}

/// Quelqu'un qui n'est plus à la porte : parti, expulsé, ou dont la demande
/// a expiré.
struct Depart {
    slug: String,
    salon: ChannelId,
    nom: String,
    ip: IpAddr,
    etait_invite: bool,
    tx: Option<mpsc::Sender<Line>>,
}

/// Une porte qui vient de fermer : à qui dire adieu, quel salon effacer.
struct Fermee {
    salon: ChannelId,
    hote: UserId,
    sorties: Vec<mpsc::Sender<Line>>,
    invites: usize,
}

/// Un invité présent, vu de qui agit sur lui.
struct Fiche {
    slug: String,
    salon: ChannelId,
    hote: UserId,
    nom: String,
    vocal: Option<ChannelId>,
    /// L'hôte par lequel sa page nous a joints.
    hote_public: String,
}

impl Portes {
    fn ouvrir(&self, o: Ouverture, now: Instant) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        if inner.portes.contains_key(&o.slug) {
            return Err("une porte de ce nom est déjà ouverte".into());
        }
        if inner.portes.len() >= PORTES_MAX {
            return Err(format!("{PORTES_MAX} portes ouvertes au plus — ferme-en une d'abord"));
        }
        inner.portes.insert(
            o.slug.clone(),
            Porte {
                slug: o.slug,
                salon: o.salon,
                nom_salon: o.nom_salon,
                hote: o.hote,
                hote_nom: o.hote_nom,
                expire_le: o.expire_le,
                vide_depuis: now,
                demandes: Vec::new(),
                invites: Vec::new(),
            },
        );
        Ok(())
    }

    pub fn existe(&self, slug: &str) -> bool {
        self.inner.lock().unwrap().portes.contains_key(slug)
    }

    pub fn nombre(&self) -> usize {
        self.inner.lock().unwrap().portes.len()
    }

    fn hote_de(&self, slug: &str) -> Option<UserId> {
        self.inner.lock().unwrap().portes.get(slug).map(|p| p.hote)
    }

    /// La porte dont c'est le salon, s'il y en a une : supprimer ce salon,
    /// c'est fermer cette porte.
    pub fn slug_du_salon(&self, salon: ChannelId) -> Option<String> {
        self.inner.lock().unwrap().portes.values().find(|p| p.salon == salon).map(|p| p.slug.clone())
    }

    /// Quelqu'un frappe. Rend `(demande_id, invite_id)`, ou pourquoi non —
    /// en français, pour la page.
    #[allow(clippy::too_many_arguments)]
    fn frapper(
        &self,
        slug: &str,
        nom: &str,
        ip: IpAddr,
        hote: String,
        tx: mpsc::Sender<Line>,
        audio: mpsc::Sender<Bytes>,
        now_ms: u64,
        now: Instant,
    ) -> Result<(u64, UserId), String> {
        let mut inner = self.inner.lock().unwrap();
        let Inner { portes, ou, prochain_invite, prochaine_demande } = &mut *inner;
        let Some(porte) = portes.get_mut(slug) else {
            return Err("pas de porte ouverte à ce nom".into());
        };
        if porte.demandes.iter().any(|d| d.ip == ip) {
            return Err("une demande à la fois par adresse — attends la réponse".into());
        }
        if porte.demandes.len() >= PORTE_DEMANDES_MAX {
            return Err("trop de demandes en attente à cette porte — réessaie dans un instant".into());
        }
        if porte.invites.len() >= PORTE_INVITES_MAX {
            return Err(format!("la porte est pleine ({PORTE_INVITES_MAX} invités au plus)"));
        }
        if porte.nom_pris(nom) {
            return Err("ce nom est déjà pris ici — choisis-en un autre".into());
        }
        let demande_id = *prochaine_demande + 1;
        *prochaine_demande = demande_id;
        // Espacés du pas : la page les lit en doubles, où l'unité se perd.
        let invite_id = INVITE_ID_BASE + *prochain_invite * INVITE_ID_PAS;
        *prochain_invite += 1;
        porte.demandes.push(Demande {
            id: demande_id,
            invite_id,
            nom: nom.to_string(),
            ip,
            hote,
            depuis: now_ms,
            arrivee: now,
            tx,
            audio,
        });
        ou.insert(invite_id, slug.to_string());
        Ok((demande_id, invite_id))
    }

    fn porte_de_demande(&self, demande_id: u64) -> Option<(String, UserId)> {
        let inner = self.inner.lock().unwrap();
        inner
            .portes
            .values()
            .find(|p| p.demandes.iter().any(|d| d.id == demande_id))
            .map(|p| (p.slug.clone(), p.hote))
    }

    /// Tranche une demande. Acceptée, elle devient un invité du salon ;
    /// refusée, elle disparaît. Dans les deux cas, la file de l'intéressé
    /// est rendue pour lui dire.
    fn repondre(&self, demande_id: u64, accepter: bool, now_ms: u64) -> Result<Reponse, String> {
        let mut inner = self.inner.lock().unwrap();
        let Inner { portes, ou, .. } = &mut *inner;
        let Some(porte) = portes.values_mut().find(|p| p.demandes.iter().any(|d| d.id == demande_id)) else {
            return Err("cette demande n'est plus là — quelqu'un a déjà répondu, ou elle a expiré".into());
        };
        if accepter && porte.invites.len() >= PORTE_INVITES_MAX {
            return Err(format!("la porte est pleine ({PORTE_INVITES_MAX} invités au plus)"));
        }
        let at = porte.demandes.iter().position(|d| d.id == demande_id).expect("trouvée à l'instant");
        let d = porte.demandes.remove(at);
        if !accepter {
            ou.remove(&d.invite_id);
            return Ok(Reponse {
                salon: porte.salon,
                nom_salon: porte.nom_salon.clone(),
                nom: d.nom,
                ip: d.ip,
                tx: d.tx,
            });
        }
        let nom = format!("{}{INVITE_SUFFIXE}", d.nom);
        porte.invites.push(Invite {
            invite_id: d.invite_id,
            nom: nom.clone(),
            ip: d.ip,
            hote: d.hote,
            depuis: now_ms,
            vocal: None,
            ecoute: false,
            tx: Some(d.tx.clone()),
            audio: d.audio,
            budget: TokenBucket::new(1.0 / 3.0, 3.0),
            budget_voix: TokenBucket::new(60.0, 120.0),
            budget_vocal: TokenBucket::new(1.0, 4.0),
            // Le bit de poids fort libre : de la marge pour compter sans
            // reboucler, comme le bot musique.
            base_compteur: rand::rng().random::<u64>() >> 1,
            dernier_compteur: None,
        });
        Ok(Reponse {
            salon: porte.salon,
            nom_salon: porte.nom_salon.clone(),
            nom,
            ip: d.ip,
            tx: d.tx,
        })
    }

    /// Un invité veut écrire : où, et sous quel nom — ou pourquoi non.
    fn ecrire(&self, invite_id: UserId) -> Result<(ChannelId, String), String> {
        let mut inner = self.inner.lock().unwrap();
        let Inner { portes, ou, .. } = &mut *inner;
        let Some(slug) = ou.get(&invite_id) else {
            return Err("tu n'es plus à la porte".into());
        };
        let Some(porte) = portes.get_mut(slug) else {
            return Err("la porte est fermée".into());
        };
        let Some(invite) = porte.invites.iter_mut().find(|i| i.invite_id == invite_id) else {
            return Err("attends d'être accepté".into());
        };
        if !invite.budget.take() {
            return Err("tu écris trop vite".into());
        }
        Ok((porte.salon, invite.nom.clone()))
    }

    /// Retire quelqu'un — demande en attente ou invité entré.
    fn retirer(&self, invite_id: UserId, now: Instant) -> Option<Depart> {
        let mut inner = self.inner.lock().unwrap();
        let Inner { portes, ou, .. } = &mut *inner;
        let slug = ou.remove(&invite_id)?;
        let porte = portes.get_mut(&slug)?;
        if let Some(at) = porte.invites.iter().position(|i| i.invite_id == invite_id) {
            let i = porte.invites.remove(at);
            if porte.invites.is_empty() {
                porte.vide_depuis = now;
            }
            let salon = porte.salon;
            if i.vocal.is_some() {
                self.recalculer_ecoutes(&inner);
            }
            return Some(Depart { slug, salon, nom: i.nom, ip: i.ip, etait_invite: true, tx: i.tx });
        }
        let at = porte.demandes.iter().position(|d| d.invite_id == invite_id)?;
        let d = porte.demandes.remove(at);
        Some(Depart { slug, salon: porte.salon, nom: d.nom, ip: d.ip, etait_invite: false, tx: Some(d.tx) })
    }

    fn retirer_demande(&self, demande_id: u64) -> Option<Depart> {
        let invite_id = {
            let inner = self.inner.lock().unwrap();
            inner
                .portes
                .values()
                .flat_map(|p| p.demandes.iter())
                .find(|d| d.id == demande_id)
                .map(|d| d.invite_id)?
        };
        self.retirer(invite_id, Instant::now())
    }

    fn fermer(&self, slug: &str) -> Option<Fermee> {
        let mut inner = self.inner.lock().unwrap();
        let Inner { portes, ou, .. } = &mut *inner;
        let porte = portes.remove(slug)?;
        for d in &porte.demandes {
            ou.remove(&d.invite_id);
        }
        for i in &porte.invites {
            ou.remove(&i.invite_id);
        }
        let invites = porte.invites.len();
        let en_vocal = porte.invites.iter().any(|i| i.vocal.is_some());
        let sorties = porte
            .invites
            .into_iter()
            .filter_map(|i| i.tx)
            .chain(porte.demandes.into_iter().map(|d| d.tx))
            .collect();
        let fermee = Fermee { salon: porte.salon, hote: porte.hote, sorties, invites };
        if en_vocal {
            self.recalculer_ecoutes(&inner);
        }
        Some(fermee)
    }

    /// Fait suivre une ligne du salon à ses invités. Une file pleine, c'est
    /// une page qui ne lit plus : sa file est lâchée, la session se termine,
    /// comme pour un membre — sans faire payer sa lenteur à la mémoire.
    pub fn diffuser(&self, salon: ChannelId, line: &Line) {
        let mut inner = self.inner.lock().unwrap();
        for porte in inner.portes.values_mut().filter(|p| p.salon == salon) {
            for invite in porte.invites.iter_mut() {
                let Some(tx) = &invite.tx else { continue };
                if let Err(e) = tx.try_send(line.clone()) {
                    if matches!(e, mpsc::error::TrySendError::Full(_)) {
                        tracing::warn!("file d'envoi de l'invité {} saturée : sa page ne suit plus", invite.nom);
                    }
                    invite.tx = None;
                }
            }
        }
    }

    /// Une ligne à **un** invité.
    fn envoyer(&self, invite_id: UserId, line: &Line) {
        let mut inner = self.inner.lock().unwrap();
        let Inner { portes, ou, .. } = &mut *inner;
        let Some(slug) = ou.get(&invite_id) else { return };
        let Some(porte) = portes.get_mut(slug) else { return };
        if let Some(invite) = porte.invites.iter_mut().find(|i| i.invite_id == invite_id) {
            if let Some(tx) = &invite.tx {
                if tx.try_send(line.clone()).is_err() {
                    invite.tx = None;
                }
            }
        }
    }

    fn fiche(&self, invite_id: UserId) -> Option<Fiche> {
        let inner = self.inner.lock().unwrap();
        let slug = inner.ou.get(&invite_id)?;
        let porte = inner.portes.get(slug)?;
        let invite = porte.invites.iter().find(|i| i.invite_id == invite_id)?;
        Some(Fiche {
            slug: slug.clone(),
            salon: porte.salon,
            hote: porte.hote,
            nom: invite.nom.clone(),
            vocal: invite.vocal,
            hote_public: invite.hote.clone(),
        })
    }

    // --- La voix ---

    /// La table d'écoute, recalculée depuis `inner` (que l'appelant tient) :
    /// pour chaque salon vocal, les invités dont la page y est entrée — une
    /// autorisation sans page derrière n'écoute rien. Le verrou d'écriture
    /// n'est pris que le temps de poser la nouvelle table — le relais, qui
    /// ne prend jamais `inner`, ne peut pas nous attendre en la tenant.
    fn recalculer_ecoutes(&self, inner: &Inner) {
        let mut table: HashMap<ChannelId, Vec<Ecoute>> = HashMap::new();
        for invite in inner.portes.values().flat_map(|p| p.invites.iter()) {
            if let Some(vocal) = invite.vocal.filter(|_| invite.ecoute) {
                table
                    .entry(vocal)
                    .or_default()
                    .push(Ecoute { invite_id: invite.invite_id, audio: invite.audio.clone() });
            }
        }
        *self.ecoutes.write().unwrap() = table;
    }

    /// Amène un invité dans un salon vocal. Rend le salon textuel de sa
    /// porte, son nom, et s'il y était déjà autorisé — auquel cas rien ne
    /// change, mais la page méritera qu'on le lui redise : un membre qui
    /// l'invite une seconde fois voit un invité qui n'a pas encore cliqué.
    fn entrer_vocal(&self, invite_id: UserId, vocal: ChannelId) -> Result<(ChannelId, String, bool), String> {
        let mut inner = self.inner.lock().unwrap();
        let Inner { portes, ou, .. } = &mut *inner;
        let Some(slug) = ou.get(&invite_id) else {
            return Err("invité introuvable — déjà parti ?".into());
        };
        let Some(porte) = portes.get_mut(slug) else {
            return Err("la porte est fermée".into());
        };
        let salon = porte.salon;
        let Some(invite) = porte.invites.iter_mut().find(|i| i.invite_id == invite_id) else {
            return Err("il n'est pas encore entré".into());
        };
        let nom = invite.nom.clone();
        if invite.vocal == Some(vocal) {
            return Ok((salon, nom, true));
        }
        invite.vocal = Some(vocal);
        // Un salon quitté pour un autre : la page peut repartir de zéro —
        // la nôtre ne le fait pas, mais rien ne l'y oblige — sans qu'un
        // nonce resserve jamais : la base avance au-delà du dernier compteur
        // consommé, et tout ce qu'elle enverra sera neuf. Et si sa page
        // était dans l'ancien, elle est dans le nouveau : elle suit le
        // déplacement sans rien redire.
        let consommes = invite.dernier_compteur.map_or(0, |d| d.wrapping_add(1));
        invite.base_compteur = invite.base_compteur.wrapping_add(consommes);
        invite.dernier_compteur = None;
        self.recalculer_ecoutes(&inner);
        Ok((salon, nom, false))
    }

    /// La page d'un invité entre dans le vocal où on l'a autorisé, ou le
    /// quitte. `None` s'il n'est pas invité ou pas autorisé — sa page se
    /// croit en vocal à tort, après une coupure par exemple, et doit
    /// l'apprendre. Sinon, si quelque chose a changé — et une entrée de
    /// trop, budget épuisé, ne change rien : chaque entrée coûte un roster
    /// complet à tous les connectés, une page ne les fait pas pleuvoir.
    fn ecouter(&self, invite_id: UserId, actif: bool) -> Option<bool> {
        let mut inner = self.inner.lock().unwrap();
        let Inner { portes, ou, .. } = &mut *inner;
        let autorise = ou
            .get(&invite_id)
            .and_then(|slug| portes.get_mut(slug))
            .and_then(|p| p.invites.iter_mut().find(|i| i.invite_id == invite_id))
            .filter(|i| i.vocal.is_some());
        let Some(invite) = autorise else {
            // Quitter un vocal où l'on n'est pas ne trompe personne.
            return if actif { None } else { Some(false) };
        };
        if invite.ecoute == actif {
            return Some(false);
        }
        if actif && !invite.budget_vocal.take() {
            return Some(false);
        }
        invite.ecoute = actif;
        self.recalculer_ecoutes(&inner);
        Some(true)
    }

    /// Sort un invité du vocal. `None` s'il n'y était pas.
    fn sortir_vocal(&self, invite_id: UserId) -> Option<(ChannelId, String, ChannelId)> {
        let mut inner = self.inner.lock().unwrap();
        let Inner { portes, ou, .. } = &mut *inner;
        let slug = ou.get(&invite_id)?;
        let porte = portes.get_mut(slug)?;
        let salon = porte.salon;
        let invite = porte.invites.iter_mut().find(|i| i.invite_id == invite_id)?;
        let ancien = invite.vocal.take()?;
        invite.ecoute = false;
        let nom = invite.nom.clone();
        self.recalculer_ecoutes(&inner);
        Some((salon, nom, ancien))
    }

    /// Les invités en vocal dans un salon qui n'existe plus : sortis, et
    /// rendus pour qu'on le leur dise.
    fn vocaux_perdus(&self, existe: impl Fn(ChannelId) -> bool) -> Vec<(UserId, String, ChannelId)> {
        let mut inner = self.inner.lock().unwrap();
        let mut perdus = Vec::new();
        for porte in inner.portes.values_mut() {
            for invite in porte.invites.iter_mut() {
                if let Some(vocal) = invite.vocal {
                    if !existe(vocal) {
                        invite.vocal = None;
                        invite.ecoute = false;
                        perdus.push((invite.invite_id, invite.nom.clone(), porte.salon));
                    }
                }
            }
        }
        if !perdus.is_empty() {
            self.recalculer_ecoutes(&inner);
        }
        perdus
    }

    /// Un invité envoie une trame : dans quel salon vocal, et sous quel
    /// compteur — ou pourquoi non. Il faut que sa page soit entrée dans le
    /// vocal (`ecoute`), pas seulement qu'on l'y ait autorisé : ce que les
    /// membres entendent vient toujours de quelqu'un qu'ils voient dans le
    /// salon. Le compteur de la page doit croître strictement : c'est ce
    /// qui garantit qu'un nonce ne resserve jamais.
    fn emettre(&self, invite_id: UserId, compteur_page: u64) -> Result<(ChannelId, u64), &'static str> {
        let mut inner = self.inner.lock().unwrap();
        let Inner { portes, ou, .. } = &mut *inner;
        let Some(slug) = ou.get(&invite_id) else { return Err("plus à la porte") };
        let Some(porte) = portes.get_mut(slug) else { return Err("porte fermée") };
        let Some(invite) = porte.invites.iter_mut().find(|i| i.invite_id == invite_id) else {
            return Err("pas entré");
        };
        let Some(vocal) = invite.vocal.filter(|_| invite.ecoute) else { return Err("pas en vocal") };
        if invite.dernier_compteur.is_some_and(|d| compteur_page <= d) {
            return Err("compteur qui recule");
        }
        if !invite.budget_voix.take() {
            return Err("trop de trames");
        }
        invite.dernier_compteur = Some(compteur_page);
        Ok((vocal, invite.base_compteur.wrapping_add(compteur_page)))
    }

    /// Quelqu'un écoute-t-il ce salon ? Une lecture partagée, rien d'autre :
    /// c'est la question que pose chaque datagramme voix du serveur.
    pub fn ecoute(&self, salon: ChannelId) -> bool {
        self.ecoutes.read().unwrap().contains_key(&salon)
    }

    /// Les salons vocaux écoutés, et qui les écoute — copiés, pour n'appeler
    /// personne en tenant la table.
    fn ecoutes_par_salon(&self) -> Vec<(ChannelId, Vec<UserId>)> {
        self.ecoutes
            .read()
            .unwrap()
            .iter()
            .map(|(salon, liste)| (*salon, liste.iter().map(|e| e.invite_id).collect()))
            .collect()
    }

    /// Les invités dont la page est dans ce salon vocal, toutes portes
    /// confondues.
    fn invites_en_vocal(&self, vocal: ChannelId) -> Vec<Occupant> {
        let inner = self.inner.lock().unwrap();
        inner
            .portes
            .values()
            .flat_map(|p| p.invites.iter())
            .filter(|i| i.ecoute && i.vocal == Some(vocal))
            .map(|i| Occupant { id: i.invite_id, nom: i.nom.clone() })
            .collect()
    }

    /// Un paquet voix chiffré reçu dans un salon — d'un membre ou du bot —
    /// déchiffré et poussé en clair aux invités qui écoutent ce salon-là.
    /// Sans invité à l'écoute, rien n'est déchiffré. Un paquet qui ne se
    /// déchiffre pas (forgé, altéré, d'une autre clé) est ignoré : la page
    /// n'en saura rien, pas plus qu'un client.
    pub fn relayer(&self, salon: ChannelId, cle: &[u8; 32], paquet: &[u8]) {
        if !self.ecoute(salon) || paquet.len() > VOICE_MAX_PACKET {
            return;
        }
        let Some(pkt) = parse_voice_packet(paquet) else { return };
        if pkt.payload.is_empty() {
            return;
        }
        let chiffre = self.chiffre.get_or_init(|| XChaCha20Poly1305::new(cle.into()));
        let Ok(opus) = chiffre.decrypt(&nonce_voix(pkt.id, pkt.counter), pkt.payload) else {
            return;
        };
        self.pousser_audio(salon, pkt.id, pkt.counter, &opus, None);
    }

    /// Une trame en clair aux invités qui écoutent `salon`, sauf `except`
    /// — l'invité qui vient de la dire ne s'entend pas lui-même. Une file
    /// pleine perd la trame, pas la session : c'est du son.
    fn pousser_audio(&self, salon: ChannelId, locuteur: UserId, compteur: u64, opus: &[u8], except: Option<UserId>) {
        let ecoutes = self.ecoutes.read().unwrap();
        let Some(liste) = ecoutes.get(&salon) else { return };
        let trame = trame_descendante(locuteur, compteur, opus);
        for e in liste {
            if Some(e.invite_id) == except {
                continue;
            }
            let _ = e.audio.try_send(trame.clone());
        }
    }

    /// L'état d'une porte, et son hôte.
    fn etat(&self, slug: &str) -> Option<(ServerMsg, UserId)> {
        let inner = self.inner.lock().unwrap();
        let porte = inner.portes.get(slug)?;
        Some((
            ServerMsg::PorteEtat {
                slug: porte.slug.clone(),
                salon: porte.salon,
                invites: porte
                    .invites
                    .iter()
                    .map(|i| InviteWeb {
                        invite_id: i.invite_id,
                        nom: i.nom.clone(),
                        depuis: i.depuis,
                        // Là où sa page est, pas là où on l'a autorisé : un
                        // membre qui le voit en vocal sait qu'il y entend.
                        vocal: i.vocal.filter(|_| i.ecoute),
                    })
                    .collect(),
                demandes: porte
                    .demandes
                    .iter()
                    .map(|d| DemandeWeb { demande_id: d.id, nom: d.nom.clone(), depuis: d.depuis })
                    .collect(),
                expire_le: porte.expire_le,
            },
            porte.hote,
        ))
    }

    /// Les invités, comme la liste des membres les montre : en ligne,
    /// marqués, sans rôle ni photo.
    pub fn membres(&self) -> Vec<Member> {
        let inner = self.inner.lock().unwrap();
        inner
            .portes
            .values()
            .flat_map(|p| p.invites.iter())
            .map(|i| Member {
                user_id: i.invite_id,
                username: i.nom.clone(),
                speaking: false,
                muted: false,
                streaming: None,
                force_muted: false,
                force_deafened: false,
                admin: false,
                avatar: None,
                voice: i.vocal.filter(|_| i.ecoute),
                jeu: None,
                riot_id: None,
                rang_valorant: None,
                roles: Vec::new(),
                online: true,
                color: None,
                rank: 0,
                invite: true,
            })
            .collect()
    }

    /// Les portes qui doivent fermer, et pourquoi. `salon_existe` est le
    /// filet : un salon de porte disparu par un autre chemin que la porte
    /// ne doit pas laisser une porte ouverte sur rien.
    fn a_fermer(
        &self,
        now_ms: u64,
        now: Instant,
        salon_existe: impl Fn(ChannelId) -> bool,
    ) -> Vec<(String, &'static str)> {
        let inner = self.inner.lock().unwrap();
        inner
            .portes
            .values()
            .filter_map(|p| {
                if !salon_existe(p.salon) {
                    Some((p.slug.clone(), "salon disparu"))
                } else if now_ms >= p.expire_le {
                    Some((p.slug.clone(), "expirée"))
                } else if p.invites.is_empty()
                    && p.demandes.is_empty()
                    && now.duration_since(p.vide_depuis) >= Duration::from_secs(PORTE_VIDE_SECS)
                {
                    Some((p.slug.clone(), "plus personne depuis dix minutes"))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Les demandes que personne n'a tranchées à temps.
    fn demandes_perimees(&self, now: Instant) -> Vec<u64> {
        let inner = self.inner.lock().unwrap();
        inner
            .portes
            .values()
            .flat_map(|p| p.demandes.iter())
            .filter(|d| now.duration_since(d.arrivee) >= DEMANDE_ATTENTE)
            .map(|d| d.id)
            .collect()
    }

    /// Pour le tableau de bord.
    pub fn tableau(&self) -> Vec<TableauPorte> {
        let inner = self.inner.lock().unwrap();
        let mut portes: Vec<TableauPorte> = inner
            .portes
            .values()
            .map(|p| TableauPorte {
                slug: p.slug.clone(),
                salon: p.nom_salon.clone(),
                hote: p.hote_nom.clone(),
                invites: p.invites.len() as u32,
                demandes: p.demandes.len() as u32,
                expire_le: p.expire_le,
            })
            .collect();
        portes.sort_by(|a, b| a.slug.cmp(&b.slug));
        portes
    }
}

impl Porte {
    /// Un nom déjà porté à cette porte, casse ignorée — par quelqu'un qui
    /// attend ou par quelqu'un d'entré.
    fn nom_pris(&self, nom: &str) -> bool {
        let bas = nom.to_lowercase();
        self.demandes.iter().any(|d| d.nom.to_lowercase() == bas)
            || self.invites.iter().any(|i| {
                i.nom.strip_suffix(INVITE_SUFFIXE).unwrap_or(&i.nom).to_lowercase() == bas
            })
    }
}

// ---------------------------------------------------------------------
// Le cycle, vu du serveur
// ---------------------------------------------------------------------

/// L'hôte de la porte, ou quiconque peut expulser, agit sur elle.
fn peut_gerer(state: &AppState, acteur: UserId, hote: UserId) -> bool {
    acteur == hote || state.holds(acteur, ki_protocol::perm::KICK)
}

/// Tient une adresse à l'écart après un refus ou une expulsion : le
/// limiteur des demandes lui impose tout de suite son délai le plus long.
/// Sans quoi le refusé refrappe dans la seconde, sous un autre nom, et
/// c'est une bannière et un carillon de plus chez tous ceux qui peuvent
/// répondre — jusqu'à ce qu'on ferme la porte.
fn ecarter(state: &AppState, ip: IpAddr) {
    state.portes.throttle.ecarter(ip, &ip.to_string());
}

/// À qui parler d'une porte : son hôte, et tout connecté qui peut expulser.
fn destinataires(state: &AppState, hote: UserId) -> Vec<UserId> {
    let users = state.users.lock().unwrap();
    users
        .iter()
        .filter(|(id, u)| **id == hote || ki_protocol::perm::has(u.perms, ki_protocol::perm::KICK))
        .map(|(id, _)| *id)
        .collect()
}

/// Une sérialisation, N dépôts — chez des connectés QUIC.
fn envoyer_aux_membres(state: &AppState, ids: &[UserId], msg: &ServerMsg) {
    let Some(line) = encode(msg) else { return };
    let users = state.users.lock().unwrap();
    for id in ids {
        if let Some(u) = users.get(id) {
            let _ = u.tx.send_line(&line);
        }
    }
}

/// L'état complet d'une porte, à l'hôte et aux détenteurs d'« Expulser » :
/// un état et non des événements, trente demandes font une seule liste.
fn pousser_etat(state: &AppState, slug: &str) {
    if let Some((msg, hote)) = state.portes.etat(slug) {
        envoyer_aux_membres(state, &destinataires(state, hote), &msg);
    }
}

/// La liste des membres a changé — un invité est entré ou sorti. S'il
/// était en vocal, ceux qui l'y écoutaient l'apprennent aussi.
fn roster_a_tous(state: &AppState) {
    state.broadcast_all(&ServerMsg::Members { members: state.roster() });
    annoncer_occupants(state);
}

/// Dépose une ligne dans la file d'un invité, sans se soucier du résultat :
/// si sa page ne suit plus, la session se termine de toute façon.
fn deposer(tx: &mpsc::Sender<Line>, msg: &ServerMsg) {
    if let Some(line) = encode(msg) {
        let _ = tx.try_send(line);
    }
}

// ---------------------------------------------------------------------
// La voix : emballage et déballage
// ---------------------------------------------------------------------

/// Le nonce XChaCha20 (24 octets) d'un paquet voix : l'émetteur puis le
/// compteur, en petit-boutiste, le reste à zéro — **le même** que celui du
/// client (`ki_voice::nonce_for`) et du bot musique, sans quoi rien ne se
/// déchiffrerait d'un bord à l'autre.
fn nonce_voix(id: UserId, compteur: u64) -> XNonce {
    let mut n = [0u8; 24];
    n[..8].copy_from_slice(&id.to_le_bytes());
    n[8..16].copy_from_slice(&compteur.to_le_bytes());
    XNonce::from(n)
}

/// Une trame Opus d'un invité, emballée comme un client emballe la sienne :
/// en-tête voix à son identifiant, charge chiffrée sous la clé de session.
/// Ce qui en sort part tel quel dans les datagrammes des membres.
fn emballer(chiffre: &XChaCha20Poly1305, id: UserId, compteur: u64, opus: &[u8]) -> Option<Bytes> {
    let scelle = chiffre.encrypt(&nonce_voix(id, compteur), opus).ok()?;
    let mut paquet = vec![0u8; VOICE_HEADER_LEN + scelle.len()];
    write_voice_header(&mut paquet, id, compteur);
    paquet[VOICE_HEADER_LEN..].copy_from_slice(&scelle);
    Some(Bytes::from(paquet))
}

/// Une trame montante de la page : `[version][compteur u64 LE][Opus]`.
/// Rend `(compteur, opus)`, ou rien si ce n'est pas une trame.
fn trame_montante(trame: &[u8]) -> Option<(u64, &[u8])> {
    if trame.len() <= MONTANTE_EN_TETE || trame[0] != VOCAL_VERSION {
        return None;
    }
    let opus = &trame[MONTANTE_EN_TETE..];
    if opus.len() > OPUS_MAX {
        return None;
    }
    let compteur = u64::from_le_bytes(trame[1..MONTANTE_EN_TETE].try_into().ok()?);
    Some((compteur, opus))
}

/// Une trame descendante vers la page :
/// `[version][locuteur u64 LE][compteur u64 LE][Opus]`.
fn trame_descendante(locuteur: UserId, compteur: u64, opus: &[u8]) -> Bytes {
    let mut trame = Vec::with_capacity(DESCENDANTE_EN_TETE + opus.len());
    trame.push(VOCAL_VERSION);
    trame.extend_from_slice(&locuteur.to_le_bytes());
    trame.extend_from_slice(&compteur.to_le_bytes());
    trame.extend_from_slice(opus);
    Bytes::from(trame)
}

/// Une ligne JSON pour la page — pas un `ServerMsg` : aucun client ki-chat
/// ne lit ces messages-là, seule la page.
fn ligne_json(json: serde_json::Value) -> Line {
    let mut ligne = json.to_string();
    ligne.push('\n');
    Line::from(ligne.into_bytes())
}

fn occupants_json(occupants: &[Occupant]) -> serde_json::Value {
    occupants.iter().map(|o| serde_json::json!({ "id": o.id, "nom": o.nom })).collect()
}

/// Ce que la page reçoit quand on l'amène en vocal : le salon, son nom, et
/// qui s'y trouve.
fn ligne_porte_vocal(channel: ChannelId, nom_salon: &str, occupants: &[Occupant]) -> Line {
    ligne_json(serde_json::json!({
        "type": "porte_vocal",
        "channel": channel,
        "nom_salon": nom_salon,
        "occupants": occupants_json(occupants),
    }))
}

/// Ce que la page reçoit quand on l'en sort — ou que le salon disparaît.
fn ligne_porte_vocal_fin() -> Line {
    ligne_json(serde_json::json!({ "type": "porte_vocal_fin" }))
}

/// Les occupants de son salon vocal ont changé.
fn ligne_occupants(occupants: &[Occupant]) -> Line {
    ligne_json(serde_json::json!({ "type": "porte_vocal_occupants", "occupants": occupants_json(occupants) }))
}

/// Qui occupe ce salon vocal : les membres connectés qui y sont, le bot
/// musique s'il y joue, les invités qu'on y a amenés — dans cet ordre, les
/// membres par nom. Chaque verrou pris à son tour, aucun tenu avec un
/// autre.
fn occupants_de(state: &AppState, vocal: ChannelId) -> Vec<Occupant> {
    let mut occupants: Vec<Occupant> = {
        let users = state.users.lock().unwrap();
        users
            .iter()
            .filter(|(_, u)| u.voice == Some(vocal))
            .map(|(id, u)| Occupant { id: *id, nom: u.username.clone() })
            .collect()
    };
    occupants.sort_by_cached_key(|o| o.nom.to_lowercase());
    if state.musique.etat().salon == Some(vocal) {
        occupants.push(Occupant { id: ki_protocol::MUSIQUE_ID, nom: ki_protocol::MUSIQUE_NOM.to_string() });
    }
    occupants.extend(state.portes.invites_en_vocal(vocal));
    occupants
}

/// Les occupants des salons vocaux écoutés ont peut-être changé : chaque
/// invité à l'écoute d'un salon dont la liste a bougé la reçoit — y compris
/// celui qui vient d'y entrer, puisque c'est son entrée qui l'a fait bouger.
/// Appelé à chaque reconstruction des routes voix (un membre entre, sort,
/// se déconnecte), à chaque mouvement d'invité, et quand le bot musique
/// change de salon. Sans invité en vocal, c'est une lecture d'une table
/// vide.
pub fn annoncer_occupants(state: &AppState) {
    let ecoutes = state.portes.ecoutes_par_salon();
    let mut derniers = state.portes.derniers_occupants.lock().unwrap();
    derniers.retain(|salon, _| ecoutes.iter().any(|(s, _)| s == salon));
    for (salon, invites) in ecoutes {
        let occupants = occupants_de(state, salon);
        if derniers.get(&salon) == Some(&occupants) {
            continue;
        }
        let ligne = ligne_occupants(&occupants);
        for invite_id in invites {
            state.portes.envoyer(invite_id, &ligne);
        }
        derniers.insert(salon, occupants);
    }
}

/// Une trame voix d'un invité : vérifiée, budgétée, emballée, puis partie
/// vers les pairs QUIC de son salon vocal — et, en clair, vers les autres
/// invités qui l'écoutent. Rien n'est répondu à la page : la voix ne se
/// discute pas trame par trame.
fn emission(state: &AppState, chiffre: &XChaCha20Poly1305, invite_id: UserId, trame: &[u8]) {
    let Some((compteur_page, opus)) = trame_montante(trame) else { return };
    let Ok((vocal, compteur)) = state.portes.emettre(invite_id, compteur_page) else { return };
    let Some(paquet) = emballer(chiffre, invite_id, compteur, opus) else { return };
    {
        let routes = state.voice_routes.read().unwrap();
        if let Some(pairs) = routes.peers.get(&vocal) {
            for (_, conn) in pairs {
                let _ = conn.send_datagram(paquet.clone());
            }
        }
    }
    state.portes.pousser_audio(vocal, invite_id, compteur, opus, Some(invite_id));
}

/// « 82.65.x.x » : assez pour reconnaître un insistant, pas pour le pister.
fn ip_masquee(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            format!("{}.{}.x.x", o[0], o[1])
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            format!("{:x}:{:x}:…", s[0], s[1])
        }
    }
}

/// L'adresse publique du serveur (`KI_PUBLIC_URL`), sans barre finale.
/// Jamais l'en-tête `Host` d'une requête : c'est le visiteur qui le choisit.
fn base_publique() -> Option<String> {
    std::env::var("KI_PUBLIC_URL")
        .ok()
        .map(|u| u.trim().trim_end_matches('/').to_string())
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
}

/// Le lien à partager : la forme courte, criable en vocal.
fn lien(slug: &str) -> String {
    match base_publique() {
        Some(base) => format!("{base}/{slug}"),
        None => {
            tracing::warn!("KI_PUBLIC_URL absent : le lien de la porte {slug} est relatif");
            format!("/{slug}")
        }
    }
}

/// L'adresse à saisir dans ki-chat (« ts.baws.fun:9988 ») : `KI_PUBLIC_QUIC`
/// si l'admin l'a posée ; sinon l'hôte de `KI_PUBLIC_URL`, sinon celui par
/// lequel l'invité a ouvert la page, avec le port QUIC du serveur.
fn adresse_quic(hote_requete: &str) -> String {
    let public_quic = std::env::var("KI_PUBLIC_QUIC").ok();
    let port = std::env::var("KI_UDP_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(9987);
    adresse_quic_de(public_quic.as_deref(), base_publique().as_deref(), hote_requete, port)
}

/// Le choix de l'adresse, sans l'environnement — pour le tester. L'hôte
/// de la page vaut mieux qu'un texte de repli : un invité qui nous a
/// joints par `192.168.2.36:8080` saura taper `192.168.2.36:9987` ; en
/// dernier recours, la machine elle-même.
fn adresse_quic_de(public_quic: Option<&str>, base: Option<&str>, hote_requete: &str, port: u16) -> String {
    if let Some(a) = public_quic.map(str::trim).filter(|a| !a.is_empty()) {
        return a.to_string();
    }
    let hote = base
        .and_then(|b| b.split("://").nth(1))
        .and_then(|sans_schema| sans_schema.split(['/', ':']).next())
        .filter(|h| !h.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let h = hote_sans_port(hote_requete);
            (!h.is_empty()).then(|| h.to_string())
        })
        .unwrap_or_else(|| "127.0.0.1".into());
    format!("{hote}:{port}")
}

/// L'hôte d'un en-tête `Host`, sans son port — « [::1]:8080 » compris.
fn hote_sans_port(hote: &str) -> &str {
    let hote = hote.trim();
    if let Some(fin) = hote.strip_prefix('[').and_then(|h| h.find(']')) {
        return &hote[..fin + 2];
    }
    hote.rsplit_once(':').map_or(hote, |(h, _)| h)
}

/// Un nom d'invité acceptable : nettoyé comme un pseudo — espaces réduits,
/// sans caractère de contrôle ni commande bidirectionnelle, borné — et sans
/// le suffixe réservé, que le serveur colle lui-même.
fn nom_propre(nom: &str) -> Result<String, String> {
    let plat = ki_protocol::safe_display(nom, MAX_USERNAME + 1)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if plat.is_empty() {
        return Err("donne-toi un nom".into());
    }
    if plat.chars().count() > MAX_USERNAME {
        return Err(format!("nom trop long ({MAX_USERNAME} caractères au plus)"));
    }
    if plat.to_lowercase().ends_with("(web)") {
        return Err("« (web) » est réservé : le serveur l'ajoute lui-même".into());
    }
    Ok(plat)
}

/// Un nom qu'un invité ne peut pas prendre : celui d'un compte, du bot, du
/// fil de jeu ou de la porte elle-même. « Kevin (web) » se distingue déjà
/// de « Kevin », mais un inconnu n'a pas à se faire passer pour un membre
/// dans la bannière de demande.
fn nom_reserve(state: &AppState, nom: &str) -> bool {
    let bas = nom.to_lowercase();
    bas == ki_protocol::MUSIQUE_NOM.to_lowercase()
        || bas == crate::valorant::PSEUDO_DU_FIL.to_lowercase()
        || bas == PSEUDO_PORTE.to_lowercase()
        || state.accounts.list(&state.roles).iter().any(|a| a.username.to_lowercase() == bas)
}

/// Ouvre une porte : salon temporaire, table, audit, liste des salons à
/// tout le monde. Rend le `PorteOuverte` à envoyer à l'hôte. La permission
/// (« Créer des invitations ») est vérifiée par l'appelant.
pub fn ouvrir(
    state: &AppState,
    hote: UserId,
    hote_nom: &str,
    slug: &str,
    nom_salon: &str,
    ttl_secs: u64,
) -> Result<ServerMsg, String> {
    if !slug_valide(slug) {
        return Err(format!(
            "nom de porte invalide : {PORTE_SLUG_MIN} à {PORTE_SLUG_MAX} caractères parmi a-z, 0-9 et le tiret"
        ));
    }
    if state.portes.existe(slug) {
        return Err("une porte de ce nom est déjà ouverte".into());
    }
    if state.portes.nombre() >= PORTES_MAX {
        return Err(format!("{PORTES_MAX} portes ouvertes au plus — ferme-en une d'abord"));
    }
    let ttl = if ttl_secs == 0 { PORTE_TTL_MAX_SECS } else { ttl_secs.clamp(60, PORTE_TTL_MAX_SECS) };
    let expire_le = now_millis().saturating_add(ttl.saturating_mul(1000));
    let nom = if nom_salon.trim().is_empty() { slug } else { nom_salon };
    let salon = state.creer_salon(nom, ki_protocol::ChannelKind::Text, None, Some(expire_le))?;
    let ouverture = Ouverture {
        slug: slug.to_string(),
        salon: salon.id,
        nom_salon: salon.name.clone(),
        hote,
        hote_nom: hote_nom.to_string(),
        expire_le,
    };
    if let Err(e) = state.portes.ouvrir(ouverture, Instant::now()) {
        // Deux hôtes, le même nom, au même instant : la table tranche, et
        // le salon du perdant repart.
        let _ = state.effacer_salon(salon.id);
        return Err(e);
    }
    state.audit.record(
        "porte.open",
        hote_nom,
        slug,
        &format!("salon « {} », {} min", salon.name, ttl / 60),
    );
    tracing::info!("porte {slug} ouverte par {hote_nom} (salon {})", salon.id);
    state.push_channels();
    pousser_etat(state, slug);
    Ok(ServerMsg::PorteOuverte { slug: slug.to_string(), url: lien(slug), salon: salon.id, expire_le })
}

/// Quelqu'un frappe : nom nettoyé, limiteur, table, puis la demande part
/// chez ceux qui peuvent répondre. Rend `(demande_id, invite_id)`.
fn frapper(
    state: &AppState,
    slug: &str,
    nom: &str,
    ip: IpAddr,
    hote: &str,
    tx: mpsc::Sender<Line>,
    audio: mpsc::Sender<Bytes>,
) -> Result<(u64, UserId), String> {
    let nom = nom_propre(nom)?;
    if nom_reserve(state, &nom) {
        return Err("ce nom est celui d'un membre — choisis-en un autre".into());
    }
    // Chaque demande compte comme un « échec » : cinq gratuites, puis un
    // délai qui double. Refusée avant tout le reste, elle ne coûte qu'une
    // recherche dans une table.
    let cle = ip.to_string();
    if let Err(attente) = state.portes.throttle.check(ip, &cle) {
        return Err(format!("trop de demandes — réessaie dans {} s", attente.as_secs().max(1)));
    }
    state.portes.throttle.record_failure(ip, &cle);
    let (demande_id, invite_id) =
        state.portes.frapper(slug, &nom, ip, hote.to_string(), tx, audio, now_millis(), Instant::now())?;
    state.audit.record("porte.request", &nom, slug, &format!("depuis {ip}"));
    tracing::info!("{nom} frappe à la porte {slug} depuis {ip}");
    if let Some(hote) = state.portes.hote_de(slug) {
        envoyer_aux_membres(
            state,
            &destinataires(state, hote),
            &ServerMsg::PorteDemande { slug: slug.to_string(), demande_id, nom, ip_masquee: ip_masquee(ip) },
        );
    }
    pousser_etat(state, slug);
    Ok((demande_id, invite_id))
}

/// Accepte ou refuse une demande. L'hôte de la porte, ou qui peut expulser.
pub fn repondre(
    state: &AppState,
    acteur: UserId,
    acteur_nom: &str,
    demande_id: u64,
    accepter: bool,
    motif: &str,
) -> Result<(), String> {
    let Some((slug, hote)) = state.portes.porte_de_demande(demande_id) else {
        return Err("cette demande n'est plus là — quelqu'un a déjà répondu, ou elle a expiré".into());
    };
    if !peut_gerer(state, acteur, hote) {
        return Err(PERMISSION_REFUSEE.into());
    }
    let r = state.portes.repondre(demande_id, accepter, now_millis())?;
    if accepter {
        // L'historique d'abord, puis le mot d'accueil ; le message système
        // qui suit passe par la diffusion du salon, donc arrive après.
        deposer(&r.tx, &ServerMsg::History { messages: state.history.recent(r.salon, HISTORIQUE_A_L_ENTREE) });
        deposer(
            &r.tx,
            &ServerMsg::Info { message: format!("tu es dans « {} » — bienvenue, {}", r.nom_salon, r.nom) },
        );
        state.audit.record("porte.accept", acteur_nom, &r.nom, &format!("{slug} depuis {}", r.ip));
        tracing::info!("{} entre par la porte {slug}, accepté par {acteur_nom}", r.nom);
        state.poster_systeme(r.salon, PSEUDO_PORTE, &format!("{} a rejoint par la porte {slug}", r.nom));
        roster_a_tous(state);
    } else {
        let reason = if motif.trim().is_empty() { "demande refusée".to_string() } else { motif.trim().to_string() };
        deposer(&r.tx, &ServerMsg::Kicked { reason });
        ecarter(state, r.ip);
        state.audit.record("porte.refuse", acteur_nom, &r.nom, &format!("{slug} depuis {} — {motif}", r.ip));
        tracing::info!("{} refusé à la porte {slug} par {acteur_nom}", r.nom);
    }
    // Refusé : lâcher sa file termine sa session, une fois le motif lu.
    drop(r);
    pousser_etat(state, &slug);
    Ok(())
}

/// Met un invité à la porte. L'hôte, ou qui peut expulser.
pub fn expulser(state: &AppState, acteur: UserId, acteur_nom: &str, invite_id: UserId) -> Result<(), String> {
    let Some(fiche) = state.portes.fiche(invite_id) else {
        return Err("invité introuvable — déjà parti ?".into());
    };
    if !peut_gerer(state, acteur, fiche.hote) {
        return Err(PERMISSION_REFUSEE.into());
    }
    let Some(d) = state.portes.retirer(invite_id, Instant::now()) else {
        return Err("invité introuvable — déjà parti ?".into());
    };
    if let Some(tx) = &d.tx {
        deposer(tx, &ServerMsg::Kicked { reason: format!("mis à la porte par {acteur_nom}") });
    }
    drop(d.tx);
    ecarter(state, d.ip);
    state.audit.record("porte.kick", acteur_nom, &d.nom, &fiche.slug);
    tracing::info!("{} mis à la porte {} par {acteur_nom}", d.nom, fiche.slug);
    // « est parti » d'abord : c'est la tournure que la page lit pour tenir
    // sa liste de présence, comme pour un départ ordinaire.
    state.poster_systeme(d.salon, PSEUDO_PORTE, &format!("{} est parti — mis à la porte par {acteur_nom}", d.nom));
    roster_a_tous(state);
    pousser_etat(state, &fiche.slug);
    Ok(())
}

/// Ferme une porte : adieu aux invités, salon effacé, tout le monde remis
/// d'aplomb. `acteur` = qui ferme, `None` pour le serveur (expiration).
pub fn fermer(state: &AppState, acteur: Option<(UserId, &str)>, slug: &str, motif: &str) -> Result<(), String> {
    let Some(hote) = state.portes.hote_de(slug) else {
        return Err("porte inconnue".into());
    };
    if let Some((id, _)) = acteur {
        if !peut_gerer(state, id, hote) {
            return Err(PERMISSION_REFUSEE.into());
        }
    }
    fermer_sans_verifier(state, acteur.map_or("serveur", |(_, nom)| nom), slug, motif)
}

/// Un admin supprime un salon (`AdminDeleteChannel`) qui est celui d'une
/// porte : c'est la porte qu'on ferme — adieu aux invités, lien mort,
/// journal effacé plutôt qu'archivé, comme à toute fermeture. Gérer les
/// salons suffit ici, sans être l'hôte ni pouvoir expulser. `None` si ce
/// salon n'est celui d'aucune porte. Efface du disque : pool bloquant.
pub fn fermer_salon(state: &AppState, acteur_nom: &str, salon: ChannelId) -> Option<Result<(), String>> {
    let slug = state.portes.slug_du_salon(salon)?;
    Some(fermer_sans_verifier(state, acteur_nom, &slug, &format!("salon supprimé par {acteur_nom}")))
}

/// [`fermer`], la permission déjà tranchée par l'appelant. `par` signe
/// l'audit.
fn fermer_sans_verifier(state: &AppState, par: &str, slug: &str, motif: &str) -> Result<(), String> {
    let Some(fermee) = state.portes.fermer(slug) else {
        return Err("porte inconnue".into());
    };
    let adieu = ServerMsg::PorteFermee { slug: slug.to_string(), motif: motif.to_string() };
    for tx in &fermee.sorties {
        deposer(tx, &adieu);
    }
    // Leurs files lâchées : chaque session se termine, une fois l'adieu lu.
    drop(fermee.sorties);
    // Un salon déjà disparu (le filet du tour d'horloge) n'a plus rien à
    // effacer.
    if state.channels.get(fermee.salon).is_some() {
        if let Err(e) = state.effacer_salon(fermee.salon) {
            tracing::error!("salon {} de la porte {slug} : {e}", fermee.salon);
        }
    }
    state.audit.record("porte.close", par, slug, &format!("{motif} — {} invité(s)", fermee.invites));
    tracing::info!("porte {slug} fermée ({motif}) par {par}");
    state.reconcile_memberships();
    envoyer_aux_membres(state, &destinataires(state, fermee.hote), &adieu);
    Ok(())
}

/// « Lui offrir ki-chat » : une invitation à usage unique, sept jours, au
/// nom de l'acteur. Le lien est posté dans le salon pour que tout le monde
/// voie l'offre ; le code, lui, ne va qu'à l'invité par sa porte
/// (`porte_invitation`) — et revient à l'acteur, qui peut le lui redire.
/// Une ligne du salon est lue par tous les invités de la porte, des
/// inconnus : un code d'accès au serveur n'y a pas sa place, le premier
/// qui l'entrerait aurait un compte. Écrit sur le disque : à appeler
/// depuis le pool bloquant. La permission (« Créer des invitations ») est
/// vérifiée par l'appelant.
pub fn offrir(state: &AppState, acteur_nom: &str, invite_id: UserId) -> Result<String, String> {
    let Some(fiche) = state.portes.fiche(invite_id) else {
        return Err("invité introuvable — déjà parti ?".into());
    };
    let label = format!("porte {} — {}", fiche.slug, fiche.nom);
    let code = state.accounts.create_invite(acteur_nom, Some(1), &label, INVITATION_TTL_SECS)?;
    state.audit.record("invite.create", acteur_nom, "", &format!("{code} — 1 usage(s) « {label} »"));
    tracing::info!("invitation {code} offerte à {} par {acteur_nom}", fiche.nom);
    let serveur = adresse_quic(&fiche.hote_public);
    state.poster_systeme(
        fiche.salon,
        PSEUDO_PORTE,
        &format!(
            "Voilà ki-chat pour {} : {PORTE_TELECHARGEMENT} — serveur {serveur} ; son code d'invitation (valable 7 jours, une fois) est sur sa page",
            fiche.nom
        ),
    );
    if let Some(line) = encode(&ServerMsg::PorteInvitation {
        code: code.clone(),
        serveur,
        telechargement: PORTE_TELECHARGEMENT.to_string(),
    }) {
        state.portes.envoyer(invite_id, &line);
    }
    Ok(code)
}

/// Amène un invité dans un salon vocal (`channel: Some`) ou l'en sort
/// (`None`). L'hôte de la porte, ou qui peut expulser — et, pour l'amener,
/// il faut y être soi-même : on n'envoie pas un inconnu seul dans un salon
/// vocal, on l'y accueille.
pub fn vocal(
    state: &AppState,
    acteur: UserId,
    acteur_nom: &str,
    invite_id: UserId,
    channel: Option<ChannelId>,
) -> Result<(), String> {
    let acteur_en_vocal = {
        let users = state.users.lock().unwrap();
        users.get(&acteur).and_then(|u| u.voice)
    };
    vocal_depuis(state, acteur, acteur_nom, invite_id, channel, acteur_en_vocal)
}

/// [`vocal`], avec le salon vocal de l'acteur déjà lu — de quoi l'éprouver
/// sans connexion QUIC.
fn vocal_depuis(
    state: &AppState,
    acteur: UserId,
    acteur_nom: &str,
    invite_id: UserId,
    channel: Option<ChannelId>,
    acteur_en_vocal: Option<ChannelId>,
) -> Result<(), String> {
    let Some(fiche) = state.portes.fiche(invite_id) else {
        return Err("invité introuvable — déjà parti ?".into());
    };
    if !peut_gerer(state, acteur, fiche.hote) {
        return Err(PERMISSION_REFUSEE.into());
    }
    match channel {
        Some(vocal) => {
            // Un salon qu'on ne voit pas répond comme un salon qui n'existe
            // pas — et un salon textuel n'est pas un salon vocal.
            if !state.channel_is(vocal, ChannelKind::Voice) {
                return Err("salon vocal inconnu".into());
            }
            if acteur_en_vocal != Some(vocal) {
                return Err("entre d'abord dans ce salon vocal : l'invité t'y rejoint".into());
            }
            let (salon, nom, deja) = state.portes.entrer_vocal(invite_id, vocal)?;
            let nom_salon = state.channels.get(vocal).map(|c| c.name).unwrap_or_default();
            // La liste telle qu'elle est : lui n'y figure qu'une fois sa page
            // entrée (`vocal actif:true`), et tous à l'écoute la recevront
            // alors par `annoncer_occupants`, qui verra qu'elle a changé.
            let occupants = occupants_de(state, vocal);
            state.portes.envoyer(invite_id, &ligne_porte_vocal(vocal, &nom_salon, &occupants));
            if deja {
                // Déjà autorisé là : la page est simplement relancée, rien
                // n'a bougé pour les autres.
                return Ok(());
            }
            state.audit.record("porte.vocal", acteur_nom, &nom, &format!("{} → « {nom_salon} »", fiche.slug));
            tracing::info!("{nom} amené en vocal dans « {nom_salon} » par {acteur_nom}");
            // « est en vocal » : une tournure que la page ne lit ni comme
            // une arrivée ni comme un départ dans sa liste de présence.
            state.poster_systeme(salon, PSEUDO_PORTE, &format!("{nom} est en vocal dans « {nom_salon} »"));
        }
        None => {
            if fiche.vocal.is_none() {
                return Err("il n'est pas en vocal".into());
            }
            let Some((salon, nom, ancien)) = state.portes.sortir_vocal(invite_id) else {
                return Err("il n'est pas en vocal".into());
            };
            state.portes.envoyer(invite_id, &ligne_porte_vocal_fin());
            state.audit.record("porte.vocal", acteur_nom, &nom, &format!("{} sort du vocal {ancien}", fiche.slug));
            tracing::info!("{nom} sorti du vocal par {acteur_nom}");
            state.poster_systeme(salon, PSEUDO_PORTE, &format!("{nom} sort du vocal"));
        }
    }
    // Le roster et l'état de la porte le placent là où sa page est : amené
    // d'un vocal à un autre, il y suit ; sorti, il n'y est plus. Ceux qui
    // écoutent l'un ou l'autre salon l'apprennent.
    roster_a_tous(state);
    pousser_etat(state, &fiche.slug);
    Ok(())
}

/// La page d'un invité entre dans son vocal ou le quitte (`vocal`). Entrée,
/// elle reçoit le son, et les membres le voient dans le salon ; sortie, plus
/// rien. Une page qui se croit en vocal sans y être autorisée — reprise
/// après une coupure, la session est nouvelle — reçoit `porte_vocal_fin`
/// pour en sortir proprement, directement, avant même d'être invité.
fn ecouter(state: &AppState, invite_id: UserId, actif: bool) -> Option<Line> {
    match state.portes.ecouter(invite_id, actif) {
        None => Some(ligne_porte_vocal_fin()),
        Some(false) => None,
        Some(true) => {
            if let Some(fiche) = state.portes.fiche(invite_id) {
                tracing::info!("{} {} le vocal", fiche.nom, if actif { "entre dans" } else { "quitte" });
                roster_a_tous(state);
                pousser_etat(state, &fiche.slug);
            }
            None
        }
    }
}

/// Les salons vocaux ont changé : un invité dont le salon a disparu en est
/// sorti, et sa page l'apprend. Appelé par `AppState::reconcile_memberships`,
/// qui diffuse le roster juste après.
pub fn verifier_vocaux(state: &AppState) {
    for (invite_id, nom, salon) in state.portes.vocaux_perdus(|c| state.channel_is(c, ChannelKind::Voice)) {
        state.portes.envoyer(invite_id, &ligne_porte_vocal_fin());
        tracing::info!("{nom} sorti du vocal : le salon n'existe plus");
        state.poster_systeme(salon, PSEUDO_PORTE, &format!("{nom} sort du vocal — le salon n'existe plus"));
    }
}

/// La page s'est fermée, ou la session s'est terminée : on retire la
/// personne, et l'on dit son départ si elle était entrée.
fn depart(state: &AppState, invite_id: UserId) {
    let Some(d) = state.portes.retirer(invite_id, Instant::now()) else { return };
    if d.etait_invite {
        tracing::info!("{} quitte la porte {}", d.nom, d.slug);
        state.poster_systeme(d.salon, PSEUDO_PORTE, &format!("{} est parti", d.nom));
        roster_a_tous(state);
    }
    pousser_etat(state, &d.slug);
}

/// Un tour d'horloge : les portes expirées ou désertes ferment, les
/// demandes que personne n'a tranchées sont congédiées.
pub fn tour(state: &AppState) {
    tour_a(state, now_millis(), Instant::now());
}

fn tour_a(state: &AppState, now_ms: u64, now: Instant) {
    for (slug, motif) in state.portes.a_fermer(now_ms, now, |salon| state.channel_is(salon, ChannelKind::Text)) {
        if let Err(e) = fermer(state, None, &slug, motif) {
            tracing::warn!("fermeture de la porte {slug} : {e}");
        }
    }
    for id in state.portes.demandes_perimees(now) {
        let Some(d) = state.portes.retirer_demande(id) else { continue };
        if let Some(tx) = &d.tx {
            deposer(tx, &ServerMsg::Kicked { reason: "personne n'a répondu — reviens plus tard".into() });
        }
        drop(d.tx);
        tracing::info!("demande de {} à la porte {} sans réponse, retirée", d.nom, d.slug);
        pousser_etat(state, &d.slug);
    }
}

/// La boucle des portes : un tour par minute, tant que le serveur tourne.
/// Le tour efface parfois un salon — du disque — donc sur le pool bloquant.
pub async fn boucle(state: Arc<AppState>) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let s = state.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || tour(&s)).await {
            tracing::error!("tour des portes : {e}");
        }
    }
}

// ---------------------------------------------------------------------
// HTTP : la page et la WebSocket
// ---------------------------------------------------------------------

/// Les routes des portes : `/s/{slug}` et sa forme courte `/{slug}` pour la
/// page, la WebSocket juste en dessous (`…/ws` — la page la déduit de son
/// propre chemin), la feuille et le script à côté. La forme courte ne gêne
/// aucune route statique du serveur (`/files`, `/clips`, `/tel`, `/diag`,
/// `/admin`, `/musique`, `/upload`…) : le routeur donne la priorité aux
/// chemins statiques, et un slug ne contient de toute façon ni point ni
/// majuscule.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/s/porte.css", get(feuille))
        .route("/s/porte.js", get(script))
        .route("/s/{slug}", get(page))
        .route("/s/{slug}/ws", get(ws))
        .route("/{slug}", get(page))
        .route("/{slug}/ws", get(ws))
}

/// `GET /s/{slug}` et `GET /{slug}` : la page, si la porte est ouverte.
async fn page(State(state): State<Arc<AppState>>, Path(slug): Path<String>) -> Response {
    if !slug_valide(&slug) || !state.portes.existe(&slug) {
        return (StatusCode::NOT_FOUND, "pas de porte ouverte à ce nom").into_response();
    }
    let page = PAGE.replace("{{serveur}}", &echapper(&state.meta.get().name));
    let mut reponse =
        (StatusCode::OK, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], page).into_response();
    en_tetes(reponse.headers_mut());
    reponse
}

/// `GET /s/porte.css` : la feuille de la page.
async fn feuille() -> Response {
    fichier("text/css; charset=utf-8", FEUILLE)
}

/// `GET /s/porte.js` : le script de la page.
async fn script() -> Response {
    fichier("text/javascript; charset=utf-8", SCRIPT)
}

/// Un fichier de la page, avec les mêmes en-têtes qu'elle.
fn fichier(genre: &'static str, contenu: &'static str) -> Response {
    let mut reponse = (StatusCode::OK, [(header::CONTENT_TYPE, genre)], contenu).into_response();
    en_tetes(reponse.headers_mut());
    reponse
}

/// Le nom du serveur dans un attribut HTML : les cinq caractères qui
/// changeraient le sens de la page.
fn echapper(texte: &str) -> String {
    let mut out = String::with_capacity(texte.len());
    for c in texte.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// `GET /s/{slug}/ws` et `GET /{slug}/ws` : la WebSocket d'un visiteur. Le
/// sas par adresse s'applique dès la poignée de main, et la place se rend
/// dès que la demande est posée (voir [`session`]). L'origine, quand un
/// navigateur la donne, doit être la nôtre.
async fn ws(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
    ConnectInfo(adresse): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !slug_valide(&slug) || !state.portes.existe(&slug) {
        return (StatusCode::NOT_FOUND, "pas de porte ouverte à ce nom").into_response();
    }
    let origine = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    let hote = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    if !origine_admise(origine, hote, base_publique().as_deref()) {
        tracing::warn!("porte {slug} : WebSocket refusée depuis l'origine {}", origine.unwrap_or("?"));
        return (StatusCode::FORBIDDEN, "cette page n'est pas celle du serveur").into_response();
    }
    // L'hôte tel que l'invité l'a tapé, sans le port : c'est l'adresse
    // qu'il saura resaisir dans ki-chat si on la lui offre.
    let hote_public = hote_sans_port(hote.unwrap_or("")).to_string();
    let ip = adresse.ip();
    let Some(jeton) = state.sas.entrer(ip) else {
        tracing::warn!("sas plein pour {ip} : WebSocket refusée");
        return (StatusCode::TOO_MANY_REQUESTS, "trop de connexions depuis cette adresse").into_response();
    };
    upgrade
        .max_message_size(TRAME_MAX)
        .max_frame_size(TRAME_MAX)
        .on_upgrade(move |socket| session(state, socket, slug, ip, hote_public, jeton))
}

/// L'origine d'une ouverture de WebSocket est-elle la nôtre ? Un
/// navigateur envoie toujours `Origin` ; un autre client (un outil, les
/// tests) ne l'envoie pas, et passe. Présente, elle doit être l'origine
/// publique du serveur (`KI_PUBLIC_URL` : schéma, hôte et port) ou, à
/// défaut, avoir pour hôte celui de la requête (`Host`). Sans cela, la page
/// d'un autre site pourrait ouvrir une WebSocket vers nous depuis le
/// navigateur de son visiteur — frapper à la porte avec **son** adresse,
/// celle d'un membre connu par exemple, puis écrire et entendre à sa place.
/// La politique de sécurité de notre page borne ce qu'elle contacte, pas ce
/// qu'une autre page ouvre vers nous.
fn origine_admise(origine: Option<&str>, hote: Option<&str>, base: Option<&str>) -> bool {
    let Some(origine) = origine else { return true };
    let origine = origine.trim().trim_end_matches('/');
    if let Some(base) = base {
        // L'origine de la base : jusqu'au premier `/` après le schéma.
        let apres_schema = base.find("://").map_or(0, |i| i + 3);
        let fin = base[apres_schema..].find('/').map_or(base.len(), |j| apres_schema + j);
        return origine.eq_ignore_ascii_case(&base[..fin]);
    }
    let Some(hote) = hote else { return false };
    let hote_origine = origine.split("://").nth(1).unwrap_or("");
    !hote_origine.is_empty() && hote_origine.eq_ignore_ascii_case(hote.trim())
}

/// Ce que la page envoie. Rien d'autre n'est accepté : ni fichier, ni
/// réaction, ni réponse — la porte est le seul chemin d'écriture d'un
/// invité, et il est étroit à dessein.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum MsgInvite {
    Hello { nom: String },
    Chat { text: String },
    Ping,
    /// Sa page entre dans le vocal où on l'a autorisé (`true`), ou le quitte.
    Vocal { actif: bool },
}

/// Une ligne de contrôle, sans son saut de ligne : une trame texte.
fn texte_de(line: &Line) -> String {
    let sans_fin = line.strip_suffix(b"\n").unwrap_or(line);
    String::from_utf8_lossy(sans_fin).into_owned()
}

type Sortie = futures_util::stream::SplitSink<WebSocket, Message>;
type Entree = futures_util::stream::SplitStream<WebSocket>;

/// Une session : `hello`, la demande, l'attente, puis le salon — jusqu'à
/// ce que la page ferme, que la file soit lâchée (refus, expulsion,
/// fermeture, saturation) ou que le silence dure trop.
///
/// La place du sas (`place`) est rendue dès que la demande est posée : le
/// sas est celui de l'authentification QUIC, prévu pour une traversée de
/// quelques secondes, et une session d'invité dure des heures — tenue
/// jusqu'au bout, trente-deux pages ouvertes derrière une même box
/// fermeraient le serveur aux membres qui la partagent. Une fois la
/// demande posée, ce sont les plafonds de la porte qui bornent.
async fn session(
    state: Arc<AppState>,
    socket: WebSocket,
    slug: String,
    ip: IpAddr,
    hote: String,
    place: JetonSas,
) {
    let (mut sink, mut stream) = socket.split();
    let nom = match tokio::time::timeout(HELLO_DELAI, stream.next()).await {
        Ok(Some(Ok(Message::Text(t)))) => match serde_json::from_str::<MsgInvite>(&t) {
            Ok(MsgInvite::Hello { nom }) => nom,
            _ => {
                dire(&mut sink, &ServerMsg::Error { message: "le premier message doit être hello".into() }).await;
                fermer_proprement(&mut sink, &mut stream).await;
                return;
            }
        },
        _ => {
            fermer_proprement(&mut sink, &mut stream).await;
            return;
        }
    };
    let (tx, mut rx) = mpsc::channel::<Line>(FILE_INVITE);
    // Sa file audio, dès maintenant : la table en garde le bout émetteur
    // pour le jour où on l'amène en vocal.
    let (audio_tx, mut audio_rx) = mpsc::channel::<Bytes>(FILE_AUDIO);
    let invite_id = match frapper(&state, &slug, &nom, ip, &hote, tx, audio_tx) {
        Ok((_, invite_id)) => invite_id,
        Err(e) => {
            dire(&mut sink, &ServerMsg::Error { message: e }).await;
            fermer_proprement(&mut sink, &mut stream).await;
            return;
        }
    };
    drop(place);
    dire(&mut sink, &ServerMsg::Info { message: "les membres ont été prévenus — en attente d'une réponse".into() })
        .await;
    // Le chiffre de session, pour emballer sa voix : la clé du serveur, la
    // même que celle remise aux clients dans `Welcome`.
    let chiffre = XChaCha20Poly1305::new((&state.voice_key).into());

    let mut ping = tokio::time::interval(PING_TOUTES);
    let mut dernier_signe = Instant::now();
    // Chaque envoi est borné (`ecrire`) : tant qu'un `send` attend, aucune
    // autre branche n'est sondée — pas même le ping qui mesure le silence.
    loop {
        tokio::select! {
            ligne = rx.recv() => match ligne {
                Some(ligne) => {
                    if !ecrire(&mut sink, Message::Text(texte_de(&ligne).into())).await {
                        break;
                    }
                }
                // Plus personne ne tient sa file : la session est finie, et
                // ce qui devait lui être dit l'a été.
                None => {
                    fermer_proprement(&mut sink, &mut stream).await;
                    break;
                }
            },
            // Le son du salon, en clair, pendant qu'il est en vocal.
            Some(trame) = audio_rx.recv() => {
                if !ecrire(&mut sink, Message::Binary(trame)).await {
                    break;
                }
            },
            trame = stream.next() => match trame {
                Some(Ok(Message::Text(t))) => {
                    dernier_signe = Instant::now();
                    if let Some(reponse) = recevoir(&state, invite_id, &t) {
                        if !ecrire(&mut sink, Message::Text(texte_de(&reponse).into())).await {
                            break;
                        }
                    }
                }
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => dernier_signe = Instant::now(),
                // Sa voix. Hors vocal, ou mal formée : la trame tombe, sans
                // un mot — une page qui envoie encore une trame ou deux après
                // en être sortie n'a rien fait de mal.
                Some(Ok(Message::Binary(b))) => {
                    dernier_signe = Instant::now();
                    emission(&state, &chiffre, invite_id, &b);
                }
                _ => break,
            },
            _ = ping.tick() => {
                if dernier_signe.elapsed() > SILENCE_MAX {
                    tracing::info!("porte {slug} : la page de {nom} ne répond plus, session fermée");
                    fermer_proprement(&mut sink, &mut stream).await;
                    break;
                }
                if !ecrire(&mut sink, Message::Ping(bytes::Bytes::new())).await {
                    break;
                }
            }
        }
    }
    depart(&state, invite_id);
}

/// Écrit une trame à la page sans s'y laisser prendre. Une page disparue
/// sans fermer — téléphone verrouillé, réseau tombé — laisse un socket
/// demi-ouvert : le tampon d'envoi se remplit, en vocal en quelques
/// secondes, et l'écriture reste suspendue jusqu'à ce que le noyau
/// abandonne ses retransmissions, bien après nos soixante secondes de
/// silence. Passé ce délai, c'est une erreur comme une autre : la session
/// se termine, et l'invité sort du roster.
async fn ecrire(sink: &mut Sortie, trame: Message) -> bool {
    matches!(tokio::time::timeout(SILENCE_MAX, sink.send(trame)).await, Ok(Ok(())))
}

/// Ferme la session proprement : le Close part, puis on lit ce que la page
/// avait encore envoyé jusqu'à son propre Close — borné. Sans cette
/// lecture, une trame reçue mais jamais lue (le pong à notre ping, par
/// exemple) fait fermer le socket par un RST, qui peut emporter avec lui
/// ce qu'on venait d'écrire : le motif d'un refus, l'adieu d'une fermeture.
async fn fermer_proprement(sink: &mut Sortie, stream: &mut Entree) {
    let _ = tokio::time::timeout(Duration::from_secs(2), sink.close()).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(Ok(trame)) = stream.next().await {
            if matches!(trame, Message::Close(_)) {
                break;
            }
        }
    })
    .await;
}

/// Écrit un message à la page, directement.
async fn dire(sink: &mut Sortie, msg: &ServerMsg) {
    if let Some(line) = encode(msg) {
        ecrire(sink, Message::Text(texte_de(&line).into())).await;
    }
}

/// Une trame de la page : un message à poster, un ping, un mot sur le
/// vocal — et ce qu'on lui répond directement, s'il y a lieu.
fn recevoir(state: &AppState, invite_id: UserId, trame: &str) -> Option<Line> {
    let erreur = |message: String| encode(&ServerMsg::Error { message });
    match serde_json::from_str::<MsgInvite>(trame) {
        Ok(MsgInvite::Chat { text }) => match state.portes.ecrire(invite_id) {
            Ok((salon, nom)) => match ki_protocol::clean_chat(&text) {
                Ok(text) => {
                    state.poster_membre(salon, invite_id, &nom, &text);
                    None
                }
                Err(e) => erreur(e),
            },
            Err(e) => erreur(e),
        },
        Ok(MsgInvite::Ping) => encode(&ServerMsg::Pong),
        Ok(MsgInvite::Vocal { actif }) => ecouter(state, invite_id, actif),
        Ok(MsgInvite::Hello { .. }) => erreur("tu t'es déjà présenté".into()),
        Err(_) => erreur("message invalide".into()),
    }
}

// ---------------------------------------------------------------------
// Les en-têtes de la page
// ---------------------------------------------------------------------

/// La politique de sécurité de contenu, calculée une fois : rien par
/// défaut ; pour les scripts et les styles, ce que le serveur sert lui-même
/// (`'self'` — la feuille et le script à côté de la page) et les blocs que
/// la page embarquerait, reconnus à leur empreinte. Jamais
/// `unsafe-inline` : un message qui réussirait à s'écrire dans la page ne
/// pourrait rien exécuter.
static CSP: LazyLock<String> = LazyLock::new(|| csp_de(PAGE));

fn csp_de(page: &str) -> String {
    let sources = |balise: &str| -> String {
        let mut sources = vec!["'self'".to_string()];
        sources.extend(
            blocs(page, balise)
                .into_iter()
                .map(|bloc| format!("'sha256-{}'", base64(&Sha256::digest(bloc.as_bytes())))),
        );
        sources.join(" ")
    };
    // La WebSocket : `'self'` la couvre chez les navigateurs récents ; on
    // ajoute l'origine publique en clair pour les autres — c'est une
    // configuration de l'admin, pas un en-tête du visiteur.
    let mut connexion = String::from("'self'");
    if let Some(base) = base_publique() {
        if let Some(hote) = base.split("://").nth(1) {
            let schema = if base.starts_with("https://") { "wss" } else { "ws" };
            connexion.push_str(&format!(" {schema}://{hote}"));
        }
    }
    format!(
        "default-src 'none'; script-src {}; style-src {}; connect-src {connexion}; img-src 'self' data:; \
         base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        sources("script"),
        sources("style"),
    )
}

/// Le contenu de chaque `<balise …>…</balise>` de la page, tel quel — c'est
/// exactement ce que le navigateur hache.
fn blocs<'a>(page: &'a str, balise: &str) -> Vec<&'a str> {
    let ouvre = format!("<{balise}");
    let ferme = format!("</{balise}>");
    let mut reste = page;
    let mut out = Vec::new();
    while let Some(i) = reste.find(&ouvre) {
        let apres = &reste[i + ouvre.len()..];
        // `<scripts>` ou `<styles>` n'est pas la balise cherchée.
        if !apres.starts_with('>') && !apres.starts_with(char::is_whitespace) {
            reste = apres;
            continue;
        }
        let Some(j) = apres.find('>') else { break };
        let contenu = &apres[j + 1..];
        let Some(k) = contenu.find(&ferme) else { break };
        // `<script src="…"></script>` : rien en ligne, rien à hacher.
        if !contenu[..k].trim().is_empty() {
            out.push(&contenu[..k]);
        }
        reste = &contenu[k + ferme.len()..];
    }
    out
}

/// Base64 standard, avec remplissage — celui qu'exige une empreinte CSP.
/// Quinze lignes valent mieux qu'une dépendance pour ça.
fn base64(octets: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(octets.len().div_ceil(3) * 4);
    for bloc in octets.chunks(3) {
        let n = (u32::from(bloc[0]) << 16)
            | (u32::from(*bloc.get(1).unwrap_or(&0)) << 8)
            | u32::from(*bloc.get(2).unwrap_or(&0));
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if bloc.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if bloc.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Les en-têtes de la page : la CSP, jamais dans un cadre, pas de
/// référent, pas de reniflage de type, pas de cache, le micro pour la page
/// seule (sa voix), ni caméra ni position.
fn en_tetes(h: &mut HeaderMap) {
    let csp = HeaderValue::from_str(&CSP).unwrap_or_else(|_| HeaderValue::from_static("default-src 'none'"));
    h.insert(header::CONTENT_SECURITY_POLICY, csp);
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("microphone=(self), camera=(), geolocation=()"),
    );
    h.insert(HeaderName::from_static("cross-origin-opener-policy"), HeaderValue::from_static("same-origin"));
}

#[cfg(test)]
mod tests {

    /// L'adresse à saisir dans ki-chat : celle que l'admin a fixée, sinon
    /// l'hôte public, sinon celui par lequel l'invité est venu — jamais un
    /// texte de repli qui ne se tape pas.
    #[test]
    fn l_adresse_a_saisir_revient_a_l_hote_de_la_page() {
        assert_eq!(adresse_quic_de(Some("ts.baws.fun:9988"), None, "127.0.0.1", 9987), "ts.baws.fun:9988");
        assert_eq!(adresse_quic_de(Some("  "), Some("https://ts.baws.fun:8080/"), "127.0.0.1", 9987), "ts.baws.fun:9987");
        assert_eq!(adresse_quic_de(None, None, "192.168.2.36:8080", 9987), "192.168.2.36:9987");
        assert_eq!(adresse_quic_de(None, None, "[::1]:8080", 9987), "[::1]:9987");
        assert_eq!(adresse_quic_de(None, None, "", 9987), "127.0.0.1:9987");
        assert_eq!(hote_sans_port("ts.baws.fun"), "ts.baws.fun");
        assert_eq!(hote_sans_port(" 127.0.0.1:8080 "), "127.0.0.1");
    }
    use super::*;
    use crate::state::SAS_MAX_PAR_IP;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message as Trame;

    type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

    const HOTE: UserId = 1;
    const HOTE_NOM: &str = "redik";

    /// Un serveur complet sur un dossier jetable — pas de connexion QUIC :
    /// l'hôte n'est pas « connecté », ce qui n'empêche rien ici, puisque
    /// ce qu'on lui envoie tombe simplement dans le vide.
    fn etat(nom: &str) -> Arc<AppState> {
        let dir = std::env::temp_dir().join(format!("ki-porte-{}-{nom}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let quota = crate::files::Quota { max_bytes: 0, ttl_days: 0 };
        Arc::new(AppState::new("changeme".into(), dir.to_str().unwrap(), quota, quota, 512).unwrap())
    }

    /// Le routeur des portes, seul, sur un port libre de la boucle locale.
    async fn servir(state: Arc<AppState>) -> SocketAddr {
        let app = routes().with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
                .await
                .unwrap();
        });
        addr
    }

    async fn connecter(addr: SocketAddr, slug: &str) -> Ws {
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/s/{slug}/ws")).await.unwrap();
        ws
    }

    async fn frapper_ws(addr: SocketAddr, slug: &str, nom: &str) -> Ws {
        let mut ws = connecter(addr, slug).await;
        envoyer(&mut ws, &serde_json::json!({ "type": "hello", "nom": nom })).await;
        ws
    }

    async fn envoyer(ws: &mut Ws, v: &serde_json::Value) {
        ws.send(Trame::text(v.to_string())).await.unwrap();
    }

    /// La page clique « Rejoindre le vocal ».
    async fn entre(ws: &mut Ws) {
        envoyer(ws, &serde_json::json!({ "type": "vocal", "actif": true })).await;
    }

    /// Le prochain message de contrôle, pings et pongs ignorés ; `null`
    /// quand la connexion est fermée.
    async fn suivant(ws: &mut Ws) -> serde_json::Value {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
                Ok(Some(Ok(Trame::Text(t)))) => return serde_json::from_str(t.as_str()).unwrap(),
                Ok(Some(Ok(Trame::Ping(_)))) | Ok(Some(Ok(Trame::Pong(_)))) => continue,
                Ok(Some(Ok(Trame::Close(_)))) | Ok(None) => return serde_json::Value::Null,
                Ok(Some(Err(e))) => panic!("erreur de la connexion : {e}"),
                Ok(Some(Ok(autre))) => panic!("trame inattendue : {autre:?}"),
                Err(_) => panic!("rien reçu en cinq secondes"),
            }
        }
    }

    /// Le prochain message de contrôle de ce type, les autres passés —
    /// une annonce du fil peut s'intercaler avant la réponse attendue.
    async fn jusqu_au(ws: &mut Ws, genre: &str) -> serde_json::Value {
        loop {
            let v = suivant(ws).await;
            if v.is_null() || v["type"] == genre {
                return v;
            }
        }
    }

    /// Attend la fermeture : rien d'autre ne doit arriver avant.
    async fn attendre_fermeture(ws: &mut Ws) {
        assert_eq!(suivant(ws).await, serde_json::Value::Null, "la connexion devait se fermer");
    }

    /// La prochaine trame binaire — le son. Les lignes de contrôle qui
    /// s'intercalent (une annonce dans le fil) sont passées : les deux
    /// files ne sont pas ordonnées entre elles.
    async fn suivant_bin(ws: &mut Ws) -> Vec<u8> {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
                Ok(Some(Ok(Trame::Binary(b)))) => return b.to_vec(),
                Ok(Some(Ok(Trame::Text(_)))) | Ok(Some(Ok(Trame::Ping(_)))) | Ok(Some(Ok(Trame::Pong(_)))) => continue,
                autre => panic!("pas de trame binaire : {autre:?}"),
            }
        }
    }

    /// Rien en binaire pendant un court instant : ce qui ne devait pas
    /// être entendu ne l'est pas.
    async fn rien_en_binaire(ws: &mut Ws) {
        let fin = tokio::time::sleep(Duration::from_millis(300));
        tokio::pin!(fin);
        loop {
            tokio::select! {
                _ = &mut fin => return,
                trame = ws.next() => match trame {
                    Some(Ok(Trame::Binary(b))) => panic!("du son est arrivé alors que rien ne devait : {b:?}"),
                    Some(Ok(_)) => continue,
                    autre => panic!("connexion perdue : {autre:?}"),
                },
            }
        }
    }

    /// Rien en texte pendant un court instant : ce qui ne devait pas être
    /// dit ne l'est pas.
    async fn rien_en_texte(ws: &mut Ws) {
        let fin = tokio::time::sleep(Duration::from_millis(300));
        tokio::pin!(fin);
        loop {
            tokio::select! {
                _ = &mut fin => return,
                trame = ws.next() => match trame {
                    Some(Ok(Trame::Text(t))) => panic!("une ligne est arrivée alors que rien ne devait : {t}"),
                    Some(Ok(_)) => continue,
                    autre => panic!("connexion perdue : {autre:?}"),
                },
            }
        }
    }

    /// Un paquet voix tel qu'un client — ou le bot — le forge : en-tête à
    /// son identifiant, charge scellée sous la clé, nonce (id, compteur).
    fn paquet_de_membre(cle: &[u8; 32], id: UserId, compteur: u64, opus: &[u8]) -> Vec<u8> {
        let chiffre = XChaCha20Poly1305::new(cle.into());
        let mut nonce = [0u8; 24];
        nonce[..8].copy_from_slice(&id.to_le_bytes());
        nonce[8..16].copy_from_slice(&compteur.to_le_bytes());
        let scelle = chiffre.encrypt(&XNonce::from(nonce), opus).unwrap();
        let mut paquet = vec![0u8; VOICE_HEADER_LEN + scelle.len()];
        write_voice_header(&mut paquet, id, compteur);
        paquet[VOICE_HEADER_LEN..].copy_from_slice(&scelle);
        paquet
    }

    /// Ce que la page reçoit pour un locuteur donné.
    fn attendu(locuteur: UserId, compteur: u64, opus: &[u8]) -> Vec<u8> {
        trame_descendante(locuteur, compteur, opus).to_vec()
    }

    /// Une trame montante de la page.
    fn montante(compteur: u64, opus: &[u8]) -> Vec<u8> {
        let mut t = vec![VOCAL_VERSION];
        t.extend_from_slice(&compteur.to_le_bytes());
        t.extend_from_slice(opus);
        t
    }

    /// Frappe, est accepté, lit l'historique, l'accueil et l'entrée ; rend
    /// la page et l'identifiant de l'invité.
    async fn entrer(state: &AppState, addr: SocketAddr, slug: &str, nom: &str) -> (Ws, UserId) {
        let mut ws = frapper_ws(addr, slug, nom).await;
        suivant(&mut ws).await;
        let demande = demande_en_attente(state, slug);
        repondre(state, HOTE, HOTE_NOM, demande, true, "").unwrap();
        for _ in 0..3 {
            suivant(&mut ws).await;
        }
        let complet = format!("{nom}{INVITE_SUFFIXE}");
        let id = state.roster().into_iter().find(|m| m.invite && m.username == complet).unwrap().user_id;
        (ws, id)
    }

    fn salon_vocal(state: &AppState, nom: &str) -> ChannelId {
        state.creer_salon(nom, ChannelKind::Voice, None, None).unwrap().id
    }

    fn voix_de(state: &AppState, id: UserId) -> Option<ChannelId> {
        state.roster().into_iter().find(|m| m.user_id == id).and_then(|m| m.voice)
    }

    async fn http_get(addr: SocketAddr, chemin: &str) -> (u16, String) {
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        s.write_all(format!("GET {chemin} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        let texte = String::from_utf8_lossy(&buf).into_owned();
        let statut = texte.split_whitespace().nth(1).unwrap().parse().unwrap();
        (statut, texte)
    }

    fn demande_en_attente(state: &AppState, slug: &str) -> u64 {
        let inner = state.portes.inner.lock().unwrap();
        inner.portes[slug].demandes[0].id
    }

    fn ouvrir_ok(state: &AppState, slug: &str) -> ChannelId {
        match ouvrir(state, HOTE, HOTE_NOM, slug, "", 0).unwrap() {
            ServerMsg::PorteOuverte { salon, .. } => salon,
            autre => panic!("{autre:?}"),
        }
    }

    /// LE test : ouvrir, frapper, accepter, écrire dans les deux sens, ne
    /// rien recevoir d'ailleurs, fermer — et que le salon disparaisse sans
    /// laisser de trace.
    #[tokio::test]
    async fn le_cycle_complet_d_une_porte() {
        let state = etat("cycle");
        let addr = servir(state.clone()).await;

        // Ouverture : un salon textuel daté, visible dans la liste.
        let ouverte = ouvrir(&state, HOTE, HOTE_NOM, "salon1", "Soirée", 3600).unwrap();
        let ServerMsg::PorteOuverte { slug, url, salon, expire_le } = ouverte else { panic!("{ouverte:?}") };
        assert_eq!(slug, "salon1");
        assert!(url.ends_with("/salon1"), "{url}");
        let info = state.channels.get(salon).expect("le salon temporaire existe");
        assert_eq!(info.name, "Soirée");
        assert_eq!(info.expire_le, Some(expire_le));
        assert!(expire_le > now_millis() && expire_le <= now_millis() + 3_600_000);
        assert_eq!(state.audit.recent(1)[0].action, "porte.open");

        // Quelqu'un frappe : il attend, et l'hôte a la demande.
        let mut kevin = frapper_ws(addr, "salon1", "  Kevin  ").await;
        let attente = suivant(&mut kevin).await;
        assert_eq!(attente["type"], "info", "{attente}");
        let demande_id = demande_en_attente(&state, "salon1");
        // La demande posée, sa place dans le sas est rendue : le sas est
        // celui des connexions QUIC, et une page qui attend — puis reste
        // deux heures — ne doit pas fermer le serveur aux membres derrière
        // la même box. Toutes les places sont libres.
        let ip_locale: IpAddr = "127.0.0.1".parse().unwrap();
        let places: Vec<_> = (0..SAS_MAX_PAR_IP).filter_map(|_| state.sas.entrer(ip_locale)).collect();
        assert_eq!(places.len() as u32, SAS_MAX_PAR_IP, "la WebSocket tient encore une place du sas");
        drop(places);
        assert_eq!(state.audit.recent(1)[0].action, "porte.request");
        // Écrire avant d'être accepté : refusé, sans fermer.
        envoyer(&mut kevin, &serde_json::json!({ "type": "chat", "text": "coucou" })).await;
        let refus = suivant(&mut kevin).await;
        assert_eq!(refus["type"], "error", "{refus}");
        assert!(state.history.recent(salon, 10).is_empty());

        // Quelqu'un qui n'est ni l'hôte ni modérateur ne tranche pas.
        assert_eq!(repondre(&state, 42, "intrus", demande_id, true, "").unwrap_err(), PERMISSION_REFUSEE);

        // L'hôte accepte : historique, mot d'accueil, puis l'entrée annoncée
        // dans le salon — et la liste des membres le montre, marqué.
        repondre(&state, HOTE, HOTE_NOM, demande_id, true, "").unwrap();
        let historique = suivant(&mut kevin).await;
        assert_eq!(historique["type"], "history", "{historique}");
        assert_eq!(historique["messages"].as_array().unwrap().len(), 0);
        let accueil = suivant(&mut kevin).await;
        assert_eq!(accueil["type"], "info", "{accueil}");
        assert!(accueil["message"].as_str().unwrap().contains("Kevin (web)"));
        let entree = suivant(&mut kevin).await;
        assert_eq!(entree["type"], "chat", "{entree}");
        assert_eq!(entree["user_id"], 0);
        assert_eq!(entree["username"], PSEUDO_PORTE);
        assert!(entree["text"].as_str().unwrap().starts_with("Kevin (web) a rejoint"));
        assert_eq!(state.audit.recent(1)[0].action, "porte.accept");
        let invites: Vec<Member> = state.roster().into_iter().filter(|m| m.invite).collect();
        assert_eq!(invites.len(), 1);
        assert_eq!(invites[0].username, "Kevin (web)");
        assert!(invites[0].online && ki_protocol::est_invite(invites[0].user_id));
        let invite_id = invites[0].user_id;
        // Jamais dans la table des connectés.
        assert!(!state.users.lock().unwrap().contains_key(&invite_id));

        // L'invité écrit : le message est dans l'historique à son nom, et il
        // lui revient par la diffusion du salon.
        envoyer(&mut kevin, &serde_json::json!({ "type": "chat", "text": "salut tout le monde" })).await;
        let echo = suivant(&mut kevin).await;
        assert_eq!(echo["type"], "chat");
        assert_eq!(echo["username"], "Kevin (web)");
        assert_eq!(echo["user_id"], invite_id);
        assert_eq!(echo["text"], "salut tout le monde");
        let dernier = state.history.recent(salon, 1).pop().unwrap();
        assert_eq!((dernier.user_id, dernier.username.as_str()), (invite_id, "Kevin (web)"));

        // Un membre écrit dans le salon : l'invité le reçoit. Un membre
        // écrit ailleurs : rien — jamais une ligne d'un autre salon.
        state.poster_membre(1, HOTE, HOTE_NOM, "secret du général");
        state.poster_membre(salon, HOTE, HOTE_NOM, "bienvenue Kevin");
        let recu = suivant(&mut kevin).await;
        assert_eq!(recu["text"], "bienvenue Kevin", "la ligne du salon général ne doit pas passer : {recu}");
        // Un ping de la page : un pong, rien d'autre.
        envoyer(&mut kevin, &serde_json::json!({ "type": "ping" })).await;
        assert_eq!(suivant(&mut kevin).await["type"], "pong");
        // Le tableau de bord compte la porte.
        let tableau = state.portes.tableau();
        assert_eq!((tableau[0].slug.as_str(), tableau[0].invites, tableau[0].demandes), ("salon1", 1, 0));

        // L'hôte a lu le salon : il y a posé un repère.
        assert!(state.lus.marquer(HOTE, salon, dernier.ts));
        assert!(state.lus.de(HOTE).contains_key(&salon));

        // Fermeture par l'hôte : adieu à l'invité, connexion fermée, salon
        // effacé du disque et de la liste, plus personne d'invité — et plus
        // de repère de lecture chez qui l'avait lu.
        fermer(&state, Some((HOTE, HOTE_NOM)), "salon1", "fermée par redik").unwrap();
        let adieu = suivant(&mut kevin).await;
        assert_eq!(adieu["type"], "porte_fermee", "{adieu}");
        assert_eq!(adieu["motif"], "fermée par redik");
        attendre_fermeture(&mut kevin).await;
        assert!(state.channels.get(salon).is_none());
        assert!(!std::path::Path::new(&state.data_dir).join(format!("channel-{salon}.jsonl")).exists());
        assert!(state.roster().iter().all(|m| !m.invite));
        assert!(!state.portes.existe("salon1"));
        assert!(!state.lus.de(HOTE).contains_key(&salon), "le repère de lecture est resté");
        assert_eq!(state.audit.recent(1)[0].action, "porte.close");
        // Une porte fermée ne se ferme pas deux fois, et la page fait 404.
        assert!(fermer(&state, Some((HOTE, HOTE_NOM)), "salon1", "encore").is_err());
        assert_eq!(http_get(addr, "/s/salon1").await.0, 404);
    }

    /// Refusé, expulsé, invité : les trois autres réponses de l'hôte.
    #[tokio::test]
    async fn refus_expulsion_et_offre() {
        let state = etat("reponses");
        let addr = servir(state.clone()).await;
        let salon = ouvrir_ok(&state, "soiree");

        // Refus, avec un motif : l'intéressé le lit, puis la porte se ferme.
        let mut lea = frapper_ws(addr, "soiree", "Léa").await;
        suivant(&mut lea).await;
        let demande = demande_en_attente(&state, "soiree");
        repondre(&state, HOTE, HOTE_NOM, demande, false, "pas ce soir").unwrap();
        let refus = suivant(&mut lea).await;
        assert_eq!(refus["type"], "kicked", "{refus}");
        assert_eq!(refus["reason"], "pas ce soir");
        attendre_fermeture(&mut lea).await;
        assert_eq!(state.audit.recent(1)[0].action, "porte.refuse");
        assert!(repondre(&state, HOTE, HOTE_NOM, demande, true, "").is_err(), "une demande tranchée n'existe plus");
        // Refusée, son adresse est tenue à l'écart : refrapper tout de
        // suite, sous un autre nom, n'aboutit pas — ni demande, ni bannière.
        let ip_locale: IpAddr = "127.0.0.1".parse().unwrap();
        let mut encore = frapper_ws(addr, "soiree", "Lea2").await;
        let ralenti = suivant(&mut encore).await;
        assert_eq!(ralenti["type"], "error", "{ralenti}");
        assert!(ralenti["message"].as_str().unwrap().contains("réessaie dans"), "{ralenti}");
        attendre_fermeture(&mut encore).await;
        assert_eq!(state.portes.tableau()[0].demandes, 0);
        // L'ardoise effacée (comme après un quart d'heure de calme), on
        // peut refrapper.
        state.portes.throttle.record_success(ip_locale, &ip_locale.to_string());

        // Accepté, puis mis à la porte.
        let mut max = frapper_ws(addr, "soiree", "Max").await;
        suivant(&mut max).await;
        let demande = demande_en_attente(&state, "soiree");
        repondre(&state, HOTE, HOTE_NOM, demande, true, "").unwrap();
        for _ in 0..3 {
            suivant(&mut max).await; // historique, accueil, entrée
        }
        let invite_id = state.roster().into_iter().find(|m| m.invite).unwrap().user_id;
        assert_eq!(expulser(&state, 42, "intrus", invite_id).unwrap_err(), PERMISSION_REFUSEE);

        // Avant : lui offrir ki-chat. Une invitation à usage unique : le
        // lien posté dans le salon, le code poussé par sa porte à lui seul
        // — et rendu à qui l'offre.
        let code_rendu = offrir(&state, HOTE_NOM, invite_id).unwrap();
        let annonce = suivant(&mut max).await;
        let invitation = suivant(&mut max).await;
        // La diffusion du salon et l'envoi direct ne sont pas ordonnés
        // entre eux : on prend les deux dans l'ordre où ils arrivent.
        let (annonce, invitation) = if annonce["type"] == "chat" { (annonce, invitation) } else { (invitation, annonce) };
        assert_eq!(annonce["type"], "chat", "{annonce}");
        assert!(annonce["text"].as_str().unwrap().contains(PORTE_TELECHARGEMENT));
        assert_eq!(invitation["type"], "porte_invitation", "{invitation}");
        let code = invitation["code"].as_str().unwrap();
        assert!(code.starts_with("ki-"), "{code}");
        assert_eq!(code, code_rendu);
        assert_eq!(invitation["telechargement"], PORTE_TELECHARGEMENT);
        // Le salon est lu par tous les invités de la porte, des inconnus :
        // le code — un accès au serveur — n'y passe pas.
        let texte = annonce["text"].as_str().unwrap();
        assert!(!texte.contains(code), "le code d'invitation est dans le salon : {texte}");
        assert!(texte.contains("sur sa page"), "{texte}");
        let emise = state.accounts.invites().into_iter().find(|i| i.code == code).unwrap();
        assert_eq!(emise.uses_left, Some(1));
        assert!(emise.label.contains("Max (web)"));

        expulser(&state, HOTE, HOTE_NOM, invite_id).unwrap();
        let dehors = suivant(&mut max).await;
        assert_eq!(dehors["type"], "kicked", "{dehors}");
        assert!(dehors["reason"].as_str().unwrap().contains(HOTE_NOM));
        attendre_fermeture(&mut max).await;
        assert!(state.roster().iter().all(|m| !m.invite));
        assert_eq!(state.audit.recent(1)[0].action, "porte.kick");
        // Expulsé, son adresse est tenue à l'écart comme après un refus.
        assert!(state.portes.throttle.check(ip_locale, &ip_locale.to_string()).is_err(), "l'expulsé refrappe aussitôt");
        // Le salon a vu passer tout ça, et le salon lui-même est toujours là.
        let textes: Vec<String> = state.history.recent(salon, 10).into_iter().map(|r| r.text).collect();
        assert!(textes.iter().any(|t| t.starts_with("Max (web) est parti — mis à la porte")), "{textes:?}");
        assert!(state.portes.existe("soiree"));
        assert!(offrir(&state, HOTE_NOM, invite_id).is_err(), "parti, on ne lui offre plus rien");
    }

    /// Les plafonds : portes, demandes, une par adresse, invités, le
    /// limiteur, les noms.
    #[tokio::test]
    async fn les_plafonds_tiennent() {
        let state = etat("plafonds");
        let addr = servir(state.clone()).await;

        // Slugs : la règle du protocole, et l'unicité.
        assert!(ouvrir(&state, HOTE, HOTE_NOM, "Salon1", "", 0).is_err());
        assert!(ouvrir(&state, HOTE, HOTE_NOM, "ab", "", 0).is_err());
        ouvrir_ok(&state, "porte-1");
        assert!(ouvrir(&state, HOTE, HOTE_NOM, "porte-1", "", 0).unwrap_err().contains("déjà ouverte"));
        for n in 2..=PORTES_MAX {
            ouvrir_ok(&state, &format!("porte-{n}"));
        }
        let trop = ouvrir(&state, HOTE, HOTE_NOM, "porte-de-trop", "", 0).unwrap_err();
        assert!(trop.contains("au plus"), "{trop}");
        assert_eq!(state.channels.list().iter().filter(|c| c.expire_le.is_some()).count(), PORTES_MAX);
        // Une porte de trop n'a pas laissé de salon derrière elle.
        assert!(!state.channels.list().iter().any(|c| c.name == "porte-de-trop"));

        // Une demande par adresse à la fois : la seconde, depuis la même
        // adresse, est refusée sans toucher à la première.
        let mut premier = frapper_ws(addr, "porte-1", "Kevin").await;
        assert_eq!(suivant(&mut premier).await["type"], "info");
        let mut second = frapper_ws(addr, "porte-1", "Kevin2").await;
        let refus = suivant(&mut second).await;
        assert_eq!(refus["type"], "error", "{refus}");
        assert!(refus["message"].as_str().unwrap().contains("une demande à la fois"));
        attendre_fermeture(&mut second).await;
        assert_eq!(state.portes.tableau()[0].demandes, 1);

        // Les noms : vide, trop long, suffixe réservé, celui d'un membre,
        // celui du bot, un doublon à la porte.
        let (tx, _rx) = mpsc::channel(4);
        let (audio, _audio_rx) = mpsc::channel(4);
        let ip: IpAddr = "203.0.113.9".parse().unwrap();
        assert!(frapper(&state, "porte-2", "   ", ip, "127.0.0.1", tx.clone(), audio.clone()).is_err());
        assert!(frapper(&state, "porte-2", &"x".repeat(MAX_USERNAME + 1), ip, "127.0.0.1", tx.clone(), audio.clone()).is_err());
        assert!(frapper(&state, "porte-2", "Kevin (WEB)", ip, "127.0.0.1", tx.clone(), audio.clone()).is_err());
        assert!(frapper(&state, "porte-2", ki_protocol::MUSIQUE_NOM, ip, "127.0.0.1", tx.clone(), audio.clone()).is_err());
        assert!(frapper(&state, "porte-2", PSEUDO_PORTE, ip, "127.0.0.1", tx.clone(), audio.clone()).is_err());
        assert!(frapper(&state, "porte-2", "kevin", "203.0.113.10".parse().unwrap(), "127.0.0.1", tx.clone(), audio.clone()).is_ok(), "Kevin attend à porte-1, pas à porte-2 : le nom y est libre");
        assert!(frapper(&state, "porte-2", "kevin", "203.0.113.11".parse().unwrap(), "127.0.0.1", tx.clone(), audio.clone()).is_err(), "le même nom, casse ignorée, est pris à porte-2");
        assert!(frapper(&state, "nulle-part", "Ana", ip, "127.0.0.1", tx.clone(), audio.clone()).is_err());

        // Cinq demandes par porte, depuis cinq adresses, pas six.
        for n in 0..PORTE_DEMANDES_MAX {
            let ip: IpAddr = format!("198.51.100.{n}").parse().unwrap();
            frapper(&state, "porte-3", &format!("invite{n}"), ip, "127.0.0.1", tx.clone(), audio.clone()).unwrap();
        }
        let trop = frapper(&state, "porte-3", "encore", "198.51.100.99".parse().unwrap(), "127.0.0.1", tx.clone(), audio.clone()).unwrap_err();
        assert!(trop.contains("trop de demandes"), "{trop}");

        // Vingt invités par porte : la vingt-et-unième demande est refusée,
        // et une demande acceptée de trop aussi.
        for n in 0..PORTE_INVITES_MAX {
            let ip: IpAddr = format!("192.0.2.{n}").parse().unwrap();
            let (id, _) = frapper(&state, "porte-4", &format!("inv{n}"), ip, "127.0.0.1", tx.clone(), audio.clone()).unwrap();
            repondre(&state, HOTE, HOTE_NOM, id, true, "").unwrap();
        }
        let plein = frapper(&state, "porte-4", "de-trop", "192.0.2.200".parse().unwrap(), "127.0.0.1", tx.clone(), audio.clone()).unwrap_err();
        assert!(plein.contains("pleine"), "{plein}");
        assert_eq!(state.roster().iter().filter(|m| m.invite).count(), PORTE_INVITES_MAX);

        // Le limiteur par adresse : cinq demandes gratuites, puis un délai.
        let ip: IpAddr = "203.0.113.77".parse().unwrap();
        for n in 0..6 {
            let (_, invite_id) = frapper(&state, "porte-5", &format!("t{n}"), ip, "127.0.0.1", tx.clone(), audio.clone()).unwrap();
            depart(&state, invite_id);
        }
        let ralenti = frapper(&state, "porte-5", "t7", ip, "127.0.0.1", tx.clone(), audio.clone()).unwrap_err();
        assert!(ralenti.contains("réessaie dans"), "{ralenti}");
    }

    /// Le temps : une porte déserte ferme au bout de dix minutes, une
    /// demande sans réponse est congédiée au bout de cinq, et tout ferme à
    /// l'échéance — sans qu'un invité présent ne retienne la porte au-delà.
    #[tokio::test]
    async fn une_porte_deserte_expire_et_une_demande_sans_reponse_aussi() {
        let state = etat("temps");
        let addr = servir(state.clone()).await;
        let salon = ouvrir_ok(&state, "deserte");
        let depart_horloge = Instant::now();

        // Neuf minutes : rien ne bouge. Onze : fermée, salon effacé.
        tour_a(&state, now_millis(), depart_horloge + Duration::from_secs(9 * 60));
        assert!(state.portes.existe("deserte"));
        tour_a(&state, now_millis(), depart_horloge + Duration::from_secs(11 * 60));
        assert!(!state.portes.existe("deserte"));
        assert!(state.channels.get(salon).is_none());
        assert!(state.audit.recent(1)[0].detail.contains("plus personne"));

        // Une demande en attente retient la porte, mais pas au-delà de cinq
        // minutes : la page apprend que personne n'a répondu.
        let salon = ouvrir_ok(&state, "patiente");
        let mut kevin = frapper_ws(addr, "patiente", "Kevin").await;
        suivant(&mut kevin).await;
        tour_a(&state, now_millis(), depart_horloge + Duration::from_secs(4 * 60));
        assert!(state.portes.existe("patiente"));
        assert_eq!(state.portes.tableau()[0].demandes, 1);
        tour_a(&state, now_millis(), Instant::now() + DEMANDE_ATTENTE);
        let fin = suivant(&mut kevin).await;
        assert_eq!(fin["type"], "kicked", "{fin}");
        assert!(fin["reason"].as_str().unwrap().contains("personne n'a répondu"));
        attendre_fermeture(&mut kevin).await;
        assert!(state.portes.existe("patiente"), "la porte reste ouverte, la demande seule est retirée");
        assert_eq!(state.portes.tableau()[0].demandes, 0);

        // Un invité présent : la porte vit, jusqu'à l'échéance.
        let mut ana = frapper_ws(addr, "patiente", "Ana").await;
        suivant(&mut ana).await;
        let demande = demande_en_attente(&state, "patiente");
        repondre(&state, HOTE, HOTE_NOM, demande, true, "").unwrap();
        for _ in 0..3 {
            suivant(&mut ana).await;
        }
        tour_a(&state, now_millis(), Instant::now() + Duration::from_secs(60 * 60));
        assert!(state.portes.existe("patiente"), "un invité présent retient la porte");
        let expire_le = state.channels.get(salon).unwrap().expire_le.unwrap();
        tour_a(&state, expire_le, Instant::now());
        let adieu = suivant(&mut ana).await;
        assert_eq!(adieu["type"], "porte_fermee", "{adieu}");
        assert_eq!(adieu["motif"], "expirée");
        attendre_fermeture(&mut ana).await;
        assert!(!state.portes.existe("patiente"));
        assert!(state.channels.get(salon).is_none());
        assert!(state.roster().iter().all(|m| !m.invite));
    }

    /// Un invité qui ferme sa page est retiré, et son départ se lit dans
    /// le salon ; un premier message qui n'est pas `hello` ferme tout.
    #[tokio::test]
    async fn un_depart_se_lit_et_un_mauvais_debut_ferme() {
        let state = etat("depart");
        let addr = servir(state.clone()).await;
        let salon = ouvrir_ok(&state, "salon2");

        let mut ws = connecter(addr, "salon2").await;
        envoyer(&mut ws, &serde_json::json!({ "type": "chat", "text": "bonjour" })).await;
        let erreur = suivant(&mut ws).await;
        assert_eq!(erreur["type"], "error", "{erreur}");
        attendre_fermeture(&mut ws).await;
        // Une porte inconnue : pas de poignée de main du tout.
        assert!(tokio_tungstenite::connect_async(format!("ws://{addr}/s/inconnue/ws")).await.is_err());
        // La page d'un autre site : son origine n'est pas la nôtre, la
        // poignée de main est refusée avant toute session. La même origine
        // que l'hôte passe ; sans origine (un outil, pas un navigateur),
        // aussi — c'est le cas de toutes les autres connexions de ces tests.
        let mut piegee = format!("ws://{addr}/s/salon2/ws").into_client_request().unwrap();
        piegee.headers_mut().insert("Origin", "https://mechant.example".parse().unwrap());
        let refus = tokio_tungstenite::connect_async(piegee).await.unwrap_err();
        assert!(
            matches!(&refus, tokio_tungstenite::tungstenite::Error::Http(r) if r.status() == 403),
            "{refus:?}"
        );
        let mut notre = format!("ws://{addr}/s/salon2/ws").into_client_request().unwrap();
        notre.headers_mut().insert("Origin", format!("http://{addr}").parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(notre).await.unwrap();
        envoyer(&mut ws, &serde_json::json!({ "type": "hello", "nom": "Origine" })).await;
        assert_eq!(suivant(&mut ws).await["type"], "info");
        ws.close(None).await.unwrap();
        let demande = demande_en_attente(&state, "salon2");
        state.portes.retirer_demande(demande);

        let mut kevin = frapper_ws(addr, "salon2", "Kevin").await;
        suivant(&mut kevin).await;
        let demande = demande_en_attente(&state, "salon2");
        repondre(&state, HOTE, HOTE_NOM, demande, true, "").unwrap();
        for _ in 0..3 {
            suivant(&mut kevin).await;
        }
        assert_eq!(state.roster().iter().filter(|m| m.invite).count(), 1);
        kevin.close(None).await.unwrap();
        // La tâche du serveur remarque la fermeture et retire l'invité.
        for _ in 0..50 {
            if state.roster().iter().all(|m| !m.invite) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(state.roster().iter().all(|m| !m.invite), "parti, il n'est plus listé");
        let dernier = state.history.recent(salon, 1).pop().unwrap();
        assert_eq!(dernier.text, "Kevin (web) est parti");
        assert_eq!(state.portes.tableau()[0].invites, 0);
    }

    /// La page : servie sous les deux formes du lien avec ses en-têtes de
    /// sécurité, 404 sans porte ; et la politique ne laisse passer que les
    /// blocs de la page, reconnus à leur empreinte.
    #[tokio::test]
    async fn la_page_est_servie_avec_ses_en_tetes() {
        let state = etat("page");
        let addr = servir(state.clone()).await;
        assert_eq!(http_get(addr, "/s/salon1").await.0, 404);
        assert_eq!(http_get(addr, "/salon1").await.0, 404);
        state.meta.set_name("Les <Baws> & co").unwrap();
        ouvrir_ok(&state, "salon1");
        for chemin in ["/s/salon1", "/salon1"] {
            let (statut, reponse) = http_get(addr, chemin).await;
            assert_eq!(statut, 200, "{chemin}");
            let bas = reponse.to_lowercase();
            assert!(bas.contains("content-security-policy: default-src 'none'"), "{chemin} : {reponse}");
            assert!(!bas.contains("unsafe-inline"));
            assert!(bas.contains("x-frame-options: deny"));
            assert!(bas.contains("referrer-policy: no-referrer"));
            assert!(bas.contains("x-content-type-options: nosniff"));
            assert!(bas.contains("cache-control: no-store"));
            assert!(bas.contains("content-type: text/html; charset=utf-8"));
            assert!(reponse.contains("<html"), "la page elle-même");
            // Le nom du serveur, échappé ; plus de gabarit.
            assert!(reponse.contains("content=\"Les &lt;Baws&gt; &amp; co\""), "{reponse}");
            assert!(!reponse.contains("{{serveur}}"));
        }
        // La feuille et le script, à côté, sous le même toit.
        let (statut, css) = http_get(addr, "/s/porte.css").await;
        assert_eq!(statut, 200);
        assert!(css.to_lowercase().contains("content-type: text/css; charset=utf-8"));
        assert!(css.contains(":root"));
        let (statut, js) = http_get(addr, "/s/porte.js").await;
        assert_eq!(statut, 200);
        assert!(js.to_lowercase().contains("content-type: text/javascript; charset=utf-8"));
        assert!(js.to_lowercase().contains("x-content-type-options: nosniff"));
        assert!(js.contains("use strict"));
        // Un slug qui n'en est pas un ne touche pas la table.
        assert_eq!(http_get(addr, "/Salon1").await.0, 404);
        assert_eq!(http_get(addr, "/s/salon1/ws").await.0, 400, "sans poignée de main WebSocket : refusé");
        assert_eq!(http_get(addr, "/salon1/ws").await.0, 400, "la forme courte a aussi sa WebSocket");

        // La page se lit avec ce que le script attend d'elle : chaque
        // élément qu'il va chercher est là, une seule fois.
        for id in [
            "nom-serveur", "nom-porte", "statut", "etat-nom", "etat-attente", "etat-refus", "etat-salon",
            "etat-ferme", "form-nom", "prenom", "apercu-nom", "erreur-nom", "bouton-demander", "attente-info",
            "attente-compteur", "bouton-annuler", "refus-motif", "bouton-reessayer", "presence-liste", "bandeau",
            "bandeau-texte", "bandeau-fermer", "fil", "form-saisie", "texte", "compteur-texte", "debit",
            "bouton-envoyer", "bouton-vocal", "vocal-controles", "vocal-parler", "vocal-sourdine", "vocal-volume",
            "vocal-quitter", "ferme-motif", "carte-invitation", "invitation-serveur", "invitation-code",
            "invitation-telechargement", "invitation-replier",
        ] {
            assert_eq!(PAGE.matches(&format!("id=\"{id}\"")).count(), 1, "élément {id}");
        }
        for id in SCRIPT.split("$('").skip(1).filter_map(|s| s.split('\'').next()) {
            assert!(PAGE.contains(&format!("id=\"{id}\"")), "le script cherche #{id}, absent de la page");
        }
        assert!(PAGE.contains("href=\"/s/porte.css\"") && PAGE.contains("src=\"/s/porte.js\""));
        // Pas de gestionnaire d'événement en attribut (` onclick="…"`) : la
        // CSP le refuserait. « onglet » dans une phrase, lui, ne gêne pas.
        let attribut_on = PAGE.split(" on").skip(1).any(|s| {
            let lettres = s.chars().take_while(|c| c.is_ascii_alphabetic()).count();
            lettres > 0 && s[lettres..].starts_with('=')
        });
        assert!(!attribut_on, "un gestionnaire d'événement en attribut dans la page");

        // Les empreintes : un bloc, une empreinte, rien d'autre.
        let page = "<html><style>a{}</style><scripts>non</scripts><script type=\"module\">x()</script><script>y()</script><script src=\"/s/porte.js\"></script></html>";
        assert_eq!(blocs(page, "script"), vec!["x()", "y()"]);
        assert_eq!(blocs(page, "style"), vec!["a{}"]);
        let csp = csp_de(page);
        let empreinte = |s: &str| format!("'sha256-{}'", base64(&Sha256::digest(s.as_bytes())));
        assert!(csp.contains(&format!("script-src 'self' {} {};", empreinte("x()"), empreinte("y()"))), "{csp}");
        assert!(csp.contains(&format!("style-src 'self' {};", empreinte("a{}"))), "{csp}");
        assert!(csp_de("<html></html>").contains("script-src 'self'; style-src 'self';"));
        // La page embarquée n'a rien en ligne : rien à hacher.
        assert!(CSP.contains("script-src 'self'; style-src 'self';"), "{}", *CSP);
        assert_eq!(echapper("a<b>&\"c'"), "a&lt;b&gt;&amp;&quot;c&#39;");
        // Base64 : les vecteurs de la RFC 4648.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    /// La voix descendante : amené en vocal, un invité entend son salon —
    /// membres et bot, déchiffrés pour lui — et rien d'un autre, rien de
    /// forgé ; sorti, plus rien.
    #[tokio::test]
    async fn un_invite_en_vocal_entend_son_salon_et_lui_seul() {
        let state = etat("vocal-descend");
        let addr = servir(state.clone()).await;
        let salon = ouvrir_ok(&state, "salon1");
        let (mut kevin, kevin_id) = entrer(&state, addr, "salon1", "Kevin").await;
        let vocal = salon_vocal(&state, "Vocal");
        let autre = salon_vocal(&state, "Autre");

        // Qui peut, et où : ni un intrus, ni un salon textuel, ni un salon
        // vocal où l'hôte n'est pas lui-même ; et pas de sortie sans entrée.
        assert_eq!(vocal_depuis(&state, 42, "intrus", kevin_id, Some(vocal), Some(vocal)).unwrap_err(), PERMISSION_REFUSEE);
        assert!(vocal_depuis(&state, HOTE, HOTE_NOM, kevin_id, Some(salon), Some(salon)).unwrap_err().contains("inconnu"));
        assert!(vocal_depuis(&state, HOTE, HOTE_NOM, kevin_id, Some(vocal), None).unwrap_err().contains("entre d'abord"));
        assert!(vocal_depuis(&state, HOTE, HOTE_NOM, kevin_id, Some(vocal), Some(autre)).is_err());
        assert!(vocal_depuis(&state, HOTE, HOTE_NOM, kevin_id, None, None).unwrap_err().contains("pas en vocal"));
        // Le vrai chemin lit le salon vocal de l'acteur chez les connectés :
        // l'hôte de ces tests n'y est pas.
        assert!(super::vocal(&state, HOTE, HOTE_NOM, kevin_id, Some(vocal)).is_err());
        assert!(vocal_depuis(&state, HOTE, HOTE_NOM, INVITE_ID_BASE + 999, Some(vocal), Some(vocal)).is_err());
        assert!(!state.portes.ecoute(vocal));
        // Une page qui se croit en vocal sans y avoir été amenée — reprise
        // après une coupure — en est sortie tout de suite ; en quitter un
        // où l'on n'est pas ne dit rien.
        envoyer(&mut kevin, &serde_json::json!({ "type": "vocal", "actif": true })).await;
        assert_eq!(suivant(&mut kevin).await["type"], "porte_vocal_fin");
        envoyer(&mut kevin, &serde_json::json!({ "type": "vocal", "actif": false })).await;
        rien_en_texte(&mut kevin).await;

        // Amené : sa page l'apprend, avec le nom du salon. Tant qu'elle n'y
        // est pas entrée, il n'écoute rien, et ni le roster ni l'état de la
        // porte ne le mettent dans le vocal.
        vocal_depuis(&state, HOTE, HOTE_NOM, kevin_id, Some(vocal), Some(vocal)).unwrap();
        let annonce = suivant(&mut kevin).await;
        assert_eq!(annonce["type"], "porte_vocal", "{annonce}");
        assert_eq!(annonce["channel"], vocal);
        assert_eq!(annonce["nom_salon"], "Vocal");
        // Qui est là : personne encore — aucun membre n'est connecté dans
        // ces tests, et lui-même n'a pas cliqué.
        assert_eq!(annonce["occupants"], serde_json::json!([]));
        let fil = suivant(&mut kevin).await;
        assert!(fil["text"].as_str().unwrap().contains("est en vocal dans « Vocal »"), "{fil}");
        assert_eq!(voix_de(&state, kevin_id), None);
        assert!(!state.portes.ecoute(vocal));
        assert_eq!(state.audit.recent(1)[0].action, "porte.vocal");
        let (etat, _) = state.portes.etat("salon1").unwrap();
        let ServerMsg::PorteEtat { invites, .. } = etat else { panic!("{etat:?}") };
        assert_eq!(invites[0].vocal, None);
        // Amené une seconde fois au même endroit : la page est relancée,
        // rien d'autre — ni annonce dans le fil, ni audit.
        vocal_depuis(&state, HOTE, HOTE_NOM, kevin_id, Some(vocal), Some(vocal)).unwrap();
        assert_eq!(suivant(&mut kevin).await["type"], "porte_vocal");
        rien_en_texte(&mut kevin).await;

        // Sa page entre : la liste des occupants lui revient, avec lui —
        // c'est ce qui lui permet de dire « toi » — ; le roster et l'état de
        // la porte le placent dans le vocal.
        envoyer(&mut kevin, &serde_json::json!({ "type": "vocal", "actif": true })).await;
        let occupants = suivant(&mut kevin).await;
        assert_eq!(occupants["type"], "porte_vocal_occupants", "{occupants}");
        assert_eq!(occupants["occupants"], serde_json::json!([{ "id": kevin_id, "nom": "Kevin (web)" }]));
        assert_eq!(voix_de(&state, kevin_id), Some(vocal));
        assert!(state.portes.ecoute(vocal) && !state.portes.ecoute(autre));
        let (etat, _) = state.portes.etat("salon1").unwrap();
        let ServerMsg::PorteEtat { invites, .. } = etat else { panic!("{etat:?}") };
        assert_eq!(invites[0].vocal, Some(vocal));
        // Le redire ne change rien.
        envoyer(&mut kevin, &serde_json::json!({ "type": "vocal", "actif": true })).await;
        rien_en_texte(&mut kevin).await;

        // Un membre parle dans le vocal : sa trame arrive à la page en
        // clair, précédée de qui parle et du compteur.
        let cle = state.voice_key;
        state.portes.relayer(vocal, &cle, &paquet_de_membre(&cle, 42, 7, b"bonjour"));
        assert_eq!(suivant_bin(&mut kevin).await, attendu(42, 7, b"bonjour"));
        // Le bot musique aussi.
        state.portes.relayer(vocal, &cle, &paquet_de_membre(&cle, ki_protocol::MUSIQUE_ID, 100, b"la-la"));
        assert_eq!(suivant_bin(&mut kevin).await, attendu(ki_protocol::MUSIQUE_ID, 100, b"la-la"));

        // Ce qui ne passe pas : un paquet altéré, un paquet d'une autre clé,
        // un paquet d'un autre salon, un paquet qui n'en est pas un. Le bon
        // paquet qui suit est le prochain reçu : les autres sont tombés.
        let mut altere = paquet_de_membre(&cle, 42, 8, b"bonjour");
        let dernier = altere.len() - 1;
        altere[dernier] ^= 0x55;
        state.portes.relayer(vocal, &cle, &altere);
        state.portes.relayer(vocal, &cle, &paquet_de_membre(&[7u8; 32], 42, 9, b"autre cle"));
        state.portes.relayer(autre, &cle, &paquet_de_membre(&cle, 43, 1, b"autre salon"));
        state.portes.relayer(vocal, &cle, b"KV\x02pas un paquet");
        state.portes.relayer(vocal, &cle, &paquet_de_membre(&cle, 44, 10, b"le bon"));
        assert_eq!(suivant_bin(&mut kevin).await, attendu(44, 10, b"le bon"));

        // Sa page quitte le vocal (« Quitter ») : plus de son, plus dans le
        // roster — mais toujours autorisé, le bouton lui reste ; elle y
        // revient d'elle-même.
        envoyer(&mut kevin, &serde_json::json!({ "type": "vocal", "actif": false })).await;
        rien_en_texte(&mut kevin).await;
        assert_eq!(voix_de(&state, kevin_id), None);
        assert!(!state.portes.ecoute(vocal));
        state.portes.relayer(vocal, &cle, &paquet_de_membre(&cle, 42, 11, b"dans le vide"));
        rien_en_binaire(&mut kevin).await;
        // Et sa voix ne passe pas non plus : on n'entend que qui se voit
        // dans le vocal — une page sortie qui émettrait encore parlerait
        // sans que personne sache d'où.
        assert_eq!(state.portes.emettre(kevin_id, 1), Err("pas en vocal"));
        envoyer(&mut kevin, &serde_json::json!({ "type": "vocal", "actif": true })).await;
        assert_eq!(suivant(&mut kevin).await["type"], "porte_vocal_occupants");
        assert_eq!(voix_de(&state, kevin_id), Some(vocal));
        state.portes.relayer(vocal, &cle, &paquet_de_membre(&cle, 42, 12, b"de retour"));
        assert_eq!(suivant_bin(&mut kevin).await, attendu(42, 12, b"de retour"));

        // Sorti par un membre : sa page l'apprend, la liste le remet hors
        // vocal, et plus rien ne lui parvient du salon — même si sa page,
        // en retard, dit encore qu'elle y est.
        vocal_depuis(&state, HOTE, HOTE_NOM, kevin_id, None, None).unwrap();
        let sortie = suivant(&mut kevin).await;
        assert_eq!(sortie["type"], "porte_vocal_fin", "{sortie}");
        let fil = suivant(&mut kevin).await;
        assert!(fil["text"].as_str().unwrap().contains("sort du vocal"), "{fil}");
        assert_eq!(voix_de(&state, kevin_id), None);
        assert!(!state.portes.ecoute(vocal));
        state.portes.relayer(vocal, &cle, &paquet_de_membre(&cle, 42, 13, b"trop tard"));
        rien_en_binaire(&mut kevin).await;
        envoyer(&mut kevin, &serde_json::json!({ "type": "vocal", "actif": true })).await;
        assert_eq!(suivant(&mut kevin).await["type"], "porte_vocal_fin");
        assert!(!state.portes.ecoute(vocal));
    }

    /// La voix montante : la trame d'un invité est emballée comme celle d'un
    /// membre — un client la déchiffrerait —, partagée aux autres invités du
    /// même vocal et à eux seuls, bornée en débit, et refusée hors vocal.
    #[tokio::test]
    async fn la_voix_d_un_invite_est_chiffree_comme_celle_d_un_membre() {
        // L'emballage seul : ce qu'un client ferait de ce paquet.
        let cle = [3u8; 32];
        let chiffre = XChaCha20Poly1305::new((&cle).into());
        let id = INVITE_ID_BASE + 3;
        let paquet = emballer(&chiffre, id, 99, b"opus").unwrap();
        let pkt = parse_voice_packet(&paquet).unwrap();
        assert_eq!((pkt.id, pkt.counter), (id, 99));
        assert_eq!(chiffre.decrypt(&nonce_voix(id, 99), pkt.payload).unwrap(), b"opus");
        assert!(chiffre.decrypt(&nonce_voix(id + 1, 99), pkt.payload).is_err(), "un autre émetteur, un autre nonce");
        assert!(chiffre.decrypt(&nonce_voix(id, 98), pkt.payload).is_err());
        // Les trames de la page.
        assert_eq!(trame_montante(&montante(5, b"x")), Some((5, &b"x"[..])));
        assert_eq!(trame_montante(&montante(5, &[0u8; OPUS_MAX])).map(|(c, o)| (c, o.len())), Some((5, OPUS_MAX)));
        assert!(trame_montante(&montante(5, &[0u8; OPUS_MAX + 1])).is_none(), "trop long");
        assert!(trame_montante(&montante(5, b"")).is_none(), "vide");
        let mut mauvaise_version = montante(5, b"x");
        mauvaise_version[0] = 2;
        assert!(trame_montante(&mauvaise_version).is_none());
        assert_eq!(attendu(7, 8, b"ab"), [&[1u8][..], &7u64.to_le_bytes(), &8u64.to_le_bytes(), b"ab"].concat());

        let state = etat("vocal-monte");
        let addr = servir(state.clone()).await;
        ouvrir_ok(&state, "salon1");
        ouvrir_ok(&state, "salon2");
        let (mut kevin, kevin_id) = entrer(&state, addr, "salon1", "Kevin").await;
        let (mut lea, lea_id) = entrer(&state, addr, "salon1", "Léa").await;
        let (mut max, max_id) = entrer(&state, addr, "salon2", "Max").await;
        let vocal = salon_vocal(&state, "Vocal");
        let autre = salon_vocal(&state, "Autre");

        // Hors vocal, une trame tombe sans un mot, et la session continue
        // — le pong revient, derrière l'arrivée de Léa dans le fil.
        kevin.send(Trame::binary(montante(1, b"trop tot"))).await.unwrap();
        envoyer(&mut kevin, &serde_json::json!({ "type": "ping" })).await;
        assert_eq!(jusqu_au(&mut kevin, "pong").await["type"], "pong");

        // Kevin, amené puis entré ; Léa, amenée ensuite, trouve Kevin dans
        // son `porte_vocal`, et son entrée revient à tous deux avec la liste
        // entière ; Max, ailleurs, n'en sait rien.
        vocal_depuis(&state, HOTE, HOTE_NOM, kevin_id, Some(vocal), Some(vocal)).unwrap();
        entre(&mut kevin).await;
        let occupants = jusqu_au(&mut kevin, "porte_vocal_occupants").await;
        assert_eq!(occupants["occupants"], serde_json::json!([{ "id": kevin_id, "nom": "Kevin (web)" }]));
        vocal_depuis(&state, HOTE, HOTE_NOM, lea_id, Some(vocal), Some(vocal)).unwrap();
        let entree = jusqu_au(&mut lea, "porte_vocal").await;
        assert_eq!(entree["occupants"], serde_json::json!([{ "id": kevin_id, "nom": "Kevin (web)" }]));
        entre(&mut lea).await;
        let tous = serde_json::json!([{ "id": kevin_id, "nom": "Kevin (web)" }, { "id": lea_id, "nom": "Léa (web)" }]);
        assert_eq!(jusqu_au(&mut kevin, "porte_vocal_occupants").await["occupants"], tous);
        assert_eq!(jusqu_au(&mut lea, "porte_vocal_occupants").await["occupants"], tous);
        vocal_depuis(&state, HOTE, HOTE_NOM, max_id, Some(autre), Some(autre)).unwrap();
        entre(&mut max).await;
        let entree = jusqu_au(&mut max, "porte_vocal").await;
        assert_eq!(entree["occupants"], serde_json::json!([]));
        let occupants = jusqu_au(&mut max, "porte_vocal_occupants").await;
        assert_eq!(occupants["occupants"], serde_json::json!([{ "id": max_id, "nom": "Max (web)" }]));
        assert_eq!(voix_de(&state, kevin_id), Some(vocal));
        assert_eq!(voix_de(&state, lea_id), Some(vocal));
        assert_eq!(voix_de(&state, max_id), Some(autre));
        // Les identifiants d'invités, tels que la page les lit : distincts
        // une fois passés par un double.
        assert_ne!(kevin_id as f64, lea_id as f64);
        assert_eq!(kevin_id as f64 as u64, kevin_id);

        // Kevin parle : Léa l'entend, sous son identifiant ; Max, dans un
        // autre vocal, non ; Kevin ne s'entend pas lui-même.
        kevin.send(Trame::binary(montante(10, b"salut"))).await.unwrap();
        let recu = suivant_bin(&mut lea).await;
        assert_eq!(recu[0], VOCAL_VERSION);
        assert_eq!(u64::from_le_bytes(recu[1..9].try_into().unwrap()), kevin_id);
        let premier = u64::from_le_bytes(recu[9..17].try_into().unwrap());
        assert_eq!(&recu[17..], b"salut");
        // Le compteur part d'un tirage : le seul vrai compteur est celui de
        // la page, et ses trous se retrouvent chez qui écoute.
        kevin.send(Trame::binary(montante(11, b"ca va"))).await.unwrap();
        assert_eq!(suivant_bin(&mut lea).await, attendu(kevin_id, premier + 1, b"ca va"));
        // Un compteur qui recule ou stagne : ignoré — un nonce ne resserve
        // pas. Puis un saut : le trou est gardé.
        kevin.send(Trame::binary(montante(11, b"rejoue"))).await.unwrap();
        kevin.send(Trame::binary(montante(3, b"en arriere"))).await.unwrap();
        let mut mauvaise_version = montante(12, b"v2");
        mauvaise_version[0] = 2;
        kevin.send(Trame::binary(mauvaise_version)).await.unwrap();
        kevin.send(Trame::binary(montante(12, &[0u8; OPUS_MAX + 1]))).await.unwrap();
        kevin.send(Trame::binary(montante(15, b"apres le trou"))).await.unwrap();
        assert_eq!(suivant_bin(&mut lea).await, attendu(kevin_id, premier + 5, b"apres le trou"));
        rien_en_binaire(&mut kevin).await;
        rien_en_binaire(&mut max).await;
        // Et dans l'autre sens : Léa parle, Kevin l'entend.
        lea.send(Trame::binary(montante(1, b"coucou"))).await.unwrap();
        let recu = suivant_bin(&mut kevin).await;
        assert_eq!(u64::from_le_bytes(recu[1..9].try_into().unwrap()), lea_id);
        assert_eq!(&recu[17..], b"coucou");

        // Le budget : cinquante trames par seconde, cent vingt en rafale —
        // quelques-unes ont déjà servi, quelques-unes sont revenues.
        let ok = (100u64..400).filter(|n| state.portes.emettre(kevin_id, *n).is_ok()).count();
        assert!((110..=125).contains(&ok), "{ok} trames passées sur 300");
        assert_eq!(state.portes.emettre(kevin_id, 400), Err("trop de trames"));
        // Max, à l'autre porte et dans l'autre vocal : sa trame va là-bas,
        // sous le compteur du serveur — celui de la page, décalé du tirage.
        let (salon_max, base) = state.portes.emettre(max_id, 0).unwrap();
        assert_eq!(salon_max, autre);
        assert_eq!(state.portes.emettre(max_id, 1), Ok((autre, base.wrapping_add(1))));

        // Expulsé, Kevin n'écoute plus — et Léa, restée seule, l'apprend ;
        // la porte fermée, plus personne.
        expulser(&state, HOTE, HOTE_NOM, kevin_id).unwrap();
        assert!(state.portes.ecoute(vocal), "Léa écoute encore");
        let seule = jusqu_au(&mut lea, "porte_vocal_occupants").await;
        assert_eq!(seule["occupants"], serde_json::json!([{ "id": lea_id, "nom": "Léa (web)" }]));
        // Une liste qui n'a pas bougé n'est pas répétée.
        annoncer_occupants(&state);
        rien_en_texte(&mut lea).await;
        assert!(state.portes.derniers_occupants.lock().unwrap().contains_key(&vocal));
        lea.send(Trame::binary(montante(2, b"seule"))).await.unwrap();
        rien_en_binaire(&mut max).await;
        fermer(&state, Some((HOTE, HOTE_NOM)), "salon1", "fermée").unwrap();
        assert!(!state.portes.ecoute(vocal));
        assert!(state.portes.ecoute(autre), "Max, à l'autre porte, écoute toujours");
        fermer(&state, Some((HOTE, HOTE_NOM)), "salon2", "fermée").unwrap();
        assert!(!state.portes.ecoute(autre));
        assert!(state.portes.ecoutes.read().unwrap().is_empty());
    }

    /// Un salon vocal supprimé sous les pieds d'un invité l'en sort, comme
    /// un membre, par la remise d'aplomb du serveur.
    #[tokio::test]
    async fn un_salon_vocal_supprime_sort_l_invite() {
        let state = etat("vocal-supprime");
        let addr = servir(state.clone()).await;
        ouvrir_ok(&state, "salon1");
        let (mut kevin, kevin_id) = entrer(&state, addr, "salon1", "Kevin").await;
        let vocal = salon_vocal(&state, "Vocal");
        vocal_depuis(&state, HOTE, HOTE_NOM, kevin_id, Some(vocal), Some(vocal)).unwrap();
        suivant(&mut kevin).await;
        suivant(&mut kevin).await;
        envoyer(&mut kevin, &serde_json::json!({ "type": "vocal", "actif": true })).await;
        assert_eq!(suivant(&mut kevin).await["type"], "porte_vocal_occupants");
        assert!(state.portes.ecoute(vocal));
        state.channels.delete(&state.data_dir, vocal).unwrap();
        state.reconcile_memberships();
        let sortie = suivant(&mut kevin).await;
        assert_eq!(sortie["type"], "porte_vocal_fin", "{sortie}");
        assert_eq!(voix_de(&state, kevin_id), None);
        assert!(!state.portes.ecoute(vocal));
        let fil = suivant(&mut kevin).await;
        assert!(fil["text"].as_str().unwrap().contains("n'existe plus"), "{fil}");
    }

    /// Entrer dans le vocal a un budget : chaque entrée rediffuse le roster
    /// à tous les connectés, et une page qui bascule en boucle ne doit pas
    /// les faire pleuvoir. Sortir, non : on ne retient personne.
    #[tokio::test]
    async fn basculer_le_vocal_en_boucle_ne_rediffuse_pas_le_roster() {
        let state = etat("vocal-budget");
        let addr = servir(state.clone()).await;
        ouvrir_ok(&state, "salon1");
        let (_kevin, kevin_id) = entrer(&state, addr, "salon1", "Kevin").await;
        let vocal = salon_vocal(&state, "Vocal");
        vocal_depuis(&state, HOTE, HOTE_NOM, kevin_id, Some(vocal), Some(vocal)).unwrap();
        let (mut entrees, mut sorties) = (0, 0);
        for _ in 0..50 {
            if state.portes.ecouter(kevin_id, true) == Some(true) {
                entrees += 1;
            }
            if state.portes.ecouter(kevin_id, false) == Some(true) {
                sorties += 1;
            }
        }
        assert_eq!((entrees, sorties), (4, 4), "quatre bascules d'un coup, pas cinquante");
        assert_eq!(voix_de(&state, kevin_id), None);
        assert!(!state.portes.ecoute(vocal));
        // Refusée, l'entrée ne change rien — ni réponse, ni roster — et
        // la page reste autorisée : dans une seconde, elle pourra.
        assert_eq!(state.portes.ecouter(kevin_id, true), Some(false));
        assert_eq!(state.portes.ecouter(kevin_id, false), Some(false), "sortir quand on est sorti ne change rien");
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(state.portes.ecouter(kevin_id, true), Some(true), "une par seconde en régime");
        assert_eq!(voix_de(&state, kevin_id), Some(vocal));
    }

    /// Déplacé d'un vocal à un autre, un invité peut repartir de zéro sans
    /// qu'un nonce resserve : la base avance au-delà de ce qu'il a consommé.
    #[tokio::test]
    async fn changer_de_vocal_ne_ressert_pas_un_nonce() {
        let state = etat("vocal-nonce");
        let addr = servir(state.clone()).await;
        ouvrir_ok(&state, "salon1");
        let (mut kevin, kevin_id) = entrer(&state, addr, "salon1", "Kevin").await;
        let vocal = salon_vocal(&state, "Vocal");
        let autre = salon_vocal(&state, "Autre");
        vocal_depuis(&state, HOTE, HOTE_NOM, kevin_id, Some(vocal), Some(vocal)).unwrap();
        entre(&mut kevin).await;
        jusqu_au(&mut kevin, "porte_vocal_occupants").await;
        let (_, premier) = state.portes.emettre(kevin_id, 1).unwrap();
        assert_eq!(state.portes.emettre(kevin_id, 2), Ok((vocal, premier.wrapping_add(1))));
        assert_eq!(state.portes.emettre(kevin_id, 1000), Ok((vocal, premier.wrapping_add(999))));
        assert_eq!(state.portes.emettre(kevin_id, 1), Err("compteur qui recule"));
        // Déplacé : sa page suit, et le compteur 1 — déjà servi — donne un
        // nonce jamais vu, au-delà du dernier consommé.
        vocal_depuis(&state, HOTE, HOTE_NOM, kevin_id, Some(autre), Some(autre)).unwrap();
        assert_eq!(voix_de(&state, kevin_id), Some(autre));
        let (salon, neuf) = state.portes.emettre(kevin_id, 1).unwrap();
        assert_eq!(salon, autre);
        assert!(neuf > premier.wrapping_add(999), "{neuf} ne dépasse pas {}", premier.wrapping_add(999));
        assert_eq!(neuf, premier.wrapping_add(1001));
        assert_eq!(state.portes.emettre(kevin_id, 0), Err("compteur qui recule"));
        assert_eq!(state.portes.emettre(kevin_id, 2), Ok((autre, neuf.wrapping_add(1))));
    }

    /// Un admin supprime le salon d'une porte : c'est la porte qui ferme —
    /// invités congédiés, journal effacé et non archivé, lien mort. Et si le
    /// salon disparaissait par un autre chemin, le tour d'horloge ferme la
    /// porte plutôt que de la laisser ouverte sur rien.
    #[tokio::test]
    async fn supprimer_le_salon_d_une_porte_la_ferme() {
        let state = etat("salon-supprime");
        let addr = servir(state.clone()).await;
        let salon = ouvrir_ok(&state, "salon1");
        let (mut kevin, _) = entrer(&state, addr, "salon1", "Kevin").await;
        assert!(fermer_salon(&state, "admin", 999_999).is_none(), "un salon sans porte n'est pas notre affaire");
        assert_eq!(state.portes.slug_du_salon(salon).as_deref(), Some("salon1"));
        assert_eq!(state.portes.slug_du_salon(999_999), None);

        fermer_salon(&state, "admin", salon).unwrap().unwrap();
        let adieu = suivant(&mut kevin).await;
        assert_eq!(adieu["type"], "porte_fermee", "{adieu}");
        assert!(adieu["motif"].as_str().unwrap().contains("supprimé par admin"), "{adieu}");
        attendre_fermeture(&mut kevin).await;
        assert!(!state.portes.existe("salon1"));
        assert!(state.channels.get(salon).is_none());
        assert!(state.roster().iter().all(|m| !m.invite));
        // Ni journal, ni archive : ce que des inconnus ont écrit ne reste pas.
        let prefixe = format!("channel-{salon}.");
        let restes: Vec<String> = std::fs::read_dir(&state.data_dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(&prefixe))
            .collect();
        assert!(restes.is_empty(), "{restes:?}");
        let audit = &state.audit.recent(1)[0];
        assert_eq!((audit.action.as_str(), audit.actor.as_str()), ("porte.close", "admin"));
        assert_eq!(http_get(addr, "/salon1").await.0, 404, "le lien est mort");

        // Le filet : le salon parti sans la porte, le tour suivant la ferme.
        let salon = ouvrir_ok(&state, "salon2");
        state.effacer_salon(salon).unwrap();
        tour_a(&state, now_millis(), Instant::now());
        assert!(!state.portes.existe("salon2"));
        assert!(state.audit.recent(1)[0].detail.contains("salon disparu"));
    }

    #[test]
    fn les_petites_fonctions() {
        // L'origine d'une WebSocket : absente, on passe ; présente, c'est
        // la nôtre ou rien.
        assert!(origine_admise(None, None, None));
        assert!(origine_admise(None, Some("ts.baws.fun"), Some("https://ts.baws.fun")));
        let base = Some("https://ts.baws.fun");
        assert!(origine_admise(Some("https://ts.baws.fun"), None, base));
        assert!(origine_admise(Some("https://ts.baws.fun/"), Some("mechant.example"), base));
        assert!(origine_admise(Some("HTTPS://TS.BAWS.FUN"), None, base));
        assert!(origine_admise(Some("https://ts.baws.fun"), None, Some("https://ts.baws.fun/portes")));
        assert!(origine_admise(Some("https://ts.baws.fun:8080"), None, Some("https://ts.baws.fun:8080")));
        assert!(!origine_admise(Some("https://mechant.example"), Some("ts.baws.fun"), base));
        assert!(!origine_admise(Some("http://ts.baws.fun"), None, base), "pas le même schéma");
        assert!(!origine_admise(Some("https://ts.baws.fun:8080"), None, base), "pas le même port");
        assert!(!origine_admise(Some("https://ts.baws.fun.mechant.example"), None, base));
        assert!(!origine_admise(Some("null"), None, base));
        // Sans KI_PUBLIC_URL : l'hôte de la requête fait foi.
        assert!(origine_admise(Some("https://ts.baws.fun:8080"), Some("ts.baws.fun:8080"), None));
        assert!(origine_admise(Some("http://127.0.0.1:18080"), Some("127.0.0.1:18080"), None));
        assert!(!origine_admise(Some("https://ts.baws.fun"), Some("ts.baws.fun:8080"), None));
        assert!(!origine_admise(Some("https://mechant.example"), Some("ts.baws.fun"), None));
        assert!(!origine_admise(Some("null"), Some("ts.baws.fun"), None));
        assert!(!origine_admise(Some("https://mechant.example"), None, None), "sans hôte non plus");
        assert!(!origine_admise(Some("ts.baws.fun"), Some("ts.baws.fun"), None), "une origine a un schéma");

        assert_eq!(ip_masquee("82.65.12.34".parse().unwrap()), "82.65.x.x");
        assert_eq!(ip_masquee("2001:db8::1".parse().unwrap()), "2001:db8:…");
        assert_eq!(nom_propre("  Kevin   Dupont ").unwrap(), "Kevin Dupont");
        assert_eq!(nom_propre("Ke\u{202e}vin").unwrap(), "Kevin");
        assert!(nom_propre("\t\n").is_err());
        assert!(nom_propre("moi (web)").is_err());
        assert_eq!(texte_de(&Line::from(&b"{\"a\":1}\n"[..])), "{\"a\":1}");
    }
}
