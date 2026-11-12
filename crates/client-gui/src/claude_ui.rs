//! Le salon de Claude : la conversation privée d'un membre avec l'assistant
//! du serveur, et, pour le propriétaire, l'onglet d'administration qui dit
//! ce que chacun a consommé.
//!
//! Rien ne se décide ici : la clé, les droits de lecture, le plafond du jour
//! et le blocage sont au serveur. Le client montre ce qu'il en reçoit.

use std::time::{Duration, Instant};

use eframe::egui::{self, Sense, Vec2};
use ki_protocol::{ClientMsg, EtatClaude, MessageClaude, ModeleClaude, UsageClaude, UserId};
use ki_ui::fenetre::{Fenetre, Modale};
use ki_ui::flex::Flex;
use ki_ui::jetons::{espace, marge, rayon};
use ki_ui::tableau::{Colonne, Tableau};
use ki_ui::typo::Texte;

use crate::formats::{day_label, format_time};
use crate::icons::Icon;
use crate::saisie::menu_edition;
use crate::theme::{self, ACCENT};
use crate::ui::Ton;
use crate::{fil, icons, safe_name, ui, KiApp, PIED_MARGE};

/// Le temps laissé au serveur pour dire qu'il a pris la question. Passé ce
/// délai, elle revient dans la saisie : mieux qu'une attente sans fin.
const ACCUSE_MAX: Duration = Duration::from_secs(10);

/// De quoi commencer, devant une conversation vide.
const EXEMPLES: [&str; 3] = [
    "De quoi on a parlé aujourd'hui dans le salon général ?",
    "Qui est connecté en ce moment ?",
    "Comment va le groupe sur VALORANT cette semaine ?",
];

/// La conversation d'un membre que le propriétaire relit.
struct Relue {
    de: UserId,
    pseudo: String,
    /// `None` tant que le serveur ne l'a pas envoyée.
    messages: Option<Vec<MessageClaude>>,
}

/// Ce que le client retient de Claude.
#[derive(Default)]
pub(crate) struct Claude {
    /// Le serveur a un Claude (`Welcome.claude`).
    pub(crate) servi: bool,
    /// Le salon tient la place du chat.
    pub(crate) ouvert: bool,
    /// Une réponse est arrivée pendant qu'on regardait ailleurs.
    pub(crate) non_lu: bool,
    /// La conversation a été demandée au serveur, depuis cette connexion.
    demandee: bool,
    messages: Vec<MessageClaude>,
    etat: EtatClaude,
    saisie: String,
    /// La question partie, tant que le serveur n'a pas dit l'avoir prise.
    envoyee: Option<(String, Instant)>,
    /// Ce que Claude est en train de faire.
    etape: Option<String>,
    erreur: Option<String>,
    focus: bool,
    /// La confirmation d'effacement est à l'écran.
    effacement: bool,

    // --- Le propriétaire ---
    usages: Vec<UsageClaude>,
    modeles: Vec<ModeleClaude>,
    modele: String,
    par_jour: u32,
    /// Le plafond en cours de saisie, pas encore appliqué.
    plafond_saisi: u32,
    usages_demandes: bool,
    /// Le membre choisi dans le tableau.
    choisi: Option<UserId>,
    relue: Option<Relue>,
}

fn propre(mut message: MessageClaude) -> MessageClaude {
    message.texte = ki_protocol::safe_display(&message.texte, 64 * 1024);
    if message.de_claude {
        message.texte = puces(&message.texte);
    }
    message
}

/// Les listes à tirets de Claude, avec de vraies puces : « - » et « * » en
/// début de ligne deviennent « • ». Un bloc de code garde ses tirets.
fn puces(texte: &str) -> String {
    let mut dans_un_bloc = false;
    let lignes: Vec<String> = texte
        .lines()
        .map(|ligne| {
            let sans_retrait = ligne.trim_start();
            if sans_retrait.starts_with("```") {
                dans_un_bloc = !dans_un_bloc;
                return ligne.to_string();
            }
            match sans_retrait.strip_prefix("- ").or_else(|| sans_retrait.strip_prefix("* ")) {
                Some(reste) if !dans_un_bloc => {
                    let retrait = &ligne[..ligne.len() - sans_retrait.len()];
                    format!("{retrait}•  {reste}")
                }
                _ => ligne.to_string(),
            }
        })
        .collect();
    lignes.join("\n")
}

