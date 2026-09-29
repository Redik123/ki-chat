//! L'onglet Audio des réglages.
//!
//! Rangé comme le son circule : ton micro, quand il s'ouvre, ce que ta voix
//! traverse avant de partir, puis ce que tu entends. Chaque section dit en une
//! phrase à quoi elle sert ; ce qui ne sert qu'au dépannage attend, replié, en
//! bas. Les choix courts se font en pastilles (tout se voit d'un coup), les
//! marches/arrêts par interrupteur.

use std::ops::RangeInclusive;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText, Vec2};

use crate::icons::Icon;
use crate::ptt::PttKey;
use crate::theme::{self, ACCENT, DANGER, SPEAK, TEXT, TEXT_DIM, WARN};
use crate::ui::{self, Tone};
use crate::{KiApp, MicMode, VoiceSnapshot};
use ki_voice::dynamique::{COMPRESSION_AUCUNE, COMPRESSION_DOUCE, COMPRESSION_FORTE};

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
                    }
                    ui::precision(
                        ui,
                        "Tu t'entends comme les autres t'entendent : traitement et codec \
                         compris.",
                    );
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
fn curseur(
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
