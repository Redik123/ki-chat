//! La zone de saisie : le champ, la réponse et la modification en cours, les
//! commandes, ce qu'on y insère, et le sort d'un message envoyé.

use ki_ui::typo::Texte;
use eframe::egui::{self, Vec2};
use ki_protocol::{ChatRecord, ClientMsg, MsgRef};
use ki_ui::flex::Flex;
use ki_ui::jetons::{espace, marge, rayon};

use crate::{brouillons, prendre_echap, suggestions, theme, ui, KiApp};
use crate::icons::Icon;
use crate::membres::{couleur_de_membre, est_bot};
use crate::registre::{Action, Registre};
use crate::theme::{ACCENT, WARN};

/// Ligne de membre : avatar, pseudo, badges, vumètre pendant qu'il parle.
/// Rend la réponse de la ligne, et `true` si l'on a cliqué l'icône
/// « diffuse » à côté du pseudo.
/// Deux messages ne partent pas à moins de 1,5 s d'écart — la même cadence
/// que le serveur exige.
pub(crate) const CADENCE_CHAT: std::time::Duration = std::time::Duration::from_millis(1500);

/// Ce qui vient du serveur lui-même : le fil de jeu (identifiant 0) et le
/// bot musique. Jamais un compte, jamais un membre.
/// La zone de saisie du salon : son curseur et son focus se manipulent par cet id.
/// Ce que dit le bouton d'envoi au survol : comment envoyer, et les
/// raccourcis de mise en forme, qu'on ne devine pas — ceux du moment, s'ils
/// ont été changés.
fn aide_envoi(registre: &Registre) -> String {
    let mise_en_forme: Vec<String> = [(Action::Gras, "gras"), (Action::Italique, "italique"), (Action::Code, "code")]
        .into_iter()
        .filter_map(|(action, nom)| Some(format!("{} {nom}", registre.libelle(action)?)))
        .collect();
    let mut aide = "Envoyer (Entrée) — Maj+Entrée pour aller à la ligne".to_string();
    if !mise_en_forme.is_empty() {
        aide = format!("{aide}\n{}", mise_en_forme.join(" · "));
    }
    aide
}

pub(crate) fn id_saisie() -> egui::Id {
    egui::Id::unique("saisie_du_chat")
}

/// Ce que le presse-papiers contient, s'il contient du texte.
pub(crate) fn presse_papiers() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok().filter(|t| !t.is_empty())
}

/// Le menu d'édition d'un champ de texte, au clic droit : couper, copier,
/// coller — ce que Ctrl+X/C/V font déjà, pour qui ne les connaît pas ou
/// travaille à la souris. `secret` : un mot de passe, qu'on colle mais
/// qu'on ne recopie pas. Rend `true` si le texte a changé.
pub(crate) fn menu_edition(reponse: &egui::Response, texte: &mut String, secret: bool) -> bool {
    let id = reponse.id;
    let mut modifie = false;
    reponse.context_menu(|ui| {
        ui.set_width(150.0);
        let mut etat = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
        // La sélection courante, en indices de caractères, ordonnée.
        let selection = etat.cursor.char_range().map(|r| {
            let (a, b) = (r.primary.index.0, r.secondary.index.0);
            (a.min(b), a.max(b))
        });
        let curseur = selection.map(|(a, _)| a).unwrap_or_else(|| texte.chars().count());
        let selection = selection.filter(|(a, b)| a < b);
        let extrait = |texte: &String, (a, b): (usize, usize)| -> String {
            texte.chars().skip(a).take(b - a).collect()
        };
        let retirer = |texte: &mut String, (a, b): (usize, usize)| {
            let debut = texte.char_indices().nth(a).map(|(i, _)| i).unwrap_or(texte.len());
            let fin = texte.char_indices().nth(b).map(|(i, _)| i).unwrap_or(texte.len());
            texte.replace_range(debut..fin, "");
        };
        // Où poser le curseur après l'action, s'il y en a une.
        let mut nouveau_curseur: Option<usize> = None;
        if !secret {
            if ui.add_enabled(selection.is_some(), egui::Button::new("Couper")).clicked() {
                if let Some(sel) = selection {
                    ui.ctx().copy_text(extrait(texte, sel));
                    retirer(texte, sel);
                    nouveau_curseur = Some(sel.0);
                    modifie = true;
                }
            }
            if ui.add_enabled(selection.is_some(), egui::Button::new("Copier")).clicked() {
                if let Some(sel) = selection {
                    ui.ctx().copy_text(extrait(texte, sel));
                }
            }
        }
        if ui.button("Coller").clicked() {
            if let Some(colle) = presse_papiers() {
                let mut ou = curseur;
                if let Some(sel) = selection {
                    retirer(texte, sel);
                    ou = sel.0;
                }
                let debut = texte.char_indices().nth(ou).map(|(i, _)| i).unwrap_or(texte.len());
                texte.insert_str(debut, &colle);
                nouveau_curseur = Some(ou + colle.chars().count());
                modifie = true;
            }
        }
        if let Some(ou) = nouveau_curseur {
            etat.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(ou))));
            etat.store(ui.ctx(), id);
        }
    });
    modifie
}

