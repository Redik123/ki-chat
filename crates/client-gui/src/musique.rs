//! Le bot musique : son bandeau au-dessus du fil, sa file, ses playlists.
//!
//! L'état vit dans [`Musique`] ; le bandeau le dessine et envoie les commandes au
//! serveur, qui pilote le bot.

use ki_ui::typo::Texte;
use eframe::egui::{self, RichText, Sense, Vec2};
use ki_protocol::ClientMsg;
use ki_ui::jetons::{espace, rayon, texte};

use crate::{images, theme, ui, KiApp};
use crate::formats::mmss;
use crate::icons::Icon;
use crate::membres::pastille_bot;
use crate::saisie::menu_edition;
use crate::theme::{ACCENT, TEXT, TEXT_FAINT};
use crate::ui::Ton;

/// Le bot musique : son état poussé par le serveur, et ce qu'on prépare pour lui
/// (la recherche, ses résultats, le nom d'une playlist).
pub(crate) struct Musique {
    /// L'état du bot, tel que le serveur le pousse.
    pub(crate) etat: ki_protocol::EtatMusique,
    /// Quand l'état est arrivé : la barre de progression avance depuis.
    pub(crate) recu: std::time::Instant,
    pub(crate) deroulee: bool,
    pub(crate) recherche: String,
    pub(crate) soundcloud: bool,
    pub(crate) resultats: Vec<ki_protocol::Piste>,
    pub(crate) resultats_pour: String,
    pub(crate) playlist_nom: String,
}

impl KiApp {
    /// La position de lecture, avancée depuis le dernier état reçu.
    pub(crate) fn musique_position_s(&self) -> u64 {
        let base = self.musique.etat.position_ms;
        let avance = if self.musique.etat.lecture { self.musique.recu.elapsed().as_millis() as u64 } else { 0 };
        let duree = self.musique.etat.en_cours.as_ref().map(|p| p.duree_s as u64).unwrap_or(0);
        let pos = (base + avance) / 1000;
        if duree > 0 { pos.min(duree) } else { pos }
    }

    pub(crate) fn commander_musique(&self, commande: ki_protocol::CommandeMusique) {
        self.send(ClientMsg::Musique { commande });
    }

    /// L'étoile : la piste va dans la playlist « Favoris » du groupe.
    pub(crate) fn etoile_musique(&self, ui: &mut egui::Ui, piste: &ki_protocol::Piste, peut: bool) {
        ui.add_enabled_ui(peut, |ui| {
            if ui::Bouton::icone_seule(Icon::Star).cote(18.0).bulle("ajouter aux Favoris").show(ui).clicked() {
                self.commander_musique(ki_protocol::CommandeMusique::PlaylistAjouterPiste {
                    nom: ki_protocol::PLAYLIST_FAVORIS.to_string(),
                    piste: piste.clone(),
                });
            }
        });
    }

    /// La bannière du bot, au-dessus du chat. Repliée : une ligne — lecture,
    /// suivant, la progression, le titre, mon volume, le chevron. Déroulée :
    /// la pochette, la file d'attente, la recherche, le volume global.
    /// Sans la permission, les commandes sont grisées ; le volume perso et
    /// le chevron restent à tous.
    pub(crate) fn bandeau_musique(&mut self, ui: &mut egui::Ui) {
        use ki_protocol::CommandeMusique as C;
        let peut = self.can(ki_protocol::perm::CONTROL_MUSIC);
        // Chercher et ajouter en fin de file : ouvert à tous si l'admin
        // l'a voulu ; piloter reste aux modérateurs.
        let peut_ajouter = peut || self.server_info.musique_membres_ajoutent;
        let etat = self.musique.etat.clone();
        let en_cours = etat.en_cours.clone();
        let position = self.musique_position_s();
        if etat.lecture && en_cours.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
        }
        let salon_nom = etat
            .salon
            .and_then(|id| self.session.channels.iter().find(|c| c.id == id))
            .map(|c| c.name.clone())
            .unwrap_or_default();
        let mon_salon = self.session.my_id.and_then(|me| self.session.members.iter().find(|m| m.user_id == me)).and_then(|m| m.voice);
        let ailleurs = etat.salon.is_some() && mon_salon != etat.salon;

