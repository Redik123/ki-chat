//! La vitrine de ki-ui : chaque composant, la palette et les échelles, sur
//! une seule page — pour voir d'un coup d'œil ce qu'une appli ki-* a sous
//! la main, et vérifier qu'un changement de ki-ui n'abîme rien.
//!
//! `cargo run -p ki-ui --example vitrine`

use eframe::egui::{self, vec2, Color32, CornerRadius, RichText, Sense};
use ki_ui::composants::{self as c, Tone};
use ki_ui::flex::{Case, Flex, Repartit};
use ki_ui::icones::{self, Icon};
use ki_ui::jetons::{couleur, espace, rayon, texte};
use ki_ui::liste::Liste;

fn main() -> eframe::Result {
    // Le mouchard de ki-ui parle au journal : on le veut dans le terminal.
    let _ = tracing_subscriber::fmt().try_init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 860.0]).with_title("Vitrine ki-ui"),
        ..Default::default()
    };
    eframe::run_native(
        "vitrine-ki-ui",
        options,
        Box::new(|cc| {
            ki_ui::style::installer(&cc.egui_ctx);
            Ok(Box::new(Vitrine::default()))
        }),
    )
}

#[derive(Default)]
struct Vitrine {
    actif: bool,
    mode: u8,
    qualite: u8,
    pseudo: String,
    bandeau_ferme: bool,
    /// Les clés des éléments de la liste virtualisée, et la prochaine à
    /// donner à ce qu'on ajoute au-dessus.
    elements: Vec<u64>,
    plus_ancien: u64,
}

impl eframe::App for Vitrine {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Les vumètres et le sablier bougent : une image par 30 ms.
        let temps = ui.input(|i| i.time);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(30));
        let niveau = ((temps * 1.7).sin() * 0.5 + 0.5) as f32;

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.label(RichText::new("Vitrine ki-ui").size(texte::GRAND).strong().color(couleur::TEXT));
                c::hint(ui, "Tout ce qu'une appli ki-* a sous la main : jetons, composants, mise en page.");
                ui.add_space(espace::L);

                self.palette(ui);
                self.echelles(ui);
                self.boutons(ui);
                self.icones(ui);
                self.reglages(ui);
                self.bandeaux(ui);
                self.mesures(ui, niveau, temps);
                self.flex(ui);
                self.liste(ui);
            });
        });
    }
}

impl Vitrine {
    fn palette(&mut self, ui: &mut egui::Ui) {
        c::section(ui, Icon::Star, "Palette", Some("ki_ui::jetons::couleur"), |ui| {
            let teintes = [
                ("BG_DEEP", couleur::BG_DEEP),
                ("BG_SIDE", couleur::BG_SIDE),
                ("BG_BASE", couleur::BG_BASE),
                ("BG_RAISED", couleur::BG_RAISED),
                ("BG_HOVER", couleur::BG_HOVER),
                ("BG_ACTIVE", couleur::BG_ACTIVE),
                ("BORDER", couleur::BORDER),
                ("BORDER_STRONG", couleur::BORDER_STRONG),
                ("TEXT", couleur::TEXT),
                ("TEXT_DIM", couleur::TEXT_DIM),
                ("TEXT_FAINT", couleur::TEXT_FAINT),
                ("ACCENT", couleur::ACCENT),
                ("SPEAK", couleur::SPEAK),
                ("DANGER", couleur::DANGER),
                ("WARN", couleur::WARN),
                ("INFO", couleur::INFO),
                ("INVITE", couleur::INVITE),
            ];
            Flex::ligne().ecart(espace::M).passer_a_la_ligne().show(ui, "teintes", |f| {
                for (nom, teinte) in teintes {
                    f.ui(|ui| {
                        ui.vertical(|ui| {
                            let (rect, _) = ui.allocate_exact_size(vec2(110.0, 34.0), Sense::hover());
                            ui.painter().rect_filled(rect, CornerRadius::same(rayon::M), teinte);
                            ui.label(RichText::new(nom).size(texte::MINUSCULE).color(couleur::TEXT_DIM));
                        });
                    });
                }
            });
        });
    }