impl KiApp {
    pub(crate) fn chat_input(&mut self, ui: &mut egui::Ui, channel_name: &str) {
        let mut submit = false;
        // Sans le droit d'écrire, la zone de saisie n'est pas grisée : elle
        // disparaît, remplacée par la raison. Un champ où l'on peut taper mais
        // dont rien ne part est plus déroutant qu'une absence de champ.
        if !self.can(ki_protocol::perm::SEND_MESSAGE) {
            egui::Frame::NONE
                .fill(theme::BG_RAISED)
                .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
                .corner_radius(egui::CornerRadius::same(rayon::XL))
                .inner_margin(marge::symetrique(espace::L, espace::L))
                .show(ui, |ui| {
                    ui.label(
                        Texte::Libelle.riche(ui, "Tu n'as pas le droit d'écrire dans ce serveur."),
                    );
                });
            return;
        }
        let can_upload = self.can(ki_protocol::perm::UPLOAD_FILE);
        // Ctrl+B, Ctrl+I, Ctrl+E : avant la zone de saisie, comme la suite.
        self.mise_en_forme_au_clavier(ui.ctx());
        // Avant la zone de saisie : elle prendrait ces touches pour elle.
        if let Some(choix) = self.suggestions.clavier(ui.ctx()) {
            self.appliquer_suggestion(ui.ctx(), &choix);
        }
        let mut curseur_saisie = None;
        let mut focus_saisie = false;
        self.edition_bar(ui);
        self.reply_bar(ui);
        let mut emoji_choisi: Option<String> = None;
        let cadre = egui::Frame::NONE
            .fill(theme::BG_RAISED)
            .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
            .corner_radius(egui::CornerRadius::same(rayon::XL))
            .inner_margin(marge::symetrique(espace::S, espace::S))
            .show(ui, |ui| {
                // Le trombone et l'envoi à leur taille, le champ prend tout ce qui
                // reste — le flex fait le calcul qu'on faisait à la main.
                let ecart = ui.spacing().item_spacing.x;
                Flex::ligne().ecart(ecart).show(ui, "saisie", |f| {
                    if can_upload {
                        f.ui(|ui| {
                            if ui::Bouton::icone_seule(Icon::Paperclip).bulle("Envoyer un fichier (25 Mo max)").show(ui).clicked() {
                                self.start_upload();
                            }
                        });
                    }

                    let filled = !self.input.trim().is_empty();
                    // Zone **multiligne**. Le protocole a toujours accepté les
                    // sauts de ligne ; c'était l'interface qui les interdisait,
                    // si bien que Maj+Entrée envoyait le message au lieu d'aller
                    // à la ligne — et qu'on ne pouvait pas coller un extrait de
                    // code sans qu'il parte en dix messages.
                    //
                    // La hauteur suit le contenu, dans des bornes : une ligne au
                    // repos, jusqu'à six ensuite. Sans plafond, coller cent
                    // lignes mangerait la conversation.
                    let lignes = self.input.lines().count().clamp(1, 6);
                    let indice = if self.edition.is_some() {
                        "Modifie ton message — Entrée pour enregistrer, Échap pour annuler".to_string()
                    } else {
                        format!("Message dans #{channel_name}")
                    };
                    f.grandit(|ui| {
                        // Les emoji en couleur : la mise en page de ki-ui, puis
                        // leurs images posées sur le texte.
                        let mut mise_en_page = |ui: &egui::Ui, tampon: &dyn egui::TextBuffer, largeur: f32| {
                            let police = egui::FontSelection::Default.resolve(ui.style());
                            ki_ui::emoji::mise_en_page(ui, tampon.as_str(), police, largeur)
                        };
                        let taille = Vec2::new(ui.available_width(), 18.0 * lignes as f32 + 8.0);
                        let disposition = egui::Layout::centered_and_justified(ui.layout().main_dir());
                        let sortie = ui
                            .allocate_ui_with_layout(taille, disposition, |ui| {
                                egui::TextEdit::multiline(&mut self.input)
                                    .id(id_saisie())
                                    .char_limit(ki_protocol::MAX_CHAT_TEXT)
                                    .desired_rows(lignes)
                                    .frame(egui::Frame::new().inner_margin(marge::symetrique(espace::XS, espace::XS)))
                                    .hint_text(indice)
                                    .layouter(&mut mise_en_page)
                                    .show(ui)
                            })
                            .inner;
                        ki_ui::emoji::peindre(
                            &ui.painter().with_clip_rect(sortie.text_clip_rect),
                            sortie.galley_pos,
                            &sortie.galley,
                        );
                        curseur_saisie =
                            sortie.cursor_range.filter(|r| r.primary == r.secondary).map(|r| r.primary.index.0);
                        let response = sortie.response.response;
                        if menu_edition(&response, &mut self.input, false) {
                            self.focus_input = true;
                        }
                        // Entrée envoie, Maj+Entrée va à la ligne. La zone
                        // multiligne consomme Entrée pour son propre compte : on
                        // intercepte donc AVANT elle, et l'on retire le saut de
                        // ligne qu'elle vient d'insérer.
                        if response.has_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift)
                        {
                            // egui a déjà écrit le saut de ligne dans le tampon au
                            // moment où l'on regarde : on l'enlève, sinon chaque
                            // message partirait avec une ligne vide en trop.
                            if self.input.ends_with('\n') {
                                self.input.pop();
                            }
                            submit = true;
                            self.focus_input = true;
                        }
                        // Flèche haut dans un champ vide : reprendre son dernier
                        // message pour le corriger — le geste de Discord.
                        if response.has_focus()
                            && self.input.is_empty()
                            && self.edition.is_none()
                            && ui.input(|i| i.key_pressed(egui::Key::ArrowUp))
                        {
                            let dernier = self.session
                                .my_id
                                .and_then(|me| self.session.messages.iter().rev().find(|m| m.user_id == me))
                                .cloned();
                            if let Some(m) = dernier {
                                self.commencer_edition(&m);
                            }
                        }
                        if std::mem::take(&mut self.focus_input) {
                            response.request_focus();
                        }
                        // Après le champ : c'est l'état qu'il relira à
                        // l'image suivante.
                        if std::mem::take(&mut self.curseur_a_la_fin) {
                            let mut etat = egui::TextEdit::load_state(ui.ctx(), id_saisie()).unwrap_or_default();
                            let fin = egui::text::CCursor::new(self.input.chars().count());
                            etat.cursor.set_char_range(Some(egui::text::CCursorRange::one(fin)));
                            etat.store(ui.ctx(), id_saisie());
                        }
                        focus_saisie = response.has_focus();
                    });

                    // Les emoji, sans passer par le sélecteur de Windows.
                    f.ui(|ui| {
                        emoji_choisi = ki_ui::selecteur_emoji::bouton(ui, "Emoji");
                    });

                    f.ui(|ui| {
                        // En attente de cadence, le bouton passe à l'ambre et le
                        // dit : le message reste écrit, il partira dans un instant.
                        let reste = self
                            .dernier_envoi
                            .map(|t| CADENCE_CHAT.saturating_sub(t.elapsed()))
                            .filter(|r| !r.is_zero());
                        let aide_envoi = aide_envoi(&self.registre);
                        let (tint, aide) = match reste {
                            Some(r) => {
                                ui.ctx().request_repaint_after(r);
                                (Some(WARN), "un message toutes les 1,5 s — encore un instant")
                            }
                            None if filled => (Some(ACCENT), aide_envoi.as_str()),
                            None => (None, aide_envoi.as_str()),
                        };
                        if ui::Bouton::icone_seule(Icon::Send).couleur_si(tint).bulle(aide).show(ui).clicked() {
                            submit = true;
                            self.focus_input = true;
                        }
                    });
                });
            });
        if let Some(emoji) = emoji_choisi {
            self.inserer_dans_la_saisie(ui.ctx(), &emoji);
        }
        // Sur une ligne, la barre donne sa hauteur au pied de la colonne des
        // salons (voir `PIED_MARGE`) ; sur plusieurs, elle grandit vers le
        // haut et la garde.
        if self.input.lines().count() <= 1 {
            self.hauteur_saisie = cadre.response.rect.height();
        }

