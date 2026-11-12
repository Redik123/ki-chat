//! ki-chat : client graphique (egui) — chat texte + vocal, orienté gaming.
//!
//! Ce fichier tient l'application elle-même ([`KiApp`]) : sa construction, la
//! session, le vocal, la boucle d'une image, la sauvegarde des réglages. Ce
//! qu'elle affiche vit dans un module par zone — `lanceur`, `salons`, `fil`,
//! `saisie`, `membres`, `barre`, `reglages_ui`, `admin_ui`, `clips_ui`,
//! `diffusion_ui`, `musique`, `recherche_ui` — et ce qui arrive du serveur
//! dans `reception`. Les zones qui ont leur propre état le rangent dans un
//! champ de `KiApp` (`admin`, `clips`, `diffusion`, `regard`, `musique`,
//! `recherche`, `diag`). L'audio a les siens dans le module `audio` : `audio`
//! pour les réglages qui se sauvent, `vocal` pour l'état du moment.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod appicon;
mod admin_fichiers;
mod atelier;
mod beta_ui;
mod brouillons;
mod cartes;
mod clips;
mod graphes;
mod icons;
mod images;
mod instance;
mod jeux;
mod loupedeck;
// Le serveur de test pour Claude : jamais dans une version publiée.
#[cfg(all(debug_assertions, windows))]
mod mcp;
mod loupedeck_config;
mod loupedeck_page;
mod loupedeck_ui;
mod medailles;
mod medias;
mod net;
mod overlay;
mod partage;
mod perf;
mod prefs;
mod photos;
mod porte_ui;
mod boutique;
mod ptt;
mod regard_detache;
mod reglages_audio;
mod reglages_casque;
mod egaliseur_ui;
mod studio_ui;
mod changeur_ui;
mod voisine_ui;
mod raccourci;
mod rangs;
mod secours;
mod secret;
mod servers;
mod sfxgen;
mod soundboard;
mod suggestions;
mod tactique;
mod theme;
mod ui;
mod update;
mod valorant;
mod valo_catalogue;
mod valo_page;
mod veille;
mod visionneuse;
mod zone;
mod audio;
mod admin_ui;
mod barre;
mod clips_ui;
mod diag;
mod diffusion_ui;
mod fil;
mod formats;
mod lanceur;
mod membres;
mod musique;
mod reception;
mod recherche_ui;
mod reglages_ui;
mod saisie;
mod salons;
mod televersement;

/// Sous `--features mesures`, toutes les allocations du processus passent par
/// un compteur. C'est ce qui rend vérifiable la cible « ~0 allocation par
/// image » de P3 : aujourd'hui le rendu recopie l'état complet (membres,
/// rôles, salons, journal) vingt fois par seconde, et l'on ne peut pas
/// corriger ce qu'on ne compte pas.
#[cfg(feature = "mesures")]
#[global_allocator]
static COMPTEUR: perf::alloc::Compteur = perf::alloc::Compteur;

use std::collections::HashMap;
use audio::{MicMode, BITRATES};

use eframe::egui::{self, RichText};
use ki_core::markup;
use ki_protocol::{
    ChannelId, ChannelInfo, ChatRecord, ClientMsg, IconChange, Member, MsgRef, ReplyRef, ServerInfo,
    UserId,
};
use ki_ui::jetons::{espace, marge, texte};

use admin_ui::AdminTab;
use fil::{lecture_effective, split_links, MenuMessage};
use icons::Icon;
use lanceur::ServerDraft;
use membres::{couleur_de_membre, empreinte_compte, est_bot, palier_de, MemberRow};
use reglages_ui::{DemandeReglages, Onglet};
use salons::VoicePrompt;
use televersement::LienPret;
use theme::{ACCENT, TEXT, TEXT_DIM};
use ui::Ton;

/// Les colonnes des salons (à gauche) et des membres (à droite) : leur
/// largeur d'origine, et ce qu'on peut leur retirer ou leur ajouter en
/// tirant leur bord. egui retient la largeur choisie d'une session à
/// l'autre ; ce qui ne tiendrait pas dans une colonne étroite est coupé à
/// son bord, jamais par-dessus la conversation.
const SIDEBAR_WIDTH: f32 = 248.0;
/// Combien de comptes Riot d'un membre on garde de `MesComptes` : le
/// serveur en lie trois au plus, la marge couvre une version future.
const MES_COMPTES_MAX: usize = 8;
/// Combien de comptes non liés « à taire » on retient, les plus récents.
const COMPTES_TUS_MAX: usize = 32;
const SIDEBAR_LARGEURS: std::ops::RangeInclusive<f32> = 200.0..=360.0;
const ROSTER_WIDTH: f32 = 210.0;
const ROSTER_LARGEURS: std::ops::RangeInclusive<f32> = 180.0..=340.0;

/// Le pied de la colonne des salons (mon compte) et celui de la
/// conversation (la saisie) : la même marge dessus et dessous, et la même
/// hauteur au repos — la carte du compte et la barre de saisie s'alignent.
const PIED_MARGE: f32 = 10.0;

/// À la fermeture, la connexion a une seconde et demie pour se fermer
/// proprement, le démontage (fils de veille, moteur vocal…) deux secondes :
/// au-delà, on n'attend plus — le garde-fou de fermeture finit le travail.
const FERMETURE_CONNEXION: std::time::Duration = std::time::Duration::from_millis(1500);
const FERMETURE_DEMONTAGE: std::time::Duration = std::time::Duration::from_secs(2);

/// Au-dessus de ce niveau crête, on considère que la personne parle, même si
/// son `VoiceState` ne nous est pas parvenu. Le niveau vient du tampon de
/// gigue et décroît tout seul : l'indicateur s'éteint donc sans message.
const SPEAK_LEVEL: f32 = 0.02;

fn main() -> eframe::Result {
    // Le banc des pages VALORANT, en version de debug : une fenêtre sur un
    // groupe inventé, photographiée scène par scène (valo_page/banc.rs).
    #[cfg(debug_assertions)]
    if let Some(dossier) = valo_page::banc::demande() {
        return valo_page::banc::lancer(dossier);
    }
    // Traces sur stderr ET dans un fichier, plus un panic hook : compilée
    // `panic = "abort"` sans console, l'application mourait sans témoin.
    let journal = secours::installer();
    match &journal {
        Some(chemin) => {
            tracing::info!("ki-chat {} démarre — journal : {}", update::current(), chemin.display());
        }
        None => tracing::warn!(
            "ki-chat {} démarre — journal sur disque indisponible, stderr seul",
            update::current()
        ),
    }
    let relances = secours::essais_actuels();
    if relances > 0 {
        tracing::info!("instance relancée automatiquement (tentative {relances})");
    }
    // Un seul ki-chat par session : lancé une seconde fois — depuis
    // l'icône du Bureau, alors qu'il est réduit à côté de l'horloge —, on
    // ramène le premier au premier plan et l'on se retire. S'il ne répond
    // pas, on ne se retire plus en silence : on le dit, et l'utilisateur
    // choisit de le fermer ou de le garder.
    let verrou = match instance::prendre() {
        instance::Demarrage::Premiere(verrou) => verrou,
        instance::Demarrage::DejaLancee => return Ok(()),
        instance::Demarrage::Bloquee(bloquee) => match bloquee.resoudre() {
            instance::Demarrage::Premiere(verrou) => verrou,
            _ => return Ok(()),
        },
    };
    // Confié au module : la fermeture le rend dès son début.
    verrou.garder();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 720.0])
            .with_min_inner_size([900.0, 560.0])
            .with_title("ki-chat")
            .with_icon(std::sync::Arc::new(theme::app_icon())),
        // eframe mémorisait la géométrie et la remettait au lancement — y
        // compris une origine sur un écran secondaire débranché depuis,
        // auquel cas la fenêtre revenait minuscule en haut à gauche.
        //
        // Attention : ce drapeau ne gouverne que la **sauvegarde**. La
        // relecture au démarrage n'est pas conditionnée par lui. C'est donc
        // `save()` qui fait le vrai travail, en vidant la clé « window » ;
        // sans valeur à relire, eframe laisse la position au système. Le
        // drapeau est ici pour qu'eframe ne la réécrive pas derrière nous.
        //
        // Seul l'état « maximisée » est repris, par nos soins (`update`).
        persist_window: false,
        ..Default::default()
    };
    // Le ki-chat de test a son propre profil : ni les serveurs ni les mots
    // de passe mémorisés de l'utilisateur, donc aucune connexion à son
    // compte sans qu'on la demande — et rien d'écrit dans ses réglages.
    #[cfg(all(debug_assertions, windows))]
    let options = if mcp::demande() {
        eframe::NativeOptions { persistence_path: Some(mcp::profil()), ..options }
    } else {
        options
    };
    // Les réglages, avant qu'eframe ne les ouvre : une copie quand ki-chat a
    // changé de version depuis qu'ils ont été écrits, puis leur migration.
    if let Some(chemin) = options.persistence_path.clone().or_else(|| Some(eframe::storage_dir("ki-chat")?.join("app.ron"))) {
        prefs::avant_demarrage(&chemin, update::current());
        prefs::designer(chemin);
    }
    let depart = std::time::Instant::now();
    let outcome = eframe::run_native(
        "ki-chat",
        options,
        Box::new(|cc| Ok(Box::new(KiApp::new(cc)))),
    );
    // Le verrou est rendu avant de relancer quoi que ce soit — d'ordinaire
    // dès le début de la fermeture (`on_exit`) : le successeur d'une mise à
    // jour ou d'une relance automatique doit pouvoir le prendre sans
    // attendre notre mort.
    instance::rendre();
    tracing::info!("fermeture : boucle graphique sortie, verrou d'instance rendu");
    match &outcome {
        // Une mise à jour installée ne prend effet qu'au prochain lancement :
        // on le déclenche ici, la fenêtre fermée — donc après que les
        // réglages ont été enregistrés et les périphériques audio rendus.
        Ok(()) => update::relaunch_if_requested(zone::repartir_reduit()),
        // eframe tient toute erreur de rendu pour fatale : un seul
        // `SwapBuffers` raté — pilote réinitialisé sous un jeu, veille,
        // bascule de GPU d'un portable — et sa boucle se termine, fenêtre
        // fermée, en rendant l'erreur ici. Le contexte graphique suivant
        // n'aura rien : on relance, plutôt que de laisser un hoquet d'une
        // image coûter la soirée. Budget borné : une erreur qui revient dès
        // le démarrage n'est pas un hoquet, on rend la main.
        Err(e) => {
            tracing::error!("boucle graphique terminée en erreur : {e}");
            // Dans le rapport de plantage aussi : il part au serveur à la
            // connexion suivante (voir `envoyer_crash`).
            secours::consigner_crash(&format!("boucle graphique terminée en erreur : {e}"));
            match secours::decision_relance(relances, depart.elapsed()) {
                Some(essais) => secours::relancer(essais, zone::repartir_reduit()),
                None => tracing::error!("l'erreur revient dès le démarrage — relances épuisées"),
            }
        }
    }
    outcome
}

/// Case où le thread du sélecteur de fichier dépose la vignette encodée,
/// ou le message d'erreur si l'image est illisible.
type PickedImage = std::sync::Arc<std::sync::Mutex<Option<Result<String, String>>>>;

/// Ce qu'on n'a pas lu dans un salon — de quoi peindre sa pastille.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct NonLu {
    /// Messages non lus. Le serveur compte sur mille au plus, le client
    /// affiche « 99+ » au-delà de la centaine.
    nb: u32,
    /// L'un d'eux me nomme : la pastille passe à l'accent.
    mention: bool,
    /// Tout message d'horodatage **supérieur** est non lu — c'est là que se
    /// pose le séparateur « nouveaux messages » à l'entrée du salon.
    depuis: u64,
}

impl NonLu {
    /// Un message de plus. `ts` ne fait reculer le repère que s'il n'y en
    /// avait pas encore : le premier non-lu fixe l'endroit du séparateur.
    fn ajouter(&mut self, ts: u64, mention: bool) {
        if self.nb == 0 {
            self.depuis = ts.saturating_sub(1);
        }
        self.nb = self.nb.saturating_add(1);
        self.mention |= mention;
    }
}

/// De quoi le client prévient — son et clignotement de la barre des tâches.
/// Les pastilles, elles, restent dans tous les cas : ne pas être dérangé
/// n'est pas ne pas savoir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Notif {
    /// Tout message d'un autre salon, et ceux du salon lu quand la fenêtre
    /// est à l'arrière-plan.
    Tout,
    /// Seulement quand on est nommé.
    Mentions,
    /// Jamais.
    Rien,
}

impl Notif {
    const TOUS: [Notif; 3] = [Notif::Tout, Notif::Mentions, Notif::Rien];

    fn label(self) -> &'static str {
        match self {
            Notif::Tout => "tout",
            Notif::Mentions => "mentions seulement",
            Notif::Rien => "rien",
        }
    }

    fn cle(self) -> &'static str {
        match self {
            Notif::Tout => "tout",
            Notif::Mentions => "mentions",
            Notif::Rien => "rien",
        }
    }

    fn depuis(cle: &str) -> Self {
        Self::TOUS.into_iter().find(|n| n.cle() == cle).unwrap_or(Notif::Tout)
    }

    /// Faut-il prévenir de ce message-là ?
    fn previent(self, pour_moi: bool) -> bool {
        match self {
            Notif::Tout => true,
            Notif::Mentions => pour_moi,
            Notif::Rien => false,
        }
    }
}

/// Le délai entre « j'ai vu » et le `Lu` envoyé au serveur : un par
/// conversation, pas un par message.
const DELAI_LU: std::time::Duration = std::time::Duration::from_secs(1);

/// Combien de temps « X te poke » reste affiché dans l'en-tête du salon.
const DUREE_POKE: std::time::Duration = std::time::Duration::from_secs(10);
/// Après un refus du serveur, le bouton « Poke » de cette cible reste
/// grisé le temps de lire le motif — et de ne pas insister.
const DELAI_REFUS_POKE: std::time::Duration = std::time::Duration::from_secs(30);

struct Reprise {
    /// Tentatives déjà faites. C'est elle qui espace les suivantes.
    essais: u32,
    /// Heure de la prochaine tentative.
    quand: std::time::Instant,
    /// Ce qu'on lisait, et où l'on parlait. Rendus après le `Welcome` : se
    /// reconnecter pour se retrouver ailleurs, en pleine partie, ne vaut
    /// guère mieux que de ne pas se reconnecter.
    salon: Option<ChannelId>,
    vocal: Option<ChannelId>,
    /// Le serveur perdu, par son adresse. Les numéros de salon ci-dessus
    /// n'ont de sens que chez lui : sans ce repère, choisir un autre
    /// serveur pendant le décompte y reconnectait, et y faisait entrer dans
    /// le salon vocal portant le même numéro, micro armé.
    adresse: String,
}

impl Reprise {
    /// Au-delà, on cesse et l'on rend la main. Avec le plafond ci-dessous,
    /// cela fait une vingtaine de minutes : de quoi couvrir un redémarrage de
    /// serveur, une mise à jour Windows, une box qui reboote. Au-delà, ce
    /// n'est plus un hoquet, et marteler n'y changera rien.
    const MAX: u32 = 40;
    /// Plafond de l'attente entre deux essais.
    const PLAFOND: std::time::Duration = std::time::Duration::from_secs(30);

    /// Attente avant la `n`-ième tentative : doublement, plafonné, puis tiré
    /// **au hasard entre la moitié et la totalité**.
    ///
    /// Le tirage n'est pas un raffinement. Trente clients qui perdent le
    /// serveur au même instant le retrouvent au même instant : sans
    /// dispersion, ils frappent tous ensemble, et chaque salve fait hacher
    /// trente Argon2id d'un coup — volontairement lent — sur une machine qui
    /// vient à peine de redémarrer. Ils échoueraient ensemble et
    /// recommenceraient ensemble, indéfiniment. C'est le troupeau classique,
    /// et il se soigne par le hasard, pas par un délai plus long.
    fn attente(essais: u32, alea: u64) -> std::time::Duration {
        let doublements = essais.saturating_sub(1).min(5);
        let base = std::time::Duration::from_secs(1 << doublements).min(Self::PLAFOND);
        let plein = base.as_millis() as u64;
        let moitie = plein / 2;
        std::time::Duration::from_millis(moitie + alea % (plein - moitie + 1))
    }
}

/// Une source de hasard qui n'a pas besoin d'en être une.
///
/// Disperser des reconnexions ne demande pas de cryptographie : il suffit que
/// deux machines ne tirent pas le même nombre. Les nanosecondes de l'horloge
/// suffisent, et évitent une dépendance de plus dans un client qui en compte
/// déjà assez.
fn alea() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::from(d.subsec_nanos()))
}

struct KiApp {
    /// Sonde les releases GitHub au démarrage et applique la mise à jour si
    /// l'utilisateur l'accepte.
    updater: update::Updater,
    // Carnet de serveurs
    book: Vec<servers::Server>,
    /// Serveur sélectionné dans le lanceur.
    selected: Option<u64>,
    /// Le serveur où l'on était quand l'application a été quittée : au
    /// lancement suivant, on s'y reconnecte sans rien cliquer. Effacé par
    /// une déconnexion voulue depuis l'application — et par elle seule.
    session_auto: Option<u64>,
    /// La reprise de session n'est tentée qu'une fois par lancement.
    auto_tente: bool,
    probes: servers::Probes,
    /// Formulaire ouvert par le bouton « + » ou le crayon.
    draft: Option<ServerDraft>,
    /// Textures des logos, montées à la demande. `None` = pas de logo (ou
    /// vignette illisible) : on retient l'échec pour ne pas réessayer.
    icon_textures: HashMap<u64, Option<egui::TextureHandle>>,
    /// Logo de serveur rapporté par le sélecteur de fichier, sur son thread.
    picked_icon: PickedImage,
    /// Photo de profil rapportée par le sélecteur. Case distincte du logo :
    /// les deux fenêtres peuvent être ouvertes en même temps.
    picked_avatar: PickedImage,
    /// Aperçu de vignette, indexé par la vignette elle-même : sans ce cache
    /// on téléverserait une texture à chaque image rendue.
    /// Aperçus de vignettes, **un par emplacement**.
    ///
    /// C'était un seul créneau partagé par « Mon compte » et Admin ▸ Serveur.
    /// Les deux panneaux ouverts en même temps se le disputaient : chaque
    /// image, la clé changeait, donc décodage PNG et téléversement GPU deux
    /// fois par image, indéfiniment.
    preview_icons: HashMap<Apercu, (String, egui::TextureHandle)>,
    /// Aperçus des images partagées dans le fil.
    previews: images::Previews,
    /// Fenêtre maximisée : la seule géométrie qu'on mémorise. Cf. `main`.
    maximized: bool,
    /// Vrai tant que `maximized` n'a pas été réappliqué au démarrage.
    restore_maximized: bool,
    /// Photos de profil montées : user_id -> (empreinte, texture).
    avatars: HashMap<UserId, (String, egui::TextureHandle)>,
    /// Vignettes (octets PNG) prêtes à monter, venues du réseau ou du
    /// cache disque. Le montage demande le contexte egui, d'où l'attente.
    incoming_avatars: HashMap<UserId, (String, Vec<u8>)>,
    /// Photo choisie dans « Mon compte », pas encore envoyée.
    account_avatar: IconChange,

    // Connexion
    /// Adresse du serveur sélectionné — le reste de l'appli s'en sert
    /// (base HTTP des fichiers, code d'invitation à recopier).
    url: String,
    username: String,
    password: String,
    remember_password: bool,
    invite: String,
    /// Le champ « code d'invitation » n'est affiché qu'à la demande.
    show_invite: bool,
    conn: Option<net::NetHandle>,
    connecting: bool,
    /// Début de la tentative en cours, pour la borner (voir
    /// `DELAI_CONNEXION`). `None` = pas de tentative en vol.
    connect_started: Option<std::time::Instant>,
    welcomed: bool,
    error: Option<String>,

