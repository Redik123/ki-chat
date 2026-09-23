//! Relais des partages d'écran (SFU vidéo) — jalon S1b de PLAN-STREAM.md.
//!
//! Le serveur route des trames chiffrées sans jamais les décoder : l'en-tête
//! clair ([`ki_protocol::MediaHeader`]) lui dit tout ce qu'il a besoin de
//! savoir (qui, quelle séquence, trame clé ou non, quelle qualité), la
//! charge reste opaque.
//!
//! # Le principe qui gouverne le relais
//!
//! **Une tâche de diffusion par spectateur, nourrie par une file de deux
//! trames.** C'est la parade au spectateur lent : sa file déborde, SES trames
//! sont jetées, et les autres ne s'en aperçoivent pas. Une diffusion naïve
//! (écrire à tous depuis l'ingestion) aurait mis tout le salon au rythme du
//! plus lent. Quand on jette à quelqu'un, on le marque « en attente de trame
//! clé » : il ne recevra plus que du décodable — les trames P d'après une
//! trame jetée ne sont que de la bouillie — et le streamer est prié (au plus
//! une fois par demi-seconde) d'en produire une.
//!
//! La mémoire des trames en transit est comptée globalement et bornée : le
//! compteur monte à l'ingestion, redescend quand la **dernière** copie part
//! (les spectateurs partagent la même allocation).
//!
//! # Deux qualités (0.1.46)
//!
//! Un seul débit pour tout le salon ne satisfait personne quand les
//! connexions diffèrent. Descendu pour le spectateur le plus lent, il rend
//! la vidéo « infâme mais fluide » pour tous ; ignoré (à partir de quatre
//! spectateurs, le plus lent ne comptait pas), il laisse ce dernier
//! « laguer énormément ». Un streamer qui le sait (`StreamStart.couches`)
//! encode donc à la demande une seconde image, **basse** — plus petite, plus
//! lente, à son propre débit —, et le serveur place chaque spectateur qui le
//! sait (`Watch.couches`) sur celle que sa connexion avale : qui sature sous
//! le [`plancher_haute`] passe en basse au lieu de tirer tout le monde vers
//! le bas, retente la haute de temps en temps (trente secondes, puis de
//! plus en plus rarement s'il échoue), et y reste si elle passe. La haute
//! garde le palier commun pour les connexions moyennes ; la basse a le sien.
//! Et quand plus personne ne regarde la haute, elle ne part plus du tout
//! (budget 0) : la liaison montante du streamer n'a pas à porter une image
//! que personne ne voit. Un battement, quatre fois par seconde, prend ces
//! décisions même quand rien n'arrive du streamer.
//!
//! # La connexion du streamer
//!
//! Le palier ne regardait que les spectateurs. Un streamer dont la liaison
//! montante ne suit pas accumulait jusqu'à une seconde de retard avant de
//! jeter des trames — et chaque trame jetée exige une trame clé, plus
//! lourde : la spirale. Le serveur lit désormais l'arrivée de ses trames :
//! des trous dans la séquence, ou un retard (arrivée moins horodatage de
//! capture) qui monte de plus de 200 ms au-dessus de son plancher deux
//! secondes de suite — et le palier descend sous ce qui arrive vraiment.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ki_protocol::{ChannelId, MediaHeader, StreamMeta, UserId};

/// Deux diffusions au plus par salon (v1) : au-delà, plus personne ne sait
/// quel écran regarder, et la liaison montante du serveur non plus.
const MAX_PAR_SALON: usize = 2;
/// Mémoire totale des trames en transit vers les spectateurs.
const MEM_MAX: usize = 32 * 1024 * 1024;
/// File par spectateur : deux trames d'avance, pas une de plus.
const FILE_VIEWER: usize = 2;
/// Une demande de trame clé au plus par demi-seconde, par stream et par
/// qualité.
const IDR_COOLDOWN: Duration = Duration::from_millis(500);

/// En dessous de ce débit, la qualité haute ne descend pas pour un
/// spectateur qui sait changer de qualité : il passe en basse.
const PLANCHER_HAUTE_MIN: u32 = 2500;
/// L'échelle de la qualité basse, du plus haut au plus bas — la résolution
/// suit le débit chez le streamer (720p30 à 1500, 480p30 à 700, 360p30 en
/// dessous).
const PALIERS_BASSE: [u32; 5] = [2500, 1500, 1000, 700, 450];
/// Sans spectateur en basse depuis ce délai, elle s'éteint.
const BASSE_INUTILE: Duration = Duration::from_secs(10);
/// Demandée, la basse doit arriver dans ce délai — sinon le streamer ne sait
/// pas la produire (encodeur récalcitrant), et l'on s'en passe un moment.
const BASSE_ATTENDUE: Duration = Duration::from_secs(4);
const BASSE_PANNE: Duration = Duration::from_secs(120);
/// Le premier essai de retour en haute, et l'attente la plus longue entre
/// deux essais (elle double à chaque échec).
const ESSAI_PREMIER: Duration = Duration::from_secs(30);
const ESSAI_MAX: Duration = Duration::from_secs(480);
/// Un essai qui tient ce délai sans saturer est réussi.
const ESSAI_DUREE: Duration = Duration::from_secs(8);
/// Le retard au-dessus de son plancher qui signe une liaison montante qui
/// ne suit pas, et pendant combien de secondes de suite.
const RETARD_MONTANT_US: i64 = 200_000;
const RETARD_SECONDES: u32 = 2;
/// La fenêtre du plancher de retard, en secondes : assez longue pour qu'une
/// saturation durable ne devienne pas la norme, assez courte pour que la
/// dérive des horloges n'y compte pas.
const PLANCHER_FENETRE: usize = 120;
/// Calme avant de remonter d'un cran le palier de la liaison montante.
const MONTANT_CALME: Duration = Duration::from_secs(10);

/// Une trame prête à diffuser. Les octets sont partagés entre spectateurs
/// (Arc) ; la mémoire est rendue au compteur global quand la dernière copie
/// est écrite — c'est le Drop qui fait la comptabilité, aucun chemin ne peut
/// l'oublier.
pub struct Trame {
    pub bytes: Vec<u8>,
    /// Trame clé : elle rend caduques toutes celles d'avant.
    pub idr: bool,
    /// La priorité QUIC de son flux, calculée à l'ingestion sur la séquence
    /// de SA qualité — les deux qualités ont chacune la leur.
    pub priorite: i32,
    mem: Arc<AtomicUsize>,
}

impl Drop for Trame {
    fn drop(&mut self) {
        self.mem.fetch_sub(self.bytes.len(), Ordering::Relaxed);
    }
}

/// Ce qu'un spectateur avale vraiment, et ses saturations : la matière du
/// palier de débit (PLAN-STREAM.md, S3).
#[derive(Default)]
struct Mesure {
    /// Octets acceptés par sa connexion. La fenêtre d'envoi est bornée à
    /// 1 Mio : ce qui est accepté est parti, ou presque.
    octets: AtomicU64,
    /// Écritures hors délai, trames annulées faute de place, file pleine :
    /// autant de signes que le lien ne suit pas.
    saturations: AtomicU32,
}

