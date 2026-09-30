//! L'onglet Audio des réglages.
//!
//! Rangé comme le son circule : ton micro, quand il s'ouvre, ce que ta voix
//! traverse avant de partir, puis ce que tu entends. Chaque section dit en une
//! phrase à quoi elle sert ; ce qui ne sert qu'au dépannage attend, replié, en
//! bas. Les choix courts se font en pastilles (tout se voit d'un coup), les
//! marches/arrêts par interrupteur.

use std::ops::RangeInclusive;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Pos2, RichText, Stroke, Vec2};

use crate::icons::Icon;
use crate::ptt::PttKey;
use crate::theme::{self, ACCENT, DANGER, INFO, SPEAK, TEXT, TEXT_DIM, TEXT_FAINT, WARN};
use crate::ui::{self, Tone};
use crate::{KiApp, MicMode, VoiceSnapshot};
use ki_voice::dynamique::{COMPRESSION_AUCUNE, COMPRESSION_DOUCE, COMPRESSION_FORTE, COMPRESSION_PERSO};
use ki_voice::egaliseur;
use ki_voice::spectre::{self, NB_TIERS, TIERS};
use ki_voice::{EtatEssai, VersionEssai, ESSAI_SECONDES};

/// Combien de temps l'alerte de saturation reste affichée après la dernière
/// trame saturée : assez pour la lire, et qu'elle ne clignote pas entre deux
/// phrases.
const ALERTE_SATURATION: Duration = Duration::from_secs(12);

