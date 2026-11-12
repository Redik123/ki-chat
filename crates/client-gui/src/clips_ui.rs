//! Les clips côté interface : la galerie, le partage vers le salon, le retrait du
//! serveur, la page de réglages.
//!
//! L'enregistreur lui-même est dans [`crate::clips`] ; ici vit ce que la fenêtre en
//! retient ([`Clips`]) et ce qu'elle dessine.

use ki_ui::typo::Texte;
use std::collections::HashMap;

use eframe::egui::{self, Color32, RichText, Sense, Vec2};
use ki_protocol::{ChannelId, ChannelInfo, ChannelKind};
use ki_ui::jetons::{espace, rayon, texte};

use crate::{atelier, clips, icons, images, partage, raccourci, theme, ui, visionneuse, KiApp};
use crate::icons::Icon;
use crate::televersement::{envoyer_morceaux, erreur_http, EnvoiMorceaux, SESSION_PERDUE};
use crate::theme::{ACCENT, DANGER, TEXT, TEXT_DIM, TEXT_FAINT};
use crate::ui::Ton;

/// Les clips : l'enregistreur et ses réglages, la galerie, le partage et le retrait.
pub(crate) struct Clips {
    /// L'enregistreur de clips et ses réglages (PLAN-CLIPS.md, C1).
    pub(crate) reglages: clips::Reglages,
    pub(crate) enregistreur: Option<clips::Enregistreur>,
    /// Dernier branchement des robinets audio sur le moteur — et du
    /// déclencheur du raccourci, refait avec eux.
    pub(crate) branche: Option<std::time::Instant>,
    /// La galerie : ouverte, sa liste, ses vignettes (None = illisible),
    /// celles en cours de décodage, et ce que les fils rapportent.
    pub(crate) ouverte: bool,
    /// La boîte « Partager le clip », quand elle est ouverte.
    pub(crate) partage: Option<PartageClip>,
    pub(crate) liste: Vec<clips::ClipInfo>,
    pub(crate) vignettes: HashMap<std::path::PathBuf, Option<egui::TextureHandle>>,
    pub(crate) vignettes_en_vol: std::collections::HashSet<std::path::PathBuf>,
    pub(crate) vignettes_recues: VignettesRecues,
    /// Le clip dont on demande confirmation avant de supprimer.
    pub(crate) suppression: Option<std::path::PathBuf>,
    /// « Retirer du serveur » : le clip en attente de confirmation, puis le
    /// résultat du fil qui s'en charge.
    pub(crate) retrait: Option<clips::ClipInfo>,
    pub(crate) retrait_resultat: Option<RetraitClip>,
    /// Le dossier choisi dans le dialogue natif (sur un fil).
    pub(crate) dossier_choisi: std::sync::Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
}

/// Ce qu'un clic sur un message demande à l'application. Le message est
/// peint par une fonction libre, qui n'a pas la main sur l'état : elle rend
/// la demande, l'appelant l'exécute.
/// Les vignettes de clips décodées par les fils de fond : (chemin du clip,
/// image ou `None` si illisible).
pub(crate) type VignettesRecues = std::sync::Arc<std::sync::Mutex<Vec<(std::path::PathBuf, Option<egui::ColorImage>)>>>;

/// Ce que le fil de « Retirer du serveur » rapporte : le nom du clip, ou l'erreur.
pub(crate) type RetraitClip = std::sync::Arc<std::sync::Mutex<Option<Result<String, String>>>>;

/// Le partage d'un clip dans un salon : la boîte de dialogue, puis l'envoi
/// (PLAN-CLIPS.md, jalon C2). L'original reste sur ce PC ; le serveur en
/// prépare une copie pour le fil, et poste le message au nom du membre.
pub(crate) struct PartageClip {
    pub(crate) chemin: std::path::PathBuf,
    /// Le nom du clip, sans extension.
    pub(crate) nom: String,
    pub(crate) taille: u64,
    pub(crate) fiche: Option<clips::Fiche>,
    pub(crate) salon: Option<ChannelId>,
    pub(crate) legende: String,
    /// Garder les voix des copains dans la version partagée.
    pub(crate) voix: bool,
    /// L'envoi en cours : ce que son fil en dit.
    pub(crate) envoi: Option<std::sync::Arc<std::sync::Mutex<EnvoiClip>>>,
}

#[derive(Clone, Default)]
pub(crate) struct EnvoiClip {
    pub(crate) pour_cent: u8,
    pub(crate) fini: Option<Result<(), String>>,
}

/// Le libellé d'une source de clips, tel que le sélecteur le montre.
pub(crate) fn clips_libelle_source(source: &clips::Source, sources: &partage::Sources) -> String {
    match source {
        clips::Source::Auto => "Automatique — la fenêtre du jeu, sinon l'écran".to_string(),
        clips::Source::Ecran(0) => "Écran principal".to_string(),
        clips::Source::Ecran(n) => sources
            .ecrans
            .iter()
            .find(|e| e.index == *n)
            .map(|e| format!("Écran {n} — {}", e.name))
            .unwrap_or_else(|| format!("Écran {n}")),
        clips::Source::Fenetre(t) => format!("Fenêtre : {t}"),
    }
}

impl KiApp {
    pub(crate) fn demarrer_clips(&mut self) {
        if self.clips.enregistreur.is_some() {
            return;
        }
        match clips::Enregistreur::demarrer(&self.clips.reglages) {
            Ok(e) => {
                self.clips.enregistreur = Some(e);
                self.clips.branche = None;
                self.brancher_declencheur();
            }
            Err(e) => {
                ki_voice::journal(format!("clips : démarrage impossible : {e:#}"));
                self.info = Some(format!("enregistreur de clips : {e:#}"));
            }
        }
    }