    // État serveur
    my_id: Option<UserId>,
    /// Permissions effectives, annoncées par le serveur. L'interface s'en
    /// sert pour ne montrer que ce qui aboutira.
    my_perms: ki_protocol::Perms,
    /// Mon rang : on n'agit que sur strictement plus bas que soi.
    my_rank: u16,
    voice_token: u64,
    /// Le même jeton, en hexadécimal, partagé avec les fils qui durent
    /// (l'atelier : un export prend des minutes) — relu à chaque requête,
    /// mis à jour à chaque `Welcome`. Un fil qui aurait copié le jeton à
    /// son départ parlerait avec celui d'une session finie après une
    /// reconnexion, et le serveur lui répondrait « jeton invalide ».
    jeton_http: std::sync::Arc<std::sync::Mutex<net::JetonHttp>>,
    /// Les liens de fichiers envoyés, prêts à partir : le fil d'envoi les
    /// dépose, l'interface les poste par la connexion **du moment**, dans
    /// le salon d'où ils ont été envoyés (voir `envoyer_liens_prets`).
    liens_prets: std::sync::Arc<std::sync::Mutex<Vec<LienPret>>>,
    /// Le dernier message envoyé et pas encore revenu du serveur, avec
    /// l'instant de l'envoi. Le champ se vidait à l'envoi quoi qu'il arrive :
    /// tapé pendant une coupure pas encore détectée, un message partait dans
    /// le tampon d'une connexion morte et se perdait sans un mot.
    envoi_en_attente: Option<brouillons::Envoi>,
    /// Le serveur de test pour Claude, lancé par `--mcp` (debug seulement).
    #[cfg(all(debug_assertions, windows))]
    mcp: Option<mcp::Serveur>,
    /// Ce qui s'écrivait dans les salons qu'on ne regarde pas : chacun
    /// retrouve son texte, sa réponse ou sa modification quand on y revient.
    brouillons: brouillons::Brouillons,
    channels: Vec<ChannelInfo>,
    /// Tous les rôles du serveur, pour les couleurs et les badges.
    roles: Vec<ki_protocol::RoleInfo>,
    /// Salon textuel ouvert.
    current: Option<ChannelId>,
    /// Salon vocal où l'on se trouve. `None` = connecté au serveur sans
    /// être en vocal, ce qui est l'état à l'arrivée.
    voice_channel: Option<ChannelId>,
    /// Tout le monde sur le serveur, pas seulement le salon courant.
    members: Vec<Member>,
    messages: Vec<ChatRecord>,
    /// Le message auquel on est en train de répondre, rappelé au-dessus de
    /// la zone de saisie jusqu'à l'envoi (ou Échap).
    reponse_a: Option<ReplyRef>,
    /// Le message que l'on est en train de modifier : la zone de saisie
    /// porte son texte, Entrée l'envoie, Échap y renonce.
    edition: Option<MsgRef>,
    /// Le menu ouvert d'un clic droit sur un message.
    menu_message: Option<MenuMessage>,
    /// Effets sonores : nom court -> PCM 48 kHz mono. Les défauts synthétisés
    /// sont toujours là ; un .wav déposé dans « sons » les remplace.
    sounds: HashMap<String, Vec<f32>>,
    /// Événements dont le son vient d'un fichier (pour l'afficher).
    custom_sfx: std::collections::HashSet<String>,
    /// Événements coupés individuellement dans les réglages.
    sfx_muted: std::collections::HashSet<String>,
    sfx_on: bool,
    sfx_volume: f32,
    /// La fenêtre a-t-elle le focus ? (sons de message + notification)
    window_focused: bool,
    /// Demande de clignotement de la barre des tâches à honorer.
    wants_attention: bool,
    /// … et avec insistance : une mention clignote jusqu'au retour du
    /// focus, là où un simple message ne fait qu'un signe.
    attention_critique: bool,
    /// L'icône de la zone de notification et l'état « réduite » de la
    /// fenêtre (voir `zone.rs`).
    zone: zone::Zone,
    /// Réglage ⚙ : la croix réduit dans la zone au lieu de quitter.
    reduire_zone: bool,
    /// L'avertissement « ki-chat continue à côté de l'horloge » a été lu
    /// une fois ; persisté.
    zone_avertie: bool,
    /// L'avertissement est à l'écran : la croix attend la réponse.
    zone_dialogue: bool,
    /// On veut vraiment quitter (menu de l'icône, « Plutôt quitter », mise
    /// à jour) : la prochaine demande de fermeture passe.
    quitter: bool,
    /// Lancé avec `--reduit` (relance ou mise à jour pendant qu'on était
    /// réduit) : la fenêtre se crée, puis se réduit à la première image.
    reduire_au_demarrage: bool,
    /// Un poke reçu pendant la réduction : nommé dans le tooltip de
    /// l'icône jusqu'à la réouverture, là où le bandeau expire tout seul.
    zone_poke: Option<String>,
    /// Ce qui n'a pas été lu, par salon. Le salon courant y figure aussi
    /// quand on ne le regarde pas — fenêtre à l'arrière-plan, ou fil
    /// remonté dans le passé.
    non_lus: HashMap<ChannelId, NonLu>,
    /// Le serveur tient les repères de lecture : vrai dès le premier
    /// `NonLus` reçu. Avant ça — ou face à un serveur antérieur — on
    /// n'envoie jamais de `Lu`, qu'il prendrait pour un message invalide.
    serveur_gere_lus: bool,
    /// Le `Lu` en attente : le salon, et l'instant où l'envoyer.
    lu_a_envoyer: Option<(ChannelId, std::time::Instant)>,
    /// Le fil est-il collé en bas ? Un message qui arrive alors est vu ;
    /// un message qui arrive pendant qu'on relit le passé ne l'est pas.
    fil_en_bas: bool,
    /// Où poser « nouveaux messages » dans le fil du salon courant : avant
    /// le premier message d'horodatage supérieur. `None` : rien de neuf.
    separateur_nouveaux: Option<u64>,
    /// De quoi prévenir : tout, les mentions, rien.
    notif: Notif,
    /// Le titre envoyé à la fenêtre, pour ne le renvoyer que s'il change.
    titre_fenetre: String,
    /// « X te poke » : qui, et depuis quand — le bandeau s'efface tout
    /// seul après [`DUREE_POKE`]. À part de `error`, qu'une erreur sans
    /// rapport écraserait.
    poke_recu: Option<(String, std::time::Instant)>,
    /// Les portes web : bannières de demande, panneau, portes connues.
    portes: porte_ui::Portes,
    /// Est-ce que j'accepte les pokes ? Réglage ⚙, persisté.
    pokes_accepter: bool,
    /// Ce que le serveur sait de ce réglage : on ne lui envoie que les
    /// changements, comme `jeu_envoye`. `None` = rien envoyé encore.
    pokes_envoye: Option<bool>,
    /// Les cibles dont le poke vient d'être refusé, et quand : leur bouton
    /// reste grisé [`DELAI_REFUS_POKE`], le temps de lire le motif.
    pokes_refuses: HashMap<UserId, std::time::Instant>,
    /// Occupants du salon vocal à l'image précédente : c'est leur écart qui
    /// révèle une arrivée ou un départ. Les messages `UserJoined`/`UserLeft`
    /// portent sur le serveur entier, pas sur le salon vocal.
    prev_voice_peers: std::collections::HashSet<UserId>,
    /// Salon vocal de l'image précédente : quand il change, les occupants du
    /// nouveau salon sont adoptés EN SILENCE (entrer dans un salon peuplé ne
    /// déclenche pas six sons d'arrivée) — sans fenêtre temporelle, qui
    /// mangeait les sons dès qu'on changeait de salon rapidement.
    prev_voice_channel: Option<ChannelId>,
    /// Reste-t-il du passé à remonter dans ce salon ?
    history_more: bool,
    /// Une page est déjà demandée : on n'en réclame pas une seconde à
    /// chaque image tant que celle-ci n'est pas arrivée.
    history_pending: bool,
    /// Empreinte du certificat du serveur courant, pour épingler le HTTPS
    /// du partage de fichiers sur la même identité que le QUIC.
    server_fingerprint: String,
    /// Le client HTTP épinglé, et l'empreinte pour laquelle il l'est (voir
    /// `http_agent`).
    agent_http: std::cell::RefCell<Option<(String, ureq::Agent)>>,
    /// Adresse d'un serveur dont l'identité vient de changer. Tant qu'elle est
    /// posée, l'écran de connexion propose d'accepter la nouvelle — c'est la
    /// seule façon de se reconnecter à un serveur réinstallé.
    identity_alarm: Option<String>,
    /// Ce qu'on veut du vocal : un salon, ou en sortir.
    ///
    /// L'affichage suit cette intention le temps que le serveur la traite,
    /// puis c'est la liste des membres qui fait foi. Sans cette fenêtre, une
    /// liste produite avant notre clic nous remettait dans le salon qu'on
    /// venait de quitter ; sans sa **péremption**, un refus jamais notifié
    /// figeait l'affichage pour le reste de la session.
    voice_intent: Option<Option<ChannelId>>,
    /// Fin de la fenêtre pendant laquelle l'intention prime.
    voice_intent_until: std::time::Instant,
    /// Le fil doit descendre tout en bas à sa prochaine image : on vient
    /// d'entrer dans un salon, ou d'y sauter.
    fil_au_bas: bool,

    // UI
    input: String,
    /// Redonner le focus à la zone de saisie au prochain rendu.
    focus_input: bool,
    /// Le texte du champ vient d'être posé d'un coup (message repris pour
    /// le modifier, brouillon retrouvé) : le curseur va à sa fin. Il restait
    /// où il était — au début d'un champ vide —, et ce qu'on tapait pour
    /// compléter s'écrivait devant le message.
    curseur_a_la_fin: bool,
    /// Un export ou un import de réglages demandé par l'interface, à faire
    /// là où le fichier des réglages est à portée (`traiter_reglages`).
    reglages_demande: Option<DemandeReglages>,
    /// Un fichier de réglages choisi, en attente de confirmation : ce qu'il
    /// porte, et d'où il vient.
    import_propose: Option<(std::collections::BTreeMap<String, String>, String)>,
    /// Des réglages viennent d'être importés et ki-chat redémarre : ce qu'il
    /// a en mémoire ne doit plus s'écrire par-dessus.
    reglages_importes: bool,
    /// Ce que l'appli a à dire et qu'aucune fenêtre ouverte ne montre.
    toasts: ki_ui::toasts::Toasts,
    /// Le panneau des suggestions de la saisie : pseudos, commandes, emoji.
    suggestions: suggestions::Suggestions,
    show_settings: bool,
    /// Journal audio déplié dans les réglages.
    show_audio_journal: bool,
    /// Contexte egui, pour les fils qui doivent réveiller le repeint
    /// (décodeur du partage d'écran, notamment).
    app_ctx: egui::Context,
    /// Le stream que je regarde : son flux, son image, son volume, sa fenêtre.
    regard: diffusion_ui::MonRegard,
    /// Ma diffusion d'écran : ses réglages, le flux en cours, ce que le serveur en demande.
    diffusion: diffusion_ui::MaDiffusion,
    /// L'overlay « qui parle » par-dessus le jeu.
    overlay: overlay::Overlay,
    /// Partager son activité VALORANT (lue dans son propre client Riot).
    /// Désactivé de base : personne ne partage sans l'avoir choisi.
    valorant_presence: bool,
    /// Ajouter ses médailles à sa fiche : le serveur les lit chez HenrikDev
    /// (0.1.63), et son client Riot les donne dès la fin de chaque partie —
    /// sous « Partager mon activité », qui dit quand une partie finit.
    /// Décoché, le serveur l'apprend et les efface. Coché de base : lier
    /// son compte, c'est déjà montrer ses statistiques au groupe.
    valorant_medailles: bool,
    /// Les lectures de médailles prévues, et la dernière.
    medailles: medailles::Lecteur,
    /// Le serveur garde les médailles (`Welcome.medailles`) : sans cette
    /// preuve, rien ne lui part.
    serveur_medailles: bool,
    /// Le serveur lit lui-même les médailles chez HenrikDev, et veut
    /// savoir si on les accepte (`Welcome.choix_medailles`).
    serveur_choix_medailles: bool,
    /// Le serveur tient les tables de cartes (`Welcome.cartes`) : sans
    /// cette preuve, ni bouton ni geste.
    serveur_cartes: bool,
    /// Le serveur partage la carte tactique des salons vocaux
    /// (`Welcome.tactique`) : sans lui, chacun dessine sur la sienne.
    serveur_tactique: bool,
    /// Le serveur nous dit nos comptes Riot et lit lequel est ouvert
    /// (`Welcome.comptes_joues`) : sans lui, rien à classer.
    serveur_comptes_joues: bool,
    /// Nos comptes Riot liés, d'après le serveur (`MesComptes`) : de quoi
    /// reconnaître, ici, celui qui est ouvert dans le client Riot.
    mes_comptes: Vec<ki_protocol::MonCompte>,
    /// Les comptes non liés dont on ne veut plus entendre parler : des
    /// empreintes de puuid ([`empreinte_compte`]), gardées sur cette
    /// machine seulement.
    comptes_tus: Vec<String>,
    /// « Garder l'historique de mes parties » : décoché d'origine.
    journal_parties: bool,
    /// La bannière qui le propose a déjà eu sa réponse.
    journal_propose: bool,
    /// Le serveur tient ce journal (`Welcome.journal_parties`).
    serveur_journal: bool,
    /// Le choix déjà dit à ce serveur depuis la connexion.
    choix_journal_dit: Option<bool>,
    /// La table de cartes de mon salon vocal, et sa fenêtre.
    cartes: cartes::Cartes,
    /// La carte tactique : un plan de VALORANT où dessiner la manche
    /// suivante (V10.1 : chacun la sienne, rien ne part au serveur).
    tactique: tactique::Tactique,
    /// Le choix déjà dit à ce serveur depuis la connexion.
    choix_medailles_dit: Option<bool>,
    /// L'état VALORANT du tour d'avant, pour voir une partie finir.
    valorant_avant: Option<ki_protocol::JeuEtat>,
    /// Le dernier ennui des médailles consigné : on ne le répète pas.
    medailles_ennui: String,
    /// Le Riot ID en cours de saisie dans ⚙ → Jeu, et la dernière réponse
    /// du serveur à une liaison (réussie ou non, et pourquoi).
    riot_saisie: String,
    riot_message: Option<(bool, String)>,
    /// La fiche VALORANT ouverte au clic droit sur un membre.
    fiche: Option<FicheOuverte>,
    /// Le dernier message envoyé : un toutes les 1,5 s, pas plus.
    dernier_envoi: Option<std::time::Instant>,
    /// Les icônes de rang VALORANT, téléchargées une fois.
    rangs: rangs::Rangs,
    /// Les portraits d'agents, bandeaux de cartes et cartes de joueur de
    /// valorant-api.com, et les noms des modes sortis depuis.
    catalogue: valo_catalogue::Catalogue,
    /// La boutique du jour, lue dans son propre client Riot.
    boutique: boutique::Lecteur,
    /// Le bot musique : son état poussé par le serveur, et ce qu'on prépare pour lui
    /// (la recherche, ses résultats, le nom d'une playlist).
    musique: musique::Musique,
    /// La fiche d'un bot, ouverte depuis la liste des membres.
    fiche_bot: Option<UserId>,
    /// La visionneuse : une image ou une vidéo du chat, en grand.
    visionneuse: visionneuse::Visionneuse,
    /// L'atelier des clips (C3) : coupe, format téléphone, export.
    atelier: atelier::Atelier,
    /// Le soundboard : des sons à la touche, entendus par le salon.
    soundboard: soundboard::Soundboard,
    /// La sortie audio à part de la visionneuse, quand il n'y a pas de
    /// moteur vocal (hors salon) pour jouer sa file.
    sortie_medias: Option<ki_voice::medias::SortieSeule>,
    /// Les clips : l'enregistreur et ses réglages, la galerie, le partage et le retrait.
    clips: clips_ui::Clips,
    /// La page VALORANT du groupe (son état, ses deux fenêtres) et ce que
    /// le serveur en a envoyé : les fiches, les esports, l'activité.
    valo: valo_page::PageValo,
    stats: Vec<ki_protocol::FicheMembre>,
    stats_recu: bool,
    esports: Vec<ki_protocol::MatchEsport>,
    /// Les 168 cases jour × heure (UTC) des parties du groupe sur 30 jours.
    activite: Vec<u16>,
    /// Le contrôle de démarrage (diffusion précédente interrompue ?) est
    /// fait une fois, à la première image.
    demarrage_verifie: bool,
    /// La barre de titre de la fenêtre principale a été passée en sombre.
    barre_sombre: bool,
    /// Le fil qui lit le client Riot, tant que l'option est cochée.
    veilleur_valorant: Option<valorant::Veilleur>,
    /// Dire aux membres à quoi je joue — le jeu reconnu à sa fenêtre,
    /// sans rien lire dedans. Coché de base : c'est un nom, pas une partie.
    presence_jeux: bool,
    /// Le fil qui regarde les fenêtres, tant que l'option est cochée.
    veilleur_jeux: Option<jeux::Veilleur>,
    /// Le statut de jeu que le serveur connaît : on ne lui envoie que les
    /// changements.
    jeu_envoye: Option<ki_protocol::JeuStatut>,
    /// Docteur audio déplié dans les réglages.
    show_docteur: bool,
    /// Les diagnostics partagés : ce qui est déjà parti vers le serveur, et quand.
    diag: diag::Diag,
    /// Dernier diagnostic établi. Coûteux — il énumère les processus et
    /// interroge le registre — donc calculé au clic, pas à chaque image.
    docteur: Option<ki_voice::docteur::Diagnostic>,
    /// Relevé de performance déplié dans les réglages.
    show_perf: bool,
    /// Coût de l'interface, mesuré en continu (P1).
    perf: perf::Perf,
    /// Couleur de pseudo par auteur, reconstruite à chaque `Members`.
    ///
    /// Le fil la cherchait par un `find` **linéaire** dans la liste des
    /// membres, pour chaque message et à chaque image : cinq cents messages
    /// et trente membres, c'était quinze mille comparaisons par image, trois
    /// cent mille par seconde, pour un résultat qui ne change qu'à la
    /// réception d'un roster.
    author_colors: HashMap<UserId, egui::Color32>,
    input_devices: Vec<String>,
    output_devices: Vec<String>,
    upload_status: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    info: Option<String>,

    /// Identité du serveur courant, telle qu'il l'a annoncée.
    server_info: ServerInfo,

    /// La première image est passée : le journal le dit une fois. Un
    /// ki-chat qui tient le verrou d'instance sans arriver jusque-là — sans
    /// fenêtre — se reconnaît à l'absence de cette ligne.
    premiere_image: bool,
    /// L'instantané de la voix pris par `logique`, que l'image affiche
    /// ensuite (`interface`) : un seul par image.
    instantane_voix: Option<VoiceSnapshot>,
    /// L'administration : la fenêtre, ses onglets, et ce qu'on y saisit.
    admin: admin_ui::Admin,
    /// L'onglet ouvert de la fenêtre de réglages, mémorisé.
    reglages_onglet: Onglet,
    /// Salon vocal verrouillé dont on attend le mot de passe.
    voice_prompt: Option<VoicePrompt>,
    /// Les mots de passe des salons vocaux qu'on a posés ou tapés, le temps
    /// de la session : ils repartent avec chaque entrée dans ce salon, et
    /// celui qui pose un verrou n'a pas à retaper le sien — le serveur ne
    /// laisse plus entrer sans, admins compris, le propriétaire excepté.
    /// Oublié au premier refus.
    mots_de_passe_vocaux: HashMap<ChannelId, String>,

    /// Reprise automatique en cours. `None` = rien à reprendre.
    reprise: Option<Reprise>,
    /// Le moteur voix et ses attaches. **Hors** de la connexion, exprès :
    /// une coupure ne doit pas refermer le micro (voir `net::VoiceLink`).
    link: net::VoiceLink,

    /// La recherche dans l'historique : la requête, ses résultats, le saut dans
    /// le passé.
    recherche: recherche_ui::Recherche,

    // Mon compte
    show_account: bool,
    old_password: String,
    new_password: String,

    /// Les réglages audio, tels qu'ils se sauvent : micro, casque, traitements,
    /// raccourcis, volumes par personne.
    audio: audio::ReglagesAudio,
    /// L'état du vocal, qui ne se sauve pas : micro coupé, émission en cours, débit
    /// choisi par le réseau, mesures et essais en cours.
    vocal: audio::Vocal,
    // Labo vidéo (S1a du partage d'écran) : boucle locale de test.
    labo: Option<ki_video::LocalLoop>,
    labo_frame: std::sync::Arc<std::sync::Mutex<Option<ki_video::RgbaFrame>>>,
    labo_stats: std::sync::Arc<ki_video::StageStats>,
    labo_texture: Option<egui::TextureHandle>,
    /// Surveillance de la touche push-to-talk, sur son propre fil.
    ///
    /// `Option` parce qu'elle a besoin du contexte egui, qui n'existe qu'une
    /// fois la fenêtre ouverte : elle démarre à la première image.
    ptt: Option<ptt::Watcher>,
    /// La touche push-to-talk telle que le moteur vocal la lit : celle du
    /// clavier et le bouton du Loupedeck, branchés à leur démarrage.
    touche_ptt: ptt::TouchePtt,
    /// Le Loupedeck Live, sur son propre fil — démarré à la première image,
    /// comme `ptt` —, et ce que l'interface en retient : l'interrupteur des
    /// réglages, la page montrée, la personne choisie.
    loupedeck: Option<loupedeck::Loupedeck>,
    loupedeck_etat: loupedeck_ui::Etat,
    /// La hauteur de la barre de saisie sur une ligne, mesurée à l'image
    /// d'avant : le pied de la colonne des salons la prend pour s'aligner
    /// sur elle.
    hauteur_saisie: f32,
    /// Le bouton « Soundboard » de la barre du bas (onglet Bêta). D'origine,
    /// visible pour qui a déjà des sons dans ses dossiers, caché sinon.
    soundboard_visible: bool,
    /// La largeur de la partie gauche de la barre du bas quand ses boutons
    /// portent leur libellé, puis quand ils sont réduits à leur icône —
    /// chacune mesurée la dernière fois qu'elle a été dessinée ainsi — et
    /// celle de sa partie droite entière (vumètre, paquets, ping). De quoi
    /// savoir, d'une image à l'autre, ce qui tient : voir [`barre::niveau_de_barre`].
    barre_largeur_libelles: Option<f32>,
    barre_largeur_icones: Option<f32>,
    barre_largeur_droite: f32,
    /// Le serveur raconte la soirée (`Welcome.soiree`).
    serveur_soiree: bool,
    /// Le fil de la soirée (`ServerMsg::Soiree`), sa borne, et la dernière
    /// demande : il se relit toutes les deux minutes tant que l'accueil se
    /// montre.
    soiree: Vec<ki_protocol::PartieSoiree>,
    soiree_depuis: u64,
    soiree_demandee: Option<std::time::Instant>,
    /// Ma propre fiche, entière : « Moi » sur la carte de chaleur de
    /// l'accueil. Demandée une fois par connexion.
    ma_fiche: Option<ki_protocol::FicheValorant>,
    ma_fiche_demandee: bool,
    /// La fiche comparée à celle qui est ouverte (la vue Comparer du
    /// profil), demandée ou reçue.
    comparee: Option<FicheOuverte>,
    /// Le serveur relit le journal des parties (`Welcome.journal_lecture`),
    /// et mon journal tel qu'il l'a rendu, avec la dernière demande : il
    /// se redemande au plus toutes les cinq minutes.
    serveur_journal_lecture: bool,
    mon_journal: Option<Vec<ki_protocol::JourDeJeu>>,
    mon_journal_demande: Option<std::time::Instant>,
    /// Interdiction de veille système pendant le vocal.
    veille: veille::Garde,
}

/// Photo de l'état vocal prise une fois par frame, pour ne pas verrouiller
/// le moteur audio à répétition pendant le rendu.
#[derive(Default)]
struct VoiceSnapshot {
    engine_up: bool,
    stats: ki_voice::VoiceStats,
    ping: Option<u32>,
    levels: HashMap<u64, f32>,
    /// Périphérique audio perdu, en cours de réouverture : (micro, sortie).
    device_trouble: (bool, bool),
    /// Périphériques de repli en service : (micro, sortie).
    device_fallback: (bool, bool),
    /// Le moteur propose de basculer le micro en catégorie « communications »
    /// (micro affamé) et attend la réponse de l'utilisateur.
    comms_proposal: bool,
    /// Le micro qui ne livre rien, pour le nommer dans la proposition : son
    /// nom, et s'il est pris comme défaut de Windows. Relevé seulement quand
    /// une proposition est ouverte.
    micro_muet: Option<(String, bool)>,
}