/// Un spectateur : sa file, son drapeau « il me faut une trame clé », la
/// tâche qui écrit vers sa connexion, et la qualité qu'il regarde.
struct Viewer {
    tx: tokio::sync::mpsc::Sender<Arc<Trame>>,
    needs_idr: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
    mesure: Arc<Mesure>,
    /// Sa connexion, pour le son du jeu : des datagrammes envoyés tels
    /// quels, sans file ni tâche — un paquet de son perdu ne s'attend pas.
    conn: quinn::Connection,
    /// Il sait passer d'une qualité à l'autre (`Watch.couches`).
    couches: bool,
    /// Il regarde la qualité basse.
    basse: bool,
    /// Depuis quand sur sa qualité actuelle.
    depuis: Instant,
    /// En essai sur la haute : un échec le renvoie en basse, et l'essai
    /// suivant attendra deux fois plus.
    en_essai: bool,
    /// Le prochain essai permis, et l'attente qui suivra un échec.
    essai_le: Instant,
    attente_essai: Duration,
}

/// Une diffusion en cours.
struct Live {
    streamer: UserId,
    /// Salon vocal du streamer au démarrage : la condition d'accès.
    channel: ChannelId,
    key_hex: String,
    meta: StreamMeta,
    /// Première séquence vue de la qualité haute : la base des priorités.
    seq_start: Option<u64>,
    viewers: HashMap<UserId, Viewer>,
    last_idr_ask: Instant,
    palier: Palier,
    /// Le streamer sait produire la qualité basse.
    couches: bool,
    basse: Basse,
    montant: Montant,
    /// Le dernier budget dit au streamer.
    annonce: Budget,
}

/// Le palier de débit de la qualité haute : ce que le serveur demande au
/// streamer d'après ce que ses spectateurs avalent.
struct Palier {
    courant: u32,
    derniere_montee: Instant,
    derniere_mesure: Instant,
    /// Les compteurs de chaque spectateur à la mesure précédente.
    avant: HashMap<UserId, (u64, u32)>,
}

impl Palier {
    fn neuf(plafond: u32) -> Self {
        Self {
            courant: plafond,
            derniere_montee: Instant::now(),
            derniere_mesure: Instant::now(),
            avant: HashMap::new(),
        }
    }
}

/// La qualité basse d'une diffusion.
struct Basse {
    /// Le débit demandé au streamer ; `None` : éteinte.
    kbps: Option<u32>,
    /// Première séquence vue : la base de ses priorités.
    seq_start: Option<u64>,
    /// Quand elle a été demandée, et quand sa dernière trame est arrivée :
    /// de quoi voir qu'un streamer ne la produit pas.
    demandee_le: Option<Instant>,
    derniere_trame: Option<Instant>,
    last_idr_ask: Instant,
    derniere_montee: Instant,
    /// Depuis quand plus personne ne la regarde.
    sans_spectateur: Option<Instant>,
    /// Le streamer n'a pas su la produire : on s'en passe jusque-là.
    panne_jusqu: Option<Instant>,
}

impl Basse {
    fn eteinte() -> Self {
        Self {
            kbps: None,
            seq_start: None,
            demandee_le: None,
            derniere_trame: None,
            last_idr_ask: Instant::now() - IDR_COOLDOWN,
            derniere_montee: Instant::now(),
            sans_spectateur: None,
            panne_jusqu: None,
        }
    }
}

/// Ce que le serveur demande au streamer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// Le débit de la qualité haute, en kbit/s.
    pub haute: u32,
    /// La qualité basse à encoder en plus, s'il faut.
    pub basse: Option<u32>,
    /// La haute est bridée par la liaison montante du streamer.
    pub montant: bool,
}

/// Un spectateur mesuré sur la dernière seconde.
#[derive(Clone, Copy, Debug)]
struct Avale {
    kbps: u32,
    sature: bool,
}

/// Les paliers possibles sous un plafond (le réglage du streamer), du plus
/// haut au plus bas.
fn paliers(plafond: u32) -> Vec<u32> {
    let mut v = vec![plafond];
    v.extend(
        [8000u32, 6000, 4000, 2500, 1500, 1000, 700, 450]
            .into_iter()
            .filter(|p| *p < plafond),
    );
    v
}

/// Sous ce débit, la haute ne descend pas pour un spectateur qui sait
/// changer de qualité : il passe en basse. La moitié du réglage, 2500 kbit/s
/// au moins — en deçà, la basse (720p30 à 2500) vaut autant, et seul lui
/// y perd.
pub fn plancher_haute(plafond: u32) -> u32 {
    PLANCHER_HAUTE_MIN.max(plafond / 2)
}

/// Le palier suivant : descente immédiate sous 0,9 fois ce qu'avale le
/// spectateur saturé le plus lent — le second, à partir de quatre
/// spectateurs : un seul lien pourri ne dégrade pas tout le monde — ;
/// remontée d'un cran après cinq secondes sans saturation. `None` : rien ne
/// change.
fn prochain_palier(courant: u32, plafond: u32, spectateurs: &[Avale], depuis_montee: Duration) -> Option<u32> {
    if plafond == 0 {
        return None;
    }
    prochain_palier_sur(&paliers(plafond), courant, spectateurs, depuis_montee)
}

/// La même règle, sur une échelle donnée (du plus haut au plus bas).
fn prochain_palier_sur(echelle: &[u32], courant: u32, spectateurs: &[Avale], depuis_montee: Duration) -> Option<u32> {
    if echelle.is_empty() || spectateurs.is_empty() {
        return None;
    }
    let mut satures: Vec<u32> = spectateurs.iter().filter(|s| s.sature).map(|s| s.kbps).collect();
    satures.sort_unstable();
    let reference = if spectateurs.len() >= 4 {
        satures.get(1).copied()
    } else {
        satures.first().copied()
    };
    if let Some(r) = reference {
        let vise = (f64::from(r) * 0.9) as u32;
        let plancher = *echelle.last().unwrap_or(&courant);
        let cible = echelle.iter().copied().find(|p| *p <= vise).unwrap_or(plancher);
        return (cible < courant).then_some(cible);
    }
    if depuis_montee >= Duration::from_secs(5) {
        return echelle.iter().rev().copied().find(|p| *p > courant);
    }
    None
}

/// Où va un spectateur.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mouvement {
    VersBasse,
    VersHaute,
}

/// La place d'un spectateur, vue une seconde — sans horloge, pour se
/// tester. En haute, il passe en basse s'il sature sous le plancher ; en
/// basse, il retente la haute quand il ne sature pas et que son essai est
/// permis. Qui ne sait pas changer de qualité reste en haute.
fn placer(a: Avale, couches: bool, basse: bool, plancher: u32, essai_permis: bool) -> Option<Mouvement> {
    if !couches {
        return None;
    }
    if !basse {
        let tenable = (f64::from(a.kbps) * 0.9) as u32;
        return (a.sature && tenable < plancher).then_some(Mouvement::VersBasse);
    }
    (!a.sature && essai_permis).then_some(Mouvement::VersHaute)
}

/// Le plus haut palier de la basse sous la haute effective — elle ne sert à
/// rien à égalité.
fn max_basse(haute: u32) -> u32 {
    PALIERS_BASSE.iter().copied().find(|p| *p < haute).unwrap_or(PALIERS_BASSE[PALIERS_BASSE.len() - 1])
}

