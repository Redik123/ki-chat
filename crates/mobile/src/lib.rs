//! ki-chat sur téléphone (Tauri 2).
//!
//! Le cœur est celui du PC, dans ki-core : la connexion et la voix
//! ([`ki_core::net`]), l'état du client ([`ki_core::etat`]). Ce module ne fait
//! que relier ce cœur à la page :
//!
//! - les gestes de la page arrivent par des commandes (`connecter`,
//!   `ouvrir_salon`, `envoyer`, `rejoindre_vocal`…) ;
//! - un fil traite les événements du réseau et tient l'état à jour ;
//! - un autre, toutes les 100 ms, pousse à la page ce qui a changé (`vue` :
//!   salons, membres, vocal ; `fil` : les messages du salon ouvert), et
//!   annonce qui parle.
//!
//! Les effets de l'état (prévenir, bannière, fin de session) partent vers la
//! page en événements ponctuels.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ki_core::apparence;
use ki_core::etat::{Effet, Etat};
use ki_core::net::{self, Credentials, Event, NetHandle, VoiceLink, VoicePrefs};
use ki_protocol::{Appareil, ChannelId, ChannelKind, ClientMsg, MsgRef, ServerMsg, UserId};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

/// Délais entre deux tentatives de reconnexion ; le dernier se répète.
const RECONNEXION_S: [u64; 5] = [1, 2, 5, 10, 20];

/// Ce qu'il faut pour se reconnecter tout seul.
#[derive(Clone)]
struct Identifiants {
    serveur: String,
    pseudo: String,
    mot_de_passe: String,
    /// Le code d'invitation de la toute première connexion : effacé dès
    /// l'accueil, il ne sert plus ensuite.
    invitation: Option<String>,
    empreinte: String,
}

/// Les réglages audio, tenus par la page (retenus sur le téléphone) et
/// appliqués au moteur à chaque changement comme à chaque connexion.
#[derive(Clone, Deserialize)]
#[serde(default)]
struct Reglages {
    /// « voix » : détection de la voix ; « ptt » : appuyer pour parler ;
    /// « ouvert » : micro ouvert en continu.
    mode: String,
    /// Détection neuronale (Silero) en mode « voix », et sa sensibilité.
    neuronal: bool,
    sensibilite: f32,
    /// Seuil d'amplitude en mode « voix » : sert aussi de garde à Silero.
    seuil: f32,
    /// ki_voice::NOISE_* : 0 aucune, 1 RNNoise, 2 DeepFilterNet.
    bruit: u8,
    echo: bool,
    gain_auto: bool,
    gain_micro: f32,
    volume: f32,
}

impl Default for Reglages {
    /// Les valeurs par défaut du PC.
    fn default() -> Self {
        Self {
            mode: "voix".into(),
            neuronal: true,
            sensibilite: 0.5,
            seuil: 0.02,
            bruit: ki_voice::NOISE_RNNOISE,
            echo: true,
            gain_auto: true,
            gain_micro: 1.0,
            volume: 1.0,
        }
    }
}

impl Reglages {
    /// Le seuil que voit le moteur : hors du mode « voix », aucun — le micro
    /// s'ouvre selon le mode, pas selon la voix.
    fn seuil_effectif(&self) -> f32 {
        if self.mode == "voix" {
            self.seuil.max(0.001)
        } else {
            0.0
        }
    }

    fn prefs(&self) -> VoicePrefs {
        let mut p = VoicePrefs::par_defaut();
        p.vad_threshold = self.seuil_effectif();
        p.vad_neural = self.neuronal;
        p.vad_sensitivity = self.sensibilite;
        p.vad_hangover_ms = 400;
        p.noise_mode = self.bruit;
        p.aec = self.echo;
        p.agc = self.gain_auto;
        p.input_gain = self.gain_micro;
        p.output_gain = self.volume;
        // Le micro ne s'ouvre qu'en vocal (voir `horloge`).
        p.moteur_a_la_demande = true;
        p
    }
}

#[derive(Default)]
struct Appli {
    reglages: Reglages,
    /// Le bouton « appuyer pour parler » est tenu.
    ptt: bool,
    /// La page des réglages essaie le micro : le moteur tourne hors vocal.
    test_micro: bool,
    /// Dernier démarrage demandé au moteur, pour ne pas le relancer en
    /// boucle pendant qu'il démarre.
    demarrage: Option<std::time::Instant>,
    /// Les volumes par personne, rendus au moteur à chaque démarrage.
    volumes: std::collections::HashMap<UserId, f32>,
    /// L'appli est en arrière-plan : les battements s'espacent.
    arriere_plan: bool,
    /// Déconnectée exprès en arrière-plan (option « rester connecté »
    /// coupée) : on se reconnecte au retour.
    en_veille: bool,
    /// Le dernier battement envoyé au serveur (voir `horloge`).
    dernier_ping: Option<std::time::Instant>,
    /// La dernière réponse du serveur, quelle qu'elle soit : la preuve que
    /// la connexion vit.
    dernier_signe: Option<std::time::Instant>,
    net: Option<NetHandle>,
    etat: Etat,
    /// Le moteur voix survit aux reconnexions (voir `VoiceLink`).
    lien: VoiceLink,
    identifiants: Option<Identifiants>,
    /// Le salon vocal où l'on veut être : retrouvé après une coupure.
    vocal_voulu: Option<ChannelId>,
    muet: bool,
    sourd: bool,
    /// Ce qu'on a annoncé de sa parole, pour n'envoyer que les changements.
    parle_annonce: bool,
    /// La vue et le fil ont changé depuis le dernier envoi à la page.
    vue_sale: bool,
    fil_sale: bool,
    /// Numéro de la connexion en cours : le fil d'événements d'une connexion
    /// remplacée s'arrête de lui-même.
    generation: u64,
    /// Le serveur nous a déjà accueillis depuis `connecter` : un échec de
    /// connexion est alors une coupure à reprendre, pas un refus.
    deja_accueilli: bool,
    /// Le dernier repère de lecture envoyé : (salon, horodatage).
    lu_envoye: Option<(ChannelId, u64)>,
    /// « J'accepte les pokes » est parti sur cette connexion.
    pokes_annonces: bool,
    /// Le logo du serveur tel qu'envoyé à la page : on ne le renvoie que
    /// s'il change (il pèse jusqu'à quelques dizaines de Kio).
    logo_envoye: Option<Option<String>>,
    /// Les photos de profil reçues (empreinte), et celles demandées : on ne
    /// redemande que ce qui a changé.
    photos: std::collections::HashMap<UserId, String>,
    photos_demandees: std::collections::HashSet<(UserId, String)>,
}