impl KiApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // (le littéral Self plus bas est affecté à `app` pour charger les
        // sons juste après la construction)
        theme::install(&cc.egui_ctx);
        // Ce que la vidéo a d'important à dire rejoint le journal audio :
        // encodeur retenu, capture bridée par ce Windows, source perdue —
        // et voyage avec les diagnostics partagés.
        ki_video::set_journal(ki_voice::journal);
        // Les cartes graphiques et l'état de NVENC, relevés en arrière-plan
        // pour les rapports : « ça diffuse lentement chez lui » se lit alors
        // avec le modèle de carte et la version du pilote sous les yeux.
        ki_video::inventaire_lancer();
        // Les photos s'accumulent au fil des changements : on borne le
        // dossier une fois par démarrage.
        photos::prune();

        let get = |key: &str, default: &str| -> String {
            cc.storage
                .and_then(|s| s.get_string(key))
                .unwrap_or_else(|| default.to_string())
        };
        // Le carnet reprend l'unique adresse des versions précédentes ; on
        // rouvre sur le serveur utilisé en dernier.
        let book = servers::load(cc.storage);
        let selected = book
            .iter()
            .max_by_key(|s| s.last_used)
            .or_else(|| book.first())
            .map(|s| s.id);
        let active = selected.and_then(|id| book.iter().find(|s| s.id == id));

        // La vérification part maintenant et répond quand elle répond : le
        // lanceur s'affiche sans l'attendre.
        let refused = Some(get("update_skipped", "")).filter(|v| !v.is_empty());

        let mut app = Self {
            updater: update::Updater::start(refused, cc.egui_ctx.clone()),
            url: active.map(|s| s.address.clone()).unwrap_or_default(),
            username: active
                .map(|s| s.username.clone())
                .unwrap_or_else(|| get("username", "")),
            password: active.and_then(|s| s.password()).unwrap_or_default(),
            remember_password: active.is_some_and(|s| s.secret.is_some()),
            book,
            selected,
            session_auto: get("session_auto", "").parse().ok(),
            auto_tente: false,
            probes: servers::Probes::default(),
            draft: None,
            icon_textures: HashMap::new(),
            picked_icon: Default::default(),
            picked_avatar: Default::default(),
            preview_icons: HashMap::new(),
            previews: images::Previews::default(),
            maximized: get("window_maximized", "") == "on",
            restore_maximized: get("window_maximized", "") == "on",
            avatars: HashMap::new(),
            incoming_avatars: HashMap::new(),
            account_avatar: IconChange::Keep,
            server_info: ServerInfo::default(),
            admin: admin_ui::Admin {
                ouverte: false,
                nom: String::new(),
                adresse_web: String::new(),
                icone: IconChange::Keep,
                comptes: Vec::new(),
                invitations: Vec::new(),
                derniere_invitation: None,
                reset_cible: None,
                reset_mot_de_passe: String::new(),
                audit: Vec::new(),
                onglet: AdminTab::Server,
                fichiers: admin_fichiers::Fichiers::new(),
                invitation_usages: Some(1),
                invitation_libelle: String::new(),
                ban: None,
                role: None,
                salon: None,
                salon_edite: None,
                verrou: None,
                roles_cible: None,
                roles_choisis: Vec::new(),
                diag: Default::default(),
                diag_versions: Default::default(),
                tableau: Default::default(),
                tableau_demande: None,
                tableau_en_vol: Default::default(),
                reseau: Default::default(),
                reseau_demande: None,
                reseau_en_vol: Default::default(),
            },
            invite: String::new(),
            show_invite: false,
            conn: None,
            connecting: false,
            connect_started: None,
            welcomed: false,
            error: None,
            my_id: None,
            my_perms: 0,
            my_rank: 0,
            roles: Vec::new(),
            voice_token: 0,
            jeton_http: Default::default(),
            liens_prets: Default::default(),
            envoi_en_attente: None,
            #[cfg(all(debug_assertions, windows))]
            mcp: mcp::demande().then(|| mcp::Serveur::demarrer(cc.egui_ctx.clone())),
            brouillons: Default::default(),
            channels: Vec::new(),
            current: None,
            voice_channel: None,
            members: Vec::new(),
            messages: Vec::new(),
            sounds: HashMap::new(),   // rempli par reload_sounds() ci-dessous
            custom_sfx: std::collections::HashSet::new(),
            sfx_muted: get("sfx_muted", "")
                .split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            sfx_on: get("sfx_on", "on") == "on",
            sfx_volume: get("sfx_volume", "0.6").parse().unwrap_or(0.6),
            window_focused: true,
            wants_attention: false,
            attention_critique: false,
            zone: zone::Zone::creer(hwnd_de(cc), cc.egui_ctx.clone()),
            reduire_zone: get("reduire_zone", "on") == "on",
            zone_avertie: get("zone_avertie", "off") == "on",
            zone_dialogue: false,
            quitter: false,
            reduire_au_demarrage: zone::demande_au_demarrage(),
            zone_poke: None,
            non_lus: HashMap::new(),
            serveur_gere_lus: false,
            lu_a_envoyer: None,
            fil_en_bas: true,
            separateur_nouveaux: None,
            notif: Notif::depuis(&get("notif", "tout")),
            titre_fenetre: String::new(),
            poke_recu: None,
            portes: porte_ui::Portes::new(),
            pokes_accepter: get("pokes", "on") == "on",
            pokes_envoye: None,
            pokes_refuses: HashMap::new(),
            prev_voice_peers: std::collections::HashSet::new(),
            prev_voice_channel: None,
            history_more: false,
            history_pending: false,
            server_fingerprint: String::new(),
            agent_http: std::cell::RefCell::new(None),
            identity_alarm: None,
            voice_intent: None,
            voice_intent_until: std::time::Instant::now(),
            fil_au_bas: true,
            input: String::new(),
            focus_input: false,
            curseur_a_la_fin: false,
            reglages_demande: None,
            import_propose: None,
            reglages_importes: false,
            toasts: Default::default(),
            suggestions: suggestions::Suggestions::default(),
            show_settings: false,
            show_audio_journal: false,
            app_ctx: cc.egui_ctx.clone(),
            diffusion: diffusion_ui::MaDiffusion {
                reglages: partage::Reglages::load(get),
                sources: Default::default(),
                ouverte: false,
                live: None,
                attente: None,
                texture: None,
                palier: None,
                montant: false,
                basse: None,
                regulateur: partage::Regulateur::new(),
                spectateurs: HashMap::new(),
                cadence: partage::Cadence::new(),
                journal_flux: std::time::Instant::now(),
                pilote_averti: false,
            },
            overlay: overlay::Overlay::load(get),
            regard: diffusion_ui::MonRegard {
                flux: None,
                texture: None,
                cadence: partage::Cadence::new(),
                volume: get("regard_volume", "1")
                    .parse::<f32>()
                    .ok()
                    .filter(|v| v.is_finite())
                    .map_or(1.0, |v| v.clamp(0.0, 2.0)),
                meme_machine: false,
                detache: false,
                plein_ecran: false,
                fenetre: None,
            },
            valorant_presence: get("valorant_presence", "off") == "on",
            valorant_medailles: get("valorant_medailles", "on") == "on",
            medailles: medailles::Lecteur::new(),
            serveur_medailles: false,
            serveur_choix_medailles: false,
            serveur_cartes: false,
            serveur_tactique: false,
            serveur_comptes_joues: false,
            mes_comptes: Vec::new(),
            comptes_tus: get("comptes_tus", "").split(',').filter(|e| !e.is_empty()).map(str::to_string).collect(),
            journal_parties: get("journal_parties", "off") == "on",
            journal_propose: get("journal_propose", "non") == "oui",
            serveur_journal: false,
            choix_journal_dit: None,
            cartes: cartes::Cartes::default(),
            tactique: tactique::Tactique::default(),
            choix_medailles_dit: None,
            valorant_avant: None,
            medailles_ennui: String::new(),
            riot_saisie: String::new(),
            riot_message: None,
            fiche: None,
            dernier_envoi: None,
            rangs: rangs::Rangs::new(),
            catalogue: valo_catalogue::Catalogue::new(),
            boutique: boutique::Lecteur::new(),
            musique: musique::Musique {
                etat: ki_protocol::EtatMusique::default(),
                recu: std::time::Instant::now(),
                deroulee: false,
                recherche: String::new(),
                soundcloud: false,
                resultats: Vec::new(),
                resultats_pour: String::new(),
                playlist_nom: String::new(),
            },
            fiche_bot: None,
            visionneuse: visionneuse::Visionneuse::new(
                get("visionneuse_volume", "0.8").parse().unwrap_or(0.8),
            ),
            // Remplacé juste après : l'atelier partage la file « médias »
            // de la visionneuse, qui n'existe pas encore ici.
            atelier: atelier::Atelier::new(ki_voice::medias::File::new(), 0.8),
            soundboard: soundboard::Soundboard::load(get),
            sortie_medias: None,
            clips: clips_ui::Clips {
                reglages: clips::Reglages::load(get),
                enregistreur: None,
                branche: None,
                ouverte: false,
                partage: None,
                liste: Vec::new(),
                vignettes: HashMap::new(),
                vignettes_en_vol: std::collections::HashSet::new(),
                vignettes_recues: Default::default(),
                suppression: None,
                retrait: None,
                retrait_resultat: None,
                dossier_choisi: Default::default(),
            },
            valo: valo_page::PageValo::load(get),
            stats: Vec::new(),
            stats_recu: false,
            esports: Vec::new(),
            activite: Vec::new(),
            demarrage_verifie: false,
            barre_sombre: false,
            veilleur_valorant: None,
            presence_jeux: get("presence_jeux", "on") == "on",
            veilleur_jeux: None,
            jeu_envoye: None,
            show_docteur: false,
            docteur: None,
            diag: diag::Diag {
                partage: get("diag_share", "off") == "on",
                dernier_ts: 0,
                dernier_envoi: None,
                docteur_envoye: false,
                crash_envoye: get("diag_crash_envoye", ""),
                crash_verifie: false,
            },
            show_perf: false,
            perf: perf::Perf::default(),
            author_colors: HashMap::new(),
            input_devices: Vec::new(),
            output_devices: Vec::new(),
            upload_status: Default::default(),
            info: None,
            premiere_image: false,
            instantane_voix: None,
            reglages_onglet: Onglet::depuis(&get("reglages_onglet", "audio")),
            reponse_a: None,
            edition: None,
            menu_message: None,
            voice_prompt: None,
            mots_de_passe_vocaux: HashMap::new(),
            reprise: None,
            link: net::VoiceLink::default(),
            recherche: recherche_ui::Recherche {
                ouverte: false,
                requete: String::new(),
                ici: get("search_ici", "on") != "off",
                resultats: Vec::new(),
                encore: false,
                envoyee: None,
                focus: false,
                retour_present: None,
            },
            show_account: false,
            old_password: String::new(),
            new_password: String::new(),
            audio: audio::ReglagesAudio::load(get),
            vocal: audio::Vocal::new(),
            labo: None,
            labo_frame: Default::default(),
            labo_stats: Default::default(),
            labo_texture: None,
            ptt: None,
            touche_ptt: ptt::TouchePtt::default(),
            loupedeck: None,
            loupedeck_etat: loupedeck_ui::Etat::new(get("loupedeck", "off") == "on", &get("loupedeck_config", "")),
            hauteur_saisie: 44.0,
            barre_largeur_libelles: None,
            barre_largeur_icones: None,
            barre_largeur_droite: 0.0,
            soundboard_visible: match get("soundboard_visible", "").as_str() {
                "on" => true,
                "off" => false,
                _ => soundboard::Soundboard::a_des_sons(),
            },
            serveur_soiree: false,
            soiree: Vec::new(),
            soiree_depuis: 0,
            soiree_demandee: None,
            ma_fiche: None,
            ma_fiche_demandee: false,
            comparee: None,
            serveur_journal_lecture: false,
            mon_journal: None,
            mon_journal_demande: None,
            veille: veille::Garde::default(),
        };
        // Instance relancée par le dispositif de secours : le dire. Sans ce
        // bandeau, la fenêtre qui se ferme puis revient passe pour un caprice,
        // et personne ne sait qu'un journal attend d'être lu.
        if secours::essais_actuels() > 0 {
            app.error = Some(match secours::chemin_journal() {
                Some(chemin) => format!(
                    "ki-chat s'est relancé : la fenêtre précédente s'est fermée sur une \
                     erreur graphique (pilote réinitialisé par un jeu, veille…). \
                     Détails : {}",
                    chemin.display()
                ),
                None => "ki-chat s'est relancé : la fenêtre précédente s'est fermée sur \
                         une erreur graphique (pilote réinitialisé par un jeu, veille…)"
                    .into(),
            });
        }
        // L'atelier partage la file « médias » de la visionneuse : une seule
        // sortie son pour les deux.
        app.atelier = atelier::Atelier::new(app.visionneuse.file.clone(), app.visionneuse.volume);
        app.reload_sounds();
        app
    }

    /// (Re)charge les effets sonores : les défauts synthétisés d'abord, puis
    /// les .wav du dossier « sons » qui remplacent ceux du même nom.
    fn reload_sounds(&mut self) {
        let disk = load_sounds();
        self.custom_sfx = disk.keys().cloned().collect();
        let mut sounds = sfxgen::defaults();
        sounds.extend(disk);
        tracing::info!(
            "effets sonores : {} chargés dont {} personnalisés",
            sounds.len(),
            self.custom_sfx.len()
        );
        self.sounds = sounds;
    }

    /// Démarre la boucle locale du labo vidéo (S1a) : capture de l'écran
    /// principal, aller-retour H.264 complet, image déposée pour l'UI.
    fn start_labo(&mut self, ctx: egui::Context) {
        let stats = std::sync::Arc::new(ki_video::StageStats::default());
        let frame_slot = self.labo_frame.clone();
        let sink: ki_video::FrameSink = std::sync::Arc::new(move |frame| {
            *frame_slot.lock().unwrap() = Some(frame);
            // Chaque trame capturée réveille la fenêtre. C'était le seul
            // moyen de dépasser les 20 images par seconde du repeint
            // périodique ; depuis que celui-ci est conditionnel, c'est
            // devenu le seul moyen d'en avoir tout court — et c'est la bonne
            // façon : la vidéo peint quand elle a quelque chose à montrer,
            // pas à une cadence devinée d'avance.
            ctx.request_repaint();
        });
        match ki_video::LocalLoop::start(stats.clone(), sink) {
            Ok(handle) => {
                self.labo_stats = stats;
                self.labo_texture = None;
                self.labo = Some(handle);
            }
            Err(e) => self.error = Some(format!("labo vidéo : {e:#}")),
        }
    }

    fn stop_labo(&mut self) {
        if let Some(labo) = self.labo.take() {
            labo.stop();
        }
        *self.labo_frame.lock().unwrap() = None;
        self.labo_texture = None;
    }

    /// Fenêtre du labo : l'image décodée + les compteurs par étage.
    fn labo_window(&mut self, ctx: &egui::Context) {
        // Nouvelle image ? On met la texture à jour (allocation unique,
        // `set` ensuite — le pattern egui pour la vidéo).
        if let Some(frame) = self.labo_frame.lock().unwrap().take() {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [frame.width, frame.height],
                &frame.rgba,
            );
            match &mut self.labo_texture {
                Some(tex) if tex.size() == [frame.width, frame.height] => {
                    tex.set(image, egui::TextureOptions::LINEAR);
                }
                _ => {
                    self.labo_texture = Some(ctx.load_texture(
                        "labo-video",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
            self.labo_stats.painted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        let mut open = true;
        egui::Window::new("🧪 Labo vidéo — aller-retour H.264 local")
            .open(&mut open)
            .default_width(820.0)
            .show(ctx, |ui| {
                if let Some(tex) = &self.labo_texture {
                    let [w, h] = tex.size();
                    let avail = ui.available_width().max(320.0);
                    let scale = (avail / w as f32).min(1.0);
                    ui.image((tex.id(), egui::vec2(w as f32 * scale, h as f32 * scale)));
                } else {
                    ui.label("démarrage de la capture…");
                }
                ui.add_space(espace::S);
                ui.label(
                    RichText::new(self.labo_stats.summary()).weak().size(texte::PETIT).monospace(),
                );
            });
        if !open {
            self.stop_labo();
        }
    }

    /// Photo de l'état du moteur audio pour la frame en cours.
    fn voice_snapshot(&self) -> VoiceSnapshot {
        // Le ping n'existe que connecté ; le moteur, lui, tourne même
        // pendant une coupure — et c'est voulu : le vumètre du micro
        // continue de bouger, les réglages audio restent utilisables.
        let ping = self.conn.as_ref().and_then(|c| c.rtt_ms());
        match self.link.engine.lock().unwrap().as_ref() {
            Some(engine) => {
                let comms_proposal = engine.comms_proposal();
                VoiceSnapshot {
                    engine_up: true,
                    stats: engine.stats(),
                    ping,
                    levels: engine.user_levels().into_iter().collect(),
                    device_trouble: engine.device_trouble(),
                    device_fallback: engine.device_fallback(),
                    comms_proposal,
                    micro_muet: comms_proposal.then(|| engine.micro_en_service()).flatten(),
                }
            }
            None => VoiceSnapshot {
                ping,
                ..Default::default()
            },
        }
    }

    /// Mon pseudo **tel que le serveur le connaît**.
    ///
    /// Et non celui tapé dans le formulaire de connexion : le serveur le
    /// rogne, et une mention se compare à ce qu'il diffuse, pas à ce qu'on a
    /// saisi.
    fn my_pseudo(&self) -> Option<&str> {
        let id = self.my_id?;
        self.members
            .iter()
            .find(|m| m.user_id == id)
            .map(|m| m.username.as_str())
    }

    /// Volume mémorisé pour un utilisateur (1.0 = 100 %).
    fn volume_of(&self, user_id: UserId) -> f32 {
        self.audio.all_volumes
            .get(self.username.trim())
            .and_then(|v| v.get(&user_id))
            .copied()
            .unwrap_or(1.0)
    }

    /// Mémorise et applique à chaud le volume d'un utilisateur.
    fn set_volume(&mut self, user_id: UserId, gain: f32) {
        let my = self.username.trim().to_string();
        let volumes = self.audio.all_volumes.entry(my).or_default();
        if (gain - 1.0).abs() < 0.001 {
            volumes.remove(&user_id);
        } else {
            volumes.insert(user_id, gain);
        }
        if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
            engine.set_user_volume(user_id, gain);
        }
    }

    /// Base HTTP du serveur (partage de fichiers) : même hôte que le QUIC,
    /// port HTTP conventionnel 8080.
    fn http_base(&self) -> String {
        let trimmed = self.url.trim();
        let host = trimmed.rsplit_once(':').map(|(h, _)| h).unwrap_or(trimmed);
        // HTTPS : le partage de fichiers portait le jeton de session et le
        // contenu des fichiers en clair, à côté d'un tunnel QUIC chiffré.
        format!("https://{host}:8080")
    }

    /// La base des liens des portes : l'adresse web réglée par un admin
    /// (Admin → Serveur), sinon celle par laquelle on joint le serveur en
    /// HTTPS — même hôte que le QUIC, port 8080.
    fn base_web(&self) -> String {
        if self.server_info.adresse_web.is_empty() {
            self.http_base()
        } else {
            self.server_info.adresse_web.clone()
        }
    }

    /// Client HTTP épinglé sur l'empreinte du serveur courant.
    ///
    /// Le certificat étant celui du QUIC, la même empreinte fait foi : une
    /// seule identité à vérifier, et rien qui parte vers un imposteur.
    ///
    /// Gardé tant que l'empreinte ne change pas : il était reconstruit à chaque
    /// image (les aperçus le reprennent à chaque passage), et chaque agent neuf
    /// arrivait avec un réservoir de connexions vide — une poignée de main TLS
    /// par requête au lieu d'une connexion réutilisée. `https_only` : tout ce
    /// qu'il sert est en HTTPS, une redirection vers du clair n'est pas suivie.
    fn http_agent(&self) -> ureq::Agent {
        let mut cache = self.agent_http.borrow_mut();
        if let Some((empreinte, agent)) = cache.as_ref() {
            if *empreinte == self.server_fingerprint {
                return agent.clone();
            }
        }
        let expected =
            (!self.server_fingerprint.is_empty()).then_some(self.server_fingerprint.as_str());
        let agent = ureq::AgentBuilder::new()
            .tls_config(ki_client_quic::pinned_tls_config(expected))
            .https_only(true)
            .build();
        *cache = Some((self.server_fingerprint.clone(), agent.clone()));
        agent
    }

    /// La page VALORANT, à la place du chat : ses onglets, l'accueil et le
    /// profil. « Fermer » ou Échap la referment, avec la
    /// fiche ouverte.
    fn valo_panel(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.preparer_valo(&ctx);
        self.preparer_fiche(&ctx);
        if self.valo.sur_l_accueil() {
            self.demander_soiree();
        }
        // Ma fiche, une fois : « Moi » sur la carte de chaleur.
        if !self.ma_fiche_demandee {
            if let Some(moi) = self.my_id.filter(|_| !self.mes_comptes.is_empty()) {
                self.ma_fiche_demandee = true;
                self.send(ClientMsg::FicheValorant { user_id: moi });
            }
        }
        let mut boutique = std::mem::take(&mut self.boutique);
        let (ouvert, demandes) = {
            let cx = valo_page::Contexte {
                stats: &self.stats,
                recu: self.stats_recu,
                esports: &self.esports,
                activite: &self.activite,
                moi: self.my_id,
                membres: &self.members,
                rangs: &self.rangs,
                catalogue: &self.catalogue,
                soiree: &self.soiree,
                soiree_servie: self.serveur_soiree,
                ma_fiche: self.ma_fiche.as_ref(),
                photos: &self.avatars,
                comparee: self.comparee.as_ref(),
                journal: valo_page::Journal { servi: self.serveur_journal_lecture, coche: self.journal_parties, jours: self.mon_journal.as_deref() },
            };
            let fiche = self.fiche.as_ref();
            let valo = &mut self.valo;
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(theme::BG_BASE).inner_margin(marge::egale(espace::XL)))
                .show(ui, |ui| valo.centre(ui, &cx, fiche, &mut boutique))
                .inner
        };
        self.boutique = boutique;
        self.executer_demandes_valo(demandes);
        if !ouvert {
            self.retour_au_chat();
        }
    }

    /// La page VALORANT qui tient la place du chat se referme, avec sa
    /// fiche : « Fermer », Échap, ou un clic sur un salon textuel.
    fn retour_au_chat(&mut self) {
        if self.valo.ouvert || self.fiche.is_some() {
            self.valo.ouvert = false;
            self.fiche = None;
            self.comparee = None;
            self.valo.fermer_fiche();
        }
    }

    /// Le fil de la soirée : demandé à l'ouverture de l'accueil, puis toutes
    /// les deux minutes, et dès que la soirée change (6 h du matin).
    fn demander_soiree(&mut self) {
        if !self.serveur_soiree {
            return;
        }
        let depuis = valo_page::debut_de_soiree(chrono::Local::now());
        let vieille = self.soiree_demandee.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(120));
        if vieille || depuis != self.soiree_depuis {
            self.soiree_depuis = depuis;
            self.soiree_demandee = Some(std::time::Instant::now());
            self.send(ClientMsg::SoireeValorant { depuis });
        }
    }

    /// Ce que la page VALORANT demande : actualiser, ouvrir une fiche ou la
    /// carte tactique.
    fn executer_demandes_valo(&mut self, demandes: Vec<valo_page::Demande>) {
        for demande in demandes {
            match demande {
                valo_page::Demande::Actualiser => {
                    self.ouvrir_stats();
                    self.soiree_demandee = None;
                    self.mon_journal = None;
                    self.mon_journal_demande = None;
                }
                valo_page::Demande::OuvrirFiche(user_id, username) => self.ouvrir_fiche(user_id, username),
                valo_page::Demande::OuvrirTactique => {
                    self.tactique.ouvert = true;
                    self.catalogue.demarrer();
                }
                valo_page::Demande::Comparer(user_id, username) => {
                    if self.comparee.as_ref().is_none_or(|c| c.user_id != user_id) {
                        self.comparee = Some(FicheOuverte { user_id, username, recue: false, fiche: None });
                        self.send(ClientMsg::FicheValorant { user_id });
                    }
                    self.valo.vers_comparer();
                }
                valo_page::Demande::MonJournal => {
                    let vieille = self.mon_journal_demande.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(300));
                    if self.serveur_journal_lecture && vieille {
                        self.mon_journal_demande = Some(std::time::Instant::now());
                        self.send(ClientMsg::MonJournal { decalage_min: valo_page::decalage_journal() });
                    }
                }
            }
        }
    }

    /// Les icônes de rang que la fiche ouverte montrera (le rang, le pic,
    /// le palier suivant, le rang de chaque match), et le catalogue.
    fn preparer_fiche(&mut self, ctx: &egui::Context) {
        let Some(f) = &self.fiche else { return };
        if let Some(fiche) = &f.fiche {
            let mut tiers: Vec<u8> = vec![fiche.rang.tier, fiche.rang.tier.saturating_add(1)];
            tiers.extend(fiche.pic.iter().map(|p| p.tier));
            tiers.extend(fiche.matchs.iter().map(|m| m.tier));
            tiers.sort_unstable();
            tiers.dedup();
            for tier in tiers {
                self.rangs.preparer(ctx, tier);
            }
        }
        self.catalogue.demarrer();
    }

    // -----------------------------------------------------------------
    // Clips (PLAN-CLIPS.md, C1)
    // -----------------------------------------------------------------

    /// Retour depuis l'icône : la fenêtre revient, l'icône se tait, et ce
    /// qu'on avait sous les yeux est lu comme au retour du focus.
    fn rouvrir_depuis_zone(&mut self, ctx: &egui::Context) {
        if self.zone.reduite() {
            self.zone.rouvrir(ctx);
        } else {
            // Déjà ouverte, derrière le jeu : on la ramène devant.
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        self.zone_poke = None;
    }

    /// La première fois que la croix réduit : on le dit, avant de le
    /// faire. « Compris » réduit et ne redemandera plus ; « Plutôt
    /// quitter » ferme pour de bon (le réglage reste coché : c'est le
    /// choix d'une fois, pas un réglage).
    fn zone_dialogue_window(&mut self, ctx: &egui::Context) {
        if !self.zone_dialogue {
            return;
        }
        let mut decision: Option<bool> = None;
        egui::Window::new("ki-chat continue dans la zone de notification")
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "La croix ne quitte pas ki-chat : il continue à côté de l'horloge, \
                         et tu reçois toujours les messages, les sons, les pokes et le vocal.",
                    )
                    .color(TEXT),
                );
                ui::hint(
                    ui,
                    "clic sur l'icône pour rouvrir, « Quitter » dans son menu pour partir ; \
                     réglable dans ⚙ Sons & notifications",
                );
                ui.add_space(espace::M);
                ui.horizontal(|ui| {
                    if ui::Bouton::new("Compris").icone(Icon::Check).principal().show(ui).clicked() {
                        decision = Some(true);
                    }
                    if ui::Bouton::new("Plutôt quitter").icone(Icon::Logout).show(ui).clicked() {
                        decision = Some(false);
                    }
                });
            });
        match decision {
            Some(true) => {
                self.zone_dialogue = false;
                self.zone_avertie = true;
                self.zone.reduire(ctx);
            }
            Some(false) => {
                self.zone_dialogue = false;
                self.quitter = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            None => {}
        }
    }

    /// Ce que le rendu des portes doit savoir de moi : mes permissions,
    /// mon vocal, les salons. Le serveur tranche de toute façon ; ici on
    /// ne fait que cacher ce qu'il refuserait.
    fn contexte_portes(&self) -> porte_ui::Contexte<'_> {
        use ki_protocol::perm::*;
        porte_ui::Contexte {
            peut_ouvrir: self.can(CREATE_INVITE),
            peut_expulser: self.can(KICK),
            mon_vocal: self.voice_channel,
            salons: &self.channels,
            base_web: self.base_web(),
        }
    }

    /// Le panneau « Portes web », s'il est ouvert, et ce qu'on y a demandé.
    fn portes_window(&mut self, ctx: &egui::Context) {
        if !self.portes.ouvert {
            return;
        }
        // Le contexte emprunte `self`, le panneau modifie `self.portes` :
        // on sort les portes le temps du rendu, comme le roster.
        let mut portes = std::mem::take(&mut self.portes);
        let actions = portes.fenetre(ctx, &self.contexte_portes());
        self.portes = portes;
        self.appliquer_portes(actions);
    }

    /// Ce que le panneau, la bannière ou le menu d'un invité ont demandé.
    fn appliquer_portes(&mut self, actions: Vec<porte_ui::Action>) {
        for action in actions {
            match action {
                porte_ui::Action::Envoyer(msg) => self.send(msg),
                porte_ui::Action::Copier(texte) => self.app_ctx.copy_text(texte),
                porte_ui::Action::Lire(salon) => {
                    if self.current != Some(salon) {
                        self.join(salon);
                    }
                }
                // « Lui offrir ki-chat » : c'est le serveur qui crée le
                // code, poste le lien dans le salon et pousse le code à la
                // page de l'invité — jamais dans le salon, lu par des
                // inconnus. Il nous répond par un `Info` qui porte le code.
                porte_ui::Action::Offrir { invite_id } => self.send(ClientMsg::PorteOffrir { invite_id }),
            }
        }
    }

    /// Le soundboard : la fenêtre, et ce qu'elle demande au moteur vocal.
    /// Sans moteur (hors connexion), rien ne joue — la fenêtre le dit.
    /// La table de cartes de mon salon vocal, et mes gestes vers le serveur.
    fn cartes_window(&mut self, ctx: &egui::Context) {
        if !self.serveur_cartes {
            return;
        }
        let en_vocal = self.voice_channel.is_some();
        let membres = &self.members;
        let avatars = &self.avatars;
        let qui = |id: UserId| {
            let m = membres.iter().find(|m| m.user_id == id);
            cartes::Identite {
                nom: m.map(|m| ki_protocol::safe_display(&m.username, 32)).unwrap_or_else(|| "quelqu'un".into()),
                couleur: m.map(couleur_de_membre).unwrap_or(TEXT_DIM),
                photo: avatars.get(&id).map(|(_, t)| t.clone()),
            }
        };
        let actions = self.cartes.fenetre(ctx, self.my_id, en_vocal, &qui);
        for action in actions {
            self.send(ClientMsg::Cartes { action });
        }
    }

    /// La carte tactique : celle de mon salon vocal face à un serveur qui
    /// la partage, sinon la mienne seule. Le plan choisi et sa minimap
    /// viennent du catalogue, mes traits ont la couleur de mon pseudo, et
    /// mes gestes partent au serveur.
    fn tactique_window(&mut self, ctx: &egui::Context) {
        let lien = match (self.serveur_tactique, self.voice_channel) {
            (false, _) => tactique::Lien::SansServeur,
            (true, None) => tactique::Lien::HorsVocal,
            (true, Some(_)) => tactique::Lien::Salon,
        };
        self.tactique.relier(lien);
        let moi = self.my_id.unwrap_or(0);
        let mien = self.members.iter().find(|m| Some(m.user_id) == self.my_id);
        self.tactique.muet = mien.is_some_and(|m| m.force_muted);
        if self.tactique.ouvert {
            let couleur = mien.map_or(ACCENT, couleur_de_membre);
            let plans = self.catalogue.plans();
            let minimap = self.tactique.choisi(plans).and_then(|p| self.catalogue.minimap(p));
            self.tactique.fenetre(ctx, plans, minimap, moi, couleur);
        }
        for action in self.tactique.a_envoyer() {
            self.send(ClientMsg::Tactique { action });
        }
    }

    fn soundboard_window(&mut self, ctx: &egui::Context) {
        // Bouton caché (onglet Bêta) : la fenêtre se ferme avec lui.
        if !self.soundboard_visible {
            self.soundboard.ouvert = false;
            return;
        }
        let en_vocal = self.voice_channel.is_some();
        let Some(commande) = self.soundboard.fenetre(ctx, en_vocal) else { return };
        let joue = {
            let engine = self.link.engine.lock().unwrap();
            match engine.as_ref() {
                Some(engine) => {
                    match commande {
                        soundboard::Commande::Jouer(pcm) => {
                            engine.soundboard_push(&pcm, self.soundboard.volume);
                        }
                        soundboard::Commande::Arreter => engine.soundboard_clear(),
                    }
                    true
                }
                None => false,
            }
        };
        if !joue {
            self.info = Some("pas de moteur vocal : connecte-toi d'abord".into());
        }
    }

    /// Ouvre la visionneuse sur `cible`, avec les médias du salon courant
    /// pour passer de l'un à l'autre.
    fn ouvrir_visionneuse(&mut self, cible: visionneuse::Cible) {
        let mut liste = Vec::new();
        for msg in &self.messages {
            for (is_link, chunk) in split_links(&msg.text) {
                if !is_link || !self.previews.is_ours(chunk) {
                    continue;
                }
                if images::looks_like_image(chunk) {
                    liste.push(visionneuse::Cible::Image(chunk.to_string()));
                } else if medias::looks_like_video(chunk) {
                    liste.push(visionneuse::Cible::Video(chunk.to_string()));
                }
            }
        }
        if !liste.contains(&cible) {
            liste.push(cible.clone());
        }
        self.visionneuse.ouvrir(cible, liste);
    }

    /// La visionneuse, et ce qui la fait tourner : qui joue son son, le
    /// téléchargement de la vidéo, ses demandes à l'application.
    fn visionneuse_window(&mut self, ctx: &egui::Context) {
        // Le son sort par le moteur vocal quand il est là (même volume
        // général, même annulateur d'écho), par une sortie à part sinon.
        let en_video = self.visionneuse.a_une_video() || self.atelier.a_une_video();
        let moteur = self.link.engine.lock().unwrap().is_some();
        self.visionneuse.file.set_consommateur(if moteur {
            ki_voice::medias::Consommateur::Moteur
        } else {
            ki_voice::medias::Consommateur::Seule
        });
        if en_video && !moteur {
            if self.sortie_medias.is_none() {
                self.sortie_medias = Some(ki_voice::medias::SortieSeule::demarrer(
                    self.visionneuse.file.clone(),
                    self.audio.pref_output.clone(),
                    self.audio.native_audio,
                    self.audio.robust_output,
                ));
            }
        } else if self.sortie_medias.is_some() {
            self.sortie_medias = None;
        }
        if !self.visionneuse.est_ouverte() {
            return;
        }
        if let Some(url) = self.visionneuse.video_a_telecharger() {
            let cible = self.previews.pinned_url(&url).unwrap_or(url);
            self.visionneuse.lancer_telechargement(self.http_agent(), cible, ctx);
        }
        for demande in self.visionneuse.ui(ctx, &mut self.previews) {
            match demande {
                visionneuse::Demande::Navigateur(url) => {
                    let url = self.previews.pinned_url(&url).unwrap_or(url);
                    ctx.open_url(egui::OpenUrl::new_tab(url));
                }
                visionneuse::Demande::EnregistrerSous(cible) => self.enregistrer_media(cible),
                visionneuse::Demande::Copier(url) => self.copier_image(url),
            }
        }
    }

    /// « Enregistrer sous… » depuis la visionneuse : le dialogue natif puis
    /// la copie, sur un fil — depuis le cache si la vidéo y est déjà.
    fn enregistrer_media(&mut self, cible: visionneuse::Cible) {
        let url = cible.url().to_string();
        let nom = cible.nom();
        // Un fichier local (un clip) se copie ; un média du serveur vient du
        // cache s'il y est, du serveur sinon.
        let (cible_http, cache) = match &cible {
            visionneuse::Cible::Fichier(p) => (String::new(), Some(p.clone())),
            visionneuse::Cible::Video(_) => {
                let Some(h) = self.previews.pinned_url(&url) else { return };
                (h, medias::chemin_cache(&url).filter(|c| c.is_file()))
            }
            visionneuse::Cible::Image(_) => {
                let Some(h) = self.previews.pinned_url(&url) else { return };
                (h, None)
            }
        };
        let agent = self.http_agent();
        let avis = self.visionneuse.avis();
        std::thread::spawn(move || {
            let Some(dest) = rfd::FileDialog::new().set_file_name(&nom).save_file() else { return };
            let resultat = (|| -> Result<(), String> {
                if let Some(c) = cache {
                    std::fs::copy(&c, &dest).map_err(|e| e.to_string())?;
                    return Ok(());
                }
                let reponse = agent
                    .get(&cible_http)
                    .timeout(std::time::Duration::from_secs(600))
                    .call()
                    .map_err(|e| e.to_string())?;
                let mut fichier = std::fs::File::create(&dest).map_err(|e| e.to_string())?;
                std::io::copy(&mut reponse.into_reader(), &mut fichier).map_err(|e| e.to_string())?;
                Ok(())
            })();
            *avis.lock().unwrap() = Some(match resultat {
                Ok(()) => format!("enregistré : {}", dest.display()),
                Err(e) => format!("enregistrement impossible : {e}"),
            });
        });
    }

    /// « Copier l'image » depuis la visionneuse : retéléchargée et décodée
    /// sur un fil, puis posée dans le presse-papiers.
    fn copier_image(&mut self, url: String) {
        let Some(cible) = self.previews.pinned_url(&url) else { return };
        let agent = self.http_agent();
        let avis = self.visionneuse.avis();
        std::thread::spawn(move || {
            let resultat = (|| -> Result<(), String> {
                use std::io::Read as _;
                let reponse = agent
                    .get(&cible)
                    .timeout(std::time::Duration::from_secs(30))
                    .call()
                    .map_err(|e| e.to_string())?;
                let mut octets = Vec::new();
                reponse
                    .into_reader()
                    .take(12 * 1024 * 1024 + 1)
                    .read_to_end(&mut octets)
                    .map_err(|e| e.to_string())?;
                let image = images::decode(&octets).ok_or("image illisible")?;
                let [largeur, hauteur] = image.size;
                let octets: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
                let mut presse = arboard::Clipboard::new().map_err(|e| e.to_string())?;
                presse
                    .set_image(arboard::ImageData { width: largeur, height: hauteur, bytes: octets.into() })
                    .map_err(|e| e.to_string())?;
                Ok(())
            })();
            *avis.lock().unwrap() = Some(match resultat {
                Ok(()) => "image copiée".into(),
                Err(e) => format!("copie impossible : {e}"),
            });
        });
    }

    /// Ouvre la page de stats du groupe et demande les fiches au serveur —
    /// elles viennent de son cache, HenrikDev n'est pas sollicité.
    fn ouvrir_stats(&mut self) {
        self.valo.ouvert = true;
        self.stats_recu = false;
        self.send(ClientMsg::StatsValorant);
    }

    /// Les icônes de rang de la page du groupe — les fiches, et le rang de
    /// ceux qui jouent, fiche ou pas — et le catalogue.
    fn preparer_valo(&mut self, ctx: &egui::Context) {
        for f in &self.stats {
            self.rangs.preparer(ctx, f.fiche.rang.tier);
            if let Some(pic) = &f.fiche.pic {
                self.rangs.preparer(ctx, pic.tier);
            }
        }
        // « En direct » montre le rang de ceux qui jouent, fiche ou pas.
        for m in &self.members {
            if let Some(tier) = palier_de(m) {
                self.rangs.preparer(ctx, tier);
            }
        }
        self.catalogue.demarrer();
    }

    /// Ouvre la fiche VALORANT d'un membre et la demande au serveur. Une
    /// fiche déjà ouverte se remplace : ce qu'on y avait déplié ou déroulé
    /// s'oublie — un match joué ensemble porte le même id chez les deux,
    /// il s'ouvrirait déjà déplié chez le second.
    fn ouvrir_fiche(&mut self, user_id: UserId, username: String) {
        self.valo.fermer_fiche();
        self.valo.vers_profil();
        // On ne se compare pas à soi-même.
        if self.comparee.as_ref().is_some_and(|c| c.user_id == user_id) {
            self.comparee = None;
        }
        self.fiche = Some(FicheOuverte { user_id, username, recue: false, fiche: None });
        self.send(ClientMsg::FicheValorant { user_id });
    }

    fn send(&self, msg: ClientMsg) {
        if let Some(conn) = &self.conn {
            conn.send(msg);
        }
    }

    /// Ouvre un salon textuel : on change ce qu'on lit, rien d'autre.
    fn join(&mut self, channel: ChannelId) {
        let meme_salon = self.current == Some(channel);
        // Ce qui s'écrivait reste dans le salon qu'on quitte, et celui où
        // l'on entre rend ce qu'on y avait laissé. Le champ unique faisait
        // suivre le texte d'un salon à l'autre, et envoyait une modification
        // en cours vers un salon où son message n'existe pas : refusée, le
        // texte déjà effacé.
        if !meme_salon {
            // On n'était nulle part — connexion, reprise après coupure : ce
            // que porte le champ y reste (un message rendu par la coupure,
            // justement). Un brouillon rangé ne le remplace que s'il est vide.
            let champ_libre =
                self.input.trim().is_empty() && self.reponse_a.is_none() && self.edition.is_none();
            if let Some(quitte) = self.current {
                self.brouillons.ranger(
                    quitte,
                    brouillons::Brouillon {
                        texte: std::mem::take(&mut self.input),
                        reponse_a: self.reponse_a.take(),
                        edition: self.edition.take(),
                    },
                );
            }
            if self.current.is_some() || champ_libre {
                let repris = self.brouillons.reprendre(channel);
                self.curseur_a_la_fin = !repris.texte.is_empty();
                self.input = repris.texte;
                self.reponse_a = repris.reponse_a;
                self.edition = repris.edition;
            }
            // Le menu ouvert sur un message de l'autre salon n'a plus d'objet.
            self.menu_message = None;
        }
        self.current = Some(channel);
        self.messages.clear();
        // On repart du principe qu'il y a un passé à remonter : le serveur
        // dira le contraire dès la première page s'il n'y en a pas.
        self.history_more = true;
        self.history_pending = false;
        // Entrer normalement dans un salon, c'est en voir la fin : le pied de
        // fil « tu regardes un message retrouvé » n'a plus lieu d'être.
        self.recherche.retour_present = None;
        // Le fil s'ouvre sur la fin du salon, où qu'on en était dans celui
        // qu'on quitte.
        self.fil_au_bas = true;
        self.focus_input = true;
        // Entrer, c'est lire : la pastille tombe, et « nouveaux messages »
        // se posera là où l'on en était. Le serveur l'apprend dans une
        // seconde — s'il tient les lus.
        let repere = self
            .non_lus
            .remove(&channel)
            .filter(|n| n.nb > 0)
            .map(|n| n.depuis);
        // Rouvrir le salon où l'on est déjà — « revenir au présent » après
        // un saut de recherche — n'est pas en changer : le repère posé à
        // l'entrée reste, on veut le retrouver en bas du fil.
        self.separateur_nouveaux = if meme_salon { repere.or(self.separateur_nouveaux) } else { repere };
        self.programmer_lu(channel);
        self.send(ClientMsg::Join { channel });
        self.send(ClientMsg::History { limit: HISTORIQUE_PREMIERE_PAGE });
    }

    /// Prévoit de tenir le salon pour lu dans [`DELAI_LU`] : la pastille
    /// tombe, et le serveur l'apprend s'il tient les lus. Un seul `Lu` en
    /// attente : le dernier programmé l'emporte.
    fn programmer_lu(&mut self, channel: ChannelId) {
        self.lu_a_envoyer = Some((channel, std::time::Instant::now() + DELAI_LU));
        // Une image à l'échéance : rien ne garantit qu'une autre viendra
        // d'elle-même — fenêtre immobile, personne en vocal.
        self.app_ctx.request_repaint_after(DELAI_LU);
    }

    /// Tient le salon pour lu quand l'heure du `Lu` en attente est venue —
    /// et seulement si on le regarde vraiment : ce salon-là, fenêtre au
    /// premier plan, fil collé en bas, la même règle que celle qui décide
    /// qu'un message reçu est « vu ». Le salon ouvert d'office à la
    /// connexion, fenêtre réduite, reste donc non lu : c'est le retour du
    /// focus qui le lira ; et un fil remonté dans le passé attend qu'on
    /// redescende, sinon les messages arrivés hors écran passeraient pour
    /// lus.
    ///
    /// La pastille tombe dans tous les cas ; le `Lu` ne part que vers un
    /// serveur qui a dit tenir les lus — un autre répondrait « message
    /// invalide », en bannière. Sans ça, face à un serveur antérieur, la
    /// pastille du salon courant et le « (N) » du titre ne s'effaçaient
    /// jamais.
    fn tick_lus(&mut self, ctx: &egui::Context) {
        let Some((channel, quand)) = self.lu_a_envoyer else { return };
        let now = std::time::Instant::now();
        if now < quand {
            ctx.request_repaint_after(quand - now);
            return;
        }
        self.lu_a_envoyer = None;
        if !lecture_effective(self.window_focused, self.fil_en_bas, self.current, channel) {
            return;
        }
        self.non_lus.remove(&channel);
        if !self.serveur_gere_lus {
            return;
        }
        // Jusqu'au dernier message affiché ; fil vide, jusqu'au dernier que
        // le serveur connaît — il borne de toute façon.
        let ts = self.messages.iter().map(|m| m.ts).max().unwrap_or(u64::MAX);
        self.send(ClientMsg::Lu { channel, ts });
    }

    /// Un message non lu de plus dans `channel`.
    fn compter_non_lu(&mut self, channel: ChannelId, ts: u64, mention: bool) {
        self.non_lus.entry(channel).or_default().ajouter(ts, mention);
    }

    /// Son et clignotement pour un message reçu, selon le réglage : une
    /// mention a son propre son et insiste jusqu'au retour du focus.
    fn notifier(&mut self, pour_moi: bool) {
        if !self.notif.previent(pour_moi) {
            return;
        }
        self.play_sfx(if pour_moi { sfx::MENTION } else { sfx::MESSAGE });
        self.wants_attention = true;
        self.attention_critique |= pour_moi;
    }

    /// Ce message me nomme-t-il ? Même règle que le surlignage du fil.
    fn me_nomme(&self, user_id: UserId, text: &str) -> bool {
        Some(user_id) != self.my_id
            && !est_bot(user_id)
            && self.my_pseudo().is_some_and(|moi| {
                let membres: Vec<&str> = self.members.iter().map(|m| m.username.as_str()).collect();
                markup::me_mentionne(text, &membres, moi)
            })
    }

    /// Le total des non-lus, pour le titre de la fenêtre.
    fn total_non_lus(&self) -> u32 {
        self.non_lus.values().fold(0u32, |n, v| n.saturating_add(v.nb))
    }

    /// Ai-je cette permission ?
    fn can(&self, need: ki_protocol::Perms) -> bool {
        ki_protocol::perm::has(self.my_perms, need)
    }

    /// Puis-je agir sur cette personne ? Le rang tranche, jamais la
    /// permission : c'est ce qui empêche un second administrateur de bannir
    /// le propriétaire.
    fn outranks(&self, member_rank: u16) -> bool {
        ki_protocol::perm::outranks(self.my_rank, member_rank)
    }

    /// Ai-je de quoi ouvrir le panneau d'administration ?
    ///
    /// **Une seule** de ces permissions suffit — d'où le `any` et non un
    /// masque combiné : `perm::has` exige la totalité des bits qu'on lui
    /// passe, ce qui n'aurait ouvert le panneau qu'aux tout-puissants.
    fn any_admin_power(&self) -> bool {
        use ki_protocol::perm::*;
        [
            MANAGE_SERVER,
            MANAGE_CHANNELS,
            MANAGE_ROLES,
            KICK,
            BAN,
            RESET_PASSWORD,
            CREATE_INVITE,
            MANAGE_INVITES,
            VIEW_AUDIT_LOG,
            DELETE_MESSAGES,
        ]
        .iter()
        .any(|p| self.can(*p))
    }

    /// Ai-je de quoi agir sur un autre membre, si le rang le permet ?
    fn moderation_power(&self) -> bool {
        use ki_protocol::perm::*;
        [MUTE_MEMBERS, MOVE_MEMBERS, KICK, BAN, MANAGE_ROLES]
            .iter()
            .any(|p| self.can(*p))
    }

    /// Couleur d'un membre : celle de son rôle, sinon son pseudo.
    fn color_of(&self, member: &Member) -> egui::Color32 {
        couleur_de_membre(member)
    }

    /// Nom du rôle le mieux classé d'un membre, pour l'afficher en badge.
    fn top_role_name(&self, member: &Member) -> Option<&str> {
        member
            .roles
            .iter()
            .filter_map(|id| self.roles.iter().find(|r| r.id == *id))
            .max_by_key(|r| r.rank)
            .map(|r| r.name.as_str())
    }

    /// Joue un effet, s'il est présent et si les sons ne sont pas coupés.
    ///
    /// Silencieux quand le son n'existe pas : chacun dépose les fichiers
    /// qu'il veut, et il ne manque rien à celui qui n'en met aucun.
    fn play_sfx(&self, name: &str) {
        if !self.sfx_on || self.sfx_muted.contains(name) {
            return;
        }
        // Le ki-chat de test ne joue rien : le casque est celui de
        // l'utilisateur, qui n'a rien demandé.
        #[cfg(all(debug_assertions, windows))]
        if self.mcp.is_some() {
            return;
        }
        let Some(pcm) = self.sounds.get(name) else { return };
        if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
            engine.play_effect(pcm, self.sfx_volume);
        }
    }

    /// Compare les occupants de mon salon vocal à ceux de l'image précédente
    /// pour jouer les sons d'arrivée et de départ.
    ///
    /// C'est un écart qu'on mesure, et non `UserJoined`/`UserLeft` : ces
    /// deux-là portent sur le serveur entier, alors que seul mon propre
    /// salon vocal m'intéresse ici.
    /// Ce qui suit tout changement de la liste des membres, qu'il vienne
    /// d'un roster complet ou d'une seule fiche.
    ///
    /// **Le serveur fait foi sur notre propre présence en vocal**, sauf
    /// pendant la brève fenêtre qui suit un clic : la liste peut avoir été
    /// produite avant qu'il ne traite notre demande, et la suivre nous
    /// remettrait dans le salon qu'on vient de quitter. Dès qu'elle
    /// correspond à ce qu'on voulait, la fenêtre se referme ; si elle expire
    /// sans jamais correspondre — refus, message perdu — c'est le serveur qui
    /// reprend la main, ce qui évite tout affichage figé.
    fn after_roster_change(&mut self) {
        if let Some(me) = self.members.iter().find(|m| Some(m.user_id) == self.my_id) {
            // Un compte n'a qu'une voix : si elle passe par l'appli mobile,
            // ce PC n'est plus en vocal, même si le compte l'est.
            let mine = if me.mobile { None } else { me.voice };
            let waiting = self.voice_intent.is_some()
                && std::time::Instant::now() < self.voice_intent_until;
            match self.voice_intent {
                Some(want) if mine == want => {
                    self.voice_intent = None;
                    self.voice_channel = mine;
                }
                _ if waiting => {}
                _ => {
                    self.voice_intent = None;
                    self.voice_channel = mine;
                }
            }
        }
        self.update_voice_peers();
    }

    fn update_voice_peers(&mut self) {
        let Some(mine) = self.voice_channel else {
            self.prev_voice_peers.clear();
            self.prev_voice_channel = None;
            return;
        };
        let now: std::collections::HashSet<UserId> = self
            .members
            .iter()
            .filter(|m| m.voice == Some(mine) && Some(m.user_id) != self.my_id)
            .map(|m| m.user_id)
            .collect();
        // Je viens d'arriver dans CE salon : ses occupants sont adoptés en
        // silence — seuls les mouvements ULTÉRIEURS feront du bruit.
        if self.prev_voice_channel != Some(mine) {
            self.prev_voice_channel = Some(mine);
            self.prev_voice_peers = now;
            return;
        }
        if now.difference(&self.prev_voice_peers).next().is_some() {
            self.play_sfx(sfx::PEER_JOIN);
        }
        if self.prev_voice_peers.difference(&now).next().is_some() {
            self.play_sfx(sfx::PEER_LEAVE);
        }
        self.prev_voice_peers = now;
    }

    /// Demande la page d'historique précédant le plus ancien message affiché.
    fn load_older_history(&mut self) {
        if self.history_pending || !self.history_more {
            return;
        }
        let Some(oldest) = self.messages.first().map(|m| m.ts) else { return };
        let Some(channel) = self.current else { return };
        self.history_pending = true;
        self.send(ClientMsg::HistoryBefore { before_ts: oldest, limit: 100, channel });
    }

    /// Entre dans un salon vocal, ou en change.
    fn join_voice(&mut self, channel: ChannelId) {
        if self.voice_channel == Some(channel) {
            return;
        }
        // Le serveur refuserait : autant le dire tout de suite, plutôt que
        // d'allumer le salon et le micro pour rien.
        if !self.can(ki_protocol::perm::CONNECT_VOICE) {
            self.error = Some("tu n'as pas le droit de rejoindre le vocal".into());
            return;
        }
        // L'entrée est affichée sans attendre — le retour immédiat compte —
        // mais elle reste **en attente de confirmation** : le serveur peut
        // refuser, et la liste des membres qu'il diffuse fait foi. Sans cette
        // réconciliation, un refus laissait l'interface montrer le salon et
        // armer le micro dans le vide, indéfiniment.
        self.voice_channel = Some(channel);
        self.set_voice_intent(Some(channel));
        self.play_sfx(sfx::SELF_JOIN);
        // Le prochain passage d'update_voice_peers adoptera les occupants du
        // nouveau salon en silence (voir prev_voice_channel).
        self.prev_voice_channel = None;
        self.prev_voice_peers.clear();
        let password = self.mots_de_passe_vocaux.get(&channel).cloned();
        self.send(ClientMsg::JoinVoice { channel, password });
    }

    /// Annonce ce qu'on veut du vocal, et laisse à l'affichage le temps que le
    /// serveur le confirme. Passé ce délai, c'est lui qui décide de nouveau.
    fn set_voice_intent(&mut self, want: Option<ChannelId>) {
        self.voice_intent = Some(want);
        self.voice_intent_until =
            std::time::Instant::now() + std::time::Duration::from_secs(3);
    }

    /// Quitte le vocal, sans quitter le serveur.
    fn leave_voice(&mut self) {
        self.set_voice_intent(None);
        if self.voice_channel.take().is_none() {
            return;
        }
        self.vocal.armed = false;
        self.vocal.transmitting = false;
        self.play_sfx(sfx::SELF_LEAVE);
        self.prev_voice_peers.clear();
        self.send(ClientMsg::LeaveVoice);
    }

    /// Nom du salon vocal occupé.
    fn voice_channel_name(&self) -> Option<&str> {
        let id = self.voice_channel?;
        self.channels.iter().find(|c| c.id == id).map(|c| c.name.as_str())
    }

    /// Referme la connexion et **efface tout ce qui appartenait au serveur
    /// quitté**.
    ///
    /// L'exhaustivité n'est pas du zèle : les identifiants d'utilisateur sont
    /// attribués **par serveur**, si bien que la moindre table indexée par
    /// `user_id` qui survivait montrait, sur le serveur suivant, la photo et
    /// le nom de quelqu'un d'autre. Le reste était du même ordre : une fenêtre
    /// « Bannir untel » ressurgissait ailleurs, le panneau d'administration
    /// affichait les comptes d'un serveur auquel on n'était plus connecté, et
    /// l'empreinte retenue servait à vérifier une identité qui n'était plus la
    /// bonne.
    ///
    /// Toute donnée venue du réseau se remet donc à zéro ici. Ce qui reste est
    /// exactement ce qui appartient à la **machine** et non au serveur : les
    /// réglages audio, le carnet de serveurs, les volumes par personne.
    /// Le lien a lâché tout seul : on arme la reprise plutôt que de rendre
    /// l'écran de connexion.
    ///
    /// Distinct de [`Self::disconnect`], qui reste la sortie **définitive** —
    /// on s'est déconnecté soi-même, on a été expulsé, le mot de passe est
    /// faux, ou l'identité du serveur a changé. Confondre les deux ferait
    /// marteler un serveur qui vient de nous refuser, ou reprendre une
    /// connexion que l'on venait de fermer exprès.
    fn connexion_perdue(&mut self, error: Option<String>) {
        // Le partage d'écran ne survit pas à la connexion — le serveur a de
        // toute façon démonté le stream de son côté. Capture et visionnage
        // s'arrêtent dans `fermer_session`, par où passent toutes les
        // sorties, marqueur de plantage levé compris.
        //
        // On ne reprend qu'une connexion qui a **déjà fonctionné**. Un
        // premier essai qui échoue, c'est une adresse mal tapée ou un serveur
        // qu'on n'a jamais eu : réessayer en boucle n'y changerait rien et
        // masquerait l'erreur derrière un décompte.
        if !self.welcomed && self.reprise.is_none() {
            self.disconnect(error);
            return;
        }
        // À partir d'ici, on garde le son : `fermer_session` et non
        // `disconnect`.
        let essais = self.reprise.as_ref().map_or(0, |r| r.essais) + 1;
        // Relevés **avant** le nettoyage, qui les efface. Et repris de la
        // reprise en cours s'il y en a une : à la deuxième tentative, l'état
        // courant est déjà vide.
        let (salon, vocal, adresse) = match &self.reprise {
            Some(r) => (r.salon, r.vocal, r.adresse.clone()),
            None => (self.current, self.voice_channel, self.url.trim().to_string()),
        };
        self.fermer_session(error);
        if essais > Reprise::MAX {
            // On renonce : plus aucune raison de garder le micro ouvert, et
            // le tenir priverait un jeu du périphérique sans contrepartie.
            self.link.arreter();
            self.error = Some(
                "connexion perdue — le serveur n'a pas répondu, reconnecte-toi quand il \
                 sera revenu"
                    .into(),
            );
            return;
        }
        self.reprise = Some(Reprise {
            essais,
            quand: std::time::Instant::now() + Reprise::attente(essais, alea()),
            salon,
            vocal,
            adresse,
        });
    }

    /// Relance la connexion quand son tour est venu — vers le serveur perdu,
    /// et vers lui seul. Si le formulaire désigne désormais un autre serveur,
    /// la reprise n'a plus d'objet : elle tombe.
    fn tick_reprise(&mut self, ctx: &egui::Context) {
        let Some(r) = &self.reprise else { return };
        if r.adresse != self.url.trim() {
            self.reprise = None;
            return;
        }
        if self.connecting || std::time::Instant::now() < r.quand {
            return;
        }
        self.connect(ctx);
    }

    /// Temps restant avant la prochaine tentative, pour l'afficher.
    fn reprise_dans(&self) -> Option<std::time::Duration> {
        let r = self.reprise.as_ref()?;
        Some(r.quand.saturating_duration_since(std::time::Instant::now()))
    }

    /// Sortie **définitive** : on ferme la session **et** on rend les
    /// périphériques audio.
    ///
    /// C'est ici, et seulement ici, que le son s'arrête. Une coupure subie
    /// passe par [`Self::connexion_perdue`], qui ne ferme que la session et
    /// laisse le moteur debout, périphériques ouverts. Sans cette séparation,
    /// tout hoquet du réseau refermait le micro — et un jeu pouvait s'en
    /// emparer dans l'intervalle, en mode exclusif, pour ne plus le rendre.
    fn disconnect(&mut self, error: Option<String>) {
        self.fermer_session(error);
        self.link.arreter();
    }

    /// Défait la session sans toucher au son.
    fn fermer_session(&mut self, error: Option<String>) {
        // Toute sortie désarme la reprise : sans ça, cliquer « Se
        // déconnecter » pendant une coupure nous y ramènerait tout seul.
        // `connexion_perdue` la réarme après coup, elle seule.
        self.reprise = None;
        secours::serveur_courant(None);
        // La diffusion et le visionnage finissent avec la session, par
        // n'importe quelle sortie. Ils ne s'arrêtaient qu'à une coupure
        // subie : se déconnecter ou être expulsé laissait la capture et
        // l'encodeur tourner pour rien pendant les parties, la fenêtre de
        // visionnage figée — et le marqueur de plantage posé, si bien qu'au
        // lancement suivant ki-chat annonçait une fin brutale et passait
        // l'encodeur en logiciel. Avant `quit` : le serveur apprend encore
        // l'arrêt, s'il écoute.
        if self.diffusion.live.is_some() || self.diffusion.attente.is_some() {
            self.arreter_diffusion();
        }
        self.fermer_regard(false);
        if let Some(mut conn) = self.conn.take() {
            conn.quit();
        }
        self.connecting = false;
        self.connect_started = None;
        self.welcomed = false;

        // --- Identité et droits ---
        self.my_id = None;
        self.my_perms = 0;
        self.my_rank = 0;
        self.roles.clear();
        self.voice_token = 0;
        *self.jeton_http.lock().unwrap() = net::JetonHttp::default();
        self.server_fingerprint.clear();

        // --- Salons, présence, conversation ---
        self.channels.clear();
        // Le serveur suivant ne connaît pas notre jeu : il le réapprendra.
        self.jeu_envoye = None;
        self.current = None;
        self.voice_channel = None;
        self.voice_intent = None;
        self.members.clear();
        self.diffusion.spectateurs.clear();
        self.messages.clear();
        self.edition = None;
        self.history_more = false;
        self.history_pending = false;
        // Les non-lus sont ceux d'un serveur : le prochain redira les siens,
        // et redira s'il tient les lus.
        self.non_lus.clear();
        self.serveur_gere_lus = false;
        self.lu_a_envoyer = None;
        self.separateur_nouveaux = None;
        // Les pokes sont ceux d'un serveur, et le suivant ne connaît pas
        // notre réglage : il le réapprendra.
        self.poke_recu = None;
        self.pokes_envoye = None;
        self.pokes_refuses.clear();
        // Les portes aussi : le serveur suivant devra prouver qu'il les sert.
        self.portes.reinitialiser();

        // --- Vignettes : indexées par user_id, donc par serveur ---
        self.avatars.clear();
        self.incoming_avatars.clear();
        self.account_avatar = IconChange::Keep;
        self.preview_icons.clear();
        self.previews = images::Previews::default();

        // --- Identité du serveur et panneau d'administration ---
        self.server_info = ServerInfo::default();
        self.admin.nom.clear();
        self.admin.icone = IconChange::Keep;
        self.admin.comptes.clear();
        self.admin.invitations.clear();
        self.admin.derniere_invitation = None;
        self.admin.audit.clear();
        self.admin.roles_choisis.clear();

        // --- Fenêtres et brouillons en cours ---
        self.admin.ban = None;
        self.voice_prompt = None;
        self.mots_de_passe_vocaux.clear();
        self.admin.salon_edite = None;
        self.admin.verrou = None;
        // Les résultats se rapportent à un serveur et à ses salons : les
        // garder ferait cliquer sur des numéros de salon d'ailleurs.
        self.recherche.ouverte = false;
        self.recherche.requete.clear();
        self.recherche.resultats.clear();
        self.recherche.envoyee = None;
        self.recherche.encore = false;
        self.recherche.retour_present = None;
        *self.upload_status.lock().unwrap() = None;

        // Un message envoyé juste avant que la session tombe, et jamais
        // revenu du serveur : il est sans doute resté dans le tampon de la
        // connexion morte. Il revient dans le champ plutôt que de se perdre
        // en silence. (Au-delà de vingt secondes sans écho, c'est un refus du
        // serveur, déjà dit, pas une coupure.)
        if let Some(envoi) = self.envoi_en_attente.take() {
            if envoi.a_rendre_apres_une_coupure(std::time::Instant::now(), self.input.trim().is_empty()) {
                self.input = envoi.brut;
                ki_voice::journal("message non confirmé par le serveur : rendu au champ de saisie".into());
            }
        }

        // --- Ce qui s'était ajouté depuis, et que cette liste oubliait ---
        // La réponse en préparation citait un message de ce serveur.
        self.reponse_a = None;
        // La fiche VALORANT ouverte, les fiches et l'état du bot : ceux d'un
        // serveur. Le suivant renvoie les siens.
        self.fiche = None;
        self.stats.clear();
        self.stats_recu = false;
        self.musique.etat = ki_protocol::EtatMusique::default();
        // La table de cartes : celle d'un salon de ce serveur.
        self.cartes.effacer();
        self.serveur_cartes = false;
        // La carte tactique : celle d'un salon de ce serveur.
        self.tactique.recevoir_etat(None);
        self.serveur_tactique = false;
        // La soirée et ma fiche : celles de ce serveur.
        self.serveur_soiree = false;
        self.soiree.clear();
        self.soiree_demandee = None;
        self.ma_fiche = None;
        self.ma_fiche_demandee = false;
        self.comparee = None;
        // Mon journal relu : celui de ce serveur.
        self.serveur_journal_lecture = false;
        self.mon_journal = None;
        self.mon_journal_demande = None;
        // Nos comptes Riot : ceux de ce serveur.
        self.serveur_comptes_joues = false;
        self.mes_comptes.clear();
        self.serveur_journal = false;
        // Le partage d'un clip visait un salon de ce serveur : sur le
        // suivant, ce numéro désignerait un autre salon.
        self.clips.partage = None;
        // Les tableaux et diagnostics d'administration : ceux de ce serveur.
        self.admin.tableau = Default::default();
        self.admin.reseau = Default::default();
        *self.admin.diag.lock().unwrap() = None;
        self.admin.diag_versions.lock().unwrap().clear();

        self.vocal.armed = false;
        self.vocal.transmitting = false;
        self.vocal.loopback = false;
        self.show_settings = false;
        // Les deux fermetures complètes, pas seulement les drapeaux : les
        // mots de passe tapés pour ce serveur restaient pré-remplis sur le
        // suivant, et son tableau de bord s'y affichait.
        self.close_admin();
        self.close_account();
        self.error = error;
    }

    /// Faut-il une image de plus, et dans combien de temps ?
    ///
    /// `None` = rien ne bouge, l'application peut dormir jusqu'au prochain
    /// événement. C'est le cas par défaut, et c'est nouveau : le repeint était
    /// inconditionnel, à vingt images par seconde, même fenêtre réduite
    /// pendant une partie.
    ///
    /// Tout ce qui vient de l'extérieur réveille déjà la fenêtre de lui-même
    /// (réseau, images téléchargées, sondes de serveurs, touche
    /// push-to-talk). Ne restent donc ici que les choses qui **s'animent
    /// toutes seules** : un vumètre suit un niveau que personne ne nous
    /// signale, un décompte descend, une barre de progression avance.
    fn repaint_delay(&self, voice: &VoiceSnapshot) -> Option<std::time::Duration> {
        use std::time::Duration;
        /// Rythme des animations. Vingt par seconde suffisent à un vumètre :
        /// l'œil ne suit pas plus vite, et le moteur audio ne produit de
        /// toute façon qu'un niveau toutes les vingt millisecondes.
        const ANIME: Duration = Duration::from_millis(50);

        if !self.welcomed {
            // Écran de connexion.
            if self.connecting {
                // Le décompte du bouton d'annulation descend seconde après
                // seconde.
                return Some(Duration::from_millis(250));
            }
            // Une sonde en cours fait tourner son indicateur.
            if self
                .book
                .iter()
                .any(|s| matches!(self.probes.reach(s.id), servers::Reach::Probing))
            {
                return Some(ANIME);
            }
            // Et sinon, une image par seconde — pas pour l'affichage, pour la
            // **sonde périodique**.
            //
            // Elle est déclenchée depuis le rendu (`probes.sweep`), toutes les
            // vingt secondes. Sans image, pas de déclenchement ; sans
            // déclenchement, pas de résultat ; sans résultat, pas de réveil —
            // et l'état des serveurs se figeait pour de bon sur ce qu'il était
            // à l'ouverture. Une image par seconde, c'est un vingtième de ce
            // que coûtait l'écran de connexion avant, et ça suffit largement à
            // armer une horloge de vingt secondes.
            return Some(Duration::from_secs(1));
        }

        // Personne ne regarde — fenêtre réduite, ou derrière un jeu en plein
        // écran : c'est l'usage ordinaire, en vocal pendant une partie. Les
        // vumètres n'ont alors personne à qui s'animer, et vingt images par
        // seconde reconstruisaient toute l'interface pour rien. Quatre
        // suffisent à ce qui reste : sons d'arrivée et de départ, état
        // « parle » dit aux autres, cœur qui bat pour le garde-fou.
        let anime = if self.plein_rythme() { ANIME } else { zone::CADENCE_REDUITE };

        // En vocal : vumètres par locuteur, indicateur d'émission, ping.
        if self.voice_channel.is_some() {
            return Some(anime);
        }
        // Quelqu'un parle dans un salon qu'on regarde sans y être : son
        // vumètre s'anime quand même dans la liste des membres.
        if voice.levels.values().any(|l| *l > 0.0) {
            return Some(anime);
        }
        // Réglages ouverts : vumètre du micro en direct, statistiques réseau,
        // et la calibration qui mesure cinq secondes durant.
        if self.show_settings {
            return Some(anime);
        }
        // Un envoi de fichier en cours affiche sa progression.
        if self.upload_status.lock().unwrap().is_some() {
            return Some(anime);
        }
        // Une mise à jour en téléchargement, aussi.
        if matches!(self.updater.status(), update::Status::Downloading { .. }) {
            return Some(anime);
        }
        // Un périphérique audio perdu se rouvre en tâche de fond : la
        // bannière doit disparaître d'elle-même quand il revient.
        if voice.device_trouble.0 || voice.device_trouble.1 {
            return Some(Duration::from_millis(500));
        }
        // « X te poke » s'efface tout seul : il faut une image pour ça.
        if self.poke_recu.is_some() {
            return Some(Duration::from_secs(1));
        }
        // « Kevin veut rejoindre… expire dans 4 min » : même horloge.
        if self.portes.a_des_demandes() {
            return Some(Duration::from_secs(1));
        }
        None
    }

    /// Faut-il animer à plein rythme ? Oui dès que quelqu'un peut voir la
    /// fenêtre — et, cachée, tant que quelque chose d'autre que l'écran vit
    /// de ce rythme : l'overlay en jeu (dessiné depuis l'interface), un
    /// stream qu'on diffuse ou qu'on regarde, le Loupedeck et ses vumètres.
    fn plein_rythme(&self) -> bool {
        !self.zone.hors_de_vue()
            || self.overlay.actif
            || self.diffusion.live.is_some()
            || self.regard.flux.is_some()
            || (self.loupedeck_etat.actif
                && self.loupedeck.as_ref().is_some_and(|l| l.statut() == loupedeck::Statut::Pilote))
    }

    /// Au-delà, « Connexion… » cesse d'attendre.
    ///
    /// Rien ne bornait cette attente : `connecting` n'était levé que par
    /// `Welcome`, `ConnectFailed` ou `Disconnected`, et le keep-alive de cinq
    /// secondes tenait l'expiration d'inactivité de QUIC en échec. Un serveur
    /// qui accepte la connexion puis ne répond plus — surchargé, ou arrêté
    /// entre la poignée de main et l'authentification — laissait donc l'écran
    /// figé pour toujours.
    ///
    /// Vingt secondes : bien au-delà d'un Argon2id sur un VPS chargé, bien en
    /// deçà de la patience de qui que ce soit.
    const DELAI_CONNEXION: std::time::Duration = std::time::Duration::from_secs(20);

    /// Abandonne une connexion qui n'aboutit pas.
    fn check_connect_timeout(&mut self) {
        let Some(depuis) = self.connect_started else { return };
        if self.connecting && depuis.elapsed() >= Self::DELAI_CONNEXION {
            self.connexion_perdue(Some(
                "le serveur n'a pas répondu — vérifie l'adresse, ou réessaie".into(),
            ));
        }
    }

    fn connect(&mut self, ctx: &egui::Context) {
        self.error = None;
        self.connecting = true;
        self.connect_started = Some(std::time::Instant::now());
        let invite = self.invite.trim();
        self.conn = Some(net::connect(
            self.url.trim().to_string(),
            net::Credentials {
                username: self.username.trim().to_string(),
                password: self.password.clone(),
                invite: (!invite.is_empty()).then(|| invite.to_string()),
                // L'empreinte retenue pour ce serveur, s'il est au carnet :
                // c'est elle qui fera refuser un imposteur.
                fingerprint: self
                    .book
                    .iter()
                    .find(|s| s.address == self.url.trim())
                    .map(|s| s.cert_fingerprint.clone())
                    .unwrap_or_default(),
                appareil: ki_protocol::Appareil::Pc,
            },
            self.voice_prefs(),
            self.link.clone(),
            {
                let ctx = ctx.clone();
                std::sync::Arc::new(move || ctx.request_repaint())
            },
        ));
    }

    // -----------------------------------------------------------------
    // Carnet de serveurs
    // -----------------------------------------------------------------

    fn active_server(&self) -> Option<&servers::Server> {
        let id = self.selected?;
        self.book.iter().find(|s| s.id == id)
    }

    /// Nom du serveur courant, pour l'en-tête de la colonne de gauche.
    fn server_label(&self) -> String {
        match self.active_server() {
            Some(server) => server.label().to_string(),
            None => self.url.trim().to_string(),
        }
    }

    /// Ce qu'on raconte de son jeu aux membres : la partie VALORANT lue
    /// dans son client Riot si l'option est cochée, sinon le jeu reconnu à
    /// sa fenêtre si l'on veut bien le dire — et rien sinon. Chaque fil de
    /// lecture vit tant que son option est cochée ; le serveur n'apprend
    /// que les changements, y compris « plus rien » quand le jeu se ferme
    /// ou qu'on décoche.
    fn tick_presence(&mut self) {
        if self.valorant_presence {
            if self.veilleur_valorant.is_none() {
                self.veilleur_valorant = Some(valorant::Veilleur::demarrer(self.app_ctx.clone()));
            }
        } else {
            self.veilleur_valorant = None;
        }
        if self.presence_jeux {
            if self.veilleur_jeux.is_none() {
                self.veilleur_jeux = Some(jeux::Veilleur::demarrer(self.app_ctx.clone()));
            }
        } else {
            self.veilleur_jeux = None;
        }
        if !self.welcomed {
            return;
        }
        // VALORANT d'abord : sa présence dit plus qu'un nom de fenêtre.
        let mut valorant = self.veilleur_valorant.as_ref().and_then(|v| v.releve().1);
        // Et sur quel compte : classé ici parmi nos comptes liés — un compte
        // qu'on n'a pas lié ne part jamais, ni son nom ni son puuid.
        if self.serveur_comptes_joues {
            if let Some(j) = valorant.as_mut() {
                let session = self.veilleur_valorant.as_ref().and_then(|v| v.session());
                j.compte = valorant::classer_compte(session.as_ref(), &self.mes_comptes);
            }
        }
        self.tick_medailles(valorant.as_ref().map(|j| j.etat));
        let voulu = valorant.or_else(|| {
            self.veilleur_jeux
                .as_ref()
                .and_then(|v| v.releve())
                .map(ki_protocol::JeuStatut::autre_jeu)
        });
        if voulu != self.jeu_envoye {
            self.send(ClientMsg::GameStatus { jeu: voulu.clone() });
            self.jeu_envoye = voulu;
        }
        // Le réglage des pokes, sur le même modèle — mais seulement vers un
        // serveur qui a prouvé qu'il est récent (il tient les lus) : un
        // serveur antérieur répondrait « message invalide », en bannière,
        // à chaque connexion.
        if self.serveur_gere_lus && self.pokes_envoye != Some(self.pokes_accepter) {
            self.send(ClientMsg::AccepterPokes { accepter: self.pokes_accepter });
            self.pokes_envoye = Some(self.pokes_accepter);
        }
    }

    /// Le compte ouvert dans le client Riot quand il n'est pas lié — et
    /// qu'il faut le dire : un serveur qui comprend, une partie de
    /// VALORANT partagée, un compte qu'on n'a pas demandé à taire. Rien
    /// de tout ça ne part au serveur : c'est pour nos yeux seuls.
    fn compte_non_lie(&self) -> Option<valorant::SessionLocale> {
        if !self.serveur_comptes_joues || !self.jeu_envoye.as_ref().is_some_and(|j| j.est_valorant()) {
            return None;
        }
        let session = self.veilleur_valorant.as_ref()?.session()?;
        let autre = valorant::classer_compte(Some(&session), &self.mes_comptes) == ki_protocol::CompteLocal::Autre;
        (autre && !self.comptes_tus.contains(&empreinte_compte(&session.puuid))).then_some(session)
    }

    /// Mon Riot ID lié, d'après le roster.
    fn mon_riot_id(&self) -> Option<String> {
        let id = self.my_id?;
        self.members.iter().find(|m| m.user_id == id)?.riot_id.clone()
    }

    /// Les médailles VALORANT : une lecture prévue quand le jeu arrive et
    /// après chaque partie ; envoyée au serveur si tout le permet — l'option,
    /// un serveur qui les garde, un compte lié, et le client Riot ouvert sur
    /// ce compte-là. `etat` : où en est VALORANT à ce tour (`None` : fermé,
    /// ou la présence n'est pas partagée).
    fn tick_medailles(&mut self, etat: Option<ki_protocol::JeuEtat>) {
        use ki_protocol::JeuEtat;
        // Le choix d'abord, compte lié ou pas encore : il vaut aussi pour
        // la liaison à venir, et pour les médailles que le serveur lit
        // lui-même chez HenrikDev.
        if self.serveur_choix_medailles && self.choix_medailles_dit != Some(self.valorant_medailles) {
            self.send(ClientMsg::ChoixMedailles { oui: self.valorant_medailles });
            self.choix_medailles_dit = Some(self.valorant_medailles);
        }
        // Le journal de nos parties, sur le même modèle.
        if self.serveur_journal && self.choix_journal_dit != Some(self.journal_parties) {
            self.send(ClientMsg::ChoixJournal { oui: self.journal_parties });
            self.choix_journal_dit = Some(self.journal_parties);
            // Effacé ou repris : la Progression le relira.
            self.mon_journal = None;
            self.mon_journal_demande = None;
        }
        // À chaque image : rien n'est copié tant qu'aucune lecture n'arrive.
        let lie_existe = self
            .my_id
            .and_then(|id| self.members.iter().find(|m| m.user_id == id))
            .is_some_and(|m| m.riot_id.is_some());
        let voulu = self.valorant_medailles && self.serveur_medailles && lie_existe;
        if voulu {
            match (self.valorant_avant, etat) {
                (None, Some(_)) => self.medailles.au_lancement(),
                (Some(JeuEtat::EnJeu), apres) if apres != Some(JeuEtat::EnJeu) => self.medailles.apres_partie(),
                _ => {}
            }
        } else {
            self.medailles.oublier();
        }
        self.valorant_avant = etat;
        let Some(resultat) = self.medailles.tick(&self.app_ctx) else { return };
        let lie = self.mon_riot_id();
        // Le compte principal, au puuid quand le serveur nous l'a dit : un
        // Riot ID se renomme, un puuid jamais.
        let principal = self.mes_comptes.iter().find(|c| c.principal).map(|c| c.puuid.clone());
        let meme_compte = |lecture: &medailles::Lecture, lie: &str| match &principal {
            Some(p) => p.eq_ignore_ascii_case(&lecture.puuid),
            None => medailles::meme_riot_id(lie, &lecture.riot_id),
        };
        let ennui = match (resultat, lie) {
            (Err(e), _) => format!("médailles VALORANT : {e}"),
            (Ok(lecture), Some(lie)) if voulu && meme_compte(&lecture, &lie) => {
                let n = lecture.medailles.matchs.len();
                self.send(ClientMsg::Medailles { medailles: lecture.medailles });
                ki_voice::journal(format!("médailles VALORANT envoyées ({n} matchs médaillés)"));
                self.medailles_ennui.clear();
                return;
            }
            (Ok(_), Some(_)) if voulu => {
                "médailles VALORANT : le client Riot est ouvert sur un autre compte que celui lié — rien d'envoyé"
                    .to_string()
            }
            // L'option coupée pendant la lecture : rien à dire.
            (Ok(_), _) => return,
        };
        if ennui != self.medailles_ennui {
            ki_voice::journal(ennui.clone());
            self.medailles_ennui = ennui;
        }
    }

    /// Reprendre la session d'avant sans rien cliquer : au lancement, si
    /// l'on a quitté l'application connecté — et non déconnecté depuis
    /// elle —, on se reconnecte au même serveur, pourvu que son mot de
    /// passe soit mémorisé. Une seule tentative ; si elle échoue, le
    /// lanceur dit pourquoi, comme d'habitude.
    fn auto_connexion(&mut self, ctx: &egui::Context) {
        if self.auto_tente {
            return;
        }
        self.auto_tente = true;
        let Some(id) = self.session_auto else { return };
        if self.conn.is_some() || self.connecting {
            return;
        }
        let memorise = self.book.iter().any(|s| s.id == id && s.secret.is_some());
        if !memorise {
            self.session_auto = None;
            return;
        }
        self.select_server(id);
        self.connect(ctx);
    }

    /// Bascule sur un serveur : ses identifiants remplissent le formulaire.
    fn select_server(&mut self, id: u64) {
        // Pendant une connexion, la cible ne change pas : le `Welcome` qui
        // arrive écrirait nom, logo et empreinte du premier serveur dans la
        // fiche du second.
        if self.connecting {
            return;
        }
        let Some(server) = self.book.iter().find(|s| s.id == id) else { return };
        // Un autre serveur que celui qu'on essayait de retrouver : la reprise
        // ne le concerne pas.
        if self.reprise.as_ref().is_some_and(|r| r.adresse != server.address) {
            self.reprise = None;
        }
        self.selected = Some(id);
        self.url = server.address.clone();
        self.username = server.username.clone();
        self.password = server.password().unwrap_or_default();
        self.remember_password = server.secret.is_some();
        self.error = None;
    }

    /// Monte en textures les vignettes reçues depuis le dernier rendu.
    /// Une vignette illisible est simplement abandonnée : le monogramme
    /// prend le relais.
    fn mount_avatars(&mut self, ctx: &egui::Context) {
        for (user_id, (hash, png)) in std::mem::take(&mut self.incoming_avatars) {
            let Some(image) = servers::decode_png(&png) else { continue };
            let texture =
                ctx.load_texture(format!("avatar-{user_id}"), image, egui::TextureOptions::LINEAR);
            self.avatars.insert(user_id, (hash, texture));
        }
    }

    /// Photo de profil d'un membre, si elle est déjà arrivée.
    fn avatar_of(&self, user_id: UserId) -> Option<&egui::TextureHandle> {
        self.avatars.get(&user_id).map(|(_, texture)| texture)
    }

    /// Réclame les photos qui manquent, ou dont l'empreinte a changé. Rien
    /// n'est redemandé tant que l'image en cache est la bonne.
    fn fetch_missing_avatars(&mut self, members: &[Member]) {
        let mut to_download = Vec::new();
        for member in members {
            let Some(hash) = &member.avatar else {
                // Photo retirée côté serveur : on l'oublie ici aussi.
                self.avatars.remove(&member.user_id);
                continue;
            };
            let in_memory = self.avatars.get(&member.user_id).is_some_and(|(k, _)| k == hash);
            if in_memory {
                continue;
            }
            // Le cache disque est adressé par empreinte : si le fichier est
            // là, c'est forcément la bonne version.
            match photos::load(hash) {
                Some(png) => {
                    self.incoming_avatars.insert(member.user_id, (hash.clone(), png));
                }
                None => to_download.push(member.user_id),
            }
        }
        if !to_download.is_empty() {
            self.send(ClientMsg::RequestAvatars { user_ids: to_download });
        }
    }

    /// Texture du logo d'un serveur, montée à la première demande.
    fn server_icon(
        &mut self,
        ctx: &egui::Context,
        server: &servers::Server,
    ) -> Option<egui::TextureHandle> {
        if let Some(cached) = self.icon_textures.get(&server.id) {
            return cached.clone();
        }
        let texture = server.icon.as_deref().and_then(servers::decode_icon).map(|image| {
            ctx.load_texture(
                format!("server-icon-{}", server.id),
                image,
                egui::TextureOptions::LINEAR,
            )
        });
        self.icon_textures.insert(server.id, texture.clone());
        texture
    }

    /// Ouvre le sélecteur de fichier dans un thread : le dialogue natif ne
    /// doit pas figer le rendu.
    ///
    /// Le logo du serveur et la photo de profil ont chacun leur case : avec
    /// une case commune, la fenêtre qui la relevait en premier volait le
    /// fichier choisi par l'autre.
    fn pick_image(&self, ctx: &egui::Context, title: &str, slot: &PickedImage) {
        let slot = slot.clone();
        let title = title.to_string();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let picked = rfd::FileDialog::new()
                .set_title(title)
                .add_filter("Images", &["png", "jpg", "jpeg"])
                .pick_file();
            let Some(path) = picked else { return };
            let outcome = servers::encode_icon(&path).map_err(|e| format!("{e:#}"));
            *slot.lock().unwrap() = Some(outcome);
            ctx.request_repaint();
        });
    }

    fn delete_server(&mut self, id: u64) {
        self.book.retain(|s| s.id != id);
        self.probes.forget(id);
        self.icon_textures.remove(&id);
        if self.selected != Some(id) {
            return;
        }
        self.selected = None;
        match self.book.first().map(|s| s.id) {
            Some(next) => self.select_server(next),
            None => {
                self.url.clear();
                self.username.clear();
                self.password.clear();
            }
        }
    }

    /// Enregistre le formulaire d'édition dans le carnet et sélectionne le
    /// serveur concerné. Renvoie son identifiant.
    fn commit_draft(&mut self, draft: &ServerDraft, ctx: &egui::Context) -> u64 {
        let name = draft.name.trim().to_string();
        let address = draft.address.trim().to_string();
        let id = match draft.id {
            Some(id) => {
                if let Some(server) = self.book.iter_mut().find(|s| s.id == id) {
                    server.name = name;
                    server.address = address;
                }
                id
            }
            None => {
                let id = servers::next_id(&self.book);
                self.book.push(servers::Server {
                    id,
                    name,
                    address,
                    username: String::new(),
                    secret: None,
                    legacy_password: None,
                    last_used: 0,
                    server_name: String::new(),
                    cert_fingerprint: String::new(),
                    icon: None,
                });
                id
            }
        };
        self.select_server(id);
        if let Some(server) = self.book.iter().find(|s| s.id == id) {
            self.probes.probe(server, ctx);
        }
        id
    }

    /// Recopie dans le carnet l'identité annoncée par le serveur, pour que
    /// le lanceur puisse l'afficher même hors ligne. Le client ne fait que
    /// mémoriser : ces deux champs ne se règlent que côté serveur.
    fn cache_server_identity(&mut self) {
        let Some(id) = self.selected else { return };
        let (name, icon) = (self.server_info.name.clone(), self.server_info.icon.clone());
        let changed = match self.book.iter_mut().find(|s| s.id == id) {
            Some(server) => {
                let changed = server.icon != icon;
                server.server_name = name;
                server.icon = icon;
                changed
            }
            None => false,
        };
        if changed {
            self.icon_textures.remove(&id);
        }
    }

    /// Après une connexion réussie : mémorise pseudo, mot de passe (si
    /// demandé) et date, pour que le lanceur rouvre au bon endroit.
    fn remember_connection(&mut self) {
        let Some(id) = self.selected else { return };
        let username = self.username.trim().to_string();
        let password = self.remember_password.then(|| self.password.clone());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if let Some(server) = self.book.iter_mut().find(|s| s.id == id) {
            server.username = username;
            server.set_password(password.as_deref());
            server.last_used = now;
        }
    }

    // -----------------------------------------------------------------
    // Événements réseau
    // -----------------------------------------------------------------

    /// Lance la calibration : mesure le niveau ambiant (autres voix, bruit)
    /// pendant 5 s, micro « nu » (porte et AGC suspendus le temps de mesurer).
    fn start_calibration(&mut self) {
        {
            if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
                engine.set_gate_threshold(0.0);
                engine.set_agc(false);
            }
        }
        self.vocal.calibrating = Some((std::time::Instant::now(), 0.0));
    }

    /// Fait avancer la calibration ; à la fin, règle les seuils juste
    /// au-dessus du niveau ambiant mesuré et restaure la chaîne.
    fn tick_calibration(&mut self, niveau: f32) {
        const CALIB_SECS: f32 = 5.0;
        let Some((start, peak)) = &mut self.vocal.calibrating else { return };
        // Les réglages refermés puis rouverts des heures plus tard : la
        // mesure n'a rien vu entre-temps, elle ne vaut rien — on l'abandonne
        // au lieu de poser des seuils sur la trame du moment.
        if start.elapsed().as_secs_f32() > CALIB_SECS + 1.0 {
            self.vocal.calibrating = None;
            self.apply_audio_settings();
            return;
        }
        *peak = peak.max(niveau);
        if start.elapsed().as_secs_f32() < CALIB_SECS {
            return;
        }
        let ambient = *peak;
        let avant = self.audio.gate_threshold;
        self.vocal.calibrating = None;
        self.audio.gate_threshold = (ambient * 1.4).clamp(0.004, 0.10);
        if self.audio.mode == MicMode::Vad {
            self.audio.vad_threshold = (ambient * 1.8).clamp(0.01, 0.25);
        }
        self.apply_audio_settings();
        // En détection neuronale, le seuil d'amplitude ne décide de rien :
        // on ne l'annonce pas.
        let seuil = if self.audio.mode == MicMode::Vad && !self.audio.vad_neural {
            format!(", seuil d'activation {:.1} %", self.audio.vad_threshold * 100.0)
        } else {
            String::new()
        };
        let baisse = if avant > 0.0 && self.audio.gate_threshold < avant * 0.5 {
            format!(" (elle était à {:.1} %)", avant * 100.0)
        } else {
            String::new()
        };
        self.info = Some(format!(
            "calibré : ambiance {:.1} % → porte de bruit {:.1} %{baisse}{seuil}",
            ambient * 100.0,
            self.audio.gate_threshold * 100.0,
        ));
    }

    // -----------------------------------------------------------------
    // Vocal : décision d'émission (mode ouvert / PTT global)
    // -----------------------------------------------------------------

    fn update_voice(&mut self) {
        let engine_guard = self.link.engine.lock().unwrap();
        let Some(engine) = engine_guard.as_ref() else { return };

        // « Armé » : le micro a le droit d'émettre. En activation vocale,
        // c'est ensuite le moteur qui décide selon le seuil.
        //
        // En push-to-talk, la touche (lue par son fil à cent hertz, maintien
        // compris) et le bouton du Loupedeck sont lus par le moteur lui-même,
        // à chaque trame (`brancher_ptt`) : la prise de parole n'attend pas
        // l'interface, qu'eframe espace quand la fenêtre est réduite. D'ici,
        // on ne lui dit que le reste — en vocal, pas coupé, et si le mode
        // exige la touche ; `ptt_active` ne sert plus qu'à l'affichage.
        self.touche_ptt.regler(self.audio.mode == MicMode::Ptt, self.audio.ptt_release_ms);
        if !engine.ptt_branche() {
            engine.brancher_ptt(Some(self.touche_ptt.source()));
        }
        let ptt_active = self.audio.mode == MicMode::Ptt && self.touche_ptt.tenue();
        // Hors d'un salon vocal, le micro reste fermé quoi qu'il arrive.
        let autorise = !self.vocal.muted && self.voice_channel.is_some();
        let armed = autorise && (matches!(self.audio.mode, MicMode::Open | MicMode::Vad) || ptt_active);
        // Le moteur est réglé **à chaque image**, et non sur la seule
        // transition. `self.vocal.armed` était écrit hors d'ici — en quittant le
        // vocal, sur une erreur, à la disparition d'un salon — si bien que
        // l'état local et celui du moteur pouvaient diverger sans jamais se
        // retrouver : le micro restait ouvert alors que l'interface le
        // montrait fermé, et l'on continuait d'être entendu. L'écriture est
        // un simple stockage atomique, la refaire à chaque image ne coûte rien.
        self.vocal.armed = armed;
        engine.set_transmit(autorise);

        // Émission réelle (après VAD) : indicateur TX + diffusion aux autres.
        let sending = engine.is_sending();
        drop(engine_guard);
        if sending != self.vocal.transmitting {
            self.vocal.transmitting = sending;
            self.send(ClientMsg::VoiceState { speaking: sending, muted: self.vocal.muted });
        }
    }

    // -----------------------------------------------------------------
    // Écran de connexion
    // -----------------------------------------------------------------

    // -----------------------------------------------------------------
    // Écran principal
    // -----------------------------------------------------------------

    fn main_screen(&mut self, ui: &mut egui::Ui, voice: &VoiceSnapshot) {
        let ctx = &ui.ctx().clone();
        self.mount_avatars(ctx);
        self.previews.set_origin(self.http_base());
        self.previews.set_agent(self.http_agent());
        self.previews.mount(ctx);

        self.voice_bar(ui, voice);
        self.sidebar(ui, voice);
        self.roster_panel(ui, voice);
        if self.valo.ouvert || self.fiche.is_some() {
            self.valo_panel(ui);
        } else {
            self.chat_panel(ui);
        }
        self.comms_popup(ctx, voice);
        self.partage_windows(ctx);
        self.diffusion_window(ctx);
        self.fiche_bot_window(ctx);
        self.clips_window(ctx);
        self.soundboard_window(ctx);
        self.cartes_window(ctx);
        self.tactique_window(ctx);
        self.portes_window(ctx);
        self.loupedeck_window(ctx, voice);
        self.partage_clip_window(ctx);
        self.visionneuse_window(ctx);
        self.atelier_window(ctx);
        self.overlay_en_jeu(ctx, voice);

        // Le spectre de l'égaliseur : la page le demande à chaque image où
        // elle se montre ; le moteur n'en dépose que tant qu'on le regarde.
        self.vocal.analyse_voulue = ki_voice::ANALYSE_AUCUNE;
        if self.show_settings {
            self.settings_window(ctx, voice);
        }
        if self.vocal.analyse_voulue != self.vocal.analyse_actuelle {
            if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
                engine.set_analyse(self.vocal.analyse_voulue);
            }
            self.vocal.analyse_actuelle = self.vocal.analyse_voulue;
        }
        self.menu_message_ui(ctx);
        if self.admin.ouverte {
            self.admin_window(ctx);
        }
        if self.recherche.ouverte {
            self.search_window(ctx);
        }
        if self.show_account {
            self.account_window(ctx);
        }
        if self.admin.ban.is_some() {
            self.ban_window(ctx);
        }
        if self.voice_prompt.is_some() {
            self.voice_password_window(ctx);
        }
        if self.labo.is_some() {
            self.labo_window(ctx);
        }
    }

    /// Suis-je le propriétaire du serveur ? Son rôle est seul au rang le
    /// plus haut.
    fn suis_proprietaire(&self) -> bool {
        self.my_rank == u16::MAX
    }

    /// Allumer ou couper le changeur de voix — par le raccourci global ou le
    /// Loupedeck —, et dire lequel.
    fn basculer_changeur(&mut self) {
        self.audio.changeur_actif = !self.audio.changeur_actif;
        self.apply_audio_settings();
        self.info = Some(if self.audio.changeur_actif {
            let nom = changeur_ui::personnage_de(&self.audio.changeur)
                .map(|i| ki_voice::changeur::PERSONNAGES[i].0)
                .unwrap_or("réglage perso");
            format!("changeur de voix allumé : {nom}")
        } else {
            "changeur de voix coupé".into()
        });
    }

    /// Couper ou rétablir son micro — par le bouton ou le raccourci global.
    fn basculer_micro(&mut self) {
        self.vocal.muted = !self.vocal.muted;
        self.play_sfx(if self.vocal.muted { sfx::MUTE } else { sfx::UNMUTE });
        // Annoncer la bascule tout de suite : les autres voient l'icône
        // « muet » au lieu de se demander si l'on est parti. Sans ça, seule
        // la prochaine transition de parole aurait porté l'information.
        self.send(ClientMsg::VoiceState { speaking: self.vocal.transmitting, muted: self.vocal.muted });
    }

    /// Se rendre sourd, ou retrouver l'écoute. Sourd coupe aussi le micro,
    /// comme partout ailleurs ; le retour le rend, sans se souvenir d'un
    /// micro coupé avant — on redevient présent d'un coup.
    fn basculer_sourd(&mut self) {
        self.vocal.sourd = !self.vocal.sourd;
        if self.vocal.muted != self.vocal.sourd {
            self.vocal.muted = self.vocal.sourd;
            self.send(ClientMsg::VoiceState { speaking: self.vocal.transmitting, muted: self.vocal.muted });
        }
        // L'effet sonore part avant que la sortie ne se taise ; au retour,
        // après qu'elle est revenue.
        if self.vocal.sourd {
            self.play_sfx(sfx::MUTE);
            self.apply_audio_settings();
        } else {
            self.apply_audio_settings();
            self.play_sfx(sfx::UNMUTE);
        }
    }

    // -----------------------------------------------------------------
    // Fenêtres
    // -----------------------------------------------------------------

    /// Fenêtre d'administration : invitations, comptes, blocages, mots de passe.
    /// Texture d'aperçu d'une vignette, reconstruite seulement si elle change.
    ///
    /// `emplacement` distingue les deux aperçus possibles. Sans lui, ouvrir
    /// « Mon compte » et Admin ▸ Serveur ensemble faisait alterner la clé à
    /// chaque image : un décodage PNG et un téléversement GPU par panneau et
    /// par image, pour deux vignettes qui ne changeaient pas.
    fn preview_texture(
        &mut self,
        ctx: &egui::Context,
        emplacement: Apercu,
        data: Option<&str>,
    ) -> Option<egui::TextureHandle> {
        let Some(data) = data else {
            self.preview_icons.remove(&emplacement);
            return None;
        };
        let a_jour = self
            .preview_icons
            .get(&emplacement)
            .is_some_and(|(key, _)| key == data);
        if !a_jour {
            match servers::decode_icon(data) {
                Some(image) => {
                    let texture = ctx.load_texture(
                        format!("icon-preview-{emplacement:?}"),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.preview_icons.insert(emplacement, (data.to_string(), texture));
                }
                None => {
                    self.preview_icons.remove(&emplacement);
                }
            }
        }
        self.preview_icons.get(&emplacement).map(|(_, texture)| texture.clone())
    }

    // -----------------------------------------------------------------
    // Partage d'écran (S1b)
    // -----------------------------------------------------------------

    /// L'overlay « qui parle » : les occupants de mon salon vocal, avec
    /// l'anneau de ceux qui parlent — la même règle que la liste des
    /// membres (mon état local pour moi, le drapeau serveur ou le niveau
    /// reçu pour les autres).
    fn overlay_en_jeu(&mut self, ctx: &egui::Context, voice: &VoiceSnapshot) {
        self.overlay.veiller(ctx);
        let Some(channel) = self.voice_channel else {
            self.overlay.montrer(ctx, Vec::new(), self.window_focused);
            return;
        };
        let lignes: Vec<overlay::Ligne> = self
            .members
            .iter()
            .filter(|m| m.voice == Some(channel))
            .map(|m| {
                let is_me = Some(m.user_id) == self.my_id;
                let level = if is_me {
                    voice.stats.mic_peak
                } else {
                    voice.levels.get(&m.user_id).copied().unwrap_or(0.0)
                };
                let parle =
                    if is_me { self.vocal.transmitting } else { m.speaking || level > SPEAK_LEVEL };
                overlay::Ligne {
                    nom: m.username.clone(),
                    parle,
                    photo: self.avatar_of(m.user_id).cloned(),
                }
            })
            .collect();
        self.overlay.montrer(ctx, lignes, self.window_focused);
    }

    /// La demande de bascule du micro en catégorie « communications ».
    ///
    /// Le moteur a détecté un micro affamé (il s'ouvre, rien n'en sort) et
    /// propose la seule parade logicielle : partager la voie de traitement
    /// avec la voix du jeu. Mais cette bascule peut faire baisser le volume
    /// des autres sons (l'atténuation Windows) — alors on demande, on
    /// n'impose pas. Refuser vaut pour la session : à l'utilisateur de
    /// régler son casque, le docteur audio lui dit comment.
    ///
    /// La fenêtre nomme le micro et parle d'abord de l'appareil éteint — la
    /// cause que le terrain a montrée — et se ferme d'elle-même quand le son
    /// revient : le moteur retire alors sa proposition.
    fn comms_popup(&mut self, ctx: &egui::Context, voice: &VoiceSnapshot) {
        if !voice.comms_proposal {
            return;
        }
        let mut reponse: Option<bool> = None;
        egui::Window::new("Ton micro ne livre rien")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -40.0])
            .show(ctx, |ui| {
                ui.set_max_width(380.0);
                // Nommer le micro change tout : « ton micro ne capte rien » ne
                // dit pas que c'est une manette que Windows a prise par défaut.
                let (qui, par_defaut) = match &voice.micro_muet {
                    Some((nom, par_defaut)) => (format!("« {nom} »"), *par_defaut),
                    None => ("Ton micro".to_string(), false),
                };
                ui.label(format!(
                    "{qui} s'ouvre mais ne capte rien{}.",
                    if par_defaut {
                        " — c'est le micro que Windows désigne par défaut"
                    } else {
                        ""
                    }
                ));
                ui.add_space(espace::S);
                ui.label(
                    "Le plus souvent, l'appareil est éteint ou en veille alors que son \
                     récepteur reste branché — casque sans fil, manette. Rallume-le : \
                     cette fenêtre se fermera d'elle-même quand le son reviendra. Si ce \
                     n'est pas ton micro, choisis le bon dans ⚙ Audio.",
                );
                ui.add_space(espace::S);
                ui.label(
                    "Sinon, un autre logiciel — la voix intégrée d'un jeu — tient \
                     peut-être la voie de capture. ki-chat peut demander la même voie \
                     que lui (catégorie « communications » de Windows). Revers possible : \
                     Windows peut alors baisser le volume de tes autres sons — réglable \
                     dans Panneau son → Communications → « Ne rien faire ».",
                );
                ui.add_space(espace::L);
                ui.horizontal(|ui| {
                    if ui::Bouton::new("Basculer (cette session)").icone(Icon::Check).show(ui).clicked() {
                        reponse = Some(true);
                    }
                    if ui::Bouton::new("Non — je vérifie mon micro").icone(Icon::Close).show(ui).clicked() {
                        reponse = Some(false);
                    }
                });
                ui.add_space(espace::XS);
                ui::hint(
                    ui,
                    "pour le rendre permanent : ⚙ Audio → « Partager le micro avec \
                     la voix du jeu ». Pour comprendre : le docteur audio, au même \
                     endroit.",
                );
            });
        if let Some(accept) = reponse {
            if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
                engine.resolve_comms(accept);
            }
            self.info = Some(
                if accept {
                    "micro basculé en catégorie communications pour cette session"
                } else {
                    "compris — le micro reste en catégorie standard"
                }
                .into(),
            );
        }
    }

}

