//! La table de cartes, côté client : la fenêtre « Dernière carte ».
//!
//! Le serveur décide de tout (`ki_protocol::TableCartes`) : la fenêtre
//! montre la table vue de ma place et renvoie mes gestes. Ce qu'elle grise,
//! c'est ce que le serveur refuserait — la règle est la même, dans le
//! protocole.
//!
//! Les cartes sont dessinées, pas chargées : un fond de couleur, un cadre,
//! la valeur au milieu et dans les coins, et le dos aux couleurs de ki-chat.
//! Ce qui arrive à la table s'anime (voir « Les animations ») : les cartes
//! volent, « Dernière carte ! » éclate, le contre secoue.
//!
//! Le module ne dépend que d'egui, de ki-ui et du protocole : le banc
//! `examples/banc_cartes.rs` le photographie sans ki-chat autour.

use std::collections::HashMap;
use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, RichText, Sense, Shape, Stroke,
    StrokeKind, Vec2,
};
use ki_protocol::{
    ActionCartes, Carte, CouleurCarte, EvenementCartes, JoueurCartes, PhaseCartes, ReglesCartes, TableCartes,
    UserId, ValeurCarte, MAX_JOUEURS_CARTES, PENALITE_ANNONCE, TOUR_CARTES_S,
};
use ki_ui::composants::{self as ui, Tone};
use ki_ui::icones::{self, Icon};
use ki_ui::jetons::{couleur as c, espace, rayon, texte};

/// Largeur d'une carte pour sa hauteur.
const RATIO: f32 = 0.68;
/// Ce qu'un refus du serveur reste à l'écran.
const REFUS_VISIBLE: Duration = Duration::from_secs(4);
/// La cadence des halos qui battent : trente images par seconde suffisent à
/// un pouls, un écran à 280 Hz n'a pas à tourner à plein pour lui.
const POULS: Duration = Duration::from_millis(33);

/// De quoi nommer et reconnaître quelqu'un à la table.
pub struct Identite {
    pub nom: String,
    pub couleur: Color32,
    pub photo: Option<egui::TextureHandle>,
}

/// Ce qui mérite un son : le client choisit lequel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Son {
    MonTour,
    FinDeManche,
    /// Quelqu'un dit « Dernière carte ! ».
    Annonce,
    /// Quelqu'un se fait contrer.
    Contre,
    /// Des mains changent de place : un 7 qui échange, un 0 qui tourne.
    Echange,
}

#[derive(Default)]
pub struct Cartes {
    pub ouvert: bool,
    table: Option<TableCartes>,
    recue: Option<Instant>,
    /// Le joker posé qui attend sa couleur.
    choix_couleur: Option<Carte>,
    /// Le 7 posé qui attend avec qui échanger.
    choix_cible: Option<Carte>,
    /// Le serveur connaît les règles en plus (protocole 2) : sans lui,
    /// l'hôte n'a rien à régler.
    pub regles_disponibles: bool,
    refus: Option<(String, Instant)>,
    /// Les effets en cours : cartes qui volent, annonce, contre, victoire.
    anims: Vec<Anim>,
    /// Où sont les choses à l'image d'avant.
    reperes: Reperes,
    /// Quand la table a été peinte pour la dernière fois.
    peinte_le: Option<Instant>,
}

impl Cartes {
    /// La table de mon salon, s'il y en a une.
    pub fn table(&self) -> Option<&TableCartes> {
        self.table.as_ref()
    }

    /// Déconnecté : plus de table, plus de fenêtre.
    pub fn effacer(&mut self) {
        *self = Self::default();
    }

    /// À moi de jouer.
    pub fn mon_tour(&self, moi: Option<UserId>) -> bool {
        self.table.as_ref().is_some_and(|t| a_moi_de_jouer(t, moi))
    }

    /// Un bot joue ma main : trois tours manqués, ou j'ai abandonné.
    pub fn ma_place_au_bot(&self, moi: Option<UserId>) -> bool {
        self.table.as_ref().is_some_and(|t| {
            t.phase == PhaseCartes::EnCours && moi.is_some_and(|m| t.joueur(m).is_some_and(|j| j.bot))
        })
    }

    /// Une manche en cours où je ne suis pas assis, et qu'un bot tient une
    /// place : je peux la prendre, cartes comprises.
    pub fn place_libre(&self, moi: Option<UserId>) -> bool {
        self.table.as_ref().is_some_and(|t| {
            t.phase == PhaseCartes::EnCours
                && !t.en_attente
                && moi.is_some_and(|m| t.joueur(m).is_none())
                && t.joueurs.iter().any(|j| j.bot)
        })
    }

    /// La table, telle que le serveur vient de l'envoyer.
    pub fn recevoir(&mut self, table: Option<TableCartes>, moi: Option<UserId>) -> Vec<Son> {
        let avant = self.table.take();
        let mut sons = Vec::new();
        match &table {
            Some(t) => {
                // Une manche distribuée à ma place, ou une place qui me
                // revient (reprise, ou retrouvée en rentrant dans le salon) :
                // la fenêtre s'ouvre — avant les effets, pour que la donne
                // qui l'ouvre s'anime.
                let assis = |table: &TableCartes| moi.is_some_and(|m| table.joueur(m).is_some_and(|j| !j.bot));
                let nouvelle = avant.as_ref().is_none_or(|a| a.manche != t.manche || !assis(a));
                if assis(t) && t.phase == PhaseCartes::EnCours && nouvelle {
                    self.ouvert = true;
                }
                // Une autre table (j'ai changé de salon), ou un fil qui
                // recule : ce n'est pas une suite, rien à rejouer.
                let meme_table = avant.as_ref().filter(|a| a.salon == t.salon && t.numero >= a.numero);
                if meme_table.is_none() {
                    self.anims.clear();
                }
                if let Some(a) = meme_table {
                    let neufs = (t.numero.saturating_sub(a.numero) as usize).min(t.journal.len());
                    let neufs = &t.journal[t.journal.len() - neufs..];
                    for e in neufs {
                        match e {
                            EvenementCartes::Gagne { .. } => sons.push(Son::FinDeManche),
                            EvenementCartes::Annonce { .. } => sons.push(Son::Annonce),
                            EvenementCartes::Contre { .. } => sons.push(Son::Contre),
                            EvenementCartes::Echange { .. } | EvenementCartes::Tourne { .. } => {
                                sons.push(Son::Echange)
                            }
                            _ => {}
                        }
                    }
                    // Les effets ne se jouent que fenêtre ouverte : ouverte
                    // plus tard, elle montre l'état, pas un rattrapage.
                    if self.ouvert {
                        let now = Instant::now();
                        for (effet, retard, duree) in effets_de(neufs, a, t, moi, &self.reperes.main) {
                            self.anims.push(Anim { effet, debut: now + Duration::from_secs_f32(retard), duree });
                        }
                        if self.anims.len() > 300 {
                            let trop = self.anims.len() - 300;
                            self.anims.drain(..trop);
                        }
                    }
                }
                let a_moi = a_moi_de_jouer(t, moi);
                let avait = avant.as_ref().is_some_and(|a| a_moi_de_jouer(a, moi) && a.manche == t.manche);
                if a_moi && !avait {
                    sons.push(Son::MonTour);
                }
                if self.choix_couleur.is_some_and(|j| !a_moi || !t.main.contains(&j)) {
                    self.choix_couleur = None;
                }
                if self.choix_cible.is_some_and(|k| !a_moi || !t.main.contains(&k)) {
                    self.choix_cible = None;
                }
            }
            None => {
                self.choix_couleur = None;
                self.choix_cible = None;
                self.anims.clear();
            }
        }
        self.table = table;
        self.recue = Some(Instant::now());
        sons
    }

    /// Un geste refusé par le serveur.
    pub fn refuser(&mut self, message: String) {
        self.refus = Some((message, Instant::now()));
    }

    /// Le temps qui reste au joueur du tour, en millisecondes.
    fn reste_ms(&self) -> u32 {
        let (Some(t), Some(recue)) = (&self.table, self.recue) else { return 0 };
        t.reste_ms.saturating_sub(recue.elapsed().as_millis().min(u32::MAX as u128) as u32)
    }