impl KiApp {
    /// Le contenu de l'onglet. `apply` : un réglage du moteur a changé ;
    /// `restart` : il faut rouvrir les périphériques.
    pub(crate) fn onglet_audio(
        &mut self,
        ui: &mut egui::Ui,
        voice: &VoiceSnapshot,
        apply: &mut bool,
        restart: &mut bool,
    ) {
        let engine_up = voice.engine_up;
        let stats = &voice.stats;
        if stats.micro_sature {
            self.alerte_saturation = Some(Instant::now());
        }

        // --- Micro ----------------------------------------------------
        ui::section(
            ui,
            Icon::Mic,
            "Micro",
            Some("Ce que capte ton micro, et comment tu t'entends."),
            |ui| {
                ui::ligne(ui, "Périphérique", |ui| {
                    ui.horizontal(|ui| {
                        let devices = self.input_devices.clone();
                        *restart |= combo_peripherique(ui, "input_dev", &devices, &mut self.pref_input);
                        self.bouton_actualiser(ui);
                    });
                    if self.pref_input.is_none() {
                        ui::precision(
                            ui,
                            "Le défaut de Windows — pas celui de communication, que suit \
                             Discord. Choisis ton casque pour que ki-chat le prenne quoi que \
                             Windows désigne.",
                        );
                    }
                });

                ui::ligne(ui, "Niveau", |ui| {
                    ui.horizontal(|ui| {
                        let parle = if self.mode == MicMode::Vad {
                            if self.vad_neural {
                                stats.vad_prob >= self.vad_sens
                            } else {
                                stats.mic_peak >= self.vad_threshold
                            }
                        } else {
                            stats.mic_peak > 0.01
                        };
                        let couleur = if !engine_up {
                            theme::BG_ACTIVE
                        } else if stats.micro_sature {
                            DANGER
                        } else if parle {
                            SPEAK
                        } else {
                            TEXT_DIM
                        };
                        // En détection par seuil, le repère est le seuil ; en
                        // détection neuronale, il est sur la jauge de parole.
                        let repere = (self.mode == MicMode::Vad && !self.vad_neural)
                            .then(|| (self.vad_threshold * 3.0).min(1.0));
                        let largeur = (ui.available_width() - 80.0).clamp(120.0, 300.0);
                        ui::meter_with_threshold(
                            ui,
                            (stats.mic_peak * 3.0).min(1.0),
                            repere,
                            Vec2::new(largeur, 10.0),
                            couleur,
                        );
                        if !engine_up {
                            ui.label(RichText::new("vocal inactif").color(WARN).size(11.5));
                        } else if stats.micro_sature {
                            ui.label(RichText::new("saturé").color(DANGER).size(11.5).strong());
                        }
                    });
                });

                // Le micro qui sature à la source : ce que sa carte son a
                // coupé ne se rattrape plus, ni par la compression, ni par le
                // gain. Dit clairement, avec le chemin du remède.
                if self.alerte_saturation.is_some_and(|t| t.elapsed() < ALERTE_SATURATION) {
                    ui::banner(
                        ui,
                        Tone::Warn,
                        "Ton micro sature avant même d'arriver dans ki-chat : quand tu parles \
                         fort, sa carte son coupe le haut de ta voix. Baisse son niveau dans \
                         Windows (ou sa molette sur le casque) jusqu'à ce que « saturé » ne \
                         s'allume plus — le gain automatique remontera le reste.",
                        false,
                    );
                    if cfg!(windows) {
                        ui.add_space(6.0);
                        if ui::button(ui, Icon::Gear, "Ouvrir le réglage du micro dans Windows")
                            .clicked()
                        {
                            ouvrir_reglage_micro_windows();
                        }
                    }
                    ui.add_space(10.0);
                }

                ui::ligne(ui, "Tester", |ui| {
                    if ui::interrupteur(ui, &mut self.loopback, "M'écouter").changed() {
                        *apply = true;
                        if self.loopback {
                            // Même tampon de lecture : l'un chasse l'autre.
                            if let Some(e) = self.link.engine.lock().unwrap().as_ref() {
                                e.arreter_essai();
                            }
                        }
                    }
                    if self.loopback {
                        ui.label(
                            RichText::new("les autres ne t'entendent pas pendant l'essai")
                                .color(WARN)
                                .size(11.5),
                        );
                    }
                    ui::precision(
                        ui,
                        "En direct, traitement et codec compris — avec un peu de retard. Sous \
                         un casque fermé, ta voix t'arrive aussi par les os du crâne : ce \
                         retard la double et la fait paraître plus creuse qu'elle n'est. L'essai \
                         est privé — rien ne part vers le salon tant qu'il tourne.",
                    );
                    ui.add_space(8.0);
                    self.essai_voix_ui(ui, engine_up);
                });
            },
        );

        // --- Prise de parole ------------------------------------------
        ui::section(
            ui,
            Icon::Chat,
            "Prise de parole",
            Some("Quand ton micro s'ouvre pour les autres."),
            |ui| {
                ui::ligne(ui, "Mode", |ui| {
                    if ui::segmente(
                        ui,
                        &mut self.mode,
                        &[
                            (MicMode::Open, "Ouvert"),
                            (MicMode::Ptt, "Push-to-talk"),
                            (MicMode::Vad, "À la voix"),
                        ],
                    ) {
                        *apply = true;
                    }
                    ui::precision(
                        ui,
                        match self.mode {
                            MicMode::Open => "Ton micro émet en permanence.",
                            MicMode::Ptt => "Ton micro s'ouvre tant que tu tiens la touche.",
                            MicMode::Vad => "Ton micro s'ouvre quand tu parles.",
                        },
                    );
                });
                match self.mode {
                    MicMode::Ptt => {
                        ui::ligne(ui, "Touche", |ui| {
                            egui::ComboBox::from_id_salt("reglages_ptt_key")
                                .width(160.0)
                                .selected_text(RichText::new(self.ptt_key.label()).color(TEXT))
                                .show_ui(ui, |ui| {
                                    for key in PttKey::ALL {
                                        ui.selectable_value(&mut self.ptt_key, key, key.label());
                                    }
                                });
                        });
                        ui::ligne(ui, "Relâchement", |ui| {
                            let mut ms = self.ptt_release_ms as f32;
                            if curseur(ui, &mut ms, 0.0..=500.0, " ms", Some(25.0)) {
                                self.ptt_release_ms = ms as u32;
                            }
                            ui::precision(ui, "Le micro reste ouvert un instant après la touche.");
                        });
                    }
                    MicMode::Vad => {
                        ui::ligne(ui, "Détection", |ui| {
                            if ui::interrupteur(ui, &mut self.vad_neural, "Neuronale (Silero)")
                                .changed()
                            {
                                *apply = true;
                            }
                            ui::precision(
                                ui,
                                "Un clavier, une respiration ou un souffle ne sont plus pris \
                                 pour une voix.",
                            );
                        });
                        if self.vad_neural {
                            ui::ligne(ui, "Sensibilité", |ui| {
                                let mut pct = self.vad_sens * 100.0;
                                if curseur(ui, &mut pct, 20.0..=90.0, " %", Some(1.0)) {
                                    self.vad_sens = pct / 100.0;
                                    *apply = true;
                                }
                                ui.add_space(4.0);
                                let prob = stats.vad_prob;
                                let largeur = (ui.available_width() - 20.0).clamp(120.0, 300.0);
                                ui::meter_with_threshold(
                                    ui,
                                    prob,
                                    Some(self.vad_sens),
                                    Vec2::new(largeur, 8.0),
                                    if engine_up && prob >= self.vad_sens { SPEAK } else { TEXT_DIM },
                                );
                                ui::precision(
                                    ui,
                                    "Parle : la jauge doit passer le repère quand ta voix passe.",
                                );
                            });
                        } else {
                            ui::ligne(ui, "Seuil", |ui| {
                                let mut pct = self.vad_threshold * 100.0;
                                if curseur(ui, &mut pct, 0.5..=25.0, " %", None) {
                                    self.vad_threshold = pct / 100.0;
                                    *apply = true;
                                }
                                ui::precision(ui, "Le repère orange sur la jauge du micro.");
                            });
                        }
                        ui::ligne(ui, "Maintien", |ui| {
                            let mut ms = self.vad_hangover_ms as f32;
                            if curseur(ui, &mut ms, 100.0..=1000.0, " ms", Some(50.0)) {
                                self.vad_hangover_ms = ms as u32;
                                *apply = true;
                            }
                            ui::precision(ui, "Le micro reste ouvert après ta dernière syllabe.");
                        });
                    }
                    MicMode::Open => {}
                }
                ui::ligne(ui, "Raccourcis", |ui| {
                    let touche_ptt = (self.mode == MicMode::Ptt).then_some(self.ptt_key);
                    for (intitule, salt, choix) in [
                        ("Couper le micro", "hotkey_micro", &mut self.hotkey_micro),
                        ("Me rendre sourd", "hotkey_sourd", &mut self.hotkey_sourd),
                    ] {
                        ui.horizontal(|ui| {
                            ui.allocate_ui_with_layout(
                                Vec2::new(118.0, 22.0),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.set_min_width(118.0);
                                    ui.label(RichText::new(intitule).color(TEXT).size(12.5));
                                },
                            );
                            let actuel = choix.map(|k| k.label()).unwrap_or("aucun");
                            egui::ComboBox::from_id_salt(salt)
                                .width(120.0)
                                .selected_text(RichText::new(actuel).color(TEXT))
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(choix, None, "aucun");
                                    for key in PttKey::ALL {
                                        ui.selectable_value(choix, Some(key), key.label());
                                    }
                                });
                            if choix.is_some() && *choix == touche_ptt {
                                ui.label(
                                    RichText::new("c'est la touche du push-to-talk")
                                        .color(WARN)
                                        .size(11.5),
                                );
                            }
                        });
                        ui.add_space(4.0);
                    }
                    ui::precision(
                        ui,
                        "Ils marchent même en jeu, fenêtre au second plan : une pression \
                         bascule.",
                    );
                });
            },
        );

        // --- Traitement de la voix ------------------------------------
        ui::section(
            ui,
            Icon::Sliders,
            "Traitement de la voix",
            Some(
                "Dans l'ordre où ta voix le traverse avant de partir. En bout de chaîne, un \
                 limiteur, toujours là, empêche toute saturation.",
            ),
            |ui| {
                ui::ligne(ui, "Suppression de bruit", |ui| {
                    if ui::segmente(
                        ui,
                        &mut self.noise_mode,
                        &[
                            (ki_voice::NOISE_OFF, "Aucune"),
                            (ki_voice::NOISE_RNNOISE, "Légère"),
                            (ki_voice::NOISE_DEEP, "Studio"),
                        ],
                    ) {
                        *apply = true;
                    }
                    ui::precision(
                        ui,
                        match self.noise_mode {
                            ki_voice::NOISE_DEEP => {
                                "DeepFilterNet3 : clavier, ventilo et fond sonore effacés, \
                                 100 % local, 30 ms de plus."
                            }
                            ki_voice::NOISE_OFF => "Ta voix telle que le micro la capte.",
                            _ => "RNNoise : le souffle et les bruits continus, pour presque rien.",
                        },
                    );
                });

                ui::ligne(ui, "Compression", |ui| {
                    if ui::segmente(
                        ui,
                        &mut self.compression,
                        &[
                            (COMPRESSION_AUCUNE, "Aucune"),
                            (COMPRESSION_DOUCE, "Douce"),
                            (COMPRESSION_FORTE, "Forte"),
                            (COMPRESSION_PERSO, "Perso"),
                        ],
                    ) {
                        *apply = true;
                    }
                    ui::precision(
                        ui,
                        match self.compression {
                            COMPRESSION_FORTE => {
                                "Tient toute ta voix : pour qui parle fort ou crie souvent."
                            }
                            COMPRESSION_PERSO => {
                                "Réglée au chiffre près dans l'onglet Casque, mode studio."
                            }
                            COMPRESSION_AUCUNE => {
                                "Ta voix garde toute sa dynamique ; le limiteur empêche quand \
                                 même la saturation."
                            }
                            _ => {
                                "Ne touche qu'aux éclats : quand tu cries, ta voix reste propre \
                                 chez les autres."
                            }
                        },
                    );
                });

                ui::ligne(ui, "Gain automatique", |ui| {
                    if ui::interrupteur(ui, &mut self.agc, "Normaliser ma voix").changed() {
                        *apply = true;
                    }
                    if self.agc {
                        ui.add_space(6.0);
                        let mut pct = self.agc_target * 100.0;
                        if curseur(ui, &mut pct, 15.0..=50.0, " %", Some(1.0)) {
                            self.agc_target = pct / 100.0;
                            *apply = true;
                        }
                        ui::precision(ui, "Le niveau que vise ta voix, qu'elle soit douce ou forte.");
                    }
                });

                ui::ligne(ui, if self.agc { "Pré-ampli" } else { "Gain d'entrée" }, |ui| {
                    let mut pct = self.input_gain * 100.0;
                    if curseur(ui, &mut pct, 0.0..=200.0, " %", Some(1.0)) {
                        self.input_gain = pct / 100.0;
                        *apply = true;
                    }
                });

                ui::ligne(ui, "Porte de bruit", |ui| {
                    let mut pct = self.gate_threshold * 100.0;
                    if curseur(ui, &mut pct, 0.0..=10.0, " %", None) {
                        self.gate_threshold = pct / 100.0;
                        *apply = true;
                    }
                    ui::precision(ui, "Coupe ce qui reste sous ce niveau ; 0 % la désactive.");
                    ui.add_space(4.0);
                    self.calibration_ui(ui, engine_up);
                });

                ui::ligne(ui, "Écho", |ui| {
                    if ui::interrupteur(ui, &mut self.aec_on, "Annulation d'écho").changed() {
                        *apply = true;
                    }
                    ui::precision(
                        ui,
                        "Indispensable sur haut-parleurs : les autres ne s'entendent plus \
                         revenir. Sans effet notable au casque.",
                    );
                });
            },
        );

        // --- Ce que tu entends ----------------------------------------
        ui::section(
            ui,
            Icon::Headphones,
            "Ce que tu entends",
            Some("La voix des autres, et tout ce que ki-chat te joue."),
            |ui| {
                ui::ligne(ui, "Périphérique", |ui| {
                    ui.horizontal(|ui| {
                        let devices = self.output_devices.clone();
                        *restart |= combo_peripherique(ui, "output_dev", &devices, &mut self.pref_output);
                        self.bouton_actualiser(ui);
                    });
                });
                ui::ligne(ui, "Volume", |ui| {
                    let mut pct = self.output_gain * 100.0;
                    if curseur(ui, &mut pct, 0.0..=200.0, " %", Some(1.0)) {
                        self.output_gain = pct / 100.0;
                        *apply = true;
                    }
                });
                ui::ligne(ui, "Cris", |ui| {
                    if ui::interrupteur(ui, &mut self.adoucir_cris, "Adoucir les cris des autres")
                        .changed()
                    {
                        *apply = true;
                    }
                    ui::precision(
                        ui,
                        "Celui qui hurle dans son micro redescend, les autres ne bougent pas. \
                         La musique du bot n'est pas touchée.",
                    );
                });
                ui::ligne(ui, "Tester", |ui| {
                    if ui::button(ui, Icon::Play, "Jouer un son de test").clicked() {
                        if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
                            engine.play_test_tone();
                        }
                    }
                });
            },
        );

        // --- Avancé ---------------------------------------------------
        egui::CollapsingHeader::new(
            RichText::new("Avancé — moteur et dépannage").color(TEXT_DIM).size(13.0),
        )
        .id_salt("reglages_audio_avance")
        .default_open(false)
        .show(ui, |ui| {
            ui.add_space(6.0);
            if ui::button(ui, Icon::Refresh, "Réinitialiser l'audio").clicked() {
                if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
                    engine.reset_audio_devices();
                    self.info = Some(
                        "micro et sortie rouverts — comme un débranchement/rebranchement du \
                         casque"
                            .into(),
                    );
                }
            }
            ui::precision(
                ui,
                "Si le son part en vrille quand un jeu se lance ou se ferme : rouvre tout, \
                 sans toucher au câble.",
            );
            if cfg!(windows) {
                ui.add_space(12.0);
                if ui::interrupteur(ui, &mut self.native_audio, "Moteur audio natif (recommandé)")
                    .changed()
                {
                    *restart = true;
                }
                ui::precision(
                    ui,
                    "Parle à Windows sans intermédiaire : survit aux jeux qui changent le \
                     format audio. À décocher seulement si le son se comporte moins bien \
                     qu'avant.",
                );
                if self.native_audio {
                    ui.add_space(10.0);
                    if ui::interrupteur(ui, &mut self.raw_mic, "Micro brut").changed() {
                        *restart = true;
                    }
                    ui::precision(
                        ui,
                        "Court-circuite les effets du casque (Sonar, Nahimic, Synapse…) sur \
                         le micro. À essayer si le micro bugue quand un jeu se lance.",
                    );
                    ui.add_space(10.0);
                    if ui::interrupteur(ui, &mut self.comms_mic, "Partager le micro avec la voix du jeu")
                        .changed()
                    {
                        *restart = true;
                    }
                    ui::precision(
                        ui,
                        "Nécessaire quand la voix intégrée d'un jeu affame le micro. Revers : \
                         Windows peut baisser les autres sons pendant le vocal (Panneau son → \
                         Communications → « Ne rien faire »).",
                    );
                    ui.add_space(10.0);
                    if ui::interrupteur(ui, &mut self.robust_output, "Sortie audio robuste")
                        .changed()
                    {
                        *restart = true;
                    }
                    ui::precision(
                        ui,
                        "Plus de marge, 70 ms de latence en plus : seulement si le docteur \
                         audio trouve la carte son à sec.",
                    );
                }
            }
        });
    }

    /// Le bouton qui relit la liste des périphériques.
    fn bouton_actualiser(&mut self, ui: &mut egui::Ui) {
        if ui::icon_button(ui, Icon::Refresh, "Actualiser la liste des périphériques").clicked() {
            let (inputs, outputs) = ki_voice::list_devices();
            self.input_devices = inputs;
            self.output_devices = outputs;
        }
    }

    /// « Enregistrer et réécouter » : quelques secondes de sa voix, rejouées
    /// d'une traite — ce que les autres reçoivent, puis son micro brut au
    /// même volume. Sans le retard de « M'écouter », on juge sa voix telle
    /// qu'elle est.
    pub(crate) fn essai_voix_ui(&mut self, ui: &mut egui::Ui, engine_up: bool) {
        enum Geste {
            Enregistrer,
            Rejouer(VersionEssai),
            Arreter,
        }
        let etat = if engine_up {
            self.link.engine.lock().unwrap().as_ref().map(|e| e.essai())
        } else {
            None
        };
        let Some(etat) = etat else {
            ui::precision(ui, "L'enregistrement d'essai se fait connecté à un serveur.");
            return;
        };
        let mut geste = None;
        match etat {
            EtatEssai::Vide => {
                if ui::button(ui, Icon::Mic, &format!("Enregistrer {ESSAI_SECONDES} s et réécouter"))
                    .clicked()
                {
                    geste = Some(Geste::Enregistrer);
                }
            }
            EtatEssai::Enregistre(avancement) => {
                ui.ctx().request_repaint_after(Duration::from_millis(50));
                ui.horizontal(|ui| {
                    ui::meter(ui, avancement, Vec2::new(150.0, 8.0), DANGER);
                    let reste = ((1.0 - avancement) * ESSAI_SECONDES as f32).ceil().max(1.0);
                    ui.label(
                        RichText::new(format!("parle comme en partie… {reste:.0} s"))
                            .color(TEXT_DIM)
                            .size(12.0),
                    );
                    if ui::icon_button(ui, Icon::Close, "Annuler").clicked() {
                        geste = Some(Geste::Arreter);
                    }
                });
            }
            EtatEssai::Pret { lecture, ecart_db, numero } => {
                if lecture.is_some() {
                    ui.ctx().request_repaint_after(Duration::from_millis(50));
                }
                ui.horizontal_wrapped(|ui| {
                    for (version, libelle) in [
                        (VersionEssai::Envoyee, "Ce que les autres entendent"),
                        (VersionEssai::Brute, "Ton micro brut"),
                    ] {
                        match lecture {
                            Some((v, avancement)) if v == version => {
                                if ui::button(ui, Icon::Pause, libelle).clicked() {
                                    geste = Some(Geste::Arreter);
                                }
                                ui::meter(ui, avancement, Vec2::new(60.0, 6.0), ACCENT);
                            }
                            _ => {
                                if ui::button(ui, Icon::Play, libelle).clicked() {
                                    geste = Some(Geste::Rejouer(version));
                                }
                            }
                        }
                    }
                    if ui::icon_button(ui, Icon::Repeat, "Recommencer l'enregistrement").clicked() {
                        geste = Some(Geste::Enregistrer);
                    }
                });
                self.volume_de_l_essai(ui, ecart_db);
                self.clarte_de_l_essai(ui, numero);
            }
        }
        ui::precision(
            ui,
            &format!(
                "Parle {ESSAI_SECONDES} s comme en partie : ki-chat te rejoue ce que les autres \
                 reçoivent, sans retard. Puis compare à ton micro brut, remis au même volume — \
                 ce qui sonne creux dans le premier et pas dans le second vient des réglages. \
                 Personne ne t'entend pendant l'enregistrement."
            ),
        );
        match geste {
            Some(Geste::Enregistrer) => {
                if self.loopback {
                    self.loopback = false;
                    self.apply_audio_settings();
                }
                if let Some(e) = self.link.engine.lock().unwrap().as_ref() {
                    e.enregistrer_essai();
                }
            }
            Some(Geste::Rejouer(version)) => {
                if let Some(e) = self.link.engine.lock().unwrap().as_ref() {
                    e.rejouer_essai(version);
                }
            }
            Some(Geste::Arreter) => {
                if let Some(e) = self.link.engine.lock().unwrap().as_ref() {
                    e.arreter_essai();
                }
            }
            None => {}
        }
    }

    /// La clarté de sa voix d'après l'essai : ses aigus et ses graves face à
    /// une voix moyenne, bruts et envoyés, ce qui retire les aigus — le
    /// micro, l'égaliseur, ou le reste de la chaîne —, et de quoi corriger.
    /// L'essai est gardé en WAV au passage.
    fn clarte_de_l_essai(&mut self, ui: &mut egui::Ui, numero: u64) {
        let (analyse, pcm) = {
            let moteur = self.link.engine.lock().unwrap();
            let Some(e) = moteur.as_ref() else { return };
            let pcm = if numero != self.essai_garde { e.essai_pcm() } else { None };
            (e.essai_analyse(), pcm)
        };
        if let Some((brute, envoyee)) = pcm {
            self.essai_garde = numero;
            self.garder_essai(brute, envoyee);
        }
        let Some(analyse) = analyse else { return };
        let (Some(brute), Some(envoyee)) = (&analyse.brute, &analyse.envoyee) else {
            ui::precision(ui, "Pas assez de voix pour juger de ta clarté : parle pendant les 5 secondes.");
            return;
        };
        ui.add_space(6.0);
        courbe_de_voix(ui, &brute.ecarts_db(), &envoyee.ecarts_db());
        let (presence, graves) = (envoyee.presence_db(), envoyee.graves_db());
        let (mot, couleur) = if presence >= -5.0 {
            ("claire", SPEAK)
        } else if presence >= -10.0 {
            ("un peu sourde", WARN)
        } else {
            ("étouffée", DANGER)
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Ta voix chez les autres :").color(TEXT_DIM).size(12.0));
            ui.label(RichText::new(mot).color(couleur).size(12.0).strong());
            ui.label(
                RichText::new(format!(
                    "aigus {presence:+.0} dB, graves {graves:+.0} dB par rapport à une voix moyenne"
                ))
                .color(TEXT_DIM)
                .size(12.0),
            );
        });
        // Qui retire les aigus : le micro, l'égaliseur, ou le reste.
        let avant_eq = brute.presence_db();
        let apres_eq = brute.filtre(|f| egaliseur::reponse_db(&analyse.egaliseur, f)).presence_db();
        let (par_eq, par_chaine) = (apres_eq - avant_eq, presence - apres_eq);
        if presence < -5.0 {
            if avant_eq < -5.0 {
                ui::precision(
                    ui,
                    &format!(
                        "Ton micro capte déjà peu d'aigus ({avant_eq:+.0} dB) : ça se joue avant \
                         ki-chat — sa position (la capsule au coin de la bouche, pas sous le \
                         menton), sa mousse, ou l'entrée micro de ta carte son."
                    ),
                );
            }
            if par_eq < -1.5 {
                ui::precision(ui, &format!("Ton égaliseur en retire {:.0} dB.", -par_eq));
            }
            if par_chaine < -3.0 {
                ui::precision(
                    ui,
                    &format!(
                        "Le débruitage ou le codec en retirent {:.0} dB de plus : essaie un autre \
                         débruitage, section Traitement.",
                        -par_chaine
                    ),
                );
            }
        }
        // Le grondement, de 100 à 160 Hz : l'effet de proximité d'un micro
        // tout près de la bouche.
        let grondement = envoyee.ecarts_db()[..3].iter().fold(f32::MIN, |m, e| m.max(*e));
        if grondement > 6.0 {
            ui::precision(
                ui,
                &format!("Du grondement ({grondement:+.0} dB vers 100 Hz) : c'est ce qui fait « caverneux »."),
            );
        }
        // La correction : réglée sur ce qui part, l'égaliseur actuel ôté —
        // elle compense aussi ce que le débruitage et le codec retirent.
        let sans_eq = envoyee.filtre(|f| -egaliseur::reponse_db(&analyse.egaliseur, f));
        let correctif = spectre::egaliseur_correctif(&sans_eq);
        let a_corriger = correctif.iter().any(|b| b.forme != egaliseur::Forme::PasseHaut)
            && (presence < spectre::PRESENCE_VISEE_DB - 2.0 || grondement > spectre::GRAVES_VISES_DB + 4.0);
        if self.egaliseur_micro == correctif {
            ui::precision(
                ui,
                "L'égaliseur de ta voix est réglé sur cette mesure : refais un essai pour \
                 l'entendre.",
            );
        } else if a_corriger {
            ui.add_space(4.0);
            if ui::button(ui, Icon::Sliders, "Corriger ma voix")
                .on_hover_text(
                    "règle l'égaliseur de ta voix sur cette mesure, à la place du réglage actuel",
                )
                .clicked()
            {
                self.egaliseur_micro = correctif.clone();
                self.apply_audio_settings();
            }
            ui::precision(ui, &format!("L'égaliseur de ta voix deviendra : {}.", decrire(&correctif)));
        }
        if let Some(dossier) = self.essai_dossier.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("Les deux versions sont gardées en WAV, remplacées au prochain essai.")
                        .color(TEXT_FAINT)
                        .size(11.5),
                );
                if ui::icon_button(ui, Icon::Download, "Ouvrir le dossier").clicked() {
                    crate::soundboard::ouvrir_dossier(&dossier);
                }
            });
        }
    }

    /// Garde les deux versions de l'essai en WAV dans le dossier de ki-chat,
    /// sur un fil : quelques mégaoctets à écrire, rien pour l'interface.
    fn garder_essai(&mut self, brute: Vec<f32>, envoyee: Vec<f32>) {
        let Some(dossier) = eframe::storage_dir("ki-chat").map(|d| d.join("essais")) else { return };
        self.essai_dossier = Some(dossier.clone());
        std::thread::spawn(move || {
            let ecrire = || -> anyhow::Result<()> {
                std::fs::create_dir_all(&dossier)?;
                ki_voice::effects::ecrire_wav(dossier.join("essai-brut.wav"), &brute)?;
                ki_voice::effects::ecrire_wav(dossier.join("essai-envoye.wav"), &envoyee)?;
                Ok(())
            };
            if let Err(e) = ecrire() {
                tracing::warn!("essai non gardé en WAV : {e:#}");
            }
        });
    }

    /// La calibration de la porte (et du seuil d'activation) sur le bruit de
    /// la pièce : le bouton, ou sa progression.
    fn calibration_ui(&mut self, ui: &mut egui::Ui, engine_up: bool) {
        match self.calibrating {
            None => {
                if engine_up
                    && ui::button(ui, Icon::Target, "Régler sur le bruit de la pièce (5 s)")
                        .on_hover_text(
                            "reste silencieux : la porte (et le seuil d'activation) se posent \
                             juste au-dessus du bruit ambiant",
                        )
                        .clicked()
                {
                    self.start_calibration();
                }
            }
            Some((start, peak)) => {
                let progression = (start.elapsed().as_secs_f32() / 5.0).min(1.0);
                ui.horizontal(|ui| {
                    ui::meter(ui, progression, Vec2::new(150.0, 8.0), ACCENT);
                    ui.label(
                        RichText::new(format!("chut… ambiance {:.1} %", peak * 100.0))
                            .color(TEXT_DIM)
                            .size(12.0),
                    );
                    if ui::icon_button(ui, Icon::Close, "Annuler").clicked() {
                        self.calibrating = None;
                        self.apply_audio_settings();
                    }
                });
            }
        }
    }
}