impl Claude {
    /// La session se ferme : ce qui venait du serveur s'oublie. Le texte en
    /// cours de saisie reste, comme le salon ouvert — une reconnexion les
    /// retrouve.
    pub(crate) fn oublier(&mut self) {
        let (saisie, ouvert) = (std::mem::take(&mut self.saisie), self.ouvert);
        *self = Self { saisie, ouvert, ..Self::default() };
    }

    /// Un serveur vient de nous accueillir. Rend vrai s'il faut lui
    /// redemander la conversation : le salon était resté ouvert.
    pub(crate) fn sur_bienvenue(&mut self, servi: bool) -> bool {
        self.servi = servi;
        self.demandee = servi && self.ouvert;
        self.demandee
    }

    /// Claude travaille pour nous : une question à la fois.
    fn occupe(&self) -> bool {
        self.etat.en_cours || self.envoyee.is_some()
    }

    pub(crate) fn sur_conversation(&mut self, moi: Option<UserId>, de: UserId, messages: Vec<MessageClaude>, etat: EtatClaude) {
        let messages: Vec<MessageClaude> = messages.into_iter().map(propre).collect();
        if let Some(relue) = self.relue.as_mut().filter(|r| r.de == de) {
            relue.messages = Some(messages.clone());
        }
        if Some(de) == moi {
            self.messages = messages;
            if !etat.en_cours {
                self.etape = None;
            }
            self.etat = etat;
        }
    }

    pub(crate) fn sur_message(&mut self, message: MessageClaude, etat: EtatClaude) {
        if message.de_claude {
            self.etape = None;
            self.non_lu = !self.ouvert;
        } else {
            // Le serveur a pris la question : elle n'a plus à revenir.
            self.envoyee = None;
            self.erreur = None;
        }
        self.messages.push(propre(message));
        self.etat = etat;
    }

    pub(crate) fn sur_etape(&mut self, texte: String) {
        self.etape = Some(ki_protocol::safe_display(&texte, 200));
    }

    pub(crate) fn sur_erreur(&mut self, message: String, etat: EtatClaude) {
        self.erreur = Some(ki_protocol::safe_display(&message, 400));
        self.etat = etat;
        self.etape = None;
        self.rendre_la_question();
    }

    pub(crate) fn sur_membres(&mut self, membres: Vec<UsageClaude>, modele: String, par_jour: u32, modeles: Vec<ModeleClaude>) {
        self.usages = membres;
        for u in &mut self.usages {
            u.pseudo = safe_name(&u.pseudo);
        }
        self.modele = modele;
        self.par_jour = par_jour;
        self.plafond_saisi = par_jour;
        self.modeles = modeles;
    }

    /// Une question refusée avant d'être retenue revient dans la saisie.
    fn rendre_la_question(&mut self) {
        if let Some((texte, _)) = self.envoyee.take() {
            if self.saisie.trim().is_empty() {
                self.saisie = texte;
            }
        }
    }

    /// Le nom à l'écran du modèle qui répond, s'il est connu.
    fn nom_du_modele(&self) -> Option<&str> {
        let id = if self.modele.is_empty() { &self.etat.modele } else { &self.modele };
        self.modeles.iter().find(|m| &m.id == id).map(|m| m.nom.as_str())
    }

