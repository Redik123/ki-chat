//! Le fil d'un salon : ses messages, leurs images et vidéos, les séparateurs de
//! jour et de nouveaux messages, le menu d'un message.
//!
//! Les fonctions de dessin ne tiennent aucun état : elles reçoivent une ligne à
//! peindre et rendent ce qu'on y a fait ([`MessageAction`]).

use ki_ui::typo::Texte;
use eframe::egui::{self, Color32, RichText, Sense, Vec2};
use ki_core::markup;
use ki_protocol::{ChannelId, ChatRecord, ClientMsg, MsgRef, Reaction, ReplyRef, UserId};
use ki_ui::jetons::{espace, marge, rayon, texte};
use ki_ui::liste::Liste;

use crate::{
    icons, images, medias, porte_ui, prendre_echap, safe_name, theme, ui, visionneuse, KiApp,
    COMPTES_TUS_MAX, PIED_MARGE,
};
use crate::formats::{day_key, day_label, format_time, shorten};
use crate::icons::Icon;
use crate::membres::{empreinte_compte, est_bot, pastille_bot};
use crate::theme::{color_for, ACCENT, TEXT, TEXT_DIM, TEXT_FAINT};
use crate::ui::Ton;

/// Deux messages du même auteur espacés de moins de ça sont regroupés.
pub(crate) const GROUP_WINDOW_MS: u64 = 5 * 60 * 1000;

/// Assainit un message reçu avant affichage : pseudo et texte viennent
/// d'autrui et passent par les mêmes règles que partout ailleurs.
pub(crate) fn clean_record(mut record: ChatRecord) -> ChatRecord {
    record.username = safe_name(&record.username);
    record.text = ki_protocol::safe_display(&record.text, ki_protocol::MAX_CHAT_TEXT);
    record.reply_to = record.reply_to.map(clean_reply);
    record.reactions.retain(|r| ki_protocol::clean_emoji(&r.emoji).is_some());
    record
}

/// Le rappel d'une réponse vient du serveur, qui l'a composé d'après
/// autrui : mêmes règles d'affichage que le reste.
pub(crate) fn clean_reply(mut r: ReplyRef) -> ReplyRef {
    r.username = safe_name(&r.username);
    r.excerpt = ki_protocol::safe_display(&r.excerpt, ki_protocol::MAX_EXCERPT + 1);
    r
}

/// Applique chez nous ce que le serveur annonce : une réaction posée ou
/// retirée par quelqu'un. Même règle que le serveur, pour afficher la même
/// chose que lui.
pub(crate) fn reaction_locale(reactions: &mut Vec<Reaction>, emoji: &str, by: UserId, on: bool) {
    match reactions.iter_mut().position(|r| r.emoji == emoji) {
        Some(i) => {
            let r = &mut reactions[i];
            if on {
                if !r.users.contains(&by) {
                    r.users.push(by);
                }
            } else {
                r.users.retain(|u| *u != by);
                if r.users.is_empty() {
                    reactions.remove(i);
                }
            }
        }
        None if on => reactions.push(Reaction { emoji: emoji.to_string(), users: vec![by] }),
        None => {}
    }
}

/// Le menu d'un message, ouvert d'un clic droit.
#[derive(Clone)]
pub(crate) struct MenuMessage {
    pub(crate) message: MsgRef,
    pub(crate) pos: egui::Pos2,
    /// « Supprimer » demande un second clic : le premier arme, le second fait.
    pub(crate) confirmer: bool,
}

pub(crate) enum MessageAction {
    Rien,
    /// Clic droit : ouvrir le menu à cette position.
    Menu(egui::Pos2),
    /// Clic sur une pastille : poser (`true`) ou retirer sa réaction.
    Reagir(String, bool),
    /// Clic sur le rappel d'une réponse : aller au message d'origine.
    Aller(u64),
    /// Clic sur une image ou une vidéo : l'ouvrir dans la visionneuse.
    Ouvrir(visionneuse::Cible),
}

/// Séparateur de journée dans la conversation.
pub(crate) fn day_separator(ui: &mut egui::Ui, label: &str) {
    ui.add_space(espace::XL);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 16.0), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let galley = ui.fonts_mut(|f| {
        f.layout_no_wrap(
            label.to_owned(),
            egui::FontId::proportional(texte::PETIT),
            TEXT_FAINT,
        )
    });
    let painter = ui.painter();
    let half = galley.size().x / 2.0;
    let y = rect.center().y;
    let stroke = egui::Stroke::new(1.0_f32, theme::BORDER_SOFT);
    painter.line_segment(
        [
            egui::pos2(rect.left() + 16.0, y),
            egui::pos2(rect.center().x - half - 10.0, y),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(rect.center().x + half + 10.0, y),
            egui::pos2(rect.right() - 16.0, y),
        ],
        stroke,
    );
    painter.galley(
        egui::pos2(rect.center().x - half, y - galley.size().y / 2.0),
        galley,
        TEXT_FAINT,
    );
    ui.add_space(espace::S);
}

/// « — nouveaux messages — » : le même dessin qu'un changement de jour, à
/// l'accent — c'est le seul filet du fil qu'on cherche des yeux.
/// Regarde-t-on vraiment `channel` ? C'est ce qui tient un salon pour lu :
/// le salon ouvert, fenêtre au premier plan, fil collé en bas — la règle
/// même qui décide qu'un message reçu est « vu ». Fenêtre à l'arrière-plan
/// ou fil remonté dans le passé, rien n'est lu, quoi qu'on ait programmé.
pub(crate) fn lecture_effective(
    window_focused: bool,
    fil_en_bas: bool,
    current: Option<ChannelId>,
    channel: ChannelId,
) -> bool {
    window_focused && fil_en_bas && current == Some(channel)
}

/// Ce qui identifie une ligne du fil d'une image à l'autre — même quand une
/// page plus ancienne s'ajoute au-dessus.
#[derive(Hash, Debug)]
pub(crate) enum CleFil {
    EnTete,
    Message(UserId, u64),
    Pied,
}