/// Noms des sons reconnus. Chaque nom correspond au nom d'un fichier `.wav`
/// (sans l'extension) déposé dans le dossier des sons.
mod sfx {
    pub const SELF_JOIN: &str = "rejoint-vocal";
    pub const SELF_LEAVE: &str = "quitte-vocal";
    pub const PEER_JOIN: &str = "arrivee";
    pub const PEER_LEAVE: &str = "depart";
    pub const MESSAGE: &str = "message";
    /// On me nomme : distinct du message ordinaire, pour tendre l'oreille
    /// sans regarder.
    pub const MENTION: &str = "mention";
    /// Quelqu'un me poke : le seul son qui vise quelqu'un en particulier.
    pub const POKE: &str = "poke";
    /// Quelqu'un frappe à une porte web : il attend qu'on lui ouvre.
    pub const PORTE: &str = "porte";
    /// Quelqu'un de mon salon vocal lance un stream : on peut aller voir.
    pub const DIFFUSION: &str = "diffusion";
    /// À moi de jouer aux cartes.
    pub const CARTES_TOUR: &str = "cartes-tour";
    /// Une manche de cartes est gagnée.
    pub const CARTES_FIN: &str = "cartes-fin";
    /// Quelqu'un dit « Dernière carte ! ».
    pub const CARTES_DERNIERE: &str = "cartes-derniere";
    /// Quelqu'un se fait contrer.
    pub const CARTES_CONTRE: &str = "cartes-contre";
    /// Des mains changent de place : un 7 qui échange, un 0 qui tourne.
    pub const CARTES_ECHANGE: &str = "cartes-echange";
    pub const MUTE: &str = "micro-coupe";
    pub const UNMUTE: &str = "micro-actif";
}

