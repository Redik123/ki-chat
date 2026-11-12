//! La colonne des membres : une ligne par personne, son menu au clic droit, le
//! glisser-déposer d'un salon vocal à l'autre, la fiche d'un bot.

use ki_ui::typo::Texte;
use eframe::egui::{self, Color32, RichText, Sense, Vec2};
use ki_protocol::{ChannelId, ChannelKind, ClientMsg, Member, UserId};
use ki_ui::jetons::{espace, marge, rayon, texte};

use crate::{
    graphes, icons, porte_ui, theme, ui, valo_catalogue, KiApp, VoiceSnapshot, DELAI_REFUS_POKE,
    DUREE_POKE, ROSTER_LARGEURS, ROSTER_WIDTH, SPEAK_LEVEL,
};
use crate::admin_ui::BanDraft;
use crate::icons::Icon;
use crate::theme::{ACCENT, DANGER, SPEAK, TEXT_DIM, TEXT_FAINT, WARN};
use crate::ui::Ton;

/// Ce qu'une ligne de membre montre en plus du compte lui-même : l'état
/// « ici et maintenant », qui ne vient pas du roster mais du moteur audio et
/// des réglages locaux. Groupé, parce que huit paramètres positionnels
/// donnaient des appels du genre `(m, false, false, false, 0.0, 1.0)`, où
/// rien ne dit lequel est quoi.
pub(crate) struct MemberRow<'a> {
    pub(crate) member: &'a Member,
    pub(crate) speaking: bool,
    pub(crate) muted: bool,
    pub(crate) is_me: bool,
    pub(crate) photo: Option<&'a egui::TextureHandle>,
    /// L'icône de son rang, si elle est prête.
    pub(crate) rang_icone: Option<&'a egui::TextureHandle>,
    /// La ligne s'attrape à la souris (glisser quelqu'un vers un autre
    /// salon vocal) en plus de se cliquer.
    pub(crate) glissable: bool,
    /// Combien regardent son stream, s'il diffuse (0 : rien à afficher).
    pub(crate) spectateurs: usize,
}

/// Ce que l'on glisse d'un salon vocal à l'autre dans la barre des salons.
pub(crate) struct DeplacementVocal {
    pub(crate) username: String,
    /// Le salon d'où il part : le relâcher là ne fait rien.
    pub(crate) depuis: ChannelId,
    /// C'est soi : le relâcher sur un autre salon, c'est y aller.
    pub(crate) moi: bool,
}

/// Le palier à montrer pour un membre : sa fiche s'il a lié son compte,
/// sinon ce que sa présence dit — s'il la partage et qu'il est là.
pub(crate) fn palier_de(member: &Member) -> Option<u8> {
    let presence = member.jeu.as_ref().filter(|_| member.online).map(|j| j.rang);
    // Sur un autre compte que celui de sa fiche, le rang de la fiche
    // mentirait : celui de la présence est celui du compte joué.
    let fiche = member.rang_valorant.filter(|_| !sur_un_autre_compte(member));
    fiche.or(presence).filter(|t| *t >= 3)
}

/// Joue-t-il sur un autre compte que celui de sa fiche ? Seulement quand
/// le serveur le dit (`Member::compte_joue`).
pub(crate) fn sur_un_autre_compte(member: &Member) -> bool {
    matches!(
        member.compte_joue,
        Some(ki_protocol::CompteJoue::Autre | ki_protocol::CompteJoue::Second { .. } | ki_protocol::CompteJoue::Inconnu)
    )
}

/// Ce qui suit la partie sous le pseudo quand il joue sur un autre compte
/// que celui de sa fiche : son nom s'il l'a rendu visible, sinon rien de
/// plus que le fait.
pub(crate) fn mention_du_compte(member: &Member) -> Option<String> {
    match member.compte_joue.as_ref()? {
        ki_protocol::CompteJoue::Principal => None,
        ki_protocol::CompteJoue::Second { riot_id: Some(r) } => Some(format!("sur {r}")),
        ki_protocol::CompteJoue::Second { riot_id: None } => Some("2ᵉ compte".into()),
        ki_protocol::CompteJoue::Autre | ki_protocol::CompteJoue::Inconnu => Some("autre compte".into()),
    }
}

/// L'empreinte d'un puuid, pour se souvenir d'un compte sans garder son
/// identifiant : FNV-1a sur 64 bits, en hexadécimal, sans la casse.
pub(crate) fn empreinte_compte(puuid: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for o in puuid.to_ascii_lowercase().bytes() {
        h ^= u64::from(o);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

pub(crate) fn est_bot(user_id: UserId) -> bool {
    user_id == 0 || user_id == ki_protocol::MUSIQUE_ID
}

/// Un invité web : le drapeau du serveur, ou sa plage d'identifiants —
/// l'un ou l'autre suffit, un roster en retard n'y change rien.
pub(crate) fn est_invite_membre(m: &Member) -> bool {
    m.invite || ki_protocol::est_invite(m.user_id)
}

/// La couleur d'un membre : celle de son rôle, sinon son pseudo — et
/// celle des invités pour un invité, qui n'a ni rôle ni couleur à lui.
pub(crate) fn couleur_de_membre(m: &Member) -> Color32 {
    if est_invite_membre(m) {
        theme::INVITE
    } else {
        theme::member_color(m.color, &m.username)
    }
}

/// La pastille « BOT », à la couleur d'accent, à côté d'un nom.
pub(crate) fn pastille_bot(ui: &mut egui::Ui) -> egui::Response {
    ui::etiquette(ui, "BOT", ACCENT)
}

/// Ce qu'on glisse d'un salon vocal à l'autre suit la souris : le pseudo,
/// dans une bulle, sur la couche des infobulles — qui ne prend pas la
/// souris : la cible dessous reste survolée.
pub(crate) fn fantome_deplacement(ctx: &egui::Context) {
    let Some(glisse) = egui::DragAndDrop::payload::<DeplacementVocal>(ctx) else { return };
    let Some(pos) = ctx.pointer_hover_pos() else { return };
    egui::Area::new(egui::Id::unique("deplacement-vocal"))
        .order(egui::Order::Tooltip)
        .fixed_pos(pos + Vec2::new(14.0, 10.0))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(Texte::Corps.gras(ui, &glisse.username));
            });
        });
}