/// Le débit de départ de la basse pour un spectateur qui y arrive : sous
/// 0,9 fois ce qu'il avalait, dans l'échelle, sans dépasser `max`.
fn palier_initial_basse(avale_kbps: u32, max: u32) -> u32 {
    let vise = ((f64::from(avale_kbps) * 0.9) as u32).min(max);
    PALIERS_BASSE
        .iter()
        .copied()
        .find(|p| *p <= vise)
        .unwrap_or(PALIERS_BASSE[PALIERS_BASSE.len() - 1])
}

/// Le palier suivant de la liaison montante : saturée, sous 0,85 fois ce
/// qui arrive vraiment (moins la basse, qui passe par le même lien) ;
/// calme depuis dix secondes, un cran plus haut. `None` : rien ne change.
fn prochain_montant(
    courant: u32,
    plafond: u32,
    recu_kbps: u32,
    sature: bool,
    basse_kbps: u32,
    depuis_change: Duration,
) -> Option<u32> {
    if plafond == 0 {
        return None;
    }
    let echelle = paliers(plafond);
    let courant = courant.min(plafond);
    if sature {
        let vise = ((f64::from(recu_kbps) * 0.85) as u32).saturating_sub(basse_kbps);
        let plancher = *echelle.last().unwrap_or(&plafond);
        let cible = echelle.iter().copied().find(|p| *p <= vise).unwrap_or(plancher);
        return (cible < courant).then_some(cible);
    }
    if courant < plafond && depuis_change >= MONTANT_CALME {
        return echelle.iter().rev().copied().find(|p| *p > courant);
    }
    None
}

/// Ce qu'on lit de l'arrivée des trames d'un streamer : ce qui arrive, ce
/// qui manque, et le retard qui monte.
struct Montant {
    /// L'origine des instants d'arrivée.
    t0: Instant,
    /// Octets reçus depuis le dernier bilan, les deux qualités.
    octets: u64,
    /// La plus haute séquence vue, par qualité (haute, basse) ; les trous
    /// et ce qui les a comblés depuis le dernier bilan — les trames arrivent
    /// dans le désordre, un flux chacune.
    seq_max: [Option<u64>; 2],
    trous: u64,
    remplis: u64,
    /// Le plus petit retard de la seconde en cours (arrivée − capture, en
    /// µs, horloges confondues : seul son mouvement compte).
    min_seconde: Option<i64>,
    /// Les minima des secondes passées : leur minimum est le plancher.
    minima: VecDeque<i64>,
    /// Secondes de suite au-dessus du plancher.
    retard_s: u32,
    /// Le palier imposé par la liaison montante (le plafond : aucun).
    palier: u32,
    dernier_change: Instant,
}

impl Montant {
    fn neuf(plafond: u32) -> Self {
        Self {
            t0: Instant::now(),
            octets: 0,
            seq_max: [None, None],
            trous: 0,
            remplis: 0,
            min_seconde: None,
            minima: VecDeque::new(),
            retard_s: 0,
            palier: plafond,
            dernier_change: Instant::now(),
        }
    }

    /// Une trame arrive, à `arrivee_us` depuis `t0`.
    fn observer(&mut self, basse: bool, seq: u64, pts_us: u64, taille: usize, arrivee_us: i64) {
        self.octets += taille as u64;
        let i = usize::from(basse);
        match self.seq_max[i] {
            Some(max) if seq > max => {
                self.trous += seq - max - 1;
                self.seq_max[i] = Some(seq);
            }
            Some(max) if seq < max => self.remplis += 1,
            Some(_) => {}
            None => self.seq_max[i] = Some(seq),
        }
        let retard = arrivee_us.saturating_sub(pts_us.min(i64::MAX as u64) as i64);
        self.min_seconde = Some(self.min_seconde.map_or(retard, |m| m.min(retard)));
    }

    /// Le bilan d'une seconde : ce qui est arrivé (kbit/s), et si la
    /// liaison montante sature — des trames manquent, ou le retard monte
    /// depuis deux secondes. Remet les compteurs de la seconde à zéro.
    fn bilan(&mut self, secondes: f64) -> (u32, bool) {
        let kbps = (self.octets as f64 * 8.0 / 1000.0 / secondes.max(0.1)) as u32;
        self.octets = 0;
        let manquent = self.trous.saturating_sub(self.remplis) > 0;
        self.trous = 0;
        self.remplis = 0;
        if let Some(m) = self.min_seconde.take() {
            self.minima.push_back(m);
            while self.minima.len() > PLANCHER_FENETRE {
                self.minima.pop_front();
            }
            let plancher = self.minima.iter().copied().min().unwrap_or(m);
            if m - plancher > RETARD_MONTANT_US {
                self.retard_s += 1;
            } else {
                self.retard_s = 0;
            }
        }
        (kbps, manquent || self.retard_s >= RETARD_SECONDES)
    }
}

/// Ce que l'ingestion d'une trame a donné.
pub enum Ingest {
    /// Relayée ; `ask_haute` / `ask_basse` disent s'il faut prier le
    /// streamer pour une trame clé sur l'une ou l'autre qualité, `budget` ce
    /// qu'il faut lui demander s'il vient de changer.
    Ok { ask_haute: bool, ask_basse: bool, budget: Option<Budget> },
    /// Ce compte ne diffuse pas, ou l'en-tête ment sur le stream_id.
    Refuse,
}

#[derive(Default)]
struct Issue {
    ask_haute: bool,
    ask_basse: bool,
    budget: Option<Budget>,
}