    /// La zone de saisie. Rend la question à envoyer. `illimite` : le
    /// propriétaire, à qui l'on n'annonce pas un plafond qu'il n'a pas.
    fn zone_de_saisie(&mut self, ui: &mut egui::Ui, illimite: bool) -> Option<String> {
        let occupe = self.occupe();
        let raison = if self.etat.bloque {
            Some("Le propriétaire du serveur t'a coupé l'accès à Claude.".to_string())
        } else if !illimite && self.etat.par_jour > 0 && self.etat.reste == 0 && !occupe {
            Some(format!("Tu as posé tes {} questions d'aujourd'hui — reviens demain.", self.etat.par_jour))
        } else {
            None
        };
        let cadre = egui::Frame::NONE
            .fill(theme::BG_RAISED)
            .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
            .corner_radius(egui::CornerRadius::same(rayon::XL));
        // Sans le droit de demander, pas de champ : la raison à sa place.
        if let Some(raison) = raison {
            cadre.inner_margin(marge::symetrique(espace::L, espace::L)).show(ui, |ui| {
                ui.label(Texte::Libelle.riche(ui, raison));
            });
            return None;
        }
        let indice = if occupe {
            "Claude te répond…".to_string()
        } else if !illimite && self.etat.par_jour > 0 {
            let s = if self.etat.reste > 1 { "s" } else { "" };
            format!("Demande à Claude — encore {} question{s} aujourd'hui", self.etat.reste)
        } else {
            "Demande à Claude".to_string()
        };
        let mut envoyer = false;
        cadre.inner_margin(marge::symetrique(espace::S, espace::S)).show(ui, |ui| {
            let ecart = ui.spacing().item_spacing.x;
            Flex::ligne().ecart(ecart).show(ui, "claude-saisie", |f| {
                f.grandit(|ui| {
                    let lignes = self.saisie.lines().count().clamp(1, 6);
                    let taille = Vec2::new(ui.available_width(), 18.0 * lignes as f32 + 8.0);
                    let disposition = egui::Layout::centered_and_justified(ui.layout().main_dir());
                    let reponse = ui
                        .allocate_ui_with_layout(taille, disposition, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut self.saisie)
                                    .id(egui::Id::unique("claude-saisie-champ"))
                                    .char_limit(ki_protocol::CLAUDE_QUESTION_MAX)
                                    .desired_rows(lignes)
                                    .frame(egui::Frame::new().inner_margin(marge::symetrique(espace::XS, espace::XS)))
                                    .hint_text(indice),
                            )
                        })
                        .inner;
                    menu_edition(&reponse, &mut self.saisie, false);
                    // Entrée envoie, Maj+Entrée va à la ligne — comme dans
                    // un salon : le saut qu'egui vient d'écrire s'enlève.
                    if reponse.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift) {
                        if self.saisie.ends_with('\n') {
                            self.saisie.pop();
                        }
                        envoyer = true;
                        self.focus = true;
                    }
                    if std::mem::take(&mut self.focus) {
                        reponse.request_focus();
                    }
                });
                f.ui(|ui| {
                    let pret = !occupe && !self.saisie.trim().is_empty();
                    let aide = if occupe { "Une question à la fois : Claude te répond" } else { "Envoyer (Entrée) — Maj+Entrée pour aller à la ligne" };
                    if ui::Bouton::icone_seule(Icon::Send).couleur_si(pret.then_some(ACCENT)).bulle(aide).show(ui).clicked() {
                        envoyer = true;
                        self.focus = true;
                    }
                });
            });
        });
        let texte = self.saisie.trim().to_string();
        if !envoyer || occupe || texte.is_empty() {
            return None;
        }
        self.saisie.clear();
        self.erreur = None;
        self.etape = None;
        self.envoyee = Some((texte.clone(), Instant::now()));
        Some(texte)
    }
}

/// Celui dont on montre la conversation avec Claude.
struct Membre<'a> {
    id: UserId,
    pseudo: &'a str,
    photo: Option<&'a egui::TextureHandle>,
}

/// Un tour de la conversation : l'avatar, qui parle, l'heure, puis `corps`.
fn tour(ui: &mut egui::Ui, membre: &Membre<'_>, de_claude: bool, ts: Option<u64>, corps: impl FnOnce(&mut egui::Ui)) {
    const GOUTTIERE: f32 = 36.0;
    ui.add_space(7.0);
    ui.horizontal_top(|ui| {
        ui.add_space(espace::XL);
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(GOUTTIERE), Sense::hover());
        if de_claude {
            ui.painter().circle_filled(rect.center(), GOUTTIERE / 2.0, theme::alpha(ACCENT, 36));
            icons::draw(ui.painter(), rect.shrink(9.0), Icon::Star, ACCENT);
        } else {
            ui::paint_avatar(ui.painter(), rect, membre.pseudo, false, membre.photo, theme::BG_BASE);
        }
        ui.add_space(espace::L);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = espace::M;
                let (nom, couleur) = if de_claude { ("Claude", ACCENT) } else { (membre.pseudo, theme::color_for(membre.pseudo)) };
                ui.label(Texte::Corps.gras(ui, nom).color(couleur));
                if let Some(ts) = ts {
                    ui.label(Texte::Aide.riche(ui, format_time(ts)));
                }
            });
            ui.add_space(1.0);
            corps(ui);
        });
    });
    ui.add_space(espace::XXS);
}

