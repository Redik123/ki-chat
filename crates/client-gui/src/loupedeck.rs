//! Le Loupedeck Live dans ki-chat : ses boutons, ses molettes et ses
//! écrans, pilotés directement (crate `ki-loupedeck`), sans le logiciel
//! Loupedeck.
//!
//! Un fil à part tient l'appareil. Il le cherche toutes les trois secondes
//! — branché à chaud, ou libéré par le logiciel Loupedeck qu'on vient de
//! fermer —, lit ses événements et redessine ses écrans. L'interface ne
//! fait que deux choses à chaque image : vider la file des commandes reçues
//! ([`Loupedeck::commandes`]) et décrire ce qu'elle veut voir affiché
//! ([`Loupedeck::afficher`]). Le dessin — avatars, noms, jauges — se fait
//! sur le fil, et seules les zones qui ont changé repartent vers l'appareil.
//!
//! Deux choses ne passent pas par la file, pour la même raison que le
//! raccourci des clips (voir `raccourci.rs`) : réduite derrière un jeu,
//! l'interface peut tarder à repeindre. Le push-to-talk est un drapeau que
//! `update_voice` lit directement, et le bouton des clips déclenche
//! l'enregistreur depuis le fil, à l'instant de l'appui.
//!
//! Le port série ne s'ouvre qu'une fois : tant que le logiciel Loupedeck
//! le tient, ki-chat attend (et les réglages le disent) ; tant que ki-chat
//! le tient, c'est le logiciel qui attend. D'où l'interrupteur des
//! réglages, qui rend l'appareil sans quitter ki-chat.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ab_glyph::{Font, FontArc, PxScale, ScaleFont};
use eframe::egui::{self, Color32};
use ki_loupedeck::{Controle, Ecrivain, Evenement, Lecteur};

use crate::icons::{self, Icon};
use crate::{photos, ptt, theme};

/// Ce que l'appareil demande à l'interface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Commande {
    /// Un bouton rond enfoncé (0, le plus à gauche, à 7) : l'interface fait
    /// ce que la page Loupedeck lui a fait choisir. Le push-to-talk n'y
    /// passe pas, ni le clip quand le fil peut le déclencher lui-même.
    Rond(u8),
    /// Des crans d'une molette (0-2 à gauche, de haut en bas, 3-5 à droite).
    Molette { molette: u8, crans: i8 },
    /// Une molette enfoncée.
    AppuiMolette(u8),
    /// Une touche de la grille touchée (0 à 11, ligne par ligne).
    Touche(u8),
    /// Un doigt qui glisse sur la bande de gauche : pixels vers le haut.
    Glisse(f32),
}

/// Une touche de la grille, telle que l'interface la veut.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Tuile {
    #[default]
    Vide,
    /// Un texte centré, sur une ou deux lignes.
    Texte(String),
    /// Quelqu'un du salon vocal. `avatar` : l'empreinte de sa photo, que le
    /// fil lit dans le cache disque (`photos`) ; `rang` : son palier
    /// VALORANT, dont le logo se pose dans le coin.
    Membre {
        nom: String,
        avatar: Option<String>,
        rang: Option<u8>,
        parle: bool,
        muet: bool,
        choisi: bool,
    },
    /// Un salon vocal où entrer d'un toucher.
    Salon { nom: String, occupants: usize },
    /// Une touche de menu : une icône de l'appli ou une image (qui prend
    /// alors le haut de la touche), un intitulé, une ligne de détail.
    /// Allumée : fond relevé et cadre de la couleur.
    Bouton {
        icone: Option<Icon>,
        image: Option<Image>,
        titre: String,
        detail: String,
        couleur: [u8; 3],
        allume: bool,
    },
    /// Une stat : un intitulé en petit, un grand chiffre de la couleur, une
    /// ligne de détail (« K/D · 7 J », « 1,24 », « 18 / 14 / 6 »).
    Chiffre { titre: String, valeur: String, detail: String, couleur: [u8; 3] },
    /// Une suite de résultats en pastilles, du plus récent : 1 victoire,
    /// -1 défaite, 0 nul ; dix au plus, sur deux lignes.
    Forme { titre: String, resultats: Vec<i8>, detail: String },
    /// Un match joué : le portrait de l'agent, le score de la couleur du
    /// résultat, le K/D/A.
    Match { victoire: Option<bool>, score: String, kda: String, agent: Option<Image> },
}

/// Ce que le fil fait lui-même d'un bouton rond, sans attendre l'interface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Role {
    /// Rien : pas même la lumière blanche de l'appui.
    #[default]
    Rien,
    /// Une commande pour l'interface ([`Commande::Rond`]).
    Commande,
    /// Le push-to-talk : tenu tant qu'on appuie (voir [`Loupedeck::ptt`]).
    Ptt,
    /// Le clip, déclenché depuis le fil (voir [`Loupedeck::action_clip`]).
    Clip,
}

/// Une image que le fil lit sur le disque, à la demande — jamais d'octets
/// d'image dans l'affichage que l'interface compare à chaque image.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Image {
    /// Une photo de profil, par empreinte (cache `photos`).
    Avatar(String),
    /// Le logo d'un palier de rang VALORANT (cache `rangs`).
    Rang(u8),
    /// Un fichier image : une miniature, une image du jeu.
    Fichier(std::path::PathBuf),
}

/// Une case d'une bande latérale, en face d'une molette.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Jauge {
    pub titre: String,
    pub valeur: String,
    /// La barre sous la valeur (0 à 1), s'il y en a une.
    pub fraction: Option<f32>,
    /// Faux : grisée (sans effet pour l'instant).
    pub actif: bool,
}

/// Tout ce que montre l'appareil.
#[derive(Clone, Debug, PartialEq)]
pub struct Affichage {
    /// Le fond des touches et des bandes, et la couleur des jauges.
    pub fond: [u8; 3],
    pub accent: [u8; 3],
    /// La couleur de la LED de chaque bouton rond.
    pub ronds: [[u8; 3]; 8],
    pub tuiles: [Tuile; 12],
    /// En face des trois molettes de gauche, de haut en bas.
    pub gauche: [Jauge; 3],
    /// En face des trois molettes de droite.
    pub droite: [Jauge; 3],
}

impl Default for Affichage {
    fn default() -> Self {
        Self {
            fond: crate::loupedeck_config::FOND,
            accent: crate::loupedeck_config::ACCENT,
            ronds: Default::default(),
            tuiles: Default::default(),
            gauche: Default::default(),
            droite: Default::default(),
        }
    }
}

/// Où en est l'appareil, pour les réglages.
#[derive(Clone, Debug, PartialEq)]
pub enum Statut {
    /// L'interrupteur des réglages est coupé.
    Coupe,
    /// Aucun Loupedeck branché.
    Absent,
    /// Branché, mais le port ne s'ouvre pas — presque toujours parce que le
    /// logiciel Loupedeck le tient.
    Occupe(String),
    /// Branché et piloté par ki-chat.
    Pilote,
}

/// L'état partagé entre l'interface et le fil.
struct Partage {
    actif: AtomicBool,
    stop: AtomicBool,
    statut: Mutex<Statut>,
    commandes: Mutex<Vec<Commande>>,
    affichage: Mutex<Affichage>,
    /// Avance à chaque nouvel affichage : le fil compare à ce qu'il a dessiné.
    generation: AtomicU64,
    /// Le bouton du push-to-talk enfoncé, et l'instant où il a été relâché.
    ptt_tenu: AtomicBool,
    ptt_relache: Mutex<Option<Instant>>,
    /// Ce que déclenche le bouton des clips, depuis le fil.
    clip: Mutex<Option<ptt::Action>>,
    /// Ce que le fil fait lui-même de chaque bouton rond.
    roles: Mutex<[Role; 8]>,
    /// La luminosité des écrans voulue (0 à 10).
    luminosite: AtomicU8,
}