/// Où l'on cherche les sons, dans l'ordre : un dossier `sons/` à côté de
/// l'exécutable, puis celui des réglages.
///
/// Rien n'est embarqué dans le binaire : les fichiers audio du dépôt sont
/// des œuvres tierces, et le dépôt est public. Chacun dépose donc les siens.
fn sound_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.join("sons"));
        }
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        dirs.push(std::path::PathBuf::from(appdata).join("ki-chat").join("sons"));
    }
    // macOS : là où eframe range déjà les réglages.
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(
            std::path::PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("ki-chat")
                .join("sons"),
        );
    }
    dirs
}

fn load_sounds() -> HashMap<String, Vec<f32>> {
    let mut found = HashMap::new();
    for dir in sound_dirs() {
        for (name, pcm) in ki_voice::effects::load_dir(&dir) {
            // Le premier dossier gagne : celui à côté de l'exécutable
            // l'emporte sur celui des réglages.
            found.entry(name).or_insert(pcm);
        }
    }
    if !found.is_empty() {
        tracing::info!("{} effet(s) sonore(s) chargé(s)", found.len());
    }
    found
}

/// Les messages demandés à l'entrée dans un salon. Une page moins pleine
/// laisse penser qu'il n'y a rien avant : on le vérifie aussitôt.
const HISTORIQUE_PREMIERE_PAGE: u32 = 100;

