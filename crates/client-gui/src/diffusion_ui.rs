//! Le partage d'écran côté interface : diffuser le sien, regarder celui d'un autre.
//!
//! Les tuyaux (capture, encodeur, décodeur) sont dans [`crate::partage`] ; ici vit
//! ce que l'interface en retient — [`MaDiffusion`] pour ce que j'émets, [`MonRegard`]
//! pour ce que je regarde — et les fenêtres qui vont avec.

use ki_ui::typo::Texte;
use std::collections::HashMap;

use eframe::egui::{self, RichText};
use ki_protocol::{ClientMsg, Member, UserId};
use ki_ui::jetons::espace;

use crate::{partage, regard_detache, secours, ui, KiApp};
use crate::icons::Icon;
use crate::net::EmissionVideo as _;
use crate::theme::TEXT_DIM;
use crate::ui::Ton;

/// Ma diffusion d'écran : ses réglages, le flux en cours, ce que le serveur en demande.
pub(crate) struct MaDiffusion {
    /// Les réglages de diffusion (source, résolution, cadence, débit…).
    pub(crate) reglages: partage::Reglages,
    /// Écrans et fenêtres capturables, relevés à l'ouverture du sélecteur.
    pub(crate) sources: partage::Sources,
    /// Le sélecteur « Diffuser mon écran » est ouvert.
    pub(crate) ouverte: bool,
    /// Ma diffusion d'écran en cours.
    pub(crate) live: Option<partage::GoLive>,
    /// Clé générée, en attente du StreamGranted du serveur.
    pub(crate) attente: Option<[u8; 32]>,
    /// L'aperçu de ce que j'émets.
    pub(crate) texture: Option<egui::TextureHandle>,
    /// Le palier de débit que le serveur demande (un spectateur ne suit
    /// pas) ; `None` = le réglage.
    pub(crate) palier: Option<u32>,
    /// Ce palier vient de ma propre connexion (mes trames arrivent en
    /// retard au serveur), pas d'un spectateur.
    pub(crate) montant: bool,
    /// La qualité basse que le serveur fait encoder en plus, pour les
    /// connexions lentes (kbit/s).
    pub(crate) basse: Option<u32>,
    /// L'encodeur qui se règle tout seul : cadence puis hauteur, quand il
    /// ne suit pas.
    pub(crate) regulateur: partage::Regulateur,
    /// Qui regarde chaque stream (le mien compris), tel que le serveur
    /// l'annonce au salon vocal. Vide face à un serveur d'avant.
    pub(crate) spectateurs: HashMap<u32, Vec<UserId>>,
    /// La cadence instantanée de ce que j'émets.
    pub(crate) cadence: partage::Cadence,
    /// Dernière ligne de stats de diffusion consignée au journal.
    pub(crate) journal_flux: std::time::Instant,
    /// L'avertissement « pilote NVIDIA trop ancien pour NVENC » a été
    /// montré (une fois par session).
    pub(crate) pilote_averti: bool,
}

/// Le stream que je regarde : son flux, son image, son volume, sa fenêtre.
pub(crate) struct MonRegard {
    /// Le stream que je regarde.
    pub(crate) flux: Option<partage::Regard>,
    /// Sa dernière image.
    pub(crate) texture: Option<egui::TextureHandle>,
    /// La cadence instantanée de ce que je reçois.
    pub(crate) cadence: partage::Cadence,
    /// Volume du son du jeu du stream que je regarde (1.0 = 100 %).
    pub(crate) volume: f32,
    /// Le streamer est sur ce PC (un second ki-chat) : son son du jeu est
    /// coupé chez nous, sinon sa capture nous recapturerait sans fin.
    pub(crate) meme_machine: bool,
    /// Le stream regardé dans sa propre fenêtre système — à poser sur un
    /// second écran —, et en plein écran.
    pub(crate) detache: bool,
    pub(crate) plein_ecran: bool,
    /// Sa fenêtre, tant qu'il est détaché.
    pub(crate) fenetre: Option<regard_detache::Fenetre>,
}