pub(crate) fn member_row(ui: &mut egui::Ui, row: MemberRow<'_>) -> (egui::Response, bool) {
    let MemberRow { member, speaking, muted, is_me, photo, rang_icone, glissable, spectateurs } = row;
    let height = 38.0;
    // Un clic reste un clic (menu, fiche) : egui ne parle de glisser
    // qu'une fois la souris partie de quelques pixels.
    let sense = if glissable { Sense::click_and_drag() } else { Sense::click() };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), sense);
    if !ui.is_rect_visible(rect) {
        return (response, false);
    }
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect, egui::CornerRadius::same(rayon::L), theme::BG_HOVER);
    }

    let avatar_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 14.0, rect.center().y - 13.0),
        Vec2::splat(26.0),
    );
    if est_bot(member.user_id) {
        // Un bot n'a pas de photo : un disque à la couleur d'accent, une
        // note dedans — et l'anneau de parole comme les autres.
        painter.circle_filled(avatar_rect.center(), 13.0, theme::alpha(ACCENT, 60));
        if speaking {
            painter.circle_stroke(avatar_rect.center(), 14.5, egui::Stroke::new(2.0_f32, SPEAK));
        }
        painter.text(
            avatar_rect.center(),
            egui::Align2::CENTER_CENTER,
            "♪",
            egui::FontId::proportional(texte::TITRE),
            ACCENT,
        );
    } else {
        ui::paint_avatar(painter, avatar_rect, &member.username, speaking, photo, theme::BG_SIDE);
    }

    // La couleur vient du rôle quand le serveur en donne une ; sinon
    // c'est le hachage du pseudo, comme avant les rôles. Hors ligne, la
    // couleur s'éteint : présent dans la liste, absent de la pièce.
    let color = if member.online {
        couleur_de_membre(member)
    } else {
        couleur_de_membre(member).gamma_multiply(0.45)
    };
    let font = egui::FontId::proportional(texte::CORPS);
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(member.username.clone(), font, color));
    let name_width = galley.size().x;
    let name_left = avatar_rect.right() + 9.0;
    // En jeu : le pseudo monte d'un cran, et la partie se lit dessous —
    // « compétitive · Ascent · 7-5 », ce que son client Riot raconte.
    let jeu = member.jeu.as_ref().filter(|_| member.online);
    let name_y = if jeu.is_some() { rect.center().y - 7.0 } else { rect.center().y };
    painter.galley(
        egui::pos2(name_left, name_y - galley.size().y / 2.0),
        galley,
        color,
    );
    if let Some(j) = jeu {
        let teinte = if j.etat == ki_protocol::JeuEtat::EnJeu { TEXT_DIM } else { TEXT_FAINT };
        painter.text(
            egui::pos2(name_left, rect.center().y + 8.0),
            egui::Align2::LEFT_CENTER,
            match mention_du_compte(member) {
                Some(mention) if j.est_valorant() => format!("{} · {mention}", valo_catalogue::ligne(j)),
                _ => valo_catalogue::ligne(j),
            },
            egui::FontId::proportional(texte::MINUSCULE),
            teinte,
        );
    }

    let mut apres_nom = name_left + name_width + 5.0;
    if est_bot(member.user_id) {
        // La pastille BOT, franche : personne ne le prend pour un membre.
        let pastille = ui::peindre_etiquette(painter, egui::pos2(apres_nom + 1.0, name_y), "BOT", ACCENT);
        apres_nom += pastille.width() + espace::S;
    }
    if est_invite_membre(member) {
        // La pastille INVITÉ, même dessin que BOT, à la couleur des
        // invités : pas un compte, présent le temps d'une porte.
        let pastille =
            ui::peindre_etiquette(painter, egui::pos2(apres_nom + 1.0, name_y), "INVITÉ", theme::INVITE);
        apres_nom += pastille.width() + espace::S;
    }
    if member.admin && !est_bot(member.user_id) {
        let badge = egui::Rect::from_min_size(
            egui::pos2(apres_nom, name_y - 6.5),
            Vec2::splat(13.0),
        );
        icons::draw(painter, badge, Icon::Crown, ACCENT);
        apres_nom += 18.0;
    }
    // Sur l'appli mobile : un petit téléphone, discret — il dit pourquoi
    // quelqu'un répond moins vite, ou n'a pas le partage d'écran.
    if member.mobile && member.online {
        let badge = egui::Rect::from_min_size(egui::pos2(apres_nom, name_y - 6.5), Vec2::splat(13.0));
        icons::draw(painter, badge, Icon::Telephone, TEXT_DIM);
        apres_nom += 16.0;
    }

    // Son rang VALORANT — de sa fiche, ou de sa présence : l'icône du
    // palier quand elle est là, puis « Or 2 » en petit, à la couleur du
    // palier. Non classé ne montre rien.
    if let Some(tier) = palier_de(member).filter(|_| !est_bot(member.user_id)) {
        if let Some(icone) = rang_icone {
            let cadre = egui::Rect::from_min_size(egui::pos2(apres_nom, name_y - 8.0), Vec2::splat(16.0));
            painter.image(icone.id(), cadre, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            apres_nom += 18.0;
        }
        let galley = ui.fonts_mut(|f| {
            f.layout_no_wrap(
                ki_protocol::nom_de_rang(tier),
                egui::FontId::proportional(texte::MINUSCULE),
                graphes::couleur_de_rang(tier),
            )
        });
        let largeur = galley.size().x;
        painter.galley(
            egui::pos2(apres_nom + 1.0, name_y - galley.size().y / 2.0 + 1.0),
            galley,
            graphes::couleur_de_rang(tier),
        );
        apres_nom += largeur + 8.0;
    }

    // Il diffuse son écran : un petit écran à côté du pseudo, cliquable —
    // c'est le chemin le plus court vers son stream, là où l'œil cherche
    // déjà « qui fait quoi ».
    let mut regarder = false;
    if member.streaming.is_some() && member.online {
        let badge = egui::Rect::from_min_size(
            egui::pos2(apres_nom, name_y - 7.0),
            Vec2::splat(14.0),
        );
        // Combien le regardent, à côté de l'écran : ce qu'un streamer veut
        // savoir d'un coup d'œil, et les autres aussi.
        let mut zone = badge;
        if spectateurs > 0 {
            let g = painter.layout_no_wrap(spectateurs.to_string(), egui::FontId::proportional(texte::PETIT), TEXT_DIM);
            let pos = egui::pos2(badge.right() + 3.0, name_y - g.size().y / 2.0);
            zone = zone.union(egui::Rect::from_min_size(pos, g.size()));
            painter.galley(pos, g, TEXT_DIM);
        }
        let hit = ui.interact(zone.expand(3.0), response.id.with("diffuse"), Sense::click());
        let couleur = if hit.hovered() && !is_me { SPEAK } else { ACCENT };
        icons::draw(painter, badge, Icon::Screen, couleur);
        let qui = match spectateurs {
            0 => String::new(),
            1 => " — 1 spectateur".to_string(),
            n => format!(" — {n} spectateurs"),
        };
        let mot = if is_me {
            format!("tu diffuses ton écran{qui}")
        } else {
            format!("diffuse son écran{qui} — clic pour regarder")
        };
        regarder = hit.on_hover_text(mot).clicked() && !is_me;
    }

    // Côté droit : micro coupé, volume personnalisé, vumètre pendant la
    // parole. L'icône « muet » d'abord, la plus à droite : c'est elle qui
    // répond à « il est parti ou il s'est mute ? » d'un coup d'œil.
    let mut right = rect.right() - 10.0;

    // Les sanctions d'abord, en ambre. Même forme que l'état volontaire,
    // couleur différente : c'est ce qui sépare « il s'est tu » de « on l'a
    // fait taire », et les deux peuvent tenir en même temps sur la même
    // personne. Un rouge de plus n'aurait rien distingué du tout.
    for (actif, icone) in [
        (member.force_deafened, Icon::HeadphonesOff),
        (member.force_muted, Icon::MicOff),
    ] {
        if actif {
            let badge = egui::Rect::from_min_size(
                egui::pos2(right - 14.0, rect.center().y - 7.0),
                Vec2::splat(14.0),
            );
            icons::draw(painter, badge, icone, WARN);
            right -= 20.0;
        }
    }

    // Qui parle se voit à l'anneau de l'avatar ; le vumètre et le
    // pourcentage de volume qui s'affichaient ici encombraient la ligne
    // sans rien apprendre — le volume se règle au clic droit.
    if muted {
        let badge = egui::Rect::from_min_size(
            egui::pos2(right - 14.0, rect.center().y - 7.0),
            Vec2::splat(14.0),
        );
        icons::draw(painter, badge, Icon::MicOff, DANGER);
    }
    (response.on_hover_cursor(egui::CursorIcon::PointingHand), regarder)
}