pub(crate) fn separateur_nouveaux(ui: &mut egui::Ui) {
    ui.add_space(espace::L);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 16.0), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let galley = ui.fonts_mut(|f| {
        f.layout_no_wrap("nouveaux messages".to_owned(), egui::FontId::proportional(texte::PETIT), ACCENT)
    });
    let painter = ui.painter();
    let half = galley.size().x / 2.0;
    let y = rect.center().y;
    let stroke = egui::Stroke::new(1.0_f32, theme::alpha(ACCENT, 140));
    painter.line_segment(
        [egui::pos2(rect.left() + 16.0, y), egui::pos2(rect.center().x - half - 10.0, y)],
        stroke,
    );
    painter.line_segment(
        [egui::pos2(rect.center().x + half + 10.0, y), egui::pos2(rect.right() - 16.0, y)],
        stroke,
    );
    painter.galley(egui::pos2(rect.center().x - half, y - galley.size().y / 2.0), galley, ACCENT);
    ui.add_space(espace::XS);
}

/// Un message : en-tête (avatar + pseudo + heure) si c'est le premier du
/// groupe, puis le corps. Les messages consécutifs du même auteur se
/// collent, comme sur Discord.
/// Ce qu'il faut pour peindre un message, en plus du message lui-même.
///
/// Groupé plutôt qu'énuméré : à sept paramètres positionnels, on ne sait plus
/// lequel est lequel — et il en faut deux de plus depuis les mentions.
pub(crate) struct MessageRow<'a> {
    pub(crate) msg: &'a ChatRecord,
    pub(crate) with_header: bool,
    pub(crate) photo: Option<&'a egui::TextureHandle>,
    /// Couleur de l'auteur, résolue depuis son rôle par l'appelant. Elle
    /// n'est pas figée dans l'historique : changer un rôle doit recolorer les
    /// anciens messages, pas seulement les nouveaux.
    pub(crate) color: egui::Color32,
    /// Pseudos connus, pour reconnaître les mentions. Sans eux, `@` n'importe
    /// quoi passerait pour une mention.
    pub(crate) membres: &'a [&'a str],
    /// Le pseudo de celui qui lit, pour distinguer sa propre mention.
    pub(crate) moi: Option<&'a str>,
    /// Son identifiant, pour reconnaître ses propres réactions.
    pub(crate) moi_id: Option<UserId>,
}

pub(crate) fn message_block(
    ui: &mut egui::Ui,
    row: MessageRow<'_>,
    previews: &mut images::Previews,
) -> MessageAction {
    let MessageRow { msg, with_header, photo, color, membres, moi, moi_id } = row;
    const GUTTER: f32 = 36.0;
    let bg_slot = ui.painter().add(egui::Shape::Noop);
    let mut action = MessageAction::Rien;

    let inner = ui.vertical(|ui| {
        ui.add_space(if with_header { 7.0 } else { 1.0 });
        ui.horizontal_top(|ui| {
            ui.add_space(espace::XL);
            if with_header {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(GUTTER), Sense::hover());
                ui::paint_avatar(ui.painter(), rect, &msg.username, false, photo, theme::BG_BASE);
            } else {
                ui.allocate_exact_size(Vec2::new(GUTTER, 1.0), Sense::hover());
            }
            ui.add_space(espace::L);
            ui.vertical(|ui| {
                if with_header {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = espace::M;
                        ui.label(
                            Texte::Corps.gras(ui, &msg.username).color(color),
                        );
                        if est_bot(msg.user_id) {
                            pastille_bot(ui).on_hover_text(
                                "le serveur lui-même : il poste le fil de jeu, il n'a pas de compte",
                            );
                        } else if ki_protocol::est_invite(msg.user_id) {
                            porte_ui::pastille_invite(ui);
                        }
                        ui.label(
                            Texte::Aide.riche(ui, format_time(msg.ts)),
                        );
                    });
                    ui.add_space(1.0);
                }
                // En réponse à… : le rappel de l'original, cliquable pour y
                // aller.
                if let Some(r) = &msg.reply_to {
                    let rappel = ki_ui::emoji::label_avec(
                        ui,
                        Texte::Aide.riche_ton(ui, Ton::Discret, format!("↩ {} — {}", r.username, r.excerpt)),
                        |l| l.sense(Sense::click()).truncate(),
                    );
                    if rappel
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text("aller au message d'origine")
                        .clicked()
                    {
                        action = MessageAction::Aller(r.ts);
                    }
                }
                message_body(ui, &msg.text, membres, moi, (msg.user_id, msg.ts));
                if msg.edited {
                    ui.label(Texte::Minuscule.riche(ui, "(modifié)"));
                }
                // Les images partagées s'affichent sous le message.
                for (is_link, chunk) in split_links(&msg.text) {
                    if !is_link {
                        continue;
                    }
                    if images::looks_like_image(chunk) {
                        if image_preview(ui, chunk, previews) {
                            action =
                                MessageAction::Ouvrir(visionneuse::Cible::Image(chunk.to_string()));
                        }
                    } else if medias::looks_like_video(chunk) && video_card(ui, chunk, previews) {
                        action = MessageAction::Ouvrir(visionneuse::Cible::Video(chunk.to_string()));
                    }
                }
                // Les réactions, en pastilles : la sienne se distingue, et
                // un clic la pose ou la retire.
                if !msg.reactions.is_empty() {
                    ui.add_space(espace::XS);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = espace::XS;
                        for r in &msg.reactions {
                            let mienne = moi_id.is_some_and(|me| r.users.contains(&me));
                            let encre = if mienne { ACCENT } else { TEXT_DIM };
                            let pastille = match ki_ui::emoji::image(ui.ctx(), &r.emoji) {
                                Some(image) => egui::Button::image_and_text(
                                    image.fit_to_exact_size(Vec2::splat(16.0)),
                                    RichText::new(r.users.len().to_string()).size(texte::COURANT).color(encre),
                                ),
                                None => egui::Button::new(
                                    RichText::new(format!("{} {}", r.emoji, r.users.len()))
                                        .size(texte::COURANT)
                                        .color(encre),
                                ),
                            };
                            let pastille = pastille
                                .corner_radius(egui::CornerRadius::same(rayon::XL))
                                .fill(if mienne { theme::alpha(ACCENT, 40) } else { theme::BG_RAISED })
                                .stroke(egui::Stroke::new(
                                    1.0_f32,
                                    if mienne { theme::alpha(ACCENT, 160) } else { theme::BORDER },
                                ));
                            if ui.add(pastille).clicked() {
                                action = MessageAction::Reagir(r.emoji.clone(), !mienne);
                            }
                        }
                    });
                }
            });
        });
        ui.add_space(espace::XXS);
    });

    let row = egui::Rect::from_min_max(
        egui::pos2(ui.max_rect().left() + 6.0, inner.response.rect.top()),
        egui::pos2(ui.max_rect().right() - 6.0, inner.response.rect.bottom()),
    );
    if ui.rect_contains_pointer(row) {
        ui.painter().set(
            bg_slot,
            egui::epaint::RectShape::filled(row, egui::CornerRadius::same(rayon::L), theme::BG_GHOST),
        );
        // Clic droit n'importe où sur le message : son menu. Détecté à la
        // main plutôt que par un widget englobant, qui volerait les clics
        // des liens et des pastilles qu'il recouvre.
        if ui.input(|i| i.pointer.secondary_clicked()) {
            if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                action = MessageAction::Menu(pos);
            }
        }
    }
    action
}

