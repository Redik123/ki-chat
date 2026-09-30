//! La section « Changeur de voix » de la page Casque : l'interrupteur, les
//! personnages, où part la voix changée, un raccourci pour basculer — et, en
//! mode studio, chaque réglage à la main.

use std::time::Duration;

use eframe::egui::{self, RichText, Vec2};
use ki_voice::changeur::{ReglagesChangeur, PERSONNAGES, VERS_JEUX, VERS_KICHAT, VERS_TOUT};
use ki_voice::imitation::{empreinte, rapprocher};
use ki_voice::{EtatEssai, ESSAI_SECONDES};

use crate::icons::Icon;
use crate::ptt::PttKey;
use crate::reglages_audio::curseur;
use crate::theme::{ACCENT, DANGER, SPEAK, TEXT, TEXT_DIM, TEXT_FAINT, WARN};
use crate::ui;
use crate::KiApp;

/// Les extraits qu'on peut donner à imiter.
const EXTENSIONS_SON: [&str; 9] = ["wav", "mp3", "ogg", "opus", "m4a", "aac", "flac", "webm", "mp4"];

/// Les réglages du changeur tels que les préférences les rangent.
pub(crate) fn ecrire_changeur(r: &ReglagesChangeur) -> String {
    format!(
        "h={};f={};ch={};vo={};vs={};vhz={};so={};d={};r={};rhz={};t={};s={};v={};e={};ems={};rv={};rt={}",
        r.hauteur,
        r.formants,
        r.chuchotement,
        r.vocodeur,
        r.vocodeur_suit as u8,
        r.vocodeur_hz,
        r.sous_octave,
        r.distorsion,
        r.robot,
        r.robot_hz,
        r.talkie as u8,
        r.sombre_hz,
        r.voile,
        r.echo,
        r.echo_ms,
        r.reverb,
        r.reverb_taille
    )
}

/// L'inverse d'[`ecrire_changeur`] ; ce qui manque garde sa valeur neutre.
/// Des réglages d'avant le timbre (sans `f`) gardent leur son : leurs
/// formants suivaient la hauteur.
pub(crate) fn lire_changeur(texte: &str) -> ReglagesChangeur {
    let mut r = ReglagesChangeur::default();
    let mut formants_lus = false;
    for champ in texte.split(';') {
        let Some((cle, valeur)) = champ.split_once('=') else { continue };
        let Ok(v) = valeur.trim().parse::<f32>() else { continue };
        match cle.trim() {
            "h" => r.hauteur = v,
            "f" => {
                r.formants = v;
                formants_lus = true;
            }
            "ch" => r.chuchotement = v,
            "vo" => r.vocodeur = v,
            "vs" => r.vocodeur_suit = v != 0.0,
            "vhz" => r.vocodeur_hz = v,
            "so" => r.sous_octave = v,
            "d" => r.distorsion = v,
            "r" => r.robot = v,
            "rhz" => r.robot_hz = v,
            "t" => r.talkie = v != 0.0,
            "s" => r.sombre_hz = v,
            "v" => r.voile = v,
            "e" => r.echo = v,
            "ems" => r.echo_ms = v,
            "rv" => r.reverb = v,
            "rt" => r.reverb_taille = v,
            _ => {}
        }
    }
    if !formants_lus {
        r.formants = 2f32.powf(r.hauteur.clamp(-12.0, 12.0) / 12.0);
    }
    r.bornes()
}

/// Le personnage dont ces réglages sont exactement ceux, s'il y en a un.
pub(crate) fn personnage_de(r: &ReglagesChangeur) -> Option<usize> {
    PERSONNAGES.iter().position(|(_, _, fabrique)| fabrique() == *r)
}

/// Un curseur de 0 à 100 % sur une valeur de 0 à 1. Rend vrai au changement.
fn pourcent(ui: &mut egui::Ui, v: &mut f32) -> bool {
    let mut p = *v * 100.0;
    if curseur(ui, &mut p, 0.0..=100.0, " %", Some(1.0)) {
        *v = p / 100.0;
        true
    } else {
        false
    }
}