type Partage = Arc<Mutex<Appli>>;

impl Appli {
    fn envoyer(&self, msg: ClientMsg) {
        if let Some(n) = &self.net {
            n.send(msg);
        }
    }

    fn etat_vierge(courant: Option<ChannelId>) -> Etat {
        let mut e = Etat::pour(Appareil::Mobile);
        e.courant = courant;
        e
    }
}

// ---------------------------------------------------------------------------
// Ce que voit la page
// ---------------------------------------------------------------------------

/// Un identifiant tel que la page le voit. Les nombres JavaScript perdent
/// leur précision au-delà de 2^53 : celui du bot musique (2^64 - 2) arrivait
/// arrondi, et revenait faux. Il passe en -1.
type IdPage = i64;

fn id_page(id: UserId) -> IdPage {
    if id == ki_protocol::MUSIQUE_ID {
        -1
    } else {
        id as IdPage
    }
}

fn id_rust(id: IdPage) -> UserId {
    if id == -1 {
        ki_protocol::MUSIQUE_ID
    } else {
        id as UserId
    }
}

/// Un rang VALORANT, tel que le PC l'affiche : « Ascendant 1 », en couleur.
#[derive(Clone, Serialize)]
struct VueRang {
    nom: String,
    couleur: String,
}

fn rang_de(m: &ki_protocol::Member) -> Option<VueRang> {
    apparence::palier(m).map(|p| VueRang {
        nom: ki_protocol::nom_de_rang(p),
        couleur: apparence::hex(apparence::couleur_rang(p)),
    })
}

#[derive(Clone, Serialize)]
struct VueOccupant {
    id: IdPage,
    nom: String,
    couleur: String,
    rang: Option<VueRang>,
    jeu: Option<String>,
    parle: bool,
    muet: bool,
    mobile: bool,
}

#[derive(Clone, Serialize)]
struct VueSalon {
    id: ChannelId,
    nom: String,
    vocal: bool,
    non_lus: u32,
    mention: bool,
    occupants: Vec<VueOccupant>,
}

#[derive(Clone, Serialize)]
struct VueMembre {
    id: IdPage,
    /// Le serveur lui-même (bot musique) : pas de menu de modération.
    bot: bool,
    nom: String,
    couleur: String,
    rang: Option<VueRang>,
    admin: bool,
    /// Sanctions vocales posées par un modérateur.
    force_muet: bool,
    force_sourd: bool,
    /// Mon rang est au-dessus du sien : les actions de modération sur lui
    /// aboutiront (le rang tranche, comme sur PC).
    sous_moi: bool,
    en_ligne: bool,
    mobile: bool,
    vocal: Option<ChannelId>,
    /// « en partie · Ascent · 7-5 », s'il partage son statut VALORANT.
    jeu: Option<String>,
}

#[derive(Clone, Serialize)]
struct Vue {
    connecte: bool,
    moi: Option<IdPage>,
    /// « https://hôte:8080 » : les liens vers des fichiers de ce serveur
    /// s'affichent (images, vidéos).
    origine: String,
    mon_pseudo: Option<String>,
    serveur: String,
    salons: Vec<VueSalon>,
    courant: Option<ChannelId>,
    vocal: Option<ChannelId>,
    muet: bool,
    sourd: bool,
    membres: Vec<VueMembre>,
    total_non_lus: u32,
    droits: Droits,
}

/// Ce que mes permissions m'autorisent sur les autres.
#[derive(Clone, Serialize)]
struct Droits {
    couper: bool,
    deplacer: bool,
    expulser: bool,
    bannir: bool,
    /// Supprimer les messages des autres.
    supprimer: bool,
}

#[derive(Clone, Serialize)]
struct VueReaction {
    emoji: String,
    nb: usize,
    moi: bool,
}

#[derive(Clone, Serialize)]
struct VueMessage {
    auteur_id: IdPage,
    auteur: String,
    couleur: String,
    /// Le serveur lui-même (fil de jeu VALORANT, bot musique) : pastille BOT.
    bot: bool,
    texte: String,
    ts: u64,
    /// (auteur, extrait) du message auquel il répond.
    reponse: Option<(String, String)>,
    reactions: Vec<VueReaction>,
    modifie: bool,
    me_nomme: bool,
}

#[derive(Clone, Serialize)]
struct Fil {
    salon: Option<ChannelId>,
    messages: Vec<VueMessage>,
    suite: bool,
    separateur: Option<u64>,
}

/// « Valorant · en file compétitive · party 2/5 », comme sur PC.
/// L'origine HTTPS d'un serveur, comme le PC : le même hôte que QUIC, port
/// 8080.
fn origine(serveur: &str) -> String {
    let hote = serveur
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("ws://")
        .split('/')
        .next()
        .unwrap_or("");
    // « hôte:port » → l'hôte ; une adresse IPv6 entre crochets garde les siens.
    let hote = match hote.rsplit_once(':') {
        Some((h, port)) if !h.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => h,
        _ => hote,
    };
    format!("https://{hote}:8080")
}

fn ligne_de_jeu(j: &ki_protocol::JeuStatut) -> String {
    j.ligne()
}