/// Corps d'un message : texte simple, ou texte + liens cliquables.
/// Vignette d'une image partagée, cliquable pour l'ouvrir dans la
/// visionneuse. Rend vrai au clic.
pub(crate) fn image_preview(ui: &mut egui::Ui, url: &str, previews: &mut images::Previews) -> bool {
    const MAX_W: f32 = 420.0;
    const MAX_H: f32 = 320.0;

    let Some(state) = previews.get(ui.ctx(), url) else { return false };
    ui.add_space(espace::S);
    match state {
        images::Preview::Loading => {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(180.0, 24.0), Sense::hover());
            let center = egui::pos2(rect.left() + 9.0, rect.center().y);
            ui::spinner(ui.painter(), center, 6.0, ui.input(|i| i.time), TEXT_FAINT);
            ui.painter().text(
                egui::pos2(rect.left() + 24.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                "chargement de l'image…",
                egui::FontId::proportional(texte::COURANT),
                TEXT_FAINT,
            );
            false
        }
        images::Preview::Failed => {
            ui::hint(ui, "image illisible");
            false
        }
        images::Preview::Ready(texture) => vignette(ui, &texture, MAX_W, MAX_H, "Ouvrir l'image"),
        images::Preview::Anime(animation) => {
            // Une image animée : on repeint au rythme de ses images.
            let texture = animation.image_a(ui.input(|i| i.time)).clone();
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(40));
            vignette(ui, &texture, MAX_W, MAX_H, "Ouvrir l'image")
        }
    }
}

/// Une texture dans un cadre borné, aux bonnes proportions, cliquable.
pub(crate) fn vignette(ui: &mut egui::Ui, texture: &egui::TextureHandle, max_w: f32, max_h: f32, bulle: &str) -> bool {
    // On respecte les proportions, sans jamais dépasser le cadre.
    let source = texture.size_vec2();
    let scale = (max_w / source.x).min(max_h / source.y).min(1.0);
    let size = source * scale;
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        ui.painter().image(texture.id(), rect, uv, Color32::WHITE);
        if response.hovered() {
            ui.painter().rect_stroke(
                rect,
                egui::CornerRadius::same(rayon::M),
                egui::Stroke::new(1.0_f32, theme::alpha(ACCENT, 160)),
                egui::StrokeKind::Inside,
            );
        }
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(bulle).clicked()
}

/// La carte d'une vidéo partagée : son poster, sa durée, un bouton de
/// lecture — et « en préparation » tant que le serveur y travaille. Rend
/// vrai au clic.
pub(crate) fn video_card(ui: &mut egui::Ui, url: &str, previews: &mut images::Previews) -> bool {
    const MAX_W: f32 = 420.0;
    const MAX_H: f32 = 240.0;

    let Some(etat) = previews.meta(ui.ctx(), url) else { return false };
    ui.add_space(espace::S);
    let (poster, duree_s, dims): (Option<String>, f32, (u32, u32)) = match etat {
        images::EtatMeta::Chargement | images::EtatMeta::EnPreparation => {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(220.0, 24.0), Sense::hover());
            let center = egui::pos2(rect.left() + 9.0, rect.center().y);
            ui::spinner(ui.painter(), center, 6.0, ui.input(|i| i.time), TEXT_FAINT);
            let texte = if matches!(etat, images::EtatMeta::Chargement) {
                "vidéo…"
            } else {
                "vidéo en préparation sur le serveur…"
            };
            ui.painter().text(
                egui::pos2(rect.left() + 24.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                texte,
                egui::FontId::proportional(texte::COURANT),
                TEXT_FAINT,
            );
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
            return false;
        }
        images::EtatMeta::Erreur(message) => {
            ui::hint(ui, &message);
            return false;
        }
        // Pas de fiche : un partage d'avant la visionneuse. Le fichier se
        // lit quand même, s'il est dans un format que le décodeur connaît.
        images::EtatMeta::Absente => (None, 0.0, (16, 9)),
        images::EtatMeta::Prete(meta) => {
            let dims = if meta.largeur > 0 && meta.hauteur > 0 { (meta.largeur, meta.hauteur) } else { (16, 9) };
            (meta.poster, meta.duree_s, dims)
        }
    };
    let source = Vec2::new(dims.0 as f32, dims.1 as f32);
    let scale = (MAX_W / source.x).min(MAX_H / source.y);
    let size = source * scale;
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, egui::CornerRadius::same(rayon::M), theme::BG_DEEP);
        let texture = poster.and_then(|p| match previews.chez_nous(ui.ctx(), &p) {
            Some(images::Preview::Ready(t)) => Some(t),
            _ => None,
        });
        if let Some(t) = texture {
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            painter.image(t.id(), rect, uv, Color32::WHITE);
        } else {
            let c = rect.center() - Vec2::new(0.0, 18.0);
            icons::draw(painter, egui::Rect::from_center_size(c, Vec2::splat(28.0)), Icon::Film, TEXT_FAINT);
        }
        // Le bouton de lecture, au centre.
        let c = rect.center();
        let r = 22.0;
        let fond = if response.hovered() { theme::alpha(ACCENT, 230) } else { Color32::from_black_alpha(150) };
        painter.circle_filled(c, r, fond);
        painter.circle_stroke(c, r, egui::Stroke::new(1.0_f32, theme::alpha(TEXT, 120)));
        icons::draw(painter, egui::Rect::from_center_size(c + Vec2::new(2.0, 0.0), Vec2::splat(22.0)), Icon::Play, TEXT);
        if duree_s > 0.0 {
            let texte = visionneuse::mmss((duree_s * 1000.0) as u64);
            let galley = painter.layout_no_wrap(texte, egui::FontId::proportional(texte::PETIT), TEXT);
            let pos = egui::pos2(rect.right() - 8.0 - galley.size().x, rect.bottom() - 8.0 - galley.size().y);
            let fond = egui::Rect::from_min_size(pos, galley.size()).expand2(Vec2::new(5.0, 2.0));
            painter.rect_filled(fond, egui::CornerRadius::same(rayon::S), Color32::from_black_alpha(170));
            painter.galley(pos, galley, TEXT);
        }
        if response.hovered() {
            painter.rect_stroke(
                rect,
                egui::CornerRadius::same(rayon::M),
                egui::Stroke::new(1.0_f32, theme::alpha(ACCENT, 160)),
                egui::StrokeKind::Inside,
            );
        }
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Lire la vidéo").clicked()
}