impl KiApp {
    /// La section du changeur. `apply` : un réglage du moteur a changé.
    pub(crate) fn changeur_ui(&mut self, ui: &mut egui::Ui, apply: &mut bool) {
        ui::section(
            ui,
            Icon::Star,
            "Changeur de voix",
            Some(
                "Un personnage pour ta voix, dans ki-chat, dans les jeux, ou les deux. \
                 Écoute-le avec l'essai de 5 s (onglet Audio, ou « T'écouter » plus bas).",
            ),
            |ui| {
                ui::ligne(ui, "Changeur", |ui| {
                    if ui::interrupteur(ui, &mut self.changeur_actif, "Changer ma voix").changed() {
                        *apply = true;
                    }
                    if self.changeur_actif {
                        let nom = personnage_de(&self.changeur).map(|i| PERSONNAGES[i].0).unwrap_or("réglage perso");
                        ui.add_space(4.0);
                        ui.horizontal(|ui| ui::status_dot(ui, SPEAK, &format!("allumé : {nom}"), 10.0));
                    }
                });
                ui::ligne(ui, "Personnage", |ui| {
                    let mut choix = personnage_de(&self.changeur).unwrap_or(usize::MAX);
                    let options: Vec<(usize, &str)> =
                        PERSONNAGES.iter().enumerate().map(|(i, (nom, _, _))| (i, *nom)).collect();
                    if ui::pastilles(ui, &mut choix, &options) {
                        if let Some((_, _, fabrique)) = PERSONNAGES.get(choix) {
                            self.changeur = fabrique();
                            // Choisir un personnage, c'est vouloir l'entendre.
                            self.changeur_actif = true;
                            *apply = true;
                        }
                    }
                    let description = PERSONNAGES
                        .get(choix)
                        .map(|(_, d, _)| *d)
                        .unwrap_or("Ton propre mélange — il se règle en mode studio.");
                    ui::precision(ui, description);
                });
                self.imitation_ui(ui, apply);
                ui::ligne(ui, "Où", |ui| {
                    if ui::segmente(
                        ui,
                        &mut self.changeur_vers,
                        &[(VERS_TOUT, "ki-chat et jeux"), (VERS_JEUX, "Jeux seulement"), (VERS_KICHAT, "ki-chat seulement")],
                    ) {
                        *apply = true;
                    }
                    ui::precision(
                        ui,
                        "Les jeux le reçoivent par le micro pour les jeux (plus haut). « M'écouter » \
                         le fait entendre dans tous les cas.",
                    );
                });
                ui::ligne(ui, "Raccourci", |ui| {
                    let actuel = self.hotkey_changeur.map(|k| k.label()).unwrap_or("aucun");
                    egui::ComboBox::from_id_salt("hotkey_changeur")
                        .width(120.0)
                        .selected_text(RichText::new(actuel).color(TEXT))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.hotkey_changeur, None, "aucun");
                            for key in PttKey::ALL {
                                ui.selectable_value(&mut self.hotkey_changeur, Some(key), key.label());
                            }
                        });
                    let prise = [self.hotkey_micro, self.hotkey_sourd].contains(&self.hotkey_changeur)
                        || (self.hotkey_changeur.is_some() && self.hotkey_changeur == Some(self.ptt_key));
                    if self.hotkey_changeur.is_some() && prise {
                        ui.label(RichText::new("cette touche sert déjà ailleurs").color(WARN).size(11.5));
                    }
                    ui::precision(ui, "Une pression allume ou coupe le changeur, même en jeu.");
                });