    /// La fenêtre, si elle est ouverte ; renvoie mes gestes.
    pub fn fenetre(
        &mut self,
        ctx: &egui::Context,
        moi: Option<UserId>,
        en_vocal: bool,
        qui: &dyn Fn(UserId) -> Identite,
    ) -> Vec<ActionCartes> {
        let mut actions = Vec::new();
        if !self.ouvert {
            return actions;
        }
        let ecran = ctx.content_rect();
        let (max_l, max_h) = ((ecran.width() - 40.0).max(360.0), (ecran.height() - 40.0).max(300.0));
        let mut ouvert = true;
        egui::Window::new("Dernière carte")
            .id(egui::Id::new("table_cartes"))
            .open(&mut ouvert)
            .collapsible(false)
            .resizable(true)
            .default_size([920.0_f32.min(max_l), 640.0_f32.min(max_h)])
            .min_size([660.0_f32.min(max_l), 560.0_f32.min(max_h)])
            .max_size([max_l, max_h])
            .show(ctx, |ui| self.contenu(ui, moi, en_vocal, qui, &mut actions));
        self.ouvert = ouvert;
        if self.table.as_ref().is_some_and(|t| t.phase == PhaseCartes::EnCours) {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        actions
    }

    /// Le contenu de la fenêtre, dans toute la place qu'elle offre — et
    /// pas un pixel de plus : une fenêtre egui suit son contenu.
    pub fn contenu(
        &mut self,
        ui: &mut egui::Ui,
        moi: Option<UserId>,
        en_vocal: bool,
        qui: &dyn Fn(UserId) -> Identite,
        actions: &mut Vec<ActionCartes>,
    ) {
        let (zone, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
        let p = ui.painter_at(zone);
        // Le tapis : le fond le plus sombre, une pointe de vert.
        p.rect_filled(zone, rayon::XL, c::melanger(c::BG_DEEP, c::ACCENT, 0.05));
        p.rect_stroke(zone, rayon::XL, Stroke::new(1.0, c::BORDER), StrokeKind::Inside);
        let zone = zone.shrink(espace::L);
        let Some(moi) = moi else { return };
        match self.table.clone() {
            None => self.sans_table(ui, zone, en_vocal, actions),
            Some(t) if t.phase == PhaseCartes::Attente => self.attente(ui, zone, &t, moi, qui, actions),
            Some(t) => self.partie(ui, zone, &t, moi, qui, actions),
        }
        if let Some((message, quand)) = &self.refus {
            if quand.elapsed() < REFUS_VISIBLE {
                let bandeau = Rect::from_center_size(pos2(zone.center().x, zone.top() + 14.0), vec2(zone.width().min(520.0), 26.0));
                p.rect_filled(bandeau, rayon::PILULE, c::melanger(c::BG_RAISED, c::DANGER, 0.25));
                p.text(bandeau.center(), Align2::CENTER_CENTER, message, FontId::proportional(texte::COURANT), c::TEXT);
                ui.ctx().request_repaint_after(Duration::from_millis(250));
            } else {
                self.refus = None;
            }
        }
    }

    // --- Avant la table ---

    fn sans_table(&mut self, ui: &mut egui::Ui, zone: Rect, en_vocal: bool, actions: &mut Vec<ActionCartes>) {
        let colonne = Rect::from_center_size(zone.center(), vec2(zone.width().min(480.0), zone.height()));
        let mut ui = colonne_centree(ui, colonne);
        ui.add_space((zone.height() * 0.12).max(espace::XL));
        let (r, _) = ui.allocate_exact_size(Vec2::splat(56.0), Sense::hover());
        icones::draw(ui.painter(), r, Icon::Cartes, c::TEXT_DIM);
        ui.add_space(espace::M);
        ui.label(RichText::new("Dernière carte").size(texte::GRAND).color(c::TEXT).strong());
        ui.add_space(espace::S);
        if en_vocal {
            ui.label(RichText::new("Personne n'a encore ouvert de table dans ton salon.").color(c::TEXT_DIM));
            ui.add_space(espace::L);
            if ui::primary_button(&mut ui, Some(Icon::Cartes), "Ouvrir une table", None).clicked() {
                actions.push(ActionCartes::Ouvrir);
            }
        } else {
            ui.label(
                RichText::new("Rejoins un salon vocal : on joue entre ceux qui y sont, en se parlant.")
                    .color(c::TEXT_DIM),
            );
        }
        ui.add_space(espace::XXL);
        regles(&mut ui);
    }

    fn attente(
        &mut self,
        ui: &mut egui::Ui,
        zone: Rect,
        t: &TableCartes,
        moi: UserId,
        qui: &dyn Fn(UserId) -> Identite,
        actions: &mut Vec<ActionCartes>,
    ) {
        let colonne = Rect::from_center_size(zone.center(), vec2(zone.width().min(560.0), zone.height()));
        let regles_ok = self.regles_disponibles;
        // Ce qu'il y a sous les places ne tient pas toujours dans la
        // fenêtre : la colonne défile.
        colonne_defilante(ui, colonne, "attente", |ui| {
            ui.add_space((zone.height() * 0.06).max(espace::L));
            let hote = qui(t.hote);
            ui.label(RichText::new(format!("La table de {}", hote.nom)).size(texte::GRAND).color(c::TEXT).strong());
            ui.add_space(espace::S);
            let n = t.joueurs.len();
            ui::hint(ui, format!("{n} joueur{} assis sur {MAX_JOUEURS_CARTES} places", if n > 1 { "s" } else { "" }));
            ui.add_space(espace::L);

            // Les places : une pastille par joueur, l'hôte couronné.
            let case = vec2(84.0, 78.0);
            let par_ligne = ((colonne.width() / case.x).floor() as usize).max(1);
            for ligne in t.joueurs.chunks(par_ligne) {
                let (r, _) = ui.allocate_exact_size(vec2(case.x * ligne.len() as f32, case.y), Sense::hover());
                for (i, j) in ligne.iter().enumerate() {
                    let cr = Rect::from_min_size(r.min + vec2(i as f32 * case.x, 0.0), case);
                    let id = qui(j.user_id);
                    let av = Rect::from_center_size(pos2(cr.center().x, cr.top() + 26.0), Vec2::splat(44.0));
                    ui::paint_avatar(ui.painter(), av, &id.nom, false, id.photo.as_ref(), c::BG_DEEP);
                    if j.user_id == t.hote {
                        let couronne = Rect::from_center_size(av.right_top() + vec2(-4.0, 4.0), Vec2::splat(16.0));
                        icones::draw(ui.painter(), couronne, Icon::Crown, c::WARN);
                    }
                    ui.painter().text(
                        pos2(cr.center().x, cr.top() + 54.0),
                        Align2::CENTER_TOP,
                        abreger(&id.nom, 11),
                        FontId::proportional(texte::COURANT),
                        id.couleur,
                    );
                }
            }
            ui.add_space(espace::L);

            let assis = t.joueur(moi).is_some();
            ui.horizontal(|ui| {
                // Centrer une rangée de boutons : sa largeur se devine d'abord.
                let largeur = if assis && moi == t.hote { 300.0 } else { 180.0 };
                ui.add_space(((ui.available_width() - largeur) / 2.0).max(0.0));
                if moi == t.hote && assis {
                    let libelle = if t.manche == 0 { "Distribuer" } else { "Redistribuer" };
                    let pret = n >= 2;
                    if ui.add_enabled_ui(pret, |ui| ui::primary_button(ui, Some(Icon::Play), libelle, None)).inner.clicked() {
                        actions.push(ActionCartes::Lancer);
                    }
                }
                if assis {
                    if ui::button(ui, Icon::Logout, "Me lever").clicked() {
                        actions.push(ActionCartes::Quitter);
                    }
                } else if n < MAX_JOUEURS_CARTES && ui::primary_button(ui, Some(Icon::Plus), "M'asseoir", None).clicked() {
                    actions.push(ActionCartes::Rejoindre);
                }
            });
            ui.add_space(espace::S);
            if moi == t.hote && n < 2 {
                ui::hint(ui, "Il faut un deuxième joueur : ceux du salon peuvent s'asseoir.");
            } else if moi != t.hote {
                ui::hint(ui, format!("{} distribue quand tout le monde est assis.", hote.nom));
            }
            ui.add_space(espace::XL);
            if regles_ok {
                regles_en_plus(ui, t, moi, &hote.nom, actions);
                ui.add_space(espace::M);
            }
            regles(ui);
        });
    }

    // --- La partie ---

    fn partie(
        &mut self,
        ui: &mut egui::Ui,
        zone: Rect,
        t: &TableCartes,
        moi: UserId,
        qui: &dyn Fn(UserId) -> Identite,
        actions: &mut Vec<ActionCartes>,
    ) {
        let ma_place = t.joueur(moi);
        let p = ui.painter_at(zone.expand(espace::L));
        self.reperes.sieges.clear();
        self.reperes.main.clear();
        self.reperes.zone_main = None;
        self.reperes.pioche = None;
        self.reperes.defausse = None;
        let haut = Rect::from_min_size(zone.min, vec2(zone.width(), 122.0));
        let hauteur_main = (zone.height() * 0.30).clamp(120.0, 190.0);
        let bas = Rect::from_min_max(pos2(zone.left(), zone.bottom() - hauteur_main - 44.0), zone.max);
        let mut milieu = Rect::from_min_max(pos2(zone.left(), haut.bottom()), pos2(zone.right(), bas.top()));
        // Le fil des coups à droite, s'il y a la place.
        if zone.width() >= 760.0 {
            let fil = Rect::from_min_max(pos2(milieu.right() - 220.0, milieu.top() + espace::M), milieu.max);
            self.fil(ui, &p, fil, t, qui);
            milieu.max.x -= 228.0;
        }

        self.adversaires(ui, &p, haut, t, moi, qui, actions);
        if t.phase == PhaseCartes::Finie {
            let place = Rect::from_min_max(milieu.min, pos2(milieu.right(), zone.bottom()));
            self.fin_de_manche(ui, place, t, moi, qui, actions);
            self.peindre_effets(ui, &p, zone, milieu, moi, qui);
            return;
        }
        self.centre(ui, &p, milieu, t, moi, qui, actions);
        match ma_place {
            Some(j) if !j.bot => self.ma_place(ui, &p, bas, t, moi, qui, actions),
            Some(_) => self.place_du_bot(ui, &p, bas, t, moi, actions),
            None => spectateur(ui, bas, t, qui, actions),
        }
        self.peindre_effets(ui, &p, zone, milieu, moi, qui);
        if let Some(carte) = self.choix_couleur {
            self.choisir_couleur(ui, &p, milieu, carte, actions);
        }
        if let Some(carte) = self.choix_cible {
            self.choisir_cible(ui, &p, zone, t, moi, carte, qui, actions);
        }
    }

    /// Les autres joueurs, en haut, dans l'ordre où le jeu tourne après moi.
    #[allow(clippy::too_many_arguments)]
    fn adversaires(
        &mut self,
        ui: &mut egui::Ui,
        p: &Painter,
        rect: Rect,
        t: &TableCartes,
        moi: UserId,
        qui: &dyn Fn(UserId) -> Identite,
        actions: &mut Vec<ActionCartes>,
    ) {
        let n = t.joueurs.len();
        let ordre: Vec<&JoueurCartes> = match t.joueurs.iter().position(|j| j.user_id == moi) {
            Some(k) => (1..n).map(|d| &t.joueurs[(k + d) % n]).collect(),
            None => t.joueurs.iter().collect(),
        };
        if ordre.is_empty() {
            return;
        }
        let largeur = (rect.width() / ordre.len() as f32).min(168.0);
        let x0 = rect.center().x - largeur * ordre.len() as f32 / 2.0;
        let fraction = self.reste_ms() as f32 / (TOUR_CARTES_S * 1000) as f32;
        let assis = t.joueur(moi).is_some_and(|j| !j.bot);
        let now = Instant::now();
        let temps = ui.input(|i| i.time);
        for (i, j) in ordre.into_iter().enumerate() {
            let case = Rect::from_min_size(pos2(x0 + i as f32 * largeur, rect.top()), vec2(largeur, rect.height()))
                .translate(vec2(self.secousse(j.user_id, now), 0.0));
            let id = qui(j.user_id);
            let tour = t.tour == Some(j.user_id) && t.phase == PhaseCartes::EnCours;
            let centre = pos2(case.center().x, case.top() + 28.0);
            let r = 22.0;
            if tour && j.bot {
                // Un bot joue dans la seconde : pas de compte à rebours.
                p.circle_stroke(centre, r + 5.0, Stroke::new(3.0, c::translucide(c::ACCENT, 140)));
            } else if tour {
                p.circle_stroke(centre, r + 5.0, Stroke::new(3.0, c::translucide(c::ACCENT, 50)));
                arc(p, centre, r + 5.0, -FRAC_PI_2, -FRAC_PI_2 + TAU * fraction.clamp(0.0, 1.0), Stroke::new(3.0, teinte_du_temps(fraction)));
            }
            if t.expose == Some(j.user_id) {
                let pouls = (temps * 6.0).sin() as f32 * 0.5 + 0.5;
                p.circle_stroke(centre, r + 9.0, Stroke::new(2.0, c::translucide(c::DANGER, (120.0 + 120.0 * pouls) as u8)));
                ui.ctx().request_repaint_after(POULS);
            } else if j.cartes == 1 && t.phase == PhaseCartes::EnCours {
                // Plus qu'une carte : un halo d'or qui bat — toute la table
                // le voit venir.
                let pouls = (temps * 4.0).sin() as f32 * 0.5 + 0.5;
                p.circle_filled(centre, r + 10.0 + 4.0 * pouls, c::translucide(c::WARN, (25.0 + 35.0 * pouls) as u8));
                p.circle_stroke(centre, r + 9.0 + 2.0 * pouls, Stroke::new(2.5, c::translucide(c::WARN, (110.0 + 110.0 * pouls) as u8)));
                ui.ctx().request_repaint_after(POULS);
            }
            let av = Rect::from_center_size(centre, Vec2::splat(r * 2.0));
            ui::paint_avatar(p, av, &id.nom, false, id.photo.as_ref(), c::BG_DEEP);
            if j.absent || j.bot {
                p.circle_filled(centre, r, Color32::from_black_alpha(150));
            }
            if j.user_id == t.hote {
                icones::draw(p, Rect::from_center_size(av.right_top() + vec2(-3.0, 3.0), Vec2::splat(14.0)), Icon::Crown, c::WARN);
            }
            let nom = if j.absent { format!("{} · absent", abreger(&id.nom, 9)) } else { abreger(&id.nom, 14) };
            p.text(
                pos2(centre.x, case.top() + 56.0),
                Align2::CENTER_TOP,
                nom,
                FontId::proportional(texte::COURANT),
                if tour { c::TEXT } else { id.couleur.gamma_multiply(0.85) },
            );
            // Son jeu, de dos, en éventail, et le compte.
            let mini = vec2(15.0, 22.0);
            let montre = j.cartes.min(7) as f32;
            let eventail = mini.x + (montre - 1.0).max(0.0) * 5.0;
            let gauche = centre.x - (eventail + 26.0) / 2.0;
            for k in 0..montre as usize {
                peindre_dos(p, Rect::from_min_size(pos2(gauche + k as f32 * 5.0, case.top() + 78.0), mini));
            }
            // Là où ses cartes arrivent et d'où elles partent.
            self.reperes.sieges.insert(
                j.user_id,
                Rect::from_center_size(pos2(gauche + eventail / 2.0, case.top() + 89.0), vec2(22.0, 32.0)),
            );
            p.text(
                pos2(gauche + eventail + 6.0, case.top() + 89.0),
                Align2::LEFT_CENTER,
                j.cartes.to_string(),
                FontId::proportional(texte::CORPS),
                c::TEXT,
            );
            let derniere = j.annonce && (1..=2).contains(&j.cartes) && t.phase == PhaseCartes::EnCours;
            if derniere && j.bot {
                pastille(p, pos2(centre.x, case.top() + 110.0), "BOT · DERNIÈRE !", c::WARN);
            } else if derniere {
                pastille(p, pos2(centre.x, case.top() + 110.0), "DERNIÈRE !", c::WARN);
            } else if j.bot {
                pastille(p, pos2(centre.x, case.top() + 110.0), "BOT", c::ACCENT);
            } else if j.victoires > 0 {
                pastille(p, pos2(centre.x, case.top() + 110.0), &format!("{} gagnée{}", j.victoires, if j.victoires > 1 { "s" } else { "" }), c::TEXT_FAINT);
            }
            // Le contre se joue sur sa place, au clic.
            if assis && t.expose == Some(j.user_id) {
                let zone_clic = ui.interact(case, ui.id().with(("contrer", j.user_id)), Sense::click());
                if zone_clic.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    actions.push(ActionCartes::Contrer);
                }
            }
        }
    }

    /// La pioche, la défausse, le sens du jeu, et ce qu'on attend de qui.
    #[allow(clippy::too_many_arguments)]
    fn centre(
        &mut self,
        ui: &mut egui::Ui,
        p: &Painter,
        rect: Rect,
        t: &TableCartes,
        moi: UserId,
        qui: &dyn Fn(UserId) -> Identite,
        actions: &mut Vec<ActionCartes>,
    ) {
        // Le bloc tient dans sa zone : 16 px au-dessus pour la pastille du
        // cumul, les tas, puis 52 px pour leurs légendes et la phrase — sans
        // quoi, fenêtre basse, la phrase s'imprimait sous mes boutons.
        const DESSUS: f32 = 16.0;
        const DESSOUS: f32 = 52.0;
        let h = (rect.height() - DESSUS - DESSOUS).clamp(56.0, 156.0);
        let taille = vec2(h * RATIO, h);
        let haut_du_bloc = rect.top() + ((rect.height() - DESSUS - h - DESSOUS) / 2.0).max(0.0);
        let centre = pos2(rect.center().x, haut_du_bloc + DESSUS + h / 2.0);
        let ecart = taille.x * 0.35;
        let pioche = Rect::from_center_size(centre - vec2(taille.x / 2.0 + ecart, 0.0), taille);
        let defausse = Rect::from_center_size(centre + vec2(taille.x / 2.0 + ecart, 0.0), taille);
        let a_moi = a_moi_de_jouer(t, Some(moi));
        self.reperes.pioche = Some(pioche);
        self.reperes.defausse = Some(defausse);

        // Les règles en plus, rappelées dans un coin de la table : on les
        // oublie vite d'une manche à l'autre.
        let mut y = rect.top() + 12.0;
        for (en_jeu, mot) in [(t.regles.sept, "7 : ÉCHANGE"), (t.regles.zero, "0 : ÇA TOURNE")] {
            if en_jeu {
                pastille(p, pos2(rect.left() + 52.0, y), mot, c::ACCENT);
                y += 20.0;
            }
        }

        // Le sens du jeu, en arc autour des deux tas ; quand il s'inverse,
        // les flèches font un demi-tour.
        let cle = ui.id().with(("sens_du_jeu", t.manche));
        // Fenêtre fermée un moment : le sens a pu changer sans qu'on le
        // voie ; il se pose sans faire tourner les flèches.
        let vu_a_l_instant = self.peinte_le.is_some_and(|q| q.elapsed() < Duration::from_millis(500));
        let tourne = ui.ctx().animate_bool_with_time(cle, t.horaire, if vu_a_l_instant { 0.45 } else { 0.0 });
        self.peinte_le = Some(Instant::now());
        sens_du_jeu(p, centre, taille.x * 1.9, h * 0.70, t.horaire, PI * tourne);

        // La pioche : un clic pioche, quand c'est à moi et que je n'ai pas
        // encore pioché.
        let peut_piocher = a_moi && t.piochee.is_none();
        let clic = ui.interact(pioche, ui.id().with("pioche"), Sense::click());
        let survol = peut_piocher && clic.hovered();
        if peut_piocher {
            let halo = if survol { 110 } else { 55 };
            p.rect_filled(pioche.expand(4.0), coin(pioche), c::translucide(c::ACCENT, halo));
        }
        for k in (1..3).rev() {
            if t.pioche as usize > k {
                peindre_dos(p, pioche.translate(vec2(-2.0 * k as f32, -2.0 * k as f32)));
            }
        }
        if t.pioche > 0 {
            peindre_dos(p, pioche.translate(vec2(0.0, if survol { -4.0 } else { 0.0 })));
        } else {
            p.rect_stroke(pioche, coin(pioche), Stroke::new(1.5, c::BORDER_STRONG), StrokeKind::Inside);
        }
        p.text(
            pos2(pioche.center().x, pioche.bottom() + 8.0),
            Align2::CENTER_TOP,
            format!("pioche · {}", t.pioche),
            FontId::proportional(texte::PETIT),
            c::TEXT_FAINT,
        );
        if peut_piocher && clic.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            actions.push(ActionCartes::Piocher);
        }

        // La défausse, auréolée de la couleur demandée. La carte qui vole
        // encore vers elle n'y est pas : on voit ce qu'il y avait dessous.
        if let (Some(dessus), Some(couleur)) = (t.dessus, t.couleur) {
            p.rect_stroke(defausse.expand(6.0), coin(defausse), Stroke::new(3.0, teinte(couleur)), StrokeKind::Outside);
            match self.defausse_en_vol(dessus, Instant::now()) {
                Some(Some(sous)) => peindre_carte(p, defausse, sous, 0.0),
                Some(None) => {}
                None => peindre_carte(p, defausse, dessus, 0.0),
            }
            let legende = if dessus.sauvage() { format!("couleur demandée : {}", couleur.nom()) } else { "défausse".into() };
            p.text(
                pos2(defausse.center().x, defausse.bottom() + 8.0),
                Align2::CENTER_TOP,
                legende,
                FontId::proportional(texte::PETIT),
                if dessus.sauvage() { teinte(couleur) } else { c::TEXT_FAINT },
            );
        }
        if t.cumul > 0 {
            let pos = defausse.right_top() + vec2(4.0, -4.0);
            let badge = Rect::from_center_size(pos, vec2(46.0, 28.0));
            p.rect_filled(badge, rayon::PILULE, c::DANGER);
            p.text(badge.center(), Align2::CENTER_CENTER, format!("+{}", t.cumul), FontId::proportional(texte::GRAND), c::BG_DEEP);
        }

        // Ce qu'on attend, et de qui.
        let secondes = self.reste_ms().div_ceil(1000);
        let ma_place_au_bot = t.joueur(moi).is_some_and(|j| j.bot);
        let (phrase, teinte_phrase) = if t.expose == Some(moi) && !ma_place_au_bot {
            // Je suis tombé à une carte sans le dire : on peut me contrer,
            // c'est plus pressé que le reste.
            ("Tu es à une carte : dis « Dernière carte ! », vite !".to_string(), c::WARN)
        } else if a_moi {
            let quoi = if t.piochee.is_some() {
                "Tu as pioché une carte qui va : pose-la, ou passe".to_string()
            } else if t.cumul > 0 {
                format!("À toi : renchéris avec un +2 ou un +4, ou pioche {}", t.cumul)
            } else {
                "À toi de jouer".to_string()
            };
            (format!("{quoi} · {secondes} s"), c::ACCENT)
        } else if let Some(tour) = t.tour {
            if tour == moi && ma_place_au_bot {
                ("Ton bot joue ta main…".to_string(), c::WARN)
            } else if t.joueur(tour).is_some_and(|j| j.bot) {
                (format!("Au tour du bot de {}…", qui(tour).nom), c::TEXT_DIM)
            } else {
                (format!("Au tour de {} · {secondes} s", qui(tour).nom), c::TEXT_DIM)
            }
        } else {
            (String::new(), c::TEXT_DIM)
        };
        let ligne = pos2(rect.center().x, pioche.bottom() + 30.0);
        p.text(ligne, Align2::CENTER_TOP, phrase, FontId::proportional(texte::TITRE), teinte_phrase);
    }