fn vue(a: &Appli) -> Vue {
    let e = &a.etat;
    let salons = e
        .salons
        .iter()
        .map(|s| {
            let vocal = s.kind == ChannelKind::Voice;
            let n = e.non_lus.get(&s.id).copied().unwrap_or_default();
            VueSalon {
                id: s.id,
                nom: s.name.clone(),
                vocal,
                non_lus: n.nb,
                mention: n.mention,
                occupants: if vocal {
                    e.membres
                        .iter()
                        .filter(|m| m.online && m.voice == Some(s.id))
                        .map(|m| VueOccupant {
                            id: id_page(m.user_id),
                            nom: m.username.clone(),
                            couleur: apparence::hex(apparence::couleur_membre(m)),
                            rang: rang_de(m),
                            jeu: m.jeu.as_ref().map(ligne_de_jeu),
                            // Sa propre parole, le serveur ne la renvoie pas :
                            // c'est ce qu'on annonce soi-même.
                            parle: if Some(m.user_id) == e.moi { a.parle_annonce } else { m.speaking },
                            muet: if Some(m.user_id) == e.moi {
                                a.muet || a.sourd || m.force_muted
                            } else {
                                m.muted || m.force_muted
                            },
                            mobile: m.mobile,
                        })
                        .collect()
                } else {
                    Vec::new()
                },
            }
        })
        .collect();
    let membres = e
        .membres
        .iter()
        // Le fil de jeu (0) n'est personne ; le bot musique, si : on règle
        // son volume comme celui d'un membre.
        .filter(|m| m.user_id != 0)
        .map(|m| VueMembre {
            id: id_page(m.user_id),
            bot: ki_core::etat::est_bot(m.user_id),
            nom: m.username.clone(),
            couleur: apparence::hex(apparence::couleur_membre(m)),
            rang: rang_de(m),
            admin: m.admin,
            force_muet: m.force_muted,
            force_sourd: m.force_deafened,
            sous_moi: m.rank < e.rang,
            en_ligne: m.online,
            mobile: m.mobile,
            vocal: m.voice,
            jeu: m.jeu.as_ref().filter(|_| m.online).map(ligne_de_jeu),
        })
        .collect();
    Vue {
        connecte: a.net.is_some() && e.accueilli,
        moi: e.moi.map(id_page),
        origine: a.identifiants.as_ref().map(|i| origine(&i.serveur)).unwrap_or_default(),
        mon_pseudo: e.mon_pseudo().map(str::to_string),
        serveur: e.serveur.name.clone(),
        salons,
        courant: e.courant,
        vocal: e.vocal,
        muet: a.muet,
        sourd: a.sourd,
        membres,
        total_non_lus: e.total_non_lus(),
        droits: Droits {
            couper: e.peut(ki_protocol::perm::MUTE_MEMBERS),
            deplacer: e.peut(ki_protocol::perm::MOVE_MEMBERS),
            expulser: e.peut(ki_protocol::perm::KICK),
            bannir: e.peut(ki_protocol::perm::BAN),
            supprimer: e.peut(ki_protocol::perm::DELETE_MESSAGES),
        },
    }
}

fn fil(a: &Appli) -> Fil {
    let e = &a.etat;
    let moi = e.moi;
    let couleur_de = |id: UserId, nom: &str| {
        let c = e
            .membres
            .iter()
            .find(|m| m.user_id == id)
            .map(apparence::couleur_membre)
            .unwrap_or_else(|| apparence::couleur_pseudo(nom));
        apparence::hex(c)
    };
    Fil {
        salon: e.courant,
        messages: e
            .messages
            .iter()
            .map(|m| VueMessage {
                auteur_id: id_page(m.user_id),
                auteur: m.username.clone(),
                couleur: couleur_de(m.user_id, &m.username),
                bot: ki_core::etat::est_bot(m.user_id),
                texte: m.text.clone(),
                ts: m.ts,
                reponse: m.reply_to.as_ref().map(|r| (r.username.clone(), r.excerpt.clone())),
                reactions: m
                    .reactions
                    .iter()
                    .map(|r| VueReaction {
                        emoji: r.emoji.clone(),
                        nb: r.users.len(),
                        moi: moi.is_some_and(|id| r.users.contains(&id)),
                    })
                    .collect(),
                modifie: m.edited,
                me_nomme: e.me_nomme(m.user_id, &m.text),
            })
            .collect(),
        suite: e.historique_suite,
        separateur: e.separateur,
    }
}

// ---------------------------------------------------------------------------
// Connexion et événements
// ---------------------------------------------------------------------------

/// Les messages qui touchent au fil du salon ouvert.
fn touche_au_fil(msg: &ServerMsg) -> bool {
    matches!(
        msg,
        ServerMsg::Welcome { .. }
            | ServerMsg::Chat { .. }
            | ServerMsg::History { .. }
            | ServerMsg::HistoryPage { .. }
            | ServerMsg::Reaction { .. }
            | ServerMsg::MessageDeleted { .. }
            | ServerMsg::MessageEdited { .. }
            | ServerMsg::NonLus { .. }
            | ServerMsg::ChannelsUpdated { .. }
            // La liste des membres décide de qui est nommé.
            | ServerMsg::Members { .. }
    )
}