/// Le timbre de sa voix face à une voix moyenne (la ligne du milieu) : le
/// micro brut en bleu, ce qui part en vert. Les aigus — la zone qui dit si
/// la voix est claire ou étouffée — en fond.
fn courbe_de_voix(ui: &mut egui::Ui, brute: &[f32; NB_TIERS], envoyee: &[f32; NB_TIERS]) {
    let largeur = ui.available_width().min(420.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(largeur, 110.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    let (fmin, fmax) = (80f32.log10(), 16_000f32.log10());
    let (dmin, dmax) = (-24.0f32, 12.0f32);
    let x = |f: f32| rect.left() + (f.log10() - fmin) / (fmax - fmin) * rect.width();
    let y = |db: f32| rect.bottom() - (db.clamp(dmin, dmax) - dmin) / (dmax - dmin) * rect.height();
    p.rect_filled(rect, 8.0, theme::BG_DEEP);
    let police = egui::FontId::proportional(10.0);
    p.rect_filled(
        egui::Rect::from_x_y_ranges(x(2_000.0)..=x(6_300.0), rect.y_range()),
        0.0,
        theme::alpha(ACCENT, 12),
    );
    p.text(
        Pos2::new((x(2_000.0) + x(6_300.0)) / 2.0, rect.top() + 3.0),
        egui::Align2::CENTER_TOP,
        "aigus",
        police.clone(),
        TEXT_FAINT,
    );
    let grille = theme::alpha(Color32::WHITE, 14);
    for (f, texte) in [(100.0, "100"), (1_000.0, "1k"), (10_000.0, "10k")] {
        p.line_segment([Pos2::new(x(f), rect.top()), Pos2::new(x(f), rect.bottom())], Stroke::new(1.0_f32, grille));
        p.text(Pos2::new(x(f) + 3.0, rect.bottom() - 3.0), egui::Align2::LEFT_BOTTOM, texte, police.clone(), TEXT_FAINT);
    }
    for db in [-12.0f32, 6.0] {
        p.line_segment([Pos2::new(rect.left(), y(db)), Pos2::new(rect.right(), y(db))], Stroke::new(1.0_f32, grille));
        p.text(Pos2::new(rect.left() + 4.0, y(db) - 1.0), egui::Align2::LEFT_BOTTOM, format!("{db:+.0}"), police.clone(), TEXT_FAINT);
    }
    p.line_segment(
        [Pos2::new(rect.left(), y(0.0)), Pos2::new(rect.right(), y(0.0))],
        Stroke::new(1.0_f32, theme::alpha(Color32::WHITE, 50)),
    );
    p.text(Pos2::new(rect.right() - 4.0, y(0.0) - 1.0), egui::Align2::RIGHT_BOTTOM, "voix moyenne", police, TEXT_FAINT);
    let points = |ecarts: &[f32; NB_TIERS]| -> Vec<Pos2> {
        TIERS.iter().zip(ecarts).map(|(&f, &db)| Pos2::new(x(f), y(db))).collect()
    };
    p.add(egui::Shape::line(points(brute), Stroke::new(1.5_f32, theme::alpha(INFO, 200))));
    p.add(egui::Shape::line(points(envoyee), Stroke::new(2.0_f32, ACCENT)));
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("— ton micro brut").color(INFO).size(11.5));
        ui.label(RichText::new("— ce que les autres entendent").color(ACCENT).size(11.5));
    });
}

