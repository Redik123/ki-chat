//! Le soundboard : des sons à la touche, entendus par tout le salon vocal.
//!
//! Chacun dépose ses sons dans un dossier « soundboard » à côté de ses sons
//! de notification — rien n'est embarqué, le dépôt est public. Un clic, ou
//! une touche de 1 à 9 la fenêtre ouverte, et le son part vers le salon
//! comme si on l'avait dit au micro : mixé à la voix, ou seul si le micro
//! est fermé ; on l'entend aussi chez soi. Le protocole n'en sait rien,
//! c'est de la voix — et le serveur le relaie et le coupe comme telle.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText, Vec2};

use crate::icons::Icon;
use crate::theme::{self, TEXT, TEXT_DIM, TEXT_FAINT};
use crate::ui::{self, Tone};

/// Au-delà, un son est coupé : c'est un soundboard, pas un lecteur.
pub const DUREE_MAX_S: usize = 30;

const CADENCE: usize = 48_000;

/// Un son prêt à partir : mono 48 kHz, au plus [`DUREE_MAX_S`] secondes.
pub struct Son {
    pub nom: String,
    pub pcm: Arc<Vec<f32>>,
}

impl Son {
    pub fn duree_s(&self) -> f32 {
        self.pcm.len() as f32 / CADENCE as f32
    }
}

/// Ce que la fenêtre demande à l'application, qui tient le moteur vocal.
pub enum Commande {
    Jouer(Arc<Vec<f32>>),
    Arreter,
}

/// Ce que le fil de chargement rapporte.
type Charges = Arc<Mutex<Option<Vec<Son>>>>;

pub struct Soundboard {
    pub ouvert: bool,
    /// Le volume des sons, chez les autres comme chez soi (1.0 = 100 %).
    pub volume: f32,
    pub sons: Vec<Son>,
    charges: Charges,
    en_cours: bool,
    charge: bool,
    /// Un mot pour la fenêtre : le dossier créé, un fichier illisible…
    message: Option<String>,
}

impl Soundboard {
    pub fn load(get: impl Fn(&str, &str) -> String) -> Self {
        Self {
            ouvert: false,
            volume: get("soundboard_volume", "0.8").parse().unwrap_or(0.8),
            sons: Vec::new(),
            charges: Arc::default(),
            en_cours: false,
            charge: false,
            message: None,
        }
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        storage.set_string("soundboard_volume", format!("{}", self.volume));
    }

    /// Les dossiers où l'on cherche les sons, dans l'ordre : le premier
    /// nom gagne — comme pour les sons de notification.
    pub fn dossiers() -> Vec<PathBuf> {
        crate::sound_dirs().into_iter().map(|d| d.join("soundboard")).collect()
    }

    /// Le dossier où déposer ses sons : celui des réglages, créé au besoin.
    pub fn dossier_perso() -> Option<PathBuf> {
        let dossier = Self::dossiers().into_iter().last()?;
        std::fs::create_dir_all(&dossier).ok()?;
        Some(dossier)
    }

    pub fn basculer(&mut self) {
        self.ouvert = !self.ouvert;
        if self.ouvert && !self.charge {
            self.recharger();
        }
    }

    /// Relit les dossiers sur un fil : décoder dix MP3 n'a rien à faire
    /// sur celui de l'interface.
    pub fn recharger(&mut self) {
        if self.en_cours {
            return;
        }
        self.en_cours = true;
        self.charge = true;
        let charges = self.charges.clone();
        let dossiers = Self::dossiers();
        std::thread::Builder::new()
            .name("ki-soundboard".into())
            .spawn(move || {
                let sons = charger(&dossiers);
                *charges.lock().unwrap() = Some(sons);
            })
            .ok();
    }

    fn relever(&mut self) {
        if let Some(sons) = self.charges.lock().unwrap().take() {
            self.en_cours = false;
            self.message = Some(match sons.len() {
                0 => "aucun son pour l'instant".to_string(),
                1 => "1 son".to_string(),
                n => format!("{n} sons"),
            });
            self.sons = sons;
        }
    }

