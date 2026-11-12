//! La recherche dans l'historique : le champ en haut du salon, la fenêtre des
//! résultats, et le saut vers un message ancien.

use ki_ui::typo::{Texte, Ton};
use eframe::egui::{self};
use ki_protocol::{ChannelId, ClientMsg};
use ki_ui::jetons::espace;
use ki_ui::liste::Liste;

use crate::{ui, KiApp};
use crate::formats::day_label;
use crate::icons::Icon;
use crate::saisie::menu_edition;

/// La recherche dans l'historique : la requête, ses résultats, le saut dans
/// le passé.
pub(crate) struct Recherche {
    /// La fenêtre des résultats est ouverte.
    pub(crate) ouverte: bool,
    /// Ce qui est tapé dans le champ.
    pub(crate) requete: String,
    /// Chercher dans le salon courant seulement, ou dans tous ceux qu'on peut
    /// lire. Restreint par défaut : on sait presque toujours où l'on a vu
    /// passer la chose, et le serveur relit moins de fichiers.
    pub(crate) ici: bool,
    pub(crate) resultats: Vec<ki_protocol::SearchHit>,
    pub(crate) encore: bool,
    /// La requête effectivement envoyée, en attente de réponse.
    ///
    /// Sert à jeter une réponse périmée : le serveur relit des fichiers, deux
    /// réponses peuvent revenir dans le désordre, et la plus lente écraserait
    /// la bonne.
    pub(crate) envoyee: Option<String>,
    pub(crate) focus: bool,
    /// Salon dans lequel on a sauté au milieu du passé.
    ///
    /// Le fil n'affiche alors pas la fin de la conversation, et rien ne le
    /// dirait sans ce drapeau : on croirait le salon vide depuis.
    pub(crate) retour_present: Option<ChannelId>,
}