/// Ouvre une connexion avec les identifiants retenus, et lance le fil qui
/// en traite les événements.
fn lancer(app: &AppHandle, partage: &Partage) {
    let mut a = partage.lock().unwrap();
    let Some(id) = a.identifiants.clone() else { return };
    if let Some(mut ancien) = a.net.take() {
        ancien.quitter_borne(Duration::from_millis(500));
    }
    a.generation += 1;
    let generation = a.generation;
    // Une connexion neuve repart d'un état vierge, mais garde le salon lu :
    // l'accueil le rouvrira.
    a.etat = Appli::etat_vierge(a.etat.courant);
    a.pokes_annonces = false;
    a.logo_envoye = None;
    a.dernier_signe = None;
    a.photos_demandees.clear();
    let reveil: net::Reveil = Arc::new(|| {});
    let mut handle = net::connect(
        id.serveur.clone(),
        Credentials {
            username: id.pseudo.clone(),
            password: id.mot_de_passe.clone(),
            invite: id.invitation.clone(),
            fingerprint: id.empreinte.clone(),
            appareil: Appareil::Mobile,
        },
        a.reglages.prefs(),
        a.lien.clone(),
        reveil,
    );
    // Les événements se lisent sur notre fil, pas sur celui de la page.
    let evenements = std::mem::replace(&mut handle.events, std::sync::mpsc::channel().1);
    a.net = Some(handle);
    a.vue_sale = true;
    drop(a);

    let app = app.clone();
    let partage = partage.clone();
    std::thread::spawn(move || {
        for ev in evenements.iter() {
            let mut a = partage.lock().unwrap();
            if a.generation != generation {
                return;
            }
            match ev {
                Event::Fingerprint(fp) => {
                    if let Some(id) = a.identifiants.as_mut() {
                        id.empreinte = fp.clone();
                    }
                    let _ = app.emit("empreinte", fp);
                }
                Event::Msg(ServerMsg::Avatar { user_id, hash, data }) => {
                    // Une photo de profil : base64 d'un PNG, qu'on passe à
                    // la page telle quelle (après un contrôle des caractères :
                    // elle finit dans une adresse « data: »).
                    let url = data
                        .filter(|d| {
                            d.len() < 200_000
                                && d.bytes().all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
                        })
                        .map(|d| format!("data:image/png;base64,{d}"));
                    a.photos.insert(user_id, hash);
                    let _ = app.emit("photo", (id_page(user_id), url));
                }
                Event::Msg(msg) => {
                    a.dernier_signe = Some(std::time::Instant::now());
                    let accueil = matches!(msg, ServerMsg::Welcome { .. });
                    let roster = matches!(msg, ServerMsg::Members { .. } | ServerMsg::MemberUpdate { .. });
                    let non_lus = matches!(msg, ServerMsg::NonLus { .. });
                    if touche_au_fil(&msg) {
                        a.fil_sale = true;
                    }
                    a.vue_sale = true;
                    let effets = a.etat.appliquer(msg);
                    // Un serveur qui tient les lus connaît les pokes : on les
                    // accepte, comme le PC. Sans ça, il refusait qu'on nous
                    // poke (« client d'avant »).
                    if non_lus && !a.pokes_annonces {
                        a.pokes_annonces = true;
                        a.envoyer(ClientMsg::AccepterPokes { accepter: true });
                    }
                    if roster {
                        demander_photos(&mut a);
                    }
                    // Le logo du serveur, à l'accueil ou quand un admin le
                    // change : base64 d'un PNG, passé à la page en « data: ».
                    let logo = a.etat.serveur.icon.clone().filter(|d| {
                        d.len() < 200_000
                            && d.bytes().all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
                    });
                    if a.logo_envoye.as_ref() != Some(&logo) {
                        let _ = app.emit("logo", logo.as_ref().map(|d| format!("data:image/png;base64,{d}")));
                        a.logo_envoye = Some(logo);
                    }
                    if accueil {
                        a.deja_accueilli = true;
                        if let Some(id) = a.identifiants.as_mut() {
                            id.invitation = None;
                        }
                        // Reprise : le salon vocal d'avant la coupure.
                        if let Some(c) = a.vocal_voulu {
                            if let Ok(m) = a.etat.rejoindre_vocal(c, None) {
                                a.envoyer(m);
                            }
                        }
                        let _ = app.emit("connecte", ());
                    }
                    for effet in effets {
                        match effet {
                            Effet::Envoyer(m) => a.envoyer(m),
                            Effet::Prevenir { salon, mention, auteur, extrait } => {
                                let _ = app.emit("prevenir", (salon, mention, auteur, extrait));
                            }
                            Effet::Info(t) => {
                                let _ = app.emit("info", t);
                            }
                            Effet::Erreur(t) => {
                                let _ = app.emit("erreur", t);
                            }
                            Effet::Poke(qui) => {
                                let _ = app.emit("poke", qui);
                            }
                            Effet::MotDePasseVocal { salon, faux } => {
                                a.vocal_voulu = None;
                                let _ = app.emit("mot_de_passe_vocal", (salon, faux));
                            }
                            Effet::Fin(raison) => {
                                fin(&mut a, &app, raison);
                                return;
                            }
                        }
                    }
                }
                Event::ConnectFailed(e) => {
                    // Jamais accueilli sur cette session : un refus ou un
                    // serveur injoignable, à dire. Déjà accueilli une fois (la
                    // reprise d'une coupure) : on retente.
                    if !a.deja_accueilli {
                        fin(&mut a, &app, e);
                    } else {
                        let _ = app.emit("coupe", e);
                        drop(a);
                        reconnecter(&app, &partage, generation);
                    }
                    return;
                }
                Event::Disconnected => {
                    a.vue_sale = true;
                    let _ = app.emit("coupe", String::new());
                    drop(a);
                    reconnecter(&app, &partage, generation);
                    return;
                }
                Event::Congedie(raison) => {
                    fin(&mut a, &app, raison);
                    return;
                }
            }
        }
    });
}

/// Demande les photos de profil manquantes ou changées, en une fois.
fn demander_photos(a: &mut Appli) {
    let mut ids = Vec::new();
    for m in &a.etat.membres {
        let Some(h) = &m.avatar else { continue };
        if a.photos.get(&m.user_id) == Some(h) {
            continue;
        }
        if a.photos_demandees.insert((m.user_id, h.clone())) {
            ids.push(m.user_id);
        }
    }
    for lot in ids.chunks(64) {
        a.envoyer(ClientMsg::RequestAvatars { user_ids: lot.to_vec() });
    }
}

/// Fin de session sans reprise : refus, expulsion, déconnexion voulue.
fn fin(a: &mut Appli, app: &AppHandle, raison: String) {
    a.identifiants = None;
    a.vocal_voulu = None;
    a.generation += 1;
    if let Some(mut n) = a.net.take() {
        n.quitter_borne(Duration::from_millis(500));
    }
    a.lien.arreter();
    a.etat = Appli::etat_vierge(None);
    a.parle_annonce = false;
    a.vue_sale = true;
    a.fil_sale = true;
    let _ = app.emit("fin", raison);
}