impl KiApp {
    /// Le pouvoir de changer `m` de salon vocal : celui que le serveur
    /// exige (« Déplacer en vocal », et un rang au-dessus du sien), sur
    /// quelqu'un de connecté — ni un bot, ni un invité web, que sa porte
    /// gère à part — et **en vocal**, dans un salon qu'on voit : déplacer
    /// quelqu'un qui n'y est pas le montrait en vocal chez tous, sans voix
    /// ni écoute. Le serveur le refuse désormais.
    pub(crate) fn peut_deplacer(&self, m: &Member) -> bool {
        !est_bot(m.user_id)
            && !est_invite_membre(m)
            && m.online
            && m.voice.is_some_and(|c| self.channels.iter().any(|k| k.id == c))
            && self.outranks(m.rank)
            && self.can(ki_protocol::perm::MOVE_MEMBERS)
    }

    /// Un salon vocal comme cible du glisser-déposer (son intitulé et ses
    /// occupants) : survolé avec quelqu'un en main, il s'entoure ; lâché
    /// dessus, la personne y est déplacée — ou l'on y va, si c'est soi.
    /// Le serveur revérifie tout, et dit pourquoi s'il refuse.
    pub(crate) fn deposer_en_vocal(&mut self, ui: &egui::Ui, bloc: egui::Rect, channel: ChannelId) {
        let Some(glisse) = egui::DragAndDrop::payload::<DeplacementVocal>(ui.ctx()) else { return };
        if glisse.depuis == channel || !ui.rect_contains_pointer(bloc) {
            return;
        }
        ui.painter().rect_stroke(
            bloc,
            egui::CornerRadius::same(rayon::L),
            egui::Stroke::new(1.5_f32, ACCENT),
            egui::StrokeKind::Inside,
        );
        if !ui.input(|i| i.pointer.any_released()) {
            return;
        }
        egui::DragAndDrop::clear_payload(ui.ctx());
        if glisse.moi {
            self.join_voice(channel);
        } else {
            self.send(ClientMsg::AdminVoiceMove { username: glisse.username.clone(), channel: Some(channel) });
        }
    }