        // Une commande ne part pas dans le salon. Refusée, elle reste dans la
        // saisie, à corriger, et le bandeau d'erreur dit pourquoi. En édition, on
        // réécrit un message : ce qui commence par `/` en reste un.
        if submit && self.edition.is_none() {
            let text = self.input.trim().to_string();
            if let Some(ordre) = suggestions::lire_commande(&text) {
                match self.executer_commande(ordre) {
                    // Faite : rien ne part dans le salon.
                    Ok(None) => {
                        submit = false;
                        self.input.clear();
                    }
                    // Elle a réécrit le message : c'est lui qui part, par le
                    // chemin de tous les messages.
                    Ok(Some(message)) => self.input = message,
                    Err(message) => {
                        submit = false;
                        self.error = Some(message);
                    }
                }
            }
        }

        if submit {
            let text = self.input.trim().to_string();
            // Un message toutes les 1,5 s : le serveur tient la même
            // cadence, mais c'est ici qu'on l'apprend — le texte reste dans
            // le champ, Entrée à nouveau l'enverra.
            let trop_tot = self.dernier_envoi.is_some_and(|t| t.elapsed() < CADENCE_CHAT);
            if !text.is_empty() && !trop_tot {
                match self.edition.take() {
                    // Modifier : un texte inchangé ne repart pas, mais
                    // l'édition se ferme quand même.
                    Some(message) => {
                        let inchange = self.session.messages.iter().any(|m| {
                            m.user_id == message.user_id && m.ts == message.ts && m.text == text
                        });
                        if !inchange {
                            // Gardée jusqu'à son retour, comme un message :
                            // refusée, elle revient dans le champ avec son
                            // message au lieu de disparaître.
                            self.envoi_en_attente = Some(brouillons::Envoi {
                                brut: text.clone(),
                                propre: ki_protocol::clean_chat(&text).unwrap_or_else(|_| text.clone()),
                                quand: std::time::Instant::now(),
                                salon: self.session.current,
                                reponse_a: None,
                                edition: Some(message),
                                rendu: false,
                            });
                            self.send(ClientMsg::EditMessage { message, text });
                            self.dernier_envoi = Some(std::time::Instant::now());
                        }
                    }
                    None => {
                        let reponse_a = self.reponse_a.take();
                        let reply_to = reponse_a.as_ref().map(|r| MsgRef { user_id: r.user_id, ts: r.ts });
                        // Gardé jusqu'à ce que le serveur le renvoie : voir
                        // `envoi_en_attente`. Tel que le serveur le nettoiera,
                        // pour le reconnaître à son retour.
                        self.envoi_en_attente = ki_protocol::clean_chat(&text).ok().map(|propre| brouillons::Envoi {
                            brut: text.clone(),
                            propre,
                            quand: std::time::Instant::now(),
                            salon: self.session.current,
                            reponse_a,
                            edition: None,
                            rendu: false,
                        });
                        self.send(ClientMsg::Chat { text, reply_to, salon: None });
                        self.dernier_envoi = Some(std::time::Instant::now());
                    }
                }
                self.input.clear();
            }
        }