/// Retente la connexion après un délai, tant que personne n'a fermé ni
/// relancé la session entre-temps. Chaque échec repasse par ici avec le
/// délai suivant.
fn reconnecter(app: &AppHandle, partage: &Partage, generation: u64) {
    let app = app.clone();
    let partage = partage.clone();
    std::thread::spawn(move || {
        let essai = {
            let mut a = partage.lock().unwrap();
            if a.generation != generation || a.identifiants.is_none() {
                return;
            }
            let n = ESSAIS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            a.vue_sale = true;
            n
        };
        let attente = RECONNEXION_S[essai.min(RECONNEXION_S.len() - 1)];
        std::thread::sleep(Duration::from_secs(attente));
        {
            let a = partage.lock().unwrap();
            if a.generation != generation || a.identifiants.is_none() {
                return;
            }
        }
        lancer(&app, &partage);
    });
}

/// Essais de reconnexion d'affilée, remis à zéro à chaque accueil.
static ESSAIS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Toutes les 100 ms : la page reçoit ce qui a changé, le micro suit l'état,
/// et l'on annonce sa parole au serveur quand elle change.
fn horloge(app: AppHandle, partage: Partage) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(100));
        let (v, f) = {
            let mut a = partage.lock().unwrap();
            if a.etat.accueilli {
                ESSAIS.store(0, std::sync::atomic::Ordering::Relaxed);
            }
            // Le battement : la connexion du téléphone n'en a pas
            // d'automatique (voir `QuicClient::connect_mobile`). Toutes les
            // cinq secondes au premier plan ou en vocal ; toutes les vingt en
            // arrière-plan, sous les quarante d'inactivité tolérée.
            if a.net.is_some() && a.etat.accueilli {
                let rythme = if a.arriere_plan && a.etat.vocal.is_none() {
                    Duration::from_secs(20)
                } else {
                    Duration::from_secs(5)
                };
                if a.dernier_ping.is_none_or(|t| t.elapsed() >= rythme) {
                    a.dernier_ping = Some(std::time::Instant::now());
                    a.envoyer(ClientMsg::Ping);
                }
                // Le serveur ne répond plus (redémarré, réseau perdu) : QUIC
                // ne s'en apercevrait qu'au bout des quarante secondes
                // d'inactivité. Au premier plan, dix secondes sans le moindre
                // signe suffisent pour se reconnecter tout de suite.
                let silence = a.dernier_signe.map(|t| t.elapsed()).unwrap_or_default();
                let tolere = if a.arriere_plan { Duration::from_secs(45) } else { Duration::from_secs(10) };
                if silence > tolere {
                    tracing::info!("le serveur ne répond plus depuis {} s : reconnexion", silence.as_secs());
                    a.dernier_signe = None;
                    a.generation += 1;
                    let generation = a.generation;
                    if let Some(mut n) = a.net.take() {
                        std::thread::spawn(move || n.quitter_borne(Duration::from_millis(500)));
                    }
                    a.vue_sale = true;
                    let _ = app.emit("coupe", String::new());
                    reconnecter(&app, &partage, generation);
                }
            }
            // Le moteur (donc le micro) ne tourne qu'en vocal, ou pendant
            // l'essai des réglages : hors de là, Android n'a pas à montrer
            // son témoin de micro.
            let veut = a.vocal_voulu.is_some() || a.etat.vocal.is_some() || a.test_micro;
            let tourne = a.lien.engine.lock().unwrap().is_some();
            if tourne {
                a.demarrage = None;
            }
            if veut && !tourne && a.lien.pret()
                && a.demarrage.is_none_or(|t| t.elapsed() > Duration::from_secs(3))
            {
                a.demarrage = Some(std::time::Instant::now());
                let mut prefs = a.reglages.prefs();
                prefs.volumes = a.volumes.clone();
                if a.sourd {
                    prefs.output_gain = 0.0;
                }
                a.lien.restart_voice(prefs);
            } else if !veut && tourne {
                // L'arrêt attend les fils audio : hors du verrou.
                let lien = a.lien.clone();
                a.parle_annonce = false;
                std::thread::spawn(move || lien.suspendre());
            }
            // Le micro n'émet qu'en vocal, ni muet ni sourd ; en
            // « appuyer pour parler », que bouton tenu.
            let arme = a.etat.vocal.is_some()
                && !a.muet
                && !a.sourd
                && (a.reglages.mode != "ptt" || a.ptt);
            let parle = {
                let moteur = a.lien.engine.lock().unwrap();
                moteur.as_ref().map(|m| {
                    m.set_transmit(arme);
                    m.is_sending()
                })
            };
            if let Some(parle) = parle {
                if parle != a.parle_annonce && a.etat.vocal.is_some() {
                    a.parle_annonce = parle;
                    a.vue_sale = true;
                    let muet = a.muet || a.sourd;
                    a.envoyer(ClientMsg::VoiceState { speaking: parle, muted: muet });
                }
            }
            // Ce qui arrive dans le fil qu'on regarde est lu : le serveur
            // l'apprend, sans quoi ses pastilles reviendraient à la
            // prochaine connexion — et sur le PC.
            if a.etat.regarde && a.etat.serveur_gere_lus {
                if let Some(c) = a.etat.courant {
                    let dernier = a.etat.messages.iter().map(|m| m.ts).max().unwrap_or(0);
                    if dernier > 0 && a.lu_envoye != Some((c, dernier)) {
                        a.lu_envoye = Some((c, dernier));
                        if let Some(m) = a.etat.marquer_lu(c) {
                            a.envoyer(m);
                        }
                        a.vue_sale = true;
                    }
                }
            }
            let v = std::mem::take(&mut a.vue_sale).then(|| vue(&a));
            let f = std::mem::take(&mut a.fil_sale).then(|| fil(&a));
            (v, f)
        };
        if let Some(v) = v {
            let _ = app.emit("vue", v);
        }
        if let Some(f) = f {
            let _ = app.emit("fil", f);
        }
    });
}

// ---------------------------------------------------------------------------
// Commandes de la page
// ---------------------------------------------------------------------------

