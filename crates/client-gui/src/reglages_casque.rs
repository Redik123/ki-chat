//! La page Casque des réglages : le casque dessiné, et ce qui se règle
//! vraiment sur lui.
//!
//! Un casque en jack n'a ni puce ni logiciel : ses réglages sont ceux de la
//! carte son où il est branché (`ki_voice::materiel`) — le volume de la
//! sortie, et les gains du micro, que le calibrage ajuste en cinq secondes.
//! S'y ajoute ce que ki-chat fait lui-même : le retour de sa voix dans le
//! casque, et l'égaliseur des voix qu'on entend.

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2};
use ki_voice::egaliseur::{Bande, Prereglage, PREREGLAGES_ECOUTE, PREREGLAGES_VOIX};
use ki_voice::materiel::{scinder_nom, EtatMateriel, Gain, Materiel, Ordre};

use crate::icons::Icon;
use crate::reglages_audio::curseur;
use crate::theme::{self, ACCENT, DANGER, SPEAK, TEXT, TEXT_DIM, TEXT_FAINT, WARN};
use crate::ui::{self, Tone};
use crate::egaliseur_ui;
use crate::{KiApp, VoiceSnapshot};

/// Le temps qu'on laisse parler fort pendant le calibrage.
const CALIBRAGE: Duration = Duration::from_secs(5);
/// Où poser le cri le plus fort : 6 dB sous la saturation. Le gain
/// automatique remonte la voix normale ; le limiteur tient le reste.
const CIBLE_DB: f32 = -6.0;
/// Ce qu'on retire quand la carte a saturé : la vraie crête est inconnue,
/// quelque part au-dessus du plafond — on redescend franchement, et l'on
/// affine au calibrage suivant.
const RECUL_SATURE_DB: f32 = -10.0;
/// En-deçà, le réglage est bon : on ne touche à rien.
const TOLERANCE_DB: f32 = 1.5;
/// Combien de temps le verdict du calibrage reste affiché.
const VERDICT: Duration = Duration::from_secs(40);
/// Au-delà, les gains de la carte font saturer un micro-casque ordinaire à
/// la moindre voix forte.
const GAIN_TROP_DB: f32 = 20.0;

/// Un calibrage en cours : depuis quand, la plus haute crête brute relevée,
/// et le compteur de saturations au départ.
pub(crate) struct Calibrage {
    debut: Instant,
    crete: f32,
    saturations: u64,
}

fn en_db(x: f32) -> f32 {
    20.0 * x.max(1e-6).log10()
}

/// Les gains à poser pour que le cri le plus fort arrive à `CIBLE_DB`.
///
/// `crete` : la plus haute crête brute de l'essai (0 à 1) ; `sature` : la
/// carte a écrêté. `niveau` : (dB, min, max) du niveau du micro ; `ampli` :
/// l'amplification, s'il y en a une ; `autres_db` : d'éventuels gains en plus,
/// laissés tels quels. Rend le niveau et l'amplification à poser, ou `None`
/// quand le réglage est déjà bon.
///
/// L'amplification d'abord : c'est le réglage caché du panneau Windows,
/// celui qu'on oublie — on la prend la plus basse possible, et le niveau,
/// visible partout, fait le reste.
fn gains_pour(
    crete: f32,
    sature: bool,
    niveau: (f32, f32, f32),
    ampli: Option<&Gain>,
    autres_db: f32,
) -> Option<(f32, Option<f32>)> {
    let ecart = if sature { RECUL_SATURE_DB } else { CIBLE_DB - en_db(crete) };
    if ecart.abs() < TOLERANCE_DB {
        return None;
    }
    let (n, n_min, n_max) = niveau;
    let voulu = n + ampli.map_or(0.0, |g| g.db) + autres_db + ecart;
    let reste = voulu - autres_db;
    match ampli {
        None => Some((reste.clamp(n_min, n_max), None)),
        Some(g) => {
            let pas = if g.pas_db > 0.0 { g.pas_db } else { 1.0 };
            let mut b = g.min_db;
            while reste - b > n_max && b + pas <= g.max_db + 1e-3 {
                b += pas;
            }
            Some(((reste - b).clamp(n_min, n_max), Some(b)))
        }
    }
}

/// Une sortie de câble audio virtuel, d'après son nom Windows : VB-Cable
/// (« CABLE Input », ou ce que porte le pilote « VB-Audio Virtual Cable »).
fn est_cable(nom: &str) -> bool {
    let n = nom.to_lowercase();
    n.contains("cable input") || n.contains("cable in ") || n.contains("vb-audio virtual cable")
}

/// L'entrée d'un câble virtuel : ce que le jeu prend pour micro — et que
/// ki-chat ne doit surtout pas prendre pour le sien.
fn est_entree_de_cable(nom: &str) -> bool {
    let n = nom.to_lowercase();
    n.contains("cable output") || n.contains("vb-audio virtual cable")
}

/// Le câble à prendre d'office : « CABLE Input » s'il porte ce nom, sinon la
/// sortie du pilote VB-Cable en stéréo plutôt que sa variante 16 canaux.
pub(crate) fn cable_virtuel(sorties: &[String]) -> Option<String> {
    let cables: Vec<&String> = sorties.iter().filter(|s| est_cable(s)).collect();
    cables
        .iter()
        .find(|s| s.to_lowercase().contains("cable input"))
        .or_else(|| cables.iter().find(|s| !s.to_lowercase().contains("16 ch")))
        .or_else(|| cables.first())
        .map(|s| (*s).clone())
}