    /// La manche est gagnée : le vainqueur, les scores, et la suite.
    fn fin_de_manche(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        t: &TableCartes,
        moi: UserId,
        qui: &dyn Fn(UserId) -> Identite,
        actions: &mut Vec<ActionCartes>,
    ) {
        let regles_ok = self.regles_disponibles;
        let colonne = Rect::from_center_size(rect.center(), vec2(rect.width().min(460.0), rect.height()));
        // Les scores et les règles en plus ne tiennent pas toujours dans la
        // fenêtre : la colonne défile.
        colonne_defilante(ui, colonne, "fin", |ui| {
            ui.add_space((rect.height() * 0.08).max(espace::M));
            let (r, _) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
            icones::draw(ui.painter(), r, Icon::Crown, c::WARN);
            if let Some(g) = t.gagnant {
                let titre = if t.gagnant_bot {
                    format!("Le bot de {} gagne la manche !", qui(g).nom)
                } else if g == moi {
                    "Tu gagnes la manche !".to_string()
                } else {
                    format!("{} gagne la manche !", qui(g).nom)
                };
                ui.label(RichText::new(titre).size(texte::GRAND).color(c::TEXT).strong());
            }
            ui.add_space(espace::M);
            let mut scores: Vec<&JoueurCartes> = t.joueurs.iter().collect();
            scores.sort_by_key(|j| std::cmp::Reverse(j.victoires));
            for j in scores {
                let id = qui(j.user_id);
                let reste = if j.cartes > 0 { format!(" · {} en main", j.cartes) } else { String::new() };
                ui.label(
                    RichText::new(format!("{} — {} manche{} gagnée{}{reste}", id.nom, j.victoires, pluriel(j.victoires), pluriel(j.victoires)))
                        .color(id.couleur)
                        .size(texte::COURANT),
                );
            }
            ui.add_space(espace::L);
            let assis = t.joueur(moi).is_some();
            ui.horizontal(|ui| {
                ui.add_space(((ui.available_width() - 260.0) / 2.0).max(0.0));
                if moi == t.hote
                    && assis
                    && ui
                        .add_enabled_ui(t.joueurs.len() >= 2, |ui| ui::primary_button(ui, Some(Icon::Repeat), "Redistribuer", None))
                        .inner
                        .clicked()
                {
                    actions.push(ActionCartes::Lancer);
                }
                if assis {
                    if ui::button(ui, Icon::Logout, "Me lever").clicked() {
                        actions.push(ActionCartes::Quitter);
                    }
                } else if t.joueurs.len() < MAX_JOUEURS_CARTES && ui::primary_button(ui, Some(Icon::Plus), "M'asseoir", None).clicked() {
                    actions.push(ActionCartes::Rejoindre);
                }
            });
            if moi != t.hote {
                ui.add_space(espace::S);
                ui::hint(ui, format!("{} redistribue quand il veut.", qui(t.hote).nom));
            }
            if regles_ok {
                ui.add_space(espace::L);
                regles_en_plus(ui, t, moi, &qui(t.hote).nom, actions);
            }
        });
    }

