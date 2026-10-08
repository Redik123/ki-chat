//! Le mode studio de la page Casque : la chaîne de sa voix, bloc par bloc,
//! dans l'ordre où la voix la traverse — porte, égaliseur, gain
//! automatique, de-esser, compresseur, chaleur, limiteur —, chacun avec ses
//! réglages et son aiguille. Et des profils : toute la chaîne sous un nom.
//!
//! Le mode simple règle les mêmes choses, en moins de gestes : ce qu'on règle
//! ici reste actif quand on y revient.

use ki_ui::jetons::{espace, texte};
use eframe::egui::{self, RichText, Vec2};
use ki_voice::dynamique::{
    ReglagesCompresseur, ReglagesDeesser, ReglagesPorte, ReglagesStudio, COMPRESSION_AUCUNE, COMPRESSION_DOUCE,
    COMPRESSION_FORTE, COMPRESSION_PERSO,
};

use crate::icons::Icon;
use crate::reglages_audio::curseur;
use crate::theme::{ACCENT, DANGER, SPEAK, TEXT, TEXT_DIM, TEXT_FAINT, WARN};
use crate::ui;
use crate::{KiApp, VoiceSnapshot};

/// Un profil : toute la chaîne de sa voix, sous un nom.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ProfilVoix {
    pub(crate) nom: String,
    /// L'égaliseur de sa voix (`ki_voice::egaliseur::ecrire`).
    pub(crate) egaliseur: String,
    /// La chaîne studio ([`ecrire_studio`]).
    pub(crate) studio: String,
    pub(crate) compression: u8,
    /// Le seuil de la porte, en niveau linéaire (0 : pas de porte).
    pub(crate) porte: f32,
    pub(crate) agc: bool,
    pub(crate) agc_cible: f32,
}

/// La chaîne studio telle que les préférences la rangent.
pub(crate) fn ecrire_studio(r: &ReglagesStudio) -> String {
    let p = r.porte;
    let c = r.compresseur;
    let deesser = r
        .deesser
        .map(|d| format!("{},{},{}", d.frequence, d.seuil_db, d.reduction_max_db))
        .unwrap_or_else(|| "-".into());
    format!(
        "porte={},{},{},{};deesser={deesser};comp={},{},{},{},{},{};chaleur={};plafond={}",
        p.profondeur_db,
        p.attaque_ms,
        p.maintien_ms,
        p.relachement_ms,
        c.seuil_db,
        c.ratio,
        c.genou_db,
        c.attaque_ms,
        c.relachement_ms,
        c.rattrapage_db,
        r.chaleur,
        r.plafond_db
    )
}

/// L'inverse d'[`ecrire_studio`] ; ce qui manque ou ne se lit pas garde sa
/// valeur par défaut — la chaîne d'avant le mode studio.
pub(crate) fn lire_studio(texte: &str) -> ReglagesStudio {
    let mut r = ReglagesStudio::default();
    for champ in texte.split(';') {
        let Some((cle, valeur)) = champ.split_once('=') else { continue };
        let n: Vec<f32> = valeur.split(',').filter_map(|v| v.trim().parse().ok()).collect();
        match (cle.trim(), n.as_slice()) {
            ("porte", &[profondeur_db, attaque_ms, maintien_ms, relachement_ms]) => {
                // Bornée : une profondeur de 0 dB (curseur poussé à fond)
                // rendait la porte inerte sans rien dire — elle redevient la
                // coupure complète.
                r.porte = ReglagesPorte { profondeur_db, attaque_ms, maintien_ms, relachement_ms }.bornes();
            }
            ("deesser", &[frequence, seuil_db, reduction_max_db]) => {
                r.deesser = Some(ReglagesDeesser { frequence, seuil_db, reduction_max_db });
            }
            ("deesser", []) => r.deesser = None,
            ("comp", &[seuil_db, ratio, genou_db, attaque_ms, relachement_ms, rattrapage_db]) => {
                r.compresseur =
                    ReglagesCompresseur { seuil_db, ratio, genou_db, attaque_ms, relachement_ms, rattrapage_db }.bornes();
            }
            ("chaleur", &[c]) if c.is_finite() => r.chaleur = c.clamp(0.0, 1.0),
            ("plafond", &[p]) if p.is_finite() => r.plafond_db = p.clamp(-12.0, 0.0),
            _ => {}
        }
    }
    r
}

