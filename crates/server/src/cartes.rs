//! La table de cartes d'un salon vocal : « Dernière carte ».
//!
//! Un jeu de défausse à la manière du huit américain. Le serveur arbitre
//! tout : il mélange, tient la pioche, la défausse et les mains, et n'envoie
//! à chacun que la sienne — des autres, seulement le nombre de cartes. Un
//! client qui lirait ses messages n'y trouverait rien de plus que ce que
//! son écran montre.
//!
//! Une table par salon vocal, ouverte par qui y est ; tout le salon la
//! voit, ceux qui s'y assoient jouent. Elle suit le salon, et se ferme quand
//! il se vide.
//!
//! Personne ne laisse de trou : qui quitte le salon en pleine manche, ou
//! abandonne, ou laisse passer trois tours, est remplacé par un bot qui
//! joue sa main. Il retrouve sa place en revenant (sauf abandon) ; et
//! d'ici là, un pote qui arrive peut la prendre, cartes comprises.
//!
//! Les règles de la maison, à ajuster en jouant :
//! - le cumul se renchérit : un +2 sur un +2, un +4 sur tout ;
//! - tomber à une carte sans dire « Dernière carte ! », c'est s'exposer :
//!   le premier qui contre fait piocher deux cartes, jusqu'au coup suivant ;
//! - trente secondes par tour, puis le serveur pioche pour le joueur.
//!
//! [`Table`] ne connaît ni le réseau ni l'horloge : on lui passe l'instant,
//! et ses tests jouent des manches entières sans attendre. Le reste du
//! module la relie aux salons — qui est où, qui reçoit quoi.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ki_protocol::{
    carte_jouable, ActionCartes, Carte, ChannelId, CouleurCarte, EvenementCartes, JoueurCartes, PhaseCartes,
    ServerMsg, TableCartes, UserId, ValeurCarte, CARTES_DU_JEU, CARTES_EN_MAIN, MAX_JOUEURS_CARTES,
    PENALITE_ANNONCE, TOUR_CARTES_S,
};
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};

use crate::state::AppState;

const TOUR: Duration = Duration::from_secs(TOUR_CARTES_S);
/// Ce qu'un bot prend pour jouer : de quoi voir passer son coup.
const BOT_DELAI: Duration = Duration::from_millis(1200);
/// Après une pioche qu'on peut poser, au moins ce temps pour décider.
const APRES_PIOCHE: Duration = Duration::from_secs(10);
/// Tours d'affilée laissés au serveur avant qu'un bot ne prenne la place.
const RETARDS_MAX: u8 = 3;
/// Les derniers événements que chaque vue emporte.
const JOURNAL: usize = 8;

/// Pourquoi un geste est refusé, dit à celui qui l'a fait.
pub type Refus = &'static str;

struct Place {
    /// Qui tient la place — ou la tenait, quand c'est un bot qui joue.
    id: UserId,
    main: Vec<Carte>,
    annonce: bool,
    /// Il a quitté le salon.
    absent: bool,
    /// Tours d'affilée où le serveur a joué pour lui.
    retards: u8,
    /// Le serveur joue cette place, avec sa main.
    bot: bool,
    /// Il a abandonné : revenir dans le salon ne lui rend pas sa place, et
    /// elle se libère à la fin de la manche.
    parti: bool,
}

impl Place {
    fn new(id: UserId) -> Self {
        Self { id, main: Vec::new(), annonce: false, absent: false, retards: 0, bot: false, parti: false }
    }
}

/// Une table et sa partie : les règles, sans réseau ni horloge.
pub struct Table {
    salon: ChannelId,
    hote: UserId,
    phase: PhaseCartes,
    places: Vec<Place>,
    /// Le dessus est à la fin.
    pioche: Vec<Carte>,
    defausse: Vec<Carte>,
    /// La couleur demandée : celle du dessus, ou celle choisie sur un joker.
    couleur: CouleurCarte,
    /// La place du joueur du tour.
    tour: usize,
    horaire: bool,
    cumul: u8,
    /// La carte piochée ce tour-ci, que le joueur peut encore poser.
    piochee: Option<Carte>,
    a_pioche: bool,
    expose: Option<UserId>,
    gagnant: Option<UserId>,
    gagnant_bot: bool,
    /// Les manches gagnées, par personne et non par place : qui reprend la
    /// place d'un autre n'hérite pas de ses victoires.
    victoires: HashMap<UserId, u32>,
    /// Ceux dont un autre a pris la place pendant cette manche : ils
    /// attendent la suivante, même si un bot tient une autre place.
    evinces: HashSet<UserId>,
    manche: u32,
    journal: VecDeque<EvenementCartes>,
    numero: u32,
    echeance: Option<Instant>,
    rng: StdRng,
}

impl Table {
    pub fn new(salon: ChannelId, hote: UserId, rng: StdRng) -> Self {
        let mut table = Self {
            salon,
            hote,
            phase: PhaseCartes::Attente,
            places: vec![Place::new(hote)],
            pioche: Vec::new(),
            defausse: Vec::new(),
            couleur: CouleurCarte::Rouge,
            tour: 0,
            horaire: true,
            cumul: 0,
            piochee: None,
            a_pioche: false,
            expose: None,
            gagnant: None,
            gagnant_bot: false,
            victoires: HashMap::new(),
            evinces: HashSet::new(),
            manche: 0,
            journal: VecDeque::new(),
            numero: 0,
            echeance: None,
            rng,
        };
        table.noter(EvenementCartes::Assis { par: hote });
        table
    }

    fn noter(&mut self, e: EvenementCartes) {
        self.journal.push_back(e);
        if self.journal.len() > JOURNAL {
            self.journal.pop_front();
        }
        self.numero += 1;
    }

    fn place(&self, id: UserId) -> Option<usize> {
        self.places.iter().position(|p| p.id == id)
    }

    pub fn rejoindre(&mut self, id: UserId) -> Result<(), Refus> {
        if self.place(id).is_some() {
            return Ok(());
        }
        if self.phase == PhaseCartes::EnCours {
            return Err("une manche est en cours : prends la place d'un bot, ou attends la suivante");
        }
        if self.places.len() >= MAX_JOUEURS_CARTES {
            return Err("la table est pleine");
        }
        // Une table restée sans joueur revient au premier qui s'assoit.
        if self.places.is_empty() {
            self.hote = id;
        }
        self.places.push(Place::new(id));
        self.noter(EvenementCartes::Assis { par: id });
        Ok(())
    }

    /// Se lever. En pleine manche, c'est abandonner : un bot finit la
    /// manche avec sa main — pas de trou dans le tour, et quelqu'un peut
    /// encore la reprendre.
    pub fn quitter(&mut self, id: UserId, now: Instant) {
        let Some(i) = self.place(id) else { return };
        if self.phase != PhaseCartes::EnCours {
            self.retirer(i, now);
            return;
        }
        let deja_bot = self.places[i].bot;
        self.places[i].parti = true;
        self.places[i].bot = true;
        if !deja_bot {
            self.noter(EvenementCartes::Remplace { par: id });
            if i == self.tour {
                self.echeance = Some(now + BOT_DELAI);
            }
        }
        // L'hôte qui abandonne laisse la distribution à un joueur présent.
        if self.hote == id {
            if let Some(p) = self.places.iter().find(|p| !p.bot && !p.absent) {
                self.hote = p.id;
            }
        }
    }

