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
use serde::Serialize;
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

#[derive(Default)]
struct Appli {
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
    id: UserId,
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
    id: UserId,
    nom: String,
    couleur: String,
    rang: Option<VueRang>,
    admin: bool,
    en_ligne: bool,
    mobile: bool,
    vocal: Option<ChannelId>,
    /// « en partie · Ascent · 7-5 », s'il partage son statut VALORANT.
    jeu: Option<String>,
}

#[derive(Clone, Serialize)]
struct Vue {
    connecte: bool,
    moi: Option<UserId>,
    mon_pseudo: Option<String>,
    serveur: String,
    salons: Vec<VueSalon>,
    courant: Option<ChannelId>,
    vocal: Option<ChannelId>,
    muet: bool,
    sourd: bool,
    membres: Vec<VueMembre>,
    total_non_lus: u32,
}

#[derive(Clone, Serialize)]
struct VueReaction {
    emoji: String,
    nb: usize,
    moi: bool,
}

#[derive(Clone, Serialize)]
struct VueMessage {
    auteur_id: UserId,
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
                            id: m.user_id,
                            nom: m.username.clone(),
                            couleur: apparence::hex(apparence::couleur_membre(m)),
                            rang: rang_de(m),
                            jeu: m.jeu.as_ref().map(ligne_de_jeu),
                            parle: m.speaking,
                            muet: m.muted || m.force_muted,
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
        .filter(|m| !ki_core::etat::est_bot(m.user_id))
        .map(|m| VueMembre {
            id: m.user_id,
            nom: m.username.clone(),
            couleur: apparence::hex(apparence::couleur_membre(m)),
            rang: rang_de(m),
            admin: m.admin,
            en_ligne: m.online,
            mobile: m.mobile,
            vocal: m.voice,
            jeu: m.jeu.as_ref().filter(|_| m.online).map(ligne_de_jeu),
        })
        .collect();
    Vue {
        connecte: a.net.is_some() && e.accueilli,
        moi: e.moi,
        mon_pseudo: e.mon_pseudo().map(str::to_string),
        serveur: e.serveur.name.clone(),
        salons,
        courant: e.courant,
        vocal: e.vocal,
        muet: a.muet,
        sourd: a.sourd,
        membres,
        total_non_lus: e.total_non_lus(),
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
                auteur_id: m.user_id,
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
        VoicePrefs::par_defaut(),
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
                Event::Msg(msg) => {
                    let accueil = matches!(msg, ServerMsg::Welcome { .. });
                    if touche_au_fil(&msg) {
                        a.fil_sale = true;
                    }
                    a.vue_sale = true;
                    let effets = a.etat.appliquer(msg);
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
            // Le micro n'émet qu'en vocal, ni muet ni sourd.
            let arme = a.etat.vocal.is_some() && !a.muet && !a.sourd;
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
    reponse: Option<(UserId, u64)>,
) -> Result<(), String> {
    let a = partage.lock().unwrap();
    let texte = texte.trim().to_string();
    if texte.is_empty() {
        return Ok(());
    }
    let salon = a.etat.courant.ok_or("aucun salon ouvert")?;
    a.envoyer(ClientMsg::Chat {
        text: texte,
        reply_to: reponse.map(|(user_id, ts)| MsgRef { user_id, ts }),
        salon: Some(salon),
    });
    Ok(())
}

#[tauri::command]
fn reagir(partage: State<'_, Partage>, auteur: UserId, ts: u64, emoji: String, on: bool) {
    let a = partage.lock().unwrap();
    a.envoyer(ClientMsg::React { message: MsgRef { user_id: auteur, ts }, emoji, on });
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
        m.set_output_gain(if sourd { 0.0 } else { 1.0 });
    }
    a.envoyer(ClientMsg::VoiceState { speaking: false, muted: sourd });
}

/// Le volume d'une personne, 1.0 = 100 %.
#[tauri::command]
fn volume_membre(partage: State<'_, Partage>, id: UserId, gain: f32) {
    let moteur = partage.lock().unwrap().lien.engine.clone();
    let garde = moteur.lock().unwrap();
    if let Some(m) = garde.as_ref() {
        m.set_user_volume(id, gain.clamp(0.0, 2.0));
    }
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
    tauri::Builder::default()
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
        ])
        .run(tauri::generate_context!())
        .expect("lancement de l'appli");
}