/// Prend Échap : vrai si la touche vient d'être pressée, et personne d'autre
/// ne la lira dans cette image. Chaque lecteur la prenait pour lui sans la
/// retirer, si bien qu'une seule pression fermait une fenêtre **et** annulait
/// ce qui s'écrivait derrière.
fn prendre_echap(ctx: &egui::Context) -> bool {
    ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
}

// ---------------------------------------------------------------------
// Widgets propres à l'écran principal
// ---------------------------------------------------------------------

/// Les deux vignettes dont on peut afficher un aperçu, et qui peuvent l'être
/// **en même temps** : la photo de profil dans « Mon compte », le logo du
/// serveur dans Admin ▸ Serveur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Apercu {
    Avatar,
    LogoServeur,
}

impl<'a> MemberRow<'a> {
    /// Quelqu'un qui n'est pas là : rien à dire de son micro ni de son
    /// niveau, seul le compte et sa photo subsistent.
    fn offline(member: &'a Member, photo: Option<&'a egui::TextureHandle>) -> Self {
        Self {
            member,
            speaking: false,
            muted: false,
            is_me: false,
            photo,
            rang_icone: None,
            glissable: false,
            spectateurs: 0,
        }
    }
}

/// La fiche d'un membre, ouverte au clic droit : on l'a demandée au
/// serveur, et on attend — puis on l'a, ou on sait qu'il n'y en a pas.
pub(crate) struct FicheOuverte {
    pub(crate) user_id: UserId,
    pub(crate) username: String,
    pub(crate) recue: bool,
    pub(crate) fiche: Option<ki_protocol::FicheValorant>,
}