impl KiApp {
    /// Une fois par session : si la précédente est morte en pleine
    /// diffusion, le dire au journal (donc aux diagnostics partagés) et à la
    /// personne — et passer l'encodeur en logiciel pour la prochaine fois,
    /// puisqu'un écran bleu au lancement d'un stream désigne d'abord le
    /// pilote de la carte. Elle remet NVENC dans les réglages si elle veut.
    pub(crate) fn verifier_diffusion_precedente(&mut self) {
        if self.demarrage_verifie {
            return;
        }
        self.demarrage_verifie = true;
        // L'enregistreur de clips : la session précédente est-elle morte
        // avec lui en marche ? Alors il reste éteint ce coup-ci, et on le dit.
        if secours::clips_interrompus() {
            ki_voice::journal(
                "la session précédente s'est terminée brutalement l'enregistreur de clips en marche"
                    .to_string(),
            );
            self.info = Some(
                "la dernière session s'est terminée brutalement avec l'enregistreur de clips en \
                 marche : il reste éteint ce coup-ci — relance-le depuis Clips si tu veux"
                    .into(),
            );
        } else if self.clips.reglages.au_demarrage {
            self.demarrer_clips();
        }
        let Some(encodeur) = secours::diffusion_interrompue() else { return };
        ki_voice::journal(format!(
            "la session précédente s'est terminée brutalement pendant une diffusion \
             (encodeur {encodeur}) : ki-chat tué ou machine tombée"
        ));
        let mut message = "la dernière diffusion s'est terminée brutalement (ki-chat fermé de \
                           force, ou le PC tombé)"
            .to_string();
        if encodeur != "Logiciel" && self.diffusion.reglages.encodeur != ki_video::EncoderChoice::Logiciel {
            self.diffusion.reglages.encodeur = ki_video::EncoderChoice::Logiciel;
            message.push_str(
                " : l'encodeur passe en logiciel pour la prochaine — remets NVENC dans les \
                 réglages de diffusion si tu veux réessayer",
            );
        }
        self.info = Some(message);
    }