    /// Une place s'en va. Entre deux manches, c'est tout ; en pleine manche
    /// (le chemin n'est plus pris que par sécurité : un parti y est remplacé
    /// par un bot), ses cartes repartent sous la pioche, le tour passe s'il
    /// était le sien, et le dernier joueur restant gagne.
    fn retirer(&mut self, i: usize, now: Instant) {
        let place = self.places.remove(i);
        if self.expose == Some(place.id) {
            self.expose = None;
        }
        if self.hote == place.id {
            if let Some(p) = self.places.iter().find(|p| !p.bot).or(self.places.first()) {
                self.hote = p.id;
            }
        }
        if self.phase != PhaseCartes::EnCours {
            self.noter(EvenementCartes::Leve { par: place.id });
            return;
        }
        self.noter(EvenementCartes::Sorti { par: place.id });
        self.pioche.splice(0..0, place.main);
        match self.places.len() {
            0 => {
                self.phase = PhaseCartes::Attente;
                self.echeance = None;
            }
            1 => self.gagner(0, now),
            n => {
                if i < self.tour {
                    self.tour -= 1;
                } else if i == self.tour {
                    // Le tour passe à qui suivait : dans le sens horaire, il
                    // a pris la place libérée ; dans l'autre, il la précédait.
                    self.tour = if self.horaire { i % n } else { (i + n - 1) % n };
                    // Le cumul qu'il devait part avec lui : ses cartes
                    // repartent sous la pioche, et le suivant n'a rien
                    // laissé passer.
                    self.cumul = 0;
                    self.debut_de_tour(now);
                }
            }
        }
    }

    pub fn lancer(&mut self, id: UserId, now: Instant) -> Result<(), Refus> {
        if id != self.hote {
            return Err("seul l'hôte distribue");
        }
        if self.phase == PhaseCartes::EnCours {
            return Err("la manche est déjà en cours");
        }
        if self.places.len() < 2 {
            return Err("il faut être deux au moins");
        }
        let mut jeu: Vec<Carte> = (0..CARTES_DU_JEU).map(Carte).collect();
        jeu.shuffle(&mut self.rng);
        for p in &mut self.places {
            p.main = jeu.split_off(jeu.len() - CARTES_EN_MAIN);
            p.annonce = false;
            p.retards = 0;
            p.bot = false;
            p.parti = false;
        }
        // On retourne la première carte qui est un chiffre : une manche ne
        // commence ni par un joker ni par une pénalité.
        let i = jeu
            .iter()
            .rposition(|c| matches!(c.valeur(), ValeurCarte::Chiffre(_)))
            .expect("le jeu a quarante chiffres");
        let premiere = jeu.remove(i);
        self.couleur = premiere.couleur().expect("un chiffre a une couleur");
        self.defausse = vec![premiere];
        self.pioche = jeu;
        self.horaire = true;
        self.cumul = 0;
        self.expose = None;
        self.gagnant = None;
        self.gagnant_bot = false;
        self.evinces.clear();
        self.manche += 1;
        self.phase = PhaseCartes::EnCours;
        self.tour = self.rng.random_range(0..self.places.len());
        self.noter(EvenementCartes::Manche);
        self.debut_de_tour(now);
        Ok(())
    }

    /// Le temps de la place `i` : trente secondes pour un joueur, le temps
    /// de voir passer le coup pour un bot.
    fn delai(&self, i: usize) -> Duration {
        if self.places[i].bot {
            BOT_DELAI
        } else {
            TOUR
        }
    }

    fn debut_de_tour(&mut self, now: Instant) {
        self.piochee = None;
        self.a_pioche = false;
        self.echeance = Some(now + self.delai(self.tour));
    }

    fn passer_la_main(&mut self, pas: usize, now: Instant) {
        // Le tour s'achève : trop tard pour contrer un autre que celui qui
        // vient de jouer — quoi qu'il ait fait, poser, piocher ou passer, et
        // qu'il soit un joueur ou un bot.
        let id = self.places[self.tour].id;
        self.fermer_contre(id);
        let n = self.places.len();
        let pas = pas % n;
        self.tour = if self.horaire { (self.tour + pas) % n } else { (self.tour + n - pas) % n };
        self.debut_de_tour(now);
    }

    /// Ma place, tenue par moi et pas par un bot.
    fn ma_place(&self, id: UserId) -> Result<usize, Refus> {
        let i = self.place(id).ok_or("tu n'es pas assis à la table")?;
        if self.places[i].bot {
            return Err("un bot joue à ta place : reprends-la d'abord");
        }
        Ok(i)
    }

    fn mon_tour(&self, id: UserId) -> Result<usize, Refus> {
        if self.phase != PhaseCartes::EnCours {
            return Err("aucune manche en cours");
        }
        let i = self.ma_place(id)?;
        if i != self.tour {
            return Err("ce n'est pas ton tour");
        }
        Ok(i)
    }

    /// Un coup joué par un autre que l'exposé : trop tard pour le contrer.
    fn fermer_contre(&mut self, par: UserId) {
        if self.expose.is_some_and(|e| e != par) {
            self.expose = None;
        }
    }

    pub fn jouer(
        &mut self,
        id: UserId,
        carte: Carte,
        couleur: Option<CouleurCarte>,
        now: Instant,
    ) -> Result<(), Refus> {
        let i = self.mon_tour(id)?;
        self.poser(i, carte, couleur, now)
    }

    /// La place du tour pose `carte` : les règles, sans demander qui joue —
    /// un joueur, ou le bot qui tient sa place.
    fn poser(&mut self, i: usize, carte: Carte, couleur: Option<CouleurCarte>, now: Instant) -> Result<(), Refus> {
        if self.piochee.is_some_and(|p| p != carte) {
            return Err("après une pioche, seule la carte piochée se pose");
        }
        let pos = self.places[i]
            .main
            .iter()
            .position(|c| *c == carte)
            .ok_or("cette carte n'est pas dans ta main")?;
        let dessus = *self.defausse.last().expect("une manche a sa défausse");
        if !carte_jouable(carte, dessus, self.couleur, self.cumul) {
            return Err(if self.cumul > 0 {
                "renchéris avec un +2 ou un +4, ou pioche"
            } else {
                "cette carte ne se pose pas ici"
            });
        }
        let couleur = match carte.couleur() {
            Some(c) => c,
            None => couleur.ok_or("choisis une couleur")?,
        };
        let id = self.places[i].id;
        self.fermer_contre(id);
        let place = &mut self.places[i];
        place.main.remove(pos);
        place.retards = 0;
        let reste = place.main.len();
        let annonce = place.annonce;
        self.defausse.push(carte);
        self.couleur = couleur;
        self.noter(EvenementCartes::Joue { par: id, carte, couleur: carte.sauvage().then_some(couleur) });
        if reste == 0 {
            self.gagner(i, now);
            return Ok(());
        }
        if reste == 1 && !annonce {
            self.expose = Some(id);
        }
        let pas = match carte.valeur() {
            ValeurCarte::Passe => 2,
            // À deux, inverser le sens revient à sauter l'autre.
            ValeurCarte::Inverse if self.places.len() == 2 => 2,
            ValeurCarte::Inverse => {
                self.horaire = !self.horaire;
                1
            }
            v => {
                self.cumul = self.cumul.saturating_add(v.penalite());
                1
            }
        };
        self.passer_la_main(pas, now);
        Ok(())
    }