/// Un égaliseur en quelques mots : « coupe sous 80 Hz, grondement -4 dB,
/// +12 dB à 2 kHz ».
fn decrire(bandes: &[egaliseur::Bande]) -> String {
    bandes
        .iter()
        .map(|b| match b.forme {
            egaliseur::Forme::PasseHaut => format!("coupe sous {:.0} Hz", b.frequence),
            egaliseur::Forme::Cloche if b.frequence < 500.0 => format!("grondement {:+.0} dB", b.gain_db),
            _ => format!("{:+.0} dB à {}", b.gain_db, crate::egaliseur_ui::frequence_texte(b.frequence)),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

impl KiApp {
    /// Le volume de sa voix dans l'essai, comparé à une voix réglée par
    /// défaut — ce que les autres entendent de lui à côté des autres —, et
    /// de quoi le régler en un clic : le gain automatique, au niveau des
    /// voix réglées par défaut.
    fn volume_de_l_essai(&mut self, ui: &mut egui::Ui, ecart_db: f32) {
        volume_en_mots(ui, ecart_db);
        if !(-30.0..-5.0).contains(&ecart_db) {
            return;
        }
        let defaut = ki_voice::NIVEAU_VOIX_DEFAUT;
        let pendant = self
            .link
            .engine
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|e| e.essai_analyse())
            .map(|a| a.gain_auto);
        let regle = self.agc && self.agc_target >= defaut - 0.005;
        match pendant {
            // Enregistré gain automatique réglé au niveau des autres, et
            // pourtant bas : il plafonne, le micro livre trop peu.
            Some(Some(cible)) if cible >= defaut - 0.005 => ui::precision(
                ui,
                "Même le gain automatique ne te remonte pas assez : ton micro livre trop \
                 peu. Monte son niveau dans la page Casque (« Régler mon micro »).",
            ),
            _ if regle => ui::precision(
                ui,
                "Le gain automatique est réglé au niveau des autres : refais un essai pour \
                 l'entendre.",
            ),
            _ => {
                ui.add_space(4.0);
                let libelle = if self.agc { "Mettre ma voix au niveau des autres" } else { "Rallumer le gain automatique" };
                if ui::button(ui, Icon::Volume, libelle)
                    .on_hover_text(
                        "il règle ta voix sur le niveau des voix réglées par défaut, quelle que \
                         soit ta carte son",
                    )
                    .clicked()
                {
                    self.agc = true;
                    self.agc_target = defaut;
                    self.apply_audio_settings();
                }
            }
        }
    }
}

/// Le volume de sa voix dans l'essai, en mots, et ce qu'il y a à savoir.
fn volume_en_mots(ui: &mut egui::Ui, ecart_db: f32) {
    let (mot, couleur) = if ecart_db < -30.0 {
        ("presque rien", DANGER)
    } else if ecart_db < -12.0 {
        ("très bas", DANGER)
    } else if ecart_db < -5.0 {
        ("un peu bas", WARN)
    } else if ecart_db <= 4.0 {
        ("bon", SPEAK)
    } else {
        ("fort", WARN)
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Le volume de ta voix chez les autres :").color(TEXT_DIM).size(12.0));
        ui.label(RichText::new(mot).color(couleur).size(12.0).strong());
        if (-30.0..=-5.0).contains(&ecart_db) || ecart_db > 4.0 {
            ui.label(
                RichText::new(format!("{ecart_db:+.0} dB par rapport à une voix réglée par défaut"))
                    .color(TEXT_DIM)
                    .size(12.0),
            );
        }
    });
    if ecart_db < -30.0 {
        ui::precision(
            ui,
            "Ton micro n'a presque rien capté : vérifie qu'il est branché, et choisi dans \
             « Périphérique » juste au-dessus.",
        );
    } else if ecart_db > 4.0 {
        ui::precision(ui, "Baisse « Ton volume » (page Casque, section Micro).");
    }
}

/// Le choix d'un périphérique : « défaut du système » ou un nom. Rend vrai
/// au changement.
fn combo_peripherique(
    ui: &mut egui::Ui,
    id: &str,
    devices: &[String],
    choix: &mut Option<String>,
) -> bool {
    let mut change = false;
    let largeur = (ui.available_width() - 40.0).clamp(140.0, 320.0);
    egui::ComboBox::from_id_salt(id)
        .width(largeur)
        .selected_text(
            RichText::new(choix.clone().unwrap_or_else(|| "Défaut du système".into())).color(TEXT),
        )
        .show_ui(ui, |ui| {
            if ui.selectable_label(choix.is_none(), "Défaut du système").clicked() {
                change |= choix.is_some();
                *choix = None;
            }
            for d in devices {
                let actif = choix.as_deref() == Some(d.as_str());
                if ui.selectable_label(actif, d).clicked() && !actif {
                    *choix = Some(d.clone());
                    change = true;
                }
            }
        });
    change
}

/// Un curseur à la largeur de la colonne. `pas` : `Some(1.0)` pour des
/// entiers. Rend vrai au changement.
pub(crate) fn curseur(
    ui: &mut egui::Ui,
    valeur: &mut f32,
    plage: RangeInclusive<f32>,
    suffixe: &str,
    pas: Option<f64>,
) -> bool {
    ui.spacing_mut().slider_width = (ui.available_width() - 76.0).clamp(120.0, 260.0);
    let mut s = egui::Slider::new(valeur, plage).suffix(suffixe);
    if let Some(p) = pas {
        s = s.step_by(p);
        if p >= 1.0 {
            s = s.fixed_decimals(0);
        }
    }
    ui.add(s).changed()
}

/// Le panneau « Enregistrement » de Windows : c'est là que se règle le
/// niveau du micro, celui qui sature avant ki-chat.
fn ouvrir_reglage_micro_windows() {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("rundll32.exe")
            .args(["shell32.dll,Control_RunDLL", "mmsys.cpl,,1"])
            .spawn();
    }
}