/// La conversation, du plus ancien au plus récent, calée en bas. `attente` :
/// ce que Claude est en train de faire, s'il répond.
fn conversation(ui: &mut egui::Ui, cle: &str, messages: &[MessageClaude], membre: &Membre<'_>, attente: Option<&str>) {
    egui::ScrollArea::vertical().id_salt(cle).auto_shrink(false).stick_to_bottom(true).show(ui, |ui| {
        // Une réponse de Claude fait des paragraphes et des listes : les
        // lignes s'y serrent, une ligne vide suffit à séparer.
        ui.spacing_mut().item_spacing.y = espace::XS;
        let mut jour = String::new();
        for m in messages {
            let du_jour = day_label(m.ts);
            if du_jour != jour {
                fil::day_separator(ui, &du_jour);
                jour = du_jour;
            }
            tour(ui, membre, m.de_claude, Some(m.ts), |ui| {
                fil::message_body(ui, &m.texte, &[], None, (membre.id, m.ts));
            });
        }
        if let Some(etape) = attente {
            tour(ui, membre, true, None, |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(Texte::Libelle.riche_ton(ui, Ton::Discret, format!("{etape}…")));
                });
            });
        }
        ui.add_space(espace::L);
    });
}

/// Devant une conversation vide : ce que Claude sait faire, et de quoi
/// commencer. Rend l'exemple cliqué.
fn accueil(ui: &mut egui::Ui) -> Option<&'static str> {
    let mut choisi = None;
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.2).max(30.0));
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(48.0), Sense::hover());
        icons::draw(ui.painter(), rect, Icon::Star, theme::BORDER);
        ui.add_space(espace::L);
        ui.label(Texte::Corps.riche_ton(ui, Ton::Discret, "Pose une question à Claude"));
        ui.add_space(espace::XS);
        ui.label(Texte::Libelle.riche_ton(
            ui,
            Ton::Pale,
            "Il peut lire les salons que tu vois, dire qui est connecté et sortir les stats VALORANT du groupe.",
        ));
        ui.add_space(espace::XL);
        for exemple in EXEMPLES {
            if ui::Bouton::new(exemple).show(ui).clicked() {
                choisi = Some(exemple);
            }
            ui.add_space(espace::XS);
        }
    });
    choisi
}

/// Un coût en millionièmes de dollar, tel qu'on le lit : « 0,42 $ ».
pub(crate) fn dollars(micro: u64) -> String {
    match micro {
        0 => "0 $".into(),
        1..=4_999 => "< 0,01 $".into(),
        _ => format!("{:.2} $", micro as f64 / 1e6).replace('.', ","),
    }
}

/// Un nombre de jetons, arrondi : « 12,3 k », « 1,2 M ».
pub(crate) fn jetons(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_949 => format!("{:.1} k", n as f64 / 1e3).replace('.', ","),
        _ => format!("{:.1} M", n as f64 / 1e6).replace('.', ","),
    }
}

/// Un prix par million de jetons, sans zéros inutiles : « 2 », « 0,5 ».
fn prix(p: f32) -> String {
    format!("{p:.2}").trim_end_matches('0').trim_end_matches('.').replace('.', ",")
}

#[derive(Clone, Copy, PartialEq)]
enum Col {
    Membre,
    Jour,
    Questions,
    Jetons,
    Cout,
}

impl KiApp {
    /// Ouvre le salon de Claude à la place du chat, et demande la
    /// conversation au serveur la première fois.
    pub(crate) fn ouvrir_claude(&mut self) {
        self.retour_au_chat();
        self.claude.ouvert = true;
        self.claude.non_lu = false;
        self.claude.focus = true;
        if !std::mem::replace(&mut self.claude.demandee, true) {
            self.send(ClientMsg::ClaudeHistorique { de: None });
        }
    }