/// Une mesure par seconde : ce que chaque spectateur a avalé, ce qui
/// arrive du streamer, les placements, les paliers — et le budget à dire
/// au streamer s'il change.
fn mesurer(live: &mut Live) -> Issue {
    let mut issue = Issue::default();
    let dt = live.palier.derniere_mesure.elapsed();
    if dt < Duration::from_secs(1) {
        return issue;
    }
    let maintenant = Instant::now();
    live.palier.derniere_mesure = maintenant;
    let plafond = live.meta.kbps;
    let secondes = dt.as_secs_f64().max(0.1);

    // 1. La liaison montante du streamer.
    let (recu, montant_sature) = live.montant.bilan(secondes);
    if let Some(p) = prochain_montant(
        live.montant.palier,
        plafond,
        recu,
        montant_sature,
        live.basse.kbps.unwrap_or(0),
        live.montant.dernier_change.elapsed(),
    ) {
        tracing::info!(
            "diffusion {} : liaison montante du streamer {} — palier {p} kbit/s (reçu {recu} kbit/s)",
            live.streamer,
            if montant_sature { "saturée" } else { "rétablie" }
        );
        live.montant.palier = p;
        live.montant.dernier_change = maintenant;
    }

    // 2. Ce que chaque spectateur a avalé depuis la dernière mesure.
    let mut avales: HashMap<UserId, Avale> = HashMap::with_capacity(live.viewers.len());
    let mut avant = HashMap::with_capacity(live.viewers.len());
    for (user, v) in &live.viewers {
        let octets = v.mesure.octets.load(Ordering::Relaxed);
        let sat = v.mesure.saturations.load(Ordering::Relaxed);
        let (o0, s0) = live.palier.avant.get(user).copied().unwrap_or((octets, sat));
        avant.insert(*user, (octets, sat));
        let kbps = ((octets.saturating_sub(o0)) as f64 * 8.0 / 1000.0 / secondes) as u32;
        avales.insert(*user, Avale { kbps, sature: sat > s0 });
    }
    live.palier.avant = avant;

    // 3. La basse demandée qui n'arrive pas : le streamer ne sait pas la
    //    produire. Tout le monde revient en haute, et l'on s'en passe.
    if live.basse.kbps.is_some() {
        let demandee = live.basse.demandee_le.unwrap_or(maintenant);
        let muette = match live.basse.derniere_trame {
            Some(t) if t >= demandee => t.elapsed() > BASSE_ATTENDUE,
            _ => demandee.elapsed() > BASSE_ATTENDUE,
        };
        if muette {
            tracing::warn!("diffusion {} : la qualité basse n'arrive pas — on s'en passe", live.streamer);
            live.basse.kbps = None;
            live.basse.demandee_le = None;
            live.basse.panne_jusqu = Some(maintenant + BASSE_PANNE);
            for v in live.viewers.values_mut().filter(|v| v.basse) {
                v.basse = false;
                v.en_essai = false;
                v.depuis = maintenant;
                v.needs_idr.store(true, Ordering::Relaxed);
                issue.ask_haute = true;
            }
        }
    }

    // 4. Placer chaque spectateur — quand le streamer sait produire la
    //    basse, et que sa propre liaison ne sature pas (alors ce ne sont
    //    pas les spectateurs qui coincent).
    let basse_possible = live.couches && plafond > 0 && live.basse.panne_jusqu.is_none_or(|t| maintenant >= t);
    if basse_possible && !montant_sature {
        let plancher = plancher_haute(plafond);
        let haute = live.palier.courant.min(live.montant.palier);
        let mut un_essai = false;
        for (user, v) in live.viewers.iter_mut() {
            let Some(a) = avales.get(user).copied() else { continue };
            match placer(a, v.couches, v.basse, plancher, maintenant >= v.essai_le) {
                Some(Mouvement::VersBasse) => {
                    if v.en_essai {
                        v.attente_essai = (v.attente_essai * 2).min(ESSAI_MAX);
                    }
                    v.basse = true;
                    v.en_essai = false;
                    v.depuis = maintenant;
                    v.essai_le = maintenant + v.attente_essai;
                    v.needs_idr.store(true, Ordering::Relaxed);
                    issue.ask_basse = true;
                    if live.basse.kbps.is_none() {
                        live.basse.kbps = Some(palier_initial_basse(a.kbps, max_basse(haute)));
                        live.basse.demandee_le = Some(maintenant);
                        live.basse.derniere_montee = maintenant;
                    }
                    tracing::info!(
                        "diffusion {} : le spectateur {user} passe en qualité basse ({} kbit/s avalés)",
                        live.streamer,
                        a.kbps
                    );
                }
                Some(Mouvement::VersHaute) if !un_essai => {
                    // Un essai par seconde au plus : deux spectateurs qui
                    // saturent ensemble ne se départageraient pas.
                    un_essai = true;
                    v.basse = false;
                    v.en_essai = true;
                    v.depuis = maintenant;
                    v.needs_idr.store(true, Ordering::Relaxed);
                    issue.ask_haute = true;
                }
                Some(Mouvement::VersHaute) => {}
                None => {
                    if v.en_essai && !a.sature && v.depuis.elapsed() >= ESSAI_DUREE {
                        v.en_essai = false;
                        v.attente_essai = ESSAI_PREMIER;
                    }
                }
            }
        }
    }

    // 5. Le palier de la haute, sur ceux qui la regardent — personne en
    //    haute : il revient au réglage.
    let hautes: Vec<Avale> = live
        .viewers
        .iter()
        .filter(|(_, v)| !v.basse)
        .filter_map(|(u, _)| avales.get(u).copied())
        .collect();
    if hautes.is_empty() {
        live.palier.derniere_montee = maintenant;
        live.palier.courant = plafond;
    } else if let Some(suivant) =
        prochain_palier(live.palier.courant, plafond, &hautes, live.palier.derniere_montee.elapsed())
    {
        live.palier.courant = suivant;
        live.palier.derniere_montee = maintenant;
    }
    let haute = if plafond == 0 { 0 } else { live.palier.courant.min(live.montant.palier) };

    // 6. Le palier de la basse, sur ceux qui la regardent ; éteinte quand
    //    plus personne ne la regarde depuis dix secondes.
    let basses: Vec<Avale> = live
        .viewers
        .iter()
        .filter(|(_, v)| v.basse)
        .filter_map(|(u, _)| avales.get(u).copied())
        .collect();
    if live.viewers.values().any(|v| v.basse) {
        live.basse.sans_spectateur = None;
        if let Some(k) = live.basse.kbps {
            let max = max_basse(haute);
            let echelle: Vec<u32> = PALIERS_BASSE.iter().copied().filter(|p| *p <= max).collect();
            let mut k = k.min(max);
            if let Some(n) = prochain_palier_sur(&echelle, k, &basses, live.basse.derniere_montee.elapsed()) {
                k = n;
                live.basse.derniere_montee = maintenant;
            }
            live.basse.kbps = Some(k);
        }
    } else if live.basse.kbps.is_some() {
        let depuis = *live.basse.sans_spectateur.get_or_insert(maintenant);
        if depuis.elapsed() >= BASSE_INUTILE {
            live.basse.kbps = None;
            live.basse.demandee_le = None;
            live.basse.sans_spectateur = None;
        }
    }

    // 7. Le budget, dit au streamer s'il a changé. Personne en haute : elle
    //    ne part plus (0) — seulement vers un streamer qui le comprend, un
    //    client d'avant y lirait un palier.
    let personne_en_haute = !live.viewers.values().any(|v| !v.basse);
    let budget = Budget {
        haute: if live.couches && personne_en_haute { 0 } else { haute },
        basse: live.basse.kbps.filter(|_| live.couches),
        montant: live.montant.palier < live.palier.courant,
    };
    if budget != live.annonce {
        live.annonce = budget;
        issue.budget = Some(budget);
    }
    issue
}

#[derive(Default)]
struct Inner {
    next_id: u32,
    by_id: HashMap<u32, Live>,
}

/// La table des diffusions du serveur.
pub struct Streams {
    inner: Mutex<Inner>,
    mem: Arc<AtomicUsize>,
}