                if !self.mode_studio {
                    return;
                }
                // --- Le détail, en mode studio ---
                ui.add_space(4.0);
                ui.label(RichText::new("Réglages").color(TEXT).size(13.0).strong());
                ui.label(
                    RichText::new("Toucher un réglage fait ton propre personnage.").color(TEXT_FAINT).size(11.5),
                );
                ui.add_space(4.0);
                let montrer = self.explications;
                let avant = self.changeur;
                let r = &mut self.changeur;
                let aide = |ui: &mut egui::Ui, texte: &str| {
                    if montrer {
                        ui::precision(ui, texte);
                    }
                };
                ui::ligne(ui, "Hauteur", |ui| {
                    curseur(ui, &mut r.hauteur, -12.0..=12.0, " demi-tons", Some(0.5));
                    aide(
                        ui,
                        "Monte ou descend ta voix, en demi-tons. -12 : une octave plus grave, voix de \
                         géant ; +12 : une octave plus aiguë, voix d'enfant. Entre -5 et +5 : la \
                         voix de quelqu'un d'autre.",
                    );
                });
                ui::ligne(ui, "Timbre", |ui| {
                    let mut pct = (r.formants - 1.0) * 100.0;
                    if curseur(ui, &mut pct, -40.0..=60.0, " %", Some(1.0)) {
                        r.formants = 1.0 + pct / 100.0;
                    }
                    let bande = 2f32.powf(r.hauteur / 12.0);
                    if (r.formants - bande).abs() > 0.005
                        && ui::button(ui, Icon::Repeat, "Suivre la hauteur (effet écureuil)").clicked()
                    {
                        r.formants = bande;
                    }
                    aide(
                        ui,
                        "La taille de ta bouche et de ta gorge, séparée de la hauteur. À 0, tu gardes \
                         ta bouche : ta voix monte ou descend sans l'effet écureuil ni ralenti. +15 à \
                         +20 % avec la hauteur montée : une voix de femme ou d'enfant crédible. -10 à \
                         -20 % : un homme plus massif, un géant.",
                    );
                });
                ui::ligne(ui, "Chuchotement", |ui| {
                    pourcent(ui, &mut r.chuchotement);
                    aide(
                        ui,
                        "Ajoute un souffle qui dit les mêmes mots que toi : l'ombre d'Omen, un \
                         fantôme. À fond, il remplace ta voix — tu chuchotes sans chuchoter.",
                    );
                });
                ui::ligne(ui, "Vocodeur", |ui| {
                    pourcent(ui, &mut r.vocodeur);
                    if r.vocodeur > 0.005 {
                        ui::interrupteur(ui, &mut r.vocodeur_suit, "suit ta voix");
                        if !r.vocodeur_suit {
                            curseur(ui, &mut r.vocodeur_hz, 40.0..=500.0, " Hz", Some(1.0));
                        }
                    }
                    aide(
                        ui,
                        "Une note de synthèse qui parle avec ta bouche : une voix de robot qu'on \
                         comprend. Elle suit ta voix au demi-ton près, d'où les marches du robot ; \
                         ou elle reste sur une note fixe, pour un robot monocorde.",
                    );
                });
                ui::ligne(ui, "Couche grave", |ui| {
                    pourcent(ui, &mut r.sous_octave);
                    aide(
                        ui,
                        "Ajoute une deuxième voix une octave plus bas, mélangée à la tienne : la \
                         carrure d'un monstre ou d'un démon.",
                    );
                });
                ui::ligne(ui, "Distorsion", |ui| {
                    pourcent(ui, &mut r.distorsion);
                    aide(ui, "Sature ta voix : rauque à petite dose, grésillante et agressive à fond.");
                });
                ui::ligne(ui, "Robot", |ui| {
                    pourcent(ui, &mut r.robot);
                    if r.robot > 0.005 {
                        curseur(ui, &mut r.robot_hz, 10.0..=300.0, " Hz", Some(1.0));
                    }
                    aide(
                        ui,
                        "Module ta voix par une note : une voix de machine. La fréquence change le \
                         robot — grave et grondant vers 30 Hz, métallique vers 150 Hz.",
                    );
                });
                ui::ligne(ui, "Talkie", |ui| {
                    ui::interrupteur(ui, &mut r.talkie, "radio de poche");
                    aide(
                        ui,
                        "Réduit ta voix à la bande d'une radio de poche : fine, nasillarde, lointaine.",
                    );
                });
                ui::ligne(ui, "Sombre", |ui| {
                    let mut khz = r.sombre_hz / 1000.0;
                    if curseur(ui, &mut khz, 1.0..=20.0, " kHz", Some(0.5)) {
                        r.sombre_hz = khz * 1000.0;
                    }
                    aide(
                        ui,
                        "Coupe les aigus au-dessus de cette fréquence : une voix étouffée, derrière \
                         un mur ou un masque. 20 kHz : rien.",
                    );
                });
                ui::ligne(ui, "Voile", |ui| {
                    pourcent(ui, &mut r.voile);
                    aide(ui, "Fait chatoyer ta voix, lentement (un flanger) : l'effet fantôme, d'outre-tombe.");
                });
                ui::ligne(ui, "Écho", |ui| {
                    pourcent(ui, &mut r.echo);
                    if r.echo > 0.005 {
                        curseur(ui, &mut r.echo_ms, 40.0..=900.0, " ms", Some(10.0));
                    }
                    aide(
                        ui,
                        "Répète ta voix après un délai, en s'éteignant : une montagne, un hall \
                         immense. Le délai règle l'écart entre les répétitions.",
                    );
                });
                ui::ligne(ui, "Réverbération", |ui| {
                    pourcent(ui, &mut r.reverb);
                    if r.reverb > 0.005 {
                        let mut taille = r.reverb_taille * 100.0;
                        if curseur(ui, &mut taille, 0.0..=100.0, " % de pièce", Some(1.0)) {
                            r.reverb_taille = taille / 100.0;
                        }
                    }
                    aide(
                        ui,
                        "Ajoute la résonance d'un lieu : une petite pièce à 20 %, une cathédrale ou \
                         une grotte à fond. La taille règle la longueur de la résonance.",
                    );
                });
                if *r != avant {
                    *r = r.bornes();
                    *apply = true;
                }
                if !self.changeur_actif {
                    ui.label(
                        RichText::new("Le changeur est coupé : allume-le pour entendre ces réglages.")
                            .color(TEXT_DIM)
                            .size(11.5),
                    );
                }
            },
        );
    }
}