impl Partage {
    fn pousser(&self, c: Commande, ctx: &egui::Context) {
        let mut file = self.commandes.lock().unwrap();
        // Des crans qui s'enchaînent sur la même molette s'additionnent :
        // l'interface endormie ne se réveille pas sur cent commandes.
        if let (Some(Commande::Molette { molette: m0, crans: c0 }), Commande::Molette { molette, crans }) =
            (file.last_mut(), c)
        {
            if *m0 == molette {
                *c0 = c0.saturating_add(crans);
                ctx.request_repaint();
                return;
            }
        }
        // Une interface qui ne repasse pas ne doit pas faire gonfler la file.
        if file.len() < 64 {
            file.push(c);
        }
        ctx.request_repaint();
    }

    fn statut(&self, s: Statut, ctx: &egui::Context) {
        let mut actuel = self.statut.lock().unwrap();
        if *actuel != s {
            match &s {
                Statut::Pilote => tracing::info!("loupedeck : branché et piloté"),
                Statut::Occupe(e) => tracing::warn!("loupedeck : port occupé ({e})"),
                Statut::Absent if *actuel == Statut::Pilote => tracing::warn!("loupedeck : perdu"),
                _ => {}
            }
            *actuel = s;
            ctx.request_repaint();
        }
    }

    fn relacher_ptt(&self, ctx: &egui::Context) {
        if self.ptt_tenu.swap(false, Ordering::Relaxed) {
            *self.ptt_relache.lock().unwrap() = Some(Instant::now());
            ctx.request_repaint();
        }
    }
}

/// Le bouton push-to-talk du Loupedeck, vu d'un autre fil.
#[derive(Clone)]
pub struct BoutonPtt(Arc<Partage>);

impl BoutonPtt {
    /// Vrai si le bouton est enfoncé, ou relâché depuis moins que le
    /// maintien.
    pub fn tenu(&self, maintien_ms: u32) -> bool {
        if self.0.ptt_tenu.load(Ordering::Relaxed) {
            return true;
        }
        self.0
            .ptt_relache
            .lock()
            .unwrap()
            .is_some_and(|t| t.elapsed() < Duration::from_millis(maintien_ms as u64))
    }
}