    /// Clic sur soi-même : son compte. Clic droit sur un autre : volume et
    /// modération. Le même comportement dans les deux listes.
    pub(crate) fn member_menu(&mut self, response: egui::Response, m: &Member, is_me: bool) {
        if is_me {
            // Sa propre sanction se dit ici aussi. Être réduit au silence
            // sans savoir pourquoi est la pire version de cette
            // fonctionnalité : on croit son micro cassé et l'on va fouiller
            // les réglages audio.
            let quoi = match (m.force_muted, m.force_deafened) {
                (true, true) => "un modérateur t'a coupé le micro et rendu sourd",
                (true, false) => "un modérateur t'a coupé le micro",
                (false, true) => "un modérateur t'a rendu sourd",
                (false, false) => "gérer mon compte",
            };
            if response.on_hover_text(quoi).clicked() {
                self.show_account = true;
            }
            return;
        }
        // Une sanction se dit en toutes lettres au survol : l'ambre du
        // badge attire l'œil, elle n'explique pas.
        let response = match (m.force_muted, m.force_deafened) {
            (true, true) => response.on_hover_text("micro coupé et rendu sourd par un modérateur"),
            (true, false) => response.on_hover_text("micro coupé par un modérateur"),
            (false, true) => response.on_hover_text("rendu sourd par un modérateur"),
            (false, false) => response,
        };
        // Un invité web n'a pas non plus le menu d'un membre : ni rôles, ni
        // bannissement de compte (il n'en a pas), ni poke. Son volume — il
        // peut parler en vocal —, et ce que sa porte permet : le vocal,
        // l'expulsion, l'offre. Jamais sans preuve que le serveur sert les
        // portes : le seul fait qu'il soit là en est une.
        if est_invite_membre(m) {
            let mut actions = Vec::new();
            response.context_menu(|ui| {
                ui.set_width(228.0);
                ui.horizontal(|ui| {
                    ui.label(Texte::Corps.gras(ui, &m.username).color(theme::INVITE));
                    porte_ui::pastille_invite(ui);
                });
                let porte = self.portes.porte_de_l_invite(m.user_id).map(|p| p.slug.clone());
                let quoi = match porte {
                    Some(slug) => format!("entré par la porte « {slug} », sans compte"),
                    None => "entré par une porte web, sans compte".to_string(),
                };
                ui::hint(ui, quoi);
                ui.add_space(espace::XS);
                let mut pct = self.volume_of(m.user_id) * 100.0;
                if ui
                    .add(egui::Slider::new(&mut pct, 0.0..=200.0).suffix(" %").integer().text("volume"))
                    .changed()
                {
                    self.set_volume(m.user_id, pct / 100.0);
                }
                ui.add_space(espace::XS);
                ui::hairline(ui);
                ui.add_space(espace::XS);
                let contexte = self.contexte_portes();
                actions = self.portes.menu_invite(ui, m.user_id, m.voice, &contexte);
            });
            self.appliquer_portes(actions);
            return;
        }
        // Un bot n'a pas le menu d'un membre : son volume, sa fiche, c'est
        // tout — ni rôles, ni expulsion, ni compte Riot.
        if est_bot(m.user_id) {
            response.context_menu(|ui| {
                ui.set_width(228.0);
                ui.horizontal(|ui| {
                    ui.label(Texte::Corps.gras_ton(ui, Ton::Accent, &m.username));
                    pastille_bot(ui);
                });
                ui::hint(ui, "le serveur, qui joue de la musique dans ce salon");
                ui.add_space(espace::XS);
                let mut pct = self.volume_of(m.user_id) * 100.0;
                if ui
                    .add(egui::Slider::new(&mut pct, 0.0..=200.0).suffix(" %").integer().text("volume"))
                    .changed()
                {
                    self.set_volume(m.user_id, pct / 100.0);
                }
                if ui::Bouton::new("Fiche du bot").icone(Icon::Info).show(ui).clicked() {
                    self.fiche_bot = Some(m.user_id);
                    ui.close();
                }
            });
            return;
        }
        response.context_menu(|ui| {
            ui.set_width(228.0);
            ui.label(Texte::Corps.gras(ui, &m.username).color(self.color_of(m)));
            if let Some(role) = self.top_role_name(m) {
                ui::hint(ui, role);
            }
            if !m.online {
                ui::hint(ui, "hors ligne");
            }
            // Sa fiche VALORANT, s'il a lié son compte Riot : le serveur la
            // garde, l'ouvrir ne coûte aucune requête.
            if let Some(riot) = &m.riot_id {
                ui.add_space(espace::XS);
                let rang = m.rang_valorant.map(ki_protocol::nom_de_rang).unwrap_or_default();
                let texte = if rang.is_empty() { riot.clone() } else { format!("{riot} · {rang}") };
                ui::hint(ui, texte);
                if ui::Bouton::new("Fiche VALORANT").icone(Icon::Screen).show(ui).clicked() {
                    self.ouvrir_fiche(m.user_id, m.username.clone());
                }
                // Un admin délie le compte d'un autre — audité côté serveur.
                if !is_me
                    && self.can(ki_protocol::perm::MANAGE_SERVER)
                    && ui::Bouton::new("Délier son compte Riot").icone(Icon::Close).teinte(Ton::Danger).show(ui).clicked()
                {
                    self.send(ClientMsg::DelierRiot { user_id: Some(m.user_id) });
                    ui.close();
                }
            }
            // Le volume ne concerne que quelqu'un qu'on peut entendre.
            if m.online {
                ui.add_space(espace::XS);
                let mut pct = self.volume_of(m.user_id) * 100.0;
                if ui
                    .add(
                        egui::Slider::new(&mut pct, 0.0..=200.0)
                            .suffix(" %")
                            .integer()
                            .text("volume"),
                    )
                    .changed()
                {
                    self.set_volume(m.user_id, pct / 100.0);
                }
                if ui::Bouton::new("Remettre à 100 %").icone(Icon::Refresh).show(ui).clicked() {
                    self.set_volume(m.user_id, 1.0);
                }
            }
            // Le poke : un son chez lui, pour l'appeler sans lui écrire.
            // Seulement face à un serveur qui a prouvé qu'il est récent —
            // un serveur antérieur répondrait « message invalide ». Grisé
            // avec le motif quand on sait d'avance que ça ne partira pas.
            if self.serveur_gere_lus {
                ui.add_space(espace::XS);
                let motif = self.poke_impossible(m);
                ui.add_enabled_ui(motif.is_none(), |ui| {
                    let bouton = ui::Bouton::new("Poke").icone(Icon::Send).show(ui);
                    let bouton = match motif {
                        Some(motif) => bouton.on_disabled_hover_text(motif),
                        None => bouton.on_hover_text("un son et un clignotement chez lui, rien d'autre"),
                    };
                    if bouton.clicked() {
                        self.send(ClientMsg::Poke { user_id: m.user_id });
                        ui.close();
                    }
                });
            }

            // Ce que le rang masque se dit, chiffres à l'appui : sans ce mot,
            // un modérateur face à un égal voyait le menu d'un simple membre
            // et croyait ses permissions cassées.
            if !self.outranks(m.rank) && self.moderation_power() {
                ui.add_space(espace::XS);
                ui::hint(
                    ui,
                    format!(
                        "rang {}, le tien {} : on n'agit que sur plus bas que soi",
                        m.rank, self.my_rank
                    ),
                );
            }

            // Attribution de rôles : cocher/décocher, borné par son propre
            // rang — on ne donne pas un rôle au-dessus de soi.
            let can_roles = self.can(ki_protocol::perm::MANAGE_ROLES)
                && self.outranks(m.rank)
                && !self.roles.is_empty();
            if can_roles {
                ui.add_space(espace::XS);
                ui::hairline(ui);
                ui.add_space(espace::XS);
                ui.menu_button("Rôles", |ui| {
                    ui.set_width(200.0);
                    let mut roles_sorted = self.roles.clone();
                    roles_sorted.sort_by_key(|r| std::cmp::Reverse(r.rank));
                    // Ses rôles tels qu'on les connaît — y compris ce qu'on
                    // vient de cocher, dont le roster n'est pas encore revenu :
                    // deux clics rapides partaient de la même liste, et le
                    // second défaisait le premier.
                    let actuels = self
                        .members
                        .iter()
                        .find(|x| x.user_id == m.user_id)
                        .map_or_else(|| m.roles.clone(), |x| x.roles.clone());
                    // Ce qu'il détient déjà : n'est vérifié que ce qu'un rôle
                    // lui **ajoute**, comme au serveur.
                    let detient = self
                        .roles
                        .iter()
                        .filter(|r| r.id == ki_protocol::ROLE_EVERYONE || actuels.contains(&r.id))
                        .fold(0, |acc, r| acc | r.perms);
                    for role in &roles_sorted {
                        // @everyone se porte d'office : une case toujours
                        // décochée qui ne fait rien n'a rien à faire ici.
                        if role.id == ki_protocol::ROLE_EVERYONE {
                            continue;
                        }
                        let au_dessus = !self.outranks(role.rank);
                        let trop = !self.can(role.perms & !detient);
                        let mut has = actuels.contains(&role.id);
                        let label = RichText::new(&role.name)
                            .color(theme::member_color(role.color, &role.name));
                        let reponse = ui.add_enabled(!au_dessus && !trop, egui::Checkbox::new(&mut has, label));
                        let reponse = if au_dessus {
                            reponse.on_disabled_hover_text("ce rôle est à ton rang ou au-dessus")
                        } else if trop {
                            reponse.on_disabled_hover_text("il donne des permissions que tu n'as pas")
                        } else {
                            reponse
                        };
                        if reponse.changed() {
                            let mut new_roles = actuels.clone();
                            if has {
                                new_roles.push(role.id);
                            } else {
                                new_roles.retain(|id| *id != role.id);
                            }
                            if let Some(x) = self.members.iter_mut().find(|x| x.user_id == m.user_id) {
                                x.roles = new_roles.clone();
                            }
                            self.send(ClientMsg::AdminSetUserRoles {
                                username: m.username.clone(),
                                roles: new_roles,
                            });
                        }
                    }
                });
            }

            // Modération vocale. Sanctionner et déplacer sont deux pouvoirs
            // distincts : faire taire celui qui hurle n'est pas ranger les
            // gens par salon, et tout le monde n'a pas à pouvoir les deux.
            let peut_sanctionner =
                self.outranks(m.rank) && self.can(ki_protocol::perm::MUTE_MEMBERS);
            let peut_deplacer = self.peut_deplacer(m);
            if peut_sanctionner || peut_deplacer {
                ui.add_space(espace::XS);
                ui::hairline(ui);
                ui.add_space(espace::XS);
            }
            if peut_sanctionner {
                // L'intitulé dit ce que le clic va faire, pas l'état courant :
                // « Couper le micro » sur quelqu'un déjà coupé le rendrait
                // muet une seconde fois, ce qui ne veut rien dire.
                let (mute_txt, mute_ton) = if m.force_muted {
                    ("Rendre le micro", Ton::Accent)
                } else {
                    ("Couper le micro", Ton::Danger)
                };
                if ui::Bouton::new(mute_txt).icone(Icon::MicOff).teinte(mute_ton).show(ui).clicked() {
                    self.send(ClientMsg::AdminVoiceMute {
                        username: m.username.clone(),
                        muted: !m.force_muted,
                    });
                    ui.close();
                }
                let (deaf_txt, deaf_ton) = if m.force_deafened {
                    ("Lui rendre l'écoute", Ton::Accent)
                } else {
                    ("Rendre sourd", Ton::Danger)
                };
                if ui::Bouton::new(deaf_txt).icone(Icon::HeadphonesOff).teinte(deaf_ton).show(ui).clicked() {
                    self.send(ClientMsg::AdminVoiceDeafen {
                        username: m.username.clone(),
                        deafened: !m.force_deafened,
                    });
                    ui.close();
                }
            }
            if peut_deplacer {
                let vocaux: Vec<ki_protocol::ChannelInfo> = self
                    .channels
                    .iter()
                    .filter(|c| c.kind == ChannelKind::Voice && Some(c.id) != m.voice)
                    .cloned()
                    .collect();
                let proprietaire = self.suis_proprietaire();
                ui.menu_button("Déplacer en vocal", |ui| {
                    ui.set_width(200.0);
                    for c in &vocaux {
                        // Un salon fermé par un mot de passe n'est qu'à ceux
                        // qui ont le code : seul le propriétaire y amène.
                        if c.locked && !proprietaire {
                            ui.add_enabled_ui(false, |ui| ui::Bouton::new(&c.name).icone(Icon::Key).show(ui))
                                .response
                                .on_disabled_hover_text(
                                    "fermé par un mot de passe : seul le propriétaire y amène quelqu'un",
                                );
                            continue;
                        }
                        if ui::Bouton::new(&c.name).icone(Icon::Volume).show(ui).clicked() {
                            self.send(ClientMsg::AdminVoiceMove {
                                username: m.username.clone(),
                                channel: Some(c.id),
                            });
                            ui.close();
                        }
                    }
                    // Sortir quelqu'un du vocal n'est proposé que s'il y est :
                    // ailleurs, le bouton ne ferait rien de visible.
                    if m.voice.is_some()
                        && ui::Bouton::new("Sortir du vocal").icone(Icon::Logout).teinte(Ton::Danger).show(ui)
                            .clicked()
                    {
                        self.send(ClientMsg::AdminVoiceMove {
                            username: m.username.clone(),
                            channel: None,
                        });
                        ui.close();
                    }
                });
            }

            // Il diffuse son écran et l'on partage son salon vocal : le
            // visionnage est à un clic — c'est la porte d'entrée du S1b.
            if let Some(stream_id) = m.streaming {
                if !is_me && m.voice.is_some() && m.voice == self.voice_channel {
                    let deja = self.regard.flux.as_ref().map(|r| r.stream_id) == Some(stream_id);
                    let libelle =
                        if deja { "Arrêter de regarder" } else { "Regarder son écran" };
                    if ui::Bouton::new(libelle).icone(Icon::Play).show(ui).clicked() {
                        if deja {
                            self.fermer_regard(true);
                        } else {
                            self.send(ClientMsg::Watch { stream_id, couches: true });
                        }
                        ui.close();
                    }
                    ui.add_space(espace::XS);
                }
            }

            // Chaque action n'apparaît que si elle aboutirait : la permission
            // ET le rang. Un bouton grisé n'invite qu'à un clic qui échoue,
            // là où la hiérarchie se lit déjà dans les badges de rôle.
            let can_moderate = self.outranks(m.rank);
            let show_kick = can_moderate && self.can(ki_protocol::perm::KICK) && m.online;
            let show_ban = can_moderate && self.can(ki_protocol::perm::BAN);
            if show_kick || show_ban {
                ui.add_space(espace::XS);
                ui::hairline(ui);
                ui.add_space(espace::XS);
            }
            // Expulser met dehors ; la personne peut revenir aussitôt.
            if show_kick
                && ui::Bouton::new("Expulser").icone(Icon::Logout).teinte(Ton::Danger).show(ui).clicked()
            {
                self.send(ClientMsg::Kick {
                    user_id: m.user_id,
                    reason: String::new(),
                });
                ui.close();
            }
            // Bannir l'empêche de revenir : motif et durée se saisissent
            // dans une petite fenêtre, plutôt qu'au fond d'un menu.
            if show_ban
                && ui::Bouton::new("Bannir…").icone(Icon::Ban).teinte(Ton::Danger).show(ui).clicked()
            {
                self.admin.ban = Some(BanDraft {
                    username: m.username.clone(),
                    reason: String::new(),
                    duration_secs: 86_400,
                });
                ui.close();
            }
        });
    }

