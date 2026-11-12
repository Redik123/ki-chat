//! La barre du bas : micro, casque, salon vocal, les boutons des fenêtres, le
//! vumètre et le ping — sur trois largeurs, selon ce qui tient.

use ki_ui::typo::Texte;
use eframe::egui::{self, RichText, Sense, Vec2};
use ki_protocol::{ClientMsg, IconChange};
use ki_ui::jetons::{espace, marge, rayon, texte};

use crate::{theme, ui, KiApp, MicMode, VoiceSnapshot};
use crate::admin_ui::AdminTab;
use crate::formats::compact;
use crate::icons::Icon;
use crate::ptt::PttKey;
use crate::theme::{ACCENT, DANGER, SPEAK, TEXT, TEXT_DIM, TEXT_FAINT, WARN};
use crate::ui::Ton;

/// Ce que la barre du bas montre, selon la place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Barre {
    /// Tout : boutons avec leur libellé, vumètre, paquets, ping.
    Large,
    /// Les boutons réduits à leur icône.
    Etroite,
    /// Et, à droite, le ping seul.
    TresEtroite,
}

/// Le niveau le plus riche qui tient dans `disponible`. `libelles` et
/// `icones` : la largeur de la partie gauche dans chacune de ses deux formes,
/// `None` tant qu'elle n'a pas été dessinée ainsi — on l'essaie alors, pour
/// la mesurer. `droite` : celle de la partie droite entière.
pub(crate) fn niveau_de_barre(libelles: Option<f32>, icones: Option<f32>, droite: f32, disponible: f32) -> Barre {
    let tient = |gauche: Option<f32>| gauche.is_none_or(|g| g + droite + espace::M <= disponible);
    if tient(libelles) {
        Barre::Large
    } else if tient(icones) {
        Barre::Etroite
    } else {
        Barre::TresEtroite
    }
}

/// Un bouton de la barre du bas : avec son libellé quand la place le permet,
/// réduit à son icône sinon — le libellé passe alors dans la bulle.
pub(crate) fn bouton_de_barre(ui: &mut egui::Ui, compact: bool, icone: Icon, libelle: &str) -> egui::Response {
    if compact {
        ui::Bouton::icone_seule(icone).bulle(libelle).show(ui)
    } else {
        ui::Bouton::new(libelle).icone(icone).show(ui)
    }
}