    /// La manche est gagnée. Les places que tenait un bot pour quelqu'un de
    /// parti se libèrent ; celles d'un joueur resté là lui reviennent pour
    /// la suivante.
    fn gagner(&mut self, i: usize, now: Instant) {
        let (id, bot) = (self.places[i].id, self.places[i].bot);
        // Une manche gagnée par un bot ne compte pour personne.
        if !bot {
            *self.victoires.entry(id).or_default() += 1;
        }
        self.phase = PhaseCartes::Finie;
        self.gagnant = Some(id);
        self.gagnant_bot = bot;
        self.echeance = None;
        self.expose = None;
        self.cumul = 0;
        self.piochee = None;
        while let Some(j) = self.places.iter().position(|p| p.bot && (p.parti || p.absent)) {
            self.retirer(j, now);
        }
        // Noté après les places libérées : la victoire reste au bout du fil,
        // même quand beaucoup sont partis.
        self.noter(EvenementCartes::Gagne { par: id, bot });
        for p in &mut self.places {
            p.bot = false;
            p.retards = 0;
        }
        // Celui qui distribue est à la table.
        if self.place(self.hote).is_none() {
            if let Some(p) = self.places.first() {
                self.hote = p.id;
            }
        }
    }

    /// Une carte de la pioche ; vide, la défausse — moins son dessus — la
    /// refait. `None` quand tout le jeu est en main.
    fn tirer(&mut self) -> Option<Carte> {
        if self.pioche.is_empty() && self.defausse.len() > 1 {
            let dessus = self.defausse.pop().expect("la défausse n'est pas vide");
            let mut reste = std::mem::replace(&mut self.defausse, vec![dessus]);
            reste.shuffle(&mut self.rng);
            self.pioche = reste;
            self.noter(EvenementCartes::Melange);
        }
        self.pioche.pop()
    }

    /// `n` cartes pour la place `i`, moins si le jeu est épuisé. Revenu à
    /// deux cartes ou plus, son annonce ne vaut plus.
    fn faire_piocher(&mut self, i: usize, n: usize) -> u8 {
        let mut tirees = 0;
        for _ in 0..n {
            let Some(c) = self.tirer() else { break };
            self.places[i].main.push(c);
            tirees += 1;
        }
        if self.places[i].main.len() >= 2 {
            self.places[i].annonce = false;
        }
        tirees
    }

    pub fn piocher(&mut self, id: UserId, now: Instant) -> Result<(), Refus> {
        let i = self.mon_tour(id)?;
        if self.a_pioche {
            return Err("tu as déjà pioché : pose la carte piochée ou passe");
        }
        self.fermer_contre(id);
        self.places[i].retards = 0;
        self.piocher_pour(i, now, true);
        Ok(())
    }

    /// La pioche du joueur du tour : le cumul s'il y en a un, et le tour
    /// passe. Sinon une carte — qu'il peut poser si elle va et qu'il est là
    /// pour le décider (`choix`) ; sinon le tour passe.
    fn piocher_pour(&mut self, i: usize, now: Instant, choix: bool) {
        let id = self.places[i].id;
        // Piocher est un coup : celui d'un bot ferme le contre comme celui
        // d'un joueur.
        self.fermer_contre(id);
        if self.cumul > 0 {
            let n = std::mem::take(&mut self.cumul);
            let tirees = self.faire_piocher(i, n as usize);
            self.noter(EvenementCartes::Pioche { par: id, n: tirees, manque: n.saturating_sub(tirees) });
            self.passer_la_main(1, now);
            return;
        }
        let tirees = self.faire_piocher(i, 1);
        self.noter(EvenementCartes::Pioche { par: id, n: tirees, manque: 1 - tirees });
        let piochee = if tirees == 1 { self.places[i].main.last().copied() } else { None };
        let dessus = *self.defausse.last().expect("une manche a sa défausse");
        match piochee {
            Some(c) if choix && carte_jouable(c, dessus, self.couleur, 0) => {
                self.piochee = Some(c);
                self.a_pioche = true;
                self.echeance = Some(if self.places[i].bot {
                    now + BOT_DELAI
                } else {
                    self.echeance.map_or(now + APRES_PIOCHE, |e| e.max(now + APRES_PIOCHE))
                });
            }
            _ => self.passer_la_main(1, now),
        }
    }

    pub fn passer(&mut self, id: UserId, now: Instant) -> Result<(), Refus> {
        let i = self.mon_tour(id)?;
        if self.piochee.is_none() {
            return Err("pioche d'abord");
        }
        self.places[i].retards = 0;
        self.noter(EvenementCartes::Passe { par: id });
        self.passer_la_main(1, now);
        Ok(())
    }

    pub fn annoncer(&mut self, id: UserId) -> Result<(), Refus> {
        if self.phase != PhaseCartes::EnCours {
            return Err("aucune manche en cours");
        }
        let i = self.ma_place(id)?;
        if self.places[i].main.len() > 2 {
            return Err("ça s'annonce à deux cartes ou moins");
        }
        self.annoncer_en(i);
        Ok(())
    }

    fn annoncer_en(&mut self, i: usize) {
        let place = &mut self.places[i];
        if place.annonce {
            return;
        }
        place.annonce = true;
        let id = place.id;
        if self.expose == Some(id) {
            self.expose = None;
        }
        self.noter(EvenementCartes::Annonce { par: id });
    }

    /// L'exposé pioche deux cartes. Le premier qui contre l'emporte : les
    /// suivants arrivent trop tard, sans reproche.
    pub fn contrer(&mut self, id: UserId) -> Result<(), Refus> {
        self.ma_place(id)?;
        let Some(cible) = self.expose.filter(|c| *c != id) else { return Ok(()) };
        self.expose = None;
        if let Some(j) = self.place(cible) {
            let n = self.faire_piocher(j, PENALITE_ANNONCE);
            self.noter(EvenementCartes::Contre { par: id, cible, n });
        }
        Ok(())
    }

    /// Prendre une place que tient un bot : la sienne (revenu, ou réveillé
    /// après trois tours manqués), ou celle d'un parti — avec sa main telle
    /// quelle.
    pub fn reprendre(&mut self, id: UserId, place: UserId, now: Instant) -> Result<(), Refus> {
        if self.phase != PhaseCartes::EnCours {
            return Err("aucune manche en cours");
        }
        let j = self.place(place).ok_or("cette place n'existe plus")?;
        if !self.places[j].bot {
            return Err("cette place a déjà un joueur");
        }
        if id != place && self.place(id).is_some() {
            return Err("tu as déjà une place à la table");
        }
        if self.evinces.contains(&id) {
            return Err("ta place a été reprise : tu rejoues à la manche suivante");
        }
        let p = &mut self.places[j];
        p.id = id;
        p.bot = false;
        p.parti = false;
        p.absent = false;
        p.retards = 0;
        if self.expose == Some(place) {
            self.expose = Some(id);
        }
        if id != place {
            // L'ancien titulaire ne rejoue qu'à la manche suivante, et ne
            // garde pas la distribution d'une table où il n'est plus assis.
            self.evinces.insert(place);
            if self.hote == place {
                self.hote = id;
            }
        }
        self.noter(EvenementCartes::Reprend { par: id, place });
        // Un tour entier pour qui arrive au moment de jouer.
        if j == self.tour {
            self.echeance = Some(now + TOUR);
        }
        Ok(())
    }