impl Streams {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                next_id: 1,
                by_id: HashMap::new(),
            }),
            mem: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Le stream que diffuse ce compte, s'il y en a un.
    pub fn stream_of(&self, user: UserId) -> Option<u32> {
        let inner = self.inner.lock().unwrap();
        inner
            .by_id
            .iter()
            .find(|(_, l)| l.streamer == user)
            .map(|(id, _)| *id)
    }

    /// Les diffusions en cours, pour le tableau de bord : le streamer, ses
    /// spectateurs, ce qu'il annonce, et le palier de débit courant de la
    /// qualité haute.
    pub fn resume(&self) -> Vec<(UserId, usize, StreamMeta, u32)> {
        let inner = self.inner.lock().unwrap();
        inner
            .by_id
            .values()
            .map(|l| (l.streamer, l.viewers.len(), l.meta, l.palier.courant.min(l.montant.palier)))
            .collect()
    }

    /// Démarre une diffusion. Idempotent : rediffuser renvoie l'existant.
    /// `couches` : le streamer sait produire la qualité basse.
    pub fn start(
        &self,
        streamer: UserId,
        channel: ChannelId,
        key_hex: String,
        meta: StreamMeta,
        couches: bool,
    ) -> Result<u32, &'static str> {
        let mut inner = self.inner.lock().unwrap();
        if let Some((id, _)) = inner.by_id.iter().find(|(_, l)| l.streamer == streamer) {
            return Ok(*id);
        }
        let dans_le_salon = inner
            .by_id
            .values()
            .filter(|l| l.channel == channel)
            .count();
        if dans_le_salon >= MAX_PAR_SALON {
            return Err("deux diffusions tournent déjà dans ce salon");
        }
        let id = inner.next_id;
        inner.next_id = inner.next_id.wrapping_add(1).max(1);
        inner.by_id.insert(
            id,
            Live {
                streamer,
                channel,
                key_hex,
                meta,
                seq_start: None,
                viewers: HashMap::new(),
                last_idr_ask: Instant::now() - IDR_COOLDOWN,
                palier: Palier::neuf(meta.kbps),
                couches,
                basse: Basse::eteinte(),
                montant: Montant::neuf(meta.kbps),
                annonce: Budget { haute: meta.kbps, basse: None, montant: false },
            },
        );
        Ok(id)
    }

    /// Met à jour les caractéristiques annoncées ; rend l'identifiant pour la
    /// rediffusion au salon.
    pub fn meta_update(&self, streamer: UserId, meta: StreamMeta) -> Option<u32> {
        let mut inner = self.inner.lock().unwrap();
        let (id, live) = inner
            .by_id
            .iter_mut()
            .find(|(_, l)| l.streamer == streamer)?;
        // Le réglage a changé : le palier ne le dépasse jamais, et repart
        // de là s'il n'y avait pas de contrainte.
        if live.palier.courant >= live.meta.kbps || live.palier.courant > meta.kbps {
            live.palier.courant = meta.kbps;
        }
        if live.montant.palier >= live.meta.kbps || live.montant.palier > meta.kbps {
            live.montant.palier = meta.kbps;
        }
        live.meta = meta;
        Some(*id)
    }

    /// Arrête la diffusion de ce compte (départ, déconnexion, ou volonté).
    /// Rend l'identifiant arrêté, pour l'annonce StreamStopped.
    pub fn stop_by_user(&self, streamer: UserId) -> Option<u32> {
        let mut inner = self.inner.lock().unwrap();
        let id = inner
            .by_id
            .iter()
            .find(|(_, l)| l.streamer == streamer)
            .map(|(id, _)| *id)?;
        if let Some(live) = inner.by_id.remove(&id) {
            for (_, v) in live.viewers {
                v.task.abort();
            }
        }
        Some(id)
    }

    /// Ce compte cesse de regarder ce stream.
    pub fn unwatch(&self, stream_id: u32, user: UserId) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(live) = inner.by_id.get_mut(&stream_id) {
            if let Some(v) = live.viewers.remove(&user) {
                v.task.abort();
            }
        }
    }

    /// Ce compte quitte la scène (salon ou serveur) : plus spectateur de
    /// rien. Sa propre diffusion se règle par `stop_by_user`, à part.
    pub fn drop_viewer_everywhere(&self, user: UserId) {
        let mut inner = self.inner.lock().unwrap();
        for live in inner.by_id.values_mut() {
            if let Some(v) = live.viewers.remove(&user) {
                v.task.abort();
            }
        }
    }

    /// Un spectateur demande à regarder. Vérifie qu'il partage le salon vocal
    /// du streamer, lance sa tâche de diffusion, et rend (clé, meta,
    /// faut-il demander une trame clé, à qui la demander). `couches` : il
    /// sait passer d'une qualité à l'autre. Il commence en haute.
    pub fn watch(
        &self,
        stream_id: u32,
        user: UserId,
        user_channel: Option<ChannelId>,
        conn: quinn::Connection,
        couches: bool,
    ) -> Result<(String, StreamMeta, bool, UserId), &'static str> {
        let mut inner = self.inner.lock().unwrap();
        let live = inner
            .by_id
            .get_mut(&stream_id)
            .ok_or("cette diffusion est terminée")?;
        if live.streamer == user {
            return Err("tu es le streamer : ton aperçu est local");
        }
        if user_channel != Some(live.channel) {
            return Err("il faut être dans le salon vocal du streamer");
        }
        // Re-regarder remplace la tâche : une seule diffusion par spectateur.
        if let Some(v) = live.viewers.remove(&user) {
            v.task.abort();
        }
        let (tx, rx) = tokio::sync::mpsc::channel::<Arc<Trame>>(FILE_VIEWER);
        let needs_idr = Arc::new(AtomicBool::new(true));
        let mesure = Arc::new(Mesure::default());
        let task = tokio::spawn(diffuser(conn.clone(), rx, needs_idr.clone(), mesure.clone()));
        let maintenant = Instant::now();
        live.viewers.insert(
            user,
            Viewer {
                tx,
                needs_idr,
                task,
                mesure,
                conn,
                couches,
                basse: false,
                depuis: maintenant,
                en_essai: false,
                essai_le: maintenant + ESSAI_PREMIER,
                attente_essai: ESSAI_PREMIER,
            },
        );
        let ask = live.last_idr_ask.elapsed() >= IDR_COOLDOWN;
        if ask {
            live.last_idr_ask = Instant::now();
        }
        // Le prochain battement décide tout de suite : si la haute était
        // suspendue (personne ne la regardait), elle doit repartir.
        live.palier.derniere_mesure = Instant::now() - Duration::from_secs(1);
        Ok((live.key_hex.clone(), live.meta, ask, live.streamer))
    }

    /// Un datagramme de son du jeu arrive du streamer : vers chaque
    /// spectateur, tel quel — le serveur ne déchiffre rien, et un
    /// datagramme qui ne part pas (file pleine) est simplement perdu, comme
    /// la voix. `false` si ce compte ne diffuse pas ce stream.
    pub fn relayer_audio(&self, streamer: UserId, stream_id: u32, dat: &bytes::Bytes) -> bool {
        let inner = self.inner.lock().unwrap();
        let Some(live) = inner.by_id.get(&stream_id) else {
            return false;
        };
        if live.streamer != streamer {
            return false;
        }
        for v in live.viewers.values() {
            let _ = v.conn.send_datagram(dat.clone());
        }
        true
    }

    /// Une trame arrive du streamer : validation, comptabilité mémoire, et
    /// distribution à chaque spectateur de SA qualité selon sa file et son
    /// état.
    pub fn ingest(&self, streamer: UserId, header: &MediaHeader, bytes: Vec<u8>) -> Ingest {
        let mut inner = self.inner.lock().unwrap();
        let Some(live) = inner.by_id.get_mut(&header.stream_id) else {
            return Ingest::Refuse;
        };
        if live.streamer != streamer {
            return Ingest::Refuse;
        }
        let basse = header.basse;
        // La basse ne vient que d'un streamer qui a dit la savoir produire.
        if basse && !live.couches {
            return Ingest::Refuse;
        }
        let maintenant_us = live.montant.t0.elapsed().as_micros().min(i64::MAX as u128) as i64;
        live.montant.observer(basse, header.seq, header.pts_us, bytes.len(), maintenant_us);
        let seq_start = if basse {
            live.basse.derniere_trame = Some(Instant::now());
            *live.basse.seq_start.get_or_insert(header.seq)
        } else {
            *live.seq_start.get_or_insert(header.seq)
        };

        // Le plafond mémoire d'abord : un relais qui gonfle emporte le
        // serveur entier, voix comprise. Une trame refusée ici laisse les
        // files se vider ; les spectateurs repartiront d'une trame clé.
        let taille = bytes.len();
        if self.mem.load(Ordering::Relaxed).saturating_add(taille) > MEM_MAX {
            for v in live.viewers.values() {
                v.needs_idr.store(true, Ordering::Relaxed);
            }
            let mut issue = mesurer(live);
            finir(live, &mut issue);
            return Ingest::Ok { ask_haute: issue.ask_haute, ask_basse: issue.ask_basse, budget: issue.budget };
        }
        self.mem.fetch_add(taille, Ordering::Relaxed);
        let trame = Arc::new(Trame {
            bytes,
            idr: header.idr,
            priorite: priorite(header.seq, seq_start),
            mem: self.mem.clone(),
        });

        let mut ask = false;
        let mut partis: Vec<UserId> = Vec::new();
        for (user, v) in live.viewers.iter() {
            // Chacun sa qualité.
            if v.basse != basse {
                continue;
            }
            // En attente de trame clé : les P d'ici là ne seraient que de la
            // bouillie de macroblocs — on ne les envoie pas.
            if v.needs_idr.load(Ordering::Relaxed) {
                if !header.idr {
                    ask = true;
                    continue;
                }
                v.needs_idr.store(false, Ordering::Relaxed);
            }
            match v.tx.try_send(trame.clone()) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    // Spectateur lent : SA trame est jetée, il repartira
                    // d'une trame clé — les autres n'ont rien vu.
                    v.needs_idr.store(true, Ordering::Relaxed);
                    v.mesure.saturations.fetch_add(1, Ordering::Relaxed);
                    ask = true;
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    partis.push(*user);
                }
            }
        }
        for user in partis {
            if let Some(v) = live.viewers.remove(&user) {
                v.task.abort();
            }
        }
        let mut issue = mesurer(live);
        if basse {
            issue.ask_basse |= ask;
        } else {
            issue.ask_haute |= ask;
        }
        finir(live, &mut issue);
        Ingest::Ok { ask_haute: issue.ask_haute, ask_basse: issue.ask_basse, budget: issue.budget }
    }

    /// Le battement : les décisions de chaque diffusion, même quand plus
    /// rien n'arrive du streamer — une basse demandée qui ne vient pas, une
    /// haute suspendue qu'un spectateur réclame. Rend, pour chaque streamer
    /// qui doit l'entendre, ce qu'il faut lui dire.
    pub fn battre(&self) -> Vec<Consigne> {
        let mut inner = self.inner.lock().unwrap();
        let mut consignes = Vec::new();
        for (stream_id, live) in inner.by_id.iter_mut() {
            let mut issue = mesurer(live);
            finir(live, &mut issue);
            if issue.ask_haute || issue.ask_basse || issue.budget.is_some() {
                consignes.push(Consigne {
                    streamer: live.streamer,
                    stream_id: *stream_id,
                    ask_haute: issue.ask_haute,
                    ask_basse: issue.ask_basse,
                    budget: issue.budget,
                });
            }
        }
        consignes
    }
}