/// Une chaîne studio dont la porte avait une profondeur de 0 dB (ou presque) :
/// jusqu'à la 0.1.57 le curseur l'acceptait, et la porte « fermée » laissait
/// tout passer — elle ne faisait rien, quel que soit son seuil. À la lecture,
/// `lire_studio` la ramène à -80 dB ; mais rallumer d'un coup une porte à un
/// seuil réglé pendant qu'elle était inerte couperait des bouts de voix sans
/// prévenir. L'application la désactive donc (seuil à 0) et le dit.
pub(crate) fn porte_inerte(texte: &str) -> bool {
    texte.split(';').any(|champ| {
        champ.trim().strip_prefix("porte=").and_then(|v| v.split(',').next()?.trim().parse::<f32>().ok()).is_some_and(
            |profondeur| !profondeur.is_finite() || profondeur > ReglagesPorte::PROFONDEUR_MAX_DB,
        )
    })
}

pub(crate) fn lire_profils(texte: &str) -> Vec<ProfilVoix> {
    serde_json::from_str(texte).unwrap_or_default()
}

pub(crate) fn ecrire_profils(profils: &[ProfilVoix]) -> String {
    serde_json::to_string(profils).unwrap_or_default()
}

/// La colonne des noms de réglages.
const LARGEUR_NOM: f32 = 110.0;

/// Un réglage d'un bloc : son nom sur une colonne, le contrôle à côté. Ce
/// qu'il fait s'affiche au survol du nom — et dessous, quand les
/// explications sont montrées.
fn parametre(ui: &mut egui::Ui, nom: &str, aide: &str, montrer: bool, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(LARGEUR_NOM, 20.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_width(LARGEUR_NOM);
                ui.label(RichText::new(nom).color(TEXT_DIM).size(texte::COURANT)).on_hover_text(aide);
            },
        );
        add(ui);
    });
    if montrer {
        explication(ui, aide);
    }
}

/// Une explication, dans la colonne des contrôles.
fn explication(ui: &mut egui::Ui, texte: &str) {
    ui.horizontal_top(|ui| {
        ui.add_space(LARGEUR_NOM + ui.spacing().item_spacing.x);
        ui.vertical(|ui| {
            ui.add(egui::Label::new(RichText::new(texte).color(TEXT_FAINT).size(texte::PETIT)).wrap());
        });
    });
    ui.add_space(espace::XS);
}

/// L'aiguille d'un bloc : une jauge et sa valeur.
fn aiguille(ui: &mut egui::Ui, niveau: f32, couleur: egui::Color32, texte: &str) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(Vec2::new(LARGEUR_NOM, 14.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_width(LARGEUR_NOM);
        });
        ui::meter(ui, niveau, Vec2::new(160.0, 6.0), couleur);
        ui.label(RichText::new(texte).color(TEXT_FAINT).size(texte::PETIT));
    });
}

/// Le titre d'un bloc, et ce qu'il fait quand les explications sont montrées.
fn titre_bloc(ui: &mut egui::Ui, numero: usize, titre: &str, description: &str, montrer: bool) {
    ui.add_space(espace::M);
    ui.label(RichText::new(format!("{numero}. {titre}")).color(TEXT).size(texte::CORPS).strong())
        .on_hover_text(description);
    if montrer {
        ui.add(egui::Label::new(RichText::new(description).color(TEXT_DIM).size(texte::PETIT)).wrap());
    }
    ui.add_space(espace::XS);
}

impl KiApp {
    /// La chaîne de sa voix, telle qu'elle se range dans un profil.
    fn profil_actuel(&self, nom: &str) -> ProfilVoix {
        ProfilVoix {
            nom: nom.to_string(),
            egaliseur: ki_voice::egaliseur::ecrire(&self.egaliseur_micro),
            studio: ecrire_studio(&self.studio),
            compression: self.compression,
            porte: self.gate_threshold,
            agc: self.agc,
            agc_cible: self.agc_target,
        }
    }

    fn appliquer_profil(&mut self, p: &ProfilVoix) {
        self.egaliseur_micro = ki_voice::egaliseur::lire(&p.egaliseur);
        self.studio = lire_studio(&p.studio);
        self.compression = p.compression.min(COMPRESSION_PERSO);
        self.gate_threshold = p.porte.clamp(0.0, 0.5);
        self.agc = p.agc;
        self.agc_target = p.agc_cible.clamp(0.15, 0.5);
    }

