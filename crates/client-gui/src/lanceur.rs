//! L'écran d'avant la connexion : le carnet de serveurs, l'éditeur d'un serveur,
//! les identifiants.

use std::collections::HashMap;

use ki_ui::typo::Texte;
use eframe::egui::{self, Color32, RichText, Sense, Vec2};
use ki_ui::jetons::{espace, rayon, texte};

use crate::{icons, secret, servers, theme, ui, KiApp};
use crate::formats::ellipsize;
use crate::icons::Icon;
use crate::theme::{ACCENT, DANGER, SPEAK, TEXT, TEXT_DIM, TEXT_FAINT, WARN};
use crate::ui::Ton;

/// Formulaire d'ajout ou de modification d'un serveur.
pub(crate) struct ServerDraft {
    /// `None` = création.
    pub(crate) id: Option<u64>,
    /// Alias local, pas le nom officiel du serveur.
    pub(crate) name: String,
    pub(crate) address: String,
}

/// L'écran de connexion : le carnet des serveurs, celui qu'on a choisi, et ce
/// que le formulaire retient (mot de passe, code d'invitation).
pub(crate) struct Lanceur {
    /// Le carnet : les serveurs enregistrés, avec ce qu'on en retient.
    pub(crate) book: Vec<servers::Server>,
    /// Serveur sélectionné dans le lanceur.
    pub(crate) selected: Option<u64>,
    /// Le serveur où l'on était quand l'application a été quittée : au
    /// lancement suivant, on s'y reconnecte sans rien cliquer. Effacé par
    /// une déconnexion voulue depuis l'application — et par elle seule.
    pub(crate) session_auto: Option<u64>,
    /// La reprise de session n'est tentée qu'une fois par lancement.
    pub(crate) auto_tente: bool,
    /// L'état et le ping de chaque serveur du carnet, sondés en tâche de fond.
    pub(crate) probes: servers::Probes,
    /// Formulaire ouvert par le bouton « + » ou le crayon.
    pub(crate) draft: Option<ServerDraft>,
    /// Textures des logos, montées à la demande. `None` = pas de logo (ou
    /// vignette illisible) : on retient l'échec pour ne pas réessayer.
    pub(crate) icon_textures: HashMap<u64, Option<egui::TextureHandle>>,
    /// Le mot de passe saisi, ou relu du coffre de Windows.
    pub(crate) password: String,
    /// « Retenir le mot de passe » : il va au coffre, et la session reprend
    /// seule au lancement suivant.
    pub(crate) remember_password: bool,
    /// Le code d'invitation saisi, pour créer un compte.
    pub(crate) invite: String,
    /// Le champ « code d'invitation » n'est affiché qu'à la demande.
    pub(crate) show_invite: bool,
}

impl ServerDraft {
    pub(crate) fn new() -> Self {
        Self { id: None, name: String::new(), address: String::new() }
    }

    pub(crate) fn edit(server: &servers::Server) -> Self {
        Self {
            id: Some(server.id),
            name: server.name.clone(),
            address: server.address.clone(),
        }
    }
}

/// Ce qu'un clic sur une ligne de serveur a déclenché.
pub(crate) enum RowAction {
    None,
    Select,
    Edit,
}

/// Logo + nom + accroche, centrés au pixel près, posés sur un halo.
pub(crate) fn brand(ui: &mut egui::Ui) {
    let galley = ui.fonts_mut(|f| {
        f.layout_no_wrap("ki-chat".into(), egui::FontId::proportional(38.0), ACCENT)
    });
    let mark = 56.0;
    let total = mark + 16.0 + galley.size().x;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), mark), Sense::hover());
    let left = rect.center().x - total / 2.0;

    // Halo derrière la marque. Dessiné avant tout le reste, il reste sous
    // la carte qui sera posée par-dessus.
    ui::glow(
        ui.painter(),
        rect.center(),
        300.0,
        theme::alpha(ACCENT, 30),
    );

    icons::logo(
        ui.painter(),
        egui::Rect::from_min_size(
            egui::pos2(left, rect.center().y - mark / 2.0),
            Vec2::splat(mark),
        ),
        ACCENT,
        theme::BG_DEEP,
    );
    let baseline = rect.center().y - galley.size().y / 2.0;
    ui.painter()
        .galley(egui::pos2(left + mark + 15.0, baseline), galley, ACCENT);

    ui.add_space(espace::S);
    ui.label(
        Texte::Corps.riche_ton(ui, Ton::Pale, "chat & vocal basse latence — 100 % Rust"),
    );
}