/// Ce qu'il faut dire à un streamer : des trames clés sur l'une ou l'autre
/// qualité, et son budget s'il a changé.
pub struct Consigne {
    pub streamer: UserId,
    pub stream_id: u32,
    pub ask_haute: bool,
    pub ask_basse: bool,
    pub budget: Option<Budget>,
}

/// Les messages d'une consigne — le budget d'abord : une haute qui repart
/// doit être rallumée avant qu'on lui demande sa trame clé.
pub fn messages(stream_id: u32, ask_haute: bool, ask_basse: bool, budget: Option<Budget>) -> Vec<ki_protocol::ServerMsg> {
    let mut out = Vec::new();
    if let Some(b) = budget {
        out.push(ki_protocol::ServerMsg::StreamBudget {
            stream_id,
            kbps: b.haute,
            basse: b.basse,
            montant: b.montant,
        });
    }
    if ask_haute {
        out.push(ki_protocol::ServerMsg::KeyframeNeeded { stream_id, basse: false });
    }
    if ask_basse {
        out.push(ki_protocol::ServerMsg::KeyframeNeeded { stream_id, basse: true });
    }
    out
}

/// Le battement des diffusions, quatre fois par seconde tant que le serveur
/// tourne : chaque consigne part à son streamer.
pub async fn boucle(state: Arc<crate::state::AppState>) {
    let mut tic = tokio::time::interval(Duration::from_millis(250));
    tic.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tic.tick().await;
        for c in state.streams.battre() {
            for msg in messages(c.stream_id, c.ask_haute, c.ask_basse, c.budget) {
                state.send_to(c.streamer, &msg);
            }
        }
    }
}

/// Les trames clés à demander : celles qu'on a décidé de demander, plus
/// celles qu'attendent des spectateurs — au plus une par demi-seconde et
/// par qualité, et jamais pour une basse éteinte.
fn finir(live: &mut Live, issue: &mut Issue) {
    let attend_haute = live.viewers.values().any(|v| !v.basse && v.needs_idr.load(Ordering::Relaxed));
    let attend_basse = live.viewers.values().any(|v| v.basse && v.needs_idr.load(Ordering::Relaxed));
    issue.ask_haute = (issue.ask_haute || attend_haute) && demander_idr(&mut live.last_idr_ask);
    issue.ask_basse = (issue.ask_basse || attend_basse)
        && live.basse.kbps.is_some()
        && demander_idr(&mut live.basse.last_idr_ask);
}

/// Une demande de trame clé, au plus une par demi-seconde et par qualité :
/// vrai si elle part maintenant.
fn demander_idr(derniere: &mut Instant) -> bool {
    if derniere.elapsed() >= IDR_COOLDOWN {
        *derniere = Instant::now();
        true
    } else {
        false
    }
}

/// Priorité d'une trame : la même base pour tout le monde, moins l'ancienneté
/// — le plus ancien d'abord DANS un stream, round-robin naturel ENTRE
/// streams. Arithmétique saturante : le cast naïf s'inverserait à 2³¹ trames.
fn priorite(seq: u64, seq_start: u64) -> i32 {
    let age = seq.saturating_sub(seq_start);
    0i32.saturating_sub(age.min(i32::MAX as u64) as i32)
}

/// Trames d'un spectateur encore en vol (écrites, pas forcément arrivées)
/// au-delà desquelles on annule les plus anciennes : trois secondes à
/// 30 i/s, le temps qu'un lien lent se rattrape ou qu'une trame clé passe.
const EN_VOL_MAX: usize = 90;
/// Une écriture qui n'aboutit pas dans ce délai, c'est un tampon d'envoi
/// plein depuis trop longtemps : le lien du spectateur ne suit pas.
const ECRITURE_MAX: Duration = Duration::from_millis(400);