// ---------------------------------------------------------------------
// Utilitaires
// ---------------------------------------------------------------------

/// Pseudo affichable : sans caractères dangereux et de longueur bornée.
fn safe_name(username: &str) -> String {
    ki_protocol::safe_display(username, ki_protocol::MAX_USERNAME)
}

/// Identité de serveur affichable. Le nom est borné côté serveur, mais rien
/// n'oblige le serveur d'en face à être le nôtre.
fn safe_server_info(mut server: ServerInfo) -> ServerInfo {
    server.name = ki_protocol::safe_display(&server.name, ki_protocol::MAX_SERVER_NAME);
    // Un lien qu'on montre, qu'on copie et qu'on met en QR code : ce qui ne
    // se lit pas comme une adresse web est tenu pour absent.
    server.adresse_web = ki_protocol::normaliser_adresse_web(&server.adresse_web).unwrap_or_default();
    server
}

/// Le HWND de la fenêtre principale (0 hors Windows ou si inconnu), tel
/// que `CreationContext` et `eframe::Frame` le donnent — le même, la sonde
/// l'a vérifié.
fn hwnd_de(h: &impl raw_window_handle::HasWindowHandle) -> isize {
    match h.window_handle().map(|w| w.as_raw()) {
        Ok(raw_window_handle::RawWindowHandle::Win32(w)) => w.hwnd.get(),
        _ => 0,
    }
}

impl eframe::App for KiApp {
    /// Le fond de toute fenêtre est la couleur-clé de l'overlay : Windows
    /// la rend transparente sur la fenêtre de l'overlay (c'est ainsi que le
    /// jeu se voit entre les ronds), et les panneaux couvrent la fenêtre
    /// principale de bord à bord — on ne l'y voit jamais.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        overlay::CLE
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.logique(ctx, frame);
    }

    /// Les clics et les touches que Claude envoie entrent ici, avec ceux de
    /// la vraie souris : egui ne fait pas la différence.
    #[cfg(all(debug_assertions, windows))]
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(serveur) = self.mcp.as_mut() {
            serveur.injecter(ctx, raw_input);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.interface(ui);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.sauver(storage);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.quitter_proprement();
    }
}

impl KiApp {
    /// Tout ce qui doit tourner même quand rien ne se dessine — fenêtre
    /// réduite dans la zone de notification, en pleine partie : le réseau,
    /// la voix, la zone de notification, le Loupedeck, les clips. Rien n'y
    /// dessine. Fenêtre cachée, eframe n'appelle que cette partie
    /// (`App::logic`), au plus dix fois par seconde.
    fn logique(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // La mesure encadre la logique ET le dessin, sinon elle mentirait
        // par omission — c'est le coût complet d'une image qu'on cherche, pas
        // celui de la partie qu'on a pensé à instrumenter.
        self.perf.debut_image();

        // Géométrie : on ne restaure que « maximisée », et on suit l'état
        // courant pour le réenregistrer. Cf. `main` pour le pourquoi.
        if self.restore_maximized {
            self.restore_maximized = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            // La commande n'est appliquée qu'en fin d'image : ce que winit
            // rapporte maintenant, c'est la fenêtre d'avant. Lire ici,
            // c'était perdre « maximisée » si l'on se réduit dès l'image
            // suivante (`--reduit`) et qu'on quitte sans avoir rouvert.
        } else if !self.zone.reduite() {
            // Réduite dans la zone, la fenêtre est minimisée : ce que winit
            // rapporte alors n'est pas l'état qu'on voudra retrouver.
            let was_maximized = self.maximized;
            self.maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(was_maximized));
        }