impl KiApp {
    /// La sortie où envoyer la voix pour les jeux, telle que le moteur la
    /// recevra : `None` quand l'envoi est coupé, sans câble, ou quand le micro
    /// de ki-chat est lui-même l'entrée d'un câble — la voix bouclerait sur
    /// elle-même.
    pub(crate) fn micro_jeux_resolu(&self) -> Option<String> {
        if !self.micro_jeux || self.pref_input.as_deref().is_some_and(est_entree_de_cable) {
            return None;
        }
        // La liste des sorties n'est relevée qu'à l'ouverture des réglages :
        // au lancement de ki-chat, elle est vide, et le micro pour les jeux ne
        // retrouvait pas son câble à la connexion. On la lit alors ici — à la
        // connexion et aux redémarrages du moteur seulement.
        let releve;
        let sorties = if self.output_devices.is_empty() {
            releve = ki_voice::list_devices().1;
            &releve
        } else {
            &self.output_devices
        };
        self.micro_jeux_sortie
            .clone()
            .filter(|s| sorties.contains(s))
            .or_else(|| cable_virtuel(sorties))
    }

    /// Le contenu de l'onglet. `apply` : un réglage du moteur a changé ;
    /// `restart` : il faut relancer le moteur (le micro pour les jeux ouvre
    /// sa sortie au démarrage).
    pub(crate) fn onglet_casque(
        &mut self,
        ui: &mut egui::Ui,
        voice: &VoiceSnapshot,
        apply: &mut bool,
        restart: &mut bool,
    ) {
        let materiel = Materiel::global();
        let suivi = (self.pref_input.clone(), self.pref_output.clone());
        if self.materiel_suivi.as_ref() != Some(&suivi) {
            materiel.ordonner(Ordre::Suivre { entree: suivi.0.clone(), sortie: suivi.1.clone() });
            self.materiel_suivi = Some(suivi);
        }
        materiel.tenir_eveille();
        let etat = materiel.etat();
        // Le dessin et les jauges vivent : une image toutes les 33 ms tant
        // que la page est ouverte.
        ui.ctx().request_repaint_after(Duration::from_millis(33));
        self.avancer_calibrage(voice, &etat);
        let engine_up = voice.engine_up;

        // Deux façons de régler : l'essentiel, ou toute la chaîne. Les deux
        // règlent les mêmes choses — ce qu'on fait en studio reste actif en
        // simple, seulement plus affiché.
        ui.horizontal_wrapped(|ui| {
            ui::segmente(ui, &mut self.mode_studio, &[(false, "Simple"), (true, "Studio")]);
            ui.label(
                RichText::new(if self.mode_studio {
                    "toute la chaîne de ta voix, réglage par réglage"
                } else {
                    "l'essentiel, en préréglages"
                })
                .color(TEXT_FAINT)
                .size(11.5),
            );
        });
        ui.add_space(10.0);

        self.carte_casque(ui, voice, &etat);

        // --- Micro ----------------------------------------------------
        ui::section(
            ui,
            Icon::Mic,
            "Micro",
            Some(
                "Les gains de la carte son, avant ki-chat. Trop hauts, ta voix sature quand tu \
                 parles fort — et rien ne répare ensuite ce qui a été coupé.",
            ),
            |ui| {
                ui::ligne(ui, "Marge", |ui| {
                    let crete = voice.stats.crete_brute;
                    let crete_db = en_db(crete);
                    let couleur = if !engine_up {
                        theme::BG_ACTIVE
                    } else if crete >= 0.985 || voice.stats.micro_sature {
                        DANGER
                    } else if crete_db > CIBLE_DB {
                        WARN
                    } else {
                        SPEAK
                    };
                    ui.horizontal(|ui| {
                        let largeur = (ui.available_width() - 90.0).clamp(120.0, 300.0);
                        ui::meter_with_threshold(
                            ui,
                            ((crete_db + 60.0) / 60.0).clamp(0.0, 1.0),
                            Some((CIBLE_DB + 60.0) / 60.0),
                            Vec2::new(largeur, 10.0),
                            couleur,
                        );
                        let texte = if !engine_up {
                            "vocal inactif".to_string()
                        } else if crete < 0.001 {
                            "silence".to_string()
                        } else {
                            format!("{crete_db:+.0} dB")
                        };
                        ui.label(RichText::new(texte).color(TEXT_DIM).size(11.5));
                    });
                    ui::precision(
                        ui,
                        "Ton micro tel que la carte son le livre. Parle fort : la barre doit \
                         s'arrêter avant le repère — tout au bout, c'est la saturation.",
                    );
                });

                let Some(niveau) = etat.entree.clone().filter(|_| etat.disponible) else {
                    ui::precision(
                        ui,
                        if cfg!(windows) {
                            "Les gains de ce micro ne se règlent pas d'ici."
                        } else {
                            "Les gains de la carte son se règlent depuis Windows seulement."
                        },
                    );
                    return;
                };
                ui::ligne(ui, "Niveau", |ui| {
                    let mut pct = niveau.scalaire * 100.0;
                    if curseur(ui, &mut pct, 0.0..=100.0, " %", Some(1.0)) {
                        materiel.ordonner(Ordre::NiveauMicro(pct / 100.0));
                    }
                    ui::precision(
                        ui,
                        &format!(
                            "{:+.1} dB — le volume du micro dans Windows. Il règle ta marge avant \
                             la saturation, pas ton volume chez les autres.",
                            niveau.db
                        ),
                    );
                });
                for g in &etat.amplis {
                    ui::ligne(ui, "Amplification", |ui| {
                        let mut v = g.db;
                        let pas = if g.pas_db > 0.0 { g.pas_db as f64 } else { 1.0 };
                        if curseur(ui, &mut v, g.min_db..=g.max_db, " dB", Some(pas)) {
                            materiel.ordonner(Ordre::Ampli { id: g.id.clone(), db: v });
                        }
                        ui::precision(
                            ui,
                            &format!(
                                "« {} » : un gain en plus du niveau, caché dans le panneau de \
                                 Windows.",
                                g.nom
                            ),
                        );
                    });
                }
                let total = niveau.db + etat.amplis.iter().map(|g| g.db).sum::<f32>();
                // Le piège : monter ces gains pour « parler plus fort ». Vu
                // chez drion le 29/09 : +32 dB, une voix « caverneuse » et pas
                // plus forte pour autant.
                if total > GAIN_TROP_DB {
                    ui::banner(
                        ui,
                        Tone::Warn,
                        &format!(
                            "{total:+.0} dB de gain en tout, c'est beaucoup pour un micro-casque : ta \
                             voix sature dès que tu hausses le ton, et le fond de la pièce monte avec \
                             elle — la voix « de cave ». Et ça ne te rend pas plus fort chez les \
                             autres : le gain automatique te ramène toujours au même niveau. Clique \
                             « Régler mon micro », puis règle « Ton volume » juste en dessous."
                        ),
                        false,
                    );
                    ui.add_space(8.0);
                }
                ui::ligne(ui, "Calibrer", |ui| {
                    self.calibrage_ui(ui, voice, total);
                });
                // Le vrai bouton du volume chez les autres.
                ui::ligne(ui, "Ton volume", |ui| {
                    if self.agc {
                        let mut pct = self.agc_target * 100.0;
                        if curseur(ui, &mut pct, 15.0..=50.0, " %", Some(1.0)) {
                            self.agc_target = pct / 100.0;
                            *apply = true;
                        }
                        ui::precision(
                            ui,
                            "Ce que les autres entendent de toi. Pour être plus fort, c'est ici — \
                             pas dans les gains de la carte, que le gain automatique compense.",
                        );
                    } else {
                        let mut pct = self.input_gain * 100.0;
                        if curseur(ui, &mut pct, 0.0..=200.0, " %", Some(1.0)) {
                            self.input_gain = pct / 100.0;
                            *apply = true;
                        }
                        ui::precision(
                            ui,
                            "Le gain automatique est coupé : ton volume chez les autres suit ce \
                             gain, et ceux de la carte.",
                        );
                    }
                });
            },
        );

        // --- Micro pour les jeux ---------------------------------------
        ui::section(
            ui,
            Icon::Send,
            "Micro pour les jeux",
            Some(
                "Ta voix traitée par ki-chat — débruitage, gain, compression — comme micro \
                 dans Valorant ou n'importe quel jeu, qui n'en fait rien de tout ça.",
            ),
            |ui| {
                if !cfg!(windows) {
                    ui::precision(ui, "Windows seulement pour l'instant.");
                    return;
                }
                let cables: Vec<String> = self.output_devices.iter().filter(|d| est_cable(d)).cloned().collect();
                if cables.is_empty() {
                    ui::banner(
                        ui,
                        Tone::Info,
                        "Il faut un câble audio virtuel : VB-Cable, gratuit. Installe-le (Windows \
                         demande de redémarrer), puis clique sur Actualiser.",
                        false,
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui::button(ui, Icon::Download, "Télécharger VB-Cable").clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab("https://vb-audio.com/Cable/"));
                        }
                        if ui::button(ui, Icon::Refresh, "Actualiser").clicked() {
                            let (entrees, sorties) = ki_voice::list_devices();
                            self.input_devices = entrees;
                            self.output_devices = sorties;
                        }
                    });
                    return;
                }
                let boucle = self.pref_input.as_deref().is_some_and(est_entree_de_cable);
                ui::ligne(ui, "Envoi", |ui| {
                    if ui::interrupteur(ui, &mut self.micro_jeux, "Envoyer ma voix aux jeux").changed() {
                        *restart = true;
                    }
                    if self.micro_jeux {
                        let (couleur, texte) = if boucle {
                            (DANGER, "coupé : le micro de ki-chat est le câble lui-même")
                        } else if !engine_up {
                            (TEXT_FAINT, "actif dès que tu es connecté à un serveur")
                        } else if voice.stats.micro_jeux_actif {
                            (SPEAK, "ta voix part dans le câble")
                        } else {
                            (WARN, "ouverture du câble…")
                        };
                        ui.add_space(4.0);
                        ui.horizontal(|ui| ui::status_dot(ui, couleur, texte, 10.0));
                    }
                    ui::precision(
                        ui,
                        "ki-chat doit rester ouvert et connecté — réduit dans la barre des \
                         tâches, ça suffit. Environ 100 ms de retard, comme un vocal en ligne.",
                    );
                });
                if !self.micro_jeux {
                    return;
                }
                ui::ligne(ui, "Câble", |ui| {
                    let auto = cable_virtuel(&self.output_devices).unwrap_or_default();
                    let actuel = self
                        .micro_jeux_sortie
                        .clone()
                        .unwrap_or_else(|| format!("Automatique ({auto})"));
                    let largeur = (ui.available_width() - 10.0).clamp(160.0, 340.0);
                    egui::ComboBox::from_id_salt("micro_jeux_cable")
                        .width(largeur)
                        .selected_text(RichText::new(actuel).color(TEXT))
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(self.micro_jeux_sortie.is_none(), format!("Automatique ({auto})"))
                                .clicked()
                                && self.micro_jeux_sortie.is_some()
                            {
                                self.micro_jeux_sortie = None;
                                *restart = true;
                            }
                            for c in &cables {
                                let choisi = self.micro_jeux_sortie.as_deref() == Some(c.as_str());
                                if ui.selectable_label(choisi, c).clicked() && !choisi {
                                    self.micro_jeux_sortie = Some(c.clone());
                                    *restart = true;
                                }
                            }
                        });
                });
                ui::ligne(ui, "Dans le jeu", |ui| {
                    let entree = self
                        .input_devices
                        .iter()
                        .find(|e| e.to_lowercase().contains("cable output"))
                        .or_else(|| self.input_devices.iter().find(|e| est_entree_de_cable(e)))
                        .cloned()
                        .unwrap_or_else(|| "CABLE Output (VB-Audio Virtual Cable)".into());
                    ui.label(
                        RichText::new(format!("Prends « {entree} » comme micro."))
                            .color(TEXT)
                            .size(12.5),
                    );
                    ui::precision(
                        ui,
                        "Valorant : Paramètres → Audio → Chat vocal → Périphérique d'entrée. \
                         Laisse le volume d'entrée du jeu à 100 % : ki-chat règle déjà le \
                         niveau. Et jamais ce câble comme micro de ki-chat ni comme micro \
                         par défaut de Windows : ta voix tournerait en rond.",
                    );
                });
            },
        );

        // --- Changeur de voix ---------------------------------------------
        self.changeur_ui(ui, apply);

        // --- Casque ---------------------------------------------------
        ui::section(
            ui,
            Icon::Headphones,
            "Casque",
            Some("Le volume de la sortie, et ta propre voix dedans."),
            |ui| {
                match etat.sortie.clone().filter(|_| etat.disponible) {
                    Some(v) => {
                        ui::ligne(ui, "Volume", |ui| {
                            let mut pct = v.scalaire * 100.0;
                            if curseur(ui, &mut pct, 0.0..=100.0, " %", Some(1.0)) {
                                materiel.ordonner(Ordre::VolumeSortie(pct / 100.0));
                            }
                            ui::precision(ui, "Le volume Windows de la sortie casque — celui des jeux aussi.");
                        });
                        if let Some(b) = v.balance {
                            ui::ligne(ui, "Balance", |ui| {
                                let mut pct = b * 100.0;
                                ui.spacing_mut().slider_width = (ui.available_width() - 76.0).clamp(120.0, 260.0);
                                let r = ui.add(
                                    egui::Slider::new(&mut pct, -100.0..=100.0)
                                        .step_by(1.0)
                                        .custom_formatter(|v, _| {
                                            if v.abs() < 0.5 {
                                                "centre".into()
                                            } else if v < 0.0 {
                                                format!("G {:.0}", -v)
                                            } else {
                                                format!("D {v:.0}")
                                            }
                                        }),
                                );
                                if r.changed() {
                                    materiel.ordonner(Ordre::Balance(pct / 100.0));
                                }
                                if r.double_clicked() {
                                    materiel.ordonner(Ordre::Balance(0.0));
                                }
                                ui::precision(ui, "Double-clic pour recentrer.");
                            });
                        }
                    }
                    None if cfg!(windows) => {
                        ui::precision(ui, "Le volume de cette sortie ne se règle pas d'ici.");
                    }
                    None => {}
                }
                ui::ligne(ui, "Ta voix", |ui| {
                    if ui::interrupteur(ui, &mut self.retour_voix, "T'entendre dans le casque").changed() {
                        *apply = true;
                    }
                    if self.retour_voix {
                        ui.add_space(6.0);
                        let mut pct = self.retour_voix_volume * 100.0;
                        if curseur(ui, &mut pct, 0.0..=150.0, " %", Some(1.0)) {
                            self.retour_voix_volume = pct / 100.0;
                            *apply = true;
                        }
                        if !engine_up {
                            ui.label(
                                RichText::new("actif dès que tu es connecté à un serveur")
                                    .color(WARN)
                                    .size(11.5),
                            );
                        }
                    }
                    ui::precision(
                        ui,
                        "Un casque fermé t'isole de ta propre voix : on parle plus fort sans \
                         s'en rendre compte. Ton micro revient ici, brut et au plus court, pour \
                         doser ta voix — au casque seulement : sur haut-parleurs, il \
                         sifflerait. Changeur de voix allumé, c'est ta voix changée que tu \
                         entends, avec un peu plus de retard. Pour entendre ce que reçoivent les \
                         autres, c'est « M'écouter » (onglet Audio).",
                    );
                });
            },
        );

        // --- Égaliseurs -----------------------------------------------
        ui::section(
            ui,
            Icon::Sliders,
            "Égaliseur",
            Some("Ta voix telle qu'elle part, ou les voix que tu entends — réglées comme en studio."),
            |ui| {
                ui::ligne(ui, "Pour", |ui| {
                    if ui::segmente(
                        ui,
                        &mut self.egaliseur_vue,
                        &[(VUE_TA_VOIX, "Ta voix"), (VUE_LES_AUTRES, "Ce que tu entends")],
                    ) {
                        self.eq_editeur.selection = None;
                        if self.eq_editeur.comparer {
                            self.eq_editeur.comparer = false;
                            *apply = true;
                        }
                    }
                    ui::precision(
                        ui,
                        if self.egaliseur_vue == VUE_TA_VOIX && !self.mode_studio {
                            "Ce que les autres entendent de toi, dans ki-chat comme dans les \
                             jeux. « Micro-casque » coupe sous la voix — le grondement, l'effet \
                             de proximité — sans toucher à son corps : la « cave » sans le « nez \
                             bouché ». Écoute-toi avec « M'écouter » (onglet Audio)."
                        } else if self.egaliseur_vue == VUE_TA_VOIX {
                            "Ce que les autres entendent de toi, dans ki-chat comme dans les \
                             jeux. Derrière la courbe, ta voix en direct : parle, et regarde où \
                             elle gonfle. « Micro-casque » coupe sous la voix — le grondement, \
                             l'effet de proximité — sans toucher à son corps : la « cave » sans \
                             le « nez bouché ». Écoute-toi avec « M'écouter » (onglet Audio), et \
                             « Comparer » pour entendre la différence."
                        } else {
                            "Les voix que tu entends dans ki-chat — ni les jeux, ni tes \
                             notifications. Derrière la courbe, les voix reçues en direct."
                        },
                    );
                });
                let ta_voix = self.egaliseur_vue == VUE_TA_VOIX;
                let (bandes, prereglages): (&mut Vec<Bande>, &[Prereglage]) = if ta_voix {
                    (&mut self.egaliseur_micro, &PREREGLAGES_VOIX)
                } else {
                    (&mut self.egaliseur, &PREREGLAGES_ECOUTE)
                };
                if !self.mode_studio {
                    // Le mode simple : les préréglages, en pastilles.
                    ui::ligne(ui, "Préréglage", |ui| {
                        let mut choix = prereglages
                            .iter()
                            .position(|(_, fabrique)| fabrique() == *bandes)
                            .unwrap_or(usize::MAX);
                        let options: Vec<(usize, &str)> =
                            prereglages.iter().enumerate().map(|(i, (nom, _))| (i, *nom)).collect();
                        if ui::segmente(ui, &mut choix, &options) {
                            if let Some((_, fabrique)) = prereglages.get(choix) {
                                *bandes = fabrique();
                                *apply = true;
                            }
                        }
                        if choix == usize::MAX {
                            ui::precision(ui, "Réglage personnel — il se modifie en mode studio.");
                        }
                    });
                    return;
                }
                // Le spectre : sa voix telle qu'elle part, ou les voix reçues.
                self.analyse_voulue = if ta_voix { ki_voice::ANALYSE_MICRO } else { ki_voice::ANALYSE_VOIX };
                let echantillons = self
                    .link
                    .engine
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|e| e.analyse(egaliseur_ui::FENETRE))
                    .unwrap_or_default();
                let dt = ui.input(|i| i.stable_dt);
                self.eq_editeur.analyseur.nourrir(&echantillons, dt);
                if egaliseur_ui::editeur(ui, bandes, &mut self.eq_editeur, prereglages) {
                    *apply = true;
                }
            },
        );

        // --- La chaîne studio ------------------------------------------
        if self.mode_studio {
            self.chaine_studio_ui(ui, voice, apply);
        }
    }

    /// Les égaliseurs tels que le moteur doit les jouer : (écoute, sa voix).
    /// « Comparer » coupe celui que la page montre, le temps de l'écoute.
    pub(crate) fn egaliseurs_effectifs(&self) -> (Vec<Bande>, Vec<Bande>) {
        let compare = self.eq_editeur.comparer;
        let ecoute = if compare && self.egaliseur_vue == VUE_LES_AUTRES { Vec::new() } else { self.egaliseur.clone() };
        let voix = if compare && self.egaliseur_vue == VUE_TA_VOIX { Vec::new() } else { self.egaliseur_micro.clone() };
        (ecoute, voix)
    }

    /// La carte du haut : le casque dessiné, son nom, où il est branché.
    fn carte_casque(&mut self, ui: &mut egui::Ui, voice: &VoiceSnapshot, etat: &EtatMateriel) {
        let voix = (voice.levels.values().fold(0f32, |m, &l| m.max(l)) * 3.0).min(1.0);
        let micro = (voice.stats.mic_peak * 3.0).min(1.0);
        let sature = voice.stats.micro_sature;
        let coupe = self.muted || self.sourd;
        ui::card(ui, |ui| {
            ui.horizontal(|ui| {
                dessiner_casque(ui, Vec2::new(190.0, 150.0), voix, micro, sature, coupe);
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    ui.add_space(10.0);
                    let defaut = etat
                        .sortie_nom
                        .as_deref()
                        .map(|n| scinder_nom(n).0)
                        .unwrap_or("Mon casque")
                        .to_string();
                    ui.add(
                        egui::TextEdit::singleline(&mut self.casque_nom)
                            .hint_text(RichText::new(defaut).color(TEXT_DIM))
                            .font(egui::FontId::proportional(20.0))
                            .text_color(TEXT)
                            .desired_width(ui.available_width().min(240.0))
                            .frame(false),
                    )
                    .on_hover_text("clique pour lui donner son nom");
                    ui.add_space(6.0);
                    for (icone, nom) in [(Icon::Headphones, &etat.sortie_nom), (Icon::Mic, &etat.entree_nom)] {
                        if let Some(nom) = nom {
                            ui.horizontal_wrapped(|ui| {
                                ui::glyph(ui, icone, 14.0, TEXT_DIM);
                                let (point, carte) = scinder_nom(nom);
                                ui.label(RichText::new(point).color(TEXT).size(12.5));
                                if let Some(carte) = carte {
                                    ui.label(RichText::new(format!("sur {carte}")).color(TEXT_FAINT).size(12.0));
                                }
                            });
                        }
                    }
                    ui.add_space(8.0);
                    let (couleur, texte) = if !voice.engine_up {
                        (TEXT_FAINT, "vocal inactif")
                    } else if coupe {
                        (WARN, "micro coupé")
                    } else if sature {
                        (DANGER, "ton micro sature")
                    } else if voice.stats.mic_peak > 0.02 {
                        (SPEAK, "tu parles")
                    } else {
                        (TEXT_DIM, "prêt")
                    };
                    ui.horizontal(|ui| ui::status_dot(ui, couleur, texte, 10.0));
                });
            });
        });
        ui.add_space(12.0);
    }

    /// Le calibrage des gains : le bouton, sa progression, son verdict.
    fn calibrage_ui(&mut self, ui: &mut egui::Ui, voice: &VoiceSnapshot, total_db: f32) {
        match &self.calibrage_micro {
            None => {
                let pret = voice.engine_up;
                ui.add_enabled_ui(pret, |ui| {
                    if ui::button(ui, Icon::Target, "Régler mon micro (5 s)")
                        .on_hover_text("parle aussi fort qu'en jeu — crie un bon coup")
                        .clicked()
                    {
                        if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
                            // Remise à zéro de la crête relevée.
                            let _ = engine.crete_brute_max();
                        }
                        self.calibrage_micro = Some(Calibrage {
                            debut: Instant::now(),
                            crete: 0.0,
                            saturations: voice.stats.saturations,
                        });
                        self.calibrage_verdict = None;
                    }
                });
                ui::precision(
                    ui,
                    &if pret {
                        format!(
                            "Clique, puis parle aussi fort qu'en jeu pendant 5 secondes. ki-chat \
                             règle les gains pour que ton cri le plus fort reste 6 dB sous la \
                             saturation ; le gain automatique remonte ta voix normale. Gain \
                             actuel : {total_db:+.0} dB."
                        )
                    } else {
                        "Connecte-toi à un serveur : c'est là que ton micro s'ouvre.".to_string()
                    },
                );
            }
            Some(cal) => {
                let progression = (cal.debut.elapsed().as_secs_f32() / CALIBRAGE.as_secs_f32()).min(1.0);
                let crete = cal.crete;
                ui.horizontal(|ui| {
                    ui::meter(ui, progression, Vec2::new(150.0, 8.0), ACCENT);
                    let texte = if crete < 0.001 {
                        "parle fort…".to_string()
                    } else {
                        format!("parle fort… crête {:+.0} dB", en_db(crete))
                    };
                    ui.label(RichText::new(texte).color(TEXT_DIM).size(12.0));
                    if ui::icon_button(ui, Icon::Close, "Annuler").clicked() {
                        self.calibrage_micro = None;
                    }
                });
            }
        }
        if let Some((quand, texte)) = &self.calibrage_verdict {
            if quand.elapsed() < VERDICT {
                ui.add_space(6.0);
                ui::banner(ui, Tone::Info, texte, false);
            }
        }
    }

    /// Relève la crête du calibrage en cours et, les 5 secondes passées,
    /// règle les gains et rend son verdict.
    fn avancer_calibrage(&mut self, voice: &VoiceSnapshot, etat: &EtatMateriel) {
        let Some(cal) = self.calibrage_micro.as_mut() else { return };
        if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
            cal.crete = cal.crete.max(engine.crete_brute_max());
        }
        if cal.debut.elapsed() < CALIBRAGE {
            return;
        }
        let Some(cal) = self.calibrage_micro.take() else { return };
        let sature = voice.stats.saturations > cal.saturations || cal.crete >= 0.985;
        let texte = self.conclure_calibrage(cal.crete, sature, etat);
        self.calibrage_verdict = Some((Instant::now(), texte));
    }

    fn conclure_calibrage(&self, crete: f32, sature: bool, etat: &EtatMateriel) -> String {
        if crete < 0.003 {
            return "Rien entendu. Ton micro est-il coupé (bouton du casque, perche relevée) ? \
                    Vérifie aussi que c'est bien lui qui est choisi dans l'onglet Audio."
                .into();
        }
        let constat = if sature {
            "Ton micro saturait : ton cri le plus fort touchait le plafond.".to_string()
        } else {
            format!("Ton cri le plus fort arrivait à {:+.0} dB.", en_db(crete))
        };
        let Some(niveau) = etat.entree.as_ref().filter(|_| etat.disponible) else {
            return format!("{constat} Ce micro ne se règle pas d'ici : baisse son niveau à la source.");
        };
        let ampli = etat.amplis.first();
        let autres: f32 = etat.amplis.iter().skip(1).map(|g| g.db).sum();
        match gains_pour(crete, sature, (niveau.db, niveau.min_db, niveau.max_db), ampli, autres) {
            None => format!("{constat} C'est bien réglé : rien à changer."),
            Some((nouveau_niveau, nouvel_ampli)) => {
                let materiel = Materiel::global();
                let mut changements = Vec::new();
                if let (Some(g), Some(b)) = (ampli, nouvel_ampli) {
                    if (g.db - b).abs() > 0.05 {
                        materiel.ordonner(Ordre::Ampli { id: g.id.clone(), db: b });
                        changements.push(format!("amplification {:+.0} → {b:+.0} dB", g.db));
                    }
                }
                if (niveau.db - nouveau_niveau).abs() > 0.05 {
                    materiel.ordonner(Ordre::NiveauMicroDb(nouveau_niveau));
                    changements.push(format!("niveau {:+.1} → {nouveau_niveau:+.1} dB", niveau.db));
                }
                if changements.is_empty() {
                    return format!(
                        "{constat} Les gains sont déjà au bout de leur plage : rien de plus à \
                         faire d'ici."
                    );
                }
                let suite = if sature {
                    "Recommence en parlant aussi fort pour affiner."
                } else {
                    "Ton cri le plus fort arrivera vers -6 dB, sous la saturation."
                };
                format!("{constat} Réglé : {}. {suite}", changements.join(", "))
            }
        }
    }
}