impl KiApp {
    /// Barre du bas : tout le contrôle vocal et la télémétrie réseau.
    pub(crate) fn voice_bar(&mut self, ui: &mut egui::Ui, voice: &VoiceSnapshot) {
        egui::Panel::bottom("voice_bar")
            .frame(
                egui::Frame::NONE
                    .fill(theme::BG_SIDE)
                    .inner_margin(marge::symetrique(espace::L, espace::M)),
            )
            .show(ui, |ui| {
                // Périphérique disparu : le dire franchement. Sans ça, on
                // parle dans le vide sans comprendre pourquoi — le cas le
                // plus courant étant un casque sans fil qui sort de veille.
                let (mic_lost, out_lost) = voice.device_trouble;
                if mic_lost || out_lost {
                    let what = match (mic_lost, out_lost) {
                        (true, true) => "Micro et sortie audio perdus",
                        (true, false) => "Micro perdu",
                        _ => "Sortie audio perdue",
                    };
                    // Le cas qui dure : un jeu (Valorant…) a pris le
                    // périphérique en mode exclusif — Windows refusera nos
                    // réouvertures tant qu'il le tient. La reconnexion seule
                    // n'y peut rien : c'est une case à décocher côté Windows,
                    // autant le dire ici plutôt que laisser mariner.
                    ui::bandeau(
                        ui,
                        Ton::Alerte,
                        &format!( "{what} — reconnexion automatique en cours… Si ça arrive quand un jeu se lance et que ça dure : Paramètres son Windows → ton micro → Propriétés → Avancé → décoche « Autoriser les applications à prendre le contrôle exclusif »." ),
                    );
                    ui.add_space(espace::S);
                }
                // Le repli est une autre histoire : le son passe, simplement
                // pas par le périphérique réglé. Annoncer une perte serait
                // faux, et promettre une reconnexion, trompeur — c'est un
                // avis, pas une alerte, et il dit quoi faire.
                let (mic_back, out_back) = voice.device_fallback;
                if (mic_back || out_back) && !mic_lost && !out_lost {
                    let what = match (mic_back, out_back) {
                        (true, true) => "Le micro et la sortie audio réglés",
                        (true, false) => "Le micro réglé",
                        _ => "La sortie audio réglée",
                    };
                    ui::bandeau(
                        ui,
                        Ton::Info,
                        &format!( "{what} n'a pas été trouvé — on utilise le périphérique par défaut, et on le reprend dès son retour." ),
                    );
                    ui.add_space(espace::S);
                }
                ui.horizontal(|ui| {
                    // À la taille par défaut de la fenêtre, les boutons et
                    // leurs libellés ne tenaient pas : le ping recouvrait
                    // « Portes », et « Admin » sortait de l'écran. Quand la
                    // place manque, les boutons se réduisent à leur icône ;
                    // quand elle manque encore, la droite ne garde que le ping.
                    let (gauche, disponible) = (ui.max_rect().left(), ui.max_rect().width());
                    let niveau = niveau_de_barre(
                        self.barre_largeur_libelles,
                        self.barre_largeur_icones,
                        self.barre_largeur_droite,
                        disponible,
                    );
                    let etroite = niveau != Barre::Large;
                    // --- Micro : gros bouton d'état ---
                    let in_voice = self.voice_channel.is_some();
                    let (icon, tint, state, state_color) = if !in_voice {
                        (Icon::MicOff, None, "Hors vocal", TEXT_FAINT)
                    } else if self.vocal.muted {
                        (Icon::MicOff, Some(DANGER), "Micro coupé", DANGER)
                    } else if self.vocal.transmitting {
                        (Icon::Mic, Some(SPEAK), "En émission", SPEAK)
                    } else {
                        (Icon::Mic, None, "Micro prêt", TEXT_DIM)
                    };
                    let tip = if !in_voice {
                        "Rejoins un salon vocal pour parler"
                    } else if self.vocal.muted {
                        "Réactiver le micro"
                    } else {
                        "Couper le micro"
                    };
                    let clicked = ui::Bouton::icone_seule(icon).cote(38.0).couleur_si(tint).bulle(tip).show(ui).clicked();
                    if clicked && in_voice {
                        self.basculer_micro();
                    }
                    // --- Sourd : plus rien n'entre, et le micro est coupé ---
                    if in_voice {
                        let (icon, tint, tip) = if self.vocal.sourd {
                            (Icon::HeadphonesOff, Some(DANGER), "Retrouver l'écoute")
                        } else {
                            (Icon::Headphones, None, "Se rendre sourd (coupe aussi le micro)")
                        };
                        if ui::Bouton::icone_seule(icon).cote(38.0).couleur_si(tint).bulle(tip).show(ui).clicked() {
                            self.basculer_sourd();
                        }
                    }
                    // Partage d'écran : le sélecteur (source, réglages,
                    // démarrer) au repos, l'arrêt en direct — en vocal
                    // seulement.
                    let (s_icon, s_tint, s_tip) = if self.diffusion.live.is_some() {
                        (Icon::Screen, Some(DANGER), "Tu diffuses ton écran — clic pour arrêter")
                    } else if self.diffusion.attente.is_some() {
                        (Icon::Screen, Some(WARN), "Diffusion en cours de démarrage…")
                    } else if self.diffusion.ouverte {
                        (Icon::Screen, Some(ACCENT), "Diffuser mon écran au salon")
                    } else {
                        (Icon::Screen, None, "Diffuser mon écran au salon")
                    };
                    if ui::Bouton::icone_seule(s_icon).cote(38.0).couleur_si(s_tint).bulle(s_tip).show(ui).clicked() && in_voice {
                        if self.diffusion.live.is_some() {
                            self.arreter_diffusion();
                        } else if self.diffusion.attente.is_none() {
                            self.diffusion.ouverte = !self.diffusion.ouverte;
                            if self.diffusion.ouverte {
                                self.diffusion.sources.rafraichir();
                            }
                        }
                    }
                    ui.add_space(espace::XXS);
                    ui.vertical(|ui| {
                        ui.add_space(1.0);
                        ui.label(Texte::Corps.gras(ui, state).color(state_color));
                        // Sous-titre : le salon vocal occupé, ou la touche.
                        let detail = match (self.voice_channel_name(), self.audio.mode) {
                            (None, _) => "aucun salon vocal".to_string(),
                            (Some(name), MicMode::Ptt) => {
                                format!("{name} · touche {}", self.audio.ptt_key.label())
                            }
                            (Some(name), _) => name.to_string(),
                        };
                        ui::hint(ui, detail);
                    });

                    // Sortir du vocal sans quitter le serveur.
                    if in_voice
                        && ui::Bouton::icone_seule(Icon::Logout).cote(30.0).couleur(DANGER).bulle("Quitter le vocal").show(ui)
                            .clicked()
                    {
                        self.leave_voice();
                    }

                    ui.add_space(espace::XL);

                    // --- Mode d'émission ---
                    let mode_before = self.audio.mode;
                    egui::ComboBox::from_id_salt("mic_mode")
                        .width(150.0)
                        .selected_text(RichText::new(self.audio.mode.label()).color(TEXT))
                        .show_ui(ui, |ui| {
                            for mode in [MicMode::Open, MicMode::Ptt, MicMode::Vad] {
                                ui.selectable_value(&mut self.audio.mode, mode, mode.label());
                            }
                        });
                    if self.audio.mode != mode_before {
                        self.apply_audio_settings();
                    }
                    if self.audio.mode == MicMode::Ptt {
                        egui::ComboBox::from_id_salt("ptt_key")
                            .width(110.0)
                            .selected_text(RichText::new(self.audio.ptt_key.label()).color(TEXT))
                            .show_ui(ui, |ui| {
                                for key in PttKey::ALL {
                                    ui.selectable_value(&mut self.audio.ptt_key, key, key.label());
                                }
                            });
                    }

                    ui.add_space(espace::S);
                    if self.loupedeck_etat.actif
                        && bouton_de_barre(ui, etroite, Icon::Sliders, "Loupedeck")
                            .on_hover_text("ce que font les boutons, les molettes et les écrans de ton Loupedeck Live")
                            .clicked()
                    {
                        self.loupedeck_etat.page_ouverte = !self.loupedeck_etat.page_ouverte;
                    }
                    if bouton_de_barre(ui, etroite, Icon::Target, "Valorant").clicked() {
                        self.ouvrir_stats();
                        self.valo.vers_accueil();
                    }
                    if bouton_de_barre(ui, etroite, Icon::Film, "Clips").clicked() {
                        self.ouvrir_clips();
                    }
                    // La table de cartes du salon : « À toi ! » quand on
                    // attend après moi.
                    if self.serveur_cartes {
                        let bouton = if self.cartes.mon_tour(self.my_id) {
                            ui::Bouton::new("À toi !").icone(Icon::Cartes).teinte(Ton::Accent).show(ui)
                        } else {
                            bouton_de_barre(ui, etroite, Icon::Cartes, "Cartes")
                        };
                        if bouton
                            .on_hover_text("Dernière carte : un jeu de cartes avec ceux de ton salon vocal")
                            .clicked()
                        {
                            self.cartes.ouvert = !self.cartes.ouvert;
                        }
                    }
                    if bouton_de_barre(ui, etroite, Icon::Pencil, "Tactique")
                        .on_hover_text("une carte de VALORANT où dessiner la prochaine manche ; chaque trait s'efface 15 s après")
                        .clicked()
                    {
                        self.tactique.ouvert = !self.tactique.ouvert;
                        self.catalogue.demarrer();
                    }
                    if self.soundboard_visible
                        && bouton_de_barre(ui, etroite, Icon::Volume, "Soundboard")
                            .on_hover_text("des sons à la touche, entendus par tout le salon vocal")
                            .clicked()
                    {
                        self.soundboard.basculer();
                    }
                    // Les portes web : ouvrir la sienne (« Créer des
                    // invitations »), ou voir celles des autres quand le
                    // serveur nous en parle. Jamais face à un serveur
                    // antérieur, qui n'en sait rien.
                    let portes_visibles = self.portes.disponible
                        && (self.can(ki_protocol::perm::CREATE_INVITE) || !self.portes.est_vide());
                    if portes_visibles
                        && bouton_de_barre(ui, etroite, Icon::Key, "Portes")
                            .on_hover_text("un lien pour faire entrer quelqu'un par le web, sans compte, le temps d'une soirée")
                            .clicked()
                    {
                        self.portes.ouvert = !self.portes.ouvert;
                    }
                    // Le point rouge de l'enregistreur de clips : allumé, il
                    // tourne et un clic l'arrête ; éteint, un clic le lance.
                    let rec = self.clips.enregistreur.is_some();
                    let (rect, r) = ui.allocate_exact_size(Vec2::new(54.0, 28.0), Sense::click());
                    if ui.is_rect_visible(rect) {
                        let p = ui.painter();
                        if r.hovered() {
                            p.rect_filled(rect, egui::CornerRadius::same(rayon::L), theme::BG_HOVER);
                        }
                        let couleur = if rec { DANGER } else { TEXT_FAINT };
                        let pulse = if rec { 0.75 + 0.25 * (ui.input(|i| i.time) * 3.0).sin().abs() as f32 } else { 1.0 };
                        p.circle_filled(egui::pos2(rect.left() + 13.0, rect.center().y), 5.0 * pulse, couleur);
                        p.text(
                            egui::pos2(rect.left() + 23.0, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            "REC",
                            egui::FontId::proportional(texte::PETIT),
                            couleur,
                        );
                    }
                    let bulle = if rec {
                        format!("enregistreur de clips en marche ({}) — clic pour l'arrêter", self.clips.reglages.raccourci.label())
                    } else {
                        "lancer l'enregistreur de clips".to_string()
                    };
                    if r.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(bulle).clicked() {
                        if rec {
                            self.arreter_clips();
                        } else {
                            self.demarrer_clips();
                        }
                    }
                    if self.any_admin_power() && bouton_de_barre(ui, etroite, Icon::Crown, "Admin").clicked() {
                        self.admin.ouverte = !self.admin.ouverte;
                        if self.admin.ouverte {
                            self.admin.nom = self.server_info.name.clone();
                            self.admin.adresse_web = self.server_info.adresse_web.clone();
                            self.admin.icone = IconChange::Keep;
                            self.send(ClientMsg::AdminListUsers);
                            if self.admin.onglet == AdminTab::Fichiers {
                                let demande = self.admin.fichiers.demander();
                                self.send(demande);
                            }
                        }
                    }

                    // Une sanction vocale s'annonce là où l'on cherchera :
                    // à côté du micro, pas au fond d'une liste. Sans ça on
                    // croit son matériel en panne et l'on part démonter ses
                    // réglages audio — ou son casque.
                    if let Some(moi) = self.my_id.and_then(|id| {
                        self.members.iter().find(|m| m.user_id == id)
                    }) {
                        let sanction = match (moi.force_muted, moi.force_deafened) {
                            (true, true) => Some("micro coupé et sourd (modérateur)"),
                            (true, false) => Some("micro coupé par un modérateur"),
                            (false, true) => Some("rendu sourd par un modérateur"),
                            (false, false) => None,
                        };
                        if let Some(texte) = sanction {
                            ui.add_space(espace::S);
                            ui::glyph(ui, Icon::MicOff, 13.0, WARN);
                            ui.label(Texte::Libelle.riche_ton(ui, Ton::Alerte, texte)).on_hover_text(
                                "ce n'est pas ton matériel : quelqu'un l'a décidé côté serveur",
                            );
                        }
                    }

                    // Ce que la partie gauche occupe, dans la forme où elle
                    // vient d'être dessinée : la mesure de l'autre forme est
                    // gardée — c'est elle qui dira qu'elle tient à nouveau.
                    let occupe = ui.cursor().left() - gauche;
                    if etroite {
                        self.barre_largeur_icones = Some(occupe);
                    } else {
                        self.barre_largeur_libelles = Some(occupe);
                    }
                    // --- Télémétrie, alignée à droite ---
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !voice.engine_up {
                            ui.label(Texte::Libelle.riche_ton(ui, Ton::Alerte, "vocal indisponible"))
                                .on_hover_text(
                                    "aucun périphérique audio n'a pu être ouvert — \
                                 vérifie les réglages audio",
                                );
                            return;
                        }
                        let s = &voice.stats;
                        // Fenêtre très étroite : le ping seul. Le vumètre et
                        // les compteurs de paquets se lisent dans les réglages.
                        if niveau != Barre::TresEtroite {
                            let level = (s.mic_peak * 3.0).min(1.0);
                            let color = if self.vocal.transmitting {
                                SPEAK
                            } else {
                                theme::BG_ACTIVE
                            };
                            ui::meter(ui, level, Vec2::new(96.0, 7.0), color)
                                .on_hover_text("niveau du micro");

                            ui.add_space(espace::XS);
                            ui::stat_row(
                                ui,
                                &[
                                    (Icon::ArrowUp, compact(s.packets_sent), TEXT_FAINT),
                                    (Icon::ArrowDown, compact(s.packets_received), TEXT_FAINT),
                                    (
                                        Icon::Close,
                                        compact(s.packets_lost),
                                        if s.packets_lost > 0 { WARN } else { TEXT_FAINT },
                                    ),
                                ],
                                11.0,
                            )
                            .on_hover_text("paquets voix envoyés · reçus · perdus");

                            ui.add_space(espace::XS);
                        }
                        match voice.ping {
                            Some(ping) => {
                                let (lit, color) = if ping < 30 {
                                    (4, SPEAK)
                                } else if ping < 80 {
                                    (3, ACCENT)
                                } else if ping < 150 {
                                    (2, WARN)
                                } else {
                                    (1, DANGER)
                                };
                                ui::signal_badge(ui, lit, &format!("{ping} ms"), color)
                                    .on_hover_text("ping (RTT QUIC)");
                            }
                            None => {
                                ui::signal_badge(ui, 0, "— ms", TEXT_FAINT);
                            }
                        }
                        // La droite entière seulement : c'est elle qu'on
                        // compare à la place disponible.
                        if niveau != Barre::TresEtroite {
                            self.barre_largeur_droite = ui.min_rect().width();
                        }
                    });
                });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La barre du bas montre le plus qu'elle peut : libellés, puis icônes,
    /// puis le ping seul à droite. Une forme jamais dessinée est essayée
    /// d'abord — il faut bien la mesurer une fois.
    #[test]
    fn la_barre_du_bas_montre_ce_qui_tient() {
        // Rien de mesuré : tout, pour mesurer.
        assert_eq!(niveau_de_barre(None, None, 0.0, 900.0), Barre::Large);
        // Les libellés tiennent (1 130 + 260 dans 1 700).
        assert_eq!(niveau_de_barre(Some(1130.0), None, 260.0, 1700.0), Barre::Large);
        // Ils ne tiennent plus à 1 120 : les icônes, à mesurer d'abord…
        assert_eq!(niveau_de_barre(Some(1130.0), None, 260.0, 1120.0), Barre::Etroite);
        // … elles tiennent (780 + 260 dans 1 120)…
        assert_eq!(niveau_de_barre(Some(1130.0), Some(780.0), 260.0, 1120.0), Barre::Etroite);
        // … mais plus à 900 : le ping seul à droite.
        assert_eq!(niveau_de_barre(Some(1130.0), Some(780.0), 260.0, 900.0), Barre::TresEtroite);
        // La fenêtre regrandit : on remonte d'un niveau, puis de deux.
        assert_eq!(niveau_de_barre(Some(1130.0), Some(780.0), 260.0, 1100.0), Barre::Etroite);
        assert_eq!(niveau_de_barre(Some(1130.0), Some(780.0), 260.0, 1500.0), Barre::Large);
    }
}
