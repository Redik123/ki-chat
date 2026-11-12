//! La colonne des salons : textuels et vocaux, leurs occupants, le mot de passe
//! d'un salon vocal verrouillé.

use ki_ui::typo::Texte;
use eframe::egui::{self, Color32, Sense, Vec2};
use ki_protocol::{ChannelId, ChannelKind, ClientMsg, Member};
use ki_ui::flex::Flex;
use ki_ui::jetons::{espace, marge, rayon, texte};

use crate::{
    icons, porte_ui, theme, ui, KiApp, NonLu, VoiceSnapshot, PIED_MARGE, SIDEBAR_LARGEURS,
    SIDEBAR_WIDTH, SPEAK_LEVEL,
};
use crate::icons::Icon;
use crate::membres::{fantome_deplacement, member_row, DeplacementVocal, MemberRow};
use crate::saisie::menu_edition;
use crate::theme::{color_for, ACCENT, DANGER, SPEAK, TEXT, TEXT_DIM};
use crate::ui::Ton;

/// Saisie du mot de passe d'un salon vocal verrouillé.
pub(crate) struct VoicePrompt {
    pub(crate) channel: ChannelId,
    pub(crate) password: String,
    /// Vrai après un refus : on distingue « il en faut un » de « ce n'est
    /// pas le bon », sinon on ne sait pas si l'on s'est trompé.
    pub(crate) wrong: bool,
}

/// Ligne de salon : pastille pleine largeur, filet d'accent si sélectionnée.
///
/// `non_lu` : le nom passe en clair (la « graisse » de l'application, voir
/// `strong()`), et une pastille à droite dit combien — à l'accent quand on
/// y est nommé, discrète sinon. Le style est celui de `pastille_bot`, pour
/// que les deux se ressemblent.
/// `temporaire` : le salon d'une porte web — icône d'invité, badge
/// « temporaire » — plutôt qu'un « # » comme les autres.
pub(crate) fn channel_row(
    ui: &mut egui::Ui,
    name: &str,
    selected: bool,
    kind: ChannelKind,
    non_lu: Option<NonLu>,
    temporaire: bool,
) -> egui::Response {
    // Le pictogramme dit la nature du salon : on lit un « # », on parle
    // dans un haut-parleur.
    let symbole = match kind {
        _ if temporaire => Icon::User,
        // Un salon d'une nature inconnue n'est pas listé ; s'il l'était,
        // un « # » ne promettrait rien de faux.
        ChannelKind::Text | ChannelKind::Inconnu => Icon::Hash,
        ChannelKind::Voice => Icon::Volume,
    };
    ligne_de_salon(ui, name, selected, symbole, non_lu, temporaire)
}