    fn echelles(&mut self, ui: &mut egui::Ui) {
        c::section(ui, Icon::Sliders, "Échelles", Some("espace, rayon, texte"), |ui| {
            c::section_label(ui, "espaces");
            for (nom, v) in [
                ("XXS", espace::XXS),
                ("XS", espace::XS),
                ("S", espace::S),
                ("M", espace::M),
                ("L", espace::L),
                ("XL", espace::XL),
                ("XXL", espace::XXL),
            ] {
                ui.horizontal(|ui| {
                    ui.add_sized(vec2(70.0, 14.0), egui::Label::new(RichText::new(format!("{nom} · {v}")).size(texte::PETIT)));
                    let (rect, _) = ui.allocate_exact_size(vec2(v * 4.0, 10.0), Sense::hover());
                    ui.painter().rect_filled(rect, CornerRadius::same(2), couleur::ACCENT);
                });
            }
            ui.add_space(espace::M);
            c::section_label(ui, "rayons");
            Flex::ligne().ecart(espace::L).show(ui, "rayons", |f| {
                for (nom, r) in [("S", rayon::S), ("M", rayon::M), ("L", rayon::L), ("XL", rayon::XL), ("PILULE", rayon::PILULE)] {
                    f.ui(|ui| {
                        ui.vertical(|ui| {
                            let (rect, _) = ui.allocate_exact_size(vec2(64.0, 40.0), Sense::hover());
                            ui.painter().rect_filled(rect, CornerRadius::same(r), couleur::BG_ACTIVE);
                            ui.label(RichText::new(format!("{nom} · {r}")).size(texte::MINUSCULE));
                        });
                    });
                }
            });
            ui.add_space(espace::M);
            c::section_label(ui, "texte");
            for (nom, t) in [
                ("MINUSCULE", texte::MINUSCULE),
                ("PETIT", texte::PETIT),
                ("COURANT", texte::COURANT),
                ("CORPS", texte::CORPS),
                ("TITRE", texte::TITRE),
                ("GRAND", texte::GRAND),
            ] {
                ui.label(RichText::new(format!("{nom} · {t} — Portez ce vieux whisky au juge blond")).size(t));
            }
        });
    }

    fn boutons(&mut self, ui: &mut egui::Ui) {
        c::section(ui, Icon::Play, "Boutons", Some("button, primary_button, tinted_button, icon_button"), |ui| {
            Flex::ligne().ecart(espace::S).passer_a_la_ligne().show(ui, "boutons", |f| {
                f.ui(|ui| c::button(ui, Icon::Play, "Lecture"));
                f.ui(|ui| c::primary_button(ui, Some(Icon::Send), "Envoyer", None));
                f.ui(|ui| c::tinted_button(ui, Some(Icon::Check), "Accent", Tone::Accent));
                f.ui(|ui| c::tinted_button(ui, Some(Icon::Close), "Danger", Tone::Danger));
                f.ui(|ui| c::tinted_button(ui, Some(Icon::Warning), "Attention", Tone::Warn));
                f.ui(|ui| c::tinted_button(ui, Some(Icon::Info), "Info", Tone::Info));
                f.ui(|ui| c::icon_button(ui, Icon::Gear, "Réglages"));
                f.ui(|ui| c::icon_button_ex(ui, Icon::Send, 32.0, "Envoyer, teinté", Some(couleur::ACCENT)));
            });
        });
    }