pub struct Loupedeck {
    partage: Arc<Partage>,
    /// Le dernier affichage transmis au fil.
    dernier: Affichage,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Loupedeck {
    pub fn demarrer(ctx: egui::Context) -> Self {
        let partage = Arc::new(Partage {
            actif: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            statut: Mutex::new(Statut::Coupe),
            commandes: Mutex::new(Vec::new()),
            affichage: Mutex::new(Affichage::default()),
            generation: AtomicU64::new(0),
            ptt_tenu: AtomicBool::new(false),
            ptt_relache: Mutex::new(None),
            clip: Mutex::new(None),
            roles: Mutex::new([Role::Rien; 8]),
            luminosite: AtomicU8::new(10),
        });
        let handle = std::thread::Builder::new()
            .name("ki-loupedeck".into())
            .spawn({
                let partage = partage.clone();
                move || boucle(partage, ctx)
            })
            .ok();
        Self { partage, dernier: Affichage::default(), handle }
    }

    /// L'interrupteur des réglages. Coupé, le fil éteint l'appareil et rend
    /// le port.
    pub fn activer(&self, on: bool) {
        self.partage.actif.store(on, Ordering::Relaxed);
    }

    pub fn statut(&self) -> Statut {
        self.partage.statut.lock().unwrap().clone()
    }

    /// Les commandes reçues depuis le dernier appel.
    pub fn commandes(&self) -> Vec<Commande> {
        std::mem::take(&mut *self.partage.commandes.lock().unwrap())
    }

    /// Ce qu'on veut voir sur l'appareil. À appeler à chaque image : rien ne
    /// part si rien n'a changé.
    pub fn afficher(&mut self, a: Affichage) {
        if a != self.dernier {
            *self.partage.affichage.lock().unwrap() = a.clone();
            self.dernier = a;
            self.partage.generation.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Le bouton du push-to-talk, pour le fil du micro : il le lit à chaque
    /// trame, sans passer par l'interface.
    pub fn bouton_ptt(&self) -> BoutonPtt {
        BoutonPtt(self.partage.clone())
    }

    /// Ce que déclenche le bouton des clips. `None` = l'interface s'en
    /// charge (et dit qu'il n'y a pas d'enregistreur).
    pub fn action_clip(&self, a: Option<ptt::Action>) {
        *self.partage.clip.lock().unwrap() = a;
    }

    /// Ce que le fil fait lui-même de chaque bouton rond. À chaque image :
    /// rien n'est verrouillé longtemps.
    pub fn roles(&self, roles: [Role; 8]) {
        *self.partage.roles.lock().unwrap() = roles;
    }

    /// La luminosité des écrans, de 0 à 10.
    pub fn luminosite(&self, niveau: u8) {
        self.partage.luminosite.store(niveau.min(10), Ordering::Relaxed);
    }

}

impl Drop for Loupedeck {
    fn drop(&mut self) {
        self.partage.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Entre deux recherches de l'appareil.
const RECHERCHE: Duration = Duration::from_secs(3);
/// Au plus vingt-cinq dessins par seconde : quelqu'un qui parle par à-coups
/// ne doit pas saturer le lien.
const ENTRE_DESSINS: Duration = Duration::from_millis(40);
/// Entre deux nouvelles tentatives sur les touches restées sans image.
const RELANCE_IMAGES: Duration = Duration::from_secs(2);

fn boucle(p: Arc<Partage>, ctx: egui::Context) {
    let mut appareil: Option<Appareil> = None;
    let mut prochain_essai = Instant::now();
    while !p.stop.load(Ordering::Relaxed) {
        if !p.actif.load(Ordering::Relaxed) {
            if let Some(mut a) = appareil.take() {
                a.eteindre();
            }
            p.relacher_ptt(&ctx);
            p.statut(Statut::Coupe, &ctx);
            prochain_essai = Instant::now();
            std::thread::sleep(Duration::from_millis(250));
            continue;
        }

        let Some(a) = appareil.as_mut() else {
            if Instant::now() < prochain_essai {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            prochain_essai = Instant::now() + RECHERCHE;
            match ki_loupedeck::trouver_port() {
                Err(_) => p.statut(Statut::Absent, &ctx),
                Ok(port) => match Appareil::ouvrir(&port) {
                    Ok(a) => {
                        appareil = Some(a);
                        p.statut(Statut::Pilote, &ctx);
                    }
                    Err(e) => p.statut(Statut::Occupe(format!("{e:#}")), &ctx),
                },
            }
            continue;
        };

        match a.lecteur.suivant() {
            Ok(Some(ev)) => {
                if let Err(e) = a.traiter(ev, &p, &ctx) {
                    tracing::warn!("loupedeck : {e:#}");
                    appareil = None;
                    p.relacher_ptt(&ctx);
                    p.statut(Statut::Absent, &ctx);
                    continue;
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("loupedeck : {e:#}");
                appareil = None;
                p.relacher_ptt(&ctx);
                p.statut(Statut::Absent, &ctx);
                prochain_essai = Instant::now() + Duration::from_secs(1);
                continue;
            }
        }

        // Une touche dessinée sans son image (photo ou miniature pas encore
        // sur le disque) est refaite de temps en temps, le temps qu'elle
        // arrive — rien d'autre ne la ferait changer.
        if a.relance.elapsed() >= RELANCE_IMAGES {
            a.relance = Instant::now();
            for i in 0..12 {
                if a.incompletes[i] {
                    a.tuiles[i] = None;
                    a.a_redessiner = true;
                }
            }
        }

        let luminosite = p.luminosite.load(Ordering::Relaxed);
        if luminosite != a.luminosite {
            a.luminosite = luminosite;
            if let Err(e) = a.ecrivain.luminosite(luminosite) {
                tracing::warn!("loupedeck : {e:#}");
                appareil = None;
                p.relacher_ptt(&ctx);
                p.statut(Statut::Absent, &ctx);
                continue;
            }
        }

        let generation = p.generation.load(Ordering::Relaxed);
        if (generation != a.generation || a.a_redessiner) && a.dessine.elapsed() >= ENTRE_DESSINS {
            let aff = p.affichage.lock().unwrap().clone();
            a.generation = generation;
            a.a_redessiner = false;
            a.dessine = Instant::now();
            if let Err(e) = a.dessiner(&aff) {
                tracing::warn!("loupedeck : dessin : {e:#}");
                appareil = None;
                p.relacher_ptt(&ctx);
                p.statut(Statut::Absent, &ctx);
            }
        }
    }
    if let Some(mut a) = appareil.take() {
        a.eteindre();
    }
}

/// L'appareil ouvert, et ce qu'il montre en ce moment.
struct Appareil {
    ecrivain: Ecrivain,
    lecteur: Lecteur,
    generation: u64,
    a_redessiner: bool,
    dessine: Instant,
    tuiles: [Option<Tuile>; 12],
    /// Les touches dessinées sans une de leurs images, et la dernière
    /// tentative.
    incompletes: [bool; 12],
    relance: Instant,
    gauche: Option<[Jauge; 3]>,
    droite: Option<[Jauge; 3]>,
    ronds: [Option<[u8; 3]>; 8],
    /// Les ronds enfoncés, allumés en blanc tant qu'on appuie.
    tenus: [bool; 8],
    /// Les doigts posés, et ceux qui glissent sur la bande de gauche (avec
    /// leur dernière hauteur).
    doigts: HashSet<u8>,
    glisse: HashMap<u8, u16>,
    peintre: Peintre,
    /// La luminosité réglée (`u8::MAX` : pas encore).
    luminosite: u8,
    /// Les couleurs avec lesquelles l'écran a été dessiné : d'autres, et
    /// tout est à refaire.
    couleurs: Option<([u8; 3], [u8; 3])>,
}

impl Appareil {
    fn ouvrir(port: &str) -> anyhow::Result<Self> {
        let (ecrivain, mut lecteur) = ki_loupedeck::ouvrir(Some(port))?;
        if ecrivain.poignee() == ki_loupedeck::Poignee::Reprise {
            tracing::info!(
                "loupedeck : il était resté en WebSocket (ki-chat précédent arrêté net) — remis d'aplomb"
            );
        }
        // Court : le relâché du push-to-talk et le dessin n'attendent pas.
        lecteur.delai(Duration::from_millis(15))?;
        Ok(Self {
            ecrivain,
            lecteur,
            generation: u64::MAX,
            a_redessiner: true,
            dessine: Instant::now() - ENTRE_DESSINS,
            tuiles: Default::default(),
            incompletes: [false; 12],
            relance: Instant::now(),
            gauche: None,
            droite: None,
            ronds: [None; 8],
            tenus: [false; 8],
            doigts: HashSet::new(),
            glisse: HashMap::new(),
            peintre: Peintre::new(),
            luminosite: u8::MAX,
            couleurs: None,
        })
    }

    fn traiter(&mut self, ev: Evenement, p: &Partage, ctx: &egui::Context) -> anyhow::Result<()> {
        match ev {
            Evenement::Appui(Controle::Rond(i)) => {
                let role = p.roles.lock().unwrap()[i as usize];
                if role != Role::Rien {
                    self.tenus[i as usize] = true;
                    self.ecrivain.couleur_rond(i, 255, 255, 255)?;
                }
                match role {
                    Role::Rien => {}
                    Role::Commande => p.pousser(Commande::Rond(i), ctx),
                    Role::Ptt => {
                        p.ptt_tenu.store(true, Ordering::Relaxed);
                        *p.ptt_relache.lock().unwrap() = None;
                        ctx.request_repaint();
                    }
                    Role::Clip => {
                        let action = p.clip.lock().unwrap().clone();
                        match action {
                            Some(a) => {
                                a();
                                ctx.request_repaint();
                            }
                            // Pas d'enregistreur : l'interface dira pourquoi.
                            None => p.pousser(Commande::Rond(i), ctx),
                        }
                    }
                }
            }
            Evenement::Relache(Controle::Rond(i)) => {
                // Relâché quel que soit le rôle du moment : un rôle changé
                // pendant l'appui ne doit pas laisser le micro ouvert.
                if p.ptt_tenu.load(Ordering::Relaxed) {
                    p.relacher_ptt(ctx);
                }
                if self.tenus[i as usize] {
                    self.tenus[i as usize] = false;
                    self.ronds[i as usize] = None;
                    self.a_redessiner = true;
                }
            }
            Evenement::Appui(Controle::Molette(m)) => p.pousser(Commande::AppuiMolette(m), ctx),
            Evenement::Relache(Controle::Molette(_)) => {}
            Evenement::Tourne { molette, crans } => p.pousser(Commande::Molette { molette, crans }, ctx),
            Evenement::Toucher { x, y, doigt, touche } => {
                // Un doigt qu'on ne connaît pas : un nouveau contact. Un
                // Lever perdu ne doit pas bloquer la suite.
                if self.doigts.len() > 10 {
                    self.doigts.clear();
                    self.glisse.clear();
                }
                let nouveau = self.doigts.insert(doigt);
                if nouveau {
                    if let Some(t) = touche {
                        p.pousser(Commande::Touche(t), ctx);
                    }
                    if x < ki_loupedeck::MARGE_GAUCHE {
                        self.glisse.insert(doigt, y);
                    }
                } else if let Some(avant) = self.glisse.insert(doigt, y) {
                    let dy = avant as f32 - y as f32;
                    if dy != 0.0 {
                        p.pousser(Commande::Glisse(dy), ctx);
                    }
                }
            }
            Evenement::Lever { doigt, .. } => {
                self.doigts.remove(&doigt);
                self.glisse.remove(&doigt);
            }
        }
        Ok(())
    }

    /// Ne renvoie que ce qui a changé ; un seul rafraîchissement de la
    /// dalle pour toutes les zones redessinées.
    fn dessiner(&mut self, aff: &Affichage) -> anyhow::Result<()> {
        use ki_loupedeck::{LARGEUR, MARGE_GAUCHE, TOUCHE};
        if self.couleurs != Some((aff.fond, aff.accent)) {
            self.couleurs = Some((aff.fond, aff.accent));
            self.peintre.couleurs(aff.fond, aff.accent);
            self.tuiles = Default::default();
            self.gauche = None;
            self.droite = None;
        }
        let mut change = false;
        for (i, tuile) in aff.tuiles.iter().enumerate() {
            if self.tuiles[i].as_ref() == Some(tuile) {
                continue;
            }
            let (pixels, complete) = self.peintre.tuile(tuile);
            let (x, y) = (MARGE_GAUCHE + (i as u16 % 4) * TOUCHE, (i as u16 / 4) * TOUCHE);
            self.ecrivain.tampon(x, y, TOUCHE, TOUCHE, &pixels)?;
            self.tuiles[i] = Some(tuile.clone());
            self.incompletes[i] = !complete;
            change = true;
        }
        let bande_l = MARGE_GAUCHE;
        let bande_h = ki_loupedeck::HAUTEUR;
        if self.gauche.as_ref() != Some(&aff.gauche) {
            let pixels = self.peintre.bande(&aff.gauche);
            self.ecrivain.tampon(0, 0, bande_l, bande_h, &pixels)?;
            self.gauche = Some(aff.gauche.clone());
            change = true;
        }
        if self.droite.as_ref() != Some(&aff.droite) {
            let pixels = self.peintre.bande(&aff.droite);
            self.ecrivain.tampon(LARGEUR - bande_l, 0, bande_l, bande_h, &pixels)?;
            self.droite = Some(aff.droite.clone());
            change = true;
        }
        if change {
            self.ecrivain.rafraichir()?;
        }
        for (i, &[r, g, b]) in aff.ronds.iter().enumerate() {
            if self.tenus[i] || self.ronds[i] == Some([r, g, b]) {
                continue;
            }
            self.ecrivain.couleur_rond(i as u8, r, g, b)?;
            self.ronds[i] = Some([r, g, b]);
        }
        Ok(())
    }

    /// Tout éteindre avant de rendre l'appareil : des écrans figés sur un
    /// salon qu'on a quitté laisseraient croire que ki-chat les pilote encore.
    fn eteindre(&mut self) {
        let _ = self.ecrivain.remplir_tout(0, 0, 0);
        for i in 0..8 {
            let _ = self.ecrivain.couleur_rond(i, 0, 0, 0);
        }
    }
}

// ---------------------------------------------------------------------------
// Le dessin
// ---------------------------------------------------------------------------

pub(crate) fn rgb(c: Color32) -> [u8; 3] {
    [c.r(), c.g(), c.b()]
}


/// Une image en cours de dessin, en RGB 24 bits.
struct Toile {
    l: i32,
    h: i32,
    px: Vec<[u8; 3]>,
}

impl Toile {
    fn new(l: i32, h: i32, fond: [u8; 3]) -> Self {
        Self { l, h, px: vec![fond; (l * h) as usize] }
    }

    /// Mélange une couleur dans un pixel, `a` de 0 (rien) à 1 (opaque).
    fn point(&mut self, x: i32, y: i32, c: [u8; 3], a: f32) {
        if x < 0 || y < 0 || x >= self.l || y >= self.h || a <= 0.0 {
            return;
        }
        let a = a.min(1.0);
        let d = &mut self.px[(y * self.l + x) as usize];
        for k in 0..3 {
            d[k] = (d[k] as f32 + (c[k] as f32 - d[k] as f32) * a).round() as u8;
        }
    }

    fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: [u8; 3]) {
        for y in y0.max(0)..y1.min(self.h) {
            for x in x0.max(0)..x1.min(self.l) {
                self.px[(y * self.l + x) as usize] = c;
            }
        }
    }

    fn cadre(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, ep: i32, c: [u8; 3]) {
        self.rect(x0, y0, x1, y0 + ep, c);
        self.rect(x0, y1 - ep, x1, y1, c);
        self.rect(x0, y0, x0 + ep, y1, c);
        self.rect(x1 - ep, y0, x1, y1, c);
    }

    /// Parcourt les pixels autour d'un centre, avec leur distance à lui.
    fn autour(&mut self, cx: f32, cy: f32, r: f32, mut f: impl FnMut(&mut Self, i32, i32, f32)) {
        let (x0, x1) = ((cx - r - 1.0).floor() as i32, (cx + r + 1.0).ceil() as i32);
        let (y0, y1) = ((cy - r - 1.0).floor() as i32, (cy + r + 1.0).ceil() as i32);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                f(self, x, y, (dx * dx + dy * dy).sqrt());
            }
        }
    }

    fn disque(&mut self, cx: f32, cy: f32, r: f32, c: [u8; 3]) {
        self.autour(cx, cy, r, |t, x, y, d| t.point(x, y, c, (r - d + 0.5).clamp(0.0, 1.0)));
    }

    fn anneau(&mut self, cx: f32, cy: f32, r: f32, ep: f32, c: [u8; 3]) {
        self.autour(cx, cy, r, |t, x, y, d| {
            let a = (r - d + 0.5).clamp(0.0, 1.0) * (d - (r - ep) + 0.5).clamp(0.0, 1.0);
            t.point(x, y, c, a);
        });
    }

    /// Un trait épais aux bouts ronds.
    fn trait_(&mut self, (ax, ay): (f32, f32), (bx, by): (f32, f32), ep: f32, c: [u8; 3]) {
        let (vx, vy) = (bx - ax, by - ay);
        let long2 = (vx * vx + vy * vy).max(1e-6);
        let (x0, x1) = ((ax.min(bx) - ep).floor() as i32, (ax.max(bx) + ep).ceil() as i32);
        let (y0, y1) = ((ay.min(by) - ep).floor() as i32, (ay.max(by) + ep).ceil() as i32);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let t = (((px - ax) * vx + (py - ay) * vy) / long2).clamp(0.0, 1.0);
                let (dx, dy) = (px - (ax + t * vx), py - (ay + t * vy));
                let d = (dx * dx + dy * dy).sqrt();
                self.point(x, y, c, (ep / 2.0 - d + 0.5).clamp(0.0, 1.0));
            }
        }
    }

    /// Une image détourée en disque (une photo de profil, carrée).
    fn vignette(&mut self, cx: f32, cy: f32, r: f32, v: &Vignette) {
        let (l, h) = (v.l as f32, v.h as f32);
        self.autour(cx, cy, r, |t, x, y, d| {
            let a = (r - d + 0.5).clamp(0.0, 1.0);
            if a <= 0.0 {
                return;
            }
            let u = ((x as f32 + 0.5 - (cx - r)) / (2.0 * r) * l).clamp(0.0, l - 1.0) as usize;
            let w = ((y as f32 + 0.5 - (cy - r)) / (2.0 * r) * h).clamp(0.0, h - 1.0) as usize;
            let [pr, pg, pb, pa] = v.px[w * v.l + u];
            t.point(x, y, [pr, pg, pb], a * pa as f32 / 255.0);
        });
    }

    /// Une image posée telle quelle, coin haut-gauche en (x, y).
    fn image(&mut self, x: i32, y: i32, v: &Vignette) {
        for j in 0..v.h {
            for i in 0..v.l {
                let [r, g, b, a] = v.px[j * v.l + i];
                self.point(x + i as i32, y + j as i32, [r, g, b], a as f32 / 255.0);
            }
        }
    }

    /// Un masque de couverture (une icône), dans une couleur.
    fn masque(&mut self, x: i32, y: i32, cote: usize, masque: &[f32], c: [u8; 3]) {
        for j in 0..cote {
            for i in 0..cote {
                self.point(x + i as i32, y + j as i32, c, masque[j * cote + i]);
            }
        }
    }

    /// En RGB565, prête à partir.
    fn rgb565(&self) -> Vec<u16> {
        self.px.iter().map(|&[r, g, b]| ki_loupedeck::rgb565(r, g, b)).collect()
    }
}

/// Une image décodée et mise à la taille voulue, en RGBA.
struct Vignette {
    l: usize,
    h: usize,
    px: Vec<[u8; 4]>,
}

/// Comment une image prend sa place.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Ajuste {
    /// Remplit le cadre, rognée au centre (une miniature de clip, une photo).
    Couvre,
    /// Tient tout entière dans le cadre, centrée (un logo de rang).
    Contient,
}

/// Ce qu'on voit vraiment d'une touche : la grille de l'appareil couvre le
/// bord des cases, et le bas d'autant plus qu'on regarde l'écran de biais,
/// assis devant — le texte posé à 84 px était coupé (photos de drion,
/// 08/10). Tout ce qui doit se lire tient entre ces deux lignes.
const HAUT: f32 = 8.0;
const BAS: f32 = 75.0;

pub(crate) struct Peintre {
    police: Option<FontArc>,
    /// Le fond des touches et l'accent (les jauges).
    fond: [u8; 3],
    accent: [u8; 3],
    /// Les images décodées, par source et par taille. Une image absente
    /// (pas encore sur le disque) ou illisible n'y entre pas : elle est
    /// redemandée au dessin suivant.
    images: HashMap<(Image, u32, u32, Ajuste), Vignette>,
    icones: Icones,
}

impl Peintre {
    pub(crate) fn new() -> Self {
        Self {
            police: police(),
            fond: crate::loupedeck_config::FOND,
            accent: crate::loupedeck_config::ACCENT,
            images: HashMap::new(),
            icones: Icones::new(),
        }
    }