    /// Le coup d'un bot, sans finesse : la première carte de couleur qui
    /// va, un joker en dernier recours avec la couleur dont il a le plus,
    /// sinon la pioche. Il annonce toujours sa dernière carte et ne contre
    /// personne.
    fn jouer_bot(&mut self, now: Instant) {
        let i = self.tour;
        let candidates: Vec<Carte> = match self.piochee {
            Some(c) => vec![c],
            None => {
                let dessus = *self.defausse.last().expect("une manche a sa défausse");
                let mut jouables: Vec<Carte> = self.places[i]
                    .main
                    .iter()
                    .copied()
                    .filter(|&c| carte_jouable(c, dessus, self.couleur, self.cumul))
                    .collect();
                jouables.sort_by_key(|c| c.sauvage());
                jouables
            }
        };
        match candidates.first() {
            Some(&carte) => {
                if self.places[i].main.len() == 2 {
                    self.annoncer_en(i);
                }
                let couleur = couleur_preferee(&self.places[i].main, carte);
                if self.poser(i, carte, Some(couleur), now).is_err() {
                    // Ne devrait pas arriver : la carte a été choisie parmi
                    // celles qui vont. Le tour passe plutôt que de bloquer.
                    let id = self.places[i].id;
                    self.noter(EvenementCartes::Passe { par: id });
                    self.passer_la_main(1, now);
                }
            }
            None if self.piochee.is_some() => {
                let id = self.places[i].id;
                self.noter(EvenementCartes::Passe { par: id });
                self.passer_la_main(1, now);
            }
            None => self.piocher_pour(i, now, true),
        }
    }

    /// L'horloge : le bot qui joue, le tour expiré. Vrai si la table a
    /// changé.
    pub fn tic(&mut self, now: Instant) -> bool {
        if self.phase != PhaseCartes::EnCours || !self.echeance.is_some_and(|e| now >= e) {
            return false;
        }
        let i = self.tour;
        if self.places[i].bot {
            self.jouer_bot(now);
            return true;
        }
        let id = self.places[i].id;
        self.noter(EvenementCartes::Temps { par: id });
        self.places[i].retards += 1;
        if self.piochee.is_some() {
            self.passer_la_main(1, now);
        } else {
            self.fermer_contre(id);
            self.piocher_pour(i, now, false);
        }
        // Trois tours manqués d'affilée : un bot prend le relais, et la
        // place l'attend — un clic sur « Reprendre ma place ».
        if self.phase == PhaseCartes::EnCours && self.places[i].retards >= RETARDS_MAX {
            self.places[i].bot = true;
            self.noter(EvenementCartes::Remplace { par: id });
        }
        true
    }

    /// Qui est dans le salon. En pleine manche, un bot prend aussitôt la
    /// place de qui en sort, et la lui rend s'il revient — sauf s'il avait
    /// abandonné, ou si quelqu'un l'a reprise entre-temps. Entre deux
    /// manches, qui sort se lève. L'hôte parti, la main passe à un joueur
    /// présent. Vrai si la table a changé.
    pub fn accorder(&mut self, presents: &HashSet<UserId>, now: Instant) -> bool {
        let mut change = false;
        if self.phase == PhaseCartes::EnCours {
            let tour_avant = self.places[self.tour].bot;
            let mut evenements = Vec::new();
            for p in &mut self.places {
                match (presents.contains(&p.id), p.absent) {
                    (false, false) => {
                        p.absent = true;
                        if !p.bot {
                            p.bot = true;
                            evenements.push(EvenementCartes::Remplace { par: p.id });
                        }
                        change = true;
                    }
                    (true, true) => {
                        p.absent = false;
                        if p.bot && !p.parti {
                            p.bot = false;
                            p.retards = 0;
                            evenements.push(EvenementCartes::Reprend { par: p.id, place: p.id });
                        }
                        change = true;
                    }
                    _ => {}
                }
            }
            for e in evenements {
                self.noter(e);
            }
            // Le tour en cours change de mains : un bot n'attend pas trente
            // secondes, un joueur revenu a son tour entier.
            match (tour_avant, self.places[self.tour].bot) {
                (false, true) => {
                    let court = now + BOT_DELAI;
                    if self.echeance.is_none_or(|e| e > court) {
                        self.echeance = Some(court);
                    }
                }
                (true, false) => self.echeance = Some(now + TOUR),
                _ => {}
            }
        } else {
            while let Some(i) = self.places.iter().position(|p| !presents.contains(&p.id)) {
                self.retirer(i, now);
                change = true;
            }
        }
        if !presents.contains(&self.hote) {
            if let Some(p) = self.places.iter().find(|p| presents.contains(&p.id) && !p.bot) {
                self.hote = p.id;
                change = true;
            }
        }
        change
    }

    /// La table vue de la place de `pour` : sa main, et des autres le
    /// nombre de cartes seulement.
    pub fn vue(&self, pour: UserId, now: Instant) -> TableCartes {
        let moi = self.place(pour);
        let en_cours = self.phase == PhaseCartes::EnCours;
        let mut main = moi.map(|i| self.places[i].main.clone()).unwrap_or_default();
        main.sort_by_key(|c| c.rang());
        let a_moi = en_cours && moi == Some(self.tour) && !self.places[self.tour].bot;
        TableCartes {
            salon: self.salon,
            hote: self.hote,
            phase: self.phase,
            joueurs: self
                .places
                .iter()
                .map(|p| JoueurCartes {
                    user_id: p.id,
                    cartes: p.main.len().min(u8::MAX as usize) as u8,
                    annonce: p.annonce,
                    absent: p.absent,
                    bot: p.bot,
                    victoires: self.victoires.get(&p.id).copied().unwrap_or(0),
                })
                .collect(),
            main,
            dessus: self.defausse.last().copied(),
            couleur: (!self.defausse.is_empty()).then_some(self.couleur),
            tour: en_cours.then(|| self.places[self.tour].id),
            horaire: self.horaire,
            cumul: self.cumul,
            pioche: self.pioche.len().min(u8::MAX as usize) as u8,
            reste_ms: self
                .echeance
                .map(|e| e.saturating_duration_since(now).as_millis().min(u32::MAX as u128) as u32)
                .unwrap_or(0),
            piochee: if a_moi { self.piochee } else { None },
            expose: self.expose,
            gagnant: self.gagnant,
            gagnant_bot: self.gagnant_bot,
            en_attente: en_cours && self.evinces.contains(&pour) && moi.is_none(),
            manche: self.manche,
            journal: self.journal.iter().cloned().collect(),
            numero: self.numero,
        }
    }
}

/// La couleur dont `main` a le plus, sans compter la carte qu'on pose.
fn couleur_preferee(main: &[Carte], posee: Carte) -> CouleurCarte {
    CouleurCarte::TOUTES
        .into_iter()
        .max_by_key(|c| main.iter().filter(|x| **x != posee && x.couleur() == Some(*c)).count())
        .unwrap_or(CouleurCarte::Rouge)
}

// --- Les tables des salons ---

/// Une table ouverte, et ceux qui en ont reçu la vue — à qui dire qu'elle
/// n'est plus là quand ils partent.
struct Salle {
    table: Table,
    vus: HashSet<UserId>,
}

/// Des messages préparés sous le verrou des tables, envoyés après : on ne
/// tient jamais celui des connectés en même temps.
type Envois = Vec<(UserId, ServerMsg)>;