    fn icones(&mut self, ui: &mut egui::Ui) {
        c::section(ui, Icon::Star, "Icônes", Some("ki_ui::icones — 40 icônes vectorielles"), |ui| {
            Flex::ligne().ecart(espace::M).passer_a_la_ligne().show(ui, "icones", |f| {
                for icone in Icon::TOUTES {
                    f.case(Case::new().largeur(84.0), |ui| {
                        ui.vertical_centered(|ui| {
                            let (rect, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
                            icones::draw(ui.painter(), rect, icone, couleur::TEXT);
                            ui.label(RichText::new(format!("{icone:?}")).size(texte::MINUSCULE).color(couleur::TEXT_FAINT));
                        });
                    });
                }
            });
        });
    }

    fn reglages(&mut self, ui: &mut egui::Ui) {
        c::section(ui, Icon::Gear, "Réglages", Some("section, ligne, interrupteur, segmente, pastilles"), |ui| {
            c::ligne(ui, "Interrupteur", |ui| {
                c::interrupteur(ui, &mut self.actif, "Activer la chose");
                c::precision(ui, "Une précision sous le contrôle, qui passe à la ligne quand la colonne est étroite.");
            });
            c::ligne(ui, "Segments", |ui| {
                c::segmente(ui, &mut self.mode, &[(0, "Ouvert"), (1, "Push-to-talk"), (2, "À la voix")]);
            });
            c::ligne(ui, "Pastilles", |ui| {
                c::pastilles(ui, &mut self.qualite, &[(0, "720p"), (1, "1080p"), (2, "1440p")]);
            });
            c::ligne(ui, "Champ", |ui| {
                ui.add(c::text_field(&mut self.pseudo, "Ton pseudo", false));
            });
        });
    }

    fn bandeaux(&mut self, ui: &mut egui::Ui) {
        c::section(ui, Icon::Info, "Bandeaux et repères", Some("banner, hint, card, group_title…"), |ui| {
            c::banner(ui, Tone::Info, "Un bandeau d'information.", false);
            ui.add_space(espace::S);
            c::banner(ui, Tone::Warn, "Un bandeau d'avertissement.", false);
            ui.add_space(espace::S);
            c::banner(ui, Tone::Danger, "Un bandeau d'erreur.", false);
            ui.add_space(espace::S);
            if !self.bandeau_ferme && c::banner(ui, Tone::Accent, "Un bandeau qu'on peut fermer.", true) {
                self.bandeau_ferme = true;
            }
            ui.add_space(espace::M);
            c::group_title(ui, Icon::Chat, "Titre de groupe");
            c::field_label(ui, "Libellé de champ");
            c::hint(ui, "Une aide discrète.");
            c::hairline(ui);
            ui.add_space(espace::S);
            c::card(ui, |ui| {
                ui.label("Une carte : une surface relevée pour regrouper.");
            });
        });
    }

    fn mesures(&mut self, ui: &mut egui::Ui, niveau: f32, temps: f64) {
        c::section(ui, Icon::Volume, "Mesures et présence", Some("meter, spinner, status_dot, avatar…"), |ui| {
            c::meter(ui, niveau, vec2(240.0, 8.0), couleur::ACCENT);
            ui.add_space(espace::S);
            c::meter_with_threshold(ui, niveau, Some(0.6), vec2(240.0, 10.0), couleur::SPEAK);
            ui.add_space(espace::M);
            Flex::ligne().ecart(espace::L).show(ui, "presence", |f| {
                f.ui(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
                    c::spinner(ui.painter(), rect.center(), 9.0, temps, couleur::ACCENT);
                });
                f.ui(|ui| c::status_dot(ui, couleur::SPEAK, "en ligne", texte::PETIT));
                f.ui(|ui| c::signal_badge(ui, 3, "12 ms", couleur::ACCENT));
                f.ui(|ui| {
                    c::stat_row(
                        ui,
                        &[(Icon::ArrowUp, "64 kbit/s".into(), couleur::TEXT_DIM), (Icon::ArrowDown, "70 kbit/s".into(), couleur::TEXT_DIM)],
                        texte::PETIT,
                    )
                });
                f.ui(|ui| c::avatar(ui, "Redik_", 40.0, niveau > 0.6, None, couleur::BG_RAISED));
                f.ui(|ui| c::avatar(ui, "mg4230", 40.0, false, None, couleur::BG_RAISED));
                f.ui(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
                    c::paint_server_badge(ui.painter(), rect, "ki-chat", "ts.baws.fun", None);
                });
            });
        });
    }

    fn flex(&mut self, ui: &mut egui::Ui) {
        c::section(ui, Icon::Screen, "Mise en page flex", Some("ki_ui::flex"), |ui| {
            c::section_label(ui, "un champ qui prend la place qui reste");
            Flex::ligne().ecart(espace::S).show(ui, "saisie", |f| {
                f.ui(|ui| c::icon_button(ui, Icon::Paperclip, "Joindre"));
                f.grandit(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.pseudo).desired_width(ui.available_width()));
                });
                f.ui(|ui| c::icon_button_ex(ui, Icon::Send, 32.0, "Envoyer", Some(couleur::ACCENT)));
            });
            ui.add_space(espace::M);
            c::section_label(ui, "répartis : le premier au début, le dernier à la fin");
            Flex::ligne().repartir(Repartit::Entre).show(ui, "entre", |f| {
                f.ui(|ui| c::button(ui, Icon::ChevronLeft, "Précédent"));
                f.ui(|ui| ui.label(RichText::new("page 2 / 5").color(couleur::TEXT_DIM)));
                f.ui(|ui| c::button(ui, Icon::ChevronRight, "Suivant"));
            });
            ui.add_space(espace::M);
            c::section_label(ui, "passer à la ligne");
            Flex::ligne().ecart(espace::S).passer_a_la_ligne().show(ui, "etiquettes", |f| {
                for mot in ["Valorant", "Ascent", "Diamant 3", "compétitive", "party 2/5", "Lotus", "Haven", "8-9", "Jett", "Chamber"] {
                    f.ui(|ui| {
                        egui::Frame::NONE
                            .fill(couleur::BG_ACTIVE)
                            .corner_radius(CornerRadius::same(rayon::PILULE))
                            .inner_margin(egui::Margin::symmetric(10, 4))
                            .show(ui, |ui| ui.label(RichText::new(mot).size(texte::PETIT).color(Color32::WHITE)));
                    });
                }
            });
        });
    }

    fn liste(&mut self, ui: &mut egui::Ui) {
        if self.elements.is_empty() {
            self.plus_ancien = 1_000_000;
            self.elements = (self.plus_ancien..self.plus_ancien + 10_000).collect();
        }
        c::section(ui, Icon::Chat, "Liste virtualisée", Some("ki_ui::liste"), |ui| {
            let mut en_haut = false;
            let mut en_bas = false;
            ui.horizontal(|ui| {
                en_haut = c::button(ui, Icon::ArrowUp, "500 plus anciens").clicked();
                en_bas = c::button(ui, Icon::ArrowDown, "Un nouveau").clicked();
            });
            if en_haut {
                let debut = self.plus_ancien - 500;
                self.elements.splice(0..0, debut..self.plus_ancien);
                self.plus_ancien = debut;
            }
            if en_bas {
                let suivant = self.elements.last().map_or(0, |d| d + 1);
                self.elements.push(suivant);
            }
            ui.add_space(espace::S);
            let elements = &self.elements;
            let sortie = ui
                .allocate_ui(vec2(ui.available_width(), 320.0), |ui| {
                    Liste::new("vitrine-liste").coller_en_bas(true).show(
                        ui,
                        elements.len(),
                        |i| elements[i],
                        |ui, i| {
                            let cle = elements[i];
                            // Des longueurs variées : de quoi passer à la
                            // ligne, ou pas.
                            let mots = 3 + (cle * 7919 % 41) as usize;
                            let texte_ = std::iter::repeat_n("ki-chat", mots).collect::<Vec<_>>().join(" ");
                            ui.horizontal_top(|ui| {
                                ui.label(RichText::new(format!("#{cle}")).color(couleur::pour_pseudo(&cle.to_string())).strong());
                                ui.add(egui::Label::new(RichText::new(texte_).color(couleur::TEXT_DIM)).wrap());
                            });
                        },
                    )
                })
                .inner;
            c::hint(
                ui,
                &format!(
                    "{} éléments, {} construits à cette image, {} mesurés d'avance — {}",
                    self.elements.len(),
                    sortie.dessines,
                    sortie.mesures,
                    if sortie.en_bas { "collée en bas" } else { "remontée" }
                ),
            );
        });
    }
}