    pub(crate) fn arreter_clips(&mut self) {
        let Some(e) = self.clips.enregistreur.take() else { return };
        if let Some(p) = &self.ptt {
            p.action_raccourci(None);
        }
        if let Some(l) = &self.loupedeck {
            l.action_clip(None);
        }
        if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
            engine.brancher_micro(None);
            engine.brancher_copains(None);
        }
        e.arreter();
    }

    /// Le raccourci déclenche le clip lui-même, sur le fil qui a vu
    /// l'appui, sans passer par l'interface : réduite derrière le jeu, elle
    /// peut ne pas repeindre avant longtemps, et c'est à l'instant de
    /// l'appui que le tampon doit être photographié. Le son de confirmation
    /// part du fil d'écriture, pour la même raison. Rappelé chaque seconde
    /// par `tick_clips` : les réglages du son changent à chaud.
    pub(crate) fn brancher_declencheur(&mut self) {
        let Some(e) = self.clips.enregistreur.as_ref() else { return };
        if let Some(p) = &self.ptt {
            let d = e.declencheur();
            p.action_raccourci(Some(std::sync::Arc::new(move || d.appuyer())));
        }
        if let Some(l) = &self.loupedeck {
            let d = e.declencheur();
            l.action_clip(Some(std::sync::Arc::new(move || d.appuyer())));
        }
        let son = (self.clips.reglages.son && self.reglages.sfx_on && !self.reglages.sfx_muted.contains("clip"))
            .then(|| self.sounds.get("clip").cloned())
            .flatten();
        let engine = self.link.engine.clone();
        let volume = self.reglages.sfx_volume;
        e.quand_fini(Some(std::sync::Arc::new(move |r: &Result<clips::Clip, String>| {
            if r.is_err() {
                return;
            }
            if let (Some(pcm), Some(engine)) = (&son, engine.lock().unwrap().as_ref()) {
                engine.play_effect(pcm, volume);
            }
        })));
    }

    /// Le bouton « Clip ! » : le clip s'écrit sur un fil ; le résultat
    /// arrive par `tick_clips`.
    pub(crate) fn sauver_clip(&mut self) {
        let resultat = match self.clips.enregistreur.as_ref() {
            Some(e) => e.sauver(),
            None => Err("l'enregistreur n'est pas en marche".to_string()),
        };
        if let Err(m) = resultat {
            self.info = Some(format!("clip : {m}"));
        }
    }

    /// À chaque image : les robinets sur le moteur (qui peut avoir
    /// redémarré), le résultat d'une écriture, les erreurs de l'enregistreur.
    pub(crate) fn tick_clips(&mut self, ctx: &egui::Context) {
        if self.clips.enregistreur.is_none() {
            return;
        }
        clips::cadence_ui(ctx);
        if self.clips
            .branche
            .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(1))
        {
            self.clips.branche = Some(std::time::Instant::now());
            let robinets = self.clips.enregistreur.as_ref().map(|e| e.robinets());
            if let (Some((micro, copains)), Some(engine)) =
                (robinets, self.link.engine.lock().unwrap().as_ref())
            {
                engine.brancher_micro(micro);
                engine.brancher_copains(copains);
            }
            self.brancher_declencheur();
        }
        let evenement = self.clips.enregistreur.as_mut().and_then(|e| e.tick());
        match evenement {
            Some(Ok(clip)) => {
                let nom = clip
                    .chemin
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.info = Some(format!(
                    "clip enregistré : {nom} — {:.0} s, {}",
                    clip.duree_s,
                    clips::taille_lisible(clip.taille)
                ));
                // Le son est parti du fil d'écriture (`brancher_declencheur`).
                self.overlay.annoncer("Clip enregistré");
                if self.clips.ouverte {
                    self.rafraichir_clips();
                }
                self.loupedeck_etat.clips_perimes();
            }
            Some(Err(m)) => {
                ki_voice::journal(format!("clips : {m}"));
                self.info = Some(format!("clip raté : {m}"));
            }
            None => {}
        }
        let fatal = self.clips.enregistreur.as_ref().is_some_and(|e| e.fatal);
        if let Some(err) = self.clips.enregistreur.as_mut().and_then(|e| e.erreur.take()) {
            if fatal {
                ki_video::journal(format!("clips : arrêt de l'enregistreur — {err}"));
            }
            self.info = Some(format!("enregistreur de clips : {err}"));
        }
        // Le logiciel à 1080p60 en jeu, ou un enregistrement qui retombe en
        // panne : non. L'enregistreur l'a dit, on l'arrête.
        if fatal {
            self.arreter_clips();
        }
    }

    pub(crate) fn rafraichir_clips(&mut self) {
        self.clips.liste = clips::lister(&self.clips.reglages.dossier_effectif());
        let vivants: std::collections::HashSet<std::path::PathBuf> =
            self.clips.liste.iter().map(|c| c.chemin.clone()).collect();
        self.clips.vignettes.retain(|k, _| vivants.contains(k));
    }

    pub(crate) fn ouvrir_clips(&mut self) {
        self.clips.ouverte = true;
        self.rafraichir_clips();
    }

    /// Monte les vignettes décodées par les fils de fond, et lance celles
    /// qui manquent — décodage du JPEG, ou fabrication depuis la première
    /// image du clip s'il n'en a pas encore.
    pub(crate) fn preparer_vignettes_clips(&mut self, ctx: &egui::Context) {
        let arrivees = std::mem::take(&mut *self.clips.vignettes_recues.lock().unwrap());
        for (chemin, image) in arrivees {
            self.clips.vignettes_en_vol.remove(&chemin);
            let texture = image.map(|img| {
                ctx.load_texture(
                    format!("clip-{}", chemin.display()),
                    img,
                    egui::TextureOptions::LINEAR,
                )
            });
            self.clips.vignettes.insert(chemin, texture);
        }
        let mut lancees = 0;
        for c in &self.clips.liste {
            if lancees >= 4
                || self.clips.vignettes.contains_key(&c.chemin)
                || self.clips.vignettes_en_vol.contains(&c.chemin)
            {
                continue;
            }
            lancees += 1;
            self.clips.vignettes_en_vol.insert(c.chemin.clone());
            let chemin = c.chemin.clone();
            let vignette = c.vignette.clone();
            let slot = self.clips.vignettes_recues.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let fichier = vignette.or_else(|| clips::vignette(&chemin).ok());
                let image = fichier
                    .and_then(|v| std::fs::read(v).ok())
                    .and_then(|octets| images::decode(&octets));
                slot.lock().unwrap().push((chemin, image));
                ctx.request_repaint();
            });
        }
    }

    /// La galerie des clips : l'enregistreur, ses actions, et les clips du
    /// dossier en vignettes.
    pub(crate) fn clips_window(&mut self, ctx: &egui::Context) {
        if !self.clips.ouverte {
            return;
        }
        self.preparer_vignettes_clips(ctx);
        self.suivre_retrait_clip(ctx);
        let mut open = true;
        let mut ouvrir: Option<std::path::PathBuf> = None;
        let mut supprimer: Option<std::path::PathBuf> = None;
        let mut montrer: Option<std::path::PathBuf> = None;
        let mut partager: Option<clips::ClipInfo> = None;
        let mut modifier: Option<clips::ClipInfo> = None;
        let mut retirer: Option<clips::ClipInfo> = None;
        // 1 démarrer, 2 arrêter, 3 clip maintenant, 4 actualiser, 5 dossier.
        let mut action = 0u8;
        let etat = self.clips.enregistreur.as_ref().map(|e| e.etat());
        let total: u64 = self.clips.liste.iter().map(|c| c.taille).sum();
        let dossier = self.clips.reglages.dossier_effectif();
        let raccourci = self.clips.reglages.raccourci.label();
        let liste = &self.clips.liste;
        let vignettes = &self.clips.vignettes;
        ki_ui::fenetre::Fenetre::new("clips", "Clips")
            .ouverte(&mut open)
            .largeur(880.0)
            .haute()
            .taille_min(520.0, 0.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    match &etat {
                        Some(e) => {
                            ui::status_dot(ui, DANGER, "enregistreur en marche", 10.0);
                            ui.label(
                                Texte::Libelle.riche(ui, format!(
                                    "{:.0} s en mémoire · {:.0} Mo · {} · {}",
                                    e.secondes, e.megaoctets, e.encodeur, e.source
                                )),
                            );
                            if e.ecriture_en_cours {
                                ui.label(Texte::Libelle.riche_ton(ui, Ton::Accent, "écriture…"));
                            }
                        }
                        None => {
                            ui::status_dot(ui, TEXT_FAINT, "enregistreur éteint", 10.0);
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui::Bouton::icone_seule(Icon::Hash).bulle("Ouvrir le dossier").show(ui).clicked() {
                            action = 5;
                        }
                        if ui::Bouton::icone_seule(Icon::Refresh).bulle("Actualiser").show(ui).clicked() {
                            action = 4;
                        }
                        if etat.is_some() {
                            if ui::Bouton::new("Arrêter").icone(Icon::Close).teinte(Ton::Danger).show(ui).clicked() {
                                action = 2;
                            }
                            if ui::Bouton::new(&format!("Clip ! ({raccourci})")).icone(Icon::Film).principal().show(ui)
                                .clicked()
                            {
                                action = 3;
                            }
                        } else if ui::Bouton::new("Démarrer l'enregistreur").icone(Icon::Play).principal().show(ui)
                            .clicked()
                        {
                            action = 1;
                        }
                    });
                });
                ui::hint(
                    ui,
                    format!(
                        "{} clip{} · {} · {}",
                        liste.len(),
                        if liste.len() > 1 { "s" } else { "" },
                        clips::taille_lisible(total),
                        dossier.display()
                    ),
                );
                ui.add_space(espace::M);
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    if liste.is_empty() {
                        ui.label(
                            RichText::new(
                                "aucun clip pour l'instant — lance l'enregistreur, joue, et appuie sur la touche",
                            )
                            .color(TEXT_DIM),
                        );
                        return;
                    }
                    const LARGEUR: f32 = 224.0;
                    const HAUTEUR_IMAGE: f32 = 126.0;
                    let colonnes = ((ui.available_width() + 10.0) / (LARGEUR + 10.0)).floor().max(1.0) as usize;
                    egui::Grid::new("clips-grille").spacing([10.0, 10.0]).show(ui, |ui| {
                        for (i, c) in liste.iter().enumerate() {
                            let (rect, r) = ui.allocate_exact_size(
                                Vec2::new(LARGEUR, HAUTEUR_IMAGE + 40.0),
                                Sense::click(),
                            );
                            if ui.is_rect_visible(rect) {
                                let painter = ui.painter();
                                let image = egui::Rect::from_min_size(rect.min, Vec2::new(LARGEUR, HAUTEUR_IMAGE));
                                painter.rect_filled(image, egui::CornerRadius::same(rayon::M), theme::BG_DEEP);
                                match vignettes.get(&c.chemin) {
                                    Some(Some(t)) => {
                                        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                                        painter.image(t.id(), image, uv, Color32::WHITE);
                                    }
                                    Some(None) => {
                                        icons::draw(
                                            painter,
                                            egui::Rect::from_center_size(image.center(), Vec2::splat(28.0)),
                                            Icon::Film,
                                            TEXT_FAINT,
                                        );
                                    }
                                    None => {
                                        ui::spinner(painter, image.center(), 8.0, ui.input(|i| i.time), TEXT_FAINT);
                                    }
                                }
                                let centre = image.center();
                                let fond = if r.hovered() { theme::alpha(ACCENT, 230) } else { Color32::from_black_alpha(140) };
                                painter.circle_filled(centre, 18.0, fond);
                                icons::draw(
                                    painter,
                                    egui::Rect::from_center_size(centre + Vec2::new(2.0, 0.0), Vec2::splat(18.0)),
                                    Icon::Play,
                                    TEXT,
                                );
                                if r.hovered() {
                                    painter.rect_stroke(
                                        image,
                                        egui::CornerRadius::same(rayon::M),
                                        egui::Stroke::new(1.0_f32, theme::alpha(ACCENT, 160)),
                                        egui::StrokeKind::Inside,
                                    );
                                }
                                let nom: String = c.nom.chars().take(34).collect();
                                painter.text(
                                    egui::pos2(rect.left() + 2.0, image.bottom() + 6.0),
                                    egui::Align2::LEFT_TOP,
                                    nom,
                                    egui::FontId::proportional(texte::COURANT),
                                    TEXT,
                                );
                                let date = chrono::DateTime::<chrono::Local>::from(c.modifie).format("%d/%m %H:%M");
                                painter.text(
                                    egui::pos2(rect.left() + 2.0, image.bottom() + 24.0),
                                    egui::Align2::LEFT_TOP,
                                    format!("{date} · {}", clips::taille_lisible(c.taille)),
                                    egui::FontId::proportional(texte::PETIT),
                                    TEXT_FAINT,
                                );
                            }
                            let r = r.on_hover_cursor(egui::CursorIcon::PointingHand);
                            if r.clicked() {
                                ouvrir = Some(c.chemin.clone());
                            }
                            r.context_menu(|ui| {
                                if ui.button("Lire").clicked() {
                                    ouvrir = Some(c.chemin.clone());
                                    ui.close();
                                }
                                if ui.button("Partager dans un salon…").clicked() {
                                    partager = Some(c.clone());
                                    ui.close();
                                }
                                if ui.button("Modifier dans l'atelier…").clicked() {
                                    modifier = Some(c.clone());
                                    ui.close();
                                }
                                if c.fiche.as_ref().is_some_and(|f| f.serveur.is_some())
                                    && ui.button("Retirer du serveur…").clicked()
                                {
                                    retirer = Some(c.clone());
                                    ui.close();
                                }
                                if ui.button("Voir dans le dossier").clicked() {
                                    montrer = Some(c.chemin.clone());
                                    ui.close();
                                }
                                if ui.button("Supprimer…").clicked() {
                                    supprimer = Some(c.chemin.clone());
                                    ui.close();
                                }
                            });
                            if (i + 1) % colonnes == 0 {
                                ui.end_row();
                            }
                        }
                    });
                });
            });
        match action {
            1 => self.demarrer_clips(),
            2 => self.arreter_clips(),
            3 => self.sauver_clip(),
            4 => self.rafraichir_clips(),
            5 => clips::ouvrir_le_dossier(&self.clips.reglages.dossier_effectif()),
            _ => {}
        }
        if let Some(p) = montrer {
            clips::montrer_dans_le_dossier(&p);
        }
        if let Some(c) = partager {
            self.ouvrir_partage_clip(c);
        }
        if let Some(c) = modifier {
            self.ouvrir_atelier(c, ctx);
        }
        if let Some(c) = retirer {
            self.clips.retrait = Some(c);
        }
        if let Some(p) = ouvrir {
            let liste: Vec<visionneuse::Cible> =
                self.clips.liste.iter().map(|c| visionneuse::Cible::Fichier(c.chemin.clone())).collect();
            self.visionneuse.ouvrir(visionneuse::Cible::Fichier(p), liste);
        }
        if let Some(p) = supprimer {
            self.clips.suppression = Some(p);
        }
        self.confirmer_suppression_clip(ctx);
        self.confirmer_retrait_clip(ctx);
        if !open {
            self.clips.ouverte = false;
        }
    }

    /// « Partager dans un salon… » depuis la galerie : la boîte s'ouvre sur
    /// le salon courant.
    pub(crate) fn ouvrir_partage_clip(&mut self, c: clips::ClipInfo) {
        if self.conn.is_none() {
            self.info = Some("connecte-toi d'abord pour partager un clip".into());
            return;
        }
        if !self.can(ki_protocol::perm::UPLOAD_FILE) {
            self.info = Some("tu n'as pas le droit de partager des fichiers".into());
            return;
        }
        let textuels: Vec<&ChannelInfo> =
            self.channels.iter().filter(|ch| ch.kind == ChannelKind::Text).collect();
        let salon = self
            .current
            .filter(|id| textuels.iter().any(|ch| ch.id == *id))
            .or_else(|| textuels.iter().min_by_key(|ch| ch.position).map(|ch| ch.id));
        self.clips.partage = Some(PartageClip {
            chemin: c.chemin,
            nom: c.nom,
            taille: c.taille,
            fiche: c.fiche,
            salon,
            legende: String::new(),
            voix: true,
            envoi: None,
        });
    }

    /// « Modifier dans l'atelier… » depuis la galerie. Hors connexion, ou
    /// sans le droit de partager des fichiers, on regarde et l'on règle,
    /// sans exporter.
    pub(crate) fn ouvrir_atelier(&mut self, c: clips::ClipInfo, ctx: &egui::Context) {
        let reseau = (self.conn.is_some() && self.can(ki_protocol::perm::UPLOAD_FILE)).then(|| {
            atelier::Reseau {
                base: self.http_base(),
                jeton: self.jeton_http.clone(),
                agent: self.http_agent(),
            }
        });
        self.visionneuse.fermer();
        self.atelier.set_volume(self.visionneuse.volume);
        self.atelier.ouvrir(&c, reseau, ctx);
    }

    pub(crate) fn atelier_window(&mut self, ctx: &egui::Context) {
        if !self.atelier.est_ouvert() {
            return;
        }
        let mut textuels: Vec<&ChannelInfo> =
            self.channels.iter().filter(|ch| ch.kind == ChannelKind::Text).collect();
        textuels.sort_by_key(|ch| ch.position);
        let salons: Vec<(ChannelId, String)> = textuels.iter().map(|ch| (ch.id, ch.name.clone())).collect();
        self.atelier.ui(ctx, &salons, self.current);
    }

    /// La boîte « Partager le clip » : le salon, une légende, les voix des
    /// copains ; puis l'envoi, dont on suit la progression ici même.
    pub(crate) fn partage_clip_window(&mut self, ctx: &egui::Context) {
        // L'envoi a abouti : on le dit, et la boîte se ferme.
        let abouti = self.clips
            .partage
            .as_ref()
            .and_then(|p| p.envoi.as_ref())
            .is_some_and(|e| matches!(e.lock().unwrap().fini, Some(Ok(()))));
        if abouti {
            let salon = self.clips
                .partage
                .as_ref()
                .and_then(|p| p.salon)
                .and_then(|id| self.channels.iter().find(|ch| ch.id == id))
                .map(|ch| ch.name.clone())
                .unwrap_or_default();
            self.info = Some(format!(
                "clip partagé dans #{salon} — il apparaît dès que le serveur l'a préparé"
            ));
            self.clips.partage = None;
            return;
        }
        let salons: Vec<(ChannelId, String)> = {
            let mut v: Vec<&ChannelInfo> =
                self.channels.iter().filter(|ch| ch.kind == ChannelKind::Text).collect();
            v.sort_by_key(|ch| ch.position);
            v.iter().map(|ch| (ch.id, ch.name.clone())).collect()
        };
        let Some(p) = self.clips.partage.as_mut() else { return };
        let etat = p.envoi.as_ref().map(|e| e.lock().unwrap().clone());
        let en_cours = matches!(&etat, Some(EnvoiClip { fini: None, .. }));
        if en_cours {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        let mut lancer = false;
        let mut fermer = false;
        let boite = ki_ui::fenetre::Modale::new("partager_clip", "Partager le clip").largeur(440.0).show(ctx, |ui| {
                ui.label(Texte::Corps.gras(ui, &p.nom));
                let duree = p
                    .fiche
                    .as_ref()
                    .filter(|f| f.duree_s > 0.0)
                    .map(|f| format!(" · {:.0} s", f.duree_s))
                    .unwrap_or_default();
                ui::hint(ui, format!("{}{duree}", clips::taille_lisible(p.taille)));
                ui.add_space(espace::M);
                ui::field_label(ui, "Salon");
                let nom_salon = salons
                    .iter()
                    .find(|(id, _)| Some(*id) == p.salon)
                    .map(|(_, nom)| format!("#{nom}"))
                    .unwrap_or_else(|| "choisir…".into());
                egui::ComboBox::from_id_salt("partage-clip-salon")
                    .selected_text(nom_salon)
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for (id, nom) in &salons {
                            ui.selectable_value(&mut p.salon, Some(*id), format!("#{nom}"));
                        }
                    });
                ui.add_space(espace::S);
                ui::field_label(ui, "Légende");
                ui.add(ui::champ(&mut p.legende, "un mot, si tu veux"));
                if p.fiche.as_ref().is_some_and(|f| f.pistes.as_ref().is_some_and(|p| p.iter().any(|x| x == "copains"))) {
                    ui.add_space(espace::S);
                    ui.checkbox(&mut p.voix, "avec les voix des copains");
                    if !p.voix {
                        ui::hint(ui, "le serveur refait le son sans eux : leurs voix ne quittent pas ce PC");
                    }
                }
                ui.add_space(espace::S);
                ui::hint(
                    ui,
                    "l'original reste sur ce PC ; le serveur en prépare une copie pour le fil, avec une \
                     seule piste son, et poste le message en ton nom",
                );
                ui.add_space(espace::M);
                match &etat {
                    Some(EnvoiClip { fini: None, pour_cent }) => {
                        ui.add(
                            egui::ProgressBar::new(f32::from(*pour_cent) / 100.0)
                                .text(format!("envoi… {pour_cent} %")),
                        );
                    }
                    Some(EnvoiClip { fini: Some(Err(e)), .. }) => {
                        ui.label(Texte::Libelle.riche_ton(ui, Ton::Danger, format!("échec : {e}")));
                    }
                    _ => {}
                }
                ui.add_space(espace::XS);
                ui.horizontal(|ui| {
                    if en_cours {
                        ui.label(Texte::Libelle.riche(ui, "envoi en cours…"));
                        return;
                    }
                    ui.add_enabled_ui(p.salon.is_some(), |ui| {
                        if ui::Bouton::new("Partager").icone(Icon::Film).principal().show(ui).clicked() {
                            lancer = true;
                        }
                    });
                    if ui::Bouton::new("Annuler").icone(Icon::Close).show(ui).clicked() {
                        fermer = true;
                    }
                });
            });
        // Échap ou un clic à côté : comme « Annuler », sauf pendant l'envoi —
        // la boîte le suit jusqu'au bout.
        if fermer || (boite.fermee && !en_cours) {
            self.clips.partage = None;
        }
        if lancer {
            self.lancer_partage_clip();
        }
    }

    /// L'envoi : les morceaux, puis `/clips/fin` avec le salon, la légende
    /// et les pistes — sur un fil, la boîte suit.
    pub(crate) fn lancer_partage_clip(&mut self) {
        let base = self.http_base();
        let agent = self.http_agent();
        // Le jeton partagé avec l'application, relu à chaque requête : une
        // reconnexion pendant les minutes de l'envoi ne fait pas refuser
        // le morceau suivant avec l'ancien.
        let jeton = self.jeton_http.clone();
        let Some(p) = self.clips.partage.as_mut() else { return };
        let Some(salon) = p.salon else { return };
        let envoi = std::sync::Arc::new(std::sync::Mutex::new(EnvoiClip::default()));
        p.envoi = Some(envoi.clone());
        let chemin = p.chemin.clone();
        let nom = format!("{}.mp4", p.nom);
        let legende = p.legende.trim().to_string();
        let voix = p.voix;
        let pistes = p.fiche.as_ref().and_then(|f| f.pistes.clone());
        let base_fiche = base.clone();
        let nom_journal = p.nom.clone();
        std::thread::spawn(move || {
            let debut = std::time::Instant::now();
            let base_jeton = base.clone();
            let token_hex = move || jeton.lock().unwrap().pour(&base_jeton);
            let resultat = (|| -> Result<String, String> {
                let taille = std::fs::metadata(&chemin).map_err(|e| e.to_string())?.len();
                if taille == 0 {
                    return Err("fichier vide".into());
                }
                ki_voice::journal(format!(
                    "clips : partage de {nom_journal} ({} Mo) vers le salon {salon} : envoi",
                    taille / (1024 * 1024)
                ));
                let progres = {
                    let envoi = envoi.clone();
                    move |pc: u64, _: u32, _: u32| envoi.lock().unwrap().pour_cent = pc.min(100) as u8
                };
                let (upload, parts) =
                    match envoyer_morceaux(&agent, &base, &token_hex, &chemin, taille, &progres)? {
                        EnvoiMorceaux::Envoye { upload, parts } => (upload, parts),
                        EnvoiMorceaux::ServeurAncien => {
                            return Err("le serveur n'a pas encore le partage de clips (mise à jour \
                                        nécessaire)"
                                .into())
                        }
                    };
                let corps = serde_json::json!({
                    "channel": salon,
                    "legende": legende,
                    "pistes": pistes,
                    "voix": voix,
                    "nom": nom,
                });
                let reponse = agent
                    .post(&format!("{base}/clips/fin?upload={upload}&parts={parts}"))
                    .set("x-ki-token", &token_hex())
                    .set("Content-Type", "application/json")
                    .timeout(std::time::Duration::from_secs(300))
                    .send_string(&corps.to_string())
                    .map_err(|e| match e {
                        ureq::Error::Status(401, _) => SESSION_PERDUE.to_string(),
                        ureq::Error::Status(404, _) => {
                            "le serveur n'a pas encore le partage de clips (mise à jour nécessaire)"
                                .to_string()
                        }
                        autre => erreur_http(autre),
                    })?;
                // Le clip est sur ce serveur : la fiche s'en souvient, et
                // l'atelier n'aura pas à le renvoyer.
                let mut id_recu = String::from("?");
                if let Ok(json) = reponse.into_json::<serde_json::Value>() {
                    if let Some(id) = json["id"].as_str() {
                        id_recu = id.to_string();
                        clips::noter_serveur(&chemin, &base_fiche, id);
                    }
                }
                Ok(id_recu)
            })();
            match &resultat {
                Ok(id) => ki_voice::journal(format!(
                    "clips : partage de {nom_journal} : reçu par le serveur (clip {id}) en {:.0} s",
                    debut.elapsed().as_secs_f32()
                )),
                Err(e) => ki_voice::journal(format!(
                    "clips : partage de {nom_journal} : échec après {:.0} s : {e}",
                    debut.elapsed().as_secs_f32()
                )),
            }
            envoi.lock().unwrap().fini = Some(resultat.map(|_| ()));
        });
    }

    /// Le résultat de « Retirer du serveur », quand le fil a fini.
    pub(crate) fn suivre_retrait_clip(&mut self, ctx: &egui::Context) {
        let Some(r) = self.clips.retrait_resultat.as_ref() else { return };
        let fini = r.lock().unwrap().take();
        match fini {
            Some(Ok(nom)) => {
                self.info = Some(format!("{nom} : retiré du serveur — le message du fil garde un lien mort"));
                self.clips.retrait_resultat = None;
                self.rafraichir_clips();
            }
            Some(Err(e)) => {
                self.info = Some(format!("retrait impossible : {e}"));
                self.clips.retrait_resultat = None;
            }
            None => ctx.request_repaint_after(std::time::Duration::from_millis(300)),
        }
    }

    pub(crate) fn confirmer_retrait_clip(&mut self, ctx: &egui::Context) {
        let Some(c) = self.clips.retrait.clone() else { return };
        let mut decision: Option<bool> = None;
        let boite = ki_ui::fenetre::Modale::new("retirer_clip", "Retirer ce clip du serveur ?").largeur(400.0).show(ctx, |ui| {
                ui.label(Texte::Corps.gras(ui, &c.nom));
                ui::hint(
                    ui,
                    "la copie partagée et ses exports disparaissent du serveur ; le message du fil garde un \
                     lien mort. Ton clip reste sur ce PC.",
                );
                ui.add_space(espace::M);
                ui.horizontal(|ui| {
                    if ui::Bouton::new("Retirer").icone(Icon::Trash).teinte(Ton::Danger).show(ui).clicked() {
                        decision = Some(true);
                    }
                    if ui::Bouton::new("Annuler").icone(Icon::Close).show(ui).clicked() {
                        decision = Some(false);
                    }
                });
            });
        match decision {
            Some(true) => {
                self.clips.retrait = None;
                self.lancer_retrait_clip(c);
            }
            Some(false) => self.clips.retrait = None,
            None if boite.fermee => self.clips.retrait = None,
            None => {}
        }
    }

    /// `DELETE /clips/{id}` sur un fil ; la fiche oublie le serveur — un
    /// nouveau partage redéposera le clip.
    pub(crate) fn lancer_retrait_clip(&mut self, c: clips::ClipInfo) {
        let Some(fiche) = c.fiche.clone() else { return };
        let Some(id) = fiche.serveur.clone() else { return };
        if self.conn.is_none() {
            self.info = Some("connecte-toi d'abord".into());
            return;
        }
        let base = self.http_base();
        if fiche.serveur_base.as_deref() != Some(base.as_str()) {
            self.info = Some("ce clip a été déposé sur un autre serveur".into());
            return;
        }
        let agent = self.http_agent();
        let token_hex = format!("{:x}", self.voice_token);
        let resultat = std::sync::Arc::new(std::sync::Mutex::new(None));
        self.clips.retrait_resultat = Some(resultat.clone());
        let (chemin, nom) = (c.chemin.clone(), c.nom.clone());
        std::thread::spawn(move || {
            let r = (|| -> Result<String, String> {
                let reponse = agent
                    .delete(&format!("{base}/clips/{id}"))
                    .set("x-ki-token", &token_hex)
                    .timeout(std::time::Duration::from_secs(60))
                    .call();
                match reponse {
                    // Déjà parti du serveur (purgé) : la fiche l'oublie aussi.
                    Ok(_) => ki_voice::journal(format!("clips : {nom} retiré du serveur (clip {id})")),
                    Err(ureq::Error::Status(404, _)) => {
                        ki_voice::journal(format!("clips : {nom} : le serveur ne connaissait plus le clip {id}"))
                    }
                    Err(e) => {
                        let e = erreur_http(e);
                        ki_voice::journal(format!("clips : retrait de {nom} (clip {id}) : échec : {e}"));
                        return Err(e);
                    }
                }
                clips::oublier_serveur(&chemin);
                Ok(nom)
            })();
            *resultat.lock().unwrap() = Some(r);
        });
    }

    pub(crate) fn confirmer_suppression_clip(&mut self, ctx: &egui::Context) {
        let Some(chemin) = self.clips.suppression.clone() else { return };
        let nom = chemin.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut decision: Option<bool> = None;
        let boite = ki_ui::fenetre::Modale::new("supprimer_clip", "Supprimer ce clip ?").largeur(380.0).show(ctx, |ui| {
                ui.label(Texte::Corps.gras(ui, &nom));
                ui::hint(ui, "le fichier sera effacé de ce PC ; il n'y a pas de corbeille");
                ui.add_space(espace::M);
                ui.horizontal(|ui| {
                    if ui::Bouton::new("Supprimer").icone(Icon::Trash).teinte(Ton::Danger).show(ui).clicked() {
                        decision = Some(true);
                    }
                    if ui::Bouton::new("Annuler").icone(Icon::Close).show(ui).clicked() {
                        decision = Some(false);
                    }
                });
            });
        match decision {
            Some(true) => {
                if let Err(e) = std::fs::remove_file(&chemin) {
                    self.info = Some(format!("suppression impossible : {e}"));
                }
                if let Some(v) = clips::chemin_vignette(&chemin) {
                    let _ = std::fs::remove_file(v);
                }
                if let Some(f) = clips::chemin_fiche(&chemin) {
                    let _ = std::fs::remove_file(f);
                }
                self.clips.vignettes.remove(&chemin);
                self.clips.suppression = None;
                self.rafraichir_clips();
            }
            Some(false) => self.clips.suppression = None,
            None if boite.fermee => self.clips.suppression = None,
            None => {}
        }
    }

    /// L'onglet « Clips » des réglages.
    pub(crate) fn reglages_clips_ui(&mut self, ui: &mut egui::Ui) {
        let avant = self.clips.reglages.clone();
        let en_marche = self.clips.enregistreur.is_some();
        let mut action = 0u8;
        ui::group_title(ui, Icon::Film, "L'enregistreur");
        ui::hint(
            ui,
            "tant qu'il tourne, les dernières secondes de jeu sont gardées en mémoire ; un appui \
             sur la touche les écrit dans un fichier — rien ne quitte ce PC",
        );
        ui.horizontal(|ui| {
            if en_marche {
                if ui::Bouton::new("Arrêter").icone(Icon::Close).teinte(Ton::Danger).show(ui).clicked() {
                    action = 2;
                }
            } else if ui::Bouton::new("Démarrer").icone(Icon::Play).principal().show(ui).clicked() {
                action = 1;
            }
            ui.checkbox(&mut self.clips.reglages.au_demarrage, "Démarrer avec ki-chat");
        });
        if let Some(e) = self.clips.enregistreur.as_ref().map(|e| e.etat()) {
            ui::hint(
                ui,
                format!(
                    "{:.0} s en mémoire · {:.0} Mo · encodeur {} · {}",
                    e.secondes, e.megaoctets, e.encodeur, e.source
                ),
            );
        }
        ui.add_space(espace::L);

        ui::field_label(ui, "Touche");
        ui.horizontal(|ui| {
            let capture = self.ptt.as_ref().is_some_and(|p| p.en_capture());
            if capture {
                ui.label(RichText::new("appuie sur ta combinaison…").color(ACCENT));
                if ui::Bouton::new("Annuler").icone(Icon::Close).show(ui).clicked() {
                    if let Some(p) = &self.ptt {
                        p.capturer(false);
                    }
                }
            } else {
                ui.label(Texte::Corps.gras(ui, self.clips.reglages.raccourci.label()));
                if ui::Bouton::new("Changer").icone(Icon::Pencil).show(ui).clicked() {
                    if let Some(p) = &self.ptt {
                        p.capturer(true);
                    }
                }
            }
        });
        if let Some(r) = self.ptt.as_ref().and_then(|p| p.capturee()) {
            self.clips.reglages.raccourci = r;
        }
        ui::hint(ui, "avec Ctrl, Alt ou Maj de préférence, pour ne pas gêner le jeu — Alt+F10 comme NVIDIA");
        // Ce que Windows en a dit, tant que l'enregistreur tourne : tenue par
        // lui, la combinaison passe au-dessus du jeu ; refusée, on le dit,
        // parce qu'en jeu elle ne marchera sans doute pas.
        match self.ptt.as_ref().map(|p| p.etat_raccourci()) {
            Some(raccourci::Etat::Enregistre) => {
                ui::hint(ui, "tenue par Windows : elle marche au-dessus du jeu, même en plein écran")
            }
            Some(raccourci::Etat::Refuse) => {
                ui.label(
                    Texte::Aide.riche_ton(ui, Ton::Alerte, "cette combinaison est déjà prise par un autre programme (NVIDIA ?) : \
                         ki-chat la lit quand même, mais pas au-dessus d'un jeu — choisis-en une autre",),
                );
            }
            _ => {}
        }
        ui.add_space(espace::L);

        ui::field_label(ui, "Durée gardée");
        let libelles: Vec<String> = clips::DUREES.iter().map(|d| format!("{d} s")).collect();
        let durees: Vec<_> = clips::DUREES.iter().copied().zip(libelles.iter().map(String::as_str)).collect();
        ui::pastilles(ui, &mut self.clips.reglages.duree_s, &durees);
        ui::field_label(ui, "Qualité");
        egui::ComboBox::from_id_salt("clips_qualite")
            .width(300.0)
            .selected_text(RichText::new(self.clips.reglages.qualite.label()).color(TEXT))
            .show_ui(ui, |ui| {
                for q in clips::Qualite::TOUTES {
                    ui.selectable_value(&mut self.clips.reglages.qualite, q, q.label());
                }
            });
        ui::field_label(ui, "Cadence");
        ui::segmente(ui, &mut self.clips.reglages.fps, &[(30, "30 images/s"), (60, "60 images/s")]);
        ui::hint(ui, "en mémoire : 45 Mo pour 30 s en qualité équilibrée, 300 Mo pour 120 s en haute");
        ui::hint(
            ui,
            "avec une carte NVIDIA, capture et encodage restent sur la carte (NVENC seul, sans ses cœurs \
             de calcul) : ni le processeur ni le jeu ne le sentent",
        );
        ui.add_space(espace::L);

        ui::field_label(ui, "Source");
        if self.diffusion.sources.perimees() {
            self.diffusion.sources.rafraichir();
        }
        let libelle = clips_libelle_source(&self.clips.reglages.source, &self.diffusion.sources);
        egui::ComboBox::from_id_salt("clips_source")
            .width(360.0)
            .selected_text(RichText::new(libelle).color(TEXT))
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut self.clips.reglages.source,
                    clips::Source::Auto,
                    "Automatique — la fenêtre du jeu, sinon l'écran principal",
                );
                for e in &self.diffusion.sources.ecrans {
                    ui.selectable_value(
                        &mut self.clips.reglages.source,
                        clips::Source::Ecran(e.index),
                        format!("Écran {} — {} {}x{}", e.index, e.name, e.width, e.height),
                    );
                }
                if !self.diffusion.sources.fenetres.is_empty() {
                    ui.separator();
                }
                for f in &self.diffusion.sources.fenetres {
                    let txt = if f.process.is_empty() { f.title.clone() } else { format!("{} ({})", f.title, f.process) };
                    ui.selectable_value(&mut self.clips.reglages.source, clips::Source::Fenetre(f.title.clone()), txt);
                }
            });
        ui::hint(ui, "en automatique, l'enregistreur reconnaît VALORANT, CS2, Fortnite, Rocket League, Apex, LoL, Overwatch, R6, GTA V et Marvel Rivals");
        ui.add_space(espace::L);

        ui::field_label(ui, "Pistes audio");
        ui.checkbox(&mut self.clips.reglages.jeu, "Le son du jeu (tout le système, sauf ki-chat)");
        ui.checkbox(&mut self.clips.reglages.micro, "Mon micro");
        ui.checkbox(&mut self.clips.reglages.copains, "Les copains du salon vocal");
        ui::hint(
            ui,
            "chaque piste est gardée à part dans le fichier, plus un mélange en première piste \
             pour les lecteurs ordinaires ; le micro et les copains n'existent qu'en salon vocal",
        );
        ui.add_space(espace::L);

        ui::field_label(ui, "Dossier");
        ui.horizontal(|ui| {
            ui.label(
                Texte::Libelle.riche(ui, self.clips.reglages.dossier_effectif().display().to_string()),
            );
            if ui::Bouton::new("Changer…").icone(Icon::Pencil).show(ui).clicked() {
                let slot = self.clips.dossier_choisi.clone();
                std::thread::spawn(move || {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        *slot.lock().unwrap() = Some(d);
                    }
                });
            }
            if self.clips.reglages.dossier.is_some() && ui::Bouton::new("Par défaut").icone(Icon::Refresh).show(ui).clicked() {
                self.clips.reglages.dossier = None;
            }
        });
        if let Some(d) = self.clips.dossier_choisi.lock().unwrap().take() {
            self.clips.reglages.dossier = Some(d);
        }
        ui::hint(ui, "pointe-le sur un dossier synchronisé par Google Drive pour ordinateur, et les clips y montent tout seuls");
        ui.add_space(espace::S);
        ui.checkbox(&mut self.clips.reglages.son, "Un son quand le clip est enregistré");

        // Ce qui change la capture relance l'enregistreur, sans rien perdre
        // d'autre que le tampon en cours.
        let relancer = en_marche
            && self.clips.reglages != avant
            && self.clips.reglages.relance_necessaire(&avant);
        match action {
            1 => self.demarrer_clips(),
            2 => self.arreter_clips(),
            _ => {}
        }
        if relancer {
            self.arreter_clips();
            self.demarrer_clips();
        }
    }
}