    /// La fenêtre, si elle est ouverte. Rend ce qu'on y a demandé : jouer
    /// un son, ou tout arrêter — l'appelant tient le moteur vocal.
    pub fn fenetre(&mut self, ctx: &egui::Context, en_vocal: bool) -> Option<Commande> {
        if !self.ouvert {
            return None;
        }
        self.relever();
        let mut commande = None;
        let mut ouvert = true;
        let mut recharger = false;
        egui::Window::new("Soundboard")
            .open(&mut ouvert)
            .default_width(540.0)
            .default_height(360.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui::hint(
                    ui,
                    "un clic — ou les touches 1 à 9, cette fenêtre ouverte — et le son part \
                     dans ton salon vocal, mixé à ta voix ou seul si ton micro est fermé. \
                     Tout le monde l'entend, toi aussi.",
                );
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    let mut pct = self.volume * 100.0;
                    if ui
                        .add(
                            egui::Slider::new(&mut pct, 0.0..=150.0)
                                .text("volume")
                                .suffix(" %")
                                .integer(),
                        )
                        .changed()
                    {
                        self.volume = pct / 100.0;
                    }
                    if ui::button(ui, Icon::Close, "Stop").on_hover_text("coupe ce qui joue").clicked() {
                        commande = Some(Commande::Arreter);
                    }
                    if ui::button(ui, Icon::Refresh, "Recharger").clicked() {
                        recharger = true;
                    }
                    if ui::button(ui, Icon::Download, "Ouvrir le dossier")
                        .on_hover_text("dépose-y des .wav ou des .mp3 (30 s au plus)")
                        .clicked()
                    {
                        match Self::dossier_perso() {
                            Some(d) => ouvrir_dossier(&d),
                            None => self.message = Some("dossier introuvable".into()),
                        }
                    }
                });
                if !en_vocal {
                    ui.add_space(6.0);
                    ui::banner(
                        ui,
                        Tone::Warn,
                        "rejoins un salon vocal : ici, personne d'autre n'entendrait",
                        false,
                    );
                }
                ui.add_space(8.0);
                if self.en_cours {
                    ui.label(RichText::new("chargement des sons…").color(TEXT_DIM).size(12.0));
                } else if self.sons.is_empty() {
                    let dossier = Self::dossiers()
                        .into_iter()
                        .last()
                        .map(|d| d.display().to_string())
                        .unwrap_or_default();
                    ui.label(
                        RichText::new("Aucun son. Dépose des .wav ou des .mp3 dans :")
                            .color(TEXT_DIM)
                            .size(12.5),
                    );
                    ui.label(RichText::new(dossier).color(TEXT).size(11.5).monospace());
                    ui.label(
                        RichText::new("puis « Recharger ». Trente secondes au plus par son.")
                            .color(TEXT_FAINT)
                            .size(11.5),
                    );
                } else {
                    // Les touches 1 à 9 : les neuf premiers sons, tant
                    // qu'aucun champ de texte n'a le clavier — sinon un
                    // « 1 » tapé dans le chat ferait un bruit.
                    let clavier_libre = ctx.memory(|m| m.focused().is_none());
                    let touches = [
                        egui::Key::Num1,
                        egui::Key::Num2,
                        egui::Key::Num3,
                        egui::Key::Num4,
                        egui::Key::Num5,
                        egui::Key::Num6,
                        egui::Key::Num7,
                        egui::Key::Num8,
                        egui::Key::Num9,
                    ];
                    if clavier_libre && en_vocal {
                        for (i, touche) in touches.iter().enumerate() {
                            if i < self.sons.len() && ctx.input(|inp| inp.key_pressed(*touche)) {
                                commande = Some(Commande::Jouer(self.sons[i].pcm.clone()));
                            }
                        }
                    }
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                            for (i, son) in self.sons.iter().enumerate() {
                                let nom = crate::ellipsize(&son.nom, 22);
                                let libelle = if i < 9 {
                                    format!("{}  {nom}", i + 1)
                                } else {
                                    nom
                                };
                                let bouton = egui::Button::new(RichText::new(libelle).size(12.5).color(TEXT))
                                    .min_size(Vec2::new(124.0, 44.0))
                                    .fill(theme::BG_RAISED)
                                    .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
                                    .corner_radius(egui::CornerRadius::same(9));
                                let r = ui
                                    .add_enabled(en_vocal, bouton)
                                    .on_hover_text(format!("{} · {:.1} s", son.nom, son.duree_s()));
                                if r.clicked() {
                                    commande = Some(Commande::Jouer(son.pcm.clone()));
                                }
                            }
                        });
                    });
                }
                if let Some(m) = &self.message {
                    ui.add_space(4.0);
                    ui.label(RichText::new(m).color(TEXT_FAINT).size(11.0));
                }
            });
        if recharger {
            self.recharger();
        }
        if !ouvert {
            self.ouvert = false;
        }
        commande
    }
}