/// Peint le corps d'un message : liens, mentions, gras, italique, code.
///
/// Le découpage vit dans `markup`, qui se teste sans ouvrir de fenêtre ; ici,
/// on ne fait que peindre ce qu'il rend.
///
/// `membres` sert à reconnaître les mentions — `@` suivi d'un pseudo **connu**
/// — et `moi` à distinguer la sienne. Sans la liste, une adresse électronique
/// écrite dans un message deviendrait un surlignage.
pub(crate) fn message_body(
    ui: &mut egui::Ui,
    text: &str,
    membres: &[&str],
    moi: Option<&str>,
    // De quoi identifier le message d'une image a l'autre. Un bloc de code
    // defile horizontalement, et egui range ce defilement sous une cle : si
    // elle change a chaque image, le bloc revient au debut des qu'on scrute
    // autre chose. L'auteur et l'horodatage, eux, ne bougent pas.
    cle: (UserId, u64),
) {
    let blocs = markup::decouper(text, membres, moi);

    // Le cas de loin le plus fréquent : une ligne, aucun balisage. On évite
    // alors toute la machinerie de mise en page horizontale, qui coûte un
    // widget par fragment.
    if let [markup::Bloc::Ligne(frags)] = blocs.as_slice() {
        if let [markup::Fragment::Texte(t)] = frags.as_slice() {
            ki_ui::emoji::label(ui, Texte::Corps.riche(ui, *t));
            return;
        }
    }

    ui.scope(|ui| {
        // Espacement nul : les espaces font partie des fragments de texte,
        // le retour à la ligne tombe donc au bon endroit.
        ui.spacing_mut().item_spacing.x = 0.0;
        for (i, bloc) in blocs.iter().enumerate() {
            match bloc {
                markup::Bloc::Code(code) => bloc_de_code(ui, code, (cle, i)),
                markup::Bloc::Ligne(frags) => {
                    ui.horizontal_wrapped(|ui| {
                        for frag in frags {
                            peindre_fragment(ui, frag);
                        }
                    });
                }
            }
        }
    });
}

pub(crate) fn peindre_fragment(ui: &mut egui::Ui, frag: &markup::Fragment<'_>) {
    use markup::Fragment as F;
    match frag {
        F::Texte(t) => {
            ki_ui::emoji::label(ui, Texte::Corps.riche(ui, *t));
        }
        F::Lien(url) => {
            ui.hyperlink_to(RichText::new(shorten(url)).size(texte::CORPS), *url)
                .on_hover_text(*url);
        }
        F::Gras(t) => {
            ki_ui::emoji::label(ui, Texte::Corps.gras(ui, *t));
        }
        F::Italique(t) => {
            ki_ui::emoji::label(ui, Texte::Corps.riche(ui, *t).italics());
        }
        F::Code(t) => {
            // Fond discret : ce qui distingue du code d'une phrase, c'est
            // la chasse fixe et un léger relief, pas une couleur criarde.
            ui.label(
                Texte::Corps.riche(ui, *t).monospace().background_color(theme::BG_RAISED),
            );
        }
        F::Mention { pseudo, moi } => {
            // Sa propre mention attire l'œil ; celle de quelqu'un d'autre
            // se contente d'être reconnaissable. Sans cette distinction, un
            // salon actif devient un sapin de Noël et l'on cesse de voir
            // celles qui nous concernent.
            let (couleur, fond) = if *moi {
                (theme::BG_DEEP, ACCENT)
            } else {
                (ACCENT, theme::BG_RAISED)
            };
            ui.label(
                RichText::new(format!("@{pseudo}"))
                    .size(texte::CORPS)
                    .color(couleur)
                    .background_color(fond),
            );
        }
    }
}

/// Un bloc de code : chasse fixe, fond propre, et **pas** de retour à la
/// ligne automatique — un extrait de code coupé au milieu ne se lit plus. Il
/// défile horizontalement dans son cadre.
pub(crate) fn bloc_de_code(ui: &mut egui::Ui, code: &str, cle: ((UserId, u64), usize)) {
    ui.add_space(espace::XS);
    egui::Frame::NONE
        .fill(theme::BG_RAISED)
        .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
        .corner_radius(egui::CornerRadius::same(rayon::M))
        .inner_margin(marge::symetrique(espace::M, espace::S))
        .show(ui, |ui| {
            egui::ScrollArea::horizontal()
                .id_salt(("bloc-code", cle))
                .max_height(260.0)
                .show(ui, |ui| {
                    ui.label(Texte::Corps.riche(ui, code).monospace());
                });
        });
    ui.add_space(espace::XS);
}