        // La zone de notification. D'abord le cœur qui bat (le garde-fou
        // l'écoute), puis ce que l'icône a reçu, puis la croix.
        self.zone.attacher(hwnd_de(frame));
        match self.zone.tick(ctx) {
            zone::Constat::Rien => {}
            zone::Constat::ReouvertureForcee => {
                self.zone_poke = None;
                self.info = Some("ki-chat ne répondait plus réduit : fenêtre rouverte".into());
            }
            // Revenue par Alt+Tab ou la barre des tâches : elle est sous
            // les yeux, l'icône n'a plus rien à annoncer.
            zone::Constat::RouverteDehors => self.zone_poke = None,
            zone::Constat::ReductionAbandonnee => {
                self.info = Some("la fenêtre n'a pas pu être réduite dans la zone de notification".into());
            }
            // Un second ki-chat lancé depuis l'icône du Bureau nous a
            // sonnés : à nous de revenir — rouverte si réduite, devant
            // sinon — et de le lui dire, pour qu'il se retire. Sans cet
            // accusé, il conclurait qu'on ne répond plus.
            zone::Constat::Revelee => {
                self.rouvrir_depuis_zone(ctx);
                instance::accuser_reveil();
            }
        }
        for ev in self.zone.evenements() {
            match ev {
                zone::Evenement::Ouvrir => self.rouvrir_depuis_zone(ctx),
                zone::Evenement::Quitter => {
                    self.quitter = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        // Lancé `--reduit` : à la zone, mais pas avant que la fenêtre
        // existe pour le shell. eframe la crée cachée et ne la montre
        // qu'après la première image : réduire avant, c'est retirer un
        // onglet qui n'existe pas encore — il apparaîtrait avec la
        // fenêtre, et un clic dessus restaurerait un ki-chat invisible qui
        // prend le clavier en plein jeu. On réessaie à l'image suivante,
        // le drapeau intact.
        if self.reduire_au_demarrage {
            if !self.zone.disponible() {
                self.reduire_au_demarrage = false;
            } else if self.zone.fenetre_visible(ctx) {
                self.reduire_au_demarrage = false;
                self.zone.reduire(ctx);
            } else {
                ctx.request_repaint();
            }
        }
        // La croix. Avec le réglage, elle réduit — sauf si l'on a demandé à
        // quitter (menu de l'icône, mise à jour) : la fermeture normale
        // passe alors, avec `save` et `on_exit` (déconnexion propre,
        // marqueurs levés). `CancelClose` doit partir dans la même image.
        // Et sans icône pour revenir, elle ferme comme avant.
        if ctx.input(|i| i.viewport().close_requested())
            && self.reduire_zone
            && self.zone.disponible()
            && !self.quitter
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.zone_avertie {
                self.zone.reduire(ctx);
            } else {
                self.zone_dialogue = true;
            }
        }

        // La barre de titre est à Windows (ou macOS), pas à egui : sans
        // ça, elle suit le thème du système et reste blanche au-dessus
        // d'une application sombre. Une fois suffit, la fenêtre vit
        // jusqu'à la fin.
        if !self.barre_sombre {
            self.barre_sombre = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
        }

        // Focus : conditionne le son des messages et la notification.
        let focused = ctx.input(|i| i.focused);
        // Le focus revient : ce qu'on avait sous les yeux sans le regarder
        // — le salon ouvert, fenêtre à l'arrière-plan — est lu maintenant.
        if focused && !self.window_focused {
            if let Some(c) = self.current {
                self.programmer_lu(c);
            }
        }
        self.window_focused = focused;

        // Diagnostic partagé : si l'option est cochée, le journal technique
        // part vers le serveur à son rythme (une minute, et que du neuf).
        self.maybe_flush_diag();

        // Une carte NVIDIA au pilote trop vieux pour NVENC : le dire une
        // fois, au démarrage, plutôt que de laisser la diffusion ramer en
        // logiciel sans explication.
        if !self.diffusion.pilote_averti && ki_video::inventaire_pret().is_some() {
            self.diffusion.pilote_averti = true;
            if let Some(avertissement) = ki_video::avertissement_pilote() {
                self.info = Some(avertissement);
            }
        }

        // La surveillance du clavier démarre à la première image : elle a
        // besoin du contexte pour réveiller la fenêtre, et lui seul sait
        // quand il existe.
        let touche_ptt = &self.touche_ptt;
        let ptt = self.ptt.get_or_insert_with(|| {
            let watcher = ptt::Watcher::start(ctx.clone());
            touche_ptt.brancher_clavier(&watcher);
            watcher
        });
        // Deux écritures atomiques, à chaque image : le fil suit les réglages
        // à chaud sans qu'on ait à le redémarrer. Hors push-to-talk il ne lit
        // même pas le clavier.
        ptt.watch((self.audio.mode == MicMode::Ptt).then_some(self.audio.ptt_key));
        ptt.set_release_ms(self.audio.ptt_release_ms);
        // Les raccourcis à bascule : autant de pressions vues, autant de
        // bascules — mais seulement en vocal, hors vocal elles n'ont pas de
        // sens et sont simplement consommées.
        ptt.watch_bascule(ptt::Bascule::Micro, self.audio.hotkey_micro);
        ptt.watch_bascule(ptt::Bascule::Sourd, self.audio.hotkey_sourd);
        // Le changeur de voix, lui, bascule partout — il sert aussi en jeu,
        // hors vocal, par le micro pour les jeux.
        ptt.watch_bascule(ptt::Bascule::Changeur, self.audio.hotkey_changeur);
        // Le raccourci de l'enregistreur de clips, tenu tant qu'il tourne.
        // L'appui, lui, agit sans passer par ici : voir `brancher_declencheur`.
        ptt.watch_raccourci(self.clips.enregistreur.as_ref().map(|_| self.clips.reglages.raccourci));
        let pressions = (ptt.pressions(ptt::Bascule::Micro), ptt.pressions(ptt::Bascule::Sourd));
        let pressions_changeur = ptt.pressions(ptt::Bascule::Changeur);
        if pressions_changeur != self.vocal.hotkey_changeur_vues {
            let bascule = pressions_changeur.wrapping_sub(self.vocal.hotkey_changeur_vues) % 2 == 1;
            self.vocal.hotkey_changeur_vues = pressions_changeur;
            if bascule {
                self.basculer_changeur();
            }
        }
        if pressions != self.vocal.hotkey_vues {
            let (micro, sourd) = (
                pressions.0.wrapping_sub(self.vocal.hotkey_vues.0) % 2 == 1,
                pressions.1.wrapping_sub(self.vocal.hotkey_vues.1) % 2 == 1,
            );
            self.vocal.hotkey_vues = pressions;
            if self.voice_channel.is_some() {
                if sourd {
                    self.basculer_sourd();
                }
                if micro {
                    self.basculer_micro();
                }
            }
        }
        self.tick_clips(ctx);
        // Sortir du vocal rend l'écoute : sourd hors vocal, ce serait des
        // notifications muettes sans qu'on sache pourquoi.
        if self.vocal.sourd && self.voice_channel.is_none() {
            self.vocal.sourd = false;
            self.apply_audio_settings();
        }

        // En vocal, la machine ne doit pas s'endormir : parler ne compte pas
        // comme de l'activité pour Windows, et un portable dont on ne touche
        // pas le clavier se mettait en veille en pleine conversation.
        self.veille.actualiser(self.voice_channel.is_some());

        self.traiter_reglages(ctx, frame);
        self.poll_events();
        // Les demandes de Claude, après les messages reçus : `etat` les voit.
        #[cfg(all(debug_assertions, windows))]
        self.mcp_tick(ctx);
        self.envoyer_liens_prets();
        // Après les messages reçus : c'est eux qui programment les `Lu`.
        self.tick_lus(ctx);
        self.check_connect_timeout();
        // La reprise se déclenche depuis le rendu, comme la sonde des
        // serveurs : c'est sans risque ici, l'écran de connexion se repeint
        // au moins une fois par seconde de toute façon (voir
        // `repaint_delay`). Ce serait faux ailleurs — une horloge que
        // personne ne fait tourner ne sonne jamais.
        self.tick_reprise(ctx);
        self.auto_connexion(ctx);
        self.tick_presence();
        self.verifier_diffusion_precedente();
        self.update_voice();
        // Un seul instantané par image, pris ici : l'écran principal l'affiche,
        // et c'est lui qui dit s'il faut une image de plus.
        let voice = self.voice_snapshot();
        self.ranger_ancre_apprise(&voice.stats);
        self.tick_loupedeck(ctx, &voice);

        // Un message est arrivé pendant que la fenêtre était à l'arrière-plan :
        // la barre des tâches clignote (l'équivalent sobre d'une notification).
        if self.wants_attention {
            self.wants_attention = false;
            // Une mention insiste jusqu'au retour du focus ; un simple
            // message ne fait qu'un signe.
            let genre = if self.attention_critique {
                egui::UserAttentionType::Critical
            } else {
                egui::UserAttentionType::Informational
            };
            self.attention_critique = false;
            // Réduite dans la zone, il n'y a plus de bouton à faire
            // clignoter : c'est l'icône qui signale, juste en dessous.
            if !self.zone.reduite() {
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(genre));
            }
        }
        // Réduite : pastille et tooltip sur l'icône selon ce qui attend.
        if self.zone.reduite() {
            let poke = self.zone_poke.clone();
            self.zone.signaler(self.total_non_lus(), poke.as_deref());
        }

        // Le titre compte les non-lus : « (3) ki-chat » se lit dans la barre
        // des tâches sans ouvrir la fenêtre. Renvoyé seulement s'il change.
        let titre = match self.total_non_lus() {
            0 => "ki-chat".to_string(),
            n => format!("({n}) ki-chat"),
        };
        if titre != self.titre_fenetre {
            self.titre_fenetre = titre.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(titre));
        }

        // Le prochain réveil, demandé ici aussi : fenêtre cachée, il n'y a
        // pas d'`interface` pour le faire (voir la fin de celle-ci).
        if let Some(delai) = self.repaint_delay(&voice) {
            ctx.request_repaint_after(delai);
        }
        self.instantane_voix = Some(voice);
    }

    /// Ce que l'image dessine. eframe ne l'appelle que si la fenêtre se
    /// voit, ou l'une des siennes (`App::ui`) — juste après `logique`.
    fn interface(&mut self, ui: &mut egui::Ui) {
        let ctx = &ui.ctx().clone();
        if !self.premiere_image {
            self.premiere_image = true;
            tracing::info!("interface prête : première image");
        }
        // Les images VALORANT arrivées depuis l'image d'avant deviennent des
        // textures ; rien ne se télécharge ici.
        self.catalogue.preparer(ctx);
        let voice = self.instantane_voix.take().unwrap_or_else(|| self.voice_snapshot());

        if self.welcomed {
            // Avant tout le reste : ouvert, le panneau des suggestions garde Échap.
            self.suggestions.echap(ctx);
            // Échap ferme la fenêtre la plus « en avant » — et elle seule : la
            // touche est prise, pour que la zone de saisie derrière ne la
            // lise pas à son tour. Fermer les réglages annulait aussi la
            // modification en cours, et effaçait son texte.
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                let mut prise = true;
                if self.atelier.est_ouvert() {
                    self.atelier.fermer();
                } else if self.visionneuse.est_ouverte() {
                    self.visionneuse.fermer();
                } else if self.clips.partage.as_ref().is_some_and(|p| p.envoi.is_none()) {
                    self.clips.partage = None;
                } else if self.show_settings {
                    self.close_settings();
                } else if self.admin.ouverte {
                    self.close_admin();
                } else if self.show_account {
                    self.close_account();
                } else if self.soundboard.ouvert {
                    self.soundboard.ouvert = false;
                } else if self.portes.ouvert {
                    self.portes.ouvert = false;
                } else if self.fiche.is_some() {
                    self.fiche = None;
                    self.comparee = None;
                    self.valo.fermer_fiche();
                } else if self.valo.ouvert {
                    self.valo.ouvert = false;
                } else if self.clips.ouverte {
                    self.clips.ouverte = false;
                } else {
                    prise = false;
                }
                if prise {
                    prendre_echap(ctx);
                }
            }
            self.main_screen(ui, &voice);
        } else {
            self.login_screen(ui);
        }
        self.update_window(ctx);
        self.zone_dialogue_window(ctx);

        // Ce que l'appli a à dire sans qu'on l'ait demandé — « connexion
        // rétablie », « réglages exportés »… — ne s'affichait que dans une
        // ligne des fenêtres Réglages, Mon compte et Administration : ces
        // fenêtres fermées, le message n'était écrit nulle part. Il se pose
        // maintenant dans un coin, le temps de se lire.
        if !(self.show_settings || self.show_account || self.admin.ouverte) {
            if let Some(message) = self.info.take() {
                self.toasts.ajouter(Ton::Info, message, ctx.input(|i| i.time));
            }
        }
        // Au-dessus de la barre du bas et de la zone de saisie : un toast
        // ne doit pas recouvrir ce qu'on est en train d'écrire.
        self.toasts.show(ctx, 130.0);

        // Repeint périodique **seulement s'il y a quelque chose qui bouge**.
        //
        // Il était inconditionnel : vingt images par seconde à l'écran de
        // connexion, fenêtre réduite, application en arrière-plan pendant une
        // partie. Vingt reconstructions complètes de l'arbre de widgets, vingt
        // mises en page, vingt téléversements de maillage vers le GPU, par
        // seconde, pour un écran qui ne changeait pas.
        //
        // Ce qui a rendu la chose possible : la touche push-to-talk est
        // désormais lue sur son propre fil, qui réveille la fenêtre aux seuls
        // changements. Tout le reste de ce qui arrive de l'extérieur —
        // messages, événements réseau, images téléchargées, sondes de
        // serveurs — appelle déjà `request_repaint()` en arrivant.
        if let Some(delai) = self.repaint_delay(&voice) {
            ctx.request_repaint_after(delai);
        }

        self.perf.fin_image();
    }

    fn sauver(&mut self, storage: &mut dyn eframe::Storage) {
        // Des réglages viennent d'être importés : ils sont déjà dans le
        // fichier, et ce que l'appli a encore en mémoire — ceux d'avant — ne
        // doit pas les recouvrir avant le redémarrage.
        if self.reglages_importes {
            return;
        }
        storage.set_string(prefs::CLE_VERSION, prefs::VERSION.to_string());
        storage.set_string(prefs::CLE_APPLI, update::current().to_string());
        // Ces deux-là n'étaient jamais sauvés : le volume du son des streams
        // et « Ce salon / Partout » de la recherche repartaient de zéro.
        storage.set_string("regard_volume", format!("{}", self.regard.volume));
        storage.set_string("search_ici", if self.recherche.ici { "on" } else { "off" }.into());
        servers::save(storage, &self.book);
        storage.set_string(
            "session_auto",
            self.session_auto.map(|id| id.to_string()).unwrap_or_default(),
        );
        storage.set_string(
            "window_maximized",
            if self.maximized { "on" } else { "off" }.into(),
        );
        // Géométrie héritée des versions qui laissaient eframe la persister.
        // La vider est ce qui corrige réellement la fenêtre minuscule : au
        // démarrage suivant, eframe ne trouve plus rien à désérialiser et
        // laisse la position au système. On ne touche à rien d'autre — le
        // carnet de serveurs vit dans le même fichier.
        storage.set_string("window", String::new());
        storage.set_string("sfx_on", if self.sfx_on { "on" } else { "off" }.into());
        storage.set_string("sfx_volume", format!("{}", self.sfx_volume));
        storage.set_string("visionneuse_volume", format!("{}", self.visionneuse.volume));
        self.audio.save(storage);
        self.clips.reglages.save(storage);
        storage.set_string(
            "sfx_muted",
            self.sfx_muted.iter().cloned().collect::<Vec<_>>().join(","),
        );
        storage.set_string("notif", self.notif.cle().into());
        storage.set_string("pokes", if self.pokes_accepter { "on" } else { "off" }.into());
        storage.set_string("reduire_zone", if self.reduire_zone { "on" } else { "off" }.into());
        storage.set_string("zone_avertie", if self.zone_avertie { "on" } else { "off" }.into());
        storage.set_string("update_skipped", self.updater.skipped().to_string());
        storage.set_string("url", self.url.clone());
        storage.set_string("username", self.username.clone());
        storage.set_string("reglages_onglet", self.reglages_onglet.cle().into());
        storage.set_string("valorant_presence", if self.valorant_presence { "on" } else { "off" }.into());
        storage.set_string("valorant_medailles", if self.valorant_medailles { "on" } else { "off" }.into());
        storage.set_string("comptes_tus", self.comptes_tus.join(","));
        storage.set_string("journal_parties", if self.journal_parties { "on" } else { "off" }.into());
        storage.set_string("journal_propose", if self.journal_propose { "oui" } else { "non" }.into());
        storage.set_string("presence_jeux", if self.presence_jeux { "on" } else { "off" }.into());
        self.soundboard.save(storage);
        self.valo.save(storage);
        storage.set_string("loupedeck", if self.loupedeck_etat.actif { "on" } else { "off" }.into());
        storage.set_string("loupedeck_config", self.loupedeck_etat.config.ecrire());
        storage.set_string("soundboard_visible", if self.soundboard_visible { "on" } else { "off" }.into());
        storage.set_string("diag_share", if self.diag.partage { "on" } else { "off" }.into());
        storage.set_string("diag_crash_envoye", self.diag.crash_envoye.clone());
        self.diffusion.reglages.save(storage);
        self.overlay.save(storage);
    }

    fn quitter_proprement(&mut self) {
        // Une fermeture ne doit pas pouvoir s'éterniser — fenêtre figée,
        // « ne répond pas », mise à jour jamais relancée : un fil la
        // surveille, et termine le processus s'il le faut.
        secours::surveiller_fermeture();
        // L'icône quitte la zone de notification tout de suite, et son
        // garde-fou s'arrête : ki-chat a l'air fermé parce qu'il l'est.
        self.zone.retirer();
        tracing::info!("fermeture : arrêt des clips, de la diffusion et de la connexion");
        // L'enregistreur s'arrête proprement : son marqueur de plantage est
        // levé, sinon le prochain démarrage croirait à une mort brutale.
        self.arreter_clips();
        // Diffusion et visionnage : le marqueur de diffusion est levé (même
        // raison), le serveur prévenu tant que la connexion vit ; leurs fils
        // s'arrêtent avec le démontage, plus bas.
        let diffusion = self.diffusion.live.take();
        if diffusion.is_some() {
            secours::lever_diffusion();
            self.send(ClientMsg::StreamStop);
        }
        let regard = self.regard.flux.take();
        if let Some(r) = &regard {
            self.send(ClientMsg::Unwatch { stream_id: r.stream_id });
        }
        if let Some(mut conn) = self.conn.take() {
            conn.quitter_borne(FERMETURE_CONNEXION);
        }
        tracing::info!("fermeture : connexion rendue");
        // Le verrou d'instance est rendu dès maintenant : relancé à
        // l'instant, un nouveau ki-chat démarre sans attendre la suite.
        instance::rendre();
        // Le reste — fils de veille, push-to-talk, moteur vocal, sorties —
        // se démonte sur un fil à part, attendu deux secondes au plus : ce
        // qui traîne est nommé au journal, et ne retient plus la fenêtre.
        let mut demontage = secours::Demontage::default();
        if let Some(g) = diffusion {
            demontage.etape("diffusion", move || g.arreter());
        }
        if let Some(r) = regard {
            demontage.etape("visionnage", move || r.arreter());
        }
        if let Some(v) = self.veilleur_valorant.take() {
            demontage.etape("veille VALORANT", move || drop(v));
        }
        if let Some(v) = self.veilleur_jeux.take() {
            demontage.etape("veille des jeux", move || drop(v));
        }
        if let Some(p) = self.ptt.take() {
            demontage.etape("push-to-talk et raccourcis", move || drop(p));
        }
        if let Some(s) = self.sortie_medias.take() {
            demontage.etape("sortie des médias", move || drop(s));
        }
        if let Some(l) = self.labo.take() {
            demontage.etape("labo vidéo", move || drop(l));
        }
        if let Some(d) = self.docteur.take() {
            demontage.etape("docteur audio", move || drop(d));
        }
        let link = self.link.clone();
        demontage.etape("moteur vocal", move || link.arreter());
        demontage.jouer(FERMETURE_DEMONTAGE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Une pastille compte, retient si l'on y est nommé, et fixe le repère
    /// du séparateur au **premier** non-lu — pas au dernier.
    #[test]
    fn une_pastille_compte_et_retient_le_premier_non_lu() {
        let mut n = NonLu::default();
        n.ajouter(100, false);
        assert_eq!(n, NonLu { nb: 1, mention: false, depuis: 99 });
        n.ajouter(200, true);
        n.ajouter(300, false);
        assert_eq!(n, NonLu { nb: 3, mention: true, depuis: 99 });
        // Le repère lui-même est lu, le message qui le suit ne l'est pas.
        assert!(n.depuis < 100 && n.depuis >= 99);
        // Le premier message d'un salon (horodatage 0) ne fait pas déborder.
        let mut z = NonLu::default();
        z.ajouter(0, false);
        assert_eq!(z.depuis, 0);
    }

    /// Un salon n'est lu que sous les yeux : le bon salon, fenêtre au premier
    /// plan, fil en bas. Un fil remonté dans le passé au retour du focus ne
    /// marque rien lu — c'est ce qui aurait tenu pour lus des messages
    /// jamais affichés.
    #[test]
    fn un_salon_n_est_lu_que_sous_les_yeux() {
        assert!(lecture_effective(true, true, Some(3), 3));
        assert!(!lecture_effective(false, true, Some(3), 3), "fenêtre à l'arrière-plan");
        assert!(!lecture_effective(true, false, Some(3), 3), "fil remonté dans le passé");
        assert!(!lecture_effective(true, true, Some(4), 3), "on a changé de salon entre-temps");
        assert!(!lecture_effective(true, true, None, 3));
    }

    /// Le réglage se mémorise par une clé stable, et une clé inconnue
    /// retombe sur « tout » — le comportement d'avant le réglage.
    #[test]
    fn le_reglage_de_notification_se_relit_et_decide() {
        for n in Notif::TOUS {
            assert_eq!(Notif::depuis(n.cle()), n);
        }
        assert_eq!(Notif::depuis("n'importe quoi"), Notif::Tout);
        assert!(Notif::Tout.previent(false) && Notif::Tout.previent(true));
        assert!(!Notif::Mentions.previent(false) && Notif::Mentions.previent(true));
        assert!(!Notif::Rien.previent(false) && !Notif::Rien.previent(true));
    }

    /// L'attente entre deux tentatives double, plafonne, et surtout **se
    /// disperse**. La dispersion est ce qui empêche trente clients de frapper
    /// le serveur en même temps après un redémarrage : c'est la seule
    /// propriété de cette fonction qui vaille d'être testée.
    #[test]
    fn la_reprise_double_plafonne_et_se_disperse() {
        use std::time::Duration;

        // Chaque essai vaut entre la moitié et la totalité de son palier.
        for (essai, plein) in [(1u32, 1u64), (2, 2), (3, 4), (4, 8), (5, 16), (6, 30)] {
            let plein = Duration::from_secs(plein);
            for alea in [0u64, 1, 12_345, 999_999_999] {
                let d = Reprise::attente(essai, alea);
                assert!(d <= plein, "essai {essai} : {d:?} dépasse {plein:?}");
                assert!(d >= plein / 2, "essai {essai} : {d:?} sous la moitié de {plein:?}");
            }
        }

        // Plafonné : au-delà, on n'attend pas plus longtemps. Marteler moins
        // souvent ne sert plus à rien, et l'attente ne doit pas devenir telle
        // qu'on rate le retour du serveur.
        for essai in [7u32, 20, Reprise::MAX] {
            assert!(Reprise::attente(essai, 0) <= Reprise::PLAFOND);
        }

        // Et deux clients qui tirent deux nombres différents n'attendent pas
        // la même chose. Sans ça, tout le reste ne sert à rien : ils
        // repartiraient ensemble, échoueraient ensemble, et recommenceraient.
        let a = Reprise::attente(4, 0);
        let b = Reprise::attente(4, 3_999);
        assert_ne!(a, b, "deux tirages doivent donner deux attentes");
    }
}