impl KiApp {
    /// L'imitation assistée : l'extrait d'une voix, sa propre voix sur un
    /// essai de 5 s, et les réglages qui rapprochent l'une de l'autre.
    fn imitation_ui(&mut self, ui: &mut egui::Ui, apply: &mut bool) {
        // L'analyse d'un extrait se fait sur un fil : un long MP3 à décoder
        // et à mesurer figerait la fenêtre.
        if let Some(rx) = &self.imitation_calcul {
            match rx.try_recv() {
                Ok(Ok(cible)) => {
                    self.imitation_cible = Some(cible);
                    self.imitation_erreur = None;
                    self.imitation_calcul = None;
                }
                Ok(Err(erreur)) => {
                    self.imitation_erreur = Some(erreur);
                    self.imitation_calcul = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => ui.ctx().request_repaint_after(Duration::from_millis(100)),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.imitation_calcul = None,
            }
        }
        ui::ligne(ui, "Imiter", |ui| {
            // 1. La voix visée.
            if self.imitation_calcul.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("analyse de l'extrait…").color(TEXT_DIM).size(12.0));
                });
            } else {
                let libelle =
                    if self.imitation_cible.is_some() { "Choisir un autre extrait…" } else { "Choisir l'extrait d'une voix…" };
                if ui::button(ui, Icon::Paperclip, libelle).clicked() {
                    if let Some(chemin) = rfd::FileDialog::new().add_filter("Son", &EXTENSIONS_SON).pick_file() {
                        let (tx, rx) = std::sync::mpsc::channel();
                        self.imitation_calcul = Some(rx);
                        std::thread::spawn(move || {
                            let nom = chemin.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            let resultat = crate::soundboard::decoder(&chemin)
                                .map_err(|e| format!("Impossible de lire ce fichier : {e:#}"))
                                .and_then(|pcm| {
                                    empreinte(&pcm).ok_or_else(|| {
                                        "Pas assez de voix dans cet extrait : prends un passage où la voix \
                                         parle seule, sans musique ni bruitages."
                                            .to_string()
                                    })
                                });
                            let _ = tx.send(resultat.map(|e| (nom, e)));
                        });
                    }
                }
            }
            if let Some(erreur) = &self.imitation_erreur {
                ui.label(RichText::new(erreur).color(WARN).size(11.5));
            }
            let Some((nom, cible)) = self.imitation_cible.clone() else {
                ui::precision(
                    ui,
                    "Donne un extrait de la voix à imiter (quelques secondes où elle parle seule) : \
                     ki-chat mesure sa hauteur et son timbre, et règle le changeur pour t'en \
                     rapprocher.",
                );
                return;
            };
            ui::precision(ui, &format!("Voix visée : « {nom} », vers {:.0} Hz.", cible.f0_hz));

            // 2. Sa propre voix, sur l'essai de 5 s.
            let etat = self.link.engine.lock().unwrap().as_ref().map(|e| e.essai());
            let moi = match etat {
                None => {
                    ui::precision(ui, "Connecte-toi à un serveur pour enregistrer ta voix.");
                    return;
                }
                Some(EtatEssai::Enregistre(avancement)) => {
                    ui.ctx().request_repaint_after(Duration::from_millis(50));
                    ui.horizontal(|ui| {
                        ui::meter(ui, avancement, Vec2::new(150.0, 8.0), DANGER);
                        let reste = ((1.0 - avancement) * ESSAI_SECONDES as f32).ceil().max(1.0);
                        ui.label(RichText::new(format!("parle normalement… {reste:.0} s")).color(TEXT_DIM).size(12.0));
                    });
                    return;
                }
                Some(EtatEssai::Pret { numero, .. }) => self.empreinte_de_l_essai(numero),
                Some(EtatEssai::Vide) => None,
            };
            let Some(moi) = moi else {
                if ui::button(ui, Icon::Mic, &format!("Enregistrer ma voix ({ESSAI_SECONDES} s)")).clicked() {
                    if let Some(e) = self.link.engine.lock().unwrap().as_ref() {
                        e.enregistrer_essai();
                    }
                }
                ui::precision(
                    ui,
                    "Parle normalement pendant 5 secondes, sans le changeur : ki-chat mesure ta \
                     voix pour la comparer. Personne ne t'entend pendant l'enregistrement.",
                );
                return;
            };