    /// La section « Chaîne de ta voix » du mode studio.
    pub(crate) fn chaine_studio_ui(&mut self, ui: &mut egui::Ui, voice: &VoiceSnapshot, apply: &mut bool) {
        let stats = &voice.stats;
        let actif = voice.engine_up;
        ui::section(
            ui,
            Icon::Sliders,
            "Chaîne de ta voix",
            Some(
                "Dans l'ordre où ta voix la traverse, chaque bloc avec ses réglages et son \
                 aiguille. Tout s'entend aussitôt avec « M'écouter » (onglet Audio).",
            ),
            |ui| {
                ui::ligne(ui, "Aide", |ui| {
                    ui::interrupteur(ui, &mut self.explications, "Expliquer chaque réglage");
                    ui::precision(ui, "Masque les explications une fois connues : le survol d'un nom les rappelle.");
                });
                let montrer = self.explications;

                // --- Profils ---
                ui::ligne(ui, "Profil", |ui| {
                    ui.horizontal_wrapped(|ui| {
                        let actuel = self.profil_actuel("");
                        let nom_actuel = self
                            .profils_voix
                            .iter()
                            .find(|p| ProfilVoix { nom: String::new(), ..(*p).clone() } == actuel)
                            .map(|p| p.nom.clone());
                        egui::ComboBox::from_id_salt("profils_voix")
                            .width(160.0)
                            .selected_text(
                                RichText::new(nom_actuel.clone().unwrap_or_else(|| "— non enregistré —".into()))
                                    .color(TEXT),
                            )
                            .show_ui(ui, |ui| {
                                let mut choisi = None;
                                for (i, p) in self.profils_voix.iter().enumerate() {
                                    if ui.selectable_label(nom_actuel.as_deref() == Some(p.nom.as_str()), &p.nom).clicked() {
                                        choisi = Some(i);
                                    }
                                }
                                if let Some(i) = choisi {
                                    let p = self.profils_voix[i].clone();
                                    self.appliquer_profil(&p);
                                    *apply = true;
                                }
                            });
                        if let Some(nom) = &nom_actuel {
                            if ui::icon_button(ui, Icon::Trash, "Supprimer ce profil").clicked() {
                                self.profils_voix.retain(|p| &p.nom != nom);
                            }
                        }
                    });
                    ui.add_space(espace::XS);
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.profil_nom)
                                .hint_text("nom du profil")
                                .desired_width(160.0),
                        );
                        let nom = self.profil_nom.trim().to_string();
                        if ui
                            .add_enabled(!nom.is_empty(), egui::Button::new("Enregistrer"))
                            .on_hover_text("un profil du même nom est remplacé")
                            .clicked()
                        {
                            let profil = self.profil_actuel(&nom);
                            match self.profils_voix.iter_mut().find(|p| p.nom == nom) {
                                Some(p) => *p = profil,
                                None => self.profils_voix.push(profil),
                            }
                            self.profil_nom.clear();
                        }
                    });
                    if montrer {
                        ui::precision(
                            ui,
                            "Toute ta chaîne — égaliseur compris — sous un nom, pour la retrouver \
                             d'un clic : une pour jouer, une pour streamer, une pour le soir…",
                        );
                    }
                });

                // --- 1. Porte de bruit ---
                titre_bloc(
                    ui,
                    1,
                    "Porte de bruit",
                    "Coupe (ou baisse) le son quand tu ne parles pas : le clavier, le ventilateur, \
                     ta respiration entre deux phrases ne partent plus.",
                    montrer,
                );
                let mut porte_on = self.gate_threshold > 0.0;
                parametre(ui, "Active", "Allume ou coupe la porte.", false, |ui| {
                    if ui::interrupteur(ui, &mut porte_on, "coupe le fond entre tes phrases").changed() {
                        self.gate_threshold = if porte_on { 0.01 } else { 0.0 };
                        *apply = true;
                    }
                });
                if porte_on {
                    let mut seuil = 20.0 * self.gate_threshold.max(1e-4).log10();
                    parametre(
                        ui,
                        "Seuil",
                        "Le niveau à partir duquel ta voix ouvre la porte. Parle normalement : \
                         l'aiguille doit dire « ouverte » ; tais-toi : « fermée ». Trop haut, elle \
                         mange tes débuts de mots ; trop bas, elle laisse passer le clavier.",
                        montrer,
                        |ui| {
                            if curseur(ui, &mut seuil, -80.0..=-20.0, " dBFS", Some(1.0)) {
                                self.gate_threshold = 10f32.powf(seuil / 20.0);
                                *apply = true;
                            }
                        },
                    );
                    let p = &mut self.studio.porte;
                    parametre(
                        ui,
                        "Profondeur",
                        "Ce qu'il reste du son quand elle est fermée. -80 dB : silence total ; \
                         -15 dB : le fond reste, juste plus bas — plus naturel. -6 dB au plus : \
                         au-dessus, la porte ne ferait plus rien.",
                        montrer,
                        |ui| {
                            *apply |= curseur(
                                ui,
                                &mut p.profondeur_db,
                                -80.0..=ReglagesPorte::PROFONDEUR_MAX_DB,
                                " dB",
                                Some(1.0),
                            );
                        },
                    );
                    parametre(
                        ui,
                        "Attaque",
                        "Le temps pour s'ouvrir quand tu parles. 1 à 2 ms : aucun début de mot \
                         n'est mangé.",
                        montrer,
                        |ui| {
                            *apply |= curseur(ui, &mut p.attaque_ms, 0.5..=50.0, " ms", Some(0.5));
                        },
                    );
                    parametre(
                        ui,
                        "Maintien",
                        "Combien de temps elle reste ouverte après ta dernière syllabe : ni les \
                         fins de mots, ni les petites pauses ne sont coupées.",
                        montrer,
                        |ui| {
                            *apply |= curseur(ui, &mut p.maintien_ms, 0.0..=1000.0, " ms", Some(10.0));
                        },
                    );
                    parametre(
                        ui,
                        "Relâchement",
                        "Le temps pour se refermer ensuite. Long : un fondu doux ; court : une \
                         coupure nette.",
                        montrer,
                        |ui| {
                            *apply |= curseur(ui, &mut p.relachement_ms, 10.0..=1000.0, " ms", Some(10.0));
                        },
                    );
                    // L'état du détecteur, pas le gain : fermée, une porte
                    // douce garde un gain proche de 1 et disait « ouverte ».
                    let g = stats.porte_gain.clamp(0.0, 1.0);
                    let (couleur, texte) = if !actif {
                        (TEXT_FAINT, "vocal inactif")
                    } else if stats.porte_ouverte {
                        (SPEAK, "ouverte")
                    } else {
                        (TEXT_DIM, "fermée")
                    };
                    aiguille(ui, if actif { g } else { 0.0 }, couleur, texte);
                }

                // --- 2. Égaliseur ---
                titre_bloc(
                    ui,
                    2,
                    "Égaliseur",
                    "Change le timbre de ta voix — plus ou moins de graves, de médiums, \
                     d'aigus. Sa courbe est plus haut, onglet « Ta voix ».",
                    true,
                );

                // --- 3. Gain automatique ---
                titre_bloc(
                    ui,
                    3,
                    "Gain automatique",
                    "Ramène ta voix à un niveau constant : tu parles doucement, il monte ; tu \
                     cries, il baisse.",
                    montrer,
                );
                parametre(
                    ui,
                    "Actif",
                    "Coupé, ta voix garde les écarts de niveau naturels de ton micro.",
                    montrer,
                    |ui| {
                        *apply |=
                            ui::interrupteur(ui, &mut self.agc, "ramène ta voix à un niveau constant").changed();
                    },
                );
                if self.agc {
                    let mut pct = self.agc_target * 100.0;
                    parametre(
                        ui,
                        "Niveau visé",
                        "Le niveau que ta voix vise. Plus haut : plus forte chez les autres.",
                        montrer,
                        |ui| {
                            if curseur(ui, &mut pct, 15.0..=50.0, " %", Some(1.0)) {
                                self.agc_target = pct / 100.0;
                                *apply = true;
                            }
                        },
                    );
                }

                // --- 4. De-esser ---
                titre_bloc(
                    ui,
                    4,
                    "De-esser",
                    "Calme les « s », « ch » et « z » qui sifflent dans les oreilles des autres, \
                     sans ternir le reste de ta voix.",
                    montrer,
                );
                let mut deesser_on = self.studio.deesser.is_some();
                parametre(ui, "Actif", "Allume ou coupe le de-esser.", false, |ui| {
                    if ui::interrupteur(ui, &mut deesser_on, "calme les « s » et « ch » qui sifflent").changed() {
                        self.studio.deesser = deesser_on.then(ReglagesDeesser::default);
                        *apply = true;
                    }
                });
                if let Some(d) = self.studio.deesser.as_mut() {
                    let mut khz = d.frequence / 1000.0;
                    parametre(
                        ui,
                        "Fréquence",
                        "Où se trouvent tes sifflantes : entre 5 et 8 kHz d'habitude. Monte-la si \
                         tes « s » sont très aigus, descends-la pour les « ch ».",
                        montrer,
                        |ui| {
                            if curseur(ui, &mut khz, 3.0..=10.0, " kHz", Some(0.1)) {
                                d.frequence = khz * 1000.0;
                                *apply = true;
                            }
                        },
                    );
                    parametre(
                        ui,
                        "Seuil",
                        "À partir de quel niveau de sifflante il agit. Plus bas : il agit plus \
                         souvent.",
                        montrer,
                        |ui| {
                            *apply |= curseur(ui, &mut d.seuil_db, -50.0..=0.0, " dB", Some(1.0));
                        },
                    );
                    parametre(
                        ui,
                        "Réduction max",
                        "De combien, au plus, il baisse une sifflante. Trop : tu zozotes.",
                        montrer,
                        |ui| {
                            *apply |= curseur(ui, &mut d.reduction_max_db, 1.0..=20.0, " dB", Some(1.0));
                        },
                    );
                    let r = -stats.reduction_deesser_db.min(0.0);
                    aiguille(ui, (r / 20.0).min(1.0), WARN, &format!("−{r:.1} dB"));
                }

                // --- 5. Compresseur ---
                titre_bloc(
                    ui,
                    5,
                    "Compresseur",
                    "Rapproche les passages forts des passages faibles : ta voix reste lisible, et \
                     ne claque pas dans les oreilles des autres quand tu t'emportes.",
                    montrer,
                );
                parametre(
                    ui,
                    "Mode",
                    "Doux : seulement les éclats. Fort : toute la voix tenue. Perso : tes \
                     réglages, ci-dessous.",
                    montrer,
                    |ui| {
                        *apply |= ui::segmente(
                            ui,
                            &mut self.compression,
                            &[
                                (COMPRESSION_AUCUNE, "Aucun"),
                                (COMPRESSION_DOUCE, "Doux"),
                                (COMPRESSION_FORTE, "Fort"),
                                (COMPRESSION_PERSO, "Perso"),
                            ],
                        );
                    },
                );
                if self.compression != COMPRESSION_AUCUNE {
                    // Doux et Fort se montrent tels qu'ils sont ; toucher à un
                    // réglage passe en « Perso », à partir de ces valeurs.
                    let mut r = ReglagesCompresseur::du_niveau(self.compression).unwrap_or(self.studio.compresseur);
                    let avant = r;
                    parametre(
                        ui,
                        "Seuil",
                        "Au-dessus de ce niveau, il agit. Plus bas : il tient une plus grande part \
                         de ta voix.",
                        montrer,
                        |ui| {
                            curseur(ui, &mut r.seuil_db, -60.0..=0.0, " dB", Some(0.5));
                        },
                    );
                    parametre(
                        ui,
                        "Ratio",
                        "Sa force : à 3:1, pour 3 dB de trop, 1 seul passe. À 10:1 et au-delà, ta \
                         voix ne dépasse plus du tout le seuil.",
                        montrer,
                        |ui| {
                            ui.spacing_mut().slider_width = (ui.available_width() - 76.0).clamp(120.0, 260.0);
                            ui.add(
                                egui::Slider::new(&mut r.ratio, 1.0..=20.0)
                                    .logarithmic(true)
                                    .custom_formatter(|v, _| format!("{v:.1}:1")),
                            );
                        },
                    );
                    parametre(
                        ui,
                        "Attaque",
                        "Sa vitesse de réaction. 1 à 5 ms : il tient les cris ; 20 ms et plus : il \
                         laisse passer le mordant des consonnes.",
                        montrer,
                        |ui| {
                            curseur(ui, &mut r.attaque_ms, 0.1..=100.0, " ms", Some(0.1));
                        },
                    );
                    parametre(
                        ui,
                        "Relâchement",
                        "Le temps pour relâcher après un éclat. Trop court, ça « pompe » ; trop \
                         long, ta voix reste écrasée après un cri.",
                        montrer,
                        |ui| {
                            curseur(ui, &mut r.relachement_ms, 10.0..=1000.0, " ms", Some(5.0));
                        },
                    );
                    parametre(
                        ui,
                        "Genou",
                        "Adoucit l'entrée en compression : 0, d'un coup ; 6 et plus, en douceur — \
                         plus naturel.",
                        montrer,
                        |ui| {
                            curseur(ui, &mut r.genou_db, 0.0..=18.0, " dB", Some(0.5));
                        },
                    );
                    parametre(
                        ui,
                        "Rattrapage",
                        "Remonte toute ta voix après la compression, pour retrouver le volume \
                         qu'elle a pris.",
                        montrer,
                        |ui| {
                            curseur(ui, &mut r.rattrapage_db, 0.0..=24.0, " dB", Some(0.5));
                        },
                    );
                    if r != avant {
                        self.studio.compresseur = r.bornes();
                        self.compression = COMPRESSION_PERSO;
                        *apply = true;
                    }
                    let red = -stats.reduction_compresseur_db.min(0.0);
                    let couleur = if red > 12.0 { DANGER } else if red > 6.0 { WARN } else { ACCENT };
                    aiguille(ui, (red / 24.0).min(1.0), couleur, &format!("−{red:.1} dB"));
                }

                // --- 6. Chaleur ---
                titre_bloc(
                    ui,
                    6,
                    "Chaleur",
                    "Une saturation douce, façon lampe : la voix paraît plus pleine, plus « radio ».",
                    montrer,
                );
                let mut chaleur = self.studio.chaleur * 100.0;
                parametre(
                    ui,
                    "Saturation",
                    "0 % : rien. 20 à 30 % : de la présence. Au-delà, ça commence à grésiller.",
                    montrer,
                    |ui| {
                        if curseur(ui, &mut chaleur, 0.0..=100.0, " %", Some(1.0)) {
                            self.studio.chaleur = chaleur / 100.0;
                            *apply = true;
                        }
                    },
                );

                // --- 7. Limiteur ---
                titre_bloc(
                    ui,
                    7,
                    "Limiteur",
                    "Le mur que rien ne franchit, pas même un cri : la dernière sécurité avant \
                     l'envoi. Toujours là.",
                    montrer,
                );
                parametre(
                    ui,
                    "Plafond",
                    "Le niveau maximum de ta voix. -1 dB est conseillé : de quoi laisser \
                     respirer le codec.",
                    montrer,
                    |ui| {
                        *apply |= curseur(ui, &mut self.studio.plafond_db, -12.0..=0.0, " dBFS", Some(0.5));
                    },
                );
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La porte de drion (« porte=0,1,150,150 ») : 0 dB de profondeur, donc
    /// inerte ; la chaîne d'avant le mode studio ne l'est pas.
    #[test]
    fn une_porte_a_zero_db_est_reconnue_comme_inerte() {
        assert!(porte_inerte("porte=0,1,150,150;deesser=-;comp=-22,3,6,5,120,0;chaleur=0.07;plafond=-1"));
        assert!(porte_inerte("porte=-3,2,150,150"));
        assert!(!porte_inerte("porte=-35,1,150,150;deesser=-"));
        assert!(!porte_inerte(&ecrire_studio(&ReglagesStudio::default())));
        assert!(!porte_inerte(""));
    }

    #[test]
    fn la_chaine_fait_l_aller_retour() {
        let defaut = ReglagesStudio::default();
        assert_eq!(lire_studio(&ecrire_studio(&defaut)), defaut);
        let perso = ReglagesStudio {
            porte: ReglagesPorte { profondeur_db: -20.0, attaque_ms: 1.5, maintien_ms: 300.0, relachement_ms: 80.0 },
            deesser: Some(ReglagesDeesser { frequence: 7_200.0, seuil_db: -24.0, reduction_max_db: 6.0 }),
            compresseur: ReglagesCompresseur {
                seuil_db: -18.0,
                ratio: 4.5,
                genou_db: 3.0,
                attaque_ms: 8.0,
                relachement_ms: 200.0,
                rattrapage_db: 4.0,
            },
            chaleur: 0.35,
            plafond_db: -2.0,
        };
        assert_eq!(lire_studio(&ecrire_studio(&perso)), perso);
        // Rien ou n'importe quoi : la chaîne d'avant.
        assert_eq!(lire_studio(""), defaut);
        assert_eq!(lire_studio("chaleur=abc;plafond=99"), ReglagesStudio { plafond_db: 0.0, ..defaut });
    }

    #[test]
    fn les_profils_font_l_aller_retour() {
        let p = ProfilVoix {
            nom: "Stream".into(),
            egaliseur: "ph:80:0:0.7071:1:1".into(),
            studio: ecrire_studio(&ReglagesStudio::default()),
            compression: COMPRESSION_PERSO,
            porte: 0.01,
            agc: true,
            agc_cible: 0.3,
        };
        assert_eq!(lire_profils(&ecrire_profils(std::slice::from_ref(&p))), vec![p]);
        assert!(lire_profils("pas du json").is_empty());
    }
}