        // Le panneau, une fois le texte et le curseur de cette image connus.
        let mut etat = std::mem::take(&mut self.suggestions);
        etat.actualiser(ui.ctx(), &self.input, curseur_saisie, focus_saisie, || self.sources_suggestions());
        etat.retenir_le_focus(ui.ctx(), id_saisie());
        let clique = etat.panneau(ui.ctx(), cadre.response.rect);
        self.suggestions = etat;
        if let Some(choix) = clique {
            self.appliquer_suggestion(ui.ctx(), &choix);
        }
    }

    /// Ce que le panneau des suggestions peut proposer : tous les membres sauf
    /// soi et les bots, et les commandes qu'on a le droit de lancer.
    pub(crate) fn sources_suggestions(&self) -> suggestions::Sources {
        let membres = self.session
            .members
            .iter()
            .filter(|m| Some(m.user_id) != self.session.my_id && !est_bot(m.user_id))
            .map(|m| suggestions::Membre {
                pseudo: m.username.clone(),
                couleur: couleur_de_membre(m),
                en_ligne: m.online,
                detail: if !m.online {
                    "hors ligne"
                } else if m.voice.is_some() {
                    "en vocal"
                } else {
                    "en ligne"
                },
            })
            .collect();
        let commandes = if self.edition.is_some() { Vec::new() } else { self.commandes_disponibles() };
        suggestions::Sources { membres, commandes }
    }

    /// Les commandes qu'on peut lancer tout de suite. Le serveur revérifie les
    /// droits : ceci sert à ne pas proposer ce qu'il refuserait.
    pub(crate) fn commandes_disponibles(&self) -> Vec<suggestions::Commande> {
        let mut commandes = Vec::new();
        // Même condition que le bouton Poke du profil.
        if self.session.serveur_gere_lus {
            commandes.push(suggestions::POKE);
        }
        let musique = &self.musique.etat;
        let bot_la = musique.salon.is_some();
        let pilote = bot_la && self.can(ki_protocol::perm::CONTROL_MUSIC);
        // Les mêmes droits que la recherche du bandeau de musique.
        if bot_la && (pilote || self.server_info.musique_membres_ajoutent) {
            commandes.push(suggestions::MUSIQUE);
        }
        if pilote && musique.lecture {
            commandes.push(suggestions::PAUSE);
        }
        if pilote && !musique.lecture && musique.en_cours.is_some() {
            commandes.push(suggestions::REPRENDRE);
        }
        if pilote && musique.en_cours.is_some() {
            commandes.push(suggestions::SUIVANT);
        }
        if self.serveur_cartes {
            commandes.push(suggestions::CARTES);
        }
        // Celles-ci ne demandent rien au serveur qu'il ne fasse déjà.
        commandes.extend([
            suggestions::CHERCHER,
            suggestions::MOI,
            suggestions::MUET,
            suggestions::SOURD,
            suggestions::TABLEAU,
            suggestions::RACCOURCIS,
        ]);
        commandes
    }

    /// Lance une commande de la saisie. `Err` : pourquoi elle ne part pas.
    /// `Ok(Some(message))` : elle se résume à un message, à envoyer à sa
    /// place.
    pub(crate) fn executer_commande(&mut self, ordre: suggestions::Ordre<'_>) -> Result<Option<String>, String> {
        use ki_protocol::CommandeMusique as C;
        use suggestions::Ordre;
        let (commande, refus) = match ordre {
            Ordre::Poke(_) => (suggestions::POKE, "ce serveur ne gère pas les pokes"),
            Ordre::Musique(_) => (suggestions::MUSIQUE, "le bot de musique n'est pas là, ou tu n'as pas le droit d'ajouter"),
            Ordre::Pause => (suggestions::PAUSE, "rien à mettre en pause, ou tu n'as pas le droit"),
            Ordre::Reprendre => (suggestions::REPRENDRE, "rien à reprendre, ou tu n'as pas le droit"),
            Ordre::Suivant => (suggestions::SUIVANT, "aucun morceau en cours, ou tu n'as pas le droit"),
            Ordre::Cartes => (suggestions::CARTES, "ce serveur ne gère pas les cartes"),
            Ordre::Chercher(_) => (suggestions::CHERCHER, "pas ici"),
            Ordre::Moi(_) => (suggestions::MOI, "pas ici"),
            Ordre::Muet => (suggestions::MUET, "pas ici"),
            Ordre::Sourd => (suggestions::SOURD, "pas ici"),
            Ordre::Tableau => (suggestions::TABLEAU, "pas ici"),
            Ordre::Raccourcis => (suggestions::RACCOURCIS, "pas ici"),
        };
        if !self.commandes_disponibles().contains(&commande) {
            return Err(format!("/{} : {refus}", commande.nom));
        }
        match ordre {
            Ordre::Poke(pseudo) => {
                if pseudo.is_empty() {
                    return Err("/poke : qui ? Écris /poke @pseudo".into());
                }
                let nom = ki_protocol::safe_display(pseudo, 32);
                let membre = self.session
                    .members
                    .iter()
                    .find(|m| !est_bot(m.user_id) && m.username.eq_ignore_ascii_case(pseudo))
                    .ok_or_else(|| format!("/poke : personne ne s'appelle « {nom} »"))?;
                if Some(membre.user_id) == self.session.my_id {
                    return Err("/poke : tu ne peux pas te poke toi-même".into());
                }
                if let Some(motif) = self.poke_impossible(membre) {
                    return Err(format!("/poke : {nom}, {motif}"));
                }
                self.send(ClientMsg::Poke { user_id: membre.user_id });
            }
            Ordre::Musique(texte) => {
                if texte.is_empty() {
                    return Err("/musique : quoi ? Écris un lien ou une recherche".into());
                }
                if ki_protocol::url_musique_valide(texte) {
                    self.commander_musique(C::Ajouter { url: texte.to_string(), maintenant: false });
                } else if texte.chars().count() > ki_protocol::MAX_RECHERCHE_MUSIQUE {
                    return Err("/musique : recherche trop longue".into());
                } else {
                    // Les résultats arrivent dans le bandeau de musique, comme
                    // pour une recherche faite depuis son champ.
                    self.musique.recherche = texte.to_string();
                    let source = if self.musique.soundcloud { "soundcloud" } else { "youtube" }.to_string();
                    self.commander_musique(C::Chercher { texte: texte.to_string(), source });
                }
            }
            Ordre::Pause => self.commander_musique(C::Pause),
            Ordre::Reprendre => self.commander_musique(C::Lecture),
            Ordre::Suivant => self.commander_musique(C::Suivant),
            Ordre::Cartes => self.cartes.ouvert = true,
            Ordre::Chercher(mots) => {
                // Comme Entrée dans le champ du haut : la fenêtre des
                // résultats s'ouvre, la requête part. Sans mots, elle
                // s'ouvre sur son champ.
                self.recherche.ouverte = true;
                if mots.is_empty() {
                    self.recherche.focus = true;
                } else {
                    self.recherche.requete = mots.to_string();
                    self.lancer_recherche();
                }
            }
            Ordre::Moi(action) => {
                if action.is_empty() {
                    return Err("/moi : quoi ? Écris ce que tu fais".into());
                }
                return Ok(Some(format!("*{action}*")));
            }
            Ordre::Muet => {
                self.basculer_micro();
                self.info = Some(if self.vocal.muted { "micro coupé" } else { "micro rouvert" }.into());
            }
            Ordre::Sourd => {
                self.basculer_sourd();
                self.info = Some(if self.vocal.sourd { "tu n'entends plus rien, micro coupé" } else { "tu entends à nouveau" }.into());
            }
            Ordre::Tableau => {
                self.tactique.ouvert = true;
                self.catalogue.demarrer();
            }
            Ordre::Raccourcis => self.raccourcis.ouverte = true,
        }
        Ok(None)
    }

    /// Le serveur a renvoyé ce qu'on venait d'envoyer : c'est passé. Si une
    /// erreur sans rapport l'avait fait remettre dans le champ entre-temps,
    /// il en ressort — tant qu'on n'y a pas touché.
    pub(crate) fn envoi_confirme(&mut self) {
        let Some(envoi) = self.envoi_en_attente.take() else { return };
        if !envoi.rendu || self.session.current != envoi.salon || self.input != envoi.brut {
            return;
        }
        if self.edition == envoi.edition {
            self.input.clear();
            self.edition = None;
            if envoi.edition.is_none() {
                self.reponse_a = None;
            }
        }
    }

    /// Le serveur vient de dire une erreur : ce qu'on venait d'envoyer, et
    /// qui n'est pas revenu, retourne dans le champ — avec la réponse ou la
    /// modification qu'il portait — plutôt que de se perdre.
    pub(crate) fn rendre_l_envoi_refuse(&mut self) {
        let champ_libre = self.input.trim().is_empty() && self.edition.is_none();
        let maintenant = std::time::Instant::now();
        let salon = self.session.current;
        let Some(envoi) = self
            .envoi_en_attente
            .as_mut()
            .filter(|e| e.a_rendre_apres_un_refus(maintenant, salon, champ_libre))
        else {
            return;
        };
        envoi.rendu = true;
        self.input = envoi.brut.clone();
        self.curseur_a_la_fin = true;
        self.edition = envoi.edition;
        if self.reponse_a.is_none() {
            self.reponse_a = envoi.reponse_a.clone();
        }
        self.focus_input = true;
        ki_voice::journal("envoi refusé par le serveur : rendu au champ de saisie".into());
    }

    /// Reprendre un de ses messages dans la zone de saisie pour le
    /// corriger. Une réponse en cours s'efface : on ne fait pas les deux.
    pub(crate) fn commencer_edition(&mut self, msg: &ChatRecord) {
        self.edition = Some(MsgRef { user_id: msg.user_id, ts: msg.ts });
        self.input = msg.text.clone();
        self.reponse_a = None;
        self.focus_input = true;
        self.curseur_a_la_fin = true;
    }

    /// Renoncer à modifier : le texte repris s'en va avec.
    pub(crate) fn annuler_edition(&mut self) {
        if self.edition.take().is_some() {
            self.input.clear();
            self.focus_input = true;
        }
    }

    /// Le rappel « tu modifies ton message » au-dessus de la zone de
    /// saisie, tant qu'une modification est en cours. Échap ou la croix y
    /// renoncent.
    pub(crate) fn edition_bar(&mut self, ui: &mut egui::Ui) {
        if self.edition.is_none() {
            return;
        }
        if prendre_echap(ui.ctx()) {
            self.annuler_edition();
            return;
        }
        if ui::rappel(ui, "✎ Tu modifies ton message — Entrée pour enregistrer", "Annuler (Échap)") {
            self.annuler_edition();
        }
        ui.add_space(espace::XS);
    }

    /// Insère `texte` dans la saisie, au curseur — à la place de la
    /// sélection s'il y en a une —, et y remet le curseur juste après : on
    /// continue d'écrire comme si on l'avait tapé.
    pub(crate) fn inserer_dans_la_saisie(&mut self, ctx: &egui::Context, texte: &str) {
        let etat = egui::TextEdit::load_state(ctx, id_saisie()).unwrap_or_default();
        let total = self.input.chars().count();
        let (debut, fin) = match etat.cursor.char_range() {
            Some(r) => {
                let (a, b) = (r.primary.index.0.min(total), r.secondary.index.0.min(total));
                (a.min(b), a.max(b))
            }
            None => (total, total),
        };
        self.remplacer_dans_la_saisie(ctx, debut, fin, texte);
    }

    /// Le mot en cours devient ce que le panneau des suggestions a choisi.
    pub(crate) fn appliquer_suggestion(&mut self, ctx: &egui::Context, choix: &suggestions::Choix) {
        self.remplacer_dans_la_saisie(ctx, choix.debut, choix.fin, &choix.insertion);
    }

    /// Remplace les caractères `debut..fin` de la saisie par `texte`, et y remet
    /// le curseur juste après. Rien si le message dépassait alors sa taille maximale.
    pub(crate) fn remplacer_dans_la_saisie(&mut self, ctx: &egui::Context, debut: usize, fin: usize, texte: &str) {
        let id = id_saisie();
        let mut etat = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        let total = self.input.chars().count();
        let (debut, fin) = (debut.min(total), fin.min(total));
        if total - (fin - debut) + texte.chars().count() > ki_protocol::MAX_CHAT_TEXT {
            return;
        }
        let octet = |n: usize| self.input.char_indices().nth(n).map_or(self.input.len(), |(o, _)| o);
        let (a, b) = (octet(debut), octet(fin));
        self.input.replace_range(a..b, texte);
        let apres = egui::text::CCursor::new(debut + texte.chars().count());
        etat.cursor.set_char_range(Some(egui::text::CCursorRange::one(apres)));
        etat.store(ctx, id);
        self.focus_input = true;
    }

    /// Le rappel « en réponse à… » au-dessus de la zone de saisie, tant
    /// qu'une réponse est en cours. Échap ou la croix y renoncent.
    pub(crate) fn reply_bar(&mut self, ui: &mut egui::Ui) {
        let Some(r) = self.reponse_a.clone() else { return };
        if prendre_echap(ui.ctx()) {
            self.reponse_a = None;
            return;
        }
        let texte_rappel = format!("↩ Réponse à {} — {}", r.username, r.excerpt);
        if ui::rappel(ui, &texte_rappel, "Ne plus répondre (Échap)") {
            self.reponse_a = None;
        }
        ui.add_space(espace::XS);
    }
}