#[tauri::command]
fn connecter(
    app: AppHandle,
    partage: State<'_, Partage>,
    serveur: String,
    pseudo: String,
    mot_de_passe: String,
    invitation: Option<String>,
    empreinte: Option<String>,
) {
    {
        let mut a = partage.lock().unwrap();
        a.identifiants = Some(Identifiants {
            serveur: serveur.trim().to_string(),
            pseudo: pseudo.trim().to_string(),
            mot_de_passe,
            invitation: invitation.map(|i| i.trim().to_string()).filter(|i| !i.is_empty()),
            empreinte: empreinte.unwrap_or_default(),
        });
        a.etat = Appli::etat_vierge(None);
        a.vocal_voulu = None;
        a.deja_accueilli = false;
    }
    ESSAIS.store(0, std::sync::atomic::Ordering::Relaxed);
    lancer(&app, &partage);
}

#[tauri::command]
fn deconnecter(app: AppHandle, partage: State<'_, Partage>) {
    let mut a = partage.lock().unwrap();
    fin(&mut a, &app, String::new());
}

#[tauri::command]
fn ouvrir_salon(partage: State<'_, Partage>, salon: ChannelId) {
    let mut a = partage.lock().unwrap();
    for m in a.etat.ouvrir_salon(salon) {
        a.envoyer(m);
    }
    a.vue_sale = true;
    a.fil_sale = true;
}

#[tauri::command]
fn remonter(partage: State<'_, Partage>) {
    let mut a = partage.lock().unwrap();
    if let Some(m) = a.etat.remonter() {
        a.envoyer(m);
    }
}

/// La page montre le fil, à jour (appli au premier plan, fil en bas) : ce
/// qui y arrive est lu.
#[tauri::command]
fn regarde(partage: State<'_, Partage>, oui: bool) {
    let mut a = partage.lock().unwrap();
    a.etat.regarde = oui;
    if oui {
        if let Some(c) = a.etat.courant {
            if let Some(m) = a.etat.marquer_lu(c) {
                a.envoyer(m);
            }
            a.vue_sale = true;
        }
    }
}

#[tauri::command]
fn envoyer(
    partage: State<'_, Partage>,
    texte: String,
    reponse: Option<(IdPage, u64)>,
) -> Result<(), String> {
    let a = partage.lock().unwrap();
    let texte = texte.trim().to_string();
    if texte.is_empty() {
        return Ok(());
    }
    let salon = a.etat.courant.ok_or("aucun salon ouvert")?;
    a.envoyer(ClientMsg::Chat {
        text: texte,
        reply_to: reponse.map(|(user_id, ts)| MsgRef { user_id: id_rust(user_id), ts }),
        salon: Some(salon),
    });
    Ok(())
}

#[tauri::command]
fn reagir(partage: State<'_, Partage>, auteur: IdPage, ts: u64, emoji: String, on: bool) {
    let a = partage.lock().unwrap();
    a.envoyer(ClientMsg::React { message: MsgRef { user_id: id_rust(auteur), ts }, emoji, on });
}

/// Modifier son propre message (le serveur refuse ceux des autres).
#[tauri::command]
fn modifier(partage: State<'_, Partage>, auteur: IdPage, ts: u64, texte: String) {
    let texte = texte.trim().to_string();
    if texte.is_empty() {
        return;
    }
    let a = partage.lock().unwrap();
    a.envoyer(ClientMsg::EditMessage { message: MsgRef { user_id: id_rust(auteur), ts }, text: texte });
}

/// Supprimer un message : le sien, ou celui d'un autre avec la permission.
#[tauri::command]
fn supprimer(partage: State<'_, Partage>, auteur: IdPage, ts: u64) {
    let a = partage.lock().unwrap();
    a.envoyer(ClientMsg::DeleteMessage { message: MsgRef { user_id: id_rust(auteur), ts } });
}

#[tauri::command]
fn rejoindre_vocal(
    partage: State<'_, Partage>,
    salon: ChannelId,
    mot_de_passe: Option<String>,
) -> Result<(), String> {
    let mut a = partage.lock().unwrap();
    let m = a.etat.rejoindre_vocal(salon, mot_de_passe)?;
    a.vocal_voulu = Some(salon);
    a.envoyer(m);
    Ok(())
}

#[tauri::command]
fn quitter_vocal(partage: State<'_, Partage>) {
    let mut a = partage.lock().unwrap();
    a.vocal_voulu = None;
    a.parle_annonce = false;
    let m = a.etat.quitter_vocal();
    a.envoyer(m);
}

#[tauri::command]
fn micro(partage: State<'_, Partage>, muet: bool) {
    let mut a = partage.lock().unwrap();
    a.muet = muet;
    a.vue_sale = true;
    let parle = a.parle_annonce;
    let muted = muet || a.sourd;
    a.envoyer(ClientMsg::VoiceState { speaking: parle && !muet, muted });
}

/// Sourd coupe aussi le micro, comme sur PC.
#[tauri::command]
fn sourdine(partage: State<'_, Partage>, sourd: bool) {
    let mut a = partage.lock().unwrap();
    a.sourd = sourd;
    a.muet = sourd;
    a.vue_sale = true;
    if let Some(m) = a.lien.engine.lock().unwrap().as_ref() {
        m.set_output_gain(if sourd { 0.0 } else { a.reglages.volume });
    }
    a.envoyer(ClientMsg::VoiceState { speaking: false, muted: sourd });
}

/// Le volume d'une personne, 1.0 = 100 %.
#[tauri::command]
fn volume_membre(partage: State<'_, Partage>, id: IdPage, gain: f32) {
    let id = id_rust(id);
    let gain = gain.clamp(0.0, 2.0);
    let moteur = {
        let mut a = partage.lock().unwrap();
        a.volumes.insert(id, gain);
        a.lien.engine.clone()
    };
    let garde = moteur.lock().unwrap();
    if let Some(m) = garde.as_ref() {
        m.set_user_volume(id, gain);
    }
}