impl KiApp {
    /// Ouvre la recherche, curseur dans le champ.
    /// La recherche en haut à droite du salon : on tape, Entrée cherche et
    /// ouvre les résultats ; la loupe ouvre la fenêtre sans chercher.
    pub(crate) fn recherche_en_tete(&mut self, ui: &mut egui::Ui) {
        let champ = ui.add(ui::champ(&mut self.recherche.requete, "chercher…").desired_width(190.0));
        menu_edition(&champ, &mut self.recherche.requete, false);
        if champ.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.recherche.ouverte = true;
            self.lancer_recherche();
        }
        if ui::Bouton::icone_seule(Icon::Loupe).cote(24.0).bulle("chercher dans les messages").show(ui).clicked() {
            self.recherche.ouverte = !self.recherche.ouverte;
            if self.recherche.ouverte {
                self.recherche.focus = true;
            }
        }
    }

    /// Envoie la requête en cours, si elle a de quoi chercher.
    pub(crate) fn lancer_recherche(&mut self) {
        let query = self.recherche.requete.trim().to_string();
        if query.is_empty() {
            self.recherche.resultats.clear();
            self.recherche.envoyee = None;
            return;
        }
        let channel = self.recherche.ici.then_some(self.current).flatten();
        self.recherche.envoyee = Some(query.clone());
        self.send(ClientMsg::Search {
            query,
            channel,
            limit: ki_protocol::MAX_SEARCH_HITS as u32,
        });
    }

    /// Ouvre un salon **au niveau d'un message précis** plutôt qu'à sa fin.
    ///
    /// Ce n'est pas `join` : celui-ci demande les derniers messages, alors
    /// qu'ici on veut la page qui **se termine** par le message trouvé. Le
    /// résultat est donc en bas de l'écran, sans qu'il faille recalculer un
    /// défilement — et sans afficher ce qui a suivi, d'où le retour au
    /// présent proposé en bas du fil.
    pub(crate) fn sauter_a(&mut self, channel: ChannelId, ts: u64) {
        self.current = Some(channel);
        self.messages.clear();
        self.history_more = true;
        self.history_pending = true;
        self.fil_au_bas = true;
        self.recherche.retour_present = Some(channel);
        // C'est bien entrer dans le salon, comme `join` : la pastille tombe
        // (le serveur nous tient pour lecteur dès le `Join`, elle ne
        // bougerait plus), et « nouveaux messages » se pose à **son** repère
        // — pas à celui du salon qu'on quitte, qui se dessinerait ici au
        // mauvais endroit. Il reste posé pour le retour au présent.
        self.separateur_nouveaux = self
            .non_lus
            .remove(&channel)
            .filter(|n| n.nb > 0)
            .map(|n| n.depuis);
        self.programmer_lu(channel);
        self.send(ClientMsg::Join { channel });
        // `before_ts` est exclusif : +1 pour que le message cherché soit dans
        // la page, et en dernier.
        self.send(ClientMsg::HistoryBefore { before_ts: ts + 1, limit: 100, channel });
    }

    /// Fenêtre de recherche : la requête, la portée, et les résultats.
    pub(crate) fn search_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let mut lancer = false;
        let mut saut: Option<(ChannelId, u64)> = None;
        let roomy = (ctx.content_rect().height() - 160.0).clamp(280.0, 720.0);
        egui::Window::new("Rechercher")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(440.0)
            .default_height(roomy)
            .min_width(320.0)
            .show(ctx, |ui| {
                let champ = ui.add(ui::champ(&mut self.recherche.requete, "chercher…"));
                menu_edition(&champ, &mut self.recherche.requete, false);
                if std::mem::take(&mut self.recherche.focus) {
                    champ.request_focus();
                }
                // À la frappe, pas de recherche : chaque requête fait relire
                // des journaux entiers au serveur. On cherche quand on a fini
                // de taper, et on le dit.
                if champ.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    lancer = true;
                }
                ui.horizontal(|ui| {
                    // Changer de portée relance : sinon la liste affichée ne
                    // correspond plus au bouton allumé, et l'on croit que la
                    // recherche n'a rien trouvé ailleurs.
                    if ui::pastilles(ui, &mut self.recherche.ici, &[(true, "Ce salon"), (false, "Partout")]) {
                        lancer = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui::Bouton::new("Chercher").icone(Icon::Loupe).show(ui).clicked() {
                            lancer = true;
                        }
                    });
                });
                ui::hint(ui, "Entrée pour chercher — la casse et les accents majuscules sont ignorés");

                ui.add_space(espace::M);
                ui::hairline(ui);
                ui.add_space(espace::M);

                if self.recherche.envoyee.is_some() && self.recherche.resultats.is_empty() {
                    ui.label(Texte::Libelle.riche_ton(ui, Ton::Pale, "Recherche en cours…"));
                    return;
                }
                if self.recherche.resultats.is_empty() {
                    let quoi = if self.recherche.requete.trim().is_empty() {
                        "Tape ce que tu cherches, puis Entrée."
                    } else {
                        "Aucun message ne contient ça."
                    };
                    ui.label(Texte::Libelle.riche_ton(ui, Ton::Pale, quoi));
                    return;
                }
                if self.recherche.encore {
                    ui::hint(ui, "il y en avait davantage : voici les plus récents");
                }

                // Les plus récents en premier : c'est l'ordre dans lequel on
                // cherche, alors que le serveur rend l'ordre du fil.
                let hits: Vec<ki_protocol::SearchHit> =
                    self.recherche.resultats.iter().rev().cloned().collect();
                // Jusqu'à cent résultats : seuls ceux à l'écran sont
                // construits.
                let channels = &self.channels;
                Liste::new("recherche").ecart(espace::XS).show(
                    ui,
                    hits.len(),
                    // Le rang en plus de salon et horodatage : deux messages
                    // peuvent partager les deux à la milliseconde près, et
                    // deux widgets de même identité, egui n'en peint qu'un.
                    |rang| (rang, hits[rang].channel, hits[rang].record.ts),
                    |ui, rang| {
                        let hit = &hits[rang];
                        let nom = channels
                            .iter()
                            .find(|c| c.id == hit.channel)
                            .map(|c| c.name.clone())
                            .unwrap_or_else(|| format!("salon {}", hit.channel));
                        let bloc = ui.scope(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    Texte::Aide.riche_ton(ui, Ton::Accent, format!("#{nom}")),
                                );
                                ui.label(
                                    Texte::Aide.riche_ton(ui, Ton::Discret, &hit.record.username),
                                );
                                ui.label(
                                    Texte::Aide.riche(ui, day_label(hit.record.ts)),
                                );
                            });
                            // Une seule ligne : un résultat sert à reconnaître
                            // le message, pas à le relire. Le fil s'en charge.
                            let apercu: String = hit
                                .record
                                .text
                                .lines()
                                .next()
                                .unwrap_or_default()
                                .chars()
                                .take(160)
                                .collect();
                            ki_ui::emoji::label(ui, Texte::Corps.riche(ui, apercu));
                        });
                        if bloc
                            .response
                            .interact(egui::Sense::click())
                            .on_hover_text("aller à ce message")
                            .clicked()
                        {
                            saut = Some((hit.channel, hit.record.ts));
                        }
                        ui.add_space(espace::XS);
                        ui::hairline(ui);
                    },
                );
            });
        if lancer {
            self.lancer_recherche();
        }
        if let Some((channel, ts)) = saut {
            self.sauter_a(channel, ts);
        }
        if !open {
            self.recherche.ouverte = false;
        }
    }
}