/// Salon vide : on n'affiche pas une page blanche.
pub(crate) fn empty_state(ui: &mut egui::Ui, channel: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.32).max(40.0));
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(48.0), Sense::hover());
        icons::draw(ui.painter(), rect, Icon::Chat, theme::BORDER);
        ui.add_space(espace::L);
        ui.label(
            Texte::Corps.riche_ton(ui, Ton::Discret, format!("Personne n'a encore parlé dans #{channel}")),
        );
        ui.add_space(espace::XS);
        ui.label(
            Texte::Libelle.riche_ton(ui, Ton::Pale, "Lance la conversation."),
        );
    });
}

/// Découpe un message en fragments `(est_un_lien, texte)`. Les espaces
/// restent dans les fragments de texte pour que le retour à la ligne
/// automatique se fasse au bon endroit.
pub(crate) fn split_links(text: &str) -> Vec<(bool, &str)> {
    let mut parts = Vec::new();
    let (mut cursor, mut start) = (0usize, 0usize);
    while cursor < text.len() {
        let is_url = text.is_char_boundary(cursor)
            && (text[cursor..].starts_with("http://") || text[cursor..].starts_with("https://"));
        if is_url {
            if cursor > start {
                parts.push((false, &text[start..cursor]));
            }
            let end = text[cursor..]
                .find(char::is_whitespace)
                .map_or(text.len(), |offset| cursor + offset);
            parts.push((true, &text[cursor..end]));
            cursor = end;
            start = end;
        } else {
            cursor += 1;
        }
    }
    if start < text.len() || parts.is_empty() {
        parts.push((false, &text[start..]));
    }
    parts
}