    fn couleurs(&mut self, fond: [u8; 3], accent: [u8; 3]) {
        self.fond = fond;
        self.accent = accent;
    }

    /// L'écran entier (480 × 270, RGB565) : l'aperçu d'une page dans la page
    /// Loupedeck, dessiné comme sur l'appareil ; et s'il a toutes ses images.
    pub(crate) fn ecran(&mut self, aff: &Affichage) -> (Vec<u16>, bool) {
        use ki_loupedeck::{HAUTEUR, LARGEUR, MARGE_GAUCHE, TOUCHE};
        self.couleurs(aff.fond, aff.accent);
        let largeur = LARGEUR as usize;
        let mut ecran = vec![0u16; largeur * HAUTEUR as usize];
        let mut poser = |x: usize, y: usize, l: usize, pixels: &[u16]| {
            for (j, ligne) in pixels.chunks_exact(l).enumerate() {
                let debut = (y + j) * largeur + x;
                ecran[debut..debut + l].copy_from_slice(ligne);
            }
        };
        let mut complet = true;
        for (i, t) in aff.tuiles.iter().enumerate() {
            let (x, y) = (MARGE_GAUCHE as usize + (i % 4) * TOUCHE as usize, (i / 4) * TOUCHE as usize);
            let (pixels, complete) = self.tuile(t);
            complet &= complete;
            poser(x, y, TOUCHE as usize, &pixels);
        }
        poser(0, 0, MARGE_GAUCHE as usize, &self.bande(&aff.gauche));
        poser(largeur - MARGE_GAUCHE as usize, 0, MARGE_GAUCHE as usize, &self.bande(&aff.droite));
        (ecran, complet)
    }

