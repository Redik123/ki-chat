//! Les fenêtres de réglages : les réglages et leurs onglets, « Mon compte », la
//! mise à jour, l'export et l'import de ses réglages.

use ki_ui::typo::Texte;
use eframe::egui::{self, RichText, Vec2};
use ki_protocol::{ClientMsg, IconChange};
use ki_ui::jetons::{espace, texte};

use crate::{
    jeux, partage, prefs, sfx, sound_dirs, theme, ui, update, valo_catalogue, Apercu, KiApp, Notif,
    VoiceSnapshot, BITRATES,
};
use crate::admin_ui::AdminTab;
use crate::formats::{format_time_secs, megabytes};
use crate::icons::Icon;
use crate::saisie::menu_edition;
use crate::theme::{color_for, DANGER, SPEAK, TEXT, TEXT_DIM};
use crate::ui::Ton;

/// Les onglets de la fenêtre de réglages. Elle s'appelait « Réglages audio »
/// et empilait huit sections dans une seule colonne à faire défiler : la
/// moitié n'avait plus rien d'audio, et la diffusion se trouvait tout en bas.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Onglet {
    Audio,
    Casque,
    Reseau,
    Diffusion,
    Jeu,
    Clips,
    Sons,
    /// Les fonctions encore en essai, coupées d'origine.
    Beta,
    Aide,
}

impl Onglet {
    const TOUS: [Onglet; 9] = [
        Onglet::Audio,
        Onglet::Casque,
        Onglet::Reseau,
        Onglet::Diffusion,
        Onglet::Jeu,
        Onglet::Clips,
        Onglet::Sons,
        Onglet::Beta,
        Onglet::Aide,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Onglet::Audio => "Audio",
            Onglet::Casque => "Casque",
            Onglet::Reseau => "Réseau & qualité",
            Onglet::Diffusion => "Diffusion d'écran",
            Onglet::Jeu => "Jeu",
            Onglet::Clips => "Clips",
            Onglet::Sons => "Sons & notifications",
            Onglet::Beta => "Bêta",
            Onglet::Aide => "Aide & diagnostics",
        }
    }

    /// La clé mémorisée : l'onglet ouvert revient à la session suivante —
    /// qui règle sa diffusion retombe dessus.
    pub(crate) fn cle(self) -> &'static str {
        match self {
            Onglet::Audio => "audio",
            Onglet::Casque => "casque",
            Onglet::Reseau => "reseau",
            Onglet::Diffusion => "diffusion",
            Onglet::Jeu => "jeu",
            Onglet::Clips => "clips",
            Onglet::Sons => "sons",
            Onglet::Beta => "beta",
            Onglet::Aide => "aide",
        }
    }

    pub(crate) fn depuis(cle: &str) -> Self {
        // L'overlay avait son onglet ; il est passé en Bêta.
        if cle == "overlay" {
            return Onglet::Beta;
        }
        Self::TOUS.into_iter().find(|o| o.cle() == cle).unwrap_or(Onglet::Audio)
    }
}

/// Ce que l'interface demande pour les réglages, exécuté par la logique.
pub(crate) enum DemandeReglages {
    /// Écrire l'export dans ce fichier.
    Exporter(std::path::PathBuf),
    /// Poser ces réglages, puis redémarrer.
    Importer(std::collections::BTreeMap<String, String>),
}

impl KiApp {
    /// Cet onglet de l'administration m'est-il ouvert ?
    pub(crate) fn onglet_ouvert(&self, tab: AdminTab) -> bool {
        // Claude : ses conversations et ses dépenses ne regardent que le
        // propriétaire — et il n'y a rien à montrer sans Claude.
        if tab == AdminTab::Claude && !(self.claude.servi && self.suis_proprietaire()) {
            return false;
        }
        tab.needs().iter().any(|p| self.can(*p))
    }