impl KiApp {
    /// Zone centrale : en-tête du salon, conversation, saisie.
    pub(crate) fn chat_panel(&mut self, ui: &mut egui::Ui) {
        let ctx = &ui.ctx().clone();
        let channel_name = self.session
            .current
            .and_then(|id| self.session.channels.iter().find(|c| c.id == id))
            .map(|c| c.name.clone())
            .unwrap_or_default();

        // Un fichier lâché sur la fenêtre part dans le salon, par le même
        // chemin que le trombone ; en survol, la conversation le dit.
        let laches: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        if !laches.is_empty() {
            if self.can(ki_protocol::perm::UPLOAD_FILE) {
                self.televerser(laches);
            } else {
                self.info = Some("tu n'as pas le droit de partager des fichiers".into());
            }
        }
        let survol = ctx.input(|i| !i.raw.hovered_files.is_empty());
        // Ce que les bannières de porte demandent : appliqué une fois
        // l'en-tête rendu, hors de sa fermeture.
        let mut actions_porte: Vec<porte_ui::Action> = Vec::new();

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::BG_BASE))
            .show(ui, |ui| {
                // --- En-tête ---
                egui::Panel::top("chat_head")
                    .frame(
                        egui::Frame::NONE
                            .fill(theme::BG_BASE)
                            .inner_margin(marge::symetrique(espace::XL, espace::L)),
                    )
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui::glyph(ui, Icon::Hash, 17.0, TEXT_FAINT);
                            ui.label(Texte::TitreSection.riche(ui, &channel_name));
                            // Plus de compteur ici : un salon textuel n'a pas
                            // de membres, et la colonne de droite dit déjà qui
                            // est connecté au serveur.
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                self.recherche_en_tete(ui);
                            });
                        });

                        let status = self.upload_status.lock().unwrap().clone();
                        if let Some(status) = status {
                            ui.add_space(espace::M);
                            ui::bandeau(ui, Ton::Info, &status);
                        }
                        if let Some(err) = self.error.clone() {
                            ui.add_space(espace::M);
                            if ui::bandeau_fermable(ui, Ton::Alerte, &err) {
                                self.error = None;
                            }
                        }
                        if let Some(nom) = self.poke_en_cours() {
                            ui.add_space(espace::M);
                            if ui::bandeau_fermable(ui, Ton::Accent, &format!("{nom} te poke")) {
                                self.poke_recu = None;
                            }
                        }
                        // « Kevin veut rejoindre par le web » — une par
                        // demande, empilées, avec Accepter / Refuser.
                        actions_porte.extend(self.portes.bannieres(ui));
                    });
                self.appliquer_portes(std::mem::take(&mut actions_porte));

                // --- Musique : la bannière, quand le bot est quelque part — ou
                // quand on peut le faire venir : en vocal, avec la permission.
                let en_vocal = self.session
                    .my_id
                    .and_then(|me| self.session.members.iter().find(|m| m.user_id == me))
                    .is_some_and(|m| m.voice.is_some());
                let peut_appeler = self.musique.etat.disponible && en_vocal && self.can(ki_protocol::perm::CONTROL_MUSIC);
                if self.musique.etat.salon.is_some() || peut_appeler {
                    egui::Panel::top("chat_musique")
                        .frame(
                            egui::Frame::NONE
                                .fill(theme::BG_RAISED)
                                .inner_margin(marge::symetrique(espace::XL, espace::M)),
                        )
                        .show(ui, |ui| self.bandeau_musique(ui));
                }

                // --- Cartes : une table dans mon salon, fenêtre fermée ---
                let table_cartes = if self.cartes.ouvert {
                    None
                } else {
                    self.cartes.table().map(|t| (t.phase, t.joueurs.len()))
                };
                if let Some((phase, assis)) = table_cartes {
                    let mon_tour = self.cartes.mon_tour(self.session.my_id);
                    let place_libre = self.cartes.place_libre(self.session.my_id);
                    let au_bot = self.cartes.ma_place_au_bot(self.session.my_id);
                    egui::Panel::top("chat_cartes")
                        .frame(
                            egui::Frame::NONE
                                .fill(theme::BG_RAISED)
                                .inner_margin(marge::symetrique(espace::XL, espace::M)),
                        )
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui::glyph(ui, Icon::Cartes, 16.0, if mon_tour { ACCENT } else { TEXT_DIM });
                                let phrase = if mon_tour {
                                    "À toi de jouer aux cartes !".to_string()
                                } else if au_bot {
                                    "Un bot joue ta main à la table de cartes : reprends ta place quand tu veux".to_string()
                                } else if place_libre {
                                    "Un bot tient une place à la table de cartes de ton salon : prends-la !".to_string()
                                } else {
                                    match phase {
                                        ki_protocol::PhaseCartes::Attente => {
                                            format!("Une table de cartes est ouverte dans ton salon · {assis} assis")
                                        }
                                        ki_protocol::PhaseCartes::EnCours => {
                                            format!("Une manche de cartes est en cours dans ton salon · {assis} joueurs")
                                        }
                                        ki_protocol::PhaseCartes::Finie => {
                                            "La manche de cartes de ton salon est finie".to_string()
                                        }
                                    }
                                };
                                ui.label(
                                    RichText::new(phrase)
                                        .color(if mon_tour { ACCENT } else { TEXT })
                                        .size(texte::COURANT),
                                );
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui::Bouton::new("Voir la table").icone(Icon::Cartes).show(ui).clicked() {
                                        self.cartes.ouvert = true;
                                    }
                                });
                            });
                        });
                }

                // --- Le journal des parties, proposé une fois ---
                if self.serveur_journal
                    && !self.reglages.journal_propose
                    && !self.reglages.journal_parties
                    && self.jeu_envoye.as_ref().is_some_and(|j| j.est_valorant())
                {
                    egui::Panel::top("chat_journal")
                        .frame(
                            egui::Frame::NONE
                                .fill(theme::BG_RAISED)
                                .inner_margin(marge::symetrique(espace::XL, espace::M)),
                        )
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui::glyph(ui, Icon::Target, 16.0, theme::INFO);
                                ui.add(
                                    egui::Label::new(
                                        Texte::Libelle.riche_ton(ui, Ton::Neutre, "Garder l'historique de tes parties pour tes statistiques ? Durée, mode, carte, compte — rien d'autre",),
                                    )
                                    .truncate(),
                                );
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui::Bouton::new("Non merci").icone(Icon::Close).show(ui).clicked() {
                                        self.reglages.journal_propose = true;
                                    }
                                    if ui::Bouton::new("Oui").icone(Icon::Check).principal().show(ui)
                                        .on_hover_text("ça se décoche quand tu veux : ⚙ → Jeu")
                                        .clicked()
                                    {
                                        self.reglages.journal_parties = true;
                                        self.reglages.journal_propose = true;
                                    }
                                });
                            });
                        });
                }

                // --- Un compte Riot non lié ---
                // Pour soi seul : on joue sur un compte que ki-chat ne
                // connaît pas. Sans compte lié, on propose de le lier ;
                // sinon on le dit, une fois par compte si l'on veut.
                if let Some(session) = self.compte_non_lie() {
                    let rien_de_lie = self.mes_comptes.is_empty();
                    egui::Panel::top("chat_compte_riot")
                        .frame(
                            egui::Frame::NONE
                                .fill(theme::BG_RAISED)
                                .inner_margin(marge::symetrique(espace::XL, espace::M)),
                        )
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui::glyph(ui, Icon::Target, 16.0, theme::WARN);
                                let phrase = if rien_de_lie {
                                    format!("Tu joues sur {} : le lier à ki-chat pour avoir ta fiche VALORANT ?", session.riot_id)
                                } else {
                                    format!(
                                        "Tu joues sur {}, qui n'est pas lié à ki-chat : ses parties ne vont pas dans ta fiche",
                                        session.riot_id
                                    )
                                };
                                ui.add(egui::Label::new(Texte::Libelle.riche_ton(ui, Ton::Neutre, phrase)).truncate());
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let taire = if rien_de_lie { "Non merci" } else { "Ne plus me le dire" };
                                    if ui::Bouton::new(taire).icone(Icon::Close).show(ui)
                                        .on_hover_text("ki-chat ne t'en reparlera plus pour ce compte, sur cet ordinateur")
                                        .clicked()
                                    {
                                        self.reglages.comptes_tus.push(empreinte_compte(&session.puuid));
                                        // Les plus anciens s'oublient : la liste reste courte.
                                        let trop = self.reglages.comptes_tus.len().saturating_sub(COMPTES_TUS_MAX);
                                        self.reglages.comptes_tus.drain(..trop);
                                    }
                                    if rien_de_lie
                                        && ui::Bouton::new("Lier ce compte").icone(Icon::Check).principal().show(ui)
                                            .on_hover_text("le serveur le retrouve chez HenrikDev et ouvre ta fiche")
                                            .clicked()
                                    {
                                        self.send(ClientMsg::LierRiot { riot_id: session.riot_id.clone() });
                                    }
                                });
                            });
                        });
                }

                // --- Saisie ---
                egui::Panel::bottom("chat_input")
                    .frame(
                        egui::Frame::NONE
                            .fill(theme::BG_BASE)
                            .inner_margin(egui::Margin {
                                left: 16,
                                right: 16,
                                top: PIED_MARGE as i8,
                                bottom: PIED_MARGE as i8,
                            }),
                    )
                    .show(ui, |ui| self.chat_input(ui, &channel_name));

                // --- Conversation ---
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(theme::BG_BASE))
                    .show(ui, |ui| self.chat_log(ui, &channel_name));
            });

        if survol {
            let rect = ctx.content_rect();
            egui::Area::new(egui::Id::unique("depot-fichier"))
                .order(egui::Order::Foreground)
                .fixed_pos(rect.min)
                .show(ctx, |ui| {
                    let p = ui.painter();
                    p.rect_filled(rect, 0.0, theme::alpha(theme::BG_DEEP, 200));
                    p.rect_stroke(
                        rect.shrink(18.0),
                        14.0,
                        egui::Stroke::new(2.0_f32, ACCENT),
                        egui::StrokeKind::Inside,
                    );
                    p.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        format!("Lâche pour envoyer dans #{channel_name}"),
                        egui::FontId::proportional(texte::GRAND),
                        TEXT,
                    );
                    ui.allocate_rect(rect, Sense::hover());
                });
        }
    }

    pub(crate) fn chat_log(&mut self, ui: &mut egui::Ui, channel_name: &str) {
        if self.session.messages.is_empty() {
            empty_state(ui, channel_name);
            return;
        }
        let mut want_older = false;
        let mut revenir = false;
        self.perf.debut_fil();

        // Les pseudos connus, empruntés le temps du rendu comme les
        // messages : c'est ce qui permet de reconnaître une mention
        // sans allouer une chaîne par membre et par image.
        let membres_source = std::mem::take(&mut self.session.members);
        let membres: Vec<&str> = membres_source.iter().map(|m| m.username.as_str()).collect();
        let moi = self.session
            .my_id
            .and_then(|id| membres_source.iter().find(|m| m.user_id == id))
            .map(|m| m.username.clone());
        let moi_id = self.session.my_id;
        // Ce qu'un clic sur un message a demandé : exécuté après le
        // rendu, quand les messages sont revenus dans l'état.
        let mut demande: Option<(MsgRef, MessageAction)> = None;

        let messages = std::mem::take(&mut self.session.messages);
        let n = messages.len();
        // « Nouveaux messages » : avant le premier message postérieur au
        // repère. Posé à l'entrée du salon, il reste jusqu'au prochain
        // changement de salon — on veut le retrouver en remontant.
        let nouveaux = self.separateur_nouveaux.and_then(|depuis| messages.iter().position(|m| m.ts > depuis));
        let (history_pending, history_more) = (self.session.history_pending, self.session.history_more);
        // On a sauté au milieu du passé : ce qui a suivi n'est pas affiché.
        let retour = self.recherche.retour_present == self.session.current;
        let previews = &mut self.previews;
        let avatars = &self.avatars;
        let author_colors = &self.author_colors;

        // Le fil : un en-tête, les messages, un pied. Seul ce qui est à
        // l'écran est construit — on construisait jusqu'à cinq cents blocs
        // à chaque image pour en montrer une vingtaine —, et la ligne qu'on
        // lit ne bouge pas d'un pixel quand une page plus ancienne s'ajoute
        // au-dessus.
        let sortie = Liste::new("fil")
            .coller_en_bas(true)
            .aller_en_bas(std::mem::take(&mut self.fil_au_bas))
            .hauteur_estimee(48.0)
            .show(
                ui,
                n + 2,
                |i| match i {
                    0 => CleFil::EnTete,
                    i if i > n => CleFil::Pied,
                    i => CleFil::Message(messages[i - 1].user_id, messages[i - 1].ts),
                },
                |ui, i| {
                    if i == 0 {
                        // En-tête du fil : c'est là que se remonte le passé.
                        // Un bouton explicite en plus du chargement au
                        // défilement — le défilement seul laisse croire qu'on
                        // est au début alors qu'il reste des mois de
                        // conversation.
                        if history_pending {
                            ui.vertical_centered(|ui| {
                                ui.add_space(espace::S);
                                ui.label(
                                    Texte::Aide.riche(ui, "Chargement des messages plus anciens…"),
                                );
                                ui.add_space(espace::S);
                            });
                        } else if history_more {
                            ui.vertical_centered(|ui| {
                                ui.add_space(espace::S);
                                if ui
                                    .button(
                                        Texte::Aide.riche_ton(ui, Ton::Discret, "Charger les messages plus anciens"),
                                    )
                                    .clicked()
                                {
                                    want_older = true;
                                }
                                ui.add_space(espace::S);
                            });
                        } else {
                            day_separator(ui, &format!("Début de #{channel_name}"));
                        }
                        return;
                    }
                    if i > n {
                        // Le pied. Sans « revenir au présent » après un saut
                        // dans le passé, on croirait le salon mort depuis ce
                        // jour-là.
                        if retour {
                            ui.add_space(espace::S);
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    Texte::Aide.riche(ui, "tu regardes un message retrouvé"),
                                );
                                if ui
                                    .button(Texte::Aide.riche_ton(ui, Ton::Discret, "Revenir au présent"))
                                    .clicked()
                                {
                                    revenir = true;
                                }
                            });
                        }
                        ui.add_space(espace::L);
                        return;
                    }

                    let k = i - 1;
                    let msg = &messages[k];
                    let precedent = k.checked_sub(1).map(|j| &messages[j]);
                    let jour_change = precedent.is_none_or(|p| day_key(p.ts) != day_key(msg.ts));
                    let grouped = !jour_change
                        && precedent.is_some_and(|p| {
                            p.user_id == msg.user_id && msg.ts.saturating_sub(p.ts) < GROUP_WINDOW_MS
                        });
                    let photo = avatars.get(&msg.user_id).map(|(_, t)| t.clone());
                    // L'auteur peut avoir quitté le serveur : on retombe
                    // alors sur son pseudo, plutôt que de perdre la couleur.
                    // Le serveur lui-même (le fil de jeu, identifiant 0)
                    // parle en couleur d'accent : ce n'est pas un membre.
                    let color = if msg.user_id == 0 {
                        ACCENT
                    } else if ki_protocol::est_invite(msg.user_id) {
                        // Un invité web, même parti depuis : sa couleur
                        // ne dépend pas du roster.
                        theme::INVITE
                    } else {
                        author_colors.get(&msg.user_id).copied().unwrap_or_else(|| color_for(&msg.username))
                    };

                    if jour_change {
                        day_separator(ui, &day_label(msg.ts));
                    }
                    if nouveaux == Some(k) {
                        separateur_nouveaux(ui);
                    }
                    let action = message_block(
                        ui,
                        MessageRow {
                            msg,
                            with_header: !grouped,
                            photo: photo.as_ref(),
                            color,
                            membres: &membres,
                            moi: moi.as_deref(),
                            moi_id,
                        },
                        previews,
                    );
                    if !matches!(action, MessageAction::Rien) {
                        demande = Some((MsgRef { user_id: msg.user_id, ts: msg.ts }, action));
                    }
                },
            );
        self.session.messages = messages;
        drop(membres);
        self.session.members = membres_source;
        self.perf.fin_fil(sortie.dessines, self.session.messages.len());

        if let Some((message, action)) = demande {
            match action {
                MessageAction::Menu(pos) => {
                    self.menu_message = Some(MenuMessage { message, pos, confirmer: false });
                }
                MessageAction::Reagir(emoji, on) => {
                    self.send(ClientMsg::React { message, emoji, on });
                }
                MessageAction::Aller(ts) => {
                    if let Some(c) = self.session.current {
                        self.sauter_a(c, ts);
                    }
                }
                MessageAction::Ouvrir(cible) => self.ouvrir_visionneuse(cible),
                MessageAction::Rien => {}
            }
        }

        // Collé en bas, à quelques points près : c'est ce qui décide si un
        // message qui arrive est vu, ou s'il pose une pastille.
        let fond = (sortie.hauteur_contenu - sortie.rect.height()).max(0.0);
        let etait_en_bas = self.fil_en_bas;
        self.fil_en_bas = sortie.decalage >= fond - 8.0;
        // Redescendu en bas, fenêtre au premier plan : ce qui est arrivé
        // pendant qu'on relisait le passé est sous les yeux maintenant. Sans
        // ça, la pastille posée sur le salon courant restait jusqu'au message
        // suivant.
        if self.fil_en_bas && !etait_en_bas && self.window_focused {
            if let Some(c) = self.session.current {
                if self.session.non_lus.contains_key(&c) {
                    self.programmer_lu(c);
                }
            }
        }

        // Arrivé tout en haut, on charge sans attendre le clic : c'est le
        // geste naturel pour remonter une conversation. Mais seulement si l'on
        // défile **réellement** : rester en haut ne doit pas suffire, sinon la
        // demande repart à chaque image. Le bouton explicite couvre le cas où
        // l'on est déjà en haut sans rien toucher. Une page arrivée, la liste
        // recale la vue sur la ligne qu'on lisait : on n'est plus en haut, la
        // demande ne s'enchaîne pas.
        let scrolling = ui.input(|i| {
            i.is_scrolling() || i.smooth_scroll_delta.y.abs() > 0.0
        });
        if sortie.decalage <= 24.0 && scrolling {
            want_older = true;
        }
        if want_older {
            self.load_older_history();
        }
        if revenir {
            if let Some(channel) = self.session.current {
                self.join(channel);
            }
        }
    }

    /// Le menu d'un message, ouvert d'un clic droit : réagir, répondre, et
    /// supprimer — le sien, ou celui d'un autre avec la permission. Il se
    /// ferme sur Échap, sur un clic ailleurs, ou une fois la chose faite.
    pub(crate) fn menu_message_ui(&mut self, ctx: &egui::Context) {
        let Some(menu) = self.menu_message.clone() else { return };
        let Some(msg) = self.session
            .messages
            .iter()
            .find(|m| m.user_id == menu.message.user_id && m.ts == menu.message.ts)
            .cloned()
        else {
            self.menu_message = None;
            return;
        };
        let peut_supprimer =
            Some(msg.user_id) == self.session.my_id || self.can(ki_protocol::perm::DELETE_MESSAGES);
        let mut fermer = false;
        let zone = egui::Area::new(egui::Id::unique("menu-message"))
            .order(egui::Order::Foreground)
            .fixed_pos(menu.pos)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(232.0);
                    ui.label(Texte::Corps.gras(ui, &msg.username));
                    ki_ui::emoji::label_tronque(
                        ui,
                        Texte::Aide.riche(ui, ki_protocol::excerpt_of(&msg.text)),
                    );
                    ui.add_space(espace::XS);
                    // Réagir : les emojis du protocole, d'un clic. Celui
                    // qu'on a déjà posé se retire.
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = espace::XS;
                        for emoji in ki_protocol::REACTIONS {
                            let deja = msg
                                .reactions
                                .iter()
                                .any(|r| r.emoji == *emoji && self.session.my_id.is_some_and(|me| r.users.contains(&me)));
                            let bouton = match ki_ui::emoji::image(ui.ctx(), emoji) {
                                Some(image) => egui::Button::image(image.fit_to_exact_size(Vec2::splat(22.0))),
                                None => egui::Button::new(RichText::new(*emoji).size(texte::TITRE)),
                            };
                            let bouton = bouton
                                .fill(if deja { theme::alpha(ACCENT, 40) } else { theme::BG_RAISED })
                                .corner_radius(egui::CornerRadius::same(rayon::M));
                            if ui.add(bouton).clicked() {
                                self.send(ClientMsg::React {
                                    message: menu.message,
                                    emoji: (*emoji).to_string(),
                                    on: !deja,
                                });
                                fermer = true;
                            }
                        }
                    });
                    ui.add_space(espace::XS);
                    if ui.button("⧉ Copier le texte").clicked() {
                        ctx.copy_text(msg.text.clone());
                        fermer = true;
                    }
                    if ui.button("↩ Répondre").clicked() {
                        self.reponse_a = Some(ReplyRef {
                            user_id: msg.user_id,
                            ts: msg.ts,
                            username: msg.username.clone(),
                            excerpt: ki_protocol::excerpt_of(&msg.text),
                        });
                        self.focus_input = true;
                        fermer = true;
                    }
                    // Les siens seulement : le serveur refuse le reste, et
                    // un bouton qui échoue n'apprend rien.
                    if Some(msg.user_id) == self.session.my_id && ui.button("✎ Modifier").clicked() {
                        self.commencer_edition(&msg);
                        fermer = true;
                    }
                    if peut_supprimer {
                        let rouge = theme::DANGER;
                        if menu.confirmer {
                            if ui
                                .button(RichText::new("Confirmer la suppression").color(rouge))
                                .clicked()
                            {
                                self.send(ClientMsg::DeleteMessage { message: menu.message });
                                fermer = true;
                            }
                        } else if ui.button(RichText::new("Supprimer…").color(rouge)).clicked() {
                            if let Some(m) = self.menu_message.as_mut() {
                                m.confirmer = true;
                            }
                        }
                    }
                });
            });
        // Le clic droit qui vient d'ouvrir le menu n'est pas un clic
        // ailleurs : seul un clic gauche hors du menu le ferme.
        let ailleurs = ctx.input(|i| i.pointer.primary_clicked())
            && !zone.response.contains_pointer();
        // Échap est **pris** : sans ça, la même touche fermait le menu et
        // annulait la modification en cours derrière lui.
        if fermer || ailleurs || prendre_echap(ctx) {
            self.menu_message = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_separated_from_the_surrounding_text() {
        let parts = split_links("regarde http://a.fr/x puis dis-moi");
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], (false, "regarde "));
        assert_eq!(parts[1], (true, "http://a.fr/x"));
        assert_eq!(parts[2], (false, " puis dis-moi"));
        // Un message sans lien reste d'un seul bloc.
        assert_eq!(split_links("salut"), vec![(false, "salut")]);
    }
}