impl Salle {
    /// La vue de chaque présent — de tous, ou des nouveaux venus seulement —
    /// et `None` à qui n'est plus là.
    fn diffuser(&mut self, presents: &HashSet<UserId>, now: Instant, tous: bool, envois: &mut Envois) {
        for &id in presents {
            if tous || !self.vus.contains(&id) {
                envois.push((id, ServerMsg::Cartes { table: Some(self.table.vue(id, now)) }));
            }
        }
        for &id in self.vus.difference(presents) {
            envois.push((id, ServerMsg::Cartes { table: None }));
        }
        self.vus = presents.clone();
    }
}

/// Les tables, une par salon vocal.
#[derive(Default)]
pub struct Cartes {
    tables: Mutex<HashMap<ChannelId, Salle>>,
}

/// Qui est dans quel salon vocal.
fn occupants(state: &AppState) -> HashMap<ChannelId, HashSet<UserId>> {
    let users = state.users.lock().unwrap();
    let mut salons: HashMap<ChannelId, HashSet<UserId>> = HashMap::new();
    for (id, u) in users.iter() {
        if let Some(c) = u.voice {
            salons.entry(c).or_default().insert(*id);
        }
    }
    salons
}

/// Les « plus de table » d'abord : qui passe d'un salon à table à un autre
/// doit recevoir la fin de l'ancienne avant la nouvelle, sinon le `None`
/// arrivé second effaçait la table où il vient d'entrer.
fn envoyer(state: &AppState, mut envois: Envois) {
    envois.sort_by_key(|(_, msg)| !matches!(msg, ServerMsg::Cartes { table: None }));
    for (id, msg) in envois {
        state.send_to(id, &msg);
    }
}

/// Un geste à la table du salon vocal de `user_id`.
pub fn agir(state: &AppState, user_id: UserId, action: ActionCartes) -> Result<(), Refus> {
    let salon = {
        let users = state.users.lock().unwrap();
        users.get(&user_id).and_then(|u| u.voice)
    }
    .ok_or("rejoins un salon vocal pour jouer aux cartes")?;
    let presents = occupants(state).remove(&salon).unwrap_or_default();
    let now = Instant::now();
    let mut envois = Vec::new();
    {
        let mut tables = state.cartes.tables.lock().unwrap();
        let salle = match tables.entry(salon) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                if action != ActionCartes::Ouvrir {
                    return Err("personne n'a ouvert de table dans ton salon");
                }
                tracing::info!("cartes : table ouverte dans le salon {salon}");
                e.insert(Salle { table: Table::new(salon, user_id, StdRng::from_os_rng()), vus: HashSet::new() })
            }
        };
        let t = &mut salle.table;
        match action {
            ActionCartes::Ouvrir | ActionCartes::Rejoindre => t.rejoindre(user_id)?,
            ActionCartes::Quitter => t.quitter(user_id, now),
            ActionCartes::Lancer => t.lancer(user_id, now)?,
            ActionCartes::Jouer { carte, couleur } => t.jouer(user_id, carte, couleur, now)?,
            ActionCartes::Piocher => t.piocher(user_id, now)?,
            ActionCartes::Passer => t.passer(user_id, now)?,
            ActionCartes::Annoncer => t.annoncer(user_id)?,
            ActionCartes::Contrer => t.contrer(user_id)?,
            ActionCartes::Reprendre { place } => t.reprendre(user_id, place, now)?,
        }
        salle.diffuser(&presents, now, true, &mut envois);
    }
    envoyer(state, envois);
    Ok(())
}

/// Qui est où a peut-être changé : appelé à chaque reconstruction des
/// routes vocales, où passent toutes les entrées, sorties, déplacements et
/// déconnexions. Le nouveau venu reçoit la table, le partant apprend
/// qu'elle n'est plus là pour lui, et un salon vide ferme la sienne.
pub fn accorder(state: &AppState) {
    let mut salons = occupants(state);
    let now = Instant::now();
    let mut envois = Vec::new();
    {
        let mut tables = state.cartes.tables.lock().unwrap();
        tables.retain(|salon, salle| {
            let presents = salons.remove(salon).unwrap_or_default();
            if presents.is_empty() {
                tracing::info!("cartes : le salon {salon} s'est vidé, la table se ferme");
                for id in salle.vus.drain() {
                    envois.push((id, ServerMsg::Cartes { table: None }));
                }
                return false;
            }
            let change = salle.table.accorder(&presents, now);
            salle.diffuser(&presents, now, change, &mut envois);
            true
        });
    }
    envoyer(state, envois);
}