    /// Demande à diffuser : génère la clé du stream, l'envoie au serveur, et
    /// attend son StreamGranted pour démarrer la capture.
    pub(crate) fn demarrer_diffusion(&mut self) {
        if self.diffusion.live.is_some() || self.diffusion.attente.is_some() {
            return;
        }
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).expect("hasard du système indisponible");
        self.diffusion.attente = Some(key);
        self.diffusion.regulateur = partage::Regulateur::new();
        self.diffusion.ouverte = false;
        let r = &self.diffusion.reglages;
        ki_voice::journal(format!(
            "diffusion demandée : {:?}, plafond {}p, {} i/s, {} kbit/s, encodeur {:?}, curseur {}, \
             aperçu {}",
            r.source, r.max_height, r.fps, r.kbps, r.encodeur, r.cursor, r.preview
        ));
        self.send(ClientMsg::StreamStart {
            meta: self.diffusion.reglages.meta(&key),
            stream_key: ki_protocol::hex_encode(&key),
            couches: true,
        });
    }

    /// Le serveur a accordé le stream : la capture démarre, l'émission aussi.
    pub(crate) fn diffusion_accordee(&mut self, stream_id: u32) {
        let Some(key) = self.diffusion.attente.take() else { return };
        let Some(conn) = &self.conn else { return };
        let force_idr = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cadence = std::sync::Arc::new(std::sync::Mutex::new(self.diffusion.reglages.meta(&key)));
        let stats = std::sync::Arc::new(ki_video::StageStats::default());
        // Les deux qualités : la basse, que le serveur demandera pour les
        // connexions lentes, et la haute, suspendue quand plus personne ne
        // la regarde.
        let qualites = std::sync::Arc::new(ki_video::Qualites::default());
        let Some(emit) = conn.video_emit(
            stream_id,
            key,
            force_idr.clone(),
            cadence.clone(),
            stats.clone(),
            qualites.clone(),
        ) else {
            self.info = Some("diffusion impossible : pas de connexion".into());
            return;
        };
        let apercu: std::sync::Arc<std::sync::Mutex<Option<egui::ColorImage>>> =
            Default::default();
        let sink: ki_video::FrameSink = {
            let (apercu, ctx) = (apercu.clone(), self.app_ctx.clone());
            std::sync::Arc::new(move |frame: ki_video::RgbaFrame| {
                // Converti ici, sur le fil vidéo : le fil d'interface n'a
                // plus que la texture à pousser.
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [frame.width, frame.height],
                    &frame.rgba,
                );
                *apercu.lock().unwrap() = Some(image);
                ctx.request_repaint();
            })
        };
        let reglages = self.diffusion.reglages.clone();
        // L'instant zéro des horodatages, partagé par l'image et le son.
        let origine = std::time::Instant::now();
        match ki_video::StreamerLoop::start(
            stats.clone(),
            sink.clone(),
            emit.clone(),
            reglages.config(),
            force_idr.clone(),
            origine,
            Some(qualites.clone()),
        ) {
            Ok(boucle) => {
                let avec_son = reglages.son;
                let seq_audio = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
                self.diffusion.live = Some(partage::GoLive {
                    boucle,
                    stats,
                    stream_id,
                    apercu,
                    emit,
                    sink,
                    force_idr,
                    cadence,
                    reglages,
                    key,
                    seq_audio: seq_audio.clone(),
                    audio: None,
                    origine,
                    qualites,
                });
                self.diffusion.cadence = partage::Cadence::new();
                self.diffusion.journal_flux = std::time::Instant::now();
                ki_voice::journal(format!("diffusion démarrée (stream {stream_id})"));
                secours::marquer_diffusion(&format!("{:?}", self.diffusion.reglages.encodeur));
                self.info = Some("tu diffuses ton écran".into());
                if avec_son {
                    let audio = self.demarrer_son_du_jeu(stream_id, key, seq_audio, origine);
                    if let Some(g) = &mut self.diffusion.live {
                        g.audio = audio;
                    }
                }
            }
            Err(e) => {
                ki_voice::journal(format!("diffusion impossible : {e:#}"));
                self.info = Some(format!("capture impossible : {e:#}"));
                self.send(ClientMsg::StreamStop);
            }
        }
    }

    /// Le son du jeu : la boucle de tout le système sauf ki-chat, en Opus
    /// stéréo, chiffré avec la clé du stream. Un échec (Windows trop
    /// ancien, refus du système) n'empêche pas la vidéo — il se dit.
    pub(crate) fn demarrer_son_du_jeu(
        &mut self,
        stream_id: u32,
        key: [u8; 32],
        seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
        origine: std::time::Instant,
    ) -> Option<ki_voice::jeu::GameAudio> {
        let emit = self.conn.as_ref()?.game_audio_emit(stream_id, key, seq);
        match ki_voice::jeu::GameAudio::start(96_000, emit, origine) {
            Ok(a) => Some(a),
            Err(e) => {
                ki_voice::journal(format!("son du jeu indisponible : {e:#}"));
                self.info = Some(format!("son du jeu indisponible : {e:#}"));
                None
            }
        }
    }

    /// Les réglages ont changé pendant une diffusion : la capture repart
    /// avec les nouveaux, le stream continue — même clé, même séquence, les
    /// spectateurs ne voient qu'une trame clé de plus.
    /// Les réglages de diffusion en vigueur : les siens, le débit plafonné
    /// au palier que le serveur demande quand un spectateur ne suit pas.
    pub(crate) fn reglages_effectifs(&self) -> partage::Reglages {
        let mut r = self.diffusion.reglages.clone();
        if let Some(p) = self.diffusion.palier {
            r.kbps = r.kbps.min(p.max(450));
            // La résolution et la cadence suivent le débit : à 1,5 Mbit/s,
            // du 720p30 net vaut mieux que du 1080p60 en bouillie — c'est
            // ce qui rendait la vidéo « infâme mais fluide ».
            if let Some((hauteur, fps)) = ki_video::qualite_pour_debit(r.kbps) {
                r.max_height = partage::plafonner_hauteur(r.max_height, hauteur);
                r.fps = r.fps.min(fps);
            }
        }
        if let Some((hauteur, fps)) = self.diffusion.regulateur.cran_actuel() {
            r.max_height = partage::plafonner_hauteur(r.max_height, hauteur);
            r.fps = r.fps.min(fps);
        }
        r
    }

    /// Le budget que le serveur demande pour ma diffusion : le palier de la
    /// haute (0 : plus personne ne la regarde, elle ne part plus), la basse
    /// à encoder en plus pour les connexions lentes, et d'où vient la
    /// contrainte.
    pub(crate) fn budget_de_diffusion(&mut self, kbps: u32, basse: Option<u32>, montant: bool) {
        let Some(g) = &self.diffusion.live else { return };
        // La haute suspendue : l'aperçu continue, rien ne part ; elle repart
        // d'une trame clé.
        if kbps == 0 {
            if g.qualites.suspendre_haute(true) {
                ki_voice::journal("diffusion : personne ne regarde la qualité haute — elle ne part plus".to_string());
            }
        } else if g.qualites.suspendre_haute(false) {
            g.force_idr.store(true, std::sync::atomic::Ordering::Relaxed);
            ki_voice::journal("diffusion : la qualité haute repart".to_string());
        }
        // La basse : réglée à chaud, sans relancer la capture.
        if g.qualites.basse() != basse {
            g.qualites.regler_basse(basse);
            ki_voice::journal(match basse {
                Some(k) => {
                    let (h, f) = ki_video::qualite_basse(k, g.stats.dims().1.max(360), g.reglages.fps);
                    format!("diffusion : qualité basse {h}p{f} à {k} kbit/s pour les connexions lentes")
                }
                None => "diffusion : plus personne en qualité basse".to_string(),
            });
        }
        self.diffusion.basse = basse;
        if kbps == 0 {
            return;
        }
        // Le palier de la haute, comme avant — la cause en plus.
        let palier = (kbps < self.diffusion.reglages.kbps).then_some(kbps);
        self.diffusion.montant = montant && palier.is_some();
        if palier != self.diffusion.palier {
            ki_voice::journal(match palier {
                Some(p) if montant => format!("diffusion : palier {p} kbit/s — ta connexion montante ne suit pas"),
                Some(p) => format!("diffusion : palier {p} kbit/s — un spectateur ne suit pas"),
                None => "diffusion : le palier revient au réglage".to_string(),
            });
            self.diffusion.palier = palier;
            self.rediffuser();
        }
    }

    pub(crate) fn rediffuser(&mut self) {
        let Some(g) = self.diffusion.live.take() else { return };
        let effectifs = self.reglages_effectifs();
        if g.reglages == effectifs {
            self.diffusion.live = Some(g);
            return;
        }
        match g.reconfigurer(&effectifs) {
            Ok(mut g) => {
                // Le son du jeu suit sa case, sans toucher à la vidéo.
                if !effectifs.son {
                    g.audio = None;
                } else if g.audio.is_none() {
                    // La séquence de la diffusion, pas une neuve : voir
                    // `GoLive::seq_audio`.
                    g.audio =
                        self.demarrer_son_du_jeu(g.stream_id, g.key, g.seq_audio.clone(), g.origine);
                }
                // Cadence et débit changent tout de suite ; les dimensions,
                // la couche réseau les annoncera d'elle-même à la première
                // trame si elles bougent.
                let mut meta = effectifs.meta(&g.key);
                // Le réglage reste le réglage : `effectifs` porte le débit
                // réduit par un palier, que le serveur prenait pour le
                // nouveau plafond — la qualité ne remontait plus jamais.
                meta.reglage = self.diffusion.reglages.kbps;
                let (w, h) = g.stats.dims();
                meta.width = w as u16;
                meta.height = h as u16;
                *g.cadence.lock().unwrap() = meta;
                self.send(ClientMsg::StreamMetaUpdate { meta });
                self.diffusion.texture = None;
                self.diffusion.live = Some(g);
            }
            Err(e) => {
                ki_voice::journal(format!("diffusion interrompue au changement de réglages : {e:#}"));
                self.info = Some(format!("diffusion interrompue : {e:#}"));
                self.diffusion.texture = None;
                // Arrêtée proprement, même en échec : le marqueur tombe.
                secours::lever_diffusion();
                self.send(ClientMsg::StreamStop);
            }
        }
    }

    /// Regarder le stream de `m` — si l'on partage son salon vocal, la seule
    /// condition que le serveur accepte. Un second clic ferme.
    pub(crate) fn regarder(&mut self, m: &Member) {
        let Some(stream_id) = m.streaming else { return };
        if Some(m.user_id) == self.my_id {
            return;
        }
        if self.regard.flux.as_ref().map(|r| r.stream_id) == Some(stream_id) {
            self.fermer_regard(true);
            return;
        }
        if m.voice.is_none() || m.voice != self.voice_channel {
            self.info = Some(format!(
                "rejoins le salon vocal de {} pour regarder son écran",
                m.username
            ));
            return;
        }
        self.send(ClientMsg::Watch { stream_id, couches: true });
    }

    /// Arrête sa propre diffusion, côté capture ET côté serveur.
    pub(crate) fn arreter_diffusion(&mut self) {
        self.diffusion.palier = None;
        self.diffusion.montant = false;
        self.diffusion.basse = None;
        self.diffusion.regulateur = partage::Regulateur::new();
        secours::lever_diffusion();
        if let Some(g) = self.diffusion.live.take() {
            ki_voice::journal(format!(
                "diffusion arrêtée (stream {}) : {} trames encodées, sautées capture {} / \
                 encodeur {} / réseau {}",
                g.stream_id,
                g.stats.encoded.load(std::sync::atomic::Ordering::Relaxed),
                g.stats.skipped.load(std::sync::atomic::Ordering::Relaxed),
                g.stats.enc_skipped.load(std::sync::atomic::Ordering::Relaxed),
                g.stats.net_dropped.load(std::sync::atomic::Ordering::Relaxed),
            ));
            g.arreter();
        }
        self.diffusion.attente = None;
        self.diffusion.texture = None;
        self.send(ClientMsg::StreamStop);
    }

    /// WatchAccepted : la clé est là, le fil décodeur démarre.
    /// `meme_machine` : le streamer est ce PC (un second ki-chat) — son son
    /// du jeu est coupé chez nous, sinon sa capture nous recapturerait sans
    /// fin.
    pub(crate) fn regard_accepte(&mut self, stream_id: u32, key_hex: &str, meme_machine: bool) {
        let Some(key) = ki_protocol::hex_decode(key_hex).and_then(|v| <[u8; 32]>::try_from(v).ok())
        else {
            self.info = Some("clé de stream invalide".into());
            return;
        };
        self.fermer_regard(true);
        self.regard.meme_machine = meme_machine;
        if meme_machine {
            ki_voice::journal("visionnage : streamer sur cette machine, son du jeu coupé ici".into());
        }
        // Bornés : un spectateur dont le décodage ne suit pas ne doit pas
        // entasser le retard en mémoire (voir `partage::FILE_TRAMES_MAX`).
        let (tx, rx) = std::sync::mpsc::sync_channel(partage::FILE_TRAMES_MAX);
        let (audio_tx, audio_rx) = std::sync::mpsc::sync_channel(partage::FILE_SON_MAX);
        if let Some(conn) = &self.conn {
            conn.set_video_feed(Some(tx));
            conn.set_game_audio_feed(Some(audio_tx));
        }
        if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
            engine.set_aux_gain(if meme_machine { 0.0 } else { self.regard.volume });
        }
        let streamer = self
            .members
            .iter()
            .find(|m| m.streaming == Some(stream_id))
            .map(|m| m.username.clone())
            .unwrap_or_else(|| "?".into());
        self.regard.flux = Some(partage::Regard::demarrer(
            stream_id,
            streamer,
            key,
            rx,
            audio_rx,
            self.link.engine.clone(),
            self.app_ctx.clone(),
        ));
        self.regard.texture = None;
    }

    /// Ferme le visionnage en cours. `prevenir` : envoyer Unwatch au serveur
    /// (inutile quand c'est lui qui vient d'annoncer la fin du stream).
    pub(crate) fn fermer_regard(&mut self, prevenir: bool) {
        if let Some(r) = self.regard.flux.take() {
            if prevenir {
                self.send(ClientMsg::Unwatch { stream_id: r.stream_id });
            }
            if let Some(conn) = &self.conn {
                conn.set_video_feed(None);
                conn.set_game_audio_feed(None);
            }
            r.arreter();
            // Ce qui restait de son du jeu à jouer ne se joue pas.
            if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
                engine.aux_clear();
            }
        }
        self.regard.texture = None;
        self.regard.fenetre = None;
        self.regard.detache = false;
        self.regard.plein_ecran = false;
    }

    /// Le sélecteur « Diffuser mon écran » : la source, les réglages, et le
    /// bouton qui lance. Pendant une diffusion, les mêmes réglages
    /// s'appliquent à chaud.
    pub(crate) fn diffusion_window(&mut self, ctx: &egui::Context) {
        if !self.diffusion.ouverte {
            return;
        }
        let en_direct = self.diffusion.live.is_some();
        let in_voice = self.voice_channel.is_some();
        let mut ouvert = true;
        let mut lancer = false;
        let mut arreter = false;
        let mut change = false;
        let titre = if en_direct { "Réglages de la diffusion" } else { "Diffuser mon écran" };
        egui::Window::new(titre)
            .open(&mut ouvert)
            .collapsible(false)
            .resizable(false)
            .default_width(360.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -20.0])
            .show(ctx, |ui| {
                if self.diffusion.sources.perimees() {
                    self.diffusion.sources.rafraichir();
                }
                change = partage::reglages_ui(ui, &mut self.diffusion.reglages, &mut self.diffusion.sources);
                ui.add_space(espace::L);
                ui.horizontal(|ui| {
                    if en_direct {
                        if ui::Bouton::new("Arrêter la diffusion").icone(Icon::Close).teinte(Ton::Danger).show(ui)
                            .clicked()
                        {
                            arreter = true;
                        }
                    } else if in_voice {
                        if ui::Bouton::new("Démarrer la diffusion").icone(Icon::Screen).principal().show(ui)
                            .clicked()
                        {
                            lancer = true;
                        }
                    } else {
                        ui.label(
                            Texte::Libelle.riche(ui, "rejoins un salon vocal pour diffuser"),
                        );
                    }
                });
            });
        if change {
            // De nouveaux réglages : le régulateur repart de zéro.
            self.diffusion.regulateur = partage::Regulateur::new();
            self.rediffuser();
        }
        if lancer {
            self.demarrer_diffusion();
        }
        if arreter {
            self.arreter_diffusion();
            self.diffusion.ouverte = false;
        }
        if !ouvert {
            self.diffusion.ouverte = false;
        }
    }

    /// Les fenêtres du partage d'écran : l'aperçu du streamer, et la vue du
    /// spectateur. Chaque image décodée arrive par un slot partagé ; elle
    /// devient texture ici, sur le fil UI — jamais ailleurs.
    pub(crate) fn partage_windows(&mut self, ctx: &egui::Context) {
        // --- Mon aperçu ---
        // La source peut s'évanouir sous nos pieds (la fenêtre diffusée se
        // ferme) : la diffusion s'arrête proprement plutôt que de figer.
        if self.diffusion.live.as_ref().is_some_and(|g| g.boucle.source_closed()) {
            ki_voice::journal("la fenêtre diffusée a été fermée : diffusion arrêtée".into());
            self.arreter_diffusion();
            self.info = Some("la fenêtre diffusée a été fermée : diffusion arrêtée".into());
        } else if self.diffusion.live.as_ref().is_some_and(|g| g.boucle.en_panne()) {
            // La capture ou l'encodeur a lâché, source toujours ouverte : plus
            // rien ne part. On conclut comme pour une fenêtre fermée, avec la
            // raison que le fil vidéo a laissée — l'arrêt l'emporterait.
            let raison = self.diffusion
                .live
                .as_ref()
                .and_then(|g| g.stats.prendre_avis())
                .unwrap_or_else(|| "la capture ou l'encodeur a lâché".into());
            ki_voice::journal(format!("diffusion arrêtée : {raison}"));
            self.arreter_diffusion();
            self.info = Some(format!("diffusion arrêtée : {raison}"));
        }
        if let Some(g) = &self.diffusion.live {
            let frame = g.apercu.lock().unwrap().take();
            if let Some(image) = frame {
                match &mut self.diffusion.texture {
                    Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                    None => {
                        self.diffusion.texture = Some(ctx.load_texture(
                            "go-live-apercu",
                            image,
                            egui::TextureOptions::LINEAR,
                        ))
                    }
                }
            }
            use std::sync::atomic::Ordering::Relaxed;
            self.diffusion.cadence.relever(
                g.stats.encoded.load(Relaxed),
                g.stats.encoded_bytes.load(Relaxed),
            );
            let (w, h) = g.stats.dims();
            let moteur = if g.stats.materiel.load(Relaxed) { "NVENC" } else { "CPU" };
            let etat = format!(
                "{moteur} · {w}x{h} · {:.0} i/s · {:.0} kbit/s · encodage {:.1} ms · sautées : \
                 capture {}, encodeur {}, réseau {}",
                self.diffusion.cadence.fps,
                self.diffusion.cadence.kbps,
                g.stats.encode_ms.get(),
                g.stats.skipped.load(Relaxed),
                g.stats.enc_skipped.load(Relaxed),
                g.stats.net_dropped.load(Relaxed),
            );
            let etat = match self.diffusion.palier {
                Some(p) if self.diffusion.montant => format!("{etat} · palier {p} kbit/s (ta connexion ne suit pas)"),
                Some(p) => format!("{etat} · palier {p} kbit/s (un spectateur ne suit pas)"),
                None => etat,
            };
            // La qualité basse, pour les connexions lentes ; la haute que
            // plus personne ne regarde.
            let etat = match g.stats.basse_dims() {
                (w, h) if h > 0 => format!(
                    "{etat} · qualité basse {w}x{h} à {} kbit/s (connexions lentes)",
                    self.diffusion.basse.unwrap_or(0)
                ),
                _ => etat,
            };
            let etat = if g.qualites.haute_suspendue() {
                format!("{etat} · qualité haute suspendue (personne ne la regarde)")
            } else {
                etat
            };
            // L'encodeur qui ne suit pas : un cran plus bas, et on le dit.
            let regle = self.diffusion.regulateur.tick(&g.stats, g.reglages.fps, g.reglages.preview);
            let etat = match self.diffusion.regulateur.cran_actuel() {
                Some((h, f)) => format!("{etat} · encodeur : {h}p{f} (il ne suivait pas au réglage)"),
                None => etat,
            };
            // La même ligne au journal toutes les dix secondes : c'est elle
            // qui dira, à distance, où une diffusion a coincé.
            if self.diffusion.journal_flux.elapsed() >= std::time::Duration::from_secs(10) {
                self.diffusion.journal_flux = std::time::Instant::now();
                ki_voice::journal(format!("diffusion : {etat}"));
            }
            // Ce que le fil vidéo a à dire à la personne qui diffuse : un
            // encodeur qui n'a pas pu s'ouvrir, un repli.
            if let Some(avis) = g.stats.prendre_avis() {
                ki_voice::journal(format!("diffusion : {avis}"));
                self.info = Some(avis);
            }
            // Qui regarde, d'après la dernière annonce du serveur — rien
            // face à un serveur d'avant, qui n'en dit rien.
            let qui_regarde = self.diffusion.spectateurs.get(&g.stream_id).map(|ids| {
                let noms: Vec<&str> = ids
                    .iter()
                    .filter_map(|id| self.members.iter().find(|m| m.user_id == *id))
                    .map(|m| m.username.as_str())
                    .collect();
                match noms.as_slice() {
                    [] => "personne ne regarde encore".to_string(),
                    [seul] => format!("1 spectateur : {seul}"),
                    _ => format!("{} spectateurs : {}", noms.len(), noms.join(", ")),
                }
            });
            let mut arreter = false;
            let mut reglages = false;
            let mut ouvert = true;
            let avec_apercu = g.reglages.preview;
            // La croix de la barre de titre arrête la diffusion : cette
            // fenêtre est son tableau de bord, la fermer c'est finir.
            egui::Window::new("Tu diffuses ton écran")
                .open(&mut ouvert)
                .default_width(380.0)
                .resizable(true)
                .show(ctx, |ui| {
                    if let Some(tex) = &self.diffusion.texture {
                        let dispo = ui.available_size();
                        let taille = tex.size_vec2();
                        // L'image garde toujours la place de la ligne de
                        // stats et des boutons sous elle.
                        let hauteur = (dispo.y - 64.0).max(60.0);
                        let echelle = (dispo.x / taille.x).min(hauteur / taille.y).min(1.0);
                        ui.image((tex.id(), taille * echelle.max(0.05)));
                    } else if avec_apercu {
                        ui.label(RichText::new("démarrage de la capture…").color(TEXT_DIM));
                    } else {
                        ui.label(
                            RichText::new("aperçu désactivé — la diffusion tourne")
                                .color(TEXT_DIM),
                        );
                    }
                    ui.add_space(espace::XS);
                    if let Some(qui) = &qui_regarde {
                        ui.label(Texte::Libelle.riche_ton(ui, Ton::Neutre, qui));
                    }
                    ui.label(Texte::Aide.riche(ui, etat).monospace());
                    ui.add_space(espace::S);
                    ui.horizontal(|ui| {
                        if ui::Bouton::new("Arrêter").icone(Icon::Close).show(ui).clicked() {
                            arreter = true;
                        }
                        if ui::Bouton::new("Réglages").icone(Icon::Sliders).show(ui).clicked() {
                            reglages = true;
                        }
                    });
                });
            if arreter || !ouvert {
                self.arreter_diffusion();
            }
            if reglages {
                self.diffusion.ouverte = true;
                self.diffusion.sources.rafraichir();
            }
            if let Some(mot) = regle {
                ki_voice::journal(format!("diffusion : {mot}"));
                self.info = Some(mot);
                self.rediffuser();
            }
        }

        // --- Ce que je regarde ---
        self.regard_windows(ctx);
    }

    /// La vue du spectateur : la fenêtre dans ki-chat, ou la fenêtre à part
    /// quand on l'a détachée. Chaque image décodée devient texture ici, sur
    /// le fil UI, quelle que soit la fenêtre qui la montre.
    pub(crate) fn regard_windows(&mut self, ctx: &egui::Context) {
        if self.regard.detache {
            self.regard_fenetre_detachee(ctx);
            return;
        }
        let Some(r) = &self.regard.flux else { return };
        let frame = r.image.lock().unwrap().take();
        if let Some(image) = frame {
            match &mut self.regard.texture {
                Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                None => {
                    self.regard.texture = Some(ctx.load_texture(
                        "regard",
                        image,
                        egui::TextureOptions::LINEAR,
                    ))
                }
            }
        }
        self.regard.cadence
            .relever(r.images.load(std::sync::atomic::Ordering::Relaxed), 0);
        let titre = format!("Écran de {}", r.streamer);
        let etat = regard_detache::ligne_etat(
            self.regard.texture.as_ref(),
            self.regard.cadence.fps,
            r.basse.load(std::sync::atomic::Ordering::Relaxed),
            r.saut.load(std::sync::atomic::Ordering::Relaxed),
        );
        let mut quitter = false;
        let mut ouvert = true;
        let mut volume_change = false;
        let mut detacher = false;
        let mut plein_ecran = false;
        // La croix de la barre de titre quitte le visionnage, comme le
        // bouton en bas — que l'on ne voit plus quand la fenêtre est
        // agrandie au-delà de l'écran.
        egui::Window::new(titre)
            .open(&mut ouvert)
            .default_width(900.0)
            .resizable(true)
            .show(ctx, |ui| {
                if let Some(tex) = &self.regard.texture {
                    let dispo = ui.available_size();
                    let taille = tex.size_vec2();
                    // L'image laisse toujours la place de la ligne du
                    // bas : bouton et cadence restent visibles.
                    let hauteur = (dispo.y - 44.0).max(60.0);
                    let echelle = (dispo.x / taille.x).min(hauteur / taille.y).min(2.0);
                    // Centrée : plus étroite que la fenêtre — bornée par la
                    // hauteur, ou un 360p qu'on n'agrandit pas plus de deux
                    // fois —, elle restait collée à gauche, une bande vide à
                    // droite.
                    ui.vertical_centered(|ui| ui.image((tex.id(), taille * echelle.max(0.05))));
                } else {
                    ui.label(
                        RichText::new("en attente de la première image…").color(TEXT_DIM),
                    );
                }
                ui.add_space(espace::S);
                ui.horizontal_wrapped(|ui| {
                    if ui::Bouton::new("Quitter le visionnage").icone(Icon::Close).show(ui).clicked() {
                        quitter = true;
                    }
                    // Sa propre fenêtre : sur le second écran, ou en plein
                    // écran — là où un stream se regarde vraiment.
                    if ui::Bouton::new("Détacher").icone(Icon::Screen).show(ui)
                        .on_hover_text("dans sa propre fenêtre, à poser sur un second écran")
                        .clicked()
                    {
                        detacher = true;
                    }
                    if ui::Bouton::new("Plein écran").icone(Icon::Screen).show(ui)
                        .on_hover_text("Échap pour en sortir, F11 ou double-clic pour y revenir")
                        .clicked()
                    {
                        plein_ecran = true;
                    }
                    ui.add_space(espace::M);
                    // Le son du jeu du streamer, à son volume à soi —
                    // mixé par le moteur vocal, il suit aussi le volume
                    // général et la mise en sourdine.
                    if self.regard.meme_machine {
                        ui.label(
                            Texte::Aide.riche(ui, "son du jeu coupé : le streamer est sur ce PC"),
                        )
                        .on_hover_text(
                            "sa capture exclut son propre ki-chat, pas celui-ci : \
                             rejouer son son ici le lui renverrait en boucle",
                        );
                    } else {
                        let mut pct = self.regard.volume * 100.0;
                        if ui
                            .add(
                                egui::Slider::new(&mut pct, 0.0..=200.0)
                                    .text("son du jeu")
                                    .suffix(" %")
                                    .integer(),
                            )
                            .on_hover_text("le son du stream est ramené au niveau des voix : 100 %, c'est ce niveau ; ce curseur l'ajuste")
                            .changed()
                        {
                            self.regard.volume = pct / 100.0;
                            volume_change = true;
                        }
                    }
                    if !etat.is_empty() {
                        ui.label(Texte::Aide.riche(ui, &etat).monospace());
                    }
                });
            });
        if volume_change {
            self.appliquer_volume_regard();
        }
        if detacher || plein_ecran {
            self.regard.detache = true;
            self.regard.plein_ecran = plein_ecran;
        }
        if quitter || !ouvert {
            self.fermer_regard(true);
        }
    }

    /// Le volume du son du jeu regardé, tel que le moteur le mixe.
    pub(crate) fn appliquer_volume_regard(&self) {
        if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
            engine.set_aux_gain(if self.regard.meme_machine { 0.0 } else { self.regard.volume });
        }
    }

    /// Le stream regardé dans sa propre fenêtre système (`regard_detache`) :
    /// elle se dessine seule, au rythme du stream ; on lui dit ce qu'on
    /// veut, elle nous dit ce qu'on lui a demandé.
    pub(crate) fn regard_fenetre_detachee(&mut self, ctx: &egui::Context) {
        let Some(r) = &self.regard.flux else { return };
        r.detache.store(true, std::sync::atomic::Ordering::Relaxed);
        let titre = format!("Écran de {}", r.streamer);
        let fenetre = self.regard
            .fenetre
            .get_or_insert_with(|| regard_detache::Fenetre::new(r, self.regard.texture.take()));
        let demandes = fenetre.echanger(self.regard.plein_ecran, self.regard.volume, self.regard.meme_machine);
        fenetre.montrer(ctx, &titre);
        if let Some(volume) = demandes.volume {
            self.regard.volume = volume;
            self.appliquer_volume_regard();
        }
        if demandes.basculer_plein {
            self.regard.plein_ecran = !self.regard.plein_ecran;
        }
        if demandes.quitter {
            self.fermer_regard(true);
        } else if demandes.rattacher {
            self.rattacher_regard(ctx);
        }
    }

    /// Le stream revient dans la fenêtre principale, avec son image.
    pub(crate) fn rattacher_regard(&mut self, ctx: &egui::Context) {
        self.regard.detache = false;
        self.regard.plein_ecran = false;
        if let Some(fenetre) = self.regard.fenetre.take() {
            self.regard.texture = fenetre.rendre_texture();
        }
        if let Some(r) = &self.regard.flux {
            r.detache.store(false, std::sync::atomic::Ordering::Relaxed);
        }
        // Si elle est réduite dans la zone, le stream disparaîtrait de
        // l'écran tout en continuant d'être décodé et mixé — on la rouvre
        // avec lui.
        if self.zone.reduite() {
            self.rouvrir_depuis_zone(ctx);
        }
    }

    /// Combien regardent le stream de `m`, s'il diffuse — selon la
    /// dernière annonce du serveur (rien face à un serveur d'avant).
    pub(crate) fn nb_spectateurs(&self, m: &Member) -> usize {
        m.streaming.and_then(|s| self.diffusion.spectateurs.get(&s)).map_or(0, Vec::len)
    }
}