/// Tous les sons des dossiers, le premier nom gagne, triés par nom.
fn charger(dossiers: &[PathBuf]) -> Vec<Son> {
    let mut sons: Vec<Son> = Vec::new();
    for dossier in dossiers {
        let Ok(entrees) = std::fs::read_dir(dossier) else { continue };
        let mut fichiers: Vec<PathBuf> =
            entrees.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
        fichiers.sort();
        for chemin in fichiers {
            let Some(nom) = chemin.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            if sons.iter().any(|s| s.nom == nom) {
                continue;
            }
            match decoder(&chemin) {
                Ok(pcm) if !pcm.is_empty() => sons.push(Son { nom, pcm: Arc::new(pcm) }),
                Ok(_) => {}
                Err(e) => tracing::warn!("soundboard : {} ignoré : {e:#}", chemin.display()),
            }
        }
    }
    sons.sort_by_key(|s| s.nom.to_lowercase());
    if !sons.is_empty() {
        tracing::info!("soundboard : {} son(s)", sons.len());
    }
    sons
}

/// Un fichier en PCM mono 48 kHz, borné à [`DUREE_MAX_S`] : le WAV par le
/// décodeur des notifications, le reste (MP3, M4A…) par le décodeur de la
/// visionneuse — Windows seulement, comme elle.
fn decoder(chemin: &Path) -> anyhow::Result<Vec<f32>> {
    let ext = chemin
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut pcm = match ext.as_str() {
        "wav" => ki_voice::effects::load_wav_file(chemin)?,
        "mp3" | "m4a" | "aac" | "wma" | "flac" | "ogg" | "opus" | "mp4" | "webm" => {
            decoder_media(chemin)?
        }
        _ => anyhow::bail!("format inconnu"),
    };
    pcm.truncate(DUREE_MAX_S * CADENCE);
    Ok(pcm)
}

fn decoder_media(chemin: &Path) -> anyhow::Result<Vec<f32>> {
    let mut lecteur = ki_media::ouvrir(chemin)?;
    anyhow::ensure!(lecteur.infos().audio, "pas de piste audio");
    let mut pcm = Vec::new();
    loop {
        match lecteur.suivant(ki_media::Flux::Audio)? {
            ki_media::Paquet::Audio { mono, .. } => {
                pcm.extend_from_slice(&mono);
                if pcm.len() >= DUREE_MAX_S * CADENCE {
                    break;
                }
            }
            ki_media::Paquet::Fin => break,
            ki_media::Paquet::Image(_) => {}
        }
    }
    Ok(pcm)
}

fn ouvrir_dossier(dossier: &Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer.exe").arg(dossier).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(dossier).spawn();
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(dossier).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_wav_se_charge_et_un_son_trop_long_est_coupe() {
        let dir = std::env::temp_dir().join("ki-chat-soundboard-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Quarante secondes de 48 kHz mono, en PCM 16 bits : coupées à
        // trente. L'en-tête WAV est écrit à la main, le client n'embarque
        // pas d'écrivain WAV.
        let n = 40 * 48_000u32;
        let mut wav = Vec::with_capacity(44 + n as usize * 2);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + n * 2).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&48_000u32.to_le_bytes());
        wav.extend_from_slice(&(48_000u32 * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(n * 2).to_le_bytes());
        for i in 0..n {
            wav.extend_from_slice(&(((i as f32 * 0.03).sin() * 8000.0) as i16).to_le_bytes());
        }
        std::fs::write(dir.join("long.wav"), wav).unwrap();
        std::fs::write(dir.join("notes.txt"), b"pas un son").unwrap();

        let sons = charger(std::slice::from_ref(&dir));
        assert_eq!(sons.len(), 1);
        assert_eq!(sons[0].nom, "long");
        assert_eq!(sons[0].pcm.len(), DUREE_MAX_S * CADENCE);
        assert!((sons[0].duree_s() - 30.0).abs() < 0.01);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