        // --- La ligne ---
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = espace::M;
            ui.add_enabled_ui(peut, |ui| {
                let (glyphe, aide) = if etat.lecture { ("⏸", "pause") } else { ("▶", "lecture") };
                if ui.add(egui::Button::new(RichText::new(glyphe).size(texte::TITRE)).frame(false)).on_hover_text(aide).clicked() {
                    self.commander_musique(if etat.lecture { C::Pause } else { C::Lecture });
                }
                if ui.add(egui::Button::new(RichText::new("⏭").size(texte::TITRE)).frame(false)).on_hover_text("suivante").clicked() {
                    self.commander_musique(C::Suivant);
                }
            });
            match &en_cours {
                // Le bot n'est nulle part : on peut le faire venir.
                None if etat.salon.is_none() => {
                    ui.label(Texte::Corps.gras_ton(ui, Ton::Accent, "Musique"));
                    pastille_bot(ui);
                    ui.label(Texte::Libelle.riche_ton(ui, Ton::Pale, "le bot n'est dans aucun salon"));
                    if peut && ui.button("Venir dans mon salon").clicked() {
                        self.commander_musique(C::Rejoindre);
                    }
                }
                Some(p) => {
                    // La progression, peinte : une barre fine, le temps à côté.
                    let duree = p.duree_s as u64;
                    // La barre se clique (ou se tire) pour avancer dans la
                    // piste — dix minutes de morceau, ça s'écoute par morceaux.
                    let (barre, reponse) = ui.allocate_exact_size(
                        Vec2::new(160.0, 10.0),
                        if peut && duree > 0 { Sense::click_and_drag() } else { Sense::hover() },
                    );
                    let painter = ui.painter();
                    let fine = egui::Rect::from_center_size(barre.center(), Vec2::new(barre.width(), 6.0));
                    painter.rect_filled(fine, egui::CornerRadius::same(rayon::S), theme::BG_DEEP);
                    if duree > 0 {
                        let part = (position as f32 / duree as f32).clamp(0.0, 1.0);
                        let plein = egui::Rect::from_min_size(fine.min, Vec2::new(fine.width() * part, fine.height()));
                        painter.rect_filled(plein, egui::CornerRadius::same(rayon::S), ACCENT);
                        if let Some(pos) = reponse.hover_pos().filter(|_| peut) {
                            let vise = ((pos.x - fine.left()) / fine.width()).clamp(0.0, 1.0);
                            painter.circle_filled(egui::pos2(fine.left() + fine.width() * vise, fine.center().y), 4.0, TEXT);
                            reponse.clone().on_hover_text(format!("aller à {}", mmss((vise * duree as f32) as u64)));
                        }
                        if reponse.clicked() || reponse.drag_stopped() {
                            if let Some(pos) = reponse.interact_pointer_pos() {
                                let vise = ((pos.x - fine.left()) / fine.width()).clamp(0.0, 1.0);
                                self.commander_musique(C::Position { secondes: (vise * duree as f32) as u32 });
                            }
                        }
                    }
                    let temps = if duree > 0 { format!("{} / {}", mmss(position), mmss(duree)) } else { mmss(position) };
                    ui::hint(ui, temps);
                    let titre = if p.artiste.is_empty() { p.titre.clone() } else { format!("{} — {}", p.artiste, p.titre) };
                    ui.add(egui::Label::new(Texte::Corps.riche(ui, titre)).truncate());
                    self.etoile_musique(ui, p, peut);
                }
                None => {
                    ui.label(Texte::Libelle.riche_ton(ui, Ton::Pale, "rien en cours"));
                    if !etat.file.is_empty() {
                        ui.label(Texte::Aide.riche(ui, format!("{} en file", etat.longueur_file())));
                    }
                }
            }
            if let Some(e) = &etat.erreur {
                ui.label(Texte::Aide.riche_ton(ui, Ton::Danger, e));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let chevron = if self.musique.deroulee { Icon::ArrowUp } else { Icon::ArrowDown };
                if ui::Bouton::icone_seule(chevron).cote(24.0).bulle("détails").show(ui).clicked() {
                    self.musique.deroulee = !self.musique.deroulee;
                }
                // Mon volume : le mien, pas celui du bot.
                let mut pct = self.volume_of(ki_protocol::MUSIQUE_ID) * 100.0;
                if ui.add(egui::Slider::new(&mut pct, 0.0..=200.0).show_value(false)).on_hover_text(format!("mon volume : {pct:.0} %")).changed() {
                    self.set_volume(ki_protocol::MUSIQUE_ID, pct / 100.0);
                }
                ui::glyph(ui, Icon::Volume, 14.0, TEXT_FAINT);
                if ailleurs {
                    ui::hint(ui, format!("dans #{salon_nom}"));
                }
            });
        });

        if !self.musique.deroulee {
            return;
        }
        ui.add_space(espace::M);
        ui::hairline(ui);
        ui.add_space(espace::M);

        // --- Le détail, sur trois colonnes ---
        ui.columns(3, |cols| {
            // La pochette et ce qui joue.
            {
                let ui = &mut cols[0];
                match &en_cours {
                    Some(p) => {
                        if let Some(chemin) = &p.vignette {
                            if let Some(images::Preview::Ready(tex)) = self.previews.chez_nous(ui.ctx(), chemin) {
                                ui.add(egui::Image::new(&tex).fit_to_exact_size(Vec2::new(200.0, 112.0)).corner_radius(6.0));
                            }
                        }
                        ui.label(Texte::Corps.gras(ui, &p.titre));
                        if !p.artiste.is_empty() {
                            ui.label(Texte::Libelle.riche(ui, &p.artiste));
                        }
                        let mut infos = p.source.clone();
                        if let Some(qui) = &p.ajoute_par {
                            infos.push_str(&format!(" · ajoutée par {qui}"));
                        }
                        ui::hint(ui, infos);
                    }
                    None => {
                        ui.label(RichText::new("rien en cours").color(TEXT_FAINT));
                    }
                }
                ui.add_space(espace::M);
                ui.add_enabled_ui(peut, |ui| {
                    ui::hint(ui, "volume du bot");
                    let mut v = etat.volume as f32;
                    if ui.add(egui::Slider::new(&mut v, 0.0..=100.0).suffix(" %").integer()).drag_stopped() {
                        self.commander_musique(C::Volume { pour_cent: v as u8 });
                    }
                    ui.horizontal(|ui| {
                        if ailleurs && ui.button("Venir dans mon salon").clicked() {
                            self.commander_musique(C::Rejoindre);
                        }
                        if ui::Bouton::new("Arrêter").icone(Icon::Close).teinte(Ton::Danger).show(ui).clicked() {
                            self.commander_musique(C::Arreter);
                        }
                    });
                });
            }
            // La file d'attente.
            {
                let ui = &mut cols[1];
                ui.horizontal(|ui| {
                    ui.label(Texte::Corps.gras(ui, format!("File d'attente · {}", etat.longueur_file())));
                    if peut && !etat.file.is_empty() && ui.small_button("vider").clicked() {
                        self.commander_musique(C::Vider);
                    }
                });
                egui::ScrollArea::vertical().id_salt("musique_file").max_height(170.0).auto_shrink([false, true]).show(ui, |ui| {
                    if etat.file.is_empty() {
                        ui::hint(ui, "vide — cherche un morceau à droite");
                    }
                    let n = etat.longueur_file();
                    for (i, p) in etat.file.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui::hint(ui, format!("{}", i + 1));
                            let nom = if p.artiste.is_empty() { p.titre.clone() } else { format!("{} — {}", p.artiste, p.titre) };
                            ui.add(egui::Label::new(RichText::new(nom).size(texte::COURANT)).truncate())
                                .on_hover_text(format!("{} · {}", mmss(p.duree_s as u64), p.ajoute_par.as_deref().unwrap_or("?")));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                self.etoile_musique(ui, p, peut);
                                ui.add_enabled_ui(peut, |ui| {
                                    if ui::Bouton::icone_seule(Icon::Close).cote(18.0).bulle("retirer").show(ui).clicked() {
                                        self.commander_musique(C::Retirer { index: i });
                                    }
                                    if i + 1 < n && ui::Bouton::icone_seule(Icon::ArrowDown).cote(18.0).bulle("descendre").show(ui).clicked() {
                                        self.commander_musique(C::Deplacer { de: i, vers: i + 1 });
                                    }
                                    if i > 0 && ui::Bouton::icone_seule(Icon::ArrowUp).cote(18.0).bulle("monter").show(ui).clicked() {
                                        self.commander_musique(C::Deplacer { de: i, vers: i - 1 });
                                    }
                                });
                            });
                        });
                    }
                    // Le serveur n'envoie que le début d'une longue file.
                    let reste = n.saturating_sub(etat.file.len());
                    if reste > 0 {
                        ui.label(Texte::Aide.riche(ui, format!("… et {reste} autre(s)")));
                    }
                });
            }
            // Les playlists du groupe, sous la file.
            {
                let ui = &mut cols[1];
                ui.add_space(espace::S);
                ui.label(Texte::Corps.gras(ui, format!("Playlists · {}", etat.playlists.len())));
                for pl in &etat.playlists {
                    ui.horizontal(|ui| {
                        ui.add(egui::Label::new(RichText::new(&pl.nom).size(texte::COURANT)).truncate())
                            .on_hover_text(format!("{} pistes · {}", pl.pistes, mmss(pl.duree_s as u64)));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_enabled_ui(peut, |ui| {
                                if ui::Bouton::icone_seule(Icon::Trash).cote(18.0).bulle("supprimer la playlist").show(ui).clicked() {
                                    self.commander_musique(C::PlaylistSupprimer { nom: pl.nom.clone() });
                                }
                                if ui.small_button("+").on_hover_text("à la suite de la file").clicked() {
                                    self.commander_musique(C::PlaylistCharger { nom: pl.nom.clone(), remplacer: false });
                                }
                                if ui.small_button("▶").on_hover_text("jouer à la place de la file").clicked() {
                                    self.commander_musique(C::PlaylistCharger { nom: pl.nom.clone(), remplacer: true });
                                }
                            });
                            ui.label(Texte::Minuscule.riche(ui, format!("{}", pl.pistes)));
                        });
                    });
                }
                if peut && (en_cours.is_some() || !etat.file.is_empty()) {
                    ui.horizontal(|ui| {
                        let champ = ui.add(
                            egui::TextEdit::singleline(&mut self.musique.playlist_nom)
                                .margin(egui::Margin::symmetric(4, 5))
                                .hint_text("nom de la playlist")
                                .desired_width(140.0)
                                .char_limit(ki_protocol::MAX_NOM_PLAYLIST),
                        );
                        menu_edition(&champ, &mut self.musique.playlist_nom, false);
                        let nom = self.musique.playlist_nom.trim().to_string();
                        let entree = champ.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.add_enabled(!nom.is_empty(), egui::Button::new("Enregistrer la file")).clicked() || (entree && !nom.is_empty()) {
                            self.commander_musique(C::PlaylistEnregistrer { nom });
                            self.musique.playlist_nom.clear();
                        }
                    });
                }
            }
            // La recherche.
            {
                let ui = &mut cols[2];
                ui.add_enabled_ui(peut_ajouter, |ui| {
                    let champ = ui.add(ui::champ(
                        &mut self.musique.recherche,
                        "chercher, ou coller l'adresse d'un morceau ou d'une playlist…",
                    ));
                    menu_edition(&champ, &mut self.musique.recherche, false);
                    let entree = champ.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let mut lancer = entree;
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.musique.soundcloud, false, "YouTube");
                        ui.selectable_value(&mut self.musique.soundcloud, true, "SoundCloud");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui::Bouton::icone_seule(Icon::Loupe).cote(24.0).bulle("chercher (Entrée)").show(ui).clicked() {
                                lancer = true;
                            }
                        });
                    });
                    let texte = self.musique.recherche.trim().to_string();
                    if lancer && !texte.is_empty() {
                        // Une adresse collée n'a rien à chercher : elle part en file.
                        if ki_protocol::url_musique_valide(&texte) {
                            self.commander_musique(C::Ajouter { url: texte, maintenant: false });
                            self.musique.recherche.clear();
                        } else {
                            let source = if self.musique.soundcloud { "soundcloud" } else { "youtube" }.to_string();
                            self.commander_musique(C::Chercher { texte, source });
                        }
                    }
                });
                if !peut_ajouter {
                    ui::hint(ui, "réservé aux modérateurs");
                }
                if !self.musique.resultats.is_empty() {
                    ui::hint(ui, format!("résultats pour « {} »", self.musique.resultats_pour));
                }
                let resultats = self.musique.resultats.clone();
                egui::ScrollArea::vertical().id_salt("musique_resultats").max_height(150.0).auto_shrink([false, true]).show(ui, |ui| {
                    for p in &resultats {
                        ui.horizontal(|ui| {
                            let nom = if p.artiste.is_empty() { p.titre.clone() } else { format!("{} — {}", p.artiste, p.titre) };
                            ui.add(egui::Label::new(RichText::new(nom).size(texte::COURANT)).truncate())
                                .on_hover_text(format!("{} · {}", mmss(p.duree_s as u64), p.url));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.add_enabled_ui(peut, |ui| {
                                    if ui.small_button("▶").on_hover_text("jouer maintenant").clicked() {
                                        self.commander_musique(C::AjouterPiste { piste: p.clone(), maintenant: true });
                                    }
                                });
                                ui.add_enabled_ui(peut_ajouter, |ui| {
                                    if ui.small_button("+").on_hover_text("ajouter à la file").clicked() {
                                        self.commander_musique(C::AjouterPiste { piste: p.clone(), maintenant: false });
                                    }
                                });
                                self.etoile_musique(ui, p, peut);
                                ui.label(Texte::Minuscule.riche(ui, mmss(p.duree_s as u64)));
                            });
                        });
                    }
                });
            }
        });
    }
}