/// La ligne elle-même, avec le pictogramme qu'on lui donne : un salon, ou
/// ce qui en tient lieu dans la colonne (Claude).
pub(crate) fn ligne_de_salon(
    ui: &mut egui::Ui,
    name: &str,
    selected: bool,
    symbol: Icon,
    non_lu: Option<NonLu>,
    temporaire: bool,
) -> egui::Response {
    let height = 34.0;
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let bg = if selected {
            theme::alpha(ACCENT, 26)
        } else if response.hovered() {
            theme::BG_HOVER
        } else {
            Color32::TRANSPARENT
        };
        if bg != Color32::TRANSPARENT {
            painter.rect_filled(rect, egui::CornerRadius::same(rayon::L), bg);
        }
        if selected {
            let bar = egui::Rect::from_min_size(
                egui::pos2(rect.left() + 2.0, rect.center().y - 8.0),
                Vec2::new(3.0, 16.0),
            );
            painter.rect_filled(bar, egui::CornerRadius::same(rayon::S), ACCENT);
        }
        let fg = if selected {
            ACCENT
        } else if response.hovered() || non_lu.is_some() {
            TEXT
        } else {
            TEXT_DIM
        };
        let icon = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 12.0, rect.center().y - 8.0),
            Vec2::splat(16.0),
        );
        icons::draw(painter, icon, symbol, if temporaire { theme::INVITE } else { fg });
        // La pastille d'abord : le nom se tronque à sa gauche, jamais
        // par-dessus.
        let mut droite = rect.right() - 10.0;
        if temporaire {
            // Le badge « temporaire », à la couleur des invités : ce salon
            // s'effacera, et ce qui s'y dit avec.
            let galley = ui.fonts_mut(|f| {
                f.layout_no_wrap("temporaire".into(), egui::FontId::proportional(texte::MINUSCULE), theme::INVITE)
            });
            let taille = Vec2::new(galley.size().x + 8.0, 14.0);
            let badge = egui::Rect::from_min_size(
                egui::pos2(droite - taille.x, rect.center().y - taille.y / 2.0),
                taille,
            );
            painter.rect_stroke(
                badge,
                egui::CornerRadius::same(rayon::S),
                egui::Stroke::new(1.0_f32, theme::alpha(theme::INVITE, 140)),
                egui::StrokeKind::Inside,
            );
            painter.galley(
                egui::pos2(badge.left() + 4.0, badge.center().y - galley.size().y / 2.0),
                galley,
                theme::INVITE,
            );
            droite = badge.left() - 6.0;
        }
        if let Some(n) = non_lu {
            let texte = if n.nb > 99 { "99+".to_string() } else { n.nb.to_string() };
            let galley = ui.fonts_mut(|f| {
                f.layout_no_wrap(texte, egui::FontId::proportional(texte::MINUSCULE), theme::BG_DEEP)
            });
            let taille = Vec2::new(galley.size().x + 10.0, 16.0);
            let pastille = egui::Rect::from_min_size(
                egui::pos2(droite - taille.x, rect.center().y - taille.y / 2.0),
                taille,
            );
            let fond = if n.mention { ACCENT } else { TEXT_DIM };
            painter.rect_filled(pastille, egui::CornerRadius::same(rayon::L), fond);
            painter.galley(
                egui::pos2(pastille.center().x - galley.size().x / 2.0, pastille.center().y - galley.size().y / 2.0),
                galley,
                theme::BG_DEEP,
            );
            droite = pastille.left() - 6.0;
        }
        // « En gras » au sens de l'application : `strong()` n'a pas de
        // graisse, c'est la couleur pleine — `TEXT` au lieu de `TEXT_DIM`.
        let galley =
            ui.fonts_mut(|f| f.layout_no_wrap(name.to_owned(), egui::FontId::proportional(texte::CORPS), fg));
        let gauche = rect.left() + 34.0;
        painter.with_clip_rect(egui::Rect::from_min_max(
            egui::pos2(gauche, rect.top()),
            egui::pos2(droite.max(gauche), rect.bottom()),
        ))
        .galley(egui::pos2(gauche, rect.center().y - galley.size().y / 2.0), galley, fg);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

