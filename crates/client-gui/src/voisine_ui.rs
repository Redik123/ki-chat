//! « La voix d'à côté » : le réglage de proximité dans l'onglet Audio.
//!
//! Le moteur (`ki_voice::proximite`) ne garde que la voix proche du micro :
//! la personne assise à côté n'ouvre plus le micro et n'est plus remontée
//! par le gain automatique. Ici : la force choisie, ce que le moteur a
//! appris de sa voix (la référence, l'ancre), une jauge du niveau brut avec
//! ses deux repères, et deux mesures de cinq secondes — « parle » pour
//! ancrer sa voix, « fais-la parler » pour vérifier l'écart et choisir la
//! force. L'ancre est rangée par micro : une autre carte son, c'est un autre
//! niveau, et c'est à réapprendre.

use ki_ui::jetons::{espace, texte};
use std::collections::HashMap;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText, Stroke, Vec2};
use ki_voice::proximite::{
    ReglagesProximite, PROXIMITE_DOUCE, PROXIMITE_FORTE, PROXIMITE_NORMALE, PROXIMITE_OFF,
};

use crate::icons::Icon;
use crate::theme::{self, ACCENT, SPEAK, TEXT_DIM, TEXT_FAINT, WARN};
use crate::ui;
use crate::{KiApp, VoiceSnapshot};

/// Ce que mesure une prise de cinq secondes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Quoi {
    /// Sa voix : la référence (l'ancre) de ce micro.
    Moi,
    /// La voix d'à côté : l'écart, et la force qui convient.
    Elle,
}

/// Une mesure en cours.
pub(crate) struct MesureProx {
    pub(crate) quoi: Quoi,
    pub(crate) debut: Instant,
}

/// La durée d'une mesure : les cinq secondes que le moteur garde.
const DUREE: Duration = Duration::from_secs(5);
/// Combien de trames (20 ms) il faut au moins pour conclure.
const TRAMES_MIN: usize = 25;
/// Le bas de la jauge, en dBFS.
const JAUGE_MIN_DB: f32 = -70.0;

/// La clé sous laquelle l'ancre d'un micro est rangée.
pub(crate) fn cle_micro(pref_input: Option<&str>) -> String {
    pref_input.unwrap_or("défaut").to_owned()
}

/// Les ancres rangées : « micro = dB » en JSON.
pub(crate) fn lire_ancres(texte: &str) -> HashMap<String, f32> {
    serde_json::from_str::<HashMap<String, f32>>(texte)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, v)| v.is_finite())
        .collect()
}

pub(crate) fn ecrire_ancres(ancres: &HashMap<String, f32>) -> String {
    serde_json::to_string(ancres).unwrap_or_else(|_| "{}".into())
}

/// Le centile `c` (0..1) d'une liste de niveaux, ou rien s'il n'y en a pas
/// assez pour que ça veuille dire quelque chose.
fn centile(mut niveaux: Vec<f32>, c: f32) -> Option<f32> {
    if niveaux.len() < TRAMES_MIN {
        return None;
    }
    niveaux.sort_by(f32::total_cmp);
    Some(niveaux[((niveaux.len() - 1) as f32 * c) as usize])
}

/// Le niveau de « ma voix » dans cinq secondes de (niveau, probabilité) : le
/// 70e centile des trames dont Silero est sûr — à défaut (push-to-talk,
/// Silero éteint), des trames les plus fortes.
pub(crate) fn niveau_de_ma_voix(recents: &[(f32, f32)]) -> Option<f32> {
    let sures: Vec<f32> = recents.iter().filter(|(n, p)| *p >= 0.85 && *n > -70.0).map(|(n, _)| *n).collect();
    if let Some(n) = centile(sures, 0.7) {
        return Some(n);
    }
    let mut tous: Vec<f32> = recents.iter().map(|(n, _)| *n).filter(|n| *n > -60.0).collect();
    tous.sort_by(f32::total_cmp);
    let hautes: Vec<f32> = tous.iter().rev().take(tous.len() * 3 / 10).copied().collect();
    centile(hautes, 0.5)
}

/// Le niveau de « la voix d'à côté » : son 95e centile — le pire cas, c'est
/// lui qu'il faut tenir dehors.
pub(crate) fn niveau_de_la_voisine(recents: &[(f32, f32)]) -> Option<f32> {
    let parole: Vec<f32> = recents.iter().filter(|(n, p)| *p >= 0.5 && *n > -70.0).map(|(n, _)| *n).collect();
    if let Some(n) = centile(parole, 0.95) {
        return Some(n);
    }
    let mut tous: Vec<f32> = recents.iter().map(|(n, _)| *n).filter(|n| *n > -70.0).collect();
    tous.sort_by(f32::total_cmp);
    let hautes: Vec<f32> = tous.iter().rev().take(tous.len() / 10).copied().collect();
    if hautes.len() < 5 {
        return None;
    }
    Some(hautes[hautes.len() / 2])
}