/// Ligne du carnet de serveurs : nom, adresse, et état mesuré à l'avance.
pub(crate) fn server_row(
    ui: &mut egui::Ui,
    server: &servers::Server,
    selected: bool,
    reach: servers::Reach,
    icon: Option<&egui::TextureHandle>,
) -> RowAction {
    const HEIGHT: f32 = 52.0;
    const EDIT: f32 = 24.0;
    const BADGE: f32 = 32.0;

    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), HEIGHT), Sense::click());

    // Le crayon est posé avec `interact` et peint à la main, pas via un
    // sous-Ui : `scope_builder` recale le curseur du parent sur le rectangle
    // de l'enfant, et comme le crayon est plus court que la ligne, cela
    // rognait l'espacement de la ligne suivante — l'écart changeait donc
    // selon la ligne sélectionnée. `interact` ne touche pas à la mise en page.
    let edit_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - EDIT - 8.0, rect.center().y - EDIT / 2.0),
        Vec2::splat(EDIT),
    );
    let edit = ui
        .interact(edit_rect, response.id.with("edit"), Sense::click())
        .on_hover_text("Modifier ou supprimer");

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let bg = if selected {
            theme::mix(theme::BG_DEEP, ACCENT, 0.12)
        } else if response.hovered() {
            theme::mix(theme::BG_DEEP, theme::BG_HOVER, 0.75)
        } else {
            theme::BG_DEEP
        };
        painter.rect(
            rect,
            egui::CornerRadius::same(rayon::XL),
            bg,
            egui::Stroke::new(1.0_f32, if selected { ACCENT } else { theme::BORDER_SOFT }),
            egui::StrokeKind::Inside,
        );
        if selected {
            let bar = egui::Rect::from_min_size(
                egui::pos2(rect.left() + 4.0, rect.center().y - 11.0),
                Vec2::new(3.0, 22.0),
            );
            painter.rect_filled(bar, egui::CornerRadius::same(rayon::S), ACCENT);
        }

        let badge = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 11.0, rect.center().y - BADGE / 2.0),
            Vec2::splat(BADGE),
        );
        ui::paint_server_badge(painter, badge, &server.name, server.address.trim(), icon);

        // Sans nom donné, l'adresse tient lieu de titre : inutile de la
        // répéter en dessous.
        let text_left = badge.right() + 11.0;
        let named = server.has_label();
        let title_color = if selected { ACCENT } else { TEXT };
        if named {
            painter.text(
                egui::pos2(text_left, rect.center().y - 8.0),
                egui::Align2::LEFT_CENTER,
                server.label(),
                egui::FontId::proportional(texte::CORPS),
                title_color,
            );
            let mut address = server.address.trim().to_string();
            if let Some(tag) = address_tag(&address) {
                address.push_str(" · ");
                address.push_str(tag);
            }
            painter.text(
                egui::pos2(text_left, rect.center().y + 9.0),
                egui::Align2::LEFT_CENTER,
                address,
                egui::FontId::proportional(texte::PETIT),
                TEXT_FAINT,
            );
        } else {
            painter.text(
                egui::pos2(text_left, rect.center().y),
                egui::Align2::LEFT_CENTER,
                server.address.trim(),
                egui::FontId::proportional(texte::CORPS),
                title_color,
            );
        }

        // Qualité : des barres de réseau se lisent d'un coup d'œil, bien
        // mieux qu'un rond de couleur, et la valeur reste lisible à côté.
        // Chasse fixe pour les valeurs : les millisecondes s'alignent d'une
        // ligne à l'autre. Les états en toutes lettres restent en romain.
        let (text, color, bars, numeric) = match reach {
            servers::Reach::Online { ping_ms } => {
                let (bars, color) = quality(ping_ms);
                (format!("{ping_ms} ms"), color, Some(bars), true)
            }
            servers::Reach::Offline => ("injoignable".to_string(), DANGER, Some(0), false),
            servers::Reach::Probing => ("test…".to_string(), TEXT_FAINT, None, false),
            servers::Reach::Unknown => ("non testé".to_string(), TEXT_FAINT, None, false),
        };
        let font = if numeric {
            egui::FontId::monospace(texte::PETIT)
        } else {
            egui::FontId::proportional(texte::COURANT)
        };
        let galley = ui.fonts_mut(|f| f.layout_no_wrap(text, font, color));
        let value_left = rect.right() - EDIT - 14.0 - galley.size().x;
        let baseline = rect.center().y - galley.size().y / 2.0;
        painter.galley(egui::pos2(value_left, baseline), galley, color);

        let marker = egui::Rect::from_center_size(
            egui::pos2(value_left - 15.0, rect.center().y),
            Vec2::splat(14.0),
        );
        match bars {
            Some(lit) => icons::signal(painter, marker, lit, color, theme::BORDER),
            None => ui::spinner(painter, marker.center(), 5.0, ui.input(|i| i.time), color),
        }

        // Le crayon ne se montre qu'au survol ou sur la ligne courante : la
        // liste reste calme. Sa place est réservée en permanence, pour que le
        // ping ne se décale pas quand la souris passe.
        if response.hovered() || edit.hovered() || selected {
            if edit.hovered() {
                let bg = if edit.is_pointer_button_down_on() {
                    theme::BG_ACTIVE
                } else {
                    theme::BG_HOVER
                };
                painter.rect_filled(edit_rect, egui::CornerRadius::same(rayon::L), bg);
            }
            let fg = if edit.hovered() { TEXT } else { TEXT_FAINT };
            icons::draw(painter, edit_rect.shrink(EDIT * 0.24), Icon::Pencil, fg);
        }
    }

    // Le crayon est au-dessus : il prend le clic avant la ligne.
    if edit.clicked() {
        RowAction::Edit
    } else if response.clicked() {
        RowAction::Select
    } else {
        RowAction::None
    }
}