    /// Ma place : mes gestes, puis ma main.
    #[allow(clippy::too_many_arguments)]
    fn ma_place(
        &mut self,
        ui: &mut egui::Ui,
        p: &Painter,
        rect: Rect,
        t: &TableCartes,
        moi: UserId,
        qui: &dyn Fn(UserId) -> Identite,
        actions: &mut Vec<ActionCartes>,
    ) {
        let en_cours = t.phase == PhaseCartes::EnCours;
        let a_moi = en_cours && t.tour == Some(moi);
        let barre = Rect::from_min_size(rect.min, vec2(rect.width(), 34.0));
        let main = Rect::from_min_max(pos2(rect.left(), barre.bottom() + espace::M), rect.max);

        // Le temps qui me reste, en filet au-dessus de la main.
        if a_moi {
            let f = (self.reste_ms() as f32 / (TOUR_CARTES_S * 1000) as f32).clamp(0.0, 1.0);
            let filet = Rect::from_min_size(pos2(main.left(), main.top() - 3.0), vec2(main.width(), 3.0));
            p.rect_filled(filet, 2.0, c::translucide(c::TEXT_FAINT, 40));
            p.rect_filled(Rect::from_min_size(filet.min, vec2(filet.width() * f, 3.0)), 2.0, teinte_du_temps(f));
        }

        if en_cours {
            let mut ligne = ui.new_child(
                egui::UiBuilder::new().max_rect(barre).layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            let ma_main = t.main.len();
            // Fenêtre étroite : des libellés courts, sinon la droite de la
            // barre (mon compte, l'abandon) passe sous le contre.
            let etroit = barre.width() < 760.0;
            let libelle = if t.cumul > 0 && a_moi { format!("Piocher {}", t.cumul) } else { "Piocher".to_string() };
            if ligne.add_enabled_ui(a_moi && t.piochee.is_none(), |ui| ui::button(ui, Icon::Download, &libelle)).inner.clicked() {
                actions.push(ActionCartes::Piocher);
            }
            if a_moi && t.piochee.is_some() && ui::button(&mut ligne, Icon::ChevronRight, "Passer").clicked() {
                actions.push(ActionCartes::Passer);
            }
            let deja = t.joueur(moi).is_some_and(|j| j.annonce);
            let peut_annoncer = ma_main <= 2 && !deja;
            let ton = if t.expose == Some(moi) { Tone::Danger } else { Tone::Warn };
            if ligne
                .add_enabled_ui(peut_annoncer, |ui| ui::tinted_button(ui, Some(Icon::Chat), "Dernière carte !", ton))
                .inner
                .on_hover_text("à dire à deux cartes ou moins — sinon, le premier qui te contre te fait piocher 2")
                .clicked()
            {
                actions.push(ActionCartes::Annoncer);
            }
            // Quelqu'un est tombé à une carte sans le dire : le premier qui
            // clique le fait piocher.
            if let Some(expose) = t.expose.filter(|e| *e != moi) {
                let nom = qui(expose).nom;
                let libelle = if etroit { "Contrer !".to_string() } else { format!("Contrer {} !", abreger(&nom, 12)) };
                if ui::tinted_button(&mut ligne, Some(Icon::Warning), &libelle, Tone::Danger)
                    .on_hover_text(format!("{nom} n'a pas dit « Dernière carte ! » : il pioche {PENALITE_ANNONCE} cartes"))
                    .clicked()
                {
                    actions.push(ActionCartes::Contrer);
                }
            }
            ligne.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui::tinted_button(ui, Some(Icon::Logout), if etroit { "" } else { "Abandonner" }, Tone::Danger)
                    .on_hover_text("un bot finit la manche avec ta main — tu pourras la reprendre, ou un pote la prendra")
                    .clicked()
                {
                    actions.push(ActionCartes::Quitter);
                }
                let moi_id = qui(moi);
                let victoires = t.joueur(moi).map(|j| j.victoires).unwrap_or(0);
                let detail = if victoires > 0 { format!(" · {victoires} gagnée{}", pluriel(victoires)) } else { String::new() };
                // Mon compte de cartes : les autres le voient sur ma place,
                // moi nulle part — et à 26, on ne les compte plus à l'œil.
                let n = ma_main as u32;
                let qui_suis_je = if etroit { String::new() } else { format!("{} · ", moi_id.nom) };
                ui.label(
                    RichText::new(format!("{qui_suis_je}{n} carte{}{detail}", pluriel(n)))
                        .color(moi_id.couleur)
                        .size(texte::COURANT),
                );
            });
        }
        self.peindre_main(ui, p, main, t, moi, a_moi, true, actions);
    }

    /// Ma place, qu'un bot tient pour moi (trois tours manqués, ou j'ai
    /// abandonné) : ma main, que je ne joue plus, et de quoi la reprendre.
    fn place_du_bot(
        &mut self,
        ui: &mut egui::Ui,
        p: &Painter,
        rect: Rect,
        t: &TableCartes,
        moi: UserId,
        actions: &mut Vec<ActionCartes>,
    ) {
        let barre = Rect::from_min_size(rect.min, vec2(rect.width(), 34.0));
        let main = Rect::from_min_max(pos2(rect.left(), barre.bottom() + espace::M), rect.max);
        let mut ligne = ui.new_child(
            egui::UiBuilder::new().max_rect(barre).layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        ligne.label(RichText::new("Un bot joue ta main à ta place.").color(c::WARN).size(texte::CORPS));
        ligne.add_space(espace::M);
        if ui::primary_button(&mut ligne, Some(Icon::Play), "Reprendre ma place", None).clicked() {
            actions.push(ActionCartes::Reprendre { place: moi });
        }
        self.peindre_main(ui, p, main, t, moi, false, false, actions);
    }

    #[allow(clippy::too_many_arguments)]
    fn peindre_main(
        &mut self,
        ui: &mut egui::Ui,
        p: &Painter,
        rect: Rect,
        t: &TableCartes,
        moi: UserId,
        a_moi: bool,
        // Faux quand un bot tient ma place : je vois ma main sans la jouer.
        active: bool,
        actions: &mut Vec<ActionCartes>,
    ) {
        let n = t.main.len();
        if n == 0 {
            return;
        }
        let cases = disposer_main(n, rect);
        let en_vol = self.cartes_en_vol(Instant::now());
        // Une carte qui n'est pas encore arrivée ne se joue pas : on ne la
        // voit pas.
        let jouables: Vec<bool> =
            t.main.iter().map(|&carte| active && !en_vol.contains(&carte) && t.jouable(moi, carte)).collect();
        self.reperes.zone_main = Some(rect);
        for (carte, case) in t.main.iter().zip(&cases) {
            self.reperes.main.insert(*carte, *case);
        }

        // Une carte jouable est relevée : on la vise là où elle est, et
        // jusqu'à sa place de repos pour que le pointeur ne la lâche pas.
        // La dernière posée par-dessus l'emporte — la rangée de devant.
        let reponse = ui.interact(rect.expand2(vec2(0.0, LEVEE)), ui.id().with("main_cartes"), Sense::click());
        let survol = reponse.hover_pos().and_then(|pos| {
            cases.iter().zip(&jouables).rposition(|(r, &jouable)| {
                let visee = if jouable { Rect::from_min_max(r.min - vec2(0.0, LEVEE), r.max) } else { *r };
                visee.contains(pos)
            })
        });

        // La carte survolée se peint en dernier : elle passe devant ses voisines.
        let mut ordre: Vec<usize> = (0..n).filter(|&i| Some(i) != survol).collect();
        ordre.extend(survol);
        for i in ordre {
            let carte = t.main[i];
            // Elle vole encore vers sa case : elle s'y posera.
            if en_vol.contains(&carte) {
                continue;
            }
            let leve = ui.ctx().animate_bool_with_time(
                ui.id().with(("leve", carte.0)),
                survol == Some(i) && jouables[i],
                0.10,
            );
            let monte = if jouables[i] { LEVEE } else { 0.0 } + 12.0 * leve;
            let r = cases[i].translate(vec2(0.0, -monte));
            // Si je la pose, son vol partira d'ici, telle qu'on la voit.
            self.reperes.main.insert(carte, r);
            if n == 1 && t.phase == PhaseCartes::EnCours {
                // Ma dernière carte brille.
                let pouls = (ui.input(|i| i.time) * 4.0).sin() as f32 * 0.5 + 0.5;
                p.rect_filled(r.expand(6.0 + 4.0 * pouls), coin(r), c::translucide(c::WARN, (50.0 + 60.0 * pouls) as u8));
                ui.ctx().request_repaint_after(POULS);
            }
            if jouables[i] {
                p.rect_filled(r.expand(3.0), coin(r), c::translucide(c::ACCENT, (60.0 + 80.0 * leve) as u8));
            }
            let voile = if !active {
                0.35
            } else if a_moi && !jouables[i] {
                0.55
            } else {
                0.0
            };
            peindre_carte(p, r, carte, voile);
        }
        if survol.is_some_and(|i| jouables[i]) {
            reponse.clone().on_hover_cursor(egui::CursorIcon::PointingHand);
        }
        if reponse.clicked() {
            if let Some(i) = survol.filter(|&i| jouables[i]) {
                self.jouer_carte(t, moi, t.main[i], actions);
            }
        }
    }

    /// Je pose `carte` : un joker attend sa couleur, un 7 qui échange
    /// attend avec qui — à deux, pas de choix à faire —, le reste part.
    pub fn jouer_carte(&mut self, t: &TableCartes, moi: UserId, carte: Carte, actions: &mut Vec<ActionCartes>) {
        if carte.sauvage() {
            self.choix_couleur = Some(carte);
        } else if t.regles.echange_au_sept(carte, t.main.len().saturating_sub(1)) {
            let autres: Vec<UserId> = t.joueurs.iter().map(|j| j.user_id).filter(|j| *j != moi).collect();
            match autres.as_slice() {
                [seul] => actions.push(ActionCartes::Jouer { carte, couleur: None, cible: Some(*seul) }),
                _ => self.choix_cible = Some(carte),
            }
        } else {
            actions.push(ActionCartes::Jouer { carte, couleur: None, cible: None });
        }
    }

    /// Le joker posé attend sa couleur : quatre pastilles par-dessus la table.
    fn choisir_couleur(&mut self, ui: &mut egui::Ui, p: &Painter, rect: Rect, carte: Carte, actions: &mut Vec<ActionCartes>) {
        p.rect_filled(rect, rayon::XL, Color32::from_black_alpha(170));
        let centre = rect.center();
        p.text(
            centre - vec2(0.0, 62.0),
            Align2::CENTER_CENTER,
            format!("{} : quelle couleur ?", carte.nom()),
            FontId::proportional(texte::GRAND),
            c::TEXT,
        );
        let rayon_pastille = 30.0;
        for (k, couleur) in CouleurCarte::TOUTES.into_iter().enumerate() {
            let pos = centre + vec2((k as f32 - 1.5) * 84.0, 0.0);
            let zone = Rect::from_center_size(pos, Vec2::splat(rayon_pastille * 2.0));
            let clic = ui.interact(zone, ui.id().with(("couleur", k)), Sense::click());
            let r = if clic.hovered() { rayon_pastille + 4.0 } else { rayon_pastille };
            p.circle_filled(pos, r, teinte(couleur));
            p.circle_stroke(pos, r, Stroke::new(2.0, Color32::from_white_alpha(if clic.hovered() { 230 } else { 120 })));
            p.text(pos + vec2(0.0, r + 12.0), Align2::CENTER_CENTER, couleur.nom(), FontId::proportional(texte::COURANT), c::TEXT_DIM);
            if clic.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                actions.push(ActionCartes::Jouer { carte, couleur: Some(couleur), cible: None });
                self.choix_couleur = None;
            }
        }
        let annuler = Rect::from_center_size(centre + vec2(0.0, 82.0), vec2(120.0, 30.0));
        let mut sous = ui.new_child(egui::UiBuilder::new().max_rect(annuler).layout(egui::Layout::top_down(egui::Align::Center)));
        if ui::button(&mut sous, Icon::Close, "Annuler").clicked() {
            self.choix_couleur = None;
        }
    }

    /// Le 7 posé attend avec qui échanger : un portrait par joueur, et ce
    /// qu'il a en main. Par-dessus toute la table — à dix, il faut la place.
    #[allow(clippy::too_many_arguments)]
    fn choisir_cible(
        &mut self,
        ui: &mut egui::Ui,
        p: &Painter,
        rect: Rect,
        t: &TableCartes,
        moi: UserId,
        carte: Carte,
        qui: &dyn Fn(UserId) -> Identite,
        actions: &mut Vec<ActionCartes>,
    ) {
        p.rect_filled(rect, rayon::XL, Color32::from_black_alpha(190));
        // Le voile prend les clics : rien de ce qu'il couvre ne se joue.
        let _ = ui.interact(rect, ui.id().with("voile_cible"), Sense::click());
        let autres: Vec<&JoueurCartes> = t.joueurs.iter().filter(|j| j.user_id != moi).collect();
        let case = vec2(96.0, 104.0);
        let par_ligne = ((rect.width() - 40.0) / case.x).floor().max(1.0) as usize;
        let lignes = autres.len().div_ceil(par_ligne).max(1);
        let haut = rect.center().y - lignes as f32 * case.y / 2.0;
        p.text(
            pos2(rect.center().x, haut - 34.0),
            Align2::CENTER_CENTER,
            format!("{} : avec qui échanges-tu ta main ?", carte.nom()),
            FontId::proportional(texte::GRAND),
            c::TEXT,
        );
        for (l, ligne) in autres.chunks(par_ligne).enumerate() {
            let x0 = rect.center().x - case.x * ligne.len() as f32 / 2.0;
            for (k, j) in ligne.iter().enumerate() {
                let zone = Rect::from_min_size(pos2(x0 + k as f32 * case.x, haut + l as f32 * case.y), case).shrink(3.0);
                let clic = ui.interact(zone, ui.id().with(("cible", j.user_id)), Sense::click());
                if clic.hovered() {
                    p.rect_filled(zone, rayon::L, c::translucide(c::ACCENT, 45));
                }
                let id = qui(j.user_id);
                let centre = pos2(zone.center().x, zone.top() + 30.0);
                let r = if clic.hovered() { 26.0 } else { 24.0 };
                ui::paint_avatar(p, Rect::from_center_size(centre, Vec2::splat(r * 2.0)), &id.nom, false, id.photo.as_ref(), c::BG_DEEP);
                p.text(pos2(centre.x, zone.top() + 62.0), Align2::CENTER_TOP, abreger(&id.nom, 11), FontId::proportional(texte::COURANT), id.couleur);
                let n = u32::from(j.cartes);
                p.text(
                    pos2(centre.x, zone.top() + 80.0),
                    Align2::CENTER_TOP,
                    format!("{n} carte{}", pluriel(n)),
                    FontId::proportional(texte::PETIT),
                    if n <= 2 { c::WARN } else { c::TEXT_DIM },
                );
                if clic.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    actions.push(ActionCartes::Jouer { carte, couleur: None, cible: Some(j.user_id) });
                    self.choix_cible = None;
                }
            }
        }
        let annuler = Rect::from_center_size(pos2(rect.center().x, haut + lignes as f32 * case.y + 22.0), vec2(120.0, 30.0));
        let mut sous = ui.new_child(egui::UiBuilder::new().max_rect(annuler).layout(egui::Layout::top_down(egui::Align::Center)));
        if ui::button(&mut sous, Icon::Close, "Annuler").clicked() {
            self.choix_cible = None;
        }
    }

    /// Le fil des coups, du plus ancien au plus récent.
    fn fil(&self, ui: &mut egui::Ui, p: &Painter, rect: Rect, t: &TableCartes, qui: &dyn Fn(UserId) -> Identite) {
        p.rect_filled(rect, rayon::L, c::translucide(c::BG_BASE, 160));
        let mut sous = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect.shrink(espace::M))
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        sous.set_clip_rect(rect);
        sous.label(RichText::new(format!("Manche {}", t.manche.max(1))).color(c::TEXT_FAINT).size(texte::PETIT).strong());
        if let Some(liste) = liste_des_regles(t.regles) {
            sous.label(RichText::new(format!("En plus : {liste}")).color(c::TEXT_FAINT).size(texte::PETIT));
        }
        sous.add_space(espace::XS);
        let nom = |id: UserId| qui(id).nom;
        let n = t.journal.len();
        for (i, e) in t.journal.iter().enumerate() {
            let recent = i + 1 == n;
            sous.label(
                RichText::new(raconter(e, &nom))
                    .size(texte::COURANT)
                    .color(if recent { c::TEXT } else { c::TEXT_DIM }),
            );
        }
    }
}

// --- Les animations ---
//
// Le serveur envoie l'état de la table et le fil de ce qui s'y passe ; ce
// qui est neuf dans le fil devient un effet : une carte qui vole de la main
// d'un joueur à la défausse, la pioche qui part chez celui qui pioche, la
// donne carte par carte, « Dernière carte ! » qui éclate au milieu de la
// table, le contre qui secoue sa cible, les confettis de la victoire. Les
// effets ne décident de rien : l'état est déjà là, ils le mettent en scène
// le temps d'une demi-seconde.

/// Ce que dure le vol d'une carte, en secondes.
const VOL: f32 = 0.38;
/// L'annonce « Dernière carte ! » au milieu de la table.
const ANNONCE: f32 = 1.9;
/// Le contre : sa bannière, et la cible qui tremble.
const CONTRE: f32 = 1.3;
/// Les confettis de la victoire.
const VICTOIRE: f32 = 3.5;
/// La bannière d'un échange de mains.
const ECHANGE: f32 = 1.6;
/// Les cartes que je reçois volent toutes jusqu'à ma main, jusqu'à ce
/// nombre : au-delà, elles y sont déjà.
const VOLS_MOI_MAX: usize = 26;
/// Entre deux cartes de la donne.
const DONNE_PAS: f32 = 0.035;
/// Au-delà, une pioche ne fait voler que ce nombre de cartes : le compte,
/// lui, est juste.
const VOLS_MAX: u8 = 6;

/// Où un effet part ou arrive. Résolu au moment de peindre : la fenêtre a
/// pu changer de taille entre l'événement et l'image.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Lieu {
    Pioche,
    Defausse,
    /// La place d'un joueur : son éventail en haut, ou ma main si c'est moi.
    Siege(UserId),
    /// La case d'une carte de ma main.
    MaCarte(Carte),
    /// Une case déjà connue : celle qu'avait la carte que je viens de poser,
    /// qui n'est plus dans ma main quand l'état arrive.
    Fixe(Rect),
}