/// L'horloge des tables, quatre fois par seconde.
pub async fn boucle(state: Arc<AppState>) {
    let mut tic = tokio::time::interval(Duration::from_millis(250));
    tic.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tic.tick().await;
        let now = Instant::now();
        let mut envois = Vec::new();
        {
            let mut tables = state.cartes.tables.lock().unwrap();
            for salle in tables.values_mut() {
                if salle.table.tic(now) {
                    let presents = salle.vus.clone();
                    salle.diffuser(&presents, now, true, &mut envois);
                }
            }
        }
        envoyer(&state, envois);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: UserId = 1;
    const B: UserId = 2;
    const C: UserId = 3;
    const D: UserId = 4;

    fn table(joueurs: &[UserId], graine: u64) -> Table {
        let mut t = Table::new(7, joueurs[0], StdRng::seed_from_u64(graine));
        for &j in &joueurs[1..] {
            t.rejoindre(j).unwrap();
        }
        t
    }

    /// Une manche posée à la main : ces mains-là, ce dessus, au tour de la
    /// première place. Le reste du jeu fait la pioche.
    fn manche(joueurs: &[(UserId, &[u8])], dessus: u8, now: Instant) -> Table {
        let ids: Vec<UserId> = joueurs.iter().map(|(id, _)| *id).collect();
        let mut t = table(&ids, 1);
        t.lancer(ids[0], now).unwrap();
        let mut pris: Vec<u8> = vec![dessus];
        for (i, (_, main)) in joueurs.iter().enumerate() {
            t.places[i].main = main.iter().map(|&n| Carte(n)).collect();
            pris.extend_from_slice(main);
        }
        t.pioche = (0..CARTES_DU_JEU).filter(|n| !pris.contains(n)).map(Carte).collect();
        t.defausse = vec![Carte(dessus)];
        t.couleur = Carte(dessus).couleur().unwrap();
        t.tour = 0;
        t.debut_de_tour(now);
        t
    }

    fn total(t: &Table) -> usize {
        t.pioche.len() + t.defausse.len() + t.places.iter().map(|p| p.main.len()).sum::<usize>()
    }

    fn main_de(t: &Table, id: UserId) -> usize {
        t.places[t.place(id).unwrap()].main.len()
    }

    #[test]
    fn la_distribution() {
        let now = Instant::now();
        let mut t = table(&[A, B, C], 3);
        assert_eq!(t.lancer(B, now), Err("seul l'hôte distribue"));
        t.lancer(A, now).unwrap();
        assert!(t.places.iter().all(|p| p.main.len() == CARTES_EN_MAIN));
        assert!(matches!(t.defausse[0].valeur(), ValeurCarte::Chiffre(_)));
        assert_eq!(total(&t), CARTES_DU_JEU as usize);
        assert_eq!(t.rejoindre(4), Err("une manche est en cours : prends la place d'un bot, ou attends la suivante"));
        // Chacun ne voit que sa main.
        let vue = t.vue(B, now);
        assert_eq!(vue.main.len(), CARTES_EN_MAIN);
        assert!(vue.main.iter().all(|c| t.places[1].main.contains(c)));
        assert!(t.vue(99, now).main.is_empty());
    }

    #[test]
    fn seul_on_ne_joue_pas() {
        let mut t = table(&[A], 1);
        assert_eq!(t.lancer(A, Instant::now()), Err("il faut être deux au moins"));
    }

    #[test]
    fn passe_et_inverse() {
        let now = Instant::now();
        // 13 = 7 rouge ; 19 = passe rouge ; 21 = inverse rouge.
        let mut t = manche(&[(A, &[19, 2, 3]), (B, &[4, 6]), (C, &[8, 10])], 13, now);
        t.jouer(A, Carte(19), None, now).unwrap();
        assert_eq!(t.places[t.tour].id, C, "B est sauté");
        let mut t = manche(&[(A, &[21, 2, 3]), (B, &[4, 6]), (C, &[8, 10])], 13, now);
        t.jouer(A, Carte(21), None, now).unwrap();
        assert!(!t.horaire);
        assert_eq!(t.places[t.tour].id, C, "le sens s'inverse");
        // À deux, l'inverse rejoue.
        let mut t = manche(&[(A, &[21, 2, 3]), (B, &[4, 6])], 13, now);
        t.jouer(A, Carte(21), None, now).unwrap();
        assert_eq!(t.places[t.tour].id, A);
    }

    #[test]
    fn le_cumul_se_rencherit_puis_se_paie() {
        let now = Instant::now();
        // 23 = +2 rouge ; 73 = +2 vert ; 104 = +4.
        let mut t = manche(&[(A, &[23, 2]), (B, &[73, 4]), (C, &[104, 6, 8])], 13, now);
        t.jouer(A, Carte(23), None, now).unwrap();
        assert_eq!(t.jouer(B, Carte(4), None, now), Err("renchéris avec un +2 ou un +4, ou pioche"));
        t.jouer(B, Carte(73), None, now).unwrap();
        t.jouer(C, Carte(104), Some(CouleurCarte::Bleu), now).unwrap();
        assert_eq!(t.cumul, 8);
        assert_eq!(t.couleur, CouleurCarte::Bleu);
        t.piocher(A, now).unwrap();
        assert_eq!(main_de(&t, A), 1 + 8);
        assert_eq!(t.cumul, 0);
        assert_eq!(t.places[t.tour].id, B, "le cumul payé, le tour passe");
    }

    #[test]
    fn le_joker_demande_une_couleur() {
        let now = Instant::now();
        let mut t = manche(&[(A, &[100, 2]), (B, &[4, 6])], 13, now);
        assert_eq!(t.jouer(A, Carte(100), None, now), Err("choisis une couleur"));
        t.jouer(A, Carte(100), Some(CouleurCarte::Vert), now).unwrap();
        assert_eq!(t.vue(B, now).couleur, Some(CouleurCarte::Vert));
    }

    #[test]
    fn la_pioche_jouable_se_pose_ou_se_garde() {
        let now = Instant::now();
        // B n'a rien de rouge ni de 7 ; la pioche lui donne un 3 rouge.
        let mut t = manche(&[(B, &[54, 56]), (A, &[2, 3])], 13, now);
        t.pioche.retain(|c| *c != Carte(5));
        t.pioche.push(Carte(5));
        t.piocher(B, now).unwrap();
        assert_eq!(t.vue(B, now).piochee, Some(Carte(5)));
        assert_eq!(t.piocher(B, now), Err("tu as déjà pioché : pose la carte piochée ou passe"));
        assert_eq!(t.jouer(B, Carte(54), None, now), Err("après une pioche, seule la carte piochée se pose"));
        t.passer(B, now).unwrap();
        assert_eq!(t.places[t.tour].id, A);
        // Une pioche qui ne va pas : le tour passe tout seul.
        let mut t = manche(&[(B, &[54, 56]), (A, &[2, 3])], 13, now);
        t.pioche.retain(|c| *c != Carte(60));
        t.pioche.push(Carte(60));
        t.piocher(B, now).unwrap();
        assert_eq!(t.places[t.tour].id, A);
    }

    #[test]
    fn derniere_carte_contre_et_annonce() {
        let now = Instant::now();
        let mut t = manche(&[(A, &[2, 3]), (B, &[4, 6, 8])], 13, now);
        t.jouer(A, Carte(2), None, now).unwrap();
        assert_eq!(t.expose, Some(A), "à une carte sans l'avoir dit");
        t.contrer(B).unwrap();
        assert_eq!(main_de(&t, A), 1 + PENALITE_ANNONCE);
        assert_eq!(t.expose, None);
        // Contrer à nouveau ne coûte rien à personne.
        t.contrer(B).unwrap();
        assert_eq!(main_de(&t, A), 3);

        // Annoncé avant de poser : rien à contrer.
        let mut t = manche(&[(A, &[2, 3]), (B, &[4, 6, 8])], 13, now);
        t.annoncer(A).unwrap();
        t.jouer(A, Carte(2), None, now).unwrap();
        assert_eq!(t.expose, None);
        // Annoncé après coup, avant d'être contré : sauvé aussi.
        let mut t = manche(&[(A, &[2, 3]), (B, &[4, 6, 8])], 13, now);
        t.jouer(A, Carte(2), None, now).unwrap();
        t.annoncer(A).unwrap();
        t.contrer(B).unwrap();
        assert_eq!(main_de(&t, A), 1);
        // Le coup suivant ferme la fenêtre.
        let mut t = manche(&[(A, &[2, 3]), (B, &[4, 6, 8])], 13, now);
        t.jouer(A, Carte(2), None, now).unwrap();
        t.piocher(B, now).unwrap();
        assert_eq!(t.expose, None);
        assert_eq!(t.annoncer(B), Err("ça s'annonce à deux cartes ou moins"));
    }

    #[test]
    fn la_derniere_carte_gagne() {
        let now = Instant::now();
        let mut t = manche(&[(A, &[2]), (B, &[4, 6])], 13, now);
        t.jouer(A, Carte(2), None, now).unwrap();
        assert_eq!(t.phase, PhaseCartes::Finie);
        assert_eq!(t.gagnant, Some(A));
        assert!(!t.gagnant_bot);
        assert_eq!(t.victoires.get(&A), Some(&1));
        assert_eq!(t.vue(B, now).tour, None);
        // On redistribue avec les mêmes.
        t.lancer(A, now).unwrap();
        assert_eq!(t.manche, 2);
        assert_eq!(t.gagnant, None);
    }

    #[test]
    fn trois_tours_manques_et_un_bot_prend_le_relais() {
        let t0 = Instant::now();
        let mut t = manche(&[(A, &[54, 56]), (B, &[4, 6]), (C, &[8, 10])], 13, t0);
        assert!(!t.tic(t0 + Duration::from_secs(29)));
        let mut now = t0 + TOUR;
        assert!(t.tic(now));
        assert_eq!(main_de(&t, A), 3, "le serveur a pioché pour lui");
        assert_ne!(t.places[t.tour].id, A);
        for _ in 0..12 {
            if t.places[0].bot {
                break;
            }
            if t.places[t.tour].id == A {
                now += TOUR;
                t.tic(now);
            } else {
                let id = t.places[t.tour].id;
                t.piocher(id, now).unwrap();
                if t.piochee.is_some() {
                    t.passer(id, now).unwrap();
                }
            }
        }
        assert!(t.places[0].bot, "trois tours manqués : un bot joue sa main");
        assert_eq!(t.places[0].id, A, "la place reste la sienne");
        assert!(t.journal.contains(&EvenementCartes::Remplace { par: A }));
        assert_eq!(t.jouer(A, Carte(54), None, now), Err("un bot joue à ta place : reprends-la d'abord"));
        assert_eq!(t.annoncer(A), Err("un bot joue à ta place : reprends-la d'abord"));
        // Il clique « Reprendre ma place ».
        t.reprendre(A, A, now).unwrap();
        assert!(!t.places[0].bot);
        assert_eq!(total(&t), CARTES_DU_JEU as usize);
    }

    #[test]
    fn qui_sort_du_salon_est_remplace_par_un_bot_puis_revient() {
        let t0 = Instant::now();
        // A tient 1, 2 et 3 rouges ; B rien qui aille sur le 7 rouge.
        let mut t = manche(&[(A, &[2, 3, 5]), (B, &[54, 56])], 13, t0);
        assert!(t.accorder(&HashSet::from([B]), t0));
        let vue = t.vue(B, t0);
        assert!(vue.joueurs[0].absent && vue.joueurs[0].bot);
        assert_eq!(t.journal.back(), Some(&EvenementCartes::Remplace { par: A }));
        // C'était son tour : le bot joue dans la seconde, une carte qui va.
        assert!(!t.tic(t0 + Duration::from_millis(500)));
        assert!(t.tic(t0 + BOT_DELAI));
        assert_eq!(main_de(&t, A), 2);
        assert_eq!(t.places[t.tour].id, B);
        // Revenu : sa place lui revient, sans bot.
        assert!(t.accorder(&HashSet::from([A, B]), t0 + Duration::from_secs(5)));
        assert!(!t.places[0].bot);
        assert_eq!(t.journal.back(), Some(&EvenementCartes::Reprend { par: A, place: A }));
        assert_eq!(total(&t), CARTES_DU_JEU as usize);
    }

    #[test]
    fn un_pote_reprend_les_cartes_du_bot() {
        let now = Instant::now();
        let mut t = manche(&[(A, &[2, 3, 5]), (B, &[54, 56]), (C, &[8, 10])], 13, now);
        let main_de_a = t.places[0].main.clone();
        // A s'en va ; D arrive dans le salon et regarde.
        t.accorder(&HashSet::from([B, C, D]), now);
        assert!(t.places[0].bot);
        assert_eq!(t.rejoindre(D), Err("une manche est en cours : prends la place d'un bot, ou attends la suivante"));
        assert_eq!(t.reprendre(D, B, now), Err("cette place a déjà un joueur"));
        assert_eq!(t.reprendre(C, A, now), Err("tu as déjà une place à la table"));
        t.reprendre(D, A, now).unwrap();
        assert_eq!(t.places[0].id, D);
        assert!(!t.places[0].bot);
        assert_eq!(t.journal.back(), Some(&EvenementCartes::Reprend { par: D, place: A }));
        let mut vue = t.vue(D, now).main;
        let mut attendu = main_de_a.clone();
        vue.sort();
        attendu.sort();
        assert_eq!(vue, attendu, "il a les cartes du bot");
        assert!(t.vue(A, now).main.is_empty(), "A ne voit plus cette main");
        // C'était le tour de cette place : D a son tour entier.
        assert_eq!(t.places[t.tour].id, D);
        assert!(t.echeance.is_some_and(|e| e >= now + TOUR));
        // A revient : la place est prise, il regarde.
        t.accorder(&HashSet::from([A, B, C, D]), now);
        assert!(t.place(A).is_none());
        // D gagne pour lui, pas pour A.
        t.places[0].main = vec![Carte(2)];
        t.jouer(D, Carte(2), None, now).unwrap();
        assert_eq!(t.victoires.get(&D), Some(&1));
        assert_eq!(t.victoires.get(&A), None);
    }

    #[test]
    fn la_pioche_d_un_bot_ferme_le_contre() {
        let now = Instant::now();
        // A pose son avant-dernière carte sans l'annoncer ; B, un bot sans
        // rien de jouable, pioche : trop tard pour contrer A.
        let mut t = manche(&[(A, &[2, 3]), (B, &[54, 56]), (C, &[8, 10])], 13, now);
        t.accorder(&HashSet::from([A, C]), now);
        t.jouer(A, Carte(2), None, now).unwrap();
        assert_eq!(t.expose, Some(A));
        t.pioche.retain(|c| *c != Carte(60));
        t.pioche.push(Carte(60));
        assert!(t.tic(now + BOT_DELAI));
        assert_eq!(t.places[t.tour].id, C);
        assert_eq!(t.expose, None);
        t.contrer(C).unwrap();
        assert_eq!(main_de(&t, A), 1, "le contre arrive trop tard");
    }

    #[test]
    fn l_evince_attend_la_manche_suivante() {
        let now = Instant::now();
        let mut t = manche(&[(A, &[2, 3, 5]), (B, &[54, 56]), (C, &[8, 10])], 13, now);
        // A part, D prend sa place ; puis C part aussi.
        t.accorder(&HashSet::from([B, C, D]), now);
        t.reprendre(D, A, now).unwrap();
        t.accorder(&HashSet::from([A, B, D]), now);
        assert!(t.places[2].bot, "un bot tient la place de C");
        // A est revenu, mais sa place a été prise : il attend.
        assert!(t.vue(A, now).en_attente);
        assert_eq!(t.reprendre(A, C, now), Err("ta place a été reprise : tu rejoues à la manche suivante"));
        // La manche suivante, il se rassoit.
        t.places[0].main = vec![Carte(2)];
        t.tour = 0;
        t.jouer(D, Carte(2), None, now).unwrap();
        assert!(!t.vue(A, now).en_attente);
        t.rejoindre(A).unwrap();
    }

    #[test]
    fn l_hote_dont_on_reprend_la_place_passe_la_main() {
        let now = Instant::now();
        let mut t = manche(&[(A, &[2, 3]), (B, &[54, 56])], 13, now);
        // A, l'hôte, laisse un bot jouer ; D prend sa place.
        t.places[0].bot = true;
        t.reprendre(D, A, now).unwrap();
        assert_eq!(t.hote, D);
    }

    #[test]
    fn abandonner_laisse_sa_main_a_un_bot() {
        let now = Instant::now();
        // 23 = +2 rouge : A le pose ; B, qui le doit, abandonne.
        let mut t = manche(&[(A, &[23, 2]), (B, &[4, 6]), (C, &[8, 10])], 13, now);
        t.jouer(A, Carte(23), None, now).unwrap();
        t.quitter(B, now);
        let b = t.place(B).expect("sa place reste, tenue par un bot");
        assert!(t.places[b].bot);
        assert_eq!(t.places[t.tour].id, B, "pas de trou dans le tour");
        // Le bot, sans +2 ni +4, paie le cumul : C n'y est pour rien.
        assert!(t.tic(now + BOT_DELAI));
        assert_eq!(main_de(&t, B), 4);
        assert_eq!(t.places[t.tour].id, C);
        assert_eq!(t.cumul, 0);
        // Revenir dans le salon ne rend pas une place abandonnée…
        t.accorder(&HashSet::from([A, C]), now);
        t.accorder(&HashSet::from([A, B, C]), now);
        assert!(t.places[b].bot);
        // … mais il peut la reprendre lui-même.
        t.reprendre(B, B, now).unwrap();
        assert!(!t.places[b].bot);
        assert_eq!(total(&t), CARTES_DU_JEU as usize);
    }

    #[test]
    fn l_hote_qui_abandonne_laisse_la_distribution() {
        let now = Instant::now();
        let mut t = manche(&[(A, &[2, 3]), (B, &[4, 6])], 13, now);
        t.quitter(A, now);
        assert_eq!(t.hote, B);
    }

    #[test]
    fn la_fin_de_manche_libere_les_places_des_partis() {
        let now = Instant::now();
        let mut t = manche(&[(A, &[2]), (B, &[4, 6]), (C, &[8, 10]), (D, &[54, 56])], 13, now);
        t.quitter(C, now);
        t.accorder(&HashSet::from([A, B, C]), now);
        // B, resté là, a laissé passer trois tours.
        t.places[1].bot = true;
        t.jouer(A, Carte(2), None, now).unwrap();
        assert!(t.place(C).is_none() && t.place(D).is_none(), "les places des partis se libèrent");
        let b = t.place(B).unwrap();
        assert!(!t.places[b].bot, "B rejouera la prochaine manche lui-même");
        t.lancer(A, now).unwrap();
        assert_eq!(t.places.len(), 2);
    }

    #[test]
    fn un_bot_qui_gagne_ne_compte_pour_personne() {
        let now = Instant::now();
        let mut t = manche(&[(A, &[2]), (B, &[4, 6])], 13, now);
        t.quitter(A, now);
        assert!(t.tic(now + BOT_DELAI));
        assert_eq!(t.phase, PhaseCartes::Finie);
        assert_eq!(t.gagnant, Some(A));
        assert!(t.gagnant_bot);
        assert!(t.vue(B, now).gagnant_bot);
        assert!(t.victoires.is_empty());
        assert!(t.place(A).is_none(), "la place du parti se libère");
        assert_eq!(t.hote, B);
    }

    #[test]
    fn des_bots_seuls_finissent_la_manche() {
        let mut now = Instant::now();
        let mut t = table(&[A, B, C], 9);
        t.lancer(A, now).unwrap();
        // Tous sortis ; il ne reste dans le salon qu'un spectateur.
        t.accorder(&HashSet::from([99]), now);
        assert!(t.places.iter().all(|p| p.bot));
        for _ in 0..10_000 {
            if t.phase != PhaseCartes::EnCours {
                break;
            }
            now += BOT_DELAI;
            t.tic(now);
        }
        assert_eq!(t.phase, PhaseCartes::Finie);
        assert!(t.gagnant_bot);
        assert!(t.places.is_empty(), "tous partis : leurs places se libèrent");
    }

    #[test]
    fn entre_deux_manches_qui_part_se_leve() {
        let now = Instant::now();
        let mut t = table(&[A, B, C], 1);
        let sans_a: HashSet<UserId> = [B, C].into();
        assert!(t.accorder(&sans_a, now));
        assert!(t.place(A).is_none());
        assert_eq!(t.hote, B, "l'hôte parti, la main passe");
    }

    #[test]
    fn le_jeu_epuise_dit_ce_qui_manque() {
        let now = Instant::now();
        let mut t = manche(&[(A, &[23, 2]), (B, &[4, 6])], 13, now);
        // Toute la pioche chez B : après le +2, la défausse remélangée ne
        // rend qu'une carte (le 7 rouge), le dessus reste.
        let reste = std::mem::take(&mut t.pioche);
        t.places[1].main.extend(reste);
        t.jouer(A, Carte(23), None, now).unwrap();
        t.piocher(B, now).unwrap();
        assert_eq!(t.journal.back(), Some(&EvenementCartes::Pioche { par: B, n: 1, manque: 1 }));
        assert_eq!(t.cumul, 0);
        assert_eq!(total(&t), CARTES_DU_JEU as usize);
    }

    /// Des manches entières jouées au hasard, coups permis seulement, avec
    /// des allées et venues : absences, retours, abandons, places reprises
    /// par des spectateurs. Le jeu garde ses 108 cartes tant que la manche
    /// dure, et chaque manche finit par un gagnant.
    #[test]
    fn des_manches_au_hasard_restent_justes() {
        let mut now = Instant::now();
        for graine in 0..300u64 {
            let joueurs: Vec<UserId> = (1..=2 + graine % 5).collect();
            let spectateurs: Vec<UserId> = vec![100, 101, 102];
            let tous: Vec<UserId> = joueurs.iter().chain(&spectateurs).copied().collect();
            let mut presents: HashSet<UserId> = tous.iter().copied().collect();
            let mut t = table(&joueurs, graine);
            let mut de = StdRng::seed_from_u64(graine ^ 0x5eed);
            t.lancer(joueurs[0], now).unwrap();
            let mut coups = 0;
            while t.phase == PhaseCartes::EnCours {
                coups += 1;
                assert!(coups < 20_000, "la manche {graine} ne finit pas");
                assert_eq!(total(&t), CARTES_DU_JEU as usize);
                // Les allées et venues du salon.
                if de.random_bool(0.03) {
                    let qui = tous[de.random_range(0..tous.len())];
                    if !presents.remove(&qui) {
                        presents.insert(qui);
                    }
                    t.accorder(&presents, now);
                }
                if de.random_bool(0.005) {
                    t.quitter(joueurs[de.random_range(0..joueurs.len())], now);
                }
                // Un présent sans place prend celle d'un bot.
                if de.random_bool(0.05) {
                    if let Some(place) = t.places.iter().find(|p| p.bot).map(|p| p.id) {
                        let libre = tous
                            .iter()
                            .copied()
                            .find(|id| {
                                presents.contains(id)
                                    && !t.evinces.contains(id)
                                    && (t.place(*id).is_none() || *id == place)
                            });
                        if let Some(qui) = libre {
                            t.reprendre(qui, place, now).unwrap();
                        }
                    }
                }
                if t.phase != PhaseCartes::EnCours {
                    break;
                }
                let i = t.tour;
                if t.places[i].bot {
                    now += BOT_DELAI;
                    assert!(t.tic(now));
                    continue;
                }
                let id = t.places[i].id;
                let vue = t.vue(id, now);
                let jouables: Vec<Carte> = vue.main.iter().copied().filter(|c| vue.jouable(id, *c)).collect();
                if vue.main.len() <= 2 && de.random_bool(0.7) {
                    t.annoncer(id).unwrap();
                }
                if let Some(&carte) = jouables.get(de.random_range(0..jouables.len().max(1))) {
                    let couleur = CouleurCarte::TOUTES[de.random_range(0..4)];
                    t.jouer(id, carte, Some(couleur), now).unwrap();
                } else if vue.piochee.is_some() {
                    t.passer(id, now).unwrap();
                } else if de.random_bool(0.05) {
                    now += TOUR;
                    t.tic(now);
                } else {
                    t.piocher(id, now).unwrap();
                }
                if t.expose.is_some() && de.random_bool(0.5) {
                    let autre = tous[de.random_range(0..tous.len())];
                    if t.place(autre).is_some_and(|j| !t.places[j].bot) {
                        t.contrer(autre).unwrap();
                    }
                }
            }
            assert!(t.gagnant.is_some());
        }
    }
}