    fn image(&mut self, source: &Image, l: u32, h: u32, ajuste: Ajuste) -> Option<&Vignette> {
        let cle = (source.clone(), l, h, ajuste);
        if !self.images.contains_key(&cle) {
            let octets = match source {
                Image::Avatar(empreinte) => photos::load(empreinte)?,
                Image::Rang(tier) => std::fs::read(
                    eframe::storage_dir("ki-chat")?
                        .join("valorant")
                        .join("rangs")
                        .join(format!("{tier}.png")),
                )
                .ok()?,
                Image::Fichier(chemin) => std::fs::read(chemin).ok()?,
            };
            let image = image::load_from_memory(&octets).ok()?.to_rgba8();
            let vignette = ajuster(&image, l, h, ajuste);
            // Le cache ne doit pas grossir sans fin sur une longue session.
            if self.images.len() > 96 {
                self.images.clear();
            }
            self.images.insert(cle.clone(), vignette);
        }
        self.images.get(&cle)
    }

    fn largeur(&self, s: &str, taille: f32) -> f32 {
        let Some(police) = &self.police else { return 0.0 };
        let sf = police.as_scaled(PxScale::from(taille));
        let mut l = 0.0;
        let mut avant = None;
        for ch in s.chars() {
            let id = sf.glyph_id(ch);
            if let Some(a) = avant {
                l += sf.kern(a, id);
            }
            l += sf.h_advance(id);
            avant = Some(id);
        }
        l
    }

    /// Le texte à la taille voulue, ou plus petit (jusqu'aux quatre
    /// cinquièmes) pour tenir dans `max`, et coupé d'un « … » au-delà.
    fn ajuster(&self, s: &str, taille: f32, max: f32) -> (String, f32) {
        let mut t = taille;
        while t > taille * 0.8 && self.largeur(s, t) > max {
            t -= 0.5;
        }
        if self.largeur(s, t) <= max {
            return (s.to_string(), t);
        }
        let mut coupe: String = s.to_string();
        while !coupe.is_empty() && self.largeur(&format!("{coupe}…"), t) > max {
            coupe.pop();
        }
        (format!("{}…", coupe.trim_end()), t)
    }

    /// Écrit `s` centré sur `cx`, la ligne de base à `y`.
    fn texte(&self, toile: &mut Toile, s: &str, taille: f32, cx: f32, y: f32, c: [u8; 3]) {
        let Some(police) = &self.police else { return };
        let echelle = PxScale::from(taille);
        let sf = police.as_scaled(echelle);
        let mut x = cx - self.largeur(s, taille) / 2.0;
        let mut avant = None;
        for ch in s.chars() {
            let id = sf.glyph_id(ch);
            if let Some(a) = avant {
                x += sf.kern(a, id);
            }
            avant = Some(id);
            let avance = sf.h_advance(id);
            // Un caractère que la police n'a pas (un émoji) : un blanc
            // plutôt qu'une boîte.
            if id.0 != 0 {
                let glyphe = id.with_scale_and_position(echelle, ab_glyph::point(x, y));
                if let Some(o) = police.outline_glyph(glyphe) {
                    let b = o.px_bounds();
                    o.draw(|gx, gy, a| {
                        toile.point(b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32, c, a);
                    });
                }
            }
            x += avance;
        }
    }

    /// Un texte ajusté à la largeur d'une touche, centré.
    fn ligne(&self, toile: &mut Toile, s: &str, taille: f32, y: f32, c: [u8; 3]) {
        let milieu = toile.l as f32 / 2.0;
        let (s, t) = self.ajuster(s, taille, toile.l as f32 - 10.0);
        self.texte(toile, &s, t, milieu, y, c);
    }

    /// Coupe un texte en lignes d'au plus `max` pixels, au plus `n` lignes.
    fn lignes(&self, s: &str, taille: f32, max: f32, n: usize) -> Vec<String> {
        let mut lignes: Vec<String> = Vec::new();
        let mut courante = String::new();
        for mot in s.split_whitespace() {
            let essai = if courante.is_empty() { mot.to_string() } else { format!("{courante} {mot}") };
            if self.largeur(&essai, taille) <= max || courante.is_empty() {
                courante = essai;
            } else {
                lignes.push(std::mem::take(&mut courante));
                courante = mot.to_string();
            }
        }
        if !courante.is_empty() {
            lignes.push(courante);
        }
        if lignes.len() > n {
            let reste = lignes[n - 1..].join(" ");
            lignes.truncate(n - 1);
            lignes.push(reste);
        }
        lignes.into_iter().map(|l| self.ajuster(&l, taille, max).0).collect()
    }