/// La tâche d'un spectateur : chaque trame part dans SON flux QUIC
/// unidirectionnel — fiabilité par trame, sans blocage de tête de ligne
/// entre trames. Une erreur d'écriture termine la tâche ; l'ingestion
/// constatera la file fermée et retirera le spectateur.
///
/// Le retard ne s'accumule pas : une trame clé annule (RESET_STREAM) toutes
/// celles d'avant encore en vol — le spectateur saute à la trame clé au lieu
/// de rattraper des images périmées, et c'est vrai aussi d'un changement de
/// qualité, qui commence toujours par une trame clé —, et une écriture qui
/// bloque trop longtemps annule sa trame et remet le spectateur en attente
/// de trame clé. C'est ce qui manquait quand un spectateur au lien trop
/// court prenait dix secondes de vidéo en retard, la voix de tout le salon
/// faisant la queue derrière.
async fn diffuser(
    conn: quinn::Connection,
    mut rx: tokio::sync::mpsc::Receiver<Arc<Trame>>,
    needs_idr: Arc<AtomicBool>,
    mesure: Arc<Mesure>,
) {
    let mut en_vol: VecDeque<quinn::SendStream> = VecDeque::new();
    while let Some(trame) = rx.recv().await {
        if trame.idr {
            for mut vieux in en_vol.drain(..) {
                // Déjà arrivée : l'annulation est refusée, sans conséquence.
                let _ = vieux.reset(quinn::VarInt::from_u32(0));
            }
        }
        let Ok(mut flux) = conn.open_uni().await else {
            return;
        };
        let _ = flux.set_priority(trame.priorite);
        match tokio::time::timeout(ECRITURE_MAX, flux.write_all(&trame.bytes)).await {
            Ok(Ok(())) => {
                mesure.octets.fetch_add(trame.bytes.len() as u64, Ordering::Relaxed);
            }
            Ok(Err(_)) => return,
            Err(_) => {
                let _ = flux.reset(quinn::VarInt::from_u32(0));
                needs_idr.store(true, Ordering::Relaxed);
                mesure.saturations.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        }
        let _ = flux.finish();
        en_vol.push_back(flux);
        while en_vol.len() > EN_VOL_MAX {
            if let Some(mut vieux) = en_vol.pop_front() {
                let _ = vieux.reset(quinn::VarInt::from_u32(0));
                mesure.saturations.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> StreamMeta {
        StreamMeta {
            width: 1920,
            height: 1080,
            fps: 30,
            kbps: 6000,
            ..Default::default()
        }
    }

    #[test]
    fn le_palier_descend_sous_le_spectateur_sature_et_remonte_cran_par_cran() {
        let a = |kbps, sature| Avale { kbps, sature };
        let calme = Duration::from_secs(0);
        let cinq = Duration::from_secs(5);
        // Personne ne sature : rien ne bouge avant cinq secondes.
        assert_eq!(prochain_palier(8000, 8000, &[a(7900, false)], calme), None);
        // Un spectateur sature en avalant 3000 kbit/s : 0,9 × 3000 = 2700,
        // le palier juste en dessous est 2500 — tout de suite.
        assert_eq!(prochain_palier(8000, 8000, &[a(3000, true), a(7900, false)], calme), Some(2500));
        // Déjà en dessous : on ne descend pas pour rien.
        assert_eq!(prochain_palier(1500, 8000, &[a(3000, true)], calme), None);
        // Un lien mort : le plancher, plus bas qu'avant 0.1.46.
        assert_eq!(prochain_palier(8000, 8000, &[a(0, true)], calme), Some(450));
        // Après cinq secondes sans saturation : un cran, pas plus.
        assert_eq!(prochain_palier(2500, 8000, &[a(2400, false)], cinq), Some(4000));
        assert_eq!(prochain_palier(4000, 8000, &[a(3900, false)], cinq), Some(6000));
        assert_eq!(prochain_palier(6000, 8000, &[a(5900, false)], cinq), Some(8000));
        assert_eq!(prochain_palier(8000, 8000, &[a(7900, false)], cinq), None);
        // À partir de quatre spectateurs, le plus lent seul ne compte pas.
        let quatre = [a(500, true), a(7900, false), a(7900, false), a(7900, false)];
        assert_eq!(prochain_palier(8000, 8000, &quatre, calme), None);
        let deux_lents = [a(500, true), a(3000, true), a(7900, false), a(7900, false)];
        assert_eq!(prochain_palier(8000, 8000, &deux_lents, calme), Some(2500));
        // Le plafond du streamer : jamais dépassé, et un plafond bas n'a
        // que lui et les paliers en dessous.
        assert_eq!(paliers(3000), vec![3000, 2500, 1500, 1000, 700, 450]);
        assert_eq!(prochain_palier(2500, 3000, &[a(2400, false)], cinq), Some(3000));
        // Sans réglage connu (client d'avant), pas de palier.
        assert_eq!(prochain_palier(0, 0, &[a(0, true)], calme), None);
    }

    /// Qui sature sous le plancher passe en basse au lieu de faire
    /// descendre tout le monde ; qui sature au-dessus fait descendre la
    /// haute (le palier commun), comme avant ; qui ne sait pas changer de
    /// qualité reste en haute. En basse, on retente la haute quand on ne
    /// sature pas et que l'essai est permis.
    #[test]
    fn un_spectateur_lent_passe_en_basse_au_lieu_de_tirer_tout_le_monde() {
        let a = |kbps, sature| Avale { kbps, sature };
        let plancher = plancher_haute(8000);
        assert_eq!(plancher, 4000, "la moitié du réglage");
        assert_eq!(plancher_haute(4000), 2500, "jamais sous 2500");
        // 2 Mbit/s sur une haute à 8 : en basse.
        assert_eq!(placer(a(2000, true), true, false, plancher, false), Some(Mouvement::VersBasse));
        // 6 Mbit/s : il tient 5400, au-dessus du plancher — la haute
        // descendra pour lui (palier commun), il reste.
        assert_eq!(placer(a(6000, true), true, false, plancher, false), None);
        // Il ne sature pas : il reste.
        assert_eq!(placer(a(2000, false), true, false, plancher, false), None);
        // Un client d'avant : jamais déplacé.
        assert_eq!(placer(a(500, true), false, false, plancher, true), None);
        // En basse : essai permis et pas de saturation → la haute.
        assert_eq!(placer(a(1400, false), true, true, plancher, true), Some(Mouvement::VersHaute));
        assert_eq!(placer(a(1400, false), true, true, plancher, false), None, "pas encore l'heure");
        assert_eq!(placer(a(900, true), true, true, plancher, true), None, "il sature même en basse");
    }

    /// La basse démarre sous ce qu'avalait le spectateur, jamais au-dessus
    /// de la haute ; son échelle a son plancher.
    #[test]
    fn la_basse_part_sous_le_spectateur_et_sous_la_haute() {
        assert_eq!(max_basse(8000), 2500);
        assert_eq!(max_basse(2500), 1500, "strictement sous la haute");
        assert_eq!(max_basse(400), 450, "le plancher de la basse");
        assert_eq!(palier_initial_basse(2000, 2500), 1500, "0,9 × 2000 = 1800 → 1500");
        assert_eq!(palier_initial_basse(10_000, 2500), 2500, "jamais au-dessus du maximum");
        assert_eq!(palier_initial_basse(600, 2500), 450, "0,9 × 600 = 540 → 450");
        assert_eq!(palier_initial_basse(0, 2500), 450, "un lien mort : le plancher");
        // Et elle suit ses spectateurs comme la haute suit les siens.
        let a = |kbps, sature| Avale { kbps, sature };
        let echelle: Vec<u32> = PALIERS_BASSE.to_vec();
        assert_eq!(prochain_palier_sur(&echelle, 1500, &[a(900, true)], Duration::ZERO), Some(700));
        assert_eq!(prochain_palier_sur(&echelle, 700, &[a(650, false)], Duration::from_secs(5)), Some(1000));
    }

    /// La liaison montante du streamer : saturée, le palier descend sous ce
    /// qui arrive (moins la basse) ; calme dix secondes, il remonte d'un
    /// cran ; jamais au-dessus du réglage.
    #[test]
    fn le_palier_montant_suit_ce_qui_arrive_du_streamer() {
        let dix = Duration::from_secs(10);
        // 3000 kbit/s arrivent quand il en faudrait 8000 : 0,85 × 3000 = 2550 → 2500.
        assert_eq!(prochain_montant(8000, 8000, 3000, true, 0, Duration::ZERO), Some(2500));
        // La basse passe par le même lien : elle se retranche.
        assert_eq!(prochain_montant(8000, 8000, 3000, true, 1000, Duration::ZERO), Some(1500));
        // Déjà en dessous : rien.
        assert_eq!(prochain_montant(1500, 8000, 3000, true, 0, Duration::ZERO), None);
        // Calme : un cran après dix secondes, pas avant.
        assert_eq!(prochain_montant(2500, 8000, 2400, false, 0, Duration::from_secs(9)), None);
        assert_eq!(prochain_montant(2500, 8000, 2400, false, 0, dix), Some(4000));
        assert_eq!(prochain_montant(8000, 8000, 7000, false, 0, dix), None, "au réglage, rien");
        // Un réglage baissé depuis : le palier ne le dépasse pas.
        assert_eq!(prochain_montant(8000, 6000, 5000, false, 0, dix), None);
        assert_eq!(prochain_montant(0, 0, 0, true, 0, dix), None, "sans réglage connu, rien");
    }

    /// Ce qui arrive du streamer se lit : des trames qui manquent (pas
    /// celles qui arrivent dans le désordre), et un retard qui monte de
    /// plus de 200 ms au-dessus de son plancher deux secondes de suite.
    #[test]
    fn la_liaison_montante_se_lit_aux_trous_et_au_retard() {
        let mut m = Montant::neuf(8000);
        // Une seconde propre : 30 trames de 20 Ko, 50 ms de retard, dans
        // le désordre (la 5 avant la 4).
        let mut seqs: Vec<u64> = (0..30).collect();
        seqs.swap(4, 5);
        for (i, s) in seqs.iter().enumerate() {
            m.observer(false, *s, (i as u64) * 33_000, 20_000, (i as i64) * 33_000 + 50_000);
        }
        let (kbps, sature) = m.bilan(1.0);
        assert_eq!(kbps, 4800, "30 × 20 Ko en une seconde");
        assert!(!sature, "le désordre n'est pas une perte");
        // Des trames manquent : saturée tout de suite.
        m.observer(false, 30, 1_000_000, 20_000, 1_050_000);
        m.observer(false, 33, 1_100_000, 20_000, 1_150_000);
        assert!(m.bilan(1.0).1, "deux trames manquent");
        // Le retard monte de 300 ms : saturée à la deuxième seconde.
        m.observer(false, 34, 2_000_000, 20_000, 2_350_000);
        assert!(!m.bilan(1.0).1, "une seconde ne suffit pas");
        m.observer(false, 35, 3_000_000, 20_000, 3_350_000);
        assert!(m.bilan(1.0).1, "deux secondes de retard");
        // Il redescend : la saturation cesse.
        m.observer(false, 36, 4_000_000, 20_000, 4_060_000);
        assert!(!m.bilan(1.0).1);
        // La basse a sa propre séquence : ses numéros ne sont pas des trous.
        m.observer(true, 0, 5_000_000, 5_000, 5_050_000);
        m.observer(true, 1, 5_033_000, 5_000, 5_083_000);
        m.observer(false, 37, 5_000_000, 20_000, 5_050_000);
        assert!(!m.bilan(1.0).1);
    }

    #[test]
    fn un_stream_par_compte_et_deux_par_salon() {
        let s = Streams::new();
        let a = s.start(1, 10, "k1".into(), meta(), true).unwrap();
        // Idempotent : le même compte retrouve SON stream.
        assert_eq!(s.start(1, 10, "k1bis".into(), meta(), true).unwrap(), a);
        let _b = s.start(2, 10, "k2".into(), meta(), false).unwrap();
        // Troisième diffusion du salon : refusée.
        assert!(s.start(3, 10, "k3".into(), meta(), true).is_err());
        // Mais un autre salon a son propre quota.
        assert!(s.start(3, 11, "k3".into(), meta(), true).is_ok());
        // L'arrêt libère la place.
        assert_eq!(s.stop_by_user(1), Some(a));
        assert!(s.start(4, 10, "k4".into(), meta(), true).is_ok());
        assert_eq!(s.stream_of(1), None);
    }

    /// Une trame de qualité basse d'un streamer qui ne l'a pas annoncée est
    /// refusée ; annoncée, elle est acceptée.
    #[test]
    fn la_basse_ne_vient_que_d_un_streamer_qui_l_a_annoncee() {
        let s = Streams::new();
        let ancien = s.start(1, 10, "k1".into(), meta(), false).unwrap();
        let neuf = s.start(2, 11, "k2".into(), meta(), true).unwrap();
        let h = |stream_id, basse| MediaHeader {
            idr: true,
            basse,
            stream_id,
            seq: 0,
            pts_us: 0,
            group_id: 0,
            width: 640,
            height: 360,
        };
        assert!(matches!(s.ingest(1, &h(ancien, true), vec![0; 100]), Ingest::Refuse));
        assert!(matches!(s.ingest(1, &h(ancien, false), vec![0; 100]), Ingest::Ok { .. }));
        assert!(matches!(s.ingest(2, &h(neuf, true), vec![0; 100]), Ingest::Ok { .. }));
        // Et jamais pour le stream d'un autre.
        assert!(matches!(s.ingest(1, &h(neuf, false), vec![0; 100]), Ingest::Refuse));
    }

    #[test]
    fn la_priorite_decroit_et_sature() {
        assert_eq!(priorite(100, 100), 0);
        assert_eq!(priorite(103, 100), -3);
        // Bien au-delà de 2³¹ trames : pas d'inversion, le plancher tient.
        assert_eq!(priorite(u64::MAX, 0), i32::MIN + 1);
        // Une séquence qui aurait reculé (impossible par contrat, mais un
        // pair hostile écrit ce qu'il veut) ne devient pas prioritaire.
        assert_eq!(priorite(50, 100), 0);
    }

    /// La comptabilité mémoire est portée par le Drop de la trame : quand la
    /// dernière copie part, le compteur redescend — chemin d'erreur compris.
    #[test]
    fn la_memoire_se_rend_au_drop() {
        let mem = Arc::new(AtomicUsize::new(0));
        mem.fetch_add(1000, Ordering::Relaxed);
        let t = Arc::new(Trame {
            bytes: vec![0u8; 1000],
            idr: false,
            priorite: 0,
            mem: mem.clone(),
        });
        let t2 = t.clone();
        drop(t);
        assert_eq!(mem.load(Ordering::Relaxed), 1000, "une copie vit encore");
        drop(t2);
        assert_eq!(mem.load(Ordering::Relaxed), 0);
    }
}