    /// Le salon de Claude : en-tête, conversation, saisie.
    pub(crate) fn claude_panel(&mut self, ui: &mut egui::Ui) {
        let ctx = &ui.ctx().clone();
        self.claude.non_lu = false;
        // Une question que le serveur n'a jamais dit avoir prise — il l'a
        // écartée, ou la ligne a sauté — revient dans la saisie.
        match &self.claude.envoyee {
            Some((_, quand)) if quand.elapsed() > ACCUSE_MAX => {
                self.claude.rendre_la_question();
                self.claude.erreur = Some("le serveur n'a pas pris ta question — réessaie".into());
            }
            Some(_) => ctx.request_repaint_after(Duration::from_secs(1)),
            None => {}
        }
        let moi = self.session.my_id.unwrap_or(0);
        let pseudo = self.username.clone();
        let illimite = self.suis_proprietaire();
        let mut fermer = false;
        let mut question = None;

        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(theme::BG_BASE)).show(ui, |ui| {
            egui::Panel::top("claude_tete")
                .frame(egui::Frame::NONE.fill(theme::BG_BASE).inner_margin(marge::symetrique(espace::XL, espace::L)))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui::glyph(ui, Icon::Star, 17.0, ACCENT);
                        ui.label(Texte::TitreSection.riche(ui, "Claude"));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui::Bouton::icone_seule(Icon::Close).bulle("Revenir au chat (Échap)").show(ui).clicked() {
                                fermer = true;
                            }
                            if !self.claude.messages.is_empty()
                                && !self.claude.occupe()
                                && ui::Bouton::icone_seule(Icon::Trash).bulle("Effacer ma conversation").show(ui).clicked()
                            {
                                self.claude.effacement = true;
                            }
                        });
                    });
                    ui.add_space(espace::XS);
                    ui.label(Texte::Aide.riche(
                        ui,
                        "Ton salon privé : toi et le propriétaire du serveur le voyez, personne d'autre. \
                         Tes questions, et ce que Claude lit pour y répondre, partent chez Anthropic.",
                    ));
                    if let Some(erreur) = self.claude.erreur.clone() {
                        ui.add_space(espace::M);
                        if ui::bandeau_fermable(ui, Ton::Alerte, &erreur) {
                            self.claude.erreur = None;
                        }
                    }
                    // Ce que le serveur refuse hors de Claude (« trop de
                    // requêtes ») se dit ici aussi : le chat est caché.
                    if let Some(erreur) = self.error.clone() {
                        ui.add_space(espace::M);
                        if ui::bandeau_fermable(ui, Ton::Alerte, &erreur) {
                            self.error = None;
                        }
                    }
                });

            egui::Panel::bottom("claude_saisie")
                .frame(egui::Frame::NONE.fill(theme::BG_BASE).inner_margin(egui::Margin {
                    left: 16,
                    right: 16,
                    top: PIED_MARGE as i8,
                    bottom: PIED_MARGE as i8,
                }))
                .show(ui, |ui| question = self.claude.zone_de_saisie(ui, illimite));

            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(theme::BG_BASE)).show(ui, |ui| {
                let occupe = self.claude.occupe();
                if self.claude.messages.is_empty() && !occupe {
                    if self.claude.etat.bloque {
                        return;
                    }
                    if let Some(exemple) = accueil(ui) {
                        self.claude.saisie = exemple.to_string();
                        self.claude.focus = true;
                    }
                    return;
                }
                let membre = Membre { id: moi, pseudo: &pseudo, photo: self.avatars.get(&moi).map(|(_, t)| t) };
                let attente = occupe.then(|| self.claude.etape.as_deref().unwrap_or("réfléchit"));
                conversation(ui, "claude-fil", &self.claude.messages, &membre, attente);
            });
        });

        if self.claude.effacement {
            let (mut oui, mut non) = (false, false);
            let boite = Modale::new("claude-effacer", "Effacer la conversation ?").largeur(340.0).show(ctx, |ui| {
                ui.label(Texte::Libelle.riche(
                    ui,
                    "Claude oubliera ce que vous vous êtes dit, et le propriétaire ne pourra plus le relire. \
                     Tes questions du jour restent comptées.",
                ));
                ui.add_space(espace::L);
                ui.horizontal(|ui| {
                    oui = ui::Bouton::new("Effacer").icone(Icon::Trash).teinte(Ton::Danger).show(ui).clicked();
                    non = ui::Bouton::new("Annuler").icone(Icon::Close).show(ui).clicked();
                });
            });
            if oui {
                self.send(ClientMsg::ClaudeEffacer);
            }
            if oui || non || boite.fermee {
                self.claude.effacement = false;
            }
        }
        if let Some(texte) = question {
            self.send(ClientMsg::ClaudeDemander { texte });
        }
        if fermer {
            self.claude.ouvert = false;
        }
    }

    /// Admin → Claude : le modèle, le plafond, ce que chacun a consommé, et
    /// de quoi relire une conversation ou couper un accès. Au propriétaire
    /// seul — le serveur le vérifie de son côté.
    pub(crate) fn admin_claude_tab(&mut self, ui: &mut egui::Ui, to_send: &mut Vec<ClientMsg>) {
        if !std::mem::replace(&mut self.claude.usages_demandes, true) {
            to_send.push(ClientMsg::ClaudeMembres);
        }
        ui.horizontal(|ui| {
            ui::group_title(ui, Icon::Star, "Claude");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui::Bouton::icone_seule(Icon::Refresh).cote(26.0).bulle("Relire les chiffres").show(ui).clicked() {
                    to_send.push(ClientMsg::ClaudeMembres);
                }
            });
        });
        ui::hint(
            ui,
            "Chaque membre a son salon privé avec Claude, payé par la clé d'API du serveur. \
             Les coûts sont une estimation aux prix publics : la console d'Anthropic fait foi.",
        );
        ui.add_space(espace::L);

        // --- Le total ---
        let cout: u64 = self.claude.usages.iter().map(|u| u.cout_micro).sum();
        let questions: u64 = self.claude.usages.iter().map(|u| u64::from(u.questions)).sum();
        let lus: u64 = self.claude.usages.iter().map(|u| u.jetons_entree).sum();
        let ecrits: u64 = self.claude.usages.iter().map(|u| u.jetons_sortie).sum();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = espace::XXL;
            for (valeur, legende) in [
                (dollars(cout), "dépensé en tout"),
                (questions.to_string(), if questions > 1 { "questions" } else { "question" }),
                (jetons(lus), "jetons lus"),
                (jetons(ecrits), "jetons écrits"),
            ] {
                ui.vertical(|ui| {
                    ui.label(Texte::Kpi.riche(ui, valeur));
                    ui.label(Texte::Aide.riche(ui, legende));
                });
            }
        });
        ui.add_space(espace::L);
        ui::hairline(ui);
        ui.add_space(espace::M);

        // --- La dépense par membre ---
        ui::group_title(ui, Icon::User, "Par membre");
        let tableau = Tableau::new("claude_usages")
            .colonne(Col::Membre, Colonne::new("Membre", 110.0).grandit().triable())
            .colonne(Col::Jour, Colonne::new("Auj.", 36.0).droite().priorite(2))
            .colonne(Col::Questions, Colonne::new("Questions", 62.0).droite().triable())
            .colonne(Col::Jetons, Colonne::new("Jetons", 100.0).droite().priorite(1))
            .colonne(Col::Cout, Colonne::new("Coût", 62.0).droite().triable())
            .tri_par_defaut(Col::Cout, false);
        let mut lignes = self.claude.usages.clone();
        if let Some(tri) = tableau.tri(ui) {
            lignes.sort_by(|a, b| {
                let ordre = match tri.colonne {
                    Col::Membre => a.pseudo.to_lowercase().cmp(&b.pseudo.to_lowercase()),
                    Col::Questions => a.questions.cmp(&b.questions),
                    _ => a.cout_micro.cmp(&b.cout_micro),
                };
                let ordre = if tri.croissant { ordre } else { ordre.reverse() };
                ordre.then_with(|| a.pseudo.to_lowercase().cmp(&b.pseudo.to_lowercase()))
            });
        }
        let surlignee = lignes.iter().position(|u| Some(u.user_id) == self.claude.choisi);
        let cliquee = tableau.surlignee(surlignee).show(ui, lignes.len(), |ui, ligne, colonne| {
            let u = &lignes[ligne];
            let discret = |ui: &egui::Ui, s: String| Texte::Libelle.riche_ton(ui, if u.questions == 0 { Ton::Pale } else { Ton::Neutre }, s);
            match colonne {
                Col::Membre => {
                    if u.bloque {
                        ui::glyph(ui, Icon::Ban, 13.0, theme::DANGER);
                        ui.label(Texte::Corps.riche_ton(ui, Ton::Danger, &u.pseudo).strikethrough());
                    } else {
                        ui.label(Texte::Corps.riche(ui, &u.pseudo).color(theme::color_for(&u.pseudo)));
                    }
                }
                Col::Jour => {
                    ui.label(discret(ui, u.aujourd_hui.to_string()));
                }
                Col::Questions => {
                    ui.label(discret(ui, u.questions.to_string()));
                }
                Col::Jetons => {
                    ui.label(discret(ui, format!("{} / {}", jetons(u.jetons_entree), jetons(u.jetons_sortie))))
                        .on_hover_text("jetons lus par le modèle / écrits par lui");
                }
                Col::Cout => {
                    ui.label(discret(ui, dollars(u.cout_micro)));
                }
            }
        });
        if let Some(i) = cliquee {
            let id = lignes[i].user_id;
            self.claude.choisi = (self.claude.choisi != Some(id)).then_some(id);
        }

        // --- Le membre choisi ---
        ui.add_space(espace::M);
        match self.claude.choisi.and_then(|id| lignes.iter().find(|u| u.user_id == id)) {
            None => ui::hint(ui, "Clique un membre pour relire sa conversation ou lui couper l'accès."),
            Some(u) => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(Texte::Corps.gras(ui, &u.pseudo));
                    if ui::Bouton::new("Relire sa conversation").icone(Icon::Loupe).show(ui).clicked() {
                        self.claude.relue = Some(Relue { de: u.user_id, pseudo: u.pseudo.clone(), messages: None });
                        to_send.push(ClientMsg::ClaudeHistorique { de: Some(u.user_id) });
                    }
                    // Pas sur soi : le propriétaire ne se coupe pas l'accès
                    // (mais se le rend, s'il l'a fait depuis un autre client).
                    let moi = Some(u.user_id) == self.session.my_id;
                    let bascule = if u.bloque {
                        ui::Bouton::new("Rendre l'accès").icone(Icon::Check).show(ui).clicked()
                    } else {
                        !moi && ui::Bouton::new("Couper l'accès").icone(Icon::Ban).teinte(Ton::Danger).show(ui).clicked()
                    };
                    if bascule {
                        to_send.push(ClientMsg::ClaudeAcces { user_id: u.user_id, bloque: !u.bloque });
                    }
                });
                if u.dernier > 0 {
                    ui::hint(ui, format!("dernière question : {}, {}", day_label(u.dernier).to_lowercase(), format_time(u.dernier)));
                }
            }
        }
        ui.add_space(espace::L);
        ui::hairline(ui);
        ui.add_space(espace::M);

        // --- Les réglages ---
        ui::field_label(ui, "Modèle qui répond");
        let courant = self.claude.nom_du_modele().unwrap_or(&self.claude.modele).to_string();
        let mut nouveau_modele = None;
        egui::ComboBox::from_id_salt("claude-modele").selected_text(courant).width(300.0).show_ui(ui, |ui| {
            for m in &self.claude.modeles {
                let libelle =
                    format!("{} — {} $ lus, {} $ écrits par million de jetons", m.nom, prix(m.prix_entree), prix(m.prix_sortie));
                if ui.selectable_label(m.id == self.claude.modele, libelle).clicked() && m.id != self.claude.modele {
                    nouveau_modele = Some(m.id.clone());
                }
            }
        });
        ui.add_space(espace::M);
        ui::field_label(ui, "Questions par membre et par jour");
        let mut appliquer = false;
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut self.claude.plafond_saisi).range(1..=1000).speed(0.2));
            if self.claude.plafond_saisi != self.claude.par_jour {
                appliquer = ui::Bouton::new("Appliquer").icone(Icon::Check).principal().show(ui).clicked();
            }
        });
        ui::hint(ui, "Toi, tu n'as pas de plafond. Le compte repart chaque jour.");
        if nouveau_modele.is_some() || appliquer {
            to_send.push(ClientMsg::ClaudeRegler {
                modele: nouveau_modele.unwrap_or_else(|| self.claude.modele.clone()),
                par_jour: self.claude.plafond_saisi.max(1),
            });
        }
    }

    /// La conversation d'un membre, relue par le propriétaire.
    pub(crate) fn claude_relue_window(&mut self, ctx: &egui::Context) {
        let Some(relue) = &self.claude.relue else { return };
        let mut ouverte = true;
        Fenetre::new("claude-relue", "Conversation avec Claude").ouverte(&mut ouverte).largeur(520.0).haute().taille_min(360.0, 260.0).show(
            ctx,
            |ui| {
                ui.label(Texte::Libelle.riche(ui, format!("{} — lui et toi seuls pouvez la lire", relue.pseudo)));
                ui.add_space(espace::M);
                match &relue.messages {
                    None => {
                        ui.spinner();
                    }
                    Some(messages) if messages.is_empty() => ui::hint(ui, "Rien pour l'instant : aucune question, ou une conversation effacée."),
                    Some(messages) => {
                        let membre =
                            Membre { id: relue.de, pseudo: &relue.pseudo, photo: self.avatars.get(&relue.de).map(|(_, t)| t) };
                        conversation(ui, "claude-relue-fil", messages, &membre, None);
                    }
                }
            },
        );
        if !ouverte {
            self.claude.relue = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(de_claude: bool, texte: &str) -> MessageClaude {
        MessageClaude { de_claude, texte: texte.into(), ts: 1 }
    }

    #[test]
    fn les_tirets_deviennent_des_puces_hors_du_code() {
        let texte = "Voilà :\n- un\n  * deux\n```\n- pas touché\n```\n- trois\ntiret - au milieu";
        assert_eq!(puces(texte), "Voilà :\n•  un\n  •  deux\n```\n- pas touché\n```\n•  trois\ntiret - au milieu");
        assert!(propre(message(false, "- ma question")).texte.starts_with("- "), "la question du membre reste telle quelle");
    }

    #[test]
    fn les_couts_et_les_jetons_se_lisent() {
        assert_eq!(dollars(0), "0 $");
        assert_eq!(dollars(300), "< 0,01 $");
        assert_eq!(dollars(60_000), "0,06 $");
        assert_eq!(dollars(12_345_678), "12,35 $");
        assert_eq!(jetons(950), "950");
        assert_eq!(jetons(12_340), "12,3 k");
        assert_eq!(jetons(1_250_000), "1,2 M");
        assert_eq!((prix(2.0), prix(0.5), prix(0.1)), ("2".into(), "0,5".into(), "0,1".into()));
    }

    /// Une question refusée d'entrée revient dans la saisie ; une question
    /// que le serveur a retenue n'y revient pas, même si Claude échoue.
    #[test]
    fn une_question_refusee_revient_dans_la_saisie() {
        let mut c = Claude { envoyee: Some(("ma question".into(), Instant::now())), ..Claude::default() };
        assert!(c.occupe());
        c.sur_erreur("tu as posé tes 40 questions".into(), EtatClaude::default());
        assert_eq!(c.saisie, "ma question");
        assert!(!c.occupe());

        let mut c = Claude { envoyee: Some(("ma question".into(), Instant::now())), ..Claude::default() };
        c.sur_message(message(false, "ma question"), EtatClaude { en_cours: true, ..EtatClaude::default() });
        assert!(c.envoyee.is_none() && c.occupe());
        c.sur_etape("lit #général".into());
        c.sur_erreur("Claude est injoignable".into(), EtatClaude::default());
        assert!(c.saisie.is_empty());
        assert_eq!(c.erreur.as_deref(), Some("Claude est injoignable"));
        assert!(c.etape.is_none() && !c.occupe());
    }

    /// Une réponse arrivée salon fermé pose la pastille ; la conversation
    /// d'un autre ne remplace pas la mienne.
    #[test]
    fn la_reponse_et_les_conversations_vont_au_bon_endroit() {
        let mut c = Claude::default();
        c.sur_message(message(true, "voilà"), EtatClaude::default());
        assert!(c.non_lu);
        c.ouvert = true;
        c.sur_message(message(true, "encore"), EtatClaude::default());
        assert!(!c.non_lu);

        c.relue = Some(Relue { de: 9, pseudo: "bob".into(), messages: None });
        c.sur_conversation(Some(1), 9, vec![message(false, "à lui")], EtatClaude { bloque: true, ..EtatClaude::default() });
        assert_eq!(c.messages.len(), 2, "la mienne n'a pas bougé");
        assert!(!c.etat.bloque, "son état n'est pas le mien");
        assert_eq!(c.relue.as_ref().unwrap().messages.as_ref().unwrap().len(), 1);
        c.sur_conversation(Some(1), 1, Vec::new(), EtatClaude::default());
        assert!(c.messages.is_empty());

        // La session se ferme : le brouillon et le salon ouvert restent.
        c.saisie = "brouillon".into();
        c.oublier();
        assert!(c.ouvert && c.saisie == "brouillon" && !c.servi && c.relue.is_none());
        assert!(c.sur_bienvenue(true), "salon resté ouvert : la conversation se redemande");
        assert!(!Claude::default().sur_bienvenue(true));
    }
}