    /// Fenêtre de réglages, en onglets : audio (périphériques, micro,
    /// suppression de bruit, sortie), réseau & qualité, diffusion d'écran,
    /// overlay, sons, aide & diagnostics.
    pub(crate) fn settings_window(&mut self, ctx: &egui::Context, voice: &VoiceSnapshot) {
        let mut restart = false;
        let mut apply = false;
        let mut open = true;
        let engine_up = voice.engine_up;
        let stats = &voice.stats;
        // La calibration mesure là où la porte et le seuil jugent : avant le
        // gain automatique, qui ramènerait tout bruit « vivant » vers la cible.
        let niveau = stats.niveau_decision;
        self.tick_calibration(niveau);
        // Réglages ouverts : la décision de parole tourne même hors salon,
        // micro coupé compris — pour que les jauges vivent sans émettre.
        if let Some(e) = self.link.engine.lock().unwrap().as_ref() {
            e.set_decision_demandee(true);
        }

        // Hauteur d'ouverture : large sur un grand écran, sans jamais
        // déborder d'un petit. L'utilisateur peut ensuite redimensionner,
        // et egui retient la taille d'une session à l'autre.
        ki_ui::fenetre::Fenetre::new("reglages", "Réglages")
            .ouverte(&mut open)
            .largeur(520.0)
            .haute()
            .taille_min(340.0, 260.0)
            .show(ctx, |ui| {
                // Onglets : une seule colonne à faire défiler ne se lisait
                // plus une fois l'overlay, les sons et la diffusion venus se
                // ranger sous « audio ».
                let onglets: Vec<_> = Onglet::TOUS.into_iter().map(|o| (o, o.label())).collect();
                ui::onglets(ui, &mut self.reglages_onglet, &onglets);
                ui.add_space(espace::M);
                let onglet = self.reglages_onglet;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if onglet == Onglet::Audio {
                            self.onglet_audio(ui, voice, &mut apply, &mut restart);
                        }
                        if onglet == Onglet::Casque {
                            self.onglet_casque(ui, voice, &mut apply, &mut restart);
                        }
                        if onglet == Onglet::Beta {
                            self.onglet_beta(ui);
                        }
                        if onglet == Onglet::Aide {
                            self.reglages_a_emporter(ui);
                            // --- Journal audio ---
                            // Ce que l'audio a vécu (ouvertures, pertes, replis,
                            // réouvertures) : les bugs « au lancement d'un jeu »
                            // varient d'un casque à l'autre, et ce journal
                            // remplace la divination — on se le fait copier-coller.
                            ui.add_space(espace::M);
                            let label = if self.show_audio_journal {
                                "Masquer le journal audio"
                            } else {
                                "Journal audio"
                            };
                            if ui::Bouton::new(label).icone(Icon::Info).show(ui).clicked() {
                                self.show_audio_journal = !self.show_audio_journal;
                            }
                            if self.show_audio_journal {
                                let events = ki_voice::journal_snapshot();
                                if events.is_empty() {
                                    ui::hint(ui, "rien à signaler pour l'instant");
                                } else {
                                    if ui::Bouton::new("Copier le journal").icone(Icon::Copy).show(ui).clicked() {
                                        let text: String = events
                                            .iter()
                                            .map(|(ts, m)| {
                                                format!("{} {m}\n", format_time_secs(*ts))
                                            })
                                            .collect();
                                        ctx.copy_text(text);
                                        self.info = Some("journal audio copié".into());
                                    }
                                    ui.add_space(espace::XS);
                                    // Les plus récents d'abord : c'est eux qu'on
                                    // vient voir quand ça vient de buguer.
                                    for (ts, msg) in events.iter().rev().take(12) {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label(
                                                Texte::Aide.riche_ton(ui, Ton::Discret, format_time_secs(*ts)).monospace(),
                                            );
                                            ui.label(
                                                Texte::Aide.riche_ton(ui, Ton::Neutre, msg.as_str()),
                                            );
                                        });
                                    }
                                    if events.len() > 12 {
                                        ui::hint(
                                            ui,
                                            "le bouton copie l'intégralité du journal",
                                        );
                                    }
                                }
                            }

                            // --- Diagnostic partagé ---
                            // Le journal ci-dessus se copie à la main ; ici, il
                            // part tout seul vers le serveur — pour que l'admin
                            // (et l'assistant qui débogue avec lui) lise ce qui
                            // s'est passé sans rien demander à personne. Opt-in,
                            // et technique seulement : jamais les messages, jamais
                            // l'audio.
                            ui.add_space(espace::M);
                            ui.checkbox(
                                &mut self.diag.partage,
                                "Partager mes diagnostics avec l'admin du serveur",
                            )
                            .on_hover_text(
                                "envoie toutes les 10 minutes le journal technique \
                                 (périphériques, pertes, réouvertures, diffusion), la \
                                 version et le rapport du docteur au serveur du groupe. \
                                 Jamais tes messages, jamais ta voix. Décochable à tout \
                                 moment.",
                            );
                            ui::hint(
                                ui,
                                "les rapports de plantage (l'erreur et sa pile d'appels, rien \
                                 d'autre) partent toujours au serveur, option cochée ou non : \
                                 c'est ce qui permet de corriger un crash sans rien te demander",
                            );
                            if self.diag.partage
                                && ui::Bouton::new("Envoyer le diagnostic maintenant").icone(Icon::Send).show(ui)
                                    .clicked()
                            {
                                self.flush_diag(true);
                                self.info = Some("diagnostic envoyé au serveur".into());
                            }

                            // --- Docteur audio ---
                            // Le journal ci-dessus raconte ce qui s'est passé ;
                            // celui-ci dit ce qu'il faut FAIRE. Windows n'offre
                            // aucune API de « priorité micro » : quand un autre
                            // logiciel tient la voie de capture, on ne peut pas la
                            // lui reprendre — seulement le nommer, et dire le
                            // réglage qui rend la main.
                            ui.add_space(espace::M);
                            let label = if self.show_docteur {
                                "Masquer le docteur audio"
                            } else {
                                "Docteur audio — pourquoi mon micro bugue ?"
                            };
                            if ui::Bouton::new(label).icone(Icon::Info).show(ui).clicked() {
                                self.show_docteur = !self.show_docteur;
                                if self.show_docteur {
                                    // Établi au clic : l'énumération des processus
                                    // et la lecture du registre n'ont rien à faire
                                    // dans une boucle de rendu.
                                    self.docteur =
                                        self.link.engine.lock().unwrap().as_ref().map(|e| e.docteur());
                                }
                            }
                            if self.show_docteur {
                                match &self.docteur {
                                    None => ui::hint(
                                        ui,
                                        "le moteur audio n'est pas démarré — rejoins un \
                                         salon vocal, puis reviens",
                                    ),
                                    Some(d) => {
                                        if ui::Bouton::new("Copier le diagnostic").icone(Icon::Copy).show(ui)
                                            .clicked()
                                        {
                                            ctx.copy_text(d.rapport());
                                            self.info = Some("diagnostic copié".into());
                                        }
                                        ui.add_space(espace::XS);
                                        // Les conseils, numérotés, dans l'ordre où
                                        // ils valent la peine d'être essayés.
                                        for (i, conseil) in d.conseils().iter().enumerate() {
                                            ui.horizontal_wrapped(|ui| {
                                                ui.label(
                                                    Texte::Aide.gras_ton(ui, Ton::Accent, format!("{}.", i + 1)),
                                                );
                                                ui.label(
                                                    Texte::Aide.riche_ton(ui, Ton::Neutre, conseil.as_str()),
                                                );
                                            });
                                            ui.add_space(espace::XS);
                                        }
                                        ui::hint(
                                            ui,
                                            "ki-chat ne touche à aucun réglage système : il \
                                             te dit quoi regarder, tu décides.",
                                        );
                                    }
                                }
                            }

                            // --- Relevé de performance ---
                            // Même usage que le journal au-dessus : on se le fait
                            // copier-coller. « Ça rame quand je joue » ne se
                            // reproduit pas sur la machine de développement, et
                            // une moyenne noierait justement l'image lente qu'on
                            // vient chercher — d'où des quantiles.
                            ui.add_space(espace::M);
                            let label = if self.show_perf {
                                "Masquer le relevé de performance"
                            } else {
                                "Relevé de performance"
                            };
                            if ui::Bouton::new(label).icone(Icon::Info).show(ui).clicked() {
                                self.show_perf = !self.show_perf;
                            }
                            if self.show_perf {
                                let lignes = self.perf.lignes();
                                if ui::Bouton::new("Copier le relevé").icone(Icon::Copy).show(ui).clicked() {
                                    let mut texte = format!(
                                        "ki-chat {} — relevé de performance\n",
                                        env!("CARGO_PKG_VERSION")
                                    );
                                    for (quoi, valeur) in &lignes {
                                        texte.push_str(&format!("{quoi} : {valeur}\n"));
                                    }
                                    texte.push_str(&format!(
                                        "Trames incomplètes (audio) : {}\n\
                                         Carte son à sec (audio) : {}\n",
                                        voice.stats.underruns, voice.stats.sortie_a_sec
                                    ));
                                    // Le diagnostic voyage avec : celui qui reçoit
                                    // ce relevé n'a alors plus rien à demander.
                                    if let Some(d) = &self.docteur {
                                        texte.push('\n');
                                        texte.push_str(&d.rapport());
                                    }
                                    ctx.copy_text(texte);
                                    self.info = Some("relevé copié".into());
                                }
                                ui.add_space(espace::XS);
                                for (quoi, valeur) in &lignes {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(
                                            Texte::Aide.riche_ton(ui, Ton::Discret, quoi.as_str()),
                                        );
                                        ui.label(
                                            Texte::Aide.riche_ton(ui, Ton::Neutre, valeur.as_str()).monospace(),
                                        );
                                    });
                                }
                                // Les trous audio vivent dans le moteur, pas dans
                                // l'interface, mais c'est le même relevé pour qui
                                // le lit. Zéro est la seule bonne valeur — pour
                                // la voix arrivée trop tard comme pour la carte
                                // son à court, deux causes distinctes.
                                for (quoi, combien) in [
                                    ("Trames incomplètes (audio)", voice.stats.underruns),
                                    ("Carte son à sec (audio)", voice.stats.sortie_a_sec),
                                ] {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(Texte::Aide.riche_ton(ui, Ton::Discret, quoi));
                                        ui.label(
                                            RichText::new(combien.to_string())
                                                .color(if combien == 0 { SPEAK } else { DANGER })
                                                .monospace()
                                                .size(texte::PETIT),
                                        );
                                    });
                                }
                            }

                        }
                        if onglet == Onglet::Sons {
                            // --- Effets sonores ---
                            ui::group_title(ui, Icon::Play, "Sons & notifications");
                            // L'état du système, en clair : si un jour « pas de
                            // son », cette ligne dit immédiatement où chercher.
                            let engine_ok = self.link.engine.lock().unwrap().is_some();
                            ui::hint(
                                ui,
                                format!(
                                    "{} sons chargés ({} perso) · sortie audio {}",
                                    self.sounds.len(),
                                    self.custom_sfx.len(),
                                    if engine_ok { "active" } else { "INACTIVE — connecte-toi" },
                                ),
                            );
                            ui.checkbox(&mut self.reglages.sfx_on, "Jouer les sons");
                            if self.reglages.sfx_on {
                                let mut pct = self.reglages.sfx_volume * 100.0;
                                if ui
                                    .add(
                                        egui::Slider::new(&mut pct, 0.0..=100.0)
                                            .suffix(" %")
                                            .integer()
                                            .text("volume des sons"),
                                    )
                                    .changed()
                                {
                                    self.reglages.sfx_volume = pct / 100.0;
                                }
                                ui.add_space(espace::S);

                                // Un réglage par événement : à couper, à écouter,
                                // et l'étiquette dit si le son est personnalisé.
                                for (name, label) in [
                                    (sfx::MESSAGE, "Message reçu (fenêtre à l'arrière-plan)"),
                                    (sfx::MENTION, "On me nomme (@moi)"),
                                    (sfx::POKE, "Quelqu'un me poke"),
                                    (sfx::PORTE, "Quelqu'un frappe à une porte web"),
                                    (sfx::PEER_JOIN, "Quelqu'un arrive dans mon vocal"),
                                    (sfx::PEER_LEAVE, "Quelqu'un quitte mon vocal"),
                                    (sfx::DIFFUSION, "Quelqu'un de mon vocal lance un stream"),
                                    (sfx::CARTES_TOUR, "À moi de jouer aux cartes"),
                                    (sfx::CARTES_FIN, "Une manche de cartes est gagnée"),
                                    (sfx::CARTES_DERNIERE, "Quelqu'un dit « Dernière carte ! »"),
                                    (sfx::CARTES_CONTRE, "Quelqu'un se fait contrer aux cartes"),
                                    (sfx::CARTES_ECHANGE, "Des mains s'échangent aux cartes (le 7, le 0)"),
                                    (sfx::SELF_JOIN, "Je rejoins un vocal"),
                                    (sfx::SELF_LEAVE, "Je quitte le vocal"),
                                    (sfx::MUTE, "Micro coupé"),
                                    (sfx::UNMUTE, "Micro réactivé"),
                                ] {
                                    ui.horizontal(|ui| {
                                        let mut enabled = !self.reglages.sfx_muted.contains(name);
                                        if ui.checkbox(&mut enabled, label).changed() {
                                            if enabled {
                                                self.reglages.sfx_muted.remove(name);
                                            } else {
                                                self.reglages.sfx_muted.insert(name.to_string());
                                            }
                                        }
                                        if self.custom_sfx.contains(name) {
                                            ui.label(
                                                Texte::Minuscule.riche_ton(ui, Ton::Accent, "perso"),
                                            );
                                        }
                                        if ui.small_button("▶").on_hover_text("écouter").clicked()
                                        {
                                            // La préécoute ignore la coupure de
                                            // l'événement, pas le volume.
                                            if let Some(pcm) = self.sounds.get(name) {
                                                if let Some(engine) =
                                                    self.link.engine.lock().unwrap().as_ref()
                                                {
                                                    engine.play_effect(pcm, self.reglages.sfx_volume);
                                                }
                                            }
                                        }
                                    });
                                }
                                if self.session.conn.is_none() {
                                    ui::hint(ui, "connecte-toi pour la préécoute ▶");
                                }
                            }

                            ui.add_space(espace::S);
                            ui.horizontal(|ui| {
                                if ui::Bouton::new("Dossier des sons").icone(Icon::Paperclip).show(ui).clicked() {
                                    if let Some(dir) = sound_dirs().into_iter().nth(1) {
                                        let _ = std::fs::create_dir_all(&dir);
                                        let _ = std::process::Command::new("explorer")
                                            .arg(&dir)
                                            .spawn();
                                    }
                                }
                                if ui::Bouton::new("Recharger").icone(Icon::Refresh).show(ui).clicked() {
                                    self.reload_sounds();
                                }
                            });
                            ui::hint(
                                ui,
                                "les sons par défaut sont intégrés ; dépose des .wav (48 kHz \
                                 conseillé) nommés message, mention, poke, arrivee, depart, \
                                 rejoint-vocal, quitte-vocal, micro-coupe, micro-actif pour \
                                 les remplacer",
                            );

                            // --- Notifications ---
                            // Indépendant des sons : « rien » coupe aussi le
                            // clignotement, et « tout » ne rallume pas un son
                            // coupé plus haut. Les pastilles restent toujours.
                            ui.add_space(espace::L);
                            ui::group_title(ui, Icon::Info, "Notifications");
                            ui.horizontal(|ui| {
                                ui.label(Texte::Libelle.riche(ui, "Notifications :"));
                                for n in Notif::TOUS {
                                    ui.selectable_value(&mut self.reglages.notif, n, n.label());
                                }
                            });
                            ui::hint(
                                ui,
                                match self.reglages.notif {
                                    Notif::Tout => {
                                        "son et clignotement pour tout message reçu dans un \
                                         autre salon, ou fenêtre à l'arrière-plan"
                                    }
                                    Notif::Mentions => {
                                        "son et clignotement seulement quand on te nomme \
                                         (@toi) ; les autres messages ne font que poser une \
                                         pastille"
                                    }
                                    Notif::Rien => {
                                        "ni son ni clignotement ; les pastilles de non-lus \
                                         restent"
                                    }
                                },
                            );
                            // Le poke a son propre interrupteur : c'est un
                            // appel personnel, pas un message, et il passe
                            // outre « rien » ci-dessus. Le serveur refuse
                            // pour nous quand on le décoche.
                            ui.add_space(espace::S);
                            ui.checkbox(&mut self.reglages.pokes_accepter, "Accepter les pokes");
                            ui::hint(
                                ui,
                                "un membre peut te « poker » depuis la liste : un son et un \
                                 clignotement chez toi, jamais en vocal ni en partie ; \
                                 décoché, le serveur lui répond que tu n'en veux pas",
                            );

                            // --- Fermeture ---
                            ui.add_space(espace::L);
                            ui::group_title(ui, Icon::Info, "Fermeture");
                            ui.checkbox(
                                &mut self.reglages.reduire_zone,
                                "Réduire dans la zone de notification à la fermeture \
                                 (la croix ne quitte pas ki-chat)",
                            );
                            ui::hint(
                                ui,
                                "ki-chat continue à côté de l'horloge : messages, sons, \
                                 pokes et vocal ; clic sur l'icône pour rouvrir, « Quitter » \
                                 dans son menu pour partir vraiment",
                            );
                        }
                        if onglet == Onglet::Reseau {
                            // --- Réseau & qualité ---
                            ui::group_title(ui, Icon::Info, "Réseau & qualité");
                            ui.horizontal(|ui| {
                                ui.label(Texte::Libelle.riche(ui, "débit voix"));
                                let selected = if self.audio.bitrate == 0 {
                                    format!("Auto ({} kbps)", self.vocal.auto_bitrate / 1000)
                                } else {
                                    format!("{} kbps", self.audio.bitrate / 1000)
                                };
                                egui::ComboBox::from_id_salt("bitrate")
                                    .width(150.0)
                                    .selected_text(RichText::new(selected).color(TEXT))
                                    .show_ui(ui, |ui| {
                                        if ui.selectable_label(self.audio.bitrate == 0, "Auto").clicked() {
                                            self.audio.bitrate = 0;
                                            apply = true;
                                        }
                                        for br in BITRATES {
                                            if ui
                                                .selectable_label(
                                                    self.audio.bitrate == br,
                                                    format!("{} kbps", br / 1000),
                                                )
                                                .clicked()
                                            {
                                                self.audio.bitrate = br;
                                                apply = true;
                                            }
                                        }
                                    });
                            });
                            ui::hint(ui, "Auto : s'adapte aux pertes mesurées par le serveur.");
                            ui.add_space(espace::S);
                            ui.horizontal(|ui| {
                                ui.label(
                                    Texte::Libelle.riche(ui, "protection pertes (DRED)"),
                                );
                                let dred_label = |m: u8| match m {
                                    0 => "Désactivée".to_string(),
                                    2 => "Toujours".to_string(),
                                    _ => "Auto (recommandé)".to_string(),
                                };
                                egui::ComboBox::from_id_salt("dred_mode")
                                    .width(150.0)
                                    .selected_text(
                                        RichText::new(dred_label(self.audio.dred_mode)).color(TEXT),
                                    )
                                    .show_ui(ui, |ui| {
                                        for mode in [1u8, 0, 2] {
                                            if ui
                                                .selectable_label(
                                                    self.audio.dred_mode == mode,
                                                    dred_label(mode),
                                                )
                                                .clicked()
                                            {
                                                self.audio.dred_mode = mode;
                                                apply = true;
                                            }
                                        }
                                    });
                            });
                            ui::hint(
                                ui,
                                match self.audio.dred_mode {
                                    0 => "redondance neuronale coupée",
                                    2 => "≈1 s de voix re-transmise en continu dans chaque paquet",
                                    _ if self.vocal.dred_active => {
                                        "ENGAGÉE — pertes détectées, la voix est protégée"
                                    }
                                    _ => "en veille : s'engage automatiquement dès 2 % de pertes",
                                },
                            );
                            ui.add_space(espace::S);
                            ui.horizontal(|ui| {
                                ui.label(Texte::Libelle.riche(ui, "tampon de gigue"));
                                let jitter_label = |frames: usize| match frames {
                                    0 => "Auto (adaptatif)".to_string(),
                                    f => format!("{} ms", f * 20),
                                };
                                egui::ComboBox::from_id_salt("jitter")
                                    .width(150.0)
                                    .selected_text(
                                        RichText::new(jitter_label(self.audio.jitter_frames)).color(TEXT),
                                    )
                                    .show_ui(ui, |ui| {
                                        for frames in [0usize, 2, 3, 4, 6, 8] {
                                            if ui
                                                .selectable_label(
                                                    self.audio.jitter_frames == frames,
                                                    jitter_label(frames),
                                                )
                                                .clicked()
                                            {
                                                self.audio.jitter_frames = frames;
                                                apply = true;
                                            }
                                        }
                                    });
                            });
                            ui::hint(ui, "Auto recommandé ; fixe si la voix est hachée.");

                            if engine_up {
                                ui.add_space(espace::L);
                                let ping_txt = match voice.ping {
                                    Some(p) => format!("{p} ms"),
                                    None => "—".into(),
                                };
                                let upstream = match self.vocal.upstream_loss {
                                    Some(l) => format!(" · pertes montantes {l:.1} %"),
                                    None => String::new(),
                                };
                                ui::hint(
                                    ui,
                                    format!(
                                        "ping {ping_txt} · gigue max {:.1} ms · perdus {} \
                                     (récupérés FEC : {}) · rejetés {}{upstream}",
                                        stats.worst_jitter_ms,
                                        stats.packets_lost,
                                        stats.packets_recovered,
                                        stats.packets_rejected,
                                    ),
                                );
                            }

                        }
                        if onglet == Onglet::Diffusion {
                            // --- Diffusion d'écran ---
                            ui::group_title(ui, Icon::Screen, "Diffusion d'écran");
                            if self.diffusion.sources.perimees() {
                                self.diffusion.sources.rafraichir();
                            }
                            if partage::reglages_ui(ui, &mut self.diffusion.reglages, &mut self.diffusion.sources) {
                                self.rediffuser();
                            }

                            // --- Labo vidéo (S1a du partage d'écran) ---
                            ui.add_space(espace::L);
                            ui.separator();
                            ui.horizontal(|ui| {
                                ui.label(
                                    Texte::Libelle.riche(ui, "🧪 labo vidéo"),
                                );
                                let running = self.labo.is_some();
                                let label = if running { "Arrêter le test" } else { "Se voir (test local)" };
                                if ui.button(label).clicked() {
                                    if running {
                                        self.stop_labo();
                                    } else {
                                        self.start_labo(ctx.clone());
                                    }
                                }
                            });
                            ui::hint(
                                ui,
                                "capture ton écran principal et l'affiche après un aller-retour \
                                 H.264 complet — zéro réseau, c'est le banc d'essai du stream",
                            );
                        }
                        if onglet == Onglet::Clips {
                            self.reglages_clips_ui(ui);
                        }
                        if onglet == Onglet::Jeu {
                            ui::group_title(ui, Icon::Target, "Valorant");
                            ui.checkbox(&mut self.reglages.valorant_presence, "Partager mon activité Valorant")
                                .on_hover_text(
                                    "lu dans ton propre client Riot, sur ce PC, en lecture seule : \
                                     l'état de ta partie, la file, la carte, le score de ton équipe, \
                                     ta party. Rien sur les adversaires, jamais.",
                                );
                            ui::hint(
                                ui,
                                "les membres du serveur voient ta partie sous ton pseudo — \
                                 « compétitive · Ascent · 7-5 ». Décoche, et ça s'efface partout.",
                            );
                            let etat = match &self.veilleur_valorant {
                                None => "désactivé".to_string(),
                                Some(v) => match v.releve().1 {
                                    None => "actif — client Riot fermé, ou pas en jeu".to_string(),
                                    Some(j) => valo_catalogue::ligne(&j),
                                },
                            };
                            ui.label(Texte::Aide.riche_ton(ui, Ton::Discret, format!("état : {etat}")));
                            ui.add_space(espace::S);
                            if ui
                                .checkbox(&mut self.reglages.journal_parties, "Garder l'historique de mes parties (durée, mode, carte)")
                                .on_hover_text(
                                    "pour tes statistiques de demain : le temps de jeu par mode, par compte, par \
                                     soirée. Le serveur note chaque partie que ta présence montre — le début, la \
                                     durée, la file, la carte, le dernier score vu, la taille de la party et si \
                                     c'était ton compte principal. Ni Riot ID, ni adversaires.",
                                )
                                .changed()
                            {
                                self.reglages.journal_propose = true;
                            }
                            let pourquoi_pas = if !self.reglages.journal_parties {
                                Some("décoché, le serveur n'en garde rien — et efface ce qu'il avait")
                            } else if !self.reglages.valorant_presence {
                                Some("il faut aussi « Partager mon activité Valorant » : c'est elle qui montre les parties")
                            } else if self.session.welcomed && !self.serveur_journal {
                                Some("le serveur ne le tient pas encore : il doit être mis à jour")
                            } else {
                                None
                            };
                            if let Some(pourquoi) = pourquoi_pas {
                                ui::hint(ui, pourquoi);
                            }

                            // Le compte Riot : lié, le serveur tient la fiche
                            // par HenrikDev et la montre aux membres. Ce que le
                            // serveur en sait vient du roster, pas d'un état à
                            // part — délier chez l'admin s'y voit aussi.
                            ui.add_space(espace::L);
                            ui::group_title(ui, Icon::Target, "Compte Riot");
                            let moi = self.session
                                .my_id
                                .and_then(|id| self.session.members.iter().find(|m| m.user_id == id))
                                .map(|m| (m.user_id, m.username.clone(), m.riot_id.clone(), m.rang_valorant));
                            match moi {
                                Some((user_id, username, Some(riot), rang)) => {
                                    let rang = rang.filter(|t| *t >= 3).map(ki_protocol::nom_de_rang);
                                    let texte = match rang {
                                        Some(r) => format!("lié : {riot} · {r}"),
                                        None => format!("lié : {riot}"),
                                    };
                                    ui.label(RichText::new(texte).color(TEXT_DIM));
                                    ui::hint(
                                        ui,
                                        "le serveur garde ton rang, tes derniers RR et tes derniers matchs — ta ligne seulement — et les montre aux membres au clic droit sur ton pseudo.",
                                    );
                                    ui.horizontal(|ui| {
                                        if ui::Bouton::new("Ma fiche").icone(Icon::Screen).show(ui).clicked() {
                                            self.ouvrir_fiche(user_id, username.clone());
                                        }
                                        if ui::Bouton::new("Délier").icone(Icon::Close).show(ui).clicked() {
                                            self.send(ClientMsg::DelierRiot { user_id: None });
                                            self.riot_message = None;
                                        }
                                    });
                                    ui.add_space(espace::XS);
                                    ui.checkbox(&mut self.reglages.valorant_medailles, "Ajouter mes médailles à ma fiche").on_hover_text(
                                        "MVP, top frag, aces, clutchs, records de l'acte… Le serveur les lit \
                                         chez HenrikDev ; avec « Partager mon activité Valorant », ton client \
                                         Riot les donne dès la fin de chaque partie. Les tiennes seulement — \
                                         celles des autres joueurs du match ne partent jamais. Décoche, et \
                                         elles s'effacent de ta fiche.",
                                    );
                                    let pourquoi_pas = if !self.reglages.valorant_medailles || self.serveur_choix_medailles {
                                        // Un serveur qui les lit chez HenrikDev n'a
                                        // besoin de rien : la présence les avance.
                                        None
                                    } else if !self.reglages.valorant_presence {
                                        Some("il faut aussi « Partager mon activité Valorant » : c'est elle qui dit quand une partie finit")
                                    } else if !self.serveur_medailles {
                                        Some("le serveur ne les garde pas encore : il doit être mis à jour")
                                    } else {
                                        None
                                    };
                                    if let Some(pourquoi) = pourquoi_pas {
                                        ui::hint(ui, pourquoi);
                                    }
                                }
                                _ => {
                                    ui::hint(
                                        ui,
                                        "ton Riot ID, « Pseudo#TAG » : le serveur le cherche par HenrikDev et garde ton rang et tes derniers matchs. Rien n'est stocké sur les autres joueurs.",
                                    );
                                    ui.horizontal(|ui| {
                                        let champ = ui.add(
                                            egui::TextEdit::singleline(&mut self.riot_saisie)
                                                .margin(egui::Margin::symmetric(4, 5))
                                                .hint_text("Pseudo#TAG")
                                                .desired_width(200.0),
                                        );
                                        menu_edition(&champ, &mut self.riot_saisie, false);
                                        let entree = champ.lost_focus()
                                            && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                        let valide = ki_protocol::parser_riot_id(&self.riot_saisie).is_some();
                                        let clic = ui
                                            .add_enabled_ui(valide, |ui| ui::Bouton::new("Lier").icone(Icon::Check).show(ui))
                                            .inner
                                            .clicked();
                                        if (clic || entree) && valide {
                                            self.send(ClientMsg::LierRiot {
                                                riot_id: self.riot_saisie.trim().to_string(),
                                            });
                                            self.riot_message = Some((true, "recherche du compte…".into()));
                                        }
                                    });
                                }
                            }
                            if let Some((ok, message)) = &self.riot_message {
                                let teinte = if *ok { TEXT_DIM } else { DANGER };
                                ui.label(RichText::new(message).color(teinte).size(texte::PETIT));
                            }

                            ui.add_space(espace::M);
                            ui::hint(ui, "ta boutique du jour est dans la page Valorant, à côté de « Chercher »");

                            // Les autres jeux : un nom sous le pseudo, reconnu
                            // à la fenêtre — le contraire d'une lecture dans
                            // le jeu, et c'est pour ça qu'il est coché de base.
                            ui.add_space(espace::L);
                            ui::group_title(ui, Icon::Play, "Les autres jeux");
                            ui.checkbox(&mut self.reglages.presence_jeux, "Dire aux membres à quoi je joue")
                                .on_hover_text(
                                    "le jeu est reconnu à sa fenêtre — rien n'est lu dedans. Les \
                                     membres voient « joue à Rocket League » sous ton pseudo.",
                                );
                            let etat = match &self.veilleur_jeux {
                                None => "désactivé".to_string(),
                                Some(v) => match v.releve() {
                                    Some(nom) => format!("en jeu : {nom}"),
                                    None => "aucun jeu connu en cours".to_string(),
                                },
                            };
                            ui.label(Texte::Aide.riche_ton(ui, Ton::Discret, format!("état : {etat}")));
                            ui::hint(
                                ui,
                                format!(
                                    "{} jeux reconnus, de VALORANT à Dofus en passant par Rocket League — \
                                     un jeu qui manque s'ajoute d'une ligne dans le code (jeux.rs)",
                                    jeux::JEUX.len()
                                ),
                            );
                        }

                        if let Some(info) = self.info.clone() {
                            ui.add_space(espace::L);
                            if ui::bandeau_fermable(ui, Ton::Accent, &info) {
                                self.info = None;
                            }
                        }
                    });
            });

        if apply {
            self.apply_audio_settings();
        }
        if restart {
            {
                self.link.restart_voice(self.voice_prefs());
            }
        }
        if !open {
            self.close_settings();
        }
    }

    pub(crate) fn close_settings(&mut self) {
        self.show_settings = false;
        self.info = None;
        self.vocal.calibrage_micro = None;
        self.vocal.mesure_prox = None;
        if let Some(e) = self.link.engine.lock().unwrap().as_ref() {
            e.set_decision_demandee(false);
        }
        if self.vocal.eq_editeur.comparer {
            // Fermer la fenêtre en pleine comparaison ne doit pas laisser
            // l'égaliseur coupé.
            self.vocal.eq_editeur.comparer = false;
            self.apply_audio_settings();
        }
        if self.vocal.calibrating.take().is_some() || self.vocal.loopback {
            self.vocal.loopback = false;
            self.apply_audio_settings();
        }
        // Un enregistrement d'essai laissé en route rendrait le micro muet
        // pour le salon quelques secondes, sans rien à l'écran pour le dire.
        if let Some(e) = self.link.engine.lock().unwrap().as_ref() {
            e.arreter_essai();
        }
    }

    /// « Mes réglages » : les emporter dans un fichier, ou reprendre ceux
    /// d'un autre PC.
    pub(crate) fn reglages_a_emporter(&mut self, ui: &mut egui::Ui) {
        ui.add_space(espace::M);
        ui::section(
            ui,
            Icon::Download,
            "Mes réglages",
            Some("Pour changer de PC sans tout refaire : serveurs, raccourcis, égaliseurs, profils de voix, volumes."),
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui::Bouton::new("Exporter…").icone(Icon::Download).show(ui)
                        .on_hover_text("un fichier à garder ou à envoyer sur ton autre PC — sans aucun mot de passe")
                        .clicked()
                    {
                        if let Some(destination) =
                            rfd::FileDialog::new().set_file_name("ki-chat-reglages.json").add_filter("Réglages ki-chat", &["json"]).save_file()
                        {
                            self.reglages_demande = Some(DemandeReglages::Exporter(destination));
                        }
                    }
                    if ui::Bouton::new("Importer…").icone(Icon::ArrowUp).show(ui)
                        .on_hover_text("reprendre les réglages exportés depuis un autre PC")
                        .clicked()
                    {
                        if let Some(source) = rfd::FileDialog::new().add_filter("Réglages ki-chat", &["json"]).pick_file() {
                            let lu = std::fs::read_to_string(&source).map_err(|e| e.to_string()).and_then(|t| prefs::lire_export(&t));
                            match lu {
                                Ok(table) => {
                                    let nom = source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                                    self.import_propose = Some((table, nom));
                                }
                                Err(e) => self.error = Some(e),
                            }
                        }
                    }
                });
                ui::hint(
                    ui,
                    "Les mots de passe ne sont pas exportés, ni ce qui dépend du PC : micro et casque choisis, place de la fenêtre, dossier des clips.",
                );
                if let Some((table, nom)) = &self.import_propose {
                    ui.add_space(espace::S);
                    let serveurs = if self.book.is_empty() {
                        "Tes serveurs seront repris aussi, sans leurs mots de passe."
                    } else {
                        "Tes serveurs enregistrés ici ne changent pas."
                    };
                    ui.label(
                        Texte::Libelle.riche_ton(ui, Ton::Neutre, format!(
                            "{nom} : {} réglages vont remplacer ceux de ce PC. {serveurs} ki-chat redémarre ensuite.",
                            table.len()
                        )),
                    );
                    let mut choix = None;
                    ui.horizontal(|ui| {
                        if ui::Bouton::new("Importer et redémarrer").icone(Icon::Check).teinte(Ton::Accent).show(ui).clicked() {
                            choix = Some(true);
                        }
                        if ui::Bouton::new("Annuler").icone(Icon::Close).show(ui).clicked() {
                            choix = Some(false);
                        }
                    });
                    match choix {
                        Some(true) => {
                            if let Some((table, _)) = self.import_propose.take() {
                                self.reglages_demande = Some(DemandeReglages::Importer(table));
                            }
                        }
                        Some(false) => self.import_propose = None,
                        None => {}
                    }
                }
            },
        );
    }

    /// Exporte ou importe les réglages, là où leur stockage est à portée.
    pub(crate) fn traiter_reglages(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let Some(demande) = self.reglages_demande.take() else { return };
        let Some(stockage) = frame.storage_mut() else {
            self.error = Some("les réglages ne sont pas enregistrés sur ce poste : rien à exporter ni à importer".into());
            return;
        };
        match demande {
            DemandeReglages::Exporter(destination) => {
                // Ce que l'appli a en mémoire d'abord : le fichier peut avoir
                // trente secondes de retard. Et comme le stockage ne sait pas
                // dire ses clés, on note celles qui s'écrivent — toutes celles
                // de l'appli, modules compris, même jamais encore sur le disque.
                let mut temoin = prefs::Temoin::sur(stockage);
                self.sauver(&mut temoin);
                let mut cles = temoin.cles();
                cles.extend(prefs::fichier().and_then(|f| prefs::lire(f).ok()).unwrap_or_default().into_keys());
                let table: std::collections::BTreeMap<String, String> =
                    cles.into_iter().filter_map(|cle| stockage.get_string(&cle).map(|valeur| (cle, valeur))).collect();
                if table.is_empty() {
                    self.error = Some("rien à exporter pour l'instant : réessaie dans une minute".into());
                    return;
                }
                match std::fs::write(&destination, prefs::exporter(&table, update::current())) {
                    Ok(()) => self.info = Some(format!("réglages exportés dans {}", destination.display())),
                    Err(e) => self.error = Some(format!("export impossible : {e}")),
                }
            }
            DemandeReglages::Importer(importes) => {
                for (cle, valeur) in prefs::a_poser(importes, self.book.is_empty()) {
                    stockage.set_string(&cle, valeur);
                }
                // Tout est relu au démarrage : on redémarre, sans rien
                // réécrire entre-temps.
                self.reglages_importes = true;
                update::request_restart();
                self.quitter = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// Fenêtre de mise à jour : proposée, jamais imposée.
    ///
    /// Elle se pose par-dessus l'écran courant, quel qu'il soit — la sonde
    /// GitHub répond quand elle répond, et rien ne garantit qu'on soit encore
    /// sur le lanceur à ce moment-là.
    pub(crate) fn update_window(&mut self, ctx: &egui::Context) {
        let status = self.updater.status();
        if matches!(status, update::Status::Idle) {
            return;
        }

        // Titre figé : le changer d'un état à l'autre ferait repartir la
        // fenêtre au centre à chaque transition.
        ki_ui::fenetre::Fenetre::new("mise_a_jour", "Mise à jour")
            .fixe()
            .largeur(380.0)
            .ancree(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| match status {
                update::Status::Idle => {}
                update::Status::Available(release) => {
                    ui.label(
                        Texte::TitreSection.riche(ui, format!("Version {} disponible", release.version)),
                    );
                    ui::hint(ui, format!("Version installée : {}", update::current()));

                    if !release.notes.is_empty() {
                        ui.add_space(espace::M);
                        egui::ScrollArea::vertical()
                            .max_height(160.0)
                            .show(ui, |ui| {
                                ui.label(
                                    Texte::Corps.riche_ton(ui, Ton::Discret, release.notes.as_str()),
                                );
                            });
                    }

                    ui.add_space(espace::L);
                    ui.horizontal(|ui| {
                        if ui::Bouton::new("Mettre à jour").icone(Icon::ArrowDown).principal().show(ui)
                            .clicked()
                        {
                            self.updater.accept(&release, ctx.clone());
                        }
                        // Libellé franc : le refus est mémorisé, on ne
                        // redemandera qu'à la version suivante. « Plus tard »
                        // promettrait une relance qui n'aura pas lieu.
                        if ui::Bouton::new("Ignorer cette version").icone(Icon::Close).show(ui).clicked() {
                            self.updater.skip(&release.version);
                        }
                    });
                    ui::hint(ui, "L'application redémarrera d'elle-même une fois à jour.");
                }
                update::Status::Downloading { done, total } => {
                    ui.label(Texte::TitreSection.riche(ui, "Téléchargement…"));
                    ui.add_space(espace::M);
                    let ratio = if total > 0 {
                        done as f32 / total as f32
                    } else {
                        0.0
                    };
                    ui::meter(ui, ratio, Vec2::new(ui.available_width(), 8.0), theme::ACCENT);
                    ui::hint(
                        ui,
                        format!("{:.1} Mo sur {:.1} Mo", megabytes(done), megabytes(total)),
                    );
                }
                update::Status::Ready => {
                    ui.label(Texte::TitreSection.riche(ui, "Mise à jour installée"));
                    ui::hint(ui, "Redémarrage…");
                    // L'utilisateur a déjà dit oui : on ne lui redemande pas
                    // s'il veut bien relancer. La fenêtre se ferme, eframe
                    // enregistre, `main` relance.
                    update::request_restart();
                    // Ce Close-là doit passer, réglage « réduire » ou pas :
                    // sinon la mise à jour ne redémarre jamais.
                    self.quitter = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                update::Status::Failed(message) => {
                    ui.label(Texte::TitreSection.riche(ui, "Mise à jour impossible"));
                    ui.label(Texte::Corps.riche_ton(ui, Ton::Danger, message.as_str()));
                    ui.add_space(espace::M);
                    ui.hyperlink_to(
                        "Télécharger depuis GitHub",
                        update::Updater::releases_page(),
                    );
                    ui.add_space(espace::M);
                    if ui::Bouton::new("Fermer").icone(Icon::Close).show(ui).clicked() {
                        self.updater.dismiss();
                    }
                }
            });
    }

    /// Fenêtre « Mon compte » : changement de son propre mot de passe.
    pub(crate) fn account_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let mut to_send: Option<ClientMsg> = None;
        ki_ui::fenetre::Fenetre::new("mon_compte", "Mon compte")
            .ouverte(&mut open)
            .fixe()
            .largeur(300.0)
            .show(ctx, |ui| {
                // Photo rapportée par le thread du sélecteur de fichier.
                if let Some(outcome) = self.picked_avatar.lock().unwrap().take() {
                    match outcome {
                        Ok(data) => self.account_avatar = IconChange::Set { data },
                        Err(e) => self.error = Some(format!("image illisible : {e}")),
                    }
                }

                // Aperçu : la photo en attente d'envoi, sinon celle en place.
                let pending = match &self.account_avatar {
                    IconChange::Set { data } => Some(data.clone()),
                    IconChange::Clear => None,
                    IconChange::Keep => None,
                };
                let staged = self.preview_texture(ctx, Apercu::Avatar, pending.as_deref());
                let mine = self.session
                    .my_id
                    .and_then(|id| self.avatars.get(&id))
                    .map(|(_, texture)| texture.clone());
                let shown = match (&self.account_avatar, &staged) {
                    (IconChange::Set { .. }, Some(texture)) => Some(texture.clone()),
                    (IconChange::Clear, _) => None,
                    _ => mine.clone(),
                };

                ui.horizontal(|ui| {
                    let me = self.username.clone();
                    ui::Avatar::new(&me, 56.0).photo(shown.as_ref()).show(ui);
                    ui.add_space(espace::XS);
                    ui.vertical(|ui| {
                        ui.label(Texte::TitreSection.riche(ui, &me).color(color_for(&me)));
                        // Le rôle vaut mieux qu'un « administrateur/membre »
                        // binaire : c'est lui que les autres voient.
                        let role = self.session
                            .members
                            .iter()
                            .find(|m| Some(m.user_id) == self.session.my_id)
                            .and_then(|m| self.top_role_name(m))
                            .unwrap_or("membre")
                            .to_string();
                        ui.label(Texte::Libelle.riche_ton(ui, Ton::Pale, role));
                        ui.add_space(espace::S);
                        ui.horizontal(|ui| {
                            if ui::Bouton::new("Photo").icone(Icon::Pencil).show(ui).clicked() {
                                self.pick_image(ctx, "Photo de profil", &self.picked_avatar);
                            }
                            if shown.is_some()
                                && ui::Bouton::new("Retirer").icone(Icon::Trash).show(ui).clicked()
                            {
                                self.account_avatar = IconChange::Clear;
                            }
                        });
                    });
                });

                let dirty = !matches!(self.account_avatar, IconChange::Keep);
                if dirty {
                    ui.add_space(espace::M);
                    let clicked =
                        ui::Bouton::new("Enregistrer la photo").icone(Icon::Check).principal().show(ui)
                            .clicked();
                    if clicked {
                        to_send = Some(ClientMsg::SetAvatar {
                            avatar: std::mem::take(&mut self.account_avatar),
                        });
                        self.preview_icons.remove(&Apercu::Avatar);
                    }
                } else {
                    ui.add_space(espace::XS);
                    ui::hint(ui, "PNG ou JPG, réduit en vignette 64×64.");
                }

                ui.add_space(espace::L);
                ui::hairline(ui);
                ui.add_space(espace::L);

                ui::group_title(ui, Icon::Key, "Changer mon mot de passe");
                ui::field_label(ui, "Mot de passe actuel");
                ui.add(ui::champ_secret(&mut self.old_password, ""));
                ui.add_space(espace::M);
                ui::field_label(ui, "Nouveau mot de passe");
                ui.add(ui::champ_secret(&mut self.new_password, "6 caractères minimum"));
                if !self.new_password.is_empty() && self.new_password.len() < 6 {
                    ui.add_space(espace::XS);
                    ui::hint(ui, "6 caractères minimum");
                }
                ui.add_space(espace::L);
                let ready = !self.old_password.is_empty() && self.new_password.len() >= 6;
                let clicked = ui
                    .add_enabled_ui(ready, |ui| {
                        ui::Bouton::new("Changer le mot de passe")
                            .icone(Icon::Check)
                            .principal()
                            .largeur(ui.available_width())
                            .show(ui)
                    })
                    .inner
                    .clicked();
                if clicked {
                    to_send = Some(ClientMsg::ChangePassword {
                        old_password: self.old_password.clone(),
                        new_password: self.new_password.clone(),
                    });
                    self.old_password.clear();
                    self.new_password.clear();
                }
                if let Some(info) = self.info.clone() {
                    ui.add_space(espace::L);
                    if ui::bandeau_fermable(ui, Ton::Accent, &info) {
                        self.info = None;
                    }
                }
            });
        if let Some(msg) = to_send {
            self.send(msg);
        }
        if !open {
            self.close_account();
        }
    }

    pub(crate) fn close_account(&mut self) {
        self.show_account = false;
        self.old_password.clear();
        self.new_password.clear();
        self.info = None;
        self.account_avatar = IconChange::Keep;
        self.preview_icons.remove(&Apercu::Avatar);
    }
}