#[derive(Clone, Debug, PartialEq)]
enum Effet {
    /// Une carte qui vole — face visible, ou de dos (`None`). `sous` : ce
    /// que la défausse montre tant qu'elle n'est pas arrivée.
    Vol { carte: Option<Carte>, de: Lieu, vers: Lieu, sous: Option<Carte> },
    /// « Dernière carte ! »
    Annonce { qui: UserId },
    /// « Contré ! » : `cible` pioche `n` cartes.
    Contre { cible: UserId, n: u8 },
    /// Le 7 : `par` et `avec` échangent leurs mains.
    Echange { par: UserId, avec: UserId },
    /// Le 0 : toutes les mains tournent.
    Tourne { par: UserId },
    Victoire,
}

#[derive(Clone, Debug)]
struct Anim {
    effet: Effet,
    debut: Instant,
    duree: f32,
}

impl Anim {
    /// De 0 à 1 pendant l'effet ; négatif avant son départ (la donne part
    /// carte après carte), au-delà de 1 une fois fini.
    fn avancement(&self, now: Instant) -> f32 {
        let ecoule = if now >= self.debut {
            (now - self.debut).as_secs_f32()
        } else {
            -(self.debut - now).as_secs_f32()
        };
        ecoule / self.duree
    }
}

/// Où sont les choses à l'image d'avant : de quoi faire partir et arriver
/// les cartes au bon endroit.
#[derive(Default)]
struct Reperes {
    /// L'éventail de chaque adversaire.
    sieges: HashMap<UserId, Rect>,
    pioche: Option<Rect>,
    defausse: Option<Rect>,
    /// Chaque carte de ma main, à sa place.
    main: HashMap<Carte, Rect>,
    /// La zone de ma main.
    zone_main: Option<Rect>,
}

/// Les effets des événements neufs, chacun avec son retard et sa durée (en
/// secondes). `cases` : où étaient mes cartes à l'image d'avant.
fn effets_de(
    neufs: &[EvenementCartes],
    avant: &TableCartes,
    apres: &TableCartes,
    moi: Option<UserId>,
    cases: &HashMap<Carte, Rect>,
) -> Vec<(Effet, f32, f32)> {
    let est_moi = |id: UserId| Some(id) == moi;
    // Mes cartes neuves, dans l'ordre où elles arrivent ; à la donne, toute
    // ma main.
    let mut neuves: std::collections::VecDeque<Carte> = if neufs.contains(&EvenementCartes::Manche) {
        apres.main.iter().copied().collect()
    } else {
        apres.main.iter().copied().filter(|c| !avant.main.contains(c)).collect()
    };
    // Mes cartes parties autrement qu'à la défausse : données dans un
    // échange, ou passées au voisin.
    let posees: Vec<Carte> = neufs
        .iter()
        .filter_map(|e| match e {
            EvenementCartes::Joue { carte, .. } => Some(*carte),
            _ => None,
        })
        .collect();
    let mut parties: Vec<Carte> =
        avant.main.iter().copied().filter(|c| !apres.main.contains(c) && !posees.contains(c)).collect();
    let mut sortie = Vec::new();
    let mut retard = 0.0;
    let mut dessus = avant.dessus;
    // `n` cartes de dos, de la pioche vers `qui` ; chez moi, chacune vers la
    // case de la carte neuve qu'elle devient.
    let tirage = |qui: UserId, n: u8, retard: &mut f32, sortie: &mut Vec<(Effet, f32, f32)>, neuves: &mut std::collections::VecDeque<Carte>| {
        for _ in 0..n.min(VOLS_MAX) {
            let vers = match est_moi(qui).then(|| neuves.pop_front()).flatten() {
                Some(c) => Lieu::MaCarte(c),
                None => Lieu::Siege(qui),
            };
            sortie.push((Effet::Vol { carte: None, de: Lieu::Pioche, vers, sous: None }, *retard, VOL));
            *retard += 0.08;
        }
    };
    for e in neufs {
        match e {
            EvenementCartes::Joue { par, carte, .. } => {
                let de = match cases.get(carte) {
                    Some(r) if est_moi(*par) => Lieu::Fixe(*r),
                    _ => Lieu::Siege(*par),
                };
                sortie.push((Effet::Vol { carte: Some(*carte), de, vers: Lieu::Defausse, sous: dessus }, retard, VOL));
                dessus = Some(*carte);
                retard += 0.1;
            }
            EvenementCartes::Pioche { par, n, .. } => tirage(*par, *n, &mut retard, &mut sortie, &mut neuves),
            EvenementCartes::Contre { cible, n, .. } => {
                sortie.push((Effet::Contre { cible: *cible, n: *n }, retard, CONTRE));
                retard += 0.25;
                tirage(*cible, *n, &mut retard, &mut sortie, &mut neuves);
            }
            EvenementCartes::Annonce { par } => sortie.push((Effet::Annonce { qui: *par }, retard, ANNONCE)),
            EvenementCartes::Echange { par, avec } => {
                sortie.push((Effet::Echange { par: *par, avec: *avec }, retard, ECHANGE));
                retard += 0.15;
                let depart = retard;
                for (de, vers) in [(*par, *avec), (*avec, *par)] {
                    let mut r = depart;
                    passer_une_main(de, vers, apres, moi, cases, &mut neuves, &mut parties, &mut r, &mut sortie);
                    retard = f32::max(retard, r);
                }
            }
            EvenementCartes::Tourne { par } => {
                sortie.push((Effet::Tourne { par: *par }, retard, ECHANGE));
                retard += 0.15;
                // Toutes ensemble, chacune vers le voisin dans le sens du jeu.
                let n = apres.joueurs.len();
                let depart = retard;
                for k in 0..n {
                    let suivant = if apres.horaire { (k + 1) % n } else { (k + n - 1) % n };
                    let (de, vers) = (apres.joueurs[k].user_id, apres.joueurs[suivant].user_id);
                    let mut r = depart;
                    passer_une_main(de, vers, apres, moi, cases, &mut neuves, &mut parties, &mut r, &mut sortie);
                    retard = f32::max(retard, r);
                }
            }
            EvenementCartes::Gagne { .. } => sortie.push((Effet::Victoire, retard + 0.2, VICTOIRE)),
            EvenementCartes::Manche => {
                // La donne : une carte à chacun, tour après tour.
                for _ in 0..ki_protocol::CARTES_EN_MAIN {
                    for j in &apres.joueurs {
                        let vers = match est_moi(j.user_id).then(|| neuves.pop_front()).flatten() {
                            Some(c) => Lieu::MaCarte(c),
                            None => Lieu::Siege(j.user_id),
                        };
                        sortie.push((Effet::Vol { carte: None, de: Lieu::Pioche, vers, sous: None }, retard, VOL));
                        retard += DONNE_PAS;
                    }
                }
            }
            _ => {}
        }
    }
    sortie
}

/// Une main entière qui passe de la place `de` à la place `vers` : de dos
/// entre deux autres, face visible quand c'est la mienne qui part, et vers
/// leurs cases quand c'est moi qui la reçois.
#[allow(clippy::too_many_arguments)]
fn passer_une_main(
    de: UserId,
    vers: UserId,
    apres: &TableCartes,
    moi: Option<UserId>,
    cases: &HashMap<Carte, Rect>,
    neuves: &mut std::collections::VecDeque<Carte>,
    parties: &mut Vec<Carte>,
    retard: &mut f32,
    sortie: &mut Vec<(Effet, f32, f32)>,
) {
    let mut voler = |carte: Option<Carte>, de: Lieu, vers: Lieu, retard: &mut f32| {
        sortie.push((Effet::Vol { carte, de, vers, sous: None }, *retard, VOL));
        *retard += 0.04;
    };
    if Some(vers) == moi {
        for c in neuves.drain(..).take(VOLS_MOI_MAX) {
            voler(None, Lieu::Siege(de), Lieu::MaCarte(c), retard);
        }
    } else if Some(de) == moi {
        for c in parties.drain(..).take(VOLS_MOI_MAX) {
            let depuis = cases.get(&c).map_or(Lieu::Siege(de), |r| Lieu::Fixe(*r));
            voler(Some(c), depuis, Lieu::Siege(vers), retard);
        }
    } else {
        // `vers` tient maintenant la main de `de` : son compte est celui
        // qui a voyagé.
        let n = apres.joueur(vers).map_or(0, |j| j.cartes).min(VOLS_MAX);
        for _ in 0..n {
            voler(None, Lieu::Siege(de), Lieu::Siege(vers), retard);
        }
    }
}

impl Cartes {
    /// Les effets en cours, peints par-dessus la table.
    fn peindre_effets(&mut self, ui: &egui::Ui, p: &Painter, zone: Rect, milieu: Rect, moi: UserId, qui: &dyn Fn(UserId) -> Identite) {
        let now = Instant::now();
        self.anims.retain(|a| a.avancement(now) < 1.0);
        if self.anims.is_empty() {
            return;
        }
        ui.ctx().request_repaint();
        // Les cartes qui volent d'abord, les bannières par-dessus.
        let mut ordre: Vec<&Anim> = self.anims.iter().collect();
        ordre.sort_by_key(|a| !matches!(a.effet, Effet::Vol { .. }));
        for a in ordre {
            let x = a.avancement(now);
            if x < 0.0 {
                continue;
            }
            match &a.effet {
                Effet::Vol { carte, de, vers, .. } => {
                    let (Some(r0), Some(r1)) = (self.lieu(*de, moi), self.lieu(*vers, moi)) else { continue };
                    let e = 1.0 - (1.0 - x).powi(3);
                    // Une courbe plutôt qu'une ligne : la carte s'élève un peu.
                    let bosse = (r0.center().distance(r1.center()) * 0.18).min(60.0) * (PI * e).sin();
                    let centre = r0.center().lerp(r1.center(), e) - vec2(0.0, bosse);
                    let taille = r0.size() + (r1.size() - r0.size()) * e;
                    let r = Rect::from_center_size(centre, taille);
                    match carte {
                        Some(k) => peindre_carte(p, r, *k, 0.0),
                        None => peindre_dos(p, r),
                    }
                }
                Effet::Annonce { qui: q } => {
                    let sous_titre = if *q == moi { "toi !".to_string() } else { qui(*q).nom };
                    banniere(&p.with_clip_rect(milieu), milieu, x, "DERNIÈRE CARTE !", &sous_titre, c::WARN);
                }
                Effet::Contre { cible, n } => {
                    let sous_titre = if *cible == moi {
                        format!("tu pioches {n}")
                    } else {
                        format!("{} pioche {n}", qui(*cible).nom)
                    };
                    banniere(&p.with_clip_rect(milieu), milieu, x * 1.4, "CONTRÉ !", &sous_titre, c::DANGER);
                    // « +2 » qui monte de la place de la cible.
                    if let Some(r) = self.lieu(Lieu::Siege(*cible), moi) {
                        let alpha = (1.0 - x).clamp(0.0, 1.0);
                        p.text(
                            r.center() - vec2(0.0, 20.0 + 40.0 * x),
                            Align2::CENTER_CENTER,
                            format!("+{n}"),
                            FontId::proportional(26.0),
                            c::DANGER.gamma_multiply(alpha),
                        );
                    }
                }
                Effet::Echange { par, avec } => {
                    let nom = |id: UserId| if id == moi { "toi".to_string() } else { qui(id).nom };
                    let sous_titre = format!("{} et {}", nom(*par), nom(*avec));
                    banniere(&p.with_clip_rect(milieu), milieu, x, "ÉCHANGE !", &sous_titre, c::ACCENT);
                }
                Effet::Tourne { par } => {
                    let sous_titre = if *par == moi { "ton 0".to_string() } else { format!("le 0 de {}", qui(*par).nom) };
                    banniere(&p.with_clip_rect(milieu), milieu, x, "ÇA TOURNE !", &sous_titre, teinte(CouleurCarte::Bleu));
                }
                Effet::Victoire => confettis(p, zone, x),
            }
        }
    }

    /// Où est un lieu à cette image.
    fn lieu(&self, l: Lieu, moi: UserId) -> Option<Rect> {
        let au_centre_de = |z: Rect| Rect::from_center_size(z.center(), vec2(64.0, 94.0));
        match l {
            Lieu::Pioche => self.reperes.pioche,
            Lieu::Defausse => self.reperes.defausse,
            Lieu::Fixe(r) => Some(r),
            Lieu::MaCarte(c) => self.reperes.main.get(&c).copied().or(self.reperes.zone_main.map(au_centre_de)),
            Lieu::Siege(id) if id == moi => self.reperes.zone_main.map(au_centre_de),
            Lieu::Siege(id) => self.reperes.sieges.get(&id).copied(),
        }
    }

    /// Une carte qui vole encore vers la défausse : elle montre ce qu'il y
    /// avait dessous jusqu'à son arrivée.
    fn defausse_en_vol(&self, dessus: Carte, now: Instant) -> Option<Option<Carte>> {
        self.anims.iter().find_map(|a| match &a.effet {
            Effet::Vol { carte: Some(k), vers: Lieu::Defausse, sous, .. } if *k == dessus && a.avancement(now) < 1.0 => Some(*sous),
            _ => None,
        })
    }

    /// Les cartes de ma main qui n'y sont pas encore arrivées.
    fn cartes_en_vol(&self, now: Instant) -> Vec<Carte> {
        self.anims
            .iter()
            .filter(|a| a.avancement(now) < 1.0)
            .filter_map(|a| match a.effet {
                Effet::Vol { vers: Lieu::MaCarte(c), .. } => Some(c),
                _ => None,
            })
            .collect()
    }