/// Applique les réglages au moteur, tout de suite, et les retient pour les
/// connexions suivantes.
#[tauri::command]
fn regler(partage: State<'_, Partage>, reglages: Reglages) {
    let mut a = partage.lock().unwrap();
    a.reglages = reglages;
    let r = &a.reglages;
    let moteur = a.lien.engine.lock().unwrap();
    if let Some(m) = moteur.as_ref() {
        m.set_vad_threshold(r.seuil_effectif());
        m.set_vad_neural(r.neuronal);
        m.set_vad_sensitivity(r.sensibilite);
        m.set_noise_mode(r.bruit);
        m.set_aec(r.echo);
        m.set_agc(r.gain_auto);
        m.set_input_gain(r.gain_micro);
        m.set_output_gain(if a.sourd { 0.0 } else { r.volume });
    }
}

/// L'appli passe au premier plan ou en arrière-plan. En arrière-plan, hors
/// vocal, et si l'on ne doit pas rester connecté (batterie faible), on se
/// déconnecte tout à fait ; au retour, on se reconnecte.
#[tauri::command]
fn premier_plan(app: AppHandle, partage: State<'_, Partage>, oui: bool, rester_connecte: bool) {
    let relancer = {
        let mut a = partage.lock().unwrap();
        a.arriere_plan = !oui;
        if oui {
            // Un battement tout de suite : la connexion a peut-être dormi. Et
            // l'on compte le silence à partir de maintenant.
            a.dernier_ping = None;
            a.dernier_signe = Some(std::time::Instant::now());
            std::mem::take(&mut a.en_veille) && a.identifiants.is_some()
        } else {
            let en_vocal = a.etat.vocal.is_some() || a.vocal_voulu.is_some();
            if !rester_connecte && !en_vocal && a.net.is_some() {
                // Comme une fin, mais en gardant de quoi revenir : les
                // identifiants et le salon lu.
                a.generation += 1;
                a.en_veille = true;
                if let Some(mut n) = a.net.take() {
                    std::thread::spawn(move || n.quitter_borne(Duration::from_millis(500)));
                }
                a.vue_sale = true;
            }
            false
        }
    };
    if relancer {
        lancer(&app, &partage);
    }
}

/// La page des réglages est ouverte : le micro tourne pour la jauge et
/// l'essai, même hors vocal.
#[tauri::command]
fn tester_micro(partage: State<'_, Partage>, oui: bool) {
    partage.lock().unwrap().test_micro = oui;
}

/// Appuyer pour parler : le bouton est tenu, ou relâché.
#[tauri::command]
fn parler(partage: State<'_, Partage>, oui: bool) {
    partage.lock().unwrap().ptt = oui;
}

#[derive(Serialize)]
struct EtatAudio {
    /// Le moteur tourne (on est connecté).
    actif: bool,
    /// Crête du micro, 0..1.
    niveau: f32,
    /// Probabilité de parole selon Silero, 0..1.
    parole: f32,
    /// La décision d'émission est ouverte : on parle, pour le moteur.
    ouvert: bool,
    /// Ce qui part réellement vers les autres.
    emet: bool,
    /// « inactif », « chargement », « pret », « echec ».
    silero: &'static str,
    micro_sature: bool,
    essai: EtatEssaiVue,
}

#[derive(Serialize)]
struct EtatEssaiVue {
    /// « vide », « enregistre », « pret ».
    etat: &'static str,
    /// Avancement de l'enregistrement ou de la lecture, 0..1.
    avancement: f32,
    /// « envoyee » ou « brute » pendant une lecture.
    lecture: Option<&'static str>,
    /// Le volume de sa voix chez les autres, en dB au-dessus d'une voix
    /// réglée par défaut.
    ecart_db: Option<f32>,
}

/// L'état du micro pour la page des réglages, lu à chaque image.
#[tauri::command]
fn etat_audio(partage: State<'_, Partage>) -> EtatAudio {
    let moteur = partage.lock().unwrap().lien.engine.clone();
    let garde = moteur.lock().unwrap();
    let Some(m) = garde.as_ref() else {
        return EtatAudio {
            actif: false,
            niveau: 0.0,
            parole: 0.0,
            ouvert: false,
            emet: false,
            silero: "inactif",
            micro_sature: false,
            essai: EtatEssaiVue { etat: "vide", avancement: 0.0, lecture: None, ecart_db: None },
        };
    };
    let st = m.stats();
    let essai = match m.essai() {
        ki_voice::EtatEssai::Vide => {
            EtatEssaiVue { etat: "vide", avancement: 0.0, lecture: None, ecart_db: None }
        }
        ki_voice::EtatEssai::Enregistre(av) => {
            EtatEssaiVue { etat: "enregistre", avancement: av, lecture: None, ecart_db: None }
        }
        ki_voice::EtatEssai::Pret { lecture, ecart_db, .. } => EtatEssaiVue {
            etat: "pret",
            avancement: lecture.map(|(_, av)| av).unwrap_or(0.0),
            lecture: lecture.map(|(v, _)| match v {
                ki_voice::VersionEssai::Envoyee => "envoyee",
                ki_voice::VersionEssai::Brute => "brute",
            }),
            ecart_db: Some(ecart_db),
        },
    };
    EtatAudio {
        actif: true,
        niveau: st.mic_peak,
        parole: st.vad_prob,
        ouvert: st.vad_ouvert,
        emet: m.is_sending(),
        silero: match st.silero_etat {
            ki_voice::SILERO_CHARGEMENT => "chargement",
            ki_voice::SILERO_PRET => "pret",
            ki_voice::SILERO_ECHEC => "echec",
            _ => "inactif",
        },
        micro_sature: st.micro_sature,
        essai,
    }
}

/// L'essai « enregistrer 5 s et réécouter », celui de la page Casque du PC.
#[tauri::command]
fn essai(partage: State<'_, Partage>, action: String) -> Result<(), String> {
    let moteur = partage.lock().unwrap().lien.engine.clone();
    let garde = moteur.lock().unwrap();
    let m = garde.as_ref().ok_or("connecte-toi d'abord : le micro s'ouvre avec la connexion")?;
    match action.as_str() {
        "enregistrer" => m.enregistrer_essai(),
        "envoyee" => m.rejouer_essai(ki_voice::VersionEssai::Envoyee),
        "brute" => m.rejouer_essai(ki_voice::VersionEssai::Brute),
        _ => m.arreter_essai(),
    }
    Ok(())
}