impl KiApp {
    /// Colonne de gauche : marque, salons, membres, et mon compte en pied.
    pub(crate) fn sidebar(&mut self, ui: &mut egui::Ui, voice: &VoiceSnapshot) {
        let ctx = &ui.ctx().clone();
        // Résolus avant le panneau : la fermeture emprunte déjà `self`.
        let header_name = self.server_label();
        let header_icon = self
            .active_server()
            .cloned()
            .and_then(|server| self.server_icon(ctx, &server));

        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(SIDEBAR_WIDTH)
            .size_range(SIDEBAR_LARGEURS)
            .frame(egui::Frame::NONE.fill(theme::BG_SIDE))
            .show(ui, |ui| {
                // --- En-tête : marque + serveur ---
                egui::Panel::top("brand")
                    .frame(
                        egui::Frame::NONE
                            .fill(theme::BG_SIDE)
                            .inner_margin(marge::symetrique(espace::L, espace::L)),
                    )
                    .show(ui, |ui| {
                        // Le logo et le nom du serveur priment : avec
                        // plusieurs serveurs enregistrés, c'est ce qui situe.
                        let mut deconnecter = false;
                        Flex::ligne().ecart(espace::M).show(ui, "en-tete-serveur", |f| {
                            f.ui(|ui| {
                                let (rect, _) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::hover());
                                match &header_icon {
                                    Some(icon) => ui::paint_server_badge(
                                        ui.painter(),
                                        rect,
                                        &header_name,
                                        self.url.trim(),
                                        Some(icon),
                                    ),
                                    None => icons::logo(ui.painter(), rect, ACCENT, theme::BG_SIDE),
                                }
                            });
                            // Le nom et l'adresse prennent la place qui reste
                            // — tronqués s'il le faut —, le bouton de
                            // déconnexion garde la sienne, à droite.
                            f.grandit(|ui| {
                                ui.add(
                                    egui::Label::new(
                                        Texte::TitreSection.riche(ui, &header_name),
                                    )
                                    .truncate(),
                                );
                                ui.add(
                                    egui::Label::new(
                                        Texte::Aide.riche(ui, self.url.trim()),
                                    )
                                    .truncate(),
                                );
                            });
                            // Se déconnecter : à côté du serveur qu'on quitte.
                            f.ui(|ui| {
                                deconnecter =
                                    ui::Bouton::icone_seule(Icon::Logout).bulle("Se déconnecter de ce serveur").show(ui).clicked();
                            });
                        });
                        if deconnecter {
                            // Voulu : on ne reviendra pas tout seul au
                            // prochain lancement.
                            self.session_auto = None;
                            self.disconnect(None);
                        }
                    });

                // --- Pied : mon compte ---
                // De la hauteur de la barre de saisie d'à côté, contenu
                // centré : la carte et la barre s'alignent.
                egui::Panel::bottom("me")
                    .exact_size(self.hauteur_saisie + 2.0 * PIED_MARGE)
                    .frame(
                        egui::Frame::NONE
                            .fill(theme::BG_SIDE)
                            .inner_margin(marge::symetrique(espace::L, 0.0)),
                    )
                    .show(ui, |ui| {
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            let me = self.username.clone();
                            let mine = self.session.my_id.and_then(|id| self.avatars.get(&id));
                            let clicked = ui::Avatar::new(&me, 30.0).parle(self.vocal.transmitting).photo(mine.map(|(_, t)| t)).fond(theme::BG_SIDE).show(ui)
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked();
                            // Le pseudo et l'état, serrés l'un sur l'autre à
                            // la hauteur de l'avatar — le pseudo sur sa moitié
                            // haute, l'état sur la basse —, peints d'un bloc
                            // que la rangée centre comme l'avatar et
                            // l'engrenage. Empilés en widgets, l'interligne
                            // d'egui les écartait : le pseudo dépassait
                            // au-dessus de l'avatar, seul en haut.
                            let (dot, text) = if self.vocal.muted {
                                (DANGER, "micro coupé")
                            } else if self.vocal.transmitting {
                                (SPEAK, "en émission")
                            } else {
                                (theme::BORDER, "connecté")
                            };
                            let largeur = (ui.available_width() - 44.0).max(60.0);
                            let (bloc, reponse) =
                                ui.allocate_exact_size(Vec2::new(largeur, 32.0), Sense::click());
                            if ui.is_rect_visible(bloc) {
                                let p = ui.painter_at(bloc);
                                p.text(
                                    egui::pos2(bloc.left(), bloc.top() + 9.0),
                                    egui::Align2::LEFT_CENTER,
                                    &me,
                                    egui::FontId::proportional(texte::CORPS),
                                    color_for(&me),
                                );
                                let y = bloc.top() + 24.0;
                                icons::dot(&p, egui::pos2(bloc.left() + 4.0, y), 8.0 * 0.32, dot);
                                p.text(
                                    egui::pos2(bloc.left() + 12.0, y),
                                    egui::Align2::LEFT_CENTER,
                                    text,
                                    egui::FontId::proportional(texte::COURANT),
                                    dot,
                                );
                            }
                            let named = reponse
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .on_hover_text("gérer mon compte")
                                .clicked();
                            if clicked || named {
                                self.show_account = true;
                            }

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui::Bouton::icone_seule(Icon::Gear).bulle("Réglages").show(ui).clicked()
                                    {
                                        // Fermer par l'engrenage range tout comme
                                        // la croix : calibration, « M'écouter »,
                                        // essai — rien ne reste en plan.
                                        if self.show_settings {
                                            self.close_settings();
                                        } else {
                                            self.show_settings = true;
                                            let (i, o) = ki_voice::list_devices();
                                            self.input_devices = i;
                                            self.output_devices = o;
                                        }
                                    }
                                },
                            );
                        });
                    });

                // --- Salons et membres ---
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::NONE
                            .fill(theme::BG_SIDE)
                            .inner_margin(marge::symetrique(espace::M, espace::XS)),
                    )
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink(false)
                            .show(ui, |ui| {
                                // Les salons se parcourent par rang, sans rien
                                // retirer ni copier : les menus ouverts depuis
                                // cette barre — « Déplacer en vocal » d'un
                                // occupant, le vocal d'un invité web — lisent
                                // `self.session.channels` pendant le rendu et doivent le
                                // trouver entier. La liste en était retirée le
                                // temps du rendu (`mem::take`), et le sous-menu
                                // « Déplacer en vocal » s'ouvrait vide.

                                // --- Salons textuels : on les ouvre ---
                                ui::section_label(ui, "Salons textuels");
                                let ailleurs = self.claude.ouvert && self.claude.servi;
                                for i in 0..self.session.channels.len() {
                                    let Some(ch) = self.session.channels.get(i) else { break };
                                    if ch.kind != ChannelKind::Text || ch.expire_le.is_some() {
                                        continue;
                                    }
                                    let id = ch.id;
                                    // Le salon de Claude ou la page VALORANT
                                    // tiennent la place du chat : aucun salon
                                    // n'est alors « celui qu'on lit ».
                                    let selected = self.session.current == Some(id) && !ailleurs;
                                    let pastille = self.session.non_lus.get(&id).filter(|n| n.nb > 0).copied();
                                    let row = channel_row(ui, &ch.name, selected, ChannelKind::Text, pastille, false);
                                    if row.clicked() {
                                        self.retour_au_chat();
                                        if self.session.current != Some(id) {
                                            self.join(id);
                                        }
                                    }
                                }
                                // Claude : un salon à soi, à la suite des
                                // autres. Absent si le serveur n'en a pas.
                                if self.claude.servi {
                                    let pastille = self.claude.non_lu.then_some(NonLu { nb: 1, mention: true, depuis: 0 });
                                    let row = ligne_de_salon(ui, "Claude", self.claude.ouvert, Icon::Star, pastille, false)
                                        .on_hover_text("ton salon privé avec Claude : toi et le propriétaire du serveur le voyez");
                                    if row.clicked() && !self.claude.ouvert {
                                        self.ouvrir_claude();
                                    }
                                }

                                // --- Salons temporaires des portes web : à part,
                                // avec ce qu'il leur reste à vivre. Un client
                                // antérieur les voit parmi les textuels, c'est
                                // voulu : `expire_le` lui est inconnu.
                                let temporaires: Vec<usize> = (0..self.session.channels.len())
                                    .filter(|&i| self.session.channels[i].expire_le.is_some())
                                    .collect();
                                if !temporaires.is_empty() {
                                    ui.add_space(espace::L);
                                    ui::section_label(ui, "Invités");
                                    let maintenant = porte_ui::maintenant_ms();
                                    let contexte = self.contexte_portes();
                                    let mut actions = Vec::new();
                                    let mut lire = None;
                                    let mut clique = false;
                                    for i in temporaires {
                                        let Some(ch) = self.session.channels.get(i) else { continue };
                                        let selected = self.session.current == Some(ch.id);
                                        let pastille = self.session.non_lus.get(&ch.id).filter(|n| n.nb > 0).copied();
                                        let reste = porte_ui::reste_texte(ch.expire_le.unwrap_or(0), maintenant);
                                        let slug = self.portes.slug_du_salon(ch.id);
                                        let bulle = match slug {
                                            Some(slug) => format!("salon temporaire de la porte « {slug} » · {reste}"),
                                            None => format!("salon temporaire d'une porte web · {reste}"),
                                        };
                                        let row = channel_row(ui, &ch.name, selected, ChannelKind::Text, pastille, true)
                                            .on_hover_text(bulle);
                                        if row.clicked() {
                                            clique = true;
                                            if !selected {
                                                lire = Some(ch.id);
                                            }
                                        }
                                        // Fermer la porte depuis son salon : la mienne,
                                        // ou n'importe laquelle avec « Expulser ».
                                        if let Some(slug) = slug.filter(|s| self.portes.peut_fermer(s, &contexte)) {
                                            let slug = slug.to_string();
                                            row.context_menu(|ui| {
                                                ui.set_width(200.0);
                                                if ui::Bouton::new("Fermer la porte")
                                                    .icone(Icon::Ban)
                                                    .teinte(Ton::Danger)
                                                    .show(ui)
                                                    .on_hover_text("les invités sont congédiés et le salon effacé")
                                                    .clicked()
                                                {
                                                    actions.push(porte_ui::Action::Envoyer(ClientMsg::PorteFermer { slug }));
                                                    ui.close();
                                                }
                                            });
                                        }
                                    }
                                    if clique {
                                        self.retour_au_chat();
                                    }
                                    if let Some(id) = lire {
                                        self.join(id);
                                    }
                                    self.appliquer_portes(actions);
                                }

                                // --- Salons vocaux : on y entre ---
                                ui.add_space(espace::L);
                                ui::section_label(ui, "Salons vocaux");
                                for i in 0..self.session.channels.len() {
                                    let Some(ch) = self.session.channels.get(i) else { break };
                                    if ch.kind != ChannelKind::Voice {
                                        continue;
                                    }
                                    let id = ch.id;
                                    let here = self.session.voice_channel == Some(id);
                                    let row = channel_row(ui, &ch.name, here, ChannelKind::Voice, None, false);
                                    if row.clicked() {
                                        if here {
                                            self.leave_voice();
                                        } else {
                                            self.join_voice(id);
                                        }
                                    }
                                    // Qui est dans ce salon vocal, comme sur
                                    // Discord : la présence se lit d'un coup
                                    // d'œil, sans y entrer.
                                    let occupants = self.voice_occupants(ui, id, voice);
                                    // L'intitulé et ses occupants : une seule
                                    // cible où déposer quelqu'un qu'on glisse.
                                    let bloc = occupants.map_or(row.rect, |r| row.rect.union(r));
                                    self.deposer_en_vocal(ui, bloc, id);
                                }
                                fantome_deplacement(ui.ctx());
                            });
                    });
            });
    }

    /// Occupants d'un salon vocal, listés sous son intitulé. Rend la place
    /// qu'ils prennent : la cible du glisser-déposer les couvre aussi.
    pub(crate) fn voice_occupants(&mut self, ui: &mut egui::Ui, channel: ChannelId, voice: &VoiceSnapshot) -> Option<egui::Rect> {
        let members: Vec<Member> = self.session
            .members
            .iter()
            .filter(|m| m.voice == Some(channel))
            .cloned()
            .collect();
        if members.is_empty() {
            return None;
        }

        let mut place: Option<egui::Rect> = None;
        for m in &members {
            let is_me = Some(m.user_id) == self.session.my_id;
            let level = if is_me {
                voice.stats.mic_peak
            } else {
                voice.levels.get(&m.user_id).copied().unwrap_or(0.0)
            };
            // Pour soi, l'état d'émission local fait foi. Pour les autres, le
            // drapeau du serveur **ou** le niveau reçu : le second sert de
            // filet si un `VoiceState` se perd, et retombe de lui-même.
            let speaking =
                if is_me { self.vocal.transmitting } else { m.speaking || level > SPEAK_LEVEL };
            let photo = self.avatar_of(m.user_id);
            // Cette liste ne contient que des occupants du vocal : l'icône
            // « muet » s'y montre sans autre condition. Pour soi, l'état
            // local fait foi — l'écho serveur peut être en retard d'un aller.
            let muted = if is_me { self.vocal.muted } else { m.muted };
            // S'attrape : soi-même (le déposer ailleurs, c'est y aller), et
            // qui l'on a le pouvoir de déplacer.
            let glissable = is_me || self.peut_deplacer(m);
            let (response, regarder) = member_row(
                ui,
                MemberRow {
                    member: m,
                    speaking,
                    muted,
                    is_me,
                    photo,
                    rang_icone: None,
                    glissable,
                    spectateurs: self.nb_spectateurs(m),
                },
            );
            if glissable && response.drag_started_by(egui::PointerButton::Primary) {
                egui::DragAndDrop::set_payload(
                    ui.ctx(),
                    DeplacementVocal { username: m.username.clone(), depuis: channel, moi: is_me },
                );
            }
            place = Some(place.map_or(response.rect, |p| p.union(response.rect)));
            if regarder {
                self.regarder(m);
            }
            self.member_menu(response, m, is_me);
        }
        ui.add_space(espace::S);
        place
    }

    /// Demande le mot de passe d'un salon vocal verrouillé.
    pub(crate) fn voice_password_window(&mut self, ctx: &egui::Context) {
        let Some(prompt) = self.voice_prompt.as_mut() else { return };
        let channel = prompt.channel;
        let name = self.session
            .channels
            .iter()
            .find(|c| c.id == channel)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        let wrong = prompt.wrong;
        let mut submit = false;
        let mut cancel = false;

        let boite = ki_ui::fenetre::Modale::new("salon_verrouille", format!("Salon verrouillé — {name}"))
            .largeur(300.0)
            .show(ctx, |ui| {
                if wrong {
                    ui::bandeau(ui, Ton::Danger, "Mot de passe incorrect");
                    ui.add_space(espace::S);
                }
                ui::field_label(ui, "Mot de passe du salon");
                let response = ui.add(ui::champ_secret(&mut prompt.password, ""));
                menu_edition(&response, &mut prompt.password, true);
                response.request_focus();
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                ui.add_space(espace::M);
                ui.horizontal(|ui| {
                    if ui::Bouton::new("Entrer").icone(Icon::Check).principal().show(ui).clicked() {
                        submit = true;
                    }
                    if ui::Bouton::new("Annuler").icone(Icon::Close).show(ui).clicked() {
                        cancel = true;
                    }
                });
            });

        if submit {
            if let Some(prompt) = self.voice_prompt.take() {
                // Même intention que par le clic ordinaire : sans elle, un
                // refus laissait l'affichage dans un salon où l'on n'est pas.
                self.session.voice_channel = Some(channel);
                self.set_voice_intent(Some(channel));
                self.mots_de_passe_vocaux.insert(channel, prompt.password.clone());
                self.send(ClientMsg::JoinVoice {
                    channel,
                    password: Some(prompt.password),
                });
            }
        } else if cancel || boite.fermee {
            self.voice_prompt = None;
        }
    }
}