    /// Le tremblement d'une place qu'on vient de contrer.
    fn secousse(&self, id: UserId, now: Instant) -> f32 {
        self.anims
            .iter()
            .find_map(|a| match a.effet {
                Effet::Contre { cible, .. } if cible == id => {
                    let x = a.avancement(now);
                    (0.0..0.45).contains(&x).then(|| (x * 70.0).sin() * 7.0 * (1.0 - x / 0.45))
                }
                _ => None,
            })
            .unwrap_or(0.0)
    }
}

/// Une bannière qui éclate au milieu de `zone` : elle arrive grande, se pose
/// en rebondissant, rayonne, puis s'efface. `x` va de 0 à 1.
fn banniere(p: &Painter, zone: Rect, x: f32, titre: &str, sous_titre: &str, teinte: Color32) {
    if !(0.0..1.0).contains(&x) {
        return;
    }
    let arrivee = (x / 0.12).min(1.0);
    // Un dépassement avant de se poser (« ease out back »).
    let rebond = 1.0 + 2.7 * (arrivee - 1.0).powi(3) + 1.7 * (arrivee - 1.0).powi(2);
    // Jamais plus large que la zone : fenêtre étroite, elle rapetisse.
    let echelle = (1.5 - 0.5 * rebond) * (zone.width() / 400.0).min(1.0);
    let alpha = if x > 0.75 { 1.0 - (x - 0.75) / 0.25 } else { 1.0 };
    let centre = zone.center();
    // Les rayons, qui tournent doucement.
    for k in 0..16 {
        let a = k as f32 * TAU / 16.0 + x * 1.4;
        let dir = vec2(a.cos(), a.sin());
        let longueur = (if k % 2 == 0 { 170.0 } else { 120.0 } as f32).min(zone.height() * 0.6);
        p.line_segment(
            [centre + dir * 60.0 * echelle, centre + dir * longueur * echelle],
            Stroke::new(3.0, teinte.gamma_multiply(0.22 * alpha)),
        );
    }
    let taille = vec2(380.0, 78.0) * echelle;
    let r = Rect::from_center_size(centre, taille);
    p.rect_filled(r, rayon::PILULE * 2, c::BG_DEEP.gamma_multiply(0.92 * alpha));
    p.rect_stroke(r, rayon::PILULE * 2, Stroke::new(3.0, teinte.gamma_multiply(alpha)), StrokeKind::Inside);
    texte_gras(p, centre - vec2(0.0, 10.0 * echelle), titre, 30.0 * echelle, teinte.gamma_multiply(alpha));
    p.text(
        centre + vec2(0.0, 20.0 * echelle),
        Align2::CENTER_CENTER,
        sous_titre,
        FontId::proportional(taille_finie(texte::CORPS * echelle)),
        c::TEXT.gamma_multiply(alpha),
    );
}

/// Une taille de police arrondie à 2 px près. egui garde les glyphes de
/// chaque taille dans un atlas qu'il ne vide que plein : une carte qui vole
/// ou une bannière qui rebondit, à une taille neuve à chaque image, le
/// faisait gonfler de manche en manche jusqu'au gigaoctet.
fn taille_finie(taille: f32) -> f32 {
    ((taille / 2.0).round() * 2.0).max(2.0)
}

/// Du texte de bannière, plein : la police proportionnelle, épaissie.
fn texte_gras(p: &Painter, centre: Pos2, mot: &str, taille: f32, encre: Color32) {
    let taille = taille_finie(taille);
    let font = FontId::proportional(taille);
    let e = (taille * 0.03).max(0.5);
    for d in [vec2(-e, 0.0), vec2(e, 0.0), vec2(0.0, -e), vec2(0.0, e), Vec2::ZERO] {
        p.text(centre + d, Align2::CENTER_CENTER, mot, font.clone(), encre);
    }
}

/// Des confettis aux quatre couleurs, qui tombent sur toute la table. Le
/// hasard est fixé par l'indice : chaque image les retrouve à leur place.
fn confettis(p: &Painter, zone: Rect, x: f32) {
    let t = x * VICTOIRE;
    let alpha = if x > 0.8 { 1.0 - (x - 0.8) / 0.2 } else { 1.0 };
    let mut graine: u32 = 0x9e37_79b9;
    let mut hasard = || {
        graine ^= graine << 13;
        graine ^= graine >> 17;
        graine ^= graine << 5;
        (graine % 10_000) as f32 / 10_000.0
    };
    for i in 0..110 {
        let depart = zone.left() + zone.width() * hasard();
        let vitesse = 0.6 + 0.8 * hasard();
        let derive = (hasard() - 0.5) * 90.0;
        let haut = zone.top() - 30.0 - 120.0 * hasard();
        let phase = hasard() * TAU;
        let y = haut + 140.0 * vitesse * t + 70.0 * t * t;
        if y > zone.bottom() + 20.0 {
            continue;
        }
        let px = depart + derive * t + 10.0 * (t * 5.0 + phase).sin();
        // Il tourne sur lui-même : sa largeur bat.
        let largeur = 7.0 * (t * 9.0 + phase).cos().abs();
        let couleur = teinte(CouleurCarte::TOUTES[i % 4]).gamma_multiply(alpha);
        p.rect_filled(Rect::from_center_size(pos2(px, y), vec2(largeur.max(1.5), 10.0)), 1.0, couleur);
    }
}

/// Mon tour, et ma place tenue par moi plutôt que par un bot.
fn a_moi_de_jouer(t: &TableCartes, moi: Option<UserId>) -> bool {
    t.phase == PhaseCartes::EnCours
        && moi.is_some()
        && t.tour == moi
        && moi.is_some_and(|m| t.joueur(m).is_some_and(|j| !j.bot))
}

/// Qui regarde sans place. Une manche en cours ne s'ouvre qu'aux places
/// que tient un bot : on les prend avec leurs cartes. Sans bot, on attend
/// la manche suivante.
fn spectateur(
    ui: &mut egui::Ui,
    rect: Rect,
    t: &TableCartes,
    qui: &dyn Fn(UserId) -> Identite,
    actions: &mut Vec<ActionCartes>,
) {
    let bots: Vec<UserId> = t.joueurs.iter().filter(|j| j.bot).map(|j| j.user_id).collect();
    let mut ui = colonne_centree(ui, rect.shrink2(vec2(0.0, espace::M)));
    if t.en_attente {
        ui.add_space(espace::XL);
        ui.label(
            RichText::new("Quelqu'un a pris ta place pendant ton absence : tu rejoues à la manche suivante.")
                .color(c::TEXT_DIM)
                .size(texte::CORPS),
        );
        return;
    }
    if bots.is_empty() {
        ui.add_space(espace::XL);
        ui.label(
            RichText::new("Tu regardes la manche — tu pourras t'asseoir à la suivante.")
                .color(c::TEXT_DIM)
                .size(texte::CORPS),
        );
        return;
    }
    ui.label(
        RichText::new("Un bot tient une place : prends-la, tu récupères sa main.")
            .color(c::TEXT)
            .size(texte::CORPS),
    );
    ui.add_space(espace::S);
    for id in bots {
        let nom = abreger(&qui(id).nom, 16);
        if ui::primary_button(&mut ui, Some(Icon::Cartes), &format!("Prendre la place de {nom}"), None).clicked() {
            actions.push(ActionCartes::Reprendre { place: id });
        }
        ui.add_space(espace::XS);
    }
}

/// Ce dont une carte jouable se relève dans ma main.
const LEVEE: f32 = 14.0;
/// Ce que la rangée du fond montre au-dessus de celle de devant, en part
/// de la hauteur d'une carte (en plus de la levée) : de quoi lire ses coins.
const DECALAGE: f32 = 0.30;

/// L'écart sous lequel le coin d'une carte de hauteur `h` disparaît sous
/// sa voisine : l'indice s'arrête vers 15 % de la hauteur depuis le bord.
fn pas_lisible(h: f32) -> f32 {
    0.17 * h + 4.0
}

/// La place de chaque carte de ma main dans `rect`, dans l'ordre de la
/// main. Une rangée tant que le coin de chaque carte reste lisible ; au
/// delà — une guerre de +4 qui a mal tourné —, deux : celle du fond,
/// remontée, ne montre que son haut, où sont les coins, et la carte qu'on
/// survole passe devant tout.
fn disposer_main(n: usize, rect: Rect) -> Vec<Rect> {
    let place = rect.width() - 24.0;
    let rangee = |k: usize, h: f32, bas: f32| -> Vec<Rect> {
        let w = h * RATIO;
        let pas = if k <= 1 { 0.0 } else { (w + 8.0).min((place - w) / (k - 1) as f32) };
        let largeur = pas * k.saturating_sub(1) as f32 + w;
        let x0 = rect.center().x - largeur / 2.0;
        (0..k).map(|i| Rect::from_min_size(pos2(x0 + i as f32 * pas, bas - h), vec2(w, h))).collect()
    };
    let bas = rect.bottom() - 2.0;
    let h1 = (rect.height() - 18.0).clamp(80.0, 156.0);
    if n <= 1 || (place - h1 * RATIO) / (n - 1) as f32 >= pas_lisible(h1) {
        return rangee(n, h1, bas);
    }
    // Deux rangées : la hauteur, levées comprises, tient dans la zone.
    let h2 = ((rect.height() - 2.0 * LEVEE - 12.0) / (1.0 + DECALAGE)).clamp(56.0, 156.0);
    let fond = n.div_ceil(2);
    let mut cases = rangee(fond, h2, bas - h2 * DECALAGE - LEVEE);
    cases.extend(rangee(n - fond, h2, bas));
    cases
}

/// Les règles de la maison, en quatre lignes.
fn regles(ui: &mut egui::Ui) {
    ui::card(ui, |ui| {
        ui.set_min_width(ui.available_width());
        // La colonne qui l'accueille centre tout : les règles se lisent
        // mieux alignées à gauche.
        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.label(RichText::new("Les règles de la maison").color(c::TEXT).strong());
            ui.add_space(espace::XS);
            for ligne in [
                "Pose une carte de même couleur, de même valeur, ou un joker.",
                "Les +2 et +4 se cumulent : un +2 sur un +2, un +4 sur tout. Qui ne renchérit pas pioche le total.",
                "À deux cartes, clique « Dernière carte ! ». Sinon, le premier qui te contre te fait piocher 2.",
                "30 secondes par tour, sinon tu pioches et ton tour passe. Trois fois de suite, et un bot joue à ta place.",
                "Qui part laisse sa main à un bot ; un pote qui arrive peut prendre sa place, cartes comprises.",
            ] {
                ui.label(RichText::new(format!("•  {ligne}")).color(c::TEXT_DIM).size(texte::COURANT));
            }
        });
    });
}

/// Les règles en plus : l'hôte les allume et les éteint, entre deux
/// manches ; les autres voient lesquelles sont en jeu.
fn regles_en_plus(ui: &mut egui::Ui, t: &TableCartes, moi: UserId, nom_hote: &str, actions: &mut Vec<ActionCartes>) {
    let hote = moi == t.hote && t.joueur(moi).is_some();
    ui::card(ui, |ui| {
        // Toute la largeur, comme les règles de la maison en dessous.
        ui.set_min_width(ui.available_width());
        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.label(RichText::new("Les règles en plus").color(c::TEXT).strong());
            ui.add_space(espace::XS);
            if hote {
                let mut regles = t.regles;
                let mut change = false;
                for (en_jeu, titre, detail) in [
                    (&mut regles.sept, "Le 7 échange", "Qui pose un 7 échange sa main avec le joueur de son choix."),
                    (
                        &mut regles.zero,
                        "Le 0 fait tourner",
                        "Qui pose un 0 fait passer toutes les mains au voisin, dans le sens du jeu.",
                    ),
                ] {
                    change |= ui::interrupteur(ui, en_jeu, titre).changed();
                    ui.label(RichText::new(detail).color(c::TEXT_DIM).size(texte::COURANT));
                    ui.add_space(espace::XS);
                }
                if change {
                    actions.push(ActionCartes::Regler { regles });
                }
            } else {
                let texte = match liste_des_regles(t.regles) {
                    Some(liste) => format!("En jeu : {liste}."),
                    None => "Aucune : les règles de la maison, rien de plus.".to_string(),
                };
                ui.label(RichText::new(texte).color(c::TEXT_DIM).size(texte::COURANT));
                ui::hint(ui, format!("{nom_hote} les choisit entre deux manches."));
            }
        });
    });
}

/// « le 7 échange, le 0 fait tourner » — `None` sans règle en plus.
fn liste_des_regles(regles: ReglesCartes) -> Option<String> {
    let mut liste = Vec::new();
    if regles.sept {
        liste.push("le 7 échange");
    }
    if regles.zero {
        liste.push("le 0 fait tourner");
    }
    (!liste.is_empty()).then(|| liste.join(", "))
}

/// Comme [`colonne_centree`], mais qui défile quand ce qu'elle tient
/// dépasse.
fn colonne_defilante(ui: &mut egui::Ui, rect: Rect, cle: &str, contenu: impl FnOnce(&mut egui::Ui)) {
    let mut sous = colonne_centree(ui, rect);
    egui::ScrollArea::vertical()
        .id_salt(("colonne_cartes", cle))
        .auto_shrink([false, false])
        .show(&mut sous, |ui| {
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), contenu);
        });
}

/// Une colonne de widgets centrés dans `rect`, sans prendre de place au
/// parent : la zone de la fenêtre est déjà réservée.
fn colonne_centree(ui: &mut egui::Ui, rect: Rect) -> egui::Ui {
    let mut sous = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Center)),
    );
    sous.set_clip_rect(rect);
    sous
}

