//! Le Loupedeck Live côté interface : ce que font ses commandes, ce que
//! montrent ses écrans, et sa ligne dans les réglages (onglet Bêta).
//!
//! Tout se règle dans la page Loupedeck (`loupedeck_page`, voir
//! `loupedeck_config`) : l'action de chaque bouton rond et de chaque
//! molette, et les pages de la grille — douze cases chacune, qui sont des
//! boutons, des places du salon vocal, des chiffres VALORANT ou des clips
//! récents. Ici, on fait vivre tout ça :
//! - une place du salon montre, en vocal, la personne à cette place (moi
//!   d'abord), et un toucher la choisit pour la molette de volume ; hors
//!   vocal, le salon vocal à cette place, où un toucher fait entrer ;
//! - les chiffres VALORANT : la partie en direct (la présence du client
//!   Riot), le reste d'après la fiche que le serveur relit après chaque
//!   match ; la première page qui en a s'affiche toute seule quand une
//!   partie commence (si on le veut) ;
//! - un clip récent se partage ou se lit d'un toucher.
//!
//! La bande de gauche : un glissé règle le volume général.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, RichText};
use ki_protocol::{
    BilanMembre, ChannelInfo, ChannelKind, ClientMsg, FicheValorant, JeuEtat, JeuStatut, MatchResume, Member,
    UserId,
};

use crate::icons::Icon;
use crate::loupedeck::{rgb, Affichage, Commande, Image, Jauge, Loupedeck, Peintre, Statut, Tuile};
use crate::loupedeck_config::{Action, ActionMolette, Bouton, Case, Config, Widget};
use crate::theme::{self, SPEAK, TEXT_FAINT, WARN};
use crate::{clips, est_bot, palier_de, ui, valo_catalogue, visionneuse, KiApp, MicMode, VoiceSnapshot, SPEAK_LEVEL};

/// Le pas d'un cran de molette sur un volume ou un gain.
const PAS_VOLUME: f32 = 0.05;
/// Les clips récents qu'une page peut montrer.
const CLIPS_MAX: usize = 12;
/// Une page qui montre des clips relit le dossier à ce rythme.
const RELIRE_CLIPS: Duration = Duration::from_secs(10);
/// Une page qui montre des chiffres VALORANT redemande les stats au
/// serveur à ce rythme.
const REDEMANDER_STATS: Duration = Duration::from_secs(300);

/// L'état du Loupedeck côté interface.
pub(crate) struct Etat {
    /// L'interrupteur des réglages : sans lui, ni pilotage, ni bouton
    /// « Loupedeck » dans la barre du bas.
    pub actif: bool,
    /// Ce que fait et montre l'appareil, réglé dans la page Loupedeck.
    pub config: Config,
    /// La page Loupedeck : ouverte, la page de la grille qu'on y regarde,
    /// la commande qu'on y règle, et son aperçu.
    pub page_ouverte: bool,
    pub onglet: usize,
    pub selection: Option<Selection>,
    /// La grille des icônes, ouverte sous le bouton qu'on règle.
    pub choix_icone: bool,
    pub apercu: Option<Apercu>,
    /// La page que montre l'appareil.
    pub ecran: usize,
    /// La personne choisie, que règle la molette « Volume de la personne
    /// choisie ».
    choix: Option<UserId>,
    /// Les derniers clips. Le dossier se relit sur un fil — il y a une
    /// fiche à lire par clip — et la liste arrive par `clips_lus`.
    clips: Vec<clips::ClipInfo>,
    clips_lus: Arc<Mutex<Option<Vec<clips::ClipInfo>>>>,
    clips_releve: Option<Instant>,
    /// La dernière demande de stats VALORANT au serveur, et les relectures
    /// programmées après une fin de partie.
    stats_demandees: Option<Instant>,
    relances_stats: Vec<Instant>,
    /// Où en était ma partie à l'image d'avant.
    partie: Option<JeuEtat>,
}

/// Une commande de l'appareil, dans la page Loupedeck : un rond, une
/// molette, ou une touche de la page qu'on y regarde.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Selection {
    Rond(usize),
    Molette(usize),
    Case(usize),
}

/// L'aperçu d'une page dans la page Loupedeck : dessiné par l'interface,
/// comme l'appareil le dessinerait — branché ou non.
pub(crate) struct Apercu {
    pub peintre: Peintre,
    pub dernier: Option<Affichage>,
    pub texture: Option<egui::TextureHandle>,
    /// Faux si une image y manquait (on redessine de temps en temps), et
    /// quand on l'a dessiné.
    pub complet: bool,
    pub dessine: Instant,
}

impl Etat {
    pub fn new(actif: bool, config: &str) -> Self {
        Self {
            actif,
            config: Config::lire(config),
            page_ouverte: false,
            onglet: 0,
            selection: None,
            choix_icone: false,
            apercu: None,
            ecran: 0,
            choix: None,
            clips: Vec::new(),
            clips_lus: Arc::default(),
            clips_releve: None,
            stats_demandees: None,
            relances_stats: Vec::new(),
            partie: None,
        }
    }

    /// Un clip vient d'être écrit : les pages de clips le montreront tout
    /// de suite.
    pub fn clips_perimes(&mut self) {
        self.clips_releve = None;
    }
}

/// Ce que dit la lumière d'une action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lumiere {
    /// Sans objet (le micro hors vocal) : éteinte.
    Eteinte,
    /// Disponible : en veilleuse.
    Veille,
    /// Allumée : la couleur de l'action.
    Allumee,
    /// L'état qu'on ne doit pas rater — micro coupé, sourd, en direct :
    /// rouge, quelle que soit la couleur choisie.
    Alerte,
}

/// Un volume avancé de quelques crans, ramené sur la grille des 5 % et
/// borné de 0 à 200 % : des crans répétés ne doivent pas accumuler
/// d'erreurs d'arrondi (« 99,99999 % »).
fn cran(v: f32, crans: f32) -> f32 {
    ((v + crans * PAS_VOLUME) / PAS_VOLUME).round().clamp(0.0, 2.0 / PAS_VOLUME) * PAS_VOLUME
}

fn pct(v: f32) -> String {
    format!("{:.0}%", v * 100.0)
}

fn attenuer([r, g, b]: [u8; 3], f: f32) -> [u8; 3] {
    [r, g, b].map(|c| (c as f32 * f) as u8)
}