    /// Colonne de droite : tout le monde sur le serveur, comme sur Discord.
    /// On y voit qui est là même sans partager de salon vocal.
    pub(crate) fn roster_panel(&mut self, ui: &mut egui::Ui, voice: &VoiceSnapshot) {
        egui::Panel::right("roster")
            .resizable(true)
            .default_size(ROSTER_WIDTH)
            .size_range(ROSTER_LARGEURS)
            .frame(
                egui::Frame::NONE
                    .fill(theme::BG_SIDE)
                    .inner_margin(marge::symetrique(espace::M, espace::L)),
            )
            .show(ui, |ui| {
                // `take` et non `clone` : la liste est empruntée le temps du
                // rendu, puis remise. Copiée, elle coûtait — pseudo, rôles et
                // empreinte d'avatar par membre — une reconstruction complète
                // à chaque image, pour lire des champs qu'on ne modifie pas.
                // C'est la technique déjà employée par le fil de discussion ;
                // la remise a lieu plus bas, avant de sortir de la fermeture.
                let members = std::mem::take(&mut self.members);
                // Trois listes : les membres en ligne, les invités web (à
                // part — pas des comptes), les hors-ligne.
                let (invites, comptes): (Vec<&Member>, Vec<&Member>) =
                    members.iter().partition(|m| est_invite_membre(m));
                let (online, offline): (Vec<&Member>, Vec<&Member>) =
                    comptes.into_iter().partition(|m| m.online);

                ui.horizontal(|ui| {
                    ui::section_label(ui, "En ligne");
                    ui.label(
                        Texte::Aide.gras(ui, online.len().to_string()).color(theme::BORDER_STRONG),
                    );
                });
                ui.add_space(espace::XXS);

                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    for m in &online {
                        let is_me = Some(m.user_id) == self.my_id;
                        // Le vumètre n'a de sens que si l'on partage le salon
                        // vocal de la personne : sinon on ne l'entend pas.
                        let audible = m.voice.is_some() && m.voice == self.voice_channel;
                        let level = if !audible {
                            0.0
                        } else if is_me {
                            voice.stats.mic_peak
                        } else {
                            voice.levels.get(&m.user_id).copied().unwrap_or(0.0)
                        };
                        let speaking =
                            if is_me { self.vocal.transmitting } else { m.speaking || level > SPEAK_LEVEL };
                        if let Some(tier) = palier_de(m) {
                            self.rangs.preparer(ui.ctx(), tier);
                        }
                        let rang_icone = palier_de(m).and_then(|t| self.rangs.texture(t));
                        let photo = self.avatar_of(m.user_id);
                        // « Muet » n'a de sens qu'en vocal : hors salon, un
                        // micro coupé résiduel n'apprend rien à personne.
                        let muted =
                            m.voice.is_some() && if is_me { self.vocal.muted } else { m.muted };
                        let (response, regarder) = member_row(
                            ui,
                            MemberRow {
                                member: m,
                                speaking: speaking && audible,
                                muted,
                                is_me,
                                photo,
                                rang_icone,
                                glissable: false,
                                spectateurs: self.nb_spectateurs(m),
                            },
                        );
                        if regarder {
                            self.regarder(m);
                        }
                        self.member_menu(response, m, is_me);
                    }

                    // Les invités web : en ligne, forcément, mais pas des
                    // nôtres — ni rôle, ni rang, ni photo. Le vumètre, oui :
                    // l'hôte peut les mettre en vocal.
                    if !invites.is_empty() {
                        ui.add_space(espace::L);
                        ui.horizontal(|ui| {
                            ui::section_label(ui, "Invités");
                            ui.label(
                                Texte::Aide.gras(ui, invites.len().to_string()).color(theme::BORDER_STRONG),
                            );
                        });
                        ui.add_space(espace::XXS);
                        for m in &invites {
                            let audible = m.voice.is_some() && m.voice == self.voice_channel;
                            let level = if audible {
                                voice.levels.get(&m.user_id).copied().unwrap_or(0.0)
                            } else {
                                0.0
                            };
                            let speaking = m.speaking || level > SPEAK_LEVEL;
                            let (response, _) = member_row(
                                ui,
                                MemberRow {
                                    member: m,
                                    speaking: speaking && audible,
                                    muted: m.voice.is_some() && m.muted,
                                    is_me: false,
                                    photo: None,
                                    rang_icone: None,
                                    glissable: false,
                                    spectateurs: 0,
                                },
                            );
                            self.member_menu(response, m, false);
                        }
                    }

                    // Toute la communauté, pas seulement les présents : les
                    // hors-ligne en dessous, éteints mais bien là — et le
                    // clic droit (rôles, bannir…) marche aussi sur eux.
                    if !offline.is_empty() {
                        ui.add_space(espace::L);
                        ui.horizontal(|ui| {
                            ui::section_label(ui, "Hors ligne");
                            ui.label(
                                Texte::Aide.gras(ui, offline.len().to_string()).color(theme::BORDER_STRONG),
                            );
                        });
                        ui.add_space(espace::XXS);
                        for m in &offline {
                            if let Some(tier) = palier_de(m) {
                                self.rangs.preparer(ui.ctx(), tier);
                            }
                            let photo = self.avatar_of(m.user_id);
                            let mut ligne = MemberRow::offline(m, photo);
                            ligne.rang_icone = palier_de(m).and_then(|t| self.rangs.texture(t));
                            let (response, _) = member_row(ui, ligne);
                            self.member_menu(response, m, false);
                        }
                    }
                });
                drop((online, offline, invites));
                self.members = members;
            });
    }

    /// La fiche d'un bot : ce qu'il est, ce qu'il fait, qui le pilote, et
    /// comment il marche — en quelques lignes honnêtes.
    pub(crate) fn fiche_bot_window(&mut self, ctx: &egui::Context) {
        let Some(id) = self.fiche_bot else { return };
        let mut open = true;
        let etat = self.musique.etat.clone();
        let pilotes: Vec<String> = self
            .roles
            .iter()
            .filter(|r| r.perms & (ki_protocol::perm::CONTROL_MUSIC | ki_protocol::perm::ADMINISTRATOR) != 0)
            .map(|r| r.name.clone())
            .collect();
        let titre = if id == ki_protocol::MUSIQUE_ID { "Musique" } else { "VALORANT" };
        ki_ui::fenetre::Fenetre::new("fiche_bot", format!("{titre} · bot"))
            .ouverte(&mut open)
            .fixe()
            .largeur(440.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(Texte::TitreSection.riche(ui, titre));
                    pastille_bot(ui);
                });
                ui.label(Texte::Libelle.riche(ui, "Ce n'est pas un membre : c'est le serveur ki-chat lui-même, qui n'a ni compte ni mot de passe."));
                ui.add_space(espace::M);
                if id == ki_protocol::MUSIQUE_ID {
                    ui.label(Texte::Corps.gras(ui, "Ce qu'il fait"));
                    match &etat.en_cours {
                        Some(p) => {
                            ui.label(format!("joue « {} » {}", p.titre, if p.artiste.is_empty() { String::new() } else { format!("de {}", p.artiste) }));
                        }
                        None => {
                            ui.label(RichText::new("rien en ce moment").color(TEXT_DIM));
                        }
                    }
                    ui.label(Texte::Libelle.riche(ui, format!("{} en file · volume global {} %", etat.longueur_file(), etat.volume)));
                    ui.add_space(espace::M);
                    ui.label(Texte::Corps.gras(ui, "Qui le pilote"));
                    ui.label(Texte::Libelle.riche(ui, if pilotes.is_empty() { "personne pour l'instant — la permission « Contrôler la musique » se donne dans les rôles".to_string() } else { format!("les rôles {} (permission « Contrôler la musique »)", pilotes.join(", ")) }));
                    ui.add_space(espace::M);
                    ui.label(Texte::Corps.gras(ui, "Comment il marche"));
                    ui.label(Texte::Libelle.riche(ui, "Le serveur tire le flux audio de YouTube ou SoundCloud avec yt-dlp, le décode avec ffmpeg, \
                         l'encode en Opus et l'envoie dans le salon chiffré comme la voix de n'importe qui. Aucun fichier \
                         audio n'est écrit, aucun client ne télécharge rien, et le flux brut ne contient pas de publicité. \
                         Chacun le règle ou le coupe comme un membre.",));
                    ui.add_space(espace::M);
                    ui.label(Texte::Corps.gras(ui, "Depuis le démarrage du serveur"));
                    let c = &etat.compteurs;
                    ui.label(Texte::Libelle.riche(ui, format!(
                        "{} piste{} jouée{} · {} échec{} · premier son en {:.1} s en moyenne",
                        c.pistes_jouees, if c.pistes_jouees > 1 { "s" } else { "" }, if c.pistes_jouees > 1 { "s" } else { "" },
                        c.echecs, if c.echecs > 1 { "s" } else { "" },
                        c.premier_son_ms as f32 / 1000.0
                    )));
                } else {
                    ui.label(Texte::Corps.gras(ui, "Ce qu'il fait"));
                    ui.label(Texte::Libelle.riche(ui, "Il poste le fil de jeu : à chaque partie finie d'un membre qui a lié son compte Riot, le résultat, sa ligne et ses RR."));
                    ui.add_space(espace::M);
                    ui.label(Texte::Corps.gras(ui, "Comment il marche"));
                    ui.label(Texte::Libelle.riche(ui, "Les données viennent de HenrikDev, à partir du Riot ID que chacun a lié lui-même. Le serveur ne garde que la ligne du membre dans ses matchs, jamais celles des adversaires, et délier efface tout."));
                }
            });
        if !open {
            self.fiche_bot = None;
        }
    }

    /// Le bandeau « X te poke » encore à afficher, s'il n'a pas expiré.
    pub(crate) fn poke_en_cours(&mut self) -> Option<String> {
        let (nom, quand) = self.poke_recu.as_ref()?;
        if quand.elapsed() >= DUREE_POKE {
            self.poke_recu = None;
            return None;
        }
        Some(nom.clone())
    }

    /// Pourquoi le poke vers `m` ne partirait pas, tel que le client le
    /// sait déjà — pour griser le bouton avec le motif au survol. Le
    /// serveur reste l'autorité : ce qu'il refuse en plus arrive en
    /// `PokeRefuse`.
    pub(crate) fn poke_impossible(&self, m: &Member) -> Option<&'static str> {
        if !m.online {
            return Some("hors ligne");
        }
        if m.voice.is_some() {
            return Some("en vocal — il t'entend déjà");
        }
        if m
            .jeu
            .as_ref()
            .is_some_and(|j| matches!(j.etat, ki_protocol::JeuEtat::PreGame | ki_protocol::JeuEtat::EnJeu))
        {
            return Some("en partie — on ne le dérange pas");
        }
        if self
            .pokes_refuses
            .get(&m.user_id)
            .is_some_and(|quand| quand.elapsed() < DELAI_REFUS_POKE)
        {
            return Some("refusé à l'instant — attends un peu");
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sur un autre compte que celui de sa fiche, le badge prend le palier
    /// de la présence et la ligne de partie le dit ; un compte principal ou
    /// non dit ne change rien. L'empreinte d'un puuid est stable, sans la
    /// casse, et ne le contient pas.
    #[test]
    fn le_compte_joue_se_voit_sous_le_pseudo() {
        let mut m: Member = serde_json::from_value(serde_json::json!({
            "user_id": 4, "username": "Kayou", "speaking": false, "rang_valorant": 21,
            "jeu": {"etat": "en_jeu", "file": "competitive", "rang": 12},
        }))
        .unwrap();
        assert_eq!(palier_de(&m), Some(21), "la fiche d'abord");
        assert_eq!(mention_du_compte(&m), None);
        m.compte_joue = Some(ki_protocol::CompteJoue::Principal);
        assert_eq!((palier_de(&m), mention_du_compte(&m)), (Some(21), None));
        m.compte_joue = Some(ki_protocol::CompteJoue::Autre);
        assert_eq!(palier_de(&m), Some(12), "le palier du compte joué");
        assert_eq!(mention_du_compte(&m).as_deref(), Some("autre compte"));
        m.compte_joue = Some(ki_protocol::CompteJoue::Second { riot_id: Some("Smurf#EUW".into()) });
        assert_eq!(mention_du_compte(&m).as_deref(), Some("sur Smurf#EUW"));
        m.compte_joue = Some(ki_protocol::CompteJoue::Second { riot_id: None });
        assert_eq!(mention_du_compte(&m).as_deref(), Some("2ᵉ compte"));

        let e = empreinte_compte("AbC-123");
        assert_eq!(e, empreinte_compte("abc-123"));
        assert_ne!(e, empreinte_compte("abc-124"));
        assert_eq!(e.len(), 16);
        assert!(!e.contains("abc"));
    }
}