/// Un événement de la table, en une phrase.
pub fn raconter(e: &EvenementCartes, nom: &dyn Fn(UserId) -> String) -> String {
    match e {
        EvenementCartes::Assis { par } => format!("{} s'assoit", nom(*par)),
        EvenementCartes::Leve { par } => format!("{} se lève", nom(*par)),
        EvenementCartes::Manche => "Nouvelle manche : 7 cartes chacun".into(),
        EvenementCartes::Joue { par, carte, couleur } => {
            let article = if matches!(carte.valeur(), ValeurCarte::Chiffre(_)) { "le" } else { "un" };
            match couleur {
                Some(couleur) => format!("{} joue {article} {} : {}", nom(*par), carte.nom(), couleur.nom()),
                None => format!("{} joue {article} {}", nom(*par), carte.nom()),
            }
        }
        EvenementCartes::Pioche { par, n: 0, .. } => format!("{} ne trouve plus rien à piocher", nom(*par)),
        EvenementCartes::Pioche { par, n: 1, manque: 0 } => format!("{} pioche", nom(*par)),
        EvenementCartes::Pioche { par, n, manque: 0 } => format!("{} pioche {n} cartes", nom(*par)),
        EvenementCartes::Pioche { par, n, manque } => {
            format!("{} pioche {n} carte{} — le jeu est épuisé, {manque} de moins", nom(*par), if *n > 1 { "s" } else { "" })
        }
        EvenementCartes::Passe { par } => format!("{} garde sa carte et passe", nom(*par)),
        EvenementCartes::Annonce { par } => format!("{} : « Dernière carte ! »", nom(*par)),
        EvenementCartes::Contre { par, cible, n } => {
            // Un serveur sans le compte (n = 0) ne peut avoir tiré que la
            // pénalité entière.
            let n = if *n == 0 { PENALITE_ANNONCE as u8 } else { *n };
            format!("{} contre {} : +{n}", nom(*par), nom(*cible))
        }
        EvenementCartes::Temps { par } => format!("Temps écoulé pour {}", nom(*par)),
        EvenementCartes::Melange => "La défausse est mélangée dans la pioche".into(),
        EvenementCartes::Sorti { par } => format!("{} sort de la manche", nom(*par)),
        EvenementCartes::Remplace { par } => format!("Un bot prend la place de {}", nom(*par)),
        EvenementCartes::Reprend { par, place } if par == place => format!("{} reprend sa place", nom(*par)),
        EvenementCartes::Reprend { par, place } => {
            format!("{} prend la place de {}, et ses cartes", nom(*par), nom(*place))
        }
        EvenementCartes::Gagne { par, bot: true } => format!("Le bot de {} gagne la manche !", nom(*par)),
        EvenementCartes::Gagne { par, .. } => format!("{} gagne la manche !", nom(*par)),
        EvenementCartes::Echange { par, avec } => format!("{} échange sa main avec {}", nom(*par), nom(*avec)),
        EvenementCartes::Tourne { .. } => "Chaque main passe au voisin".into(),
        EvenementCartes::Regles { par, regles } => match liste_des_regles(*regles) {
            Some(liste) => format!("{} règle la table : {liste}", nom(*par)),
            None => format!("{} enlève les règles en plus", nom(*par)),
        },
        EvenementCartes::Inconnu => "Un coup que cette version ne sait pas raconter".into(),
    }
}

fn pluriel(n: u32) -> &'static str {
    if n > 1 {
        "s"
    } else {
        ""
    }
}

/// Un pseudo coupé à `max` caractères, points de suspension compris.
fn abreger(nom: &str, max: usize) -> String {
    if nom.chars().count() <= max {
        return nom.to_string();
    }
    let mut court: String = nom.chars().take(max.saturating_sub(1)).collect();
    court.push('…');
    court
}

/// Un petit mot en capitales sur une pilule teintée, centré sur `centre`.
fn pastille(p: &Painter, centre: Pos2, mot: &str, teinte: Color32) {
    let galley = p.layout_no_wrap(mot.to_string(), FontId::proportional(texte::MINUSCULE), teinte);
    let r = Rect::from_center_size(centre, galley.size() + vec2(12.0, 4.0));
    p.rect_filled(r, rayon::PILULE, c::translucide(teinte, 40));
    p.galley(r.center() - galley.size() / 2.0, galley, teinte);
}

/// Vert tant qu'il y a le temps, orange puis rouge à la fin.
fn teinte_du_temps(fraction: f32) -> Color32 {
    if fraction > 1.0 / 3.0 {
        c::ACCENT
    } else if fraction > 1.0 / 6.0 {
        c::WARN
    } else {
        c::DANGER
    }
}

/// Un arc de cercle, de l'angle `de` à l'angle `a` (radians, sens des
/// aiguilles d'une montre à l'écran).
fn arc(p: &Painter, centre: Pos2, r: f32, de: f32, a: f32, stroke: Stroke) {
    let n = (((a - de).abs() / TAU) * 48.0).ceil().max(2.0) as usize;
    let points: Vec<Pos2> = (0..=n)
        .map(|i| {
            let t = de + (a - de) * i as f32 / n as f32;
            centre + r * vec2(t.cos(), t.sin())
        })
        .collect();
    p.add(Shape::line(points, stroke));
}

/// Le sens du jeu : deux arcs d'ellipse autour des tas, chacun sa flèche.
fn sens_du_jeu(p: &Painter, centre: Pos2, rx: f32, ry: f32, horaire: bool, rotation: f32) {
    let stroke = Stroke::new(2.0, c::translucide(c::TEXT_FAINT, 110));
    for debut in [-PI * 0.80 + rotation, PI * 0.20 + rotation] {
        let n = 24;
        let points: Vec<Pos2> = (0..=n)
            .map(|i| {
                let t = debut + PI * 0.6 * i as f32 / n as f32;
                centre + vec2(rx * t.cos(), ry * t.sin())
            })
            .collect();
        let (bout, avant) = if horaire { (points[n], points[n - 1]) } else { (points[0], points[1]) };
        p.add(Shape::line(points, stroke));
        fleche(p, avant, bout, stroke);
    }
}

/// Une pointe de flèche en `bout`, venue de `avant`.
fn fleche(p: &Painter, avant: Pos2, bout: Pos2, stroke: Stroke) {
    let dir = (bout - avant).normalized();
    let perp = vec2(-dir.y, dir.x);
    let h = stroke.width * 4.0;
    p.add(Shape::convex_polygon(
        vec![bout + dir * h * 0.4, bout - dir * h + perp * h * 0.7, bout - dir * h - perp * h * 0.7],
        stroke.color,
        Stroke::NONE,
    ));
}

// --- Les cartes ---

/// Les quatre couleurs du jeu, vives sur le fond sombre de ki-chat.
pub fn teinte(couleur: CouleurCarte) -> Color32 {
    match couleur {
        CouleurCarte::Rouge => Color32::from_rgb(0xe5, 0x48, 0x4d),
        CouleurCarte::Jaune => Color32::from_rgb(0xf0, 0xb4, 0x29),
        CouleurCarte::Vert => Color32::from_rgb(0x2b, 0xb3, 0x64),
        CouleurCarte::Bleu => Color32::from_rgb(0x3b, 0x82, 0xf6),
    }
}

fn coin(rect: Rect) -> CornerRadius {
    CornerRadius::same((rect.width() * 0.12).round().clamp(2.0, 40.0) as u8)
}

/// Une carte, face visible. `voile` l'assombrit (0 à 1) : ce qui ne se
/// pose pas.
pub fn peindre_carte(p: &Painter, rect: Rect, carte: Carte, voile: f32) {
    let r = coin(rect);
    let fond = carte.couleur().map(teinte).unwrap_or(Color32::from_rgb(0x1d, 0x23, 0x2c));
    p.rect_filled(rect.translate(vec2(0.0, rect.height() * 0.025)), r, Color32::from_black_alpha(80));
    p.rect_filled(rect, r, fond);
    p.rect_stroke(rect, r, Stroke::new((rect.width() * 0.03).max(1.0), Color32::from_white_alpha(215)), StrokeKind::Inside);
    // Le panneau du milieu, un ton plus sombre : la valeur s'y détache. Il
    // laisse les coins libres.
    let panneau = Rect::from_center_size(rect.center(), vec2(rect.width() * 0.64, rect.height() * 0.56));
    let rd = CornerRadius::same((rect.width() * 0.08).round().clamp(1.0, 30.0) as u8);
    if !carte.sauvage() {
        p.rect_filled(panneau, rd, Color32::from_black_alpha(34));
    }
    p.rect_stroke(panneau, rd, Stroke::new(1.0, Color32::from_white_alpha(80)), StrokeKind::Inside);
    peindre_symbole(p, rect.center(), rect.height() * 0.40, carte);
    // Les coins, tout près du bord : c'est ce qui reste d'une carte serrée
    // entre ses voisines dans une grosse main (voir `pas_lisible`).
    let s = rect.height() * 0.14;
    let bord = vec2(rect.width() * 0.08 + s * 0.40, rect.height() * 0.06 + s * 0.50);
    peindre_symbole(p, rect.min + bord, s, carte);
    peindre_symbole(p, rect.max - bord, s, carte);
    if voile > 0.0 {
        p.rect_filled(rect, r, Color32::from_black_alpha((voile * 170.0) as u8));
    }
}

/// Le dos d'une carte, aux couleurs de ki-chat.
pub fn peindre_dos(p: &Painter, rect: Rect) {
    let r = coin(rect);
    p.rect_filled(rect.translate(vec2(0.0, rect.height() * 0.025)), r, Color32::from_black_alpha(80));
    p.rect_filled(rect, r, Color32::from_rgb(0x15, 0x1a, 0x21));
    p.rect_stroke(rect, r, Stroke::new((rect.width() * 0.03).max(1.0), Color32::from_white_alpha(170)), StrokeKind::Inside);
    let dedans = rect.shrink(rect.width() * 0.13);
    let rd = CornerRadius::same((rect.width() * 0.07).round().clamp(1.0, 30.0) as u8);
    p.rect_filled(dedans, rd, c::melanger(c::BG_DEEP, c::ACCENT, 0.14));
    p.rect_stroke(dedans, rd, Stroke::new(1.0, c::translucide(c::ACCENT, 150)), StrokeKind::Inside);
    if rect.height() >= 40.0 {
        texte_epais(p, rect.center(), "KI", rect.height() * 0.24, c::ACCENT);
    }
}

/// La valeur d'une carte, dessinée à la taille `s` autour de `centre`.
fn peindre_symbole(p: &Painter, centre: Pos2, s: f32, carte: Carte) {
    let encre = Color32::WHITE;
    match carte.valeur() {
        ValeurCarte::Chiffre(n) => texte_epais(p, centre, &n.to_string(), s, encre),
        ValeurCarte::PlusDeux => texte_epais(p, centre, "+2", s * 0.78, encre),
        ValeurCarte::Passe => {
            let r = s * 0.32;
            let stroke = Stroke::new((s * 0.09).max(1.2), encre);
            p.circle_stroke(centre, r, stroke);
            let d = r * 0.68;
            p.line_segment([centre + vec2(-d, -d), centre + vec2(d, d)], stroke);
        }
        ValeurCarte::Inverse => {
            let stroke = Stroke::new((s * 0.085).max(1.2), encre);
            let dir = vec2(1.0, -1.0).normalized();
            let perp = vec2(1.0, 1.0).normalized();
            let (l, o) = (s * 0.34, s * 0.12);
            let a0 = centre - dir * l - perp * o;
            let a1 = centre + dir * l - perp * o;
            p.line_segment([a0, a1 - dir * stroke.width * 2.0], stroke);
            fleche(p, a0, a1, Stroke::new(stroke.width * 0.8, encre));
            let b0 = centre + dir * l + perp * o;
            let b1 = centre - dir * l + perp * o;
            p.line_segment([b0, b1 + dir * stroke.width * 2.0], stroke);
            fleche(p, b0, b1, Stroke::new(stroke.width * 0.8, encre));
        }
        ValeurCarte::Joker => roue(p, centre, s * 0.36, None),
        ValeurCarte::PlusQuatre => {
            roue(p, centre, s * 0.40, Some(s * 0.27));
            texte_epais(p, centre, "+4", s * 0.32, encre);
        }
    }
}

/// Les quatre couleurs en roue : pleine pour le joker, en anneau pour le +4.
fn roue(p: &Painter, centre: Pos2, r: f32, trou: Option<f32>) {
    let ordre = [CouleurCarte::Rouge, CouleurCarte::Bleu, CouleurCarte::Vert, CouleurCarte::Jaune];
    for (k, couleur) in ordre.into_iter().enumerate() {
        let a0 = PI * (k as f32 * 0.5 - 1.0);
        let points = |rr: f32| -> Vec<Pos2> {
            (0..=12)
                .map(|i| {
                    let a = a0 + FRAC_PI_2 * i as f32 / 12.0;
                    centre + rr * vec2(a.cos(), a.sin())
                })
                .collect()
        };
        match trou {
            None => {
                let mut v = vec![centre];
                v.extend(points(r));
                p.add(Shape::convex_polygon(v, teinte(couleur), Stroke::NONE));
            }
            Some(t) => {
                p.add(Shape::line(points((r + t) / 2.0), Stroke::new(r - t, teinte(couleur))));
            }
        }
    }
}