    /// Une icône de l'appli, centrée sur (cx, cy).
    fn icone(&mut self, toile: &mut Toile, icone: Icon, cote: u32, cx: f32, cy: f32, c: [u8; 3]) {
        let masque = self.icones.masque(icone, cote);
        let (x, y) = ((cx - cote as f32 / 2.0).round() as i32, (cy - cote as f32 / 2.0).round() as i32);
        toile.masque(x, y, cote as usize, masque, c);
    }

    /// Rend la touche, et dit si elle est complète : faux si une de ses
    /// images n'a pas pu être lue (on réessaiera).
    fn tuile(&mut self, t: &Tuile) -> (Vec<u16>, bool) {
        let cote = ki_loupedeck::TOUCHE as i32;
        let mut toile = Toile::new(cote, cote, self.fond);
        let milieu = cote as f32 / 2.0;
        let mut complete = true;
        match t {
            Tuile::Vide => {}
            Tuile::Texte(s) => {
                let lignes = self.lignes(s, 13.0, 80.0, 3);
                let haut = 46.0 - (lignes.len() as f32 - 1.0) * 8.0;
                for (i, l) in lignes.iter().enumerate() {
                    self.texte(&mut toile, l, 13.0, milieu, haut + i as f32 * 16.0, rgb(theme::TEXT_DIM));
                }
            }
            Tuile::Membre { nom, avatar, rang, parle, muet, choisi } => {
                if *choisi {
                    toile.rect(0, 0, cote, cote, rgb(theme::BG_RAISED));
                    toile.cadre(2, 2, cote - 2, cote - 2, 2, rgb(theme::TEXT_DIM));
                }
                let (cx, cy, r) = (milieu, HAUT + 25.0, 21.0);
                if *parle {
                    toile.anneau(cx, cy, r + 4.5, 3.0, rgb(theme::SPEAK));
                }
                let couleur = theme::color_for(nom);
                let photo = match avatar {
                    Some(e) => {
                        let v = self.image(&Image::Avatar(e.clone()), 64, 64, Ajuste::Couvre);
                        complete &= v.is_some();
                        v
                    }
                    None => None,
                };
                match photo {
                    Some(v) => toile.vignette(cx, cy, r, v),
                    None => {
                        toile.disque(cx, cy, r, rgb(theme::mix(theme::BG_RAISED, couleur, 0.22)));
                        toile.anneau(cx, cy, r, 1.5, rgb(theme::mix(theme::BG_RAISED, couleur, 0.6)));
                        let initiale = nom
                            .chars()
                            .find(|c| c.is_alphanumeric())
                            .map(|c| c.to_uppercase().to_string())
                            .unwrap_or_else(|| "?".into());
                        self.texte(&mut toile, &initiale, r * 1.05, cx, cy + r * 0.37, rgb(couleur));
                    }
                }
                if *muet {
                    let (mx, my) = (cx + r * 0.75, cy + r * 0.75);
                    toile.disque(mx, my, 8.5, self.fond);
                    toile.disque(mx, my, 7.0, rgb(theme::DANGER));
                    toile.trait_((mx - 3.5, my - 3.5), (mx + 3.5, my + 3.5), 2.0, [255, 255, 255]);
                }
                // Le logo du rang, petit, dans le coin : par-dessus la photo
                // s'il la touche.
                if let Some(tier) = rang {
                    match self.image(&Image::Rang(*tier), 22, 22, Ajuste::Contient) {
                        Some(v) => toile.image(cote - 28, HAUT as i32 - 2, v),
                        None => complete = false,
                    }
                }
                let couleur_nom = if *muet { theme::TEXT_DIM } else { theme::TEXT };
                self.ligne(&mut toile, nom, 13.5, BAS - 3.0, rgb(couleur_nom));
            }
            Tuile::Salon { nom, occupants } => {
                self.icone(&mut toile, Icon::Volume, 20, milieu, HAUT + 11.0, rgb(theme::TEXT_FAINT));
                let lignes = self.lignes(nom, 14.0, 80.0, 2);
                let haut = if lignes.len() > 1 { HAUT + 36.0 } else { HAUT + 43.0 };
                for (i, l) in lignes.iter().enumerate() {
                    self.texte(&mut toile, l, 14.0, milieu, haut + i as f32 * 15.0, rgb(theme::TEXT));
                }
                let qui = match occupants {
                    0 => "vide".to_string(),
                    n => format!("{n} en vocal"),
                };
                let couleur = if *occupants > 0 { theme::SPEAK } else { theme::TEXT_FAINT };
                self.ligne(&mut toile, &qui, 11.5, BAS, rgb(couleur));
            }
            Tuile::Bouton { icone, image, titre, detail, couleur, allume } => {
                if *allume {
                    toile.rect(0, 0, cote, cote, rgb(theme::BG_RAISED));
                    toile.cadre(2, 2, cote - 2, cote - 2, 2, *couleur);
                }
                match image {
                    // Une image prend le haut de la touche.
                    Some(source) => {
                        // Un logo (le rang) tient tout entier ; une miniature
                        // remplit le haut de la touche.
                        let (l, h, ajuste) = match source {
                            Image::Rang(_) => (40, 40, Ajuste::Contient),
                            _ => (cote as u32 - 14, 38, Ajuste::Couvre),
                        };
                        let x = (cote - l as i32) / 2;
                        match self.image(source, l, h, ajuste) {
                            Some(v) => toile.image(x, HAUT as i32 - 2, v),
                            None => {
                                complete = false;
                                if let Some(i) = icone {
                                    self.icone(&mut toile, *i, 24, milieu, HAUT + 18.0, *couleur);
                                }
                            }
                        }
                        self.ligne(&mut toile, titre, 12.5, BAS - 13.0, rgb(theme::TEXT));
                        self.ligne(&mut toile, detail, 10.5, BAS, rgb(theme::TEXT_DIM));
                    }
                    None => {
                        if let Some(i) = icone {
                            self.icone(&mut toile, *i, 26, milieu, HAUT + 16.0, *couleur);
                        }
                        self.ligne(&mut toile, titre, 14.0, BAS - 15.0, rgb(theme::TEXT));
                        self.ligne(&mut toile, detail, 11.0, BAS, rgb(theme::TEXT_DIM));
                    }
                }
            }
            Tuile::Chiffre { titre, valeur, detail, couleur } => {
                self.ligne(&mut toile, titre, 11.0, HAUT + 12.0, rgb(theme::TEXT_FAINT));
                self.ligne(&mut toile, valeur, 26.0, HAUT + 45.0, *couleur);
                self.ligne(&mut toile, detail, 11.0, BAS, rgb(theme::TEXT_DIM));
            }
            Tuile::Forme { titre, resultats, detail } => {
                self.ligne(&mut toile, titre, 11.0, HAUT + 12.0, rgb(theme::TEXT_FAINT));
                if resultats.is_empty() {
                    self.ligne(&mut toile, "—", 26.0, HAUT + 45.0, rgb(theme::TEXT_FAINT));
                }
                for (i, r) in resultats.iter().take(10).enumerate() {
                    let (col, lig) = ((i % 5) as f32, (i / 5) as f32);
                    let c = match r {
                        1 => theme::SPEAK,
                        -1 => theme::DANGER,
                        _ => theme::TEXT_FAINT,
                    };
                    toile.disque(17.0 + col * 14.0, HAUT + 28.0 + lig * 15.0, 5.0, rgb(c));
                }
                self.ligne(&mut toile, detail, 11.0, BAS, rgb(theme::TEXT_DIM));
            }
            Tuile::Match { victoire, score, kda, agent } => {
                if let Some(source) = agent {
                    match self.image(source, 34, 34, Ajuste::Contient) {
                        Some(v) => toile.image((cote - 34) / 2, HAUT as i32 - 1, v),
                        None => complete = false,
                    }
                }
                let couleur = match victoire {
                    Some(true) => theme::SPEAK,
                    Some(false) => theme::DANGER,
                    None => theme::TEXT_DIM,
                };
                self.ligne(&mut toile, score, 16.0, HAUT + 50.0, rgb(couleur));
                self.ligne(&mut toile, kda, 12.0, BAS, rgb(theme::TEXT));
            }
        }
        (toile.rgb565(), complete)
    }