/// Nombre de barres allumées et couleur, pour un ping donné.
pub(crate) fn quality(ping_ms: u32) -> (u8, Color32) {
    match ping_ms {
        0..=29 => (4, SPEAK),
        30..=79 => (3, ACCENT),
        80..=149 => (2, WARN),
        _ => (1, TEXT_DIM),
    }
}

/// Étiquette courte qui situe une adresse, ou `None` si elle est publique.
pub(crate) fn address_tag(address: &str) -> Option<&'static str> {
    let host = address.trim().rsplit_once(':').map_or(address.trim(), |(h, _)| h);
    if matches!(host, "localhost" | "127.0.0.1" | "::1") {
        return Some("local");
    }
    let mut parts = host.split('.').map(str::parse::<u8>);
    let (Some(Ok(a)), Some(Ok(b)), Some(Ok(_)), Some(Ok(_)), None) =
        (parts.next(), parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    // Plages privées RFC 1918.
    let private = a == 10 || (a == 192 && b == 168) || (a == 172 && (16..=31).contains(&b));
    private.then_some("réseau local")
}

impl KiApp {
    pub(crate) fn login_screen(&mut self, ui: &mut egui::Ui) {
        let ctx = &ui.ctx().clone();
        // Teste les serveurs en tâche de fond, au plus une fois toutes les
        // 20 s : l'état et le ping sont connus avant même de se connecter.
        self.lanceur.probes.sweep(&self.lanceur.book, ctx, false);

        let editing = self.lanceur.draft.is_some();
        let ready = self.lanceur.selected.is_some()
            && !self.username.trim().is_empty()
            && !self.lanceur.password.is_empty()
            && !self.session.connecting;
        let enter = !editing && ctx.input(|i| i.key_pressed(egui::Key::Enter));
        let mut go = false;

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::BG_DEEP))
            .show(ui, |ui| {
                const CARD: f32 = 420.0;
                ui.vertical_centered(|ui| {
                    // Bloc centré, très légèrement remonté (centre optique).
                    let free = ui.available_height();
                    ui.add_space(((free - 620.0) * 0.40).max(16.0));
                    brand(ui);
                    ui.add_space(espace::XXL);

                    // Le parent centre ses enfants ; à l'intérieur de la carte
                    // on repasse en alignement à gauche pour les étiquettes.
                    ui.allocate_ui_with_layout(
                        Vec2::new(CARD, 0.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_width(CARD);
                            ui::card(ui, |ui| {
                                if editing {
                                    self.server_editor(ui, ctx);
                                } else {
                                    self.server_picker(ui, ctx);
                                    ui.add_space(espace::XL);
                                    go = self.credentials(ui, ready);
                                }
                            });

                            // Une coupure en cours de reprise n'est pas une
                            // erreur à congédier : c'est un état, et il dit
                            // ce qu'il attend. Le rouge est réservé à ce sur
                            // quoi il faut agir.
                            if let Some(dans) = self.reprise_dans() {
                                let essais =
                                    self.reprise.as_ref().map_or(0, |r| r.essais);
                                ui.add_space(espace::L);
                                let quoi = if self.session.connecting {
                                    format!("Connexion perdue — tentative {essais}…")
                                } else {
                                    format!(
                                        "Connexion perdue — nouvelle tentative dans {} s \
                                         (essai {essais})",
                                        dans.as_secs() + 1
                                    )
                                };
                                ui::bandeau(ui, Ton::Alerte, &quoi);
                                ui.add_space(espace::S);
                                ui.horizontal(|ui| {
                                    if ui::Bouton::new("Réessayer maintenant").icone(Icon::Refresh).show(ui)
                                        .clicked()
                                    {
                                        if let Some(r) = &mut self.reprise {
                                            r.quand = std::time::Instant::now();
                                        }
                                    }
                                    // Renoncer rend la main sans rien perdre :
                                    // le formulaire est déjà rempli.
                                    if ui::Bouton::new("Arrêter d'essayer").icone(Icon::Close).show(ui)
                                        .clicked()
                                    {
                                        // Renoncer rend aussi les
                                        // périphériques : les tenir pour une
                                        // connexion qu'on ne cherche plus
                                        // n'apporte rien à personne.
                                        self.reprise = None;
                                        self.link.arreter();
                                    }
                                });
                            } else if let Some(err) = self.error.clone() {
                                ui.add_space(espace::L);
                                if ui::bandeau_fermable(ui, Ton::Danger, &err) {
                                    self.error = None;
                                }
                            }
                            // Identité changée : soit le serveur a été
                            // réinstallé, soit quelqu'un se glisse au milieu.
                            // Le client ne peut pas trancher — l'utilisateur,
                            // lui, sait si l'hébergeur a refait son serveur.
                            if let Some(address) = self.identity_alarm.clone() {
                                ui.add_space(espace::M);
                                ui.label(
                                    Texte::Aide.riche_ton(ui, Ton::Discret, "Si le serveur vient d'être réinstallé, c'est \
                                         normal. Sinon, quelqu'un s'interpose : demande \
                                         son empreinte à l'hébergeur avant d'accepter.",),
                                );
                                ui.add_space(espace::S);
                                if ui::Bouton::new("Accepter la nouvelle identité du serveur").icone(Icon::Key).show(ui)
                                .clicked()
                                {
                                    if let Some(s) = self.lanceur
                                        .book
                                        .iter_mut()
                                        .find(|s| s.address == address)
                                    {
                                        s.cert_fingerprint.clear();
                                    }
                                    self.identity_alarm = None;
                                    self.error = None;
                                }
                            }
                        },
                    );

                    ui.add_space(espace::XL);
                    ui.label(
                        RichText::new(concat!("ki-chat ", env!("CARGO_PKG_VERSION")))
                            .color(theme::BORDER)
                            .size(texte::PETIT),
                    );
                });
            });

        if go || (enter && ready) {
            self.connect(ctx);
        }
    }

    /// Liste des serveurs connus, avec leur état mesuré.
    pub(crate) fn server_picker(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui::section_label(ui, "Serveurs");
            if !self.lanceur.book.is_empty() {
                ui.label(
                    Texte::Aide.gras(ui, self.lanceur.book.len().to_string()).color(theme::BORDER_STRONG),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui::Bouton::icone_seule(Icon::Plus).cote(26.0).couleur(ACCENT).bulle("Ajouter un serveur").show(ui)
                    .clicked()
                {
                    self.lanceur.draft = Some(ServerDraft::new());
                }
                if !self.lanceur.book.is_empty()
                    && ui::Bouton::icone_seule(Icon::Refresh).cote(26.0).bulle("Retester les serveurs").show(ui)
                        .clicked()
                {
                    self.lanceur.probes.sweep(&self.lanceur.book, ctx, true);
                }
            });
        });
        ui.add_space(espace::XXS);

        if self.lanceur.book.is_empty() {
            let mut add = false;
            ui.add_space(espace::M);
            ui.vertical_centered(|ui| {
                ui::glyph(ui, Icon::Server, 30.0, theme::BORDER);
                ui.add_space(espace::M);
                ui.label(
                    Texte::Corps.riche_ton(ui, Ton::Discret, "Aucun serveur enregistré"),
                );
                ui.add_space(espace::XXS);
                ui::hint(ui, "Demande son adresse à l'admin du serveur.");
                ui.add_space(espace::L);
                add = ui::Bouton::new("Ajouter un serveur").icone(Icon::Plus).principal().show(ui)
                    .clicked();
            });
            if add {
                self.lanceur.draft = Some(ServerDraft::new());
            }
            ui.add_space(espace::S);
            return;
        }

        let book = self.lanceur.book.clone();
        for server in &book {
            let selected = self.lanceur.selected == Some(server.id);
            let reach = self.lanceur.probes.reach(server.id);
            let icon = self.server_icon(ctx, server);
            match server_row(ui, server, selected, reach, icon.as_ref()) {
                RowAction::Select => self.select_server(server.id),
                RowAction::Edit => self.lanceur.draft = Some(ServerDraft::edit(server)),
                RowAction::None => {}
            }
            // Pas d'espace ajouté ici : la mise en page verticale intercale
            // déjà `item_spacing.y` entre deux lignes.
        }
    }

    /// Formulaire d'ajout / modification d'un serveur.
    pub(crate) fn server_editor(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(mut draft) = self.lanceur.draft.take() else { return };
        let creating = draft.id.is_none();

        ui::group_title(
            ui,
            if creating { Icon::Plus } else { Icon::Pencil },
            if creating { "Nouveau serveur" } else { "Modifier le serveur" },
        );

        ui::field_label(ui, "Adresse");
        ui.add(ui::champ(&mut draft.address, "hôte ou hôte:port"));
        ui.add_space(espace::XS);
        ui::hint(ui, "Port 9987 par défaut.");

        ui.add_space(espace::L);
        ui::field_label(ui, "Alias local (facultatif)");
        ui.add(ui::champ(&mut draft.name, "ex. Chez Kévin"));
        ui.add_space(espace::XS);
        // Le nom et le logo appartiennent au serveur ; l'alias n'est qu'un
        // pense-bête local, et le logo n'est pas modifiable ici du tout.
        ui::hint(
            ui,
            "Ne change l'affichage que sur cet ordinateur. Le nom et le logo \
             officiels sont ceux définis par les admins du serveur.",
        );

        let valid = !draft.address.trim().is_empty();
        let submit = valid && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));

        ui.add_space(espace::XL);
        let mut close = false;
        ui.horizontal(|ui| {
            let save = ui
                .add_enabled_ui(valid, |ui| {
                    ui::Bouton::new("Enregistrer").icone(Icon::Check).principal().show(ui)
                })
                .inner
                .clicked();
            if save || submit {
                self.commit_draft(&draft, ctx);
                close = true;
            }
            if ui::Bouton::new("Annuler").icone(Icon::Close).show(ui).clicked() || cancel {
                close = true;
            }
            if let Some(id) = draft.id {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui::Bouton::new("Supprimer").icone(Icon::Trash).teinte(Ton::Danger).show(ui).clicked()
                    {
                        self.delete_server(id);
                        close = true;
                    }
                });
            }
        });

        if !close {
            self.lanceur.draft = Some(draft);
        }
    }

    /// Identifiants du serveur sélectionné. Renvoie `true` si l'utilisateur
    /// a demandé la connexion.
    pub(crate) fn credentials(&mut self, ui: &mut egui::Ui, ready: bool) -> bool {
        // « Enregistré » posé dans le champ : on voit d'un coup d'œil que les
        // identifiants viennent du coffre et pas d'une saisie oubliée.
        let stored = self.active_server().is_some_and(|s| s.secret.is_some());
        ui::field_label(ui, "Pseudo");
        let field = ui.add(ui::champ(&mut self.username, "ton pseudo"));
        if stored {
            let galley = ui.fonts_mut(|f| {
                f.layout_no_wrap(
                    "ENREGISTRÉ".into(),
                    egui::FontId::proportional(texte::MINUSCULE),
                    theme::alpha(ACCENT, 190),
                )
            });
            let rect = field.rect;
            ui.painter().galley(
                egui::pos2(
                    rect.right() - 11.0 - galley.size().x,
                    rect.center().y - galley.size().y / 2.0,
                ),
                galley,
                ACCENT,
            );
        }
        ui.add_space(espace::L);

        ui::field_label(ui, "Mot de passe");
        ui.add(ui::champ_secret(&mut self.lanceur.password, "ton mot de passe"));
        ui.add_space(espace::L);

        // Sans coffre natif, on ne propose pas de mémoriser : pas de repli
        // en clair, la case est simplement inerte.
        let vault = secret::available();
        if !vault {
            self.lanceur.remember_password = false;
        }
        let mut open_invite = false;
        ui.horizontal(|ui| {
            ui.add_enabled_ui(vault, |ui| {
                ui.checkbox(&mut self.lanceur.remember_password, "Retenir le mot de passe")
                    .on_hover_text(if vault {
                        "Chiffré par Windows avec une clé dérivée de ta session : \
                         illisible sur une autre machine ou sous un autre compte, \
                         même si le fichier de configuration est copié."
                    } else {
                        "Indisponible : aucun coffre à secrets sur cette plateforme."
                    });
            });
            // Le code d'invitation ne sert qu'une fois dans la vie d'un
            // compte : il tient sur la même ligne, en retrait.
            if !self.lanceur.show_invite {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    open_invite = ui
                        .add(
                            egui::Label::new(
                                Texte::Libelle.riche_ton(ui, Ton::Info, "Code d'invitation"),
                            )
                            .sense(Sense::click()),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text("Nécessaire uniquement à la première connexion")
                        .clicked();
                });
            }
        });
        if open_invite {
            self.lanceur.show_invite = true;
        }
        if self.lanceur.show_invite {
            ui.add_space(espace::L);
            ui::field_label(ui, "Code d'invitation");
            ui.add(ui::champ(&mut self.lanceur.invite, "fourni par un admin"));
            ui::hint(ui, "Nécessaire uniquement à la première connexion.");
        }

        ui.add_space(espace::XL);

        // Pendant une tentative, le bouton devient une **annulation**.
        //
        // Attendre était la seule option offerte, et l'attente n'était pas
        // bornée : quand le serveur accepte la connexion puis ne répond plus,
        // l'écran restait figé sur « Connexion… » sans rien à cliquer. Le
        // délai de `DELAI_CONNEXION` finit par trancher, mais vingt secondes
        // sans pouvoir revenir en arrière, c'est vingt secondes de trop quand
        // on vient de taper la mauvaise adresse.
        if self.session.connecting {
            let reste = self.session
                .connect_started
                .map(|d| Self::DELAI_CONNEXION.saturating_sub(d.elapsed()).as_secs() + 1)
                .unwrap_or(0);
            let annule = ui
                .add_sized(
                    [ui.available_width(), 38.0],
                    egui::Button::new(
                        RichText::new(format!("Connexion…  ✕  ({reste} s)")).size(texte::CORPS),
                    ),
                )
                .on_hover_text("Annuler la connexion")
                .clicked();
            if annule {
                self.lanceur.session_auto = None;
                self.disconnect(None);
            }
            // Jamais « se connecter » : la tentative est déjà en cours.
            return false;
        }

        // Le bouton nomme sa destination : avec plusieurs serveurs, ça évite
        // de se connecter au mauvais sans s'en rendre compte.
        let label = match self.active_server() {
            Some(server) => format!("Se connecter à {}", ellipsize(server.label(), 26)),
            None => "Se connecter".to_string(),
        };
        ui.add_enabled_ui(ready, |ui| {
            ui::Bouton::new(&label).principal().largeur(ui.available_width()).show(ui)
        })
        .inner
        .clicked()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_addresses_are_tagged() {
        assert_eq!(address_tag("127.0.0.1"), Some("local"));
        assert_eq!(address_tag("127.0.0.1:9987"), Some("local"));
        assert_eq!(address_tag("localhost"), Some("local"));
        assert_eq!(address_tag("192.168.1.30"), Some("réseau local"));
        assert_eq!(address_tag("10.0.0.7:9987"), Some("réseau local"));
        assert_eq!(address_tag("172.20.5.1"), Some("réseau local"));
        // Hors plages privées, et noms publics : pas d'étiquette.
        assert_eq!(address_tag("172.32.5.1"), None);
        assert_eq!(address_tag("8.8.8.8"), None);
        assert_eq!(address_tag("ts.baws.fun"), None);
        assert_eq!(address_tag("kora.chat:7000"), None);
    }

    #[test]
    fn quality_degrades_with_latency() {
        assert_eq!(quality(1).0, 4);
        assert_eq!(quality(29).0, 4);
        assert_eq!(quality(41).0, 3);
        assert_eq!(quality(88).0, 2);
        assert_eq!(quality(210).0, 1);
    }
}