/// Du texte bien noir sur la carte : la police monospace (Hack), plus franche
/// que la proportionnelle, épaissie de quelques passages décalés, sur une
/// ombre.
fn texte_epais(p: &Painter, centre: Pos2, mot: &str, taille: f32, encre: Color32) {
    let taille = taille_finie(taille);
    let font = FontId::monospace(taille);
    let e = (taille * 0.03).max(0.4);
    p.text(centre + vec2(e * 1.6, e * 2.2), Align2::CENTER_CENTER, mot, font.clone(), Color32::from_black_alpha(100));
    for d in [vec2(-e, 0.0), vec2(e, 0.0), vec2(0.0, -e), vec2(0.0, e), Vec2::ZERO] {
        p.text(centre + d, Align2::CENTER_CENTER, mot, font.clone(), encre);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La zone de ma main dans la fenêtre à sa taille par défaut.
    fn zone() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(870.0, 158.0))
    }

    /// L'écart entre deux cartes voisines d'une même rangée.
    fn pas(cases: &[Rect]) -> f32 {
        cases.windows(2).filter(|p| p[0].top() == p[1].top()).map(|p| p[1].left() - p[0].left()).fold(f32::MAX, f32::min)
    }

    fn table_de_test(main: &[u8], dessus: u8) -> TableCartes {
        TableCartes {
            phase: PhaseCartes::EnCours,
            joueurs: (1..=3).map(|id| JoueurCartes { user_id: id, cartes: 7, ..Default::default() }).collect(),
            main: main.iter().map(|&n| Carte(n)).collect(),
            dessus: Some(Carte(dessus)),
            couleur: Some(CouleurCarte::Rouge),
            horaire: true,
            ..Default::default()
        }
    }

    #[test]
    fn ma_carte_posee_vole_de_sa_case_a_la_defausse() {
        let avant = table_de_test(&[5, 13], 2);
        let apres = table_de_test(&[13], 5);
        let case = Rect::from_min_size(pos2(10.0, 400.0), vec2(60.0, 90.0));
        let cases = HashMap::from([(Carte(5), case)]);
        let neufs = [EvenementCartes::Joue { par: 1, carte: Carte(5), couleur: None }];
        let effets = effets_de(&neufs, &avant, &apres, Some(1), &cases);
        assert_eq!(effets.len(), 1);
        assert_eq!(
            effets[0].0,
            Effet::Vol { carte: Some(Carte(5)), de: Lieu::Fixe(case), vers: Lieu::Defausse, sous: Some(Carte(2)) }
        );
        // Celle d'un autre part de sa place.
        let neufs = [EvenementCartes::Joue { par: 2, carte: Carte(5), couleur: None }];
        let effets = effets_de(&neufs, &avant, &apres, Some(1), &cases);
        assert!(matches!(effets[0].0, Effet::Vol { de: Lieu::Siege(2), .. }));
    }

    #[test]
    fn mes_cartes_piochees_volent_vers_leur_case() {
        let avant = table_de_test(&[13], 2);
        let apres = table_de_test(&[13, 40, 77], 2);
        let neufs = [EvenementCartes::Pioche { par: 1, n: 2, manque: 0 }];
        let vers: Vec<Lieu> = effets_de(&neufs, &avant, &apres, Some(1), &HashMap::new())
            .into_iter()
            .map(|(e, _, _)| match e {
                Effet::Vol { vers, carte: None, .. } => vers,
                autre => panic!("{autre:?}"),
            })
            .collect();
        assert_eq!(vers, vec![Lieu::MaCarte(Carte(40)), Lieu::MaCarte(Carte(77))]);
        // Une pioche énorme ne fait voler que quelques cartes.
        let neufs = [EvenementCartes::Pioche { par: 2, n: 24, manque: 0 }];
        assert_eq!(effets_de(&neufs, &avant, &apres, Some(1), &HashMap::new()).len(), VOLS_MAX as usize);
    }

    #[test]
    fn la_donne_et_les_grands_moments() {
        let avant = TableCartes { phase: PhaseCartes::Attente, ..table_de_test(&[], 2) };
        let apres = table_de_test(&[1, 2, 3, 4, 5, 6, 7], 9);
        let effets = effets_de(&[EvenementCartes::Manche], &avant, &apres, Some(1), &HashMap::new());
        assert_eq!(effets.len(), 3 * ki_protocol::CARTES_EN_MAIN, "une carte à chacun, sept fois");
        let chez_moi = effets.iter().filter(|(e, _, _)| matches!(e, Effet::Vol { vers: Lieu::MaCarte(_), .. })).count();
        assert_eq!(chez_moi, 7);
        assert!(effets.windows(2).all(|p| p[1].1 > p[0].1), "carte après carte");
        let neufs = [
            EvenementCartes::Annonce { par: 2 },
            EvenementCartes::Contre { par: 3, cible: 2, n: 2 },
            EvenementCartes::Gagne { par: 3, bot: false },
        ];
        let effets = effets_de(&neufs, &apres, &apres, Some(1), &HashMap::new());
        assert!(matches!(effets[0].0, Effet::Annonce { qui: 2 }));
        assert!(matches!(effets[1].0, Effet::Contre { cible: 2, n: 2 }));
        assert!(matches!(effets.last().unwrap().0, Effet::Victoire));
    }

    /// Le 7 de Kevin (2) échange sa main avec la mienne : ma main s'envole
    /// vers sa place, face visible, et la sienne arrive dans mes cases.
    #[test]
    fn un_echange_fait_voler_les_deux_mains() {
        let avant = table_de_test(&[5, 13, 40], 2);
        let mut apres = table_de_test(&[60, 62], 14);
        apres.joueurs[0].cartes = 2;
        apres.joueurs[1].cartes = 3;
        let case = Rect::from_min_size(pos2(10.0, 400.0), vec2(60.0, 90.0));
        let cases = HashMap::from([(Carte(5), case)]);
        let neufs = [
            EvenementCartes::Joue { par: 2, carte: Carte(14), couleur: None },
            EvenementCartes::Echange { par: 2, avec: 1 },
        ];
        let effets = effets_de(&neufs, &avant, &apres, Some(1), &cases);
        assert!(effets.iter().any(|(e, _, _)| *e == Effet::Echange { par: 2, avec: 1 }));
        let mes_cartes_parties: Vec<&Effet> = effets
            .iter()
            .map(|(e, _, _)| e)
            .filter(|e| matches!(e, Effet::Vol { carte: Some(_), vers: Lieu::Siege(2), .. }))
            .collect();
        assert_eq!(mes_cartes_parties.len(), 3, "mes trois cartes partent chez Kevin");
        assert!(mes_cartes_parties.contains(&&Effet::Vol { carte: Some(Carte(5)), de: Lieu::Fixe(case), vers: Lieu::Siege(2), sous: None }));
        let arrivees: Vec<Lieu> = effets
            .iter()
            .filter_map(|(e, _, _)| match e {
                Effet::Vol { de: Lieu::Siege(2), vers: v @ Lieu::MaCarte(_), .. } => Some(*v),
                _ => None,
            })
            .collect();
        assert_eq!(arrivees, vec![Lieu::MaCarte(Carte(60)), Lieu::MaCarte(Carte(62))]);

        // Vu d'ailleurs : des dos qui se croisent entre Kevin et moi.
        let effets = effets_de(&neufs, &avant, &apres, Some(3), &HashMap::new());
        assert!(effets.iter().any(|(e, _, _)| matches!(e, Effet::Vol { carte: None, de: Lieu::Siege(2), vers: Lieu::Siege(1), .. })));
        assert!(effets.iter().any(|(e, _, _)| matches!(e, Effet::Vol { carte: None, de: Lieu::Siege(1), vers: Lieu::Siege(2), .. })));
    }

    /// Le 0 : chaque main part chez le voisin, dans le sens du jeu.
    #[test]
    fn le_zero_fait_voler_chaque_main_au_voisin() {
        let avant = table_de_test(&[5, 13], 2);
        let apres = TableCartes { horaire: false, ..table_de_test(&[40, 41, 42], 0) };
        let neufs = [EvenementCartes::Joue { par: 2, carte: Carte(0), couleur: None }, EvenementCartes::Tourne { par: 2 }];
        let effets = effets_de(&neufs, &avant, &apres, Some(1), &HashMap::new());
        assert!(effets.iter().any(|(e, _, _)| *e == Effet::Tourne { par: 2 }));
        // Sens inverse : ma main va à 3, je reçois celle de 2.
        let vols: Vec<(Option<Carte>, Lieu, Lieu)> = effets
            .iter()
            .filter_map(|(e, _, _)| match e {
                Effet::Vol { carte, de, vers, .. } if *vers != Lieu::Defausse => Some((*carte, *de, *vers)),
                _ => None,
            })
            .collect();
        assert!(vols.contains(&(Some(Carte(5)), Lieu::Siege(1), Lieu::Siege(3))));
        assert!(vols.contains(&(None, Lieu::Siege(2), Lieu::MaCarte(Carte(40)))));
        assert!(vols.iter().any(|(_, de, vers)| *de == Lieu::Siege(3) && *vers == Lieu::Siege(2)));
        assert!(!vols.iter().any(|(_, de, vers)| *de == Lieu::Siege(1) && *vers == Lieu::Siege(2)));
    }

    #[test]
    fn un_echange_se_raconte_et_se_fait_entendre() {
        let nom = |id: UserId| ["", "drion", "Kevin", "Lina"][id as usize].to_string();
        assert_eq!(raconter(&EvenementCartes::Echange { par: 2, avec: 3 }, &nom), "Kevin échange sa main avec Lina");
        let regles = ReglesCartes { sept: true, zero: true };
        assert_eq!(
            raconter(&EvenementCartes::Regles { par: 1, regles }, &nom),
            "drion règle la table : le 7 échange, le 0 fait tourner"
        );
        let mut cartes = Cartes::default();
        cartes.recevoir(Some(TableCartes { numero: 3, ..table_de_test(&[13], 2) }), Some(1));
        let t = TableCartes { numero: 4, journal: vec![EvenementCartes::Tourne { par: 2 }], ..table_de_test(&[40], 2) };
        assert_eq!(cartes.recevoir(Some(t), Some(1)), vec![Son::Echange]);
    }

    #[test]
    fn changer_de_table_ne_rejoue_rien() {
        let mut cartes = Cartes { ouvert: true, ..Default::default() };
        cartes.recevoir(Some(TableCartes { salon: 3, numero: 25, ..table_de_test(&[13], 2) }), Some(1));
        // Le salon d'à côté, plus avancé : son fil n'est pas une suite.
        let autre = TableCartes {
            salon: 4,
            numero: 40,
            journal: vec![EvenementCartes::Annonce { par: 2 }, EvenementCartes::Gagne { par: 2, bot: false }],
            ..table_de_test(&[13], 2)
        };
        assert!(cartes.recevoir(Some(autre), Some(1)).is_empty());
        assert!(cartes.anims.is_empty());
    }

    #[test]
    fn la_donne_qui_ouvre_la_fenetre_s_anime() {
        let mut cartes = Cartes::default();
        cartes.recevoir(Some(TableCartes { phase: PhaseCartes::Attente, numero: 3, ..table_de_test(&[], 2) }), Some(1));
        assert!(!cartes.ouvert);
        let donne = TableCartes { journal: vec![EvenementCartes::Manche], numero: 4, manche: 1, ..table_de_test(&[1, 2, 3, 4, 5, 6, 7], 9) };
        cartes.recevoir(Some(donne), Some(1));
        assert!(cartes.ouvert);
        assert_eq!(cartes.anims.len(), 3 * ki_protocol::CARTES_EN_MAIN);
    }

    #[test]
    fn les_tailles_animees_restent_en_nombre_fini() {
        assert_eq!(taille_finie(31.3), 32.0);
        assert_eq!(taille_finie(30.9), 30.0);
        assert_eq!(taille_finie(0.4), 2.0);
        let tailles: std::collections::HashSet<u32> = (0..10_000).map(|i| taille_finie(i as f32 * 0.0157) as u32).collect();
        assert!(tailles.len() <= 80);
    }

    #[test]
    fn une_petite_main_s_etale() {
        let cases = disposer_main(7, zone());
        assert!(cases.iter().all(|r| r.top() == cases[0].top()));
        assert!(pas(&cases) > cases[0].width(), "sept cartes ne se chevauchent pas");
    }

    #[test]
    fn vingt_six_cartes_tiennent_sur_une_rangee_lisible() {
        let cases = disposer_main(26, zone());
        assert!(cases.iter().all(|r| r.top() == cases[0].top()));
        assert!(pas(&cases) >= pas_lisible(cases[0].height()));
        assert!(cases.iter().all(|r| zone().x_range().contains(r.left()) && r.right() <= zone().right()));
    }

    #[test]
    fn une_grosse_main_passe_sur_deux_rangees_lisibles() {
        for n in [30, 45, 60, 80] {
            let cases = disposer_main(n, zone());
            assert_eq!(cases.len(), n);
            let hauts: Vec<f32> = cases.iter().map(|r| r.top()).collect();
            assert!(hauts.iter().any(|&y| y != hauts[0]), "{n} cartes : deux rangées");
            let h = cases[0].height();
            assert!(pas(&cases) >= pas_lisible(h), "{n} cartes : coins lisibles ({} < {})", pas(&cases), pas_lisible(h));
            // Levée et survol compris, rien ne sort de la zone par le haut.
            let plus_haut = hauts.iter().fold(f32::MAX, |a, &b| a.min(b)) - LEVEE - 12.0;
            assert!(plus_haut >= zone().top() - 4.0, "{n} cartes : {plus_haut}");
            assert!(cases.iter().all(|r| r.left() >= zone().left() && r.right() <= zone().right()));
        }
    }
}