/// La force qui tient une voix `ecart_db` sous la sienne dehors, avec 6 dB
/// de marge sur le seuil : `None` si aucune n'y suffit.
pub(crate) fn force_pour(ecart_db: f32) -> Option<u8> {
    if ecart_db >= 22.0 {
        Some(PROXIMITE_DOUCE)
    } else if ecart_db >= 16.0 {
        Some(PROXIMITE_NORMALE)
    } else if ecart_db >= 12.0 {
        Some(PROXIMITE_FORTE)
    } else {
        None
    }
}

fn nom_force(force: u8) -> &'static str {
    match force {
        PROXIMITE_DOUCE => "Douce",
        PROXIMITE_NORMALE => "Normale",
        PROXIMITE_FORTE => "Forte",
        _ => "Désactivée",
    }
}

/// La jauge du niveau brut : de -70 à 0 dBFS, le repère du seuil (orange)
/// et celui de sa voix (vert).
fn jauge_db(ui: &mut egui::Ui, niveau_db: f32, seuil_db: Option<f32>, reference_db: Option<f32>, couleur: Color32) {
    let largeur = (ui.available_width() - 20.0).clamp(120.0, 300.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(largeur, 10.0), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let part = |db: f32| ((db - JAUGE_MIN_DB) / -JAUGE_MIN_DB).clamp(0.0, 1.0);
    ui::paint_meter(ui.painter(), rect, part(niveau_db), couleur);
    let repere = |db: f32, couleur: Color32, debord: f32| {
        let x = rect.left() + rect.width() * part(db);
        ui.painter().line_segment(
            [egui::pos2(x, rect.top() - debord), egui::pos2(x, rect.bottom() + debord)],
            Stroke::new(1.5_f32, couleur),
        );
    };
    if let Some(s) = seuil_db {
        repere(s, WARN, 2.0);
    }
    if let Some(r) = reference_db {
        repere(r, ACCENT, 4.0);
    }
}

impl KiApp {
    /// Les réglages de proximité à pousser au moteur : la force choisie et
    /// l'ancre de ce micro, s'il en a une.
    pub(crate) fn proximite_reglages(&self) -> ReglagesProximite {
        let ancre = self.proximite_ancres.get(&cle_micro(self.pref_input.as_deref())).copied();
        ReglagesProximite::de_force(self.proximite_force, ancre)
    }

    /// Le moteur a posé une ancre tout seul (une minute de parole sûre) : on
    /// la range pour ce micro et on la lui rend — il cesse alors de la
    /// publier. Appelé à chaque image, réglages ouverts ou non.
    pub(crate) fn ranger_ancre_apprise(&mut self, stats: &ki_voice::VoiceStats) {
        let Some(a) = stats.prox_ancre_apprise else { return };
        let cle = cle_micro(self.pref_input.as_deref());
        if self.proximite_ancres.get(&cle).is_some_and(|v| (v - a).abs() < 0.01) {
            return;
        }
        self.proximite_ancres.insert(cle, a);
        if let Some(e) = self.link.engine.lock().unwrap().as_ref() {
            e.set_proximite(self.proximite_reglages());
        }
    }

    /// Le bloc « La voix d'à côté » de l'onglet Audio.
    pub(crate) fn voisine_ui(&mut self, ui: &mut egui::Ui, voice: &VoiceSnapshot, apply: &mut bool) {
        let stats = &voice.stats;
        let engine_up = voice.engine_up;
        self.avancer_mesure_prox();

        if ui::segmente(
            ui,
            &mut self.proximite_force,
            &[
                (PROXIMITE_OFF, "Désactivée"),
                (PROXIMITE_DOUCE, "Douce"),
                (PROXIMITE_NORMALE, "Normale"),
                (PROXIMITE_FORTE, "Forte"),
            ],
        ) {
            *apply = true;
        }
        ui::precision(
            ui,
            match self.proximite_force {
                PROXIMITE_OFF => {
                    "Toute voix assez nette ouvre le micro, la tienne comme celle de la personne \
                     à côté de toi — et le gain automatique la remonte au même niveau."
                }
                PROXIMITE_DOUCE => {
                    "Ne garde que ce qui arrive à moins de 18 dB sous tes syllabes fortes ; le \
                     reste baisse de 12 dB. Pour quelqu'un à plus de deux mètres, ou si « Normale » \
                     mange ta voix douce."
                }
                PROXIMITE_FORTE => {
                    "Ne garde que ce qui arrive à moins de 10 dB sous tes syllabes fortes ; le \
                     reste baisse de 30 dB. Il faut parler près du micro, toujours au même niveau : \
                     ta voix douce passera moins."
                }
                _ => {
                    "Ne garde que ce qui arrive à moins de 14 dB sous tes syllabes fortes (ta \
                     bouche est à trois centimètres de la perche, la personne à côté à un mètre : \
                     elle arrive 15 à 25 dB plus bas) ; le reste baisse de 18 dB, et le gain \
                     automatique ne le remonte plus."
                }
            },
        );
        if self.proximite_force == PROXIMITE_OFF {
            return;
        }
        ui.add_space(espace::S);

        // Ce que le moteur sait de sa voix.
        let cle = cle_micro(self.pref_input.as_deref());
        let ancre = self.proximite_ancres.get(&cle).copied();
        let (etat_couleur, etat_texte) = match (engine_up, stats.prox_etat, stats.prox_reference_db) {
            (false, _, _) => (TEXT_FAINT, "vocal inactif".to_owned()),
            (true, 0, _) => (WARN, "parle : ki-chat apprend le niveau de ta voix…".to_owned()),
            (true, 1, Some(r)) => (TEXT_DIM, format!("apprentissage — ta voix vers {r:.0} dBFS")),
            (true, _, Some(r)) => (
                SPEAK,
                format!(
                    "ta voix : {r:.0} dBFS{}",
                    stats.prox_seuil_db.map(|s| format!(" · s'ouvre au-dessus de {s:.0} dBFS")).unwrap_or_default()
                ),
            ),
            (true, _, None) => (TEXT_DIM, "en attente du micro".to_owned()),
        };
        ui.label(RichText::new(etat_texte).color(etat_couleur).size(texte::COURANT));
        ui.add_space(espace::XXS);
        ui.horizontal(|ui| {
            let (couleur, mot) = if !engine_up || stats.prox_niveau_db <= JAUGE_MIN_DB + 5.0 {
                (theme::BG_ACTIVE, "")
            } else if stats.prox_reference_db.is_none() {
                (TEXT_DIM, "")
            } else if stats.prox_proche {
                (SPEAK, "toi")
            } else {
                (WARN, "à côté")
            };
            jauge_db(ui, stats.prox_niveau_db, stats.prox_seuil_db, stats.prox_reference_db, couleur);
            if !mot.is_empty() {
                ui.label(RichText::new(mot).color(couleur).size(texte::PETIT).strong());
            }
        });
        ui::precision(
            ui,
            "Le niveau brut du micro. Trait vert : ta voix ; trait orange : en dessous, c'est « à \
             côté », le micro reste fermé et le son baisse.",
        );
        if engine_up && stats.prox_gain < 0.9 {
            ui.label(
                RichText::new(format!("en ce moment : {:+.0} dB", 20.0 * stats.prox_gain.max(1e-4).log10()))
                    .color(TEXT_DIM)
                    .size(texte::PETIT),
            );
        }

        // Les deux mesures.
        ui.add_space(espace::S);
        match self.mesure_prox.as_ref().map(|m| (m.quoi, m.debut)) {
            Some((quoi, debut)) => {
                let progression = (debut.elapsed().as_secs_f32() / DUREE.as_secs_f32()).min(1.0);
                ui.ctx().request_repaint_after(Duration::from_millis(50));
                ui.horizontal(|ui| {
                    ui::meter(ui, progression, Vec2::new(150.0, 8.0), ACCENT);
                    ui.label(
                        RichText::new(match quoi {
                            Quoi::Moi => "parle comme en partie…",
                            Quoi::Elle => "tais-toi, elle parle…",
                        })
                        .color(TEXT_DIM)
                        .size(texte::COURANT),
                    );
                    if ui::icon_button(ui, Icon::Close, "Annuler").clicked() {
                        self.mesure_prox = None;
                    }
                });
            }
            None => {
                ui.horizontal_wrapped(|ui| {
                    if engine_up
                        && ui::button(ui, Icon::Mic, "Apprendre ma voix (5 s)")
                            .on_hover_text("parle normalement pendant cinq secondes : c'est le niveau de référence de ce micro")
                            .clicked()
                    {
                        self.mesure_prox = Some(MesureProx { quoi: Quoi::Moi, debut: Instant::now() });
                        self.mesure_prox_verdict = None;
                    }
                    if engine_up
                        && ancre.is_some()
                        && ui::button(ui, Icon::Target, "Mesurer la voix d'à côté (5 s)")
                            .on_hover_text("tais-toi et fais parler la personne à côté, un peu fort : ki-chat mesure l'écart et choisit la force")
                            .clicked()
                    {
                        self.mesure_prox = Some(MesureProx { quoi: Quoi::Elle, debut: Instant::now() });
                        self.mesure_prox_verdict = None;
                    }
                    if ancre.is_some() && ui::icon_button(ui, Icon::Trash, "Oublier ce que ki-chat sait de ma voix sur ce micro").clicked() {
                        self.proximite_ancres.remove(&cle);
                        self.mesure_prox_verdict = None;
                        *apply = true;
                    }
                });
            }
        }
        if let Some(v) = &self.mesure_prox_verdict {
            ui.add_space(espace::XS);
            ui.label(RichText::new(v).color(TEXT_DIM).size(texte::COURANT));
        }
        if ancre.is_none() {
            ui::precision(
                ui,
                "Sans mesure, ki-chat apprend ta voix tout seul en une minute de parole, et \
                 l'enregistre pour ce micro. Une autre carte son : il recommence.",
            );
        }
    }

    /// Fait avancer la mesure en cours ; finie, conclut et applique.
    fn avancer_mesure_prox(&mut self) {
        let Some(m) = &self.mesure_prox else { return };
        if m.debut.elapsed() < DUREE {
            return;
        }
        let quoi = m.quoi;
        self.mesure_prox = None;
        let recents = self.link.engine.lock().unwrap().as_ref().map(|e| e.proximite_recents()).unwrap_or_default();
        let cle = cle_micro(self.pref_input.as_deref());
        match quoi {
            Quoi::Moi => match niveau_de_ma_voix(&recents) {
                Some(n) => {
                    self.proximite_ancres.insert(cle, n);
                    self.mesure_prox_verdict = Some(format!(
                        "Ta voix arrive à {n:.0} dBFS sur ce micro : c'est la référence. Fais parler la \
                         personne à côté pour vérifier l'écart."
                    ));
                    self.apply_audio_settings();
                }
                None => {
                    self.mesure_prox_verdict =
                        Some("Je ne t'ai pas entendu : parle pendant les cinq secondes, micro branché.".into());
                }
            },
            Quoi::Elle => {
                let Some(ancre) = self.proximite_ancres.get(&cle).copied() else { return };
                match niveau_de_la_voisine(&recents) {
                    Some(elle) => {
                        let ecart = ancre - elle;
                        match force_pour(ecart) {
                            Some(force) => {
                                self.proximite_force = force;
                                self.mesure_prox_verdict = Some(format!(
                                    "Elle arrive {ecart:.0} dB sous toi ({elle:.0} dBFS) : isolation réglée sur \
                                     « {} ».",
                                    nom_force(force)
                                ));
                            }
                            None => {
                                self.proximite_force = PROXIMITE_FORTE;
                                self.mesure_prox_verdict = Some(format!(
                                    "Elle arrive seulement {ecart:.0} dB sous toi ({elle:.0} dBFS) : trop près pour \
                                     être sûr. Isolation sur « Forte » ; rapproche la perche de ta bouche, ou \
                                     passe en push-to-talk."
                                ));
                            }
                        }
                        self.apply_audio_settings();
                    }
                    None => {
                        self.mesure_prox_verdict = Some(
                            "Je ne l'ai pas entendue : qu'elle parle pendant les cinq secondes, toi sans rien dire."
                                .into(),
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ma_voix_est_le_haut_des_trames_sures() {
        let mut recents = Vec::new();
        for i in 0..100 {
            // Ses voyelles vers -20, ses consonnes vers -35, Silero sûr.
            recents.push((if i % 3 == 0 { -35.0 } else { -20.0 + (i % 5) as f32 }, 0.99));
            // Du silence, pas sûr.
            recents.push((-80.0, 0.0));
        }
        let n = niveau_de_ma_voix(&recents).unwrap();
        assert!((-20.0..=-16.0).contains(&n), "{n}");
    }

    #[test]
    fn la_voisine_est_son_pire_cas() {
        let recents: Vec<(f32, f32)> = (0..100).map(|i| (-45.0 + (i % 10) as f32, 0.9)).collect();
        let n = niveau_de_la_voisine(&recents).unwrap();
        assert!((-37.0..=-36.0).contains(&n), "{n}");
        assert!(niveau_de_la_voisine(&[(-90.0, 0.0); 100]).is_none());
    }

    #[test]
    fn la_force_suit_l_ecart() {
        assert_eq!(force_pour(30.0), Some(PROXIMITE_DOUCE));
        assert_eq!(force_pour(20.0), Some(PROXIMITE_NORMALE));
        assert_eq!(force_pour(15.0), Some(PROXIMITE_FORTE));
        assert_eq!(force_pour(10.0), None);
    }

    #[test]
    fn les_ancres_font_l_aller_retour() {
        let mut a = HashMap::new();
        a.insert("External Mic (Sound Blaster G8 USB-1)".to_owned(), -18.5);
        a.insert("défaut".to_owned(), -30.0);
        let lu = lire_ancres(&ecrire_ancres(&a));
        assert_eq!(lu, a);
        assert!(lire_ancres("n'importe quoi").is_empty());
        assert!(lire_ancres(r#"{"x": null}"#).is_empty());
    }
}