/// L'âge d'un clip, court : « à l'instant », « il y a 5 min », « il y a
/// 3 h », « hier », « il y a 4 j ».
fn age(t: SystemTime) -> String {
    let s = SystemTime::now().duration_since(t).map_or(0, |d| d.as_secs());
    match s {
        0..=59 => "à l'instant".into(),
        60..=3599 => format!("il y a {} min", s / 60),
        3600..=86_399 => format!("il y a {} h", s / 3600),
        86_400..=172_799 => "hier".into(),
        _ => format!("il y a {} j", s / 86_400),
    }
}

/// Le début d'« aujourd'hui » pour un joueur : 6 h du matin, heure locale
/// — une soirée qui passe minuit reste la même soirée.
fn debut_de_journee_ms() -> u64 {
    use chrono::{Local, TimeZone, Timelike};
    let maintenant = Local::now();
    let mut jour = maintenant.date_naive();
    if maintenant.hour() < 6 {
        jour = jour.pred_opt().unwrap_or(jour);
    }
    jour.and_hms_opt(6, 0, 0)
        .and_then(|d| Local.from_local_datetime(&d).earliest())
        .map_or(0, |d| d.timestamp_millis().max(0) as u64)
}

/// Une partie VALORANT en quelques mots, pour une touche : « 7-5 Ascent »,
/// « sélection », « en file », « au menu ».
fn partie_courte(j: &JeuStatut) -> String {
    match j.etat {
        JeuEtat::EnJeu if j.custom && j.score_allie == 0 && j.score_adverse == 0 => {
            if j.carte.is_empty() { "en partie".into() } else { j.carte.clone() }
        }
        JeuEtat::EnJeu => format!("{}-{} {}", j.score_allie, j.score_adverse, j.carte).trim_end().to_string(),
        JeuEtat::PreGame => "sélection".into(),
        JeuEtat::Menus if !j.file.is_empty() => "en file".into(),
        JeuEtat::Menus => "au menu".into(),
        JeuEtat::Inconnu => "VALORANT".into(),
    }
}

/// Ce que les cases VALORANT d'une image ont besoin de savoir, lu une fois.
struct Valo<'a> {
    tier: Option<u8>,
    fiche: Option<&'a FicheValorant>,
    bilan: Option<&'a BilanMembre>,
    partie: Option<&'a JeuStatut>,
    /// Mes derniers matchs, du plus récent.
    matchs: Vec<&'a MatchResume>,
}

/// Ce que les places du salon d'une image ont besoin de savoir, lu une fois.
struct Places<'a> {
    /// En vocal : moi puis les autres ; la page de douze où est la
    /// personne choisie.
    grille: Vec<&'a Member>,
    decalage: usize,
    choisi: Option<UserId>,
    /// Hors vocal : les salons vocaux.
    salons: Vec<&'a ChannelInfo>,
}

impl KiApp {
    /// À chaque image : l'interrupteur, les commandes reçues, et ce que
    /// l'appareil doit montrer.
    pub(crate) fn tick_loupedeck(&mut self, ctx: &egui::Context, voice: &VoiceSnapshot) {
        let l = self.loupedeck.get_or_insert_with(|| Loupedeck::demarrer(ctx.clone()));
        l.activer(self.loupedeck_etat.actif);
        l.roles(self.loupedeck_etat.config.roles());
        l.luminosite(self.loupedeck_etat.config.luminosite);
        if !self.loupedeck_etat.actif || l.statut() != Statut::Pilote {
            return;
        }
        let commandes = l.commandes();
        for c in commandes {
            self.commande_loupedeck(c, ctx);
        }
        self.suivre_partie();
        let ecran = self.loupedeck_etat.ecran.min(self.loupedeck_etat.config.pages.len() - 1);
        self.loupedeck_etat.ecran = ecran;
        self.preparer_page(ctx, ecran);
        let affichage = self.affichage_page(ecran, voice);
        if let Some(l) = self.loupedeck.as_mut() {
            l.afficher(affichage);
        }
    }

    /// Ce qu'une page doit avoir sous la main avant d'être dessinée : les
    /// logos de rang, les derniers clips, les stats VALORANT et les
    /// portraits d'agents — téléchargés une fois par l'appli, lus sur le
    /// disque par le dessin.
    pub(crate) fn preparer_page(&mut self, ctx: &egui::Context, n: usize) {
        for m in &self.members {
            if let Some(tier) = palier_de(m) {
                self.rangs.preparer(ctx, tier);
            }
        }
        let maintenant = Instant::now();
        if self.loupedeck_etat.relances_stats.iter().any(|t| *t <= maintenant) {
            self.loupedeck_etat.relances_stats.retain(|t| *t > maintenant);
            if self.welcomed {
                self.loupedeck_etat.stats_demandees = Some(maintenant);
                self.send(ClientMsg::StatsValorant);
            }
        }
        let Some(page) = self.loupedeck_etat.config.pages.get(n) else { return };
        let (clips_voulus, valo_voulu) = (page.a_des_clips(), page.a_du_valorant());
        let sons_voulus = page.cases.iter().any(|c| matches!(c, Case::Bouton(Bouton { action: Action::Son(_), .. })))
            || self.loupedeck_etat.config.ronds.iter().any(|a| matches!(a, Action::Son(_)));
        if sons_voulus {
            self.soundboard.preparer();
        }
        if clips_voulus {
            if let Some(liste) = self.loupedeck_etat.clips_lus.lock().unwrap().take() {
                self.loupedeck_etat.clips = liste;
            }
            if self.loupedeck_etat.clips_releve.is_none_or(|t| t.elapsed() > RELIRE_CLIPS) {
                self.loupedeck_etat.clips_releve = Some(Instant::now());
                let dossier = self.clips_reglages.dossier_effectif();
                let lus = self.loupedeck_etat.clips_lus.clone();
                let ctx = ctx.clone();
                std::thread::Builder::new()
                    .name("ki-loupedeck-clips".into())
                    .spawn(move || {
                        let liste = clips::lister(&dossier).into_iter().take(CLIPS_MAX).collect();
                        *lus.lock().unwrap() = Some(liste);
                        ctx.request_repaint();
                    })
                    .ok();
            }
        }
        if valo_voulu {
            if self.welcomed
                && self.loupedeck_etat.stats_demandees.is_none_or(|t| t.elapsed() > REDEMANDER_STATS)
            {
                self.loupedeck_etat.stats_demandees = Some(Instant::now());
                self.send(ClientMsg::StatsValorant);
            }
            self.catalogue.demarrer();
            let agents: Vec<String> = self
                .stats
                .iter()
                .find(|f| Some(f.user_id) == self.my_id)
                .map(|f| f.fiche.matchs.iter().map(|m| m.agent.clone()).collect())
                .unwrap_or_default();
            for agent in agents {
                let _ = self.catalogue.agent(&agent);
            }
        }
    }