    /// Une bande latérale : trois cases en face des trois molettes.
    fn bande(&mut self, jauges: &[Jauge; 3]) -> Vec<u16> {
        let (l, h) = (ki_loupedeck::MARGE_GAUCHE as i32, ki_loupedeck::HAUTEUR as i32);
        let mut toile = Toile::new(l, h, self.fond);
        let milieu = l as f32 / 2.0;
        for (i, j) in jauges.iter().enumerate() {
            let y0 = i as i32 * 90;
            if i > 0 {
                toile.rect(8, y0, l - 8, y0 + 1, rgb(theme::BG_RAISED));
            }
            let (titre, t) = self.ajuster(&j.titre, 11.5, l as f32 - 6.0);
            self.texte(&mut toile, &titre, t, milieu, y0 as f32 + 27.0, rgb(theme::TEXT_FAINT));
            let (valeur, t) = self.ajuster(&j.valeur, 17.0, l as f32 - 4.0);
            let couleur = if j.actif { theme::TEXT } else { theme::TEXT_FAINT };
            self.texte(&mut toile, &valeur, t, milieu, y0 as f32 + 53.0, rgb(couleur));
            if let Some(f) = j.fraction {
                let (x0, x1, yb) = (9, l - 9, y0 + 64);
                toile.rect(x0, yb, x1, yb + 4, rgb(theme::BG_RAISED));
                let plein = x0 + ((x1 - x0) as f32 * f.clamp(0.0, 1.0)).round() as i32;
                let couleur = if j.actif { self.accent } else { rgb(theme::TEXT_FAINT) };
                toile.rect(x0, yb, plein, yb + 4, couleur);
            }
        }
        toile.rgb565()
    }
}

/// Met une image à la taille d'un cadre : rognée au centre pour le
/// couvrir, ou réduite pour y tenir, au milieu d'un fond transparent.
fn ajuster(image: &image::RgbaImage, l: u32, h: u32, ajuste: Ajuste) -> Vignette {
    use image::imageops::{self, FilterType};
    let (il, ih) = (image.width().max(1), image.height().max(1));
    let px = match ajuste {
        Ajuste::Couvre => {
            // La plus grande partie de l'image qui a les proportions du cadre.
            let echelle = (il as f32 / l as f32).min(ih as f32 / h as f32);
            let (cl, ch) = ((l as f32 * echelle) as u32, (h as f32 * echelle) as u32);
            let (cl, ch) = (cl.clamp(1, il), ch.clamp(1, ih));
            let rogne = imageops::crop_imm(image, (il - cl) / 2, (ih - ch) / 2, cl, ch).to_image();
            imageops::resize(&rogne, l, h, FilterType::Triangle).pixels().map(|p| p.0).collect()
        }
        Ajuste::Contient => {
            let echelle = (l as f32 / il as f32).min(h as f32 / ih as f32);
            let (nl, nh) = (((il as f32 * echelle) as u32).max(1), ((ih as f32 * echelle) as u32).max(1));
            let reduite = imageops::resize(image, nl, nh, FilterType::Triangle);
            let mut cadre = image::RgbaImage::new(l, h);
            imageops::overlay(&mut cadre, &reduite, ((l - nl) / 2) as i64, ((h - nh) / 2) as i64);
            cadre.pixels().map(|p| p.0).collect()
        }
    };
    Vignette { l: l as usize, h: h as usize, px }
}

/// Les icônes de l'appli (`icons.rs`) sur l'appareil. Elles sont dessinées
/// au trait par egui : un contexte egui à part, sans fenêtre, les découpe
/// en triangles, rastérisés ici en un masque de couverture, une fois par
/// icône et par taille.
struct Icones {
    ctx: egui::Context,
    masques: HashMap<(u32, u32), Vec<f32>>,
}

impl Icones {
    fn new() -> Self {
        let ctx = egui::Context::default();
        // Les petits disques sortiraient de l'atlas de la police, une
        // texture qu'on ne lit pas : on les veut en triangles comme le reste.
        ctx.tessellation_options_mut(|o| o.prerasterized_discs = false);
        Self { ctx, masques: HashMap::new() }
    }

    fn masque(&mut self, icone: Icon, cote: u32) -> &[f32] {
        let ctx = &self.ctx;
        self.masques.entry((icone as u32, cote)).or_insert_with(|| {
            let n = cote as usize;
            let mut masque = vec![0.0f32; n * n];
            let cadre = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(cote as f32, cote as f32));
            let entree = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(256.0, 256.0))),
                ..Default::default()
            };
            let mut sortie = ctx.run_ui(entree, |ui| {
                let p = ui.ctx().layer_painter(egui::LayerId::background());
                icons::draw(&p, cadre, icone, Color32::WHITE);
            });
            // Rastérisé ici même, hors écran : les textures (l'atlas des
            // polices) n'iront nulle part — egui veut qu'on le dise.
            sortie.textures_delta.clear();
            for primitive in ctx.tessellate(sortie.shapes, sortie.pixels_per_point) {
                let egui::epaint::Primitive::Mesh(mesh) = primitive.primitive else { continue };
                for t in mesh.indices.as_chunks::<3>().0 {
                    let [a, b, c] = t.map(|i| mesh.vertices[i as usize]);
                    triangle(&mut masque, n, a, b, c);
                }
            }
            masque
        })
    }
}

/// Un triangle d'egui dans un masque : la couverture est l'alpha des
/// sommets (prémultiplié, donc celui du blanc), interpolé — c'est ainsi
/// qu'egui adoucit les bords, par une frange de sommets transparents.
fn triangle(masque: &mut [f32], n: usize, a: egui::epaint::Vertex, b: egui::epaint::Vertex, c: egui::epaint::Vertex) {
    let (pa, pb, pc) = (a.pos, b.pos, c.pos);
    let aire = (pb.x - pa.x) * (pc.y - pa.y) - (pb.y - pa.y) * (pc.x - pa.x);
    if aire.abs() < 1e-6 {
        return;
    }
    let borne = |v: f32| (v.max(0.0) as usize).min(n);
    let (x0, x1) = (borne(pa.x.min(pb.x).min(pc.x).floor()), borne(pa.x.max(pb.x).max(pc.x).ceil()));
    let (y0, y1) = (borne(pa.y.min(pb.y).min(pc.y).floor()), borne(pa.y.max(pb.y).max(pc.y).ceil()));
    let alphas = [a.color.a(), b.color.a(), c.color.a()].map(|v| v as f32 / 255.0);
    for y in y0..y1 {
        for x in x0..x1 {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let wa = ((pb.x - px) * (pc.y - py) - (pb.y - py) * (pc.x - px)) / aire;
            let wb = ((pc.x - px) * (pa.y - py) - (pc.y - py) * (pa.x - px)) / aire;
            let wc = 1.0 - wa - wb;
            if wa < -1e-4 || wb < -1e-4 || wc < -1e-4 {
                continue;
            }
            let alpha = (wa * alphas[0] + wb * alphas[1] + wc * alphas[2]).clamp(0.0, 1.0);
            let d = &mut masque[y * n + x];
            *d = alpha + *d * (1.0 - alpha);
        }
    }
}