#[tauri::command]
fn poke(partage: State<'_, Partage>, id: IdPage) {
    partage.lock().unwrap().envoyer(ClientMsg::Poke { user_id: id_rust(id) });
}

/// Modération, comme le menu d'un membre sur PC. Le serveur revérifie
/// permission et rang : la page ne fait que ne pas proposer l'impossible.
#[tauri::command]
fn moderer(
    partage: State<'_, Partage>,
    id: IdPage,
    action: String,
    salon: Option<ChannelId>,
    motif: Option<String>,
    duree_s: Option<u64>,
) -> Result<(), String> {
    let id = id_rust(id);
    let a = partage.lock().unwrap();
    let m = a.etat.membres.iter().find(|m| m.user_id == id).ok_or("membre introuvable")?;
    let username = m.username.clone();
    let msg = match action.as_str() {
        "muet" => ClientMsg::AdminVoiceMute { username, muted: !m.force_muted },
        "sourd" => ClientMsg::AdminVoiceDeafen { username, deafened: !m.force_deafened },
        "deplacer" => ClientMsg::AdminVoiceMove { username, channel: salon },
        "expulser" => ClientMsg::Kick { user_id: id, reason: motif.unwrap_or_default() },
        "bannir" => ClientMsg::AdminBan {
            username,
            reason: motif.unwrap_or_default(),
            duration_secs: duree_s.unwrap_or(0),
        },
        autre => return Err(format!("action inconnue : {autre}")),
    };
    a.envoyer(msg);
    Ok(())
}

/// Les images et vidéos postées dans les salons, servies à la page par le
/// protocole `kimedia` : la page demande `kimedia://…/<adresse encodée>`, on
/// va la chercher chez le serveur — **lui seul**, par HTTPS épinglé sur
/// son empreinte, comme le PC — et on rend les octets. La WebView ne
/// pourrait pas le faire seule : le certificat du port 8080 est celui du
/// serveur, que rien ne signe.
fn media(partage: &Partage, requete: &tauri::http::Request<Vec<u8>>) -> tauri::http::Response<Vec<u8>> {
    use tauri::http::{header, Response, StatusCode};
    let refus = |code: StatusCode| Response::builder().status(code).body(Vec::new()).unwrap();
    // Le chemin porte l'adresse d'origine, encodée.
    let brut = requete.uri().path().trim_start_matches('/');
    let Ok(adresse) = urlencoding::decode(brut) else { return refus(StatusCode::BAD_REQUEST) };
    let (origine_attendue, empreinte) = {
        let a = partage.lock().unwrap();
        match &a.identifiants {
            Some(i) => (origine(&i.serveur), i.empreinte.clone()),
            None => return refus(StatusCode::FORBIDDEN),
        }
    };
    // Seulement les fichiers de notre serveur.
    if !adresse.starts_with(&format!("{origine_attendue}/files/")) {
        return refus(StatusCode::FORBIDDEN);
    }
    let agent = ureq::AgentBuilder::new()
        .tls_config(ki_client_quic::pinned_tls_config((!empreinte.is_empty()).then_some(empreinte.as_str())))
        .https_only(true)
        .build();
    let mut req = agent.get(&adresse).timeout(Duration::from_secs(30));
    // La lecture d'une vidéo avance par plages : on les transmet.
    if let Some(plage) = requete.headers().get(header::RANGE).and_then(|v| v.to_str().ok()) {
        req = req.set("Range", plage);
    }
    let reponse = match req.call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => {
            return refus(StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY))
        }
        Err(e) => {
            tracing::warn!("média {adresse} : {e}");
            return refus(StatusCode::BAD_GATEWAY);
        }
    };
    let statut = reponse.status();
    let mut construit = Response::builder()
        .status(statut)
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::ACCEPT_RANGES, "bytes");
    for nom in ["content-type", "content-range"] {
        if let Some(v) = reponse.header(nom) {
            construit = construit.header(nom, v);
        }
    }
    // 64 Mio au plus : la plus grosse vidéo raisonnable d'un salon.
    let mut octets = Vec::new();
    use std::io::Read;
    if reponse.into_reader().take(64 << 20).read_to_end(&mut octets).is_err() {
        return refus(StatusCode::BAD_GATEWAY);
    }
    construit.body(octets).unwrap_or_else(|_| refus(StatusCode::INTERNAL_SERVER_ERROR))
}

fn traces() {
    use tracing_subscriber::prelude::*;
    let filtre = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info".into());
    #[cfg(target_os = "android")]
    {
        let _ = tracing_subscriber::registry()
            .with(filtre)
            .with(tracing_android::layer("ki-chat").expect("couche logcat"))
            .try_init();
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = tracing_subscriber::registry()
            .with(filtre)
            .with(tracing_subscriber::fmt::layer())
            .try_init();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    traces();
    let partage: Partage = Arc::new(Mutex::new(Appli {
        etat: Appli::etat_vierge(None),
        ..Appli::default()
    }));
    let pour_media = partage.clone();
    tauri::Builder::default()
        .register_asynchronous_uri_scheme_protocol("kimedia", move |_ctx, requete, repondre| {
            let partage = pour_media.clone();
            // Hors du fil de la WebView : un téléchargement prend son temps.
            std::thread::spawn(move || repondre.respond(media(&partage, &requete)));
        })
        .manage(partage)
        .setup(|app| {
            horloge(app.handle().clone(), app.state::<Partage>().inner().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            connecter,
            deconnecter,
            ouvrir_salon,
            remonter,
            regarde,
            envoyer,
            reagir,
            rejoindre_vocal,
            quitter_vocal,
            micro,
            sourdine,
            volume_membre,
            poke,
            moderer,
            regler,
            parler,
            tester_micro,
            premier_plan,
            etat_audio,
            essai,
            modifier,
            supprimer,
        ])
        .run(tauri::generate_context!())
        .expect("lancement de l'appli");
}