            // 3. Le rapprochement.
            let r = rapprocher(&moi, &cible);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Pour t'en rapprocher :").color(TEXT_DIM).size(12.0));
                ui.label(
                    RichText::new(format!(
                        "hauteur {:+.1} demi-tons, timbre {:+.0} %",
                        r.hauteur,
                        (r.formants - 1.0) * 100.0
                    ))
                    .color(ACCENT)
                    .size(12.0)
                    .strong(),
                );
            });
            if r.bornee {
                ui::precision(ui, "La hauteur visée dépasse ce que permet le changeur (une octave) : il s'en approche.");
            }
            if r.confiance < 0.5 {
                ui::precision(
                    ui,
                    "Le timbre est incertain : l'extrait est peut-être bruité, ou sa voix trop \
                     différente de la tienne. Essaie un autre passage.",
                );
            }
            ui.horizontal_wrapped(|ui| {
                if ui::button(ui, Icon::Check, "Appliquer au changeur").clicked() {
                    self.changeur.hauteur = r.hauteur;
                    self.changeur.formants = r.formants;
                    self.changeur = self.changeur.bornes();
                    self.changeur_actif = true;
                    *apply = true;
                }
                if ui::icon_button(ui, Icon::Mic, "Réenregistrer ma voix").clicked() {
                    if let Some(e) = self.link.engine.lock().unwrap().as_ref() {
                        e.enregistrer_essai();
                    }
                }
            });
            ui::precision(
                ui,
                "Seules la hauteur et le timbre changent : les effets (souffle, réverbération…) \
                 restent tels quels, et se règlent en mode studio. La ressemblance s'arrête à la \
                 voix — l'accent et la façon de parler, c'est toi.",
            );
        });
    }

    /// L'empreinte de sa voix sur l'essai de ce numéro — mesurée une fois.
    /// Le micro brut passé par l'égaliseur de sa voix : ce que le changeur
    /// reçoit, sans le changeur.
    fn empreinte_de_l_essai(&mut self, numero: u64) -> Option<ki_voice::imitation::EmpreinteVoix> {
        if let Some((n, e)) = &self.imitation_moi {
            if *n == numero {
                return e.clone();
            }
        }
        let (mut brute, _) = self.link.engine.lock().unwrap().as_ref()?.essai_pcm()?;
        let mut eq = ki_voice::egaliseur::Egaliseur::new(&self.egaliseur_micro);
        for bloc in brute.chunks_mut(960) {
            eq.traiter_trame(bloc);
        }
        let e = empreinte(&brute);
        self.imitation_moi = Some((numero, e.clone()));
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_reglages_font_l_aller_retour() {
        for (_, _, fabrique) in PERSONNAGES {
            let r = fabrique();
            assert_eq!(lire_changeur(&ecrire_changeur(&r)), r);
        }
        assert_eq!(lire_changeur(""), ReglagesChangeur::default());
        // Un ancien réglage (sans formants) : ils suivent la hauteur, comme
        // avant — le même son qu'hier.
        assert_eq!(lire_changeur("h=99"), ReglagesChangeur { hauteur: 12.0, formants: 2.0, ..Default::default() });
        let ancien = lire_changeur("h=-4;rv=0.3");
        assert!((ancien.formants - 2f32.powf(-4.0 / 12.0)).abs() < 1e-6);
        // Un nouveau garde les siens.
        let r = ReglagesChangeur { hauteur: 5.0, formants: 1.2, chuchotement: 0.4, vocodeur: 0.7, vocodeur_suit: false, vocodeur_hz: 90.0, ..Default::default() };
        assert_eq!(lire_changeur(&ecrire_changeur(&r)), r);
    }

    #[test]
    fn un_personnage_se_reconnait() {
        assert_eq!(personnage_de(&(PERSONNAGES[0].2)()), Some(0));
        assert_eq!(personnage_de(&ReglagesChangeur { hauteur: 1.5, ..Default::default() }), None);
    }
}