/// Les deux égaliseurs de la page : celui de sa voix, celui des voix reçues.
pub(crate) const VUE_TA_VOIX: u8 = 0;
pub(crate) const VUE_LES_AUTRES: u8 = 1;

/// Le casque dessiné : arceau, oreillettes, perche du micro. Les oreillettes
/// s'allument avec les voix qu'on entend, la capsule du micro avec la sienne
/// — verte quand on parle, rouge quand la carte sature, barrée quand on est
/// coupé. Un dessin à nous : pas la photo ni le logo d'une marque.
fn dessiner_casque(ui: &mut egui::Ui, taille: Vec2, voix: f32, micro: f32, sature: bool, coupe: bool) {
    let (rect, _) = ui.allocate_exact_size(taille, Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let p = ui.painter();
    let (w, h) = (rect.width(), rect.height());
    let cx = rect.center().x;
    let coque = theme::BG_ACTIVE;
    let arete = theme::BORDER_STRONG;
    let creux = theme::BG_DEEP;
    let reflet = theme::alpha(Color32::WHITE, 24);

    // L'arceau : un arc au-dessus des oreillettes, son coussin, son reflet.
    let centre_arc = Pos2::new(cx, rect.top() + h * 0.60);
    let r = w * 0.34;
    let arc = |rayon: f32, marge: f32| -> Vec<Pos2> {
        (0..=48)
            .map(|i| {
                let a = std::f32::consts::PI + marge + (std::f32::consts::PI - 2.0 * marge) * i as f32 / 48.0;
                centre_arc + Vec2::new(a.cos(), a.sin()) * rayon
            })
            .collect()
    };
    p.add(Shape::line(arc(r, 0.12), Stroke::new(w * 0.06, coque)));
    p.add(Shape::line(arc(r - w * 0.034, 0.34), Stroke::new(w * 0.028, creux)));
    p.add(Shape::line(arc(r + w * 0.026, 0.16), Stroke::new(1.2_f32, reflet)));

    // Les oreillettes, et les étriers qui les pendent à l'arceau.
    let taille_ore = Vec2::new(w * 0.19, h * 0.44);
    let y_ore = centre_arc.y + h * 0.04;
    for cote in [-1.0f32, 1.0] {
        let c = Pos2::new(cx + cote * r, y_ore);
        let bout_arc = centre_arc + Vec2::new(cote * r * 0.12f32.cos(), -r * 0.12f32.sin());
        p.line_segment([bout_arc, c - Vec2::new(0.0, taille_ore.y * 0.45)], Stroke::new(w * 0.03, coque));
        if voix > 0.02 {
            ui::glow(p, c, taille_ore.y * (0.55 + 0.45 * voix), theme::alpha(ACCENT, (30.0 + 80.0 * voix) as u8));
        }
        let coquille = Rect::from_center_size(c, taille_ore);
        let rond = CornerRadius::same((taille_ore.x * 0.5).min(255.0) as u8);
        p.rect_filled(coquille, rond, coque);
        p.rect_stroke(coquille, rond, Stroke::new(1.0_f32, arete), egui::StrokeKind::Inside);
        // Le coussin, côté tête.
        let coussin = Rect::from_center_size(
            c + Vec2::new(-cote * taille_ore.x * 0.22, 0.0),
            Vec2::new(taille_ore.x * 0.5, taille_ore.y * 0.84),
        );
        p.rect_filled(coussin, CornerRadius::same((coussin.width() * 0.5).min(255.0) as u8), creux);
        // Le liseré, côté extérieur : il s'allume avec les voix.
        let lisere = Rect::from_center_size(
            c + Vec2::new(cote * taille_ore.x * 0.24, 0.0),
            Vec2::new(2.5, taille_ore.y * 0.5),
        );
        p.rect_filled(lisere, CornerRadius::same(1), theme::alpha(ACCENT, (50.0 + 200.0 * voix) as u8));
    }

    // La perche du micro, de l'oreillette gauche vers la bouche.
    let gauche = Pos2::new(cx - r, y_ore);
    let depart = gauche + Vec2::new(taille_ore.x * 0.1, taille_ore.y * 0.32);
    let bout = Pos2::new(cx - w * 0.06, rect.bottom() - h * 0.07);
    let controle = Pos2::new(gauche.x + w * 0.02, rect.bottom() - h * 0.01);
    let perche: Vec<Pos2> = (0..=24)
        .map(|i| {
            let t = i as f32 / 24.0;
            let u = 1.0 - t;
            Pos2::new(
                u * u * depart.x + 2.0 * u * t * controle.x + t * t * bout.x,
                u * u * depart.y + 2.0 * u * t * controle.y + t * t * bout.y,
            )
        })
        .collect();
    p.add(Shape::line(perche.clone(), Stroke::new(4.0_f32, coque)));
    p.add(Shape::line(perche, Stroke::new(1.0_f32, reflet)));
    let couleur = if coupe {
        TEXT_FAINT
    } else if sature {
        DANGER
    } else if micro > 0.05 {
        SPEAK
    } else {
        TEXT_DIM
    };
    if !coupe && (sature || micro > 0.05) {
        ui::glow(p, bout, w * (0.06 + 0.08 * micro), theme::alpha(couleur, 110));
    }
    p.circle_filled(bout, w * 0.036, creux);
    p.circle_filled(bout, w * 0.022, couleur);
    if coupe {
        let d = w * 0.045;
        p.line_segment([bout + Vec2::new(-d, -d), bout + Vec2::new(d, d)], Stroke::new(2.0_f32, DANGER));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ampli_drion() -> Gain {
        Gain { nom: "Ampli microphone".into(), db: 10.0, min_db: 0.0, max_db: 30.0, pas_db: 10.0, id: "x".into() }
    }

    /// Le cas de drion : +12 dB de niveau, +10 d'amplification, et ça sature.
    /// On coupe l'amplification d'abord, le niveau garde sa place.
    #[test]
    fn un_micro_qui_sature_perd_d_abord_son_amplification() {
        let g = ampli_drion();
        let (niveau, ampli) = gains_pour(1.0, true, (12.0, -17.25, 12.0), Some(&g), 0.0).unwrap();
        assert_eq!(ampli, Some(0.0));
        assert!((niveau - 12.0).abs() < 1e-4, "niveau {niveau}");
    }

    /// Une crête à -1 dB : 5 dB de trop. L'amplification ne peut descendre
    /// que par 10 : le niveau rattrape la différence.
    #[test]
    fn le_niveau_rattrape_le_pas_de_l_amplification() {
        let g = ampli_drion();
        let crete = 10f32.powf(-1.0 / 20.0);
        let (niveau, ampli) = gains_pour(crete, false, (12.0, -17.25, 12.0), Some(&g), 0.0).unwrap();
        // 22 dB en tout, 17 voulus : 0 d'amplification ne suffit pas au
        // niveau (plafonné à 12) — on garde 10, et le niveau passe à 7.
        assert_eq!(ampli, Some(10.0));
        assert!((niveau - 7.0).abs() < 0.01, "niveau {niveau}");
    }

    /// Un micro trop faible remonte : le niveau d'abord, l'amplification
    /// seulement s'il n'y suffit pas.
    #[test]
    fn un_micro_trop_faible_remonte() {
        let g = Gain { db: 0.0, ..ampli_drion() };
        let crete = 10f32.powf(-30.0 / 20.0);
        let (niveau, ampli) = gains_pour(crete, false, (0.0, -17.25, 12.0), Some(&g), 0.0).unwrap();
        // 24 dB à gagner : 20 d'amplification, 4 de niveau.
        assert_eq!(ampli, Some(20.0));
        assert!((niveau - 4.0).abs() < 0.01, "niveau {niveau}");
        // Sans amplification, le niveau seul, dans sa plage.
        let (niveau, ampli) = gains_pour(crete, false, (0.0, -17.25, 12.0), None, 0.0).unwrap();
        assert_eq!((niveau, ampli), (12.0, None));
    }

    /// Déjà bien réglé : on ne touche à rien.
    #[test]
    fn un_micro_bien_regle_ne_bouge_pas() {
        let crete = 10f32.powf(-6.5 / 20.0);
        assert!(gains_pour(crete, false, (0.0, -17.25, 12.0), Some(&ampli_drion()), 0.0).is_none());
    }

    /// Les sorties du PC de drion : un Windows français nomme la sortie de
    /// VB-Cable « Haut-parleurs », à côté de sa variante 16 canaux.
    #[test]
    fn le_cable_virtuel_se_trouve_tout_seul() {
        let noms = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let chez_drion = noms(&[
            "C27G4Z (NVIDIA High Definition Audio)",
            "CABLE In 16 Ch (VB-Audio Virtual Cable)",
            "Casque (Realtek USB Audio)",
            "Haut-parleurs (JBL Quantum Stream Talk)",
            "Haut-parleurs (Steam Streaming Speakers)",
            "Haut-parleurs (VB-Audio Virtual Cable)",
            "Realtek Digital Output (Realtek USB Audio)",
        ]);
        assert_eq!(cable_virtuel(&chez_drion).as_deref(), Some("Haut-parleurs (VB-Audio Virtual Cable)"));
        // Un Windows anglais : le nom d'origine.
        let anglais = noms(&["Speakers (Realtek)", "CABLE Input (VB-Audio Virtual Cable)"]);
        assert_eq!(cable_virtuel(&anglais).as_deref(), Some("CABLE Input (VB-Audio Virtual Cable)"));
        // Pas de câble : rien, et surtout pas le casque.
        assert_eq!(cable_virtuel(&noms(&["Casque (Realtek USB Audio)"])), None);
    }

    /// L'entrée du câble ne doit jamais devenir le micro de ki-chat : la voix
    /// bouclerait.
    #[test]
    fn l_entree_du_cable_se_reconnait() {
        assert!(est_entree_de_cable("CABLE Output (VB-Audio Virtual Cable)"));
        assert!(!est_entree_de_cable("Microphone (Realtek USB Audio)"));
        assert!(!est_cable("Casque (Realtek USB Audio)"));
    }

}