    /// Ma partie, image après image. Quand elle commence, la première page
    /// qui montre du VALORANT s'affiche toute seule (si on le veut) ; quand
    /// un match finit, le serveur relit mes stats chez HenrikDev dans les
    /// minutes qui suivent — on les lui redemande en chemin, pour que le RR
    /// du jour bouge sans attendre.
    fn suivre_partie(&mut self) {
        let etat = self.jeu_envoye.as_ref().filter(|j| j.est_valorant()).map(|j| j.etat);
        let avant = self.loupedeck_etat.partie;
        if etat == avant {
            return;
        }
        self.loupedeck_etat.partie = etat;
        let en_partie = |e: Option<JeuEtat>| matches!(e, Some(JeuEtat::PreGame | JeuEtat::EnJeu));
        let config = &self.loupedeck_etat.config;
        if en_partie(etat) && !en_partie(avant) && config.valo_auto {
            if let Some(n) = config.pages.iter().position(|p| p.a_du_valorant()) {
                self.loupedeck_etat.ecran = n;
            }
        }
        if avant == Some(JeuEtat::EnJeu) && !en_partie(etat) {
            let t = Instant::now();
            self.loupedeck_etat.relances_stats = [90, 240, 480].map(|s| t + Duration::from_secs(s)).to_vec();
        }
    }

    /// Ramène ki-chat devant, pour la fenêtre qu'une commande vient d'ouvrir
    /// — jamais en pleine partie : ce serait sortir du jeu.
    fn ramener_ki_chat(&mut self, ctx: &egui::Context) {
        let en_partie = self
            .jeu_envoye
            .as_ref()
            .is_some_and(|j| matches!(j.etat, JeuEtat::PreGame | JeuEtat::EnJeu));
        if !en_partie {
            self.rouvrir_depuis_zone(ctx);
        }
    }

    // -----------------------------------------------------------------
    // Les places du salon
    // -----------------------------------------------------------------

    /// En vocal : moi d'abord — dès l'entrée, sans attendre que le serveur
    /// m'y range —, puis les autres occupants dans l'ordre de la liste des
    /// membres. Sans le bot de musique : il a sa molette.
    fn loupedeck_grille(&self) -> Vec<&Member> {
        let Some(salon) = self.voice_channel else { return Vec::new() };
        let moi = self.members.iter().find(|m| Some(m.user_id) == self.my_id);
        moi.into_iter()
            .chain(self.members.iter().filter(|m| {
                m.voice == Some(salon) && Some(m.user_id) != self.my_id && !est_bot(m.user_id)
            }))
            .collect()
    }

    /// Ceux qu'on peut choisir pour régler leur volume : la grille sans moi.
    fn loupedeck_membres(&self) -> Vec<&Member> {
        let mut grille = self.loupedeck_grille();
        grille.retain(|m| Some(m.user_id) != self.my_id);
        grille
    }

    /// La personne choisie : celle qu'on a touchée si elle est encore là,
    /// sinon la première du salon.
    fn loupedeck_choisi(&self) -> Option<UserId> {
        let membres = self.loupedeck_membres();
        self.loupedeck_etat
            .choix
            .filter(|id| membres.iter().any(|m| m.user_id == *id))
            .or_else(|| membres.first().map(|m| m.user_id))
    }