/// Une police lisible sur un petit écran : Segoe UI semi-grasse sous
/// Windows, sinon celle d'egui (Ubuntu Light, embarquée).
fn police() -> Option<FontArc> {
    #[cfg(windows)]
    {
        let fonts = std::env::var_os("WINDIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| "C:\\Windows".into())
            .join("Fonts");
        for nom in ["segoeuisb.ttf", "segoeuib.ttf", "segoeui.ttf"] {
            if let Some(f) = std::fs::read(fonts.join(nom))
                .ok()
                .and_then(|o| FontArc::try_from_vec(o).ok())
            {
                return Some(f);
            }
        }
    }
    FontArc::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Les trois pages de la dalle en PNG, pour regarder le dessin sans
    /// l'appareil : `KI_APERCU=dossier cargo test -p ki-client-gui apercu --
    /// --ignored`. Les photos, les logos de rang et les miniatures viennent
    /// des caches de ki-chat, s'il y en a. Le bas de chaque touche, que le
    /// cadre de l'appareil cache quand on la regarde de biais, est assombri.
    #[test]
    #[ignore]
    fn apercu() {
        let dossier = std::path::PathBuf::from(std::env::var("KI_APERCU").expect("KI_APERCU=dossier"));
        let data = eframe::storage_dir("ki-chat").unwrap();
        let fichiers = |sous: &str, n: usize| -> Vec<std::path::PathBuf> {
            std::fs::read_dir(data.join(sous))
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "png" || x == "jpg"))
                .take(n)
                .collect()
        };
        let photos: Vec<String> = fichiers("avatars", 3)
            .iter()
            .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .collect();
        let miniatures = fichiers("clips", 8);
        let membre = |nom: &str, photo: Option<usize>, rang: Option<u8>, parle, muet, choisi| Tuile::Membre {
            nom: nom.into(),
            avatar: photo.and_then(|i| photos.get(i).cloned()),
            rang,
            parle,
            muet,
            choisi,
        };
        let bouton = |icone, image: Option<Image>, titre: &str, detail: &str, couleur, allume| Tuile::Bouton {
            icone: Some(icone),
            image,
            titre: titre.into(),
            detail: detail.into(),
            couleur,
            allume,
        };
        let jauge = |titre: &str, valeur: &str, f: Option<f32>, actif| Jauge {
            titre: titre.into(),
            valeur: valeur.into(),
            fraction: f,
            actif,
        };
        let base = Affichage {
            gauche: [
                jauge("VOLUME", "70%", Some(0.35), true),
                jauge("MICRO", "118%", Some(0.59), true),
                Jauge::default(),
            ],
            droite: [
                jauge("CHOIX", "1/3", None, true),
                jauge("Mathéo", "85%", Some(0.425), true),
                jauge("MUSIQUE", "22%", Some(0.11), true),
            ],
            ..Default::default()
        };

        let mut vocal = base.clone();
        vocal.tuiles[0] = membre("drion", Some(0), Some(18), true, false, false);
        vocal.tuiles[1] = membre("Mathéo", Some(1), Some(12), false, true, true);
        vocal.tuiles[2] = membre("un_pseudo_très_long", None, None, false, false, false);
        vocal.tuiles[3] = membre("Zoé", Some(2), Some(24), true, true, false);
        vocal.tuiles[4] = Tuile::Salon { nom: "Général".into(), occupants: 2 };
        vocal.tuiles[5] = Tuile::Salon { nom: "Salon des parties classées".into(), occupants: 0 };
        vocal.tuiles[6] = Tuile::Texte("personne d'autre ici".into());

        let rouge = [255, 70, 85];
        let (vert, blanc) = (rgb(theme::SPEAK), rgb(theme::TEXT));
        let chiffre = |titre: &str, valeur: &str, detail: &str, couleur| Tuile::Chiffre {
            titre: titre.into(),
            valeur: valeur.into(),
            detail: detail.into(),
            couleur,
        };
        let agents: Vec<std::path::PathBuf> = std::fs::read_dir(data.join("valorant").join("images"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().contains("minimapportrait"))
            .take(4)
            .collect();
        let mut valo = base.clone();
        valo.tuiles[0] = bouton(Icon::User, Some(Image::Rang(18)), "Diamant 1", "47 RR (+18)", rouge, false);
        valo.tuiles[1] = chiffre("ASCENT", "7-5", "compétition", vert);
        valo.tuiles[2] = chiffre("AUJOURD'HUI", "+38", "3 V · 1 D", vert);
        valo.tuiles[3] = Tuile::Forme {
            titre: "FORME".into(),
            resultats: vec![1, 1, 1, -1, 1, -1, -1, 1, 0, 1],
            detail: "série : 3 V".into(),
        };
        valo.tuiles[4] = chiffre("K/D · 7 J", "1,24", "18 / 14 / 6", vert);
        valo.tuiles[5] = chiffre("TÊTE · 7 J", "27%", "des tirs", blanc);
        valo.tuiles[6] = chiffre("VICTOIRES", "58%", "7 V · 5 D", vert);
        valo.tuiles[7] = chiffre("ADR · 7 J", "142", "dégâts / manche", blanc);
        for (i, (v, score, kda)) in
            [(Some(true), "13-9", "21/14/6"), (Some(false), "8-13", "12/17/3"), (Some(true), "13-11", "24/19/8"), (None, "Deathmatch", "40/31/0")]
                .into_iter()
                .enumerate()
        {
            valo.tuiles[8 + i] = Tuile::Match {
                victoire: v,
                score: score.into(),
                kda: kda.into(),
                agent: agents.get(i).cloned().map(Image::Fichier),
            };
        }

        let violet = [150, 90, 255];
        let mut clips = base.clone();
        clips.tuiles[0] = bouton(Icon::Film, None, "Clip !", "les 30 dernières s", violet, false);
        clips.tuiles[1] = bouton(Icon::Pause, None, "En marche", "toucher : arrêter", violet, true);
        clips.tuiles[2] = bouton(Icon::Sliders, None, "30 s", "durée du clip", violet, false);
        clips.tuiles[3] = bouton(Icon::Send, None, "Partager", "au toucher", rgb(theme::SPEAK), false);
        for (i, m) in miniatures.iter().enumerate() {
            clips.tuiles[4 + i] =
                bouton(Icon::Film, Some(Image::Fichier(m.clone())), "il y a 5 min", "VALORANT", violet, false);
        }

        let mut p = Peintre::new();
        for (nom, aff) in [("vocal", &vocal), ("valorant", &valo), ("clips", &clips)] {
            let mut image = image::RgbImage::new(480, 270);
            let mut poser = |x0: u32, y0: u32, l: u32, px: &[u16], tuile: bool| {
                for (i, v) in px.iter().enumerate() {
                    let (x, y) = (i as u32 % l, i as u32 / l);
                    let mut rgb = [((v >> 11) << 3) as u8, (((v >> 5) & 0x3f) << 2) as u8, ((v & 0x1f) << 3) as u8];
                    if tuile && (y as f32) > BAS + 3.0 {
                        rgb = rgb.map(|c| c / 3);
                    }
                    image.put_pixel(x0 + x, y0 + y, image::Rgb(rgb));
                }
            };
            for (i, t) in aff.tuiles.iter().enumerate() {
                let i = i as u32;
                poser(60 + (i % 4) * 90, (i / 4) * 90, 90, &p.tuile(t).0, true);
            }
            poser(0, 0, 60, &p.bande(&aff.gauche), false);
            poser(420, 0, 60, &p.bande(&aff.droite), false);
            image.save(dossier.join(format!("{nom}.png"))).unwrap();
        }
    }
}