    fn places(&self) -> Places<'_> {
        let grille = self.loupedeck_grille();
        let choisi = self.loupedeck_choisi();
        // Plus de douze en vocal : les places montrent la douzaine de la
        // personne choisie.
        let decalage = grille.iter().position(|m| Some(m.user_id) == choisi).map_or(0, |i| i / 12 * 12);
        let salons = self.channels.iter().filter(|c| c.kind == ChannelKind::Voice).collect();
        Places { grille, decalage, choisi, salons }
    }

    fn tuile_place(&self, n: usize, places: &Places, voice: &VoiceSnapshot) -> Tuile {
        if !self.welcomed {
            return if n == 0 { Tuile::Texte("hors ligne".into()) } else { Tuile::Vide };
        }
        if self.voice_channel.is_none() {
            return match places.salons.get(n) {
                Some(c) => Tuile::Salon {
                    nom: c.name.clone(),
                    occupants: self.members.iter().filter(|m| m.voice == Some(c.id)).count(),
                },
                None if n == 0 => Tuile::Texte("aucun salon vocal".into()),
                None => Tuile::Vide,
            };
        }
        let Some(m) = places.grille.get(places.decalage + n) else { return Tuile::Vide };
        // Pour moi, l'état local fait foi, comme dans la liste des membres :
        // l'écho du serveur peut avoir un aller de retard.
        let (parle, muet) = if Some(m.user_id) == self.my_id {
            (self.transmitting, self.muted || m.force_muted)
        } else {
            let niveau = voice.levels.get(&m.user_id).copied().unwrap_or(0.0);
            (m.speaking || niveau > SPEAK_LEVEL, m.muted || m.force_muted)
        };
        Tuile::Membre {
            nom: m.username.clone(),
            avatar: m.avatar.clone(),
            rang: palier_de(m),
            parle,
            muet,
            choisi: Some(m.user_id) == places.choisi,
        }
    }

    fn toucher_place(&mut self, n: usize) {
        if self.voice_channel.is_some() {
            // Ma propre place ne se choisit pas : mon volume ne se règle pas.
            let places = self.places();
            let touche = places.grille.get(places.decalage + n).map(|m| m.user_id);
            if let Some(id) = touche.filter(|id| Some(*id) != self.my_id) {
                self.loupedeck_etat.choix = Some(id);
            }
        } else if self.welcomed {
            let salon = self.channels.iter().filter(|c| c.kind == ChannelKind::Voice).nth(n).map(|c| c.id);
            if let Some(id) = salon {
                self.join_voice(id);
            }
        }
    }

    // -----------------------------------------------------------------
    // Le tableau de bord VALORANT
    // -----------------------------------------------------------------

    fn valo(&self) -> Valo<'_> {
        let moi = self.members.iter().find(|m| Some(m.user_id) == self.my_id);
        let membre = self.stats.iter().find(|f| Some(f.user_id) == self.my_id);
        let fiche = membre.map(|f| &f.fiche);
        let mut matchs: Vec<&MatchResume> = fiche.map(|f| f.matchs.iter().collect()).unwrap_or_default();
        matchs.sort_by_key(|m| std::cmp::Reverse(m.date));
        Valo {
            tier: moi.and_then(palier_de),
            fiche,
            bilan: membre.and_then(|f| f.bilan.as_ref()),
            partie: self.jeu_envoye.as_ref().filter(|j| j.est_valorant()),
            matchs,
        }
    }

    /// Un chiffre du tableau de bord. En direct (la présence du client
    /// Riot, toutes les deux secondes) : la partie, son score, sa carte.
    /// Après chaque match (la fiche que le serveur relit chez HenrikDev) :
    /// le reste. Riot ne dit rien des kills d'une partie en cours : son
    /// K/D/A arrive avec la fiche, une fois le match fini.
    fn tuile_valo(&self, w: Widget, v: &Valo) -> Tuile {
        if !self.welcomed {
            return if w == Widget::Rang { Tuile::Texte("hors ligne".into()) } else { Tuile::Vide };
        }
        let (gris, blanc, vert, rouge) = (rgb(TEXT_FAINT), rgb(theme::TEXT), rgb(SPEAK), rgb(theme::DANGER));
        let signe = |n: i32| match n.signum() {
            1 => vert,
            -1 => rouge,
            _ => blanc,
        };
        let chiffre = |titre: &str, valeur: String, detail: String, couleur| Tuile::Chiffre {
            titre: titre.into(),
            valeur,
            detail,
            couleur,
        };
        let sept = v.bilan.map(|b| &b.sept_jours).filter(|b| b.matchs > 0);
        let sans_sept = |titre: &str| {
            let detail = if v.fiche.is_some() { "pas de classé" } else { "chargement" };
            chiffre(titre, "—".into(), detail.into(), gris)
        };
        match w {
            Widget::Rang => {
                let rr = v.fiche.filter(|f| f.rang.tier >= 3).map(|f| {
                    if f.rang.delta != 0 {
                        format!("{} RR ({:+})", f.rang.rr, f.rang.delta)
                    } else {
                        format!("{} RR", f.rang.rr)
                    }
                });
                Tuile::Bouton {
                    icone: Some(Icon::User),
                    image: v.tier.map(Image::Rang),
                    titre: v.tier.map_or("Non classé".into(), ki_protocol::nom_de_rang),
                    detail: rr.unwrap_or_default(),
                    couleur: [255, 70, 85],
                    allume: false,
                }
            }
            Widget::Partie => {
                let carte = |j: &JeuStatut, defaut: &str| {
                    if j.carte.is_empty() { defaut.to_string() } else { j.carte.to_uppercase() }
                };
                match v.partie {
                    Some(j) if j.etat == JeuEtat::EnJeu => {
                        let ecart = j.score_allie as i32 - j.score_adverse as i32;
                        let score = format!("{}-{}", j.score_allie, j.score_adverse);
                        chiffre(&carte(j, "EN PARTIE"), score, j.libelle_file(), signe(ecart))
                    }
                    Some(j) if j.etat == JeuEtat::PreGame => {
                        chiffre(&carte(j, "PARTIE"), "—".into(), "choix de l'agent".into(), blanc)
                    }
                    Some(j) => chiffre("PARTIE", "—".into(), partie_courte(j), gris),
                    None if !self.valorant_presence => chiffre("PARTIE", "—".into(), "toucher : suivre".into(), gris),
                    None => chiffre("PARTIE", "—".into(), "VALORANT fermé".into(), gris),
                }
            }
            // Ma journée (depuis 6 h) : les RR, victoires et défaites. Les
            // points de RR comptent les classés ; à défaut, les matchs.
            Widget::Jour => match v.fiche {
                None => chiffre("AUJOURD'HUI", "…".into(), "chargement".into(), gris),
                Some(f) => {
                    let debut = debut_de_journee_ms();
                    let points: Vec<i32> =
                        f.historique_rr.iter().filter(|p| p.date >= debut).map(|p| p.delta).collect();
                    let (vi, de) = if points.is_empty() {
                        let du_jour = || f.matchs.iter().filter(|m| m.date >= debut);
                        (
                            du_jour().filter(|m| m.gagne == Some(true)).count(),
                            du_jour().filter(|m| m.gagne == Some(false)).count(),
                        )
                    } else {
                        (points.iter().filter(|r| **r > 0).count(), points.iter().filter(|r| **r < 0).count())
                    };
                    let rr: i32 = points.iter().sum();
                    if vi + de == 0 {
                        chiffre("AUJOURD'HUI", "0".into(), "pas encore joué".into(), gris)
                    } else if points.is_empty() {
                        chiffre("AUJOURD'HUI", format!("{vi}-{de}"), "victoires-défaites".into(), signe(vi as i32 - de as i32))
                    } else {
                        let valeur = if rr == 0 { "0".into() } else { format!("{rr:+}") };
                        chiffre("AUJOURD'HUI", valeur, format!("{vi} V · {de} D"), signe(rr))
                    }
                }
            },
            Widget::Forme => {
                let forme = v.bilan.map(|b| b.forme.clone()).or_else(|| v.fiche.map(|f| f.forme(10))).unwrap_or_default();
                let serie = v.bilan.map(|b| b.serie).or_else(|| v.fiche.map(|f| f.serie())).unwrap_or(0);
                Tuile::Forme {
                    titre: "FORME".into(),
                    resultats: forme,
                    detail: match serie {
                        s if s >= 2 => format!("série : {s} V"),
                        s if s <= -2 => format!("série : {} D", -s),
                        _ => "10 derniers".into(),
                    },
                }
            }
            Widget::Kd => match sept {
                None => sans_sept("K/D · 7 J"),
                Some(b) => {
                    let n = b.matchs as f32;
                    let kd = b.kills as f32 / b.deaths.max(1) as f32;
                    let par_match = |x: u32| (x as f32 / n).round() as u32;
                    chiffre(
                        "K/D · 7 J",
                        format!("{kd:.2}").replace('.', ","),
                        format!("{} / {} / {}", par_match(b.kills), par_match(b.deaths), par_match(b.assists)),
                        if kd >= 1.0 { vert } else { rouge },
                    )
                }
            },
            Widget::Tete => match sept {
                Some(b) if b.tirs > 0 => {
                    let tete = b.tetes as f32 * 100.0 / b.tirs as f32;
                    chiffre("TÊTE · 7 J", format!("{tete:.0}%"), "des tirs".into(), blanc)
                }
                _ => sans_sept("TÊTE · 7 J"),
            },
            Widget::Victoires => match sept {
                None => sans_sept("VICTOIRES"),
                Some(b) => {
                    let victoires = b.victoires as f32 * 100.0 / b.matchs as f32;
                    chiffre(
                        "VICTOIRES",
                        format!("{victoires:.0}%"),
                        format!("{} V · {} D", b.victoires, b.defaites),
                        if victoires >= 50.0 { vert } else { rouge },
                    )
                }
            },
            Widget::Adr => match sept {
                Some(b) if b.manches_degats > 0 => {
                    let adr = b.degats as f32 / b.manches_degats as f32;
                    chiffre("ADR · 7 J", format!("{adr:.0}"), "dégâts / manche".into(), blanc)
                }
                Some(b) => chiffre("RR · 7 J", format!("{:+}", b.rr), format!("{} matchs", b.matchs), signe(b.rr)),
                None => sans_sept("ADR · 7 J"),
            },
            Widget::Match(n) => match v.matchs.get(n as usize) {
                None => Tuile::Vide,
                Some(m) => {
                    // Un mode sans manches (combat à mort) : son nom à la place.
                    let score = if m.manches == (0, 0) {
                        m.mode.clone()
                    } else {
                        format!("{}-{}", m.manches.0, m.manches.1)
                    };
                    Tuile::Match {
                        victoire: m.gagne,
                        score,
                        kda: format!("{}/{}/{}", m.kills, m.deaths, m.assists),
                        agent: valo_catalogue::fichier_agent(&m.agent).map(Image::Fichier),
                    }
                }
            },
        }
    }

    /// Mon rang ouvre ma fiche ; la partie, sans activité partagée, la
    /// partage — c'est elle qui donne le score en direct ; le reste ouvre
    /// la page des stats du groupe.
    fn toucher_valo(&mut self, w: Widget, ctx: &egui::Context) {
        if !self.welcomed {
            return;
        }
        match w {
            Widget::Rang => self.executer(&Action::Fiche, ctx),
            Widget::Partie if !self.valorant_presence => self.valorant_presence = true,
            Widget::Partie => {}
            _ => self.executer(&Action::Stats, ctx),
        }
    }

    // -----------------------------------------------------------------
    // Les clips récents
    // -----------------------------------------------------------------

    fn tuile_clip(&self, n: usize) -> Tuile {
        let Some(c) = self.loupedeck_etat.clips.get(n) else {
            return if n == 0 { Tuile::Texte("aucun clip pour l'instant".into()) } else { Tuile::Vide };
        };
        let detail = match &c.fiche {
            Some(f) if !f.source.is_empty() => f.source.clone(),
            Some(f) if f.duree_s > 0.0 => format!("{:.0} s", f.duree_s),
            _ => c.nom.clone(),
        };
        Tuile::Bouton {
            icone: Some(Icon::Film),
            image: c.vignette.clone().map(Image::Fichier),
            titre: age(c.modifie),
            detail,
            couleur: Action::Galerie.couleur(),
            allume: false,
        }
    }

    fn toucher_clip(&mut self, n: usize, ctx: &egui::Context) {
        let Some(clip) = self.loupedeck_etat.clips.get(n).cloned() else { return };
        if self.loupedeck_etat.config.toucher_partage {
            self.ouvrir_partage_clip(clip);
        } else {
            let liste: Vec<visionneuse::Cible> =
                self.loupedeck_etat.clips.iter().map(|c| visionneuse::Cible::Fichier(c.chemin.clone())).collect();
            self.visionneuse.ouvrir(visionneuse::Cible::Fichier(clip.chemin), liste);
        }
        self.ramener_ki_chat(ctx);
    }

    // -----------------------------------------------------------------
    // Les actions
    // -----------------------------------------------------------------

    /// Fait une action — d'un bouton rond ou d'une touche. Le push-to-talk
    /// ne passe pas par ici (le fil le tient), ni le clip d'un rond quand
    /// l'enregistreur tourne (le fil le déclenche).
    fn executer(&mut self, action: &Action, ctx: &egui::Context) {
        let en_vocal = self.voice_channel.is_some();
        match action {
            Action::Rien | Action::Ptt => {}
            // Comme les raccourcis : hors vocal, micro et écoute n'ont pas
            // de sens.
            Action::Micro if en_vocal => self.basculer_micro(),
            Action::Sourd if en_vocal => self.basculer_sourd(),
            Action::Micro | Action::Sourd => {}
            Action::Clip => self.sauver_clip(),
            Action::Partage => {
                if !en_vocal {
                    self.info = Some("rejoins un salon vocal pour diffuser ton écran".into());
                } else if self.go_live.is_some() {
                    self.arreter_diffusion();
                } else if self.go_live_attente.is_none() {
                    // Avec les réglages de la dernière diffusion : la source
                    // et la qualité se choisissent dans la fenêtre.
                    self.demarrer_diffusion();
                }
            }
            Action::Changeur => self.basculer_changeur(),
            Action::Enregistreur => {
                if self.enregistreur.is_some() {
                    self.arreter_clips();
                } else {
                    self.demarrer_clips();
                }
            }
            Action::QuitterVocal => self.leave_voice(),
            Action::Page(n) => {
                if *n < self.loupedeck_etat.config.pages.len() {
                    self.loupedeck_etat.ecran = *n;
                    // En arrivant, des données fraîches.
                    self.loupedeck_etat.clips_releve = None;
                    self.loupedeck_etat.stats_demandees = None;
                }
            }
            Action::Salon(nom) => {
                let salon = self.channels.iter().find(|c| c.kind == ChannelKind::Voice && &c.name == nom).map(|c| c.id);
                match salon {
                    Some(id) => self.join_voice(id),
                    None => self.info = Some(format!("pas de salon vocal « {nom} » sur ce serveur")),
                }
            }
            Action::Son(nom) => self.jouer_son(nom),
            Action::Stats => {
                if self.welcomed {
                    self.ouvrir_stats();
                    self.ramener_ki_chat(ctx);
                }
            }
            Action::Fiche => {
                let moi = self.members.iter().find(|m| Some(m.user_id) == self.my_id);
                if let Some((id, nom)) = moi.map(|m| (m.user_id, m.username.clone())) {
                    self.ouvrir_fiche(id, nom);
                    self.ramener_ki_chat(ctx);
                }
            }
            Action::Galerie => {
                self.ouvrir_clips();
                self.ramener_ki_chat(ctx);
            }
            Action::DureeClip => {
                // La durée suivante ; l'enregistreur en marche repart (le
                // tampon en cours est perdu, comme depuis les réglages).
                let avant = self.clips_reglages.clone();
                let i = clips::DUREES.iter().position(|d| *d == avant.duree_s).map_or(0, |i| i + 1);
                self.clips_reglages.duree_s = clips::DUREES[i % clips::DUREES.len()];
                if self.enregistreur.is_some() && self.clips_reglages.relance_necessaire(&avant) {
                    self.arreter_clips();
                    self.demarrer_clips();
                }
            }
            Action::ToucherClip => {
                self.loupedeck_etat.config.toucher_partage = !self.loupedeck_etat.config.toucher_partage;
            }
        }
    }

    /// Un son de la soundboard, pour tout le salon vocal.
    fn jouer_son(&mut self, nom: &str) {
        self.soundboard.preparer();
        let Some(pcm) = self.soundboard.sons.iter().find(|s| s.nom == nom).map(|s| s.pcm.clone()) else {
            self.info = Some(format!("pas de son « {nom} » dans la soundboard"));
            return;
        };
        if self.voice_channel.is_none() {
            self.info = Some("rejoins un salon vocal pour jouer un son".into());
            return;
        }
        match self.link.engine.lock().unwrap().as_ref() {
            Some(engine) => engine.soundboard_push(&pcm, self.soundboard.volume),
            None => self.info = Some("pas de moteur vocal : connecte-toi d'abord".into()),
        }
    }

    /// Ce que dit la lumière d'une action, rond ou touche.
    fn lumiere(&self, action: &Action) -> Lumiere {
        use Lumiere::*;
        let en_vocal = self.voice_channel.is_some();
        let selon = |allumee: bool| if allumee { Allumee } else { Veille };
        match action {
            Action::Rien => Eteinte,
            Action::Micro if !en_vocal => Eteinte,
            Action::Micro if self.muted => Alerte,
            Action::Micro => selon(self.transmitting),
            Action::Sourd if !en_vocal => Eteinte,
            Action::Sourd => if self.sourd { Alerte } else { Veille },
            Action::Ptt if !en_vocal || self.mode != MicMode::Ptt => Eteinte,
            Action::Ptt => selon(self.armed),
            // Les clips marchent aussi hors vocal.
            Action::Clip => if self.enregistreur.is_some() { Veille } else { Eteinte },
            Action::Partage if !en_vocal => Eteinte,
            Action::Partage if self.go_live.is_some() => Alerte,
            Action::Partage => selon(self.go_live_attente.is_some()),
            Action::Changeur => selon(self.changeur_actif),
            Action::Enregistreur => selon(self.enregistreur.is_some()),
            Action::QuitterVocal => if en_vocal { Veille } else { Eteinte },
            Action::Page(n) => selon(self.loupedeck_etat.ecran == *n),
            Action::Salon(nom) => {
                match self.channels.iter().find(|c| c.kind == ChannelKind::Voice && &c.name == nom) {
                    Some(c) => selon(self.voice_channel == Some(c.id)),
                    None => Eteinte,
                }
            }
            Action::Son(_) => if en_vocal { Veille } else { Eteinte },
            Action::Stats | Action::Fiche | Action::Galerie | Action::DureeClip | Action::ToucherClip => Veille,
        }
    }

    /// La lumière d'un bouton rond : la couleur choisie (sinon celle de
    /// l'action), en veilleuse au repos — et rouge pour une alerte.
    fn lumiere_rond(&self, i: usize) -> [u8; 3] {
        let config = &self.loupedeck_etat.config;
        let action = &config.ronds[i];
        let couleur = config.couleurs_ronds[i].unwrap_or(action.couleur());
        match self.lumiere(action) {
            Lumiere::Eteinte => [0, 0, 0],
            Lumiere::Veille => attenuer(couleur, 0.22),
            Lumiere::Allumee => couleur,
            Lumiere::Alerte => [255, 0, 0],
        }
    }

    /// Le titre d'origine d'une touche-bouton, et sa ligne d'état.
    fn textes_action(&self, action: &Action) -> (String, String) {
        let en_vocal = self.voice_channel.is_some();
        let hors_vocal = || "hors vocal".to_string();
        let pages = &self.loupedeck_etat.config.pages;
        let duree = self.clips_reglages.duree_s;
        let titre = action.court(pages);
        let detail = match action {
            Action::Rien | Action::Ptt => String::new(),
            Action::Micro if !en_vocal => hors_vocal(),
            Action::Micro if self.muted => "coupé".into(),
            Action::Micro if self.transmitting => "tu parles".into(),
            Action::Micro => "ouvert".into(),
            Action::Sourd if !en_vocal => hors_vocal(),
            Action::Sourd if self.sourd => "tu n'entends rien".into(),
            Action::Sourd => "tu entends".into(),
            Action::Clip if self.enregistreur.is_some() => format!("les {duree} dernières s"),
            Action::Clip => "enregistreur arrêté".into(),
            Action::Partage if !en_vocal => hors_vocal(),
            Action::Partage if self.go_live.is_some() => "en direct".into(),
            Action::Partage if self.go_live_attente.is_some() => "démarrage…".into(),
            Action::Partage => "toucher : diffuser".into(),
            Action::Changeur => if self.changeur_actif { "allumé" } else { "coupé" }.into(),
            Action::Enregistreur if self.enregistreur.is_some() => "en marche".into(),
            Action::Enregistreur => "arrêté".into(),
            Action::QuitterVocal => self.voice_channel_name().map(str::to_string).unwrap_or_else(hors_vocal),
            Action::Page(_) => "page".into(),
            Action::Salon(nom) => match self.channels.iter().find(|c| c.kind == ChannelKind::Voice && &c.name == nom) {
                Some(c) => match self.members.iter().filter(|m| m.voice == Some(c.id)).count() {
                    0 => "vide".into(),
                    n => format!("{n} en vocal"),
                },
                None => "introuvable".into(),
            },
            Action::Son(nom) => match self.soundboard.sons.iter().find(|s| &s.nom == nom) {
                Some(s) => format!("{:.1} s", s.duree_s()).replace('.', ","),
                None => "soundboard".into(),
            },
            Action::Stats => "du groupe".into(),
            Action::Fiche => "VALORANT".into(),
            Action::Galerie => "galerie".into(),
            Action::DureeClip => "durée du clip".into(),
            Action::ToucherClip => "au toucher".into(),
        };
        // Deux actions disent leur état dans leur titre.
        let titre = match action {
            Action::DureeClip => format!("{duree} s"),
            Action::ToucherClip if self.loupedeck_etat.config.toucher_partage => "Partager".into(),
            Action::ToucherClip => "Lire".into(),
            _ => titre,
        };
        (titre, detail)
    }

    fn tuile_bouton(&self, b: &Bouton) -> Tuile {
        let (titre, detail) = self.textes_action(&b.action);
        let titre = if b.texte.trim().is_empty() { titre } else { b.texte.clone() };
        let couleur = b.couleur.unwrap_or(b.action.couleur());
        let (couleur, allume) = match self.lumiere(&b.action) {
            Lumiere::Eteinte => (rgb(TEXT_FAINT), false),
            Lumiere::Veille => (couleur, false),
            Lumiere::Allumee => (couleur, true),
            Lumiere::Alerte => (rgb(theme::DANGER), true),
        };
        Tuile::Bouton {
            icone: Some(b.icone.unwrap_or(b.action.icone())),
            image: None,
            titre,
            detail,
            couleur,
            allume,
        }
    }

    // -----------------------------------------------------------------
    // Les commandes de l'appareil
    // -----------------------------------------------------------------

    fn commande_loupedeck(&mut self, c: Commande, ctx: &egui::Context) {
        match c {
            Commande::Rond(i) => {
                if let Some(action) = self.loupedeck_etat.config.ronds.get(i as usize).cloned() {
                    self.executer(&action, ctx);
                }
            }
            Commande::Molette { molette, crans } => {
                if let Some(action) = self.loupedeck_etat.config.molettes.get(molette as usize).copied() {
                    self.molette_loupedeck(action, crans as f32);
                }
            }
            Commande::AppuiMolette(m) => {
                if let Some(action) = self.loupedeck_etat.config.molettes.get(m as usize).copied() {
                    self.appui_molette(action);
                }
            }
            Commande::Touche(t) => {
                let page = self.loupedeck_etat.ecran;
                let case = self.loupedeck_etat.config.pages.get(page).and_then(|p| p.cases.get(t as usize)).cloned();
                match case {
                    None | Some(Case::Vide) => {}
                    Some(Case::Vocal(n)) => self.toucher_place(n as usize),
                    Some(Case::Valo(w)) => self.toucher_valo(w, ctx),
                    Some(Case::Clip(n)) => self.toucher_clip(n as usize, ctx),
                    Some(Case::Bouton(b)) => self.executer(&b.action, ctx),
                }
            }
            Commande::Glisse(dy) => {
                // Toute la hauteur de la bande : 100 %. Pas d'arrondi au
                // cran, un glissé lent avance de moins de 5 % à la fois.
                self.output_gain = (self.output_gain + dy / ki_loupedeck::HAUTEUR as f32).clamp(0.0, 2.0);
                self.apply_audio_settings();
            }
        }
    }

    fn molette_loupedeck(&mut self, action: ActionMolette, crans: f32) {
        match action {
            ActionMolette::Rien => {}
            ActionMolette::Volume => {
                self.output_gain = cran(self.output_gain, crans);
                self.apply_audio_settings();
            }
            ActionMolette::Micro => {
                self.input_gain = cran(self.input_gain, crans);
                self.apply_audio_settings();
            }
            ActionMolette::Choix => {
                let membres: Vec<UserId> = self.loupedeck_membres().iter().map(|m| m.user_id).collect();
                if membres.is_empty() {
                    return;
                }
                let choisi = self.loupedeck_choisi();
                let i = membres.iter().position(|id| Some(*id) == choisi).unwrap_or(0) as i64;
                let j = (i + crans as i64).rem_euclid(membres.len() as i64) as usize;
                self.loupedeck_etat.choix = Some(membres[j]);
            }
            ActionMolette::VolumeChoisi => {
                if let Some(id) = self.loupedeck_choisi() {
                    let v = cran(self.volume_of(id), crans);
                    self.set_volume(id, v);
                }
            }
            ActionMolette::Musique => {
                let v = cran(self.volume_of(ki_protocol::MUSIQUE_ID), crans);
                self.set_volume(ki_protocol::MUSIQUE_ID, v);
            }
            // Les sons de ki-chat vont de 0 à 100 %.
            ActionMolette::Sons => self.sfx_volume = cran(self.sfx_volume, crans).min(1.0),
        }
    }

    /// Une molette enfoncée : sa valeur d'origine.
    fn appui_molette(&mut self, action: ActionMolette) {
        match action {
            ActionMolette::Rien => {}
            ActionMolette::Volume => {
                self.output_gain = 1.0;
                self.apply_audio_settings();
            }
            ActionMolette::Micro => {
                self.input_gain = 1.0;
                self.apply_audio_settings();
            }
            ActionMolette::Choix => self.loupedeck_etat.choix = None,
            ActionMolette::VolumeChoisi => {
                if let Some(id) = self.loupedeck_choisi() {
                    self.set_volume(id, 1.0);
                }
            }
            ActionMolette::Musique => self.set_volume(ki_protocol::MUSIQUE_ID, 1.0),
            ActionMolette::Sons => self.sfx_volume = 1.0,
        }
    }

    /// La case de la bande en face d'une molette.
    fn jauge(&self, action: ActionMolette) -> Jauge {
        let en_vocal = self.voice_channel.is_some();
        let volume = |titre: &str, v: f32, max: f32, actif: bool| Jauge {
            titre: titre.into(),
            valeur: pct(v),
            fraction: Some(v / max),
            actif,
        };
        match action {
            ActionMolette::Rien => Jauge::default(),
            ActionMolette::Volume if self.sourd => Jauge {
                titre: "VOLUME".into(),
                valeur: "sourd".into(),
                fraction: Some(self.output_gain / 2.0),
                actif: false,
            },
            ActionMolette::Volume => volume("VOLUME", self.output_gain, 2.0, true),
            ActionMolette::Micro => volume("MICRO", self.input_gain, 2.0, !(en_vocal && self.muted)),
            ActionMolette::Choix => {
                let membres = self.loupedeck_membres();
                let choisi = self.loupedeck_choisi();
                let position = membres.iter().position(|m| Some(m.user_id) == choisi);
                Jauge {
                    titre: "CHOIX".into(),
                    valeur: position.map_or("—".into(), |i| format!("{}/{}", i + 1, membres.len())),
                    fraction: None,
                    actif: position.is_some(),
                }
            }
            ActionMolette::VolumeChoisi => {
                let choisi = self.loupedeck_choisi();
                let nom = choisi
                    .and_then(|id| self.members.iter().find(|m| m.user_id == id))
                    .map(|m| m.username.clone());
                match (choisi, nom) {
                    (Some(id), Some(nom)) => volume(&nom, self.volume_of(id), 2.0, true),
                    _ => Jauge { titre: "VOLUME".into(), valeur: "—".into(), fraction: None, actif: false },
                }
            }
            ActionMolette::Musique => volume("MUSIQUE", self.volume_of(ki_protocol::MUSIQUE_ID), 2.0, true),
            ActionMolette::Sons => volume("SONS", self.sfx_volume, 1.0, true),
        }
    }

    /// Ce que montre l'appareil quand il affiche la page `n` — pour lui, et
    /// pour l'aperçu de la page Loupedeck.
    pub(crate) fn affichage_page(&self, n: usize, voice: &VoiceSnapshot) -> Affichage {
        let config = &self.loupedeck_etat.config;
        let mut a = Affichage {
            fond: config.fond,
            accent: config.accent,
            ronds: std::array::from_fn(|i| self.lumiere_rond(i)),
            ..Default::default()
        };
        if let Some(page) = config.pages.get(n) {
            let places = page.cases.iter().any(|c| matches!(c, Case::Vocal(_))).then(|| self.places());
            let valo = page.a_du_valorant().then(|| self.valo());
            for (t, case) in page.cases.iter().enumerate() {
                a.tuiles[t] = match case {
                    Case::Vide => Tuile::Vide,
                    Case::Vocal(k) => places.as_ref().map_or(Tuile::Vide, |p| self.tuile_place(*k as usize, p, voice)),
                    Case::Valo(w) => valo.as_ref().map_or(Tuile::Vide, |v| self.tuile_valo(*w, v)),
                    Case::Clip(k) => self.tuile_clip(*k as usize),
                    Case::Bouton(b) => self.tuile_bouton(b),
                };
            }
        }
        let m = config.molettes;
        a.gauche = [self.jauge(m[0]), self.jauge(m[1]), self.jauge(m[2])];
        a.droite = [self.jauge(m[3]), self.jauge(m[4]), self.jauge(m[5])];
        a
    }

    // -----------------------------------------------------------------
    // Les réglages
    // -----------------------------------------------------------------

    /// La ligne « Loupedeck » de l'onglet Bêta : l'interrupteur, l'état de
    /// l'appareil, et le chemin vers sa page.
    pub(crate) fn loupedeck_ui(&mut self, ui: &mut egui::Ui) {
        ui::interrupteur(ui, &mut self.loupedeck_etat.actif, "Activer le Loupedeck Live");
        if self.loupedeck_etat.actif {
            let (texte, couleur, detail) = self.statut_loupedeck();
            let r = ui.label(RichText::new(texte).color(couleur).size(11.5));
            if let Some(e) = detail {
                r.on_hover_text(e);
            }
            if ui::button(ui, Icon::Sliders, "Régler ses boutons et ses écrans").clicked() {
                self.loupedeck_etat.page_ouverte = true;
            }
        }
        ui::precision(
            ui,
            "Coupé (d'origine), ki-chat ne touche pas à l'appareil, et rien n'en apparaît ailleurs. \
             Allumé, ki-chat le pilote — le logiciel Loupedeck doit être fermé — et un bouton \
             « Loupedeck » apparaît dans la barre du bas : il ouvre la page où l'on règle chaque \
             bouton, chaque molette et chaque touche.",
        );
    }

    /// L'état de l'appareil en une phrase, sa couleur, et le détail d'une
    /// erreur au survol.
    pub(crate) fn statut_loupedeck(&self) -> (String, egui::Color32, Option<String>) {
        if !self.loupedeck_etat.actif {
            return ("pilotage coupé".into(), TEXT_FAINT, None);
        }
        match self.loupedeck.as_ref().map(|l| l.statut()) {
            Some(Statut::Pilote) => ("branché et piloté par ki-chat".into(), SPEAK, None),
            Some(Statut::Occupe(e)) => (
                "branché mais occupé : ferme le logiciel Loupedeck (ou Logi Options+), ki-chat le \
                 prendra tout seul"
                    .into(),
                WARN,
                Some(e),
            ),
            _ => ("aucun Loupedeck branché".into(), TEXT_FAINT, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crans() {
        assert_eq!(pct(cran(1.0, 1.0)), "105%");
        assert_eq!(pct(cran(0.333, -1.0)), "30%");
        assert_eq!(cran(1.98, 3.0), 2.0);
        assert_eq!(cran(0.02, -5.0), 0.0);
        let mut v = 1.0;
        for _ in 0..7 {
            v = cran(v, 1.0);
        }
        assert_eq!(pct(v), "135%");
    }

    #[test]
    fn parties() {
        let mut j = JeuStatut { etat: JeuEtat::EnJeu, score_allie: 7, score_adverse: 5, ..Default::default() };
        j.carte = "Ascent".into();
        assert_eq!(partie_courte(&j), "7-5 Ascent");
        j.carte.clear();
        assert_eq!(partie_courte(&j), "7-5");
        j.etat = JeuEtat::Menus;
        assert_eq!(partie_courte(&j), "au menu");
        j.file = "competitive".into();
        assert_eq!(partie_courte(&j), "en file");
    }
}
