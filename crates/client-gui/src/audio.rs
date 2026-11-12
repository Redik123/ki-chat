//! L'audio de l'appli : ses réglages et l'état du vocal.
//!
//! [`ReglagesAudio`] est ce qui se sauve d'un lancement à l'autre : chaque clé y
//! est lue et écrite au même endroit ([`ReglagesAudio::load`],
//! [`ReglagesAudio::save`]), et un test vérifie que les deux se répondent.
//! [`Vocal`] est l'état du moment, qui ne se sauve pas : micro coupé, émission
//! en cours, débit choisi par le réseau, mesures en cours.

use std::collections::HashMap;

use crate::{changeur_ui, egaliseur_ui, net, reglages_casque, studio_ui, voisine_ui, KiApp};
use crate::ptt::PttKey;

/// Les réglages audio, tels qu'ils se sauvent : micro, casque, traitements,
/// raccourcis, volumes par personne.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReglagesAudio {
    /// Le volume de chacun, par compte local : mon pseudo → (personne → gain).
    /// Deux comptes sur le même PC ont chacun les leurs.
    pub(crate) all_volumes: HashMap<String, HashMap<u64, f32>>,
    /// Micro ouvert, push-to-talk ou activation vocale.
    pub(crate) mode: MicMode,
    /// La touche du push-to-talk.
    pub(crate) ptt_key: PttKey,
    /// Raccourcis globaux à bascule (en plus du push-to-talk).
    pub(crate) hotkey_micro: Option<PttKey>,
    pub(crate) hotkey_sourd: Option<PttKey>,
    /// Le micro et la sortie choisis (`None` : ceux de Windows).
    pub(crate) pref_input: Option<String>,
    pub(crate) pref_output: Option<String>,
    /// Moteur audio Windows natif (WASAPI direct) — cpal si décoché.
    pub(crate) native_audio: bool,
    /// Mode brut du micro : demande à Windows d'ignorer les effets tiers
    /// (Sonar, Nahimic…). Moteur natif seulement.
    pub(crate) raw_mic: bool,
    /// Micro en catégorie « communications » dès l'ouverture (sinon le moteur
    /// ne le propose que s'il détecte un micro affamé).
    pub(crate) comms_mic: bool,
    /// Sortie robuste : tampon de lecture profond (+70 ms de latence) pour
    /// les machines saturées et les cartes son USB fragiles.
    pub(crate) robust_output: bool,
    pub(crate) noise_mode: u8,
    pub(crate) input_gain: f32,
    pub(crate) output_gain: f32,
    pub(crate) vad_threshold: f32,
    /// Activation vocale par le réseau de neurones (Silero) plutôt que par
    /// le seuil d'amplitude, et la probabilité qui ouvre le micro.
    pub(crate) vad_neural: bool,
    pub(crate) vad_sens: f32,
    pub(crate) vad_hangover_ms: u32,
    /// Débit choisi (0 = Auto : piloté par les rapports qualité du serveur).
    pub(crate) bitrate: i32,
    /// Protection contre les pertes DRED : 0 = off, 1 = auto, 2 = toujours.
    pub(crate) dred_mode: u8,
    pub(crate) agc: bool,
    /// Annulation d'écho (les haut-parleurs qui reviennent dans le micro).
    pub(crate) aec_on: bool,
    pub(crate) agc_target: f32,
    pub(crate) gate_threshold: f32,
    /// Compression de sa voix à l'émission (`ki_voice::dynamique::COMPRESSION_*`).
    pub(crate) compression: u8,
    /// Un compresseur par personne entendue : celui qui hurle redescend.
    pub(crate) adoucir_cris: bool,
    /// Le nom du casque, tapé sur la page Casque (vide : celui de Windows).
    pub(crate) casque_nom: String,
    /// Le retour de sa voix dans le casque (sidetone), et son volume.
    pub(crate) retour_voix: bool,
    pub(crate) retour_voix_volume: f32,
    /// L'égaliseur des voix reçues : ses bandes.
    pub(crate) egaliseur: Vec<ki_voice::egaliseur::Bande>,
    /// L'égaliseur de sa propre voix, sur ce qui part.
    pub(crate) egaliseur_micro: Vec<ki_voice::egaliseur::Bande>,
    /// La page Casque en mode studio (toute la chaîne) plutôt que simple.
    pub(crate) mode_studio: bool,
    /// Le mode studio explique chaque réglage (masquable une fois connu).
    pub(crate) explications: bool,
    /// La chaîne studio de sa voix (porte, de-esser, compresseur perso,
    /// chaleur, plafond).
    pub(crate) studio: ki_voice::dynamique::ReglagesStudio,
    /// Ses profils de voix, et le nom tapé pour le prochain.
    pub(crate) profils_voix: Vec<studio_ui::ProfilVoix>,
    /// Le changeur de voix : allumé ou non, ses réglages, où part la voix
    /// changée (`ki_voice::changeur::VERS_*`), son raccourci et les
    /// pressions déjà traitées.
    pub(crate) changeur_actif: bool,
    pub(crate) changeur: ki_voice::changeur::ReglagesChangeur,
    pub(crate) changeur_vers: u8,
    pub(crate) hotkey_changeur: Option<PttKey>,
    /// Le micro pour les jeux : la voix traitée envoyée dans un câble
    /// virtuel, et le câble choisi (`None` : détecté d'office).
    pub(crate) micro_jeux: bool,
    pub(crate) micro_jeux_sortie: Option<String>,
    pub(crate) jitter_frames: usize,
    pub(crate) ptt_release_ms: u32,
    /// La voix d'à côté : la force de l'isolation (`ki_voice::proximite::
    /// PROXIMITE_*`), l'ancre apprise ou mesurée de sa voix par micro (nom
    /// du périphérique → dBFS), la mesure de cinq secondes en cours et son
    /// dernier verdict.
    pub(crate) proximite_force: u8,
    pub(crate) proximite_ancres: std::collections::HashMap<String, f32>,
}

/// « on » ou « off », comme les réglages l'écrivent depuis toujours.
fn on_off(vrai: bool) -> String {
    if vrai { "on" } else { "off" }.into()
}

impl ReglagesAudio {
    /// Relit les réglages. `get(clé, défaut)` rend ce qui est rangé sous la
    /// clé, ou le défaut : ce qui manque ou ne se lit pas garde sa valeur
    /// d'origine.
    pub(crate) fn load(get: impl Fn(&str, &str) -> String) -> Self {
        // Une porte de bruit réglée à 0 dB de profondeur ne faisait rien :
        // son seuil a pu être poussé n'importe où sans que ça s'entende. La
        // rallumer telle quelle couperait des bouts de voix — on la coupe,
        // elle se réactive d'un geste dans l'onglet Audio.
        let porte_inerte = studio_ui::porte_inerte(&get("studio", ""));
        if porte_inerte {
            ki_voice::journal(
                "ta porte de bruit avait une profondeur de 0 dB (elle ne coupait rien) : elle est \
                 désactivée — remets un seuil dans l'onglet Audio si tu la veux"
                    .into(),
            );
        }
        Self {
            all_volumes: serde_json::from_str(&get("user_volumes", "")).unwrap_or_default(),
            mode: MicMode::depuis(&get("mic_mode", "ptt")),
            ptt_key: PttKey::from_id(&get("ptt_key", "lalt")).unwrap_or(PttKey::LAlt),
            hotkey_micro: PttKey::from_id(&get("hotkey_micro", "")),
            hotkey_sourd: PttKey::from_id(&get("hotkey_sourd", "")),
            pref_input: Some(get("input_device", "")).filter(|s| !s.is_empty()),
            pref_output: Some(get("output_device", "")).filter(|s| !s.is_empty()),
            native_audio: get("native_audio", "on") != "off",
            raw_mic: get("raw_mic", "off") == "on",
            comms_mic: get("comms_mic", "off") == "on",
            robust_output: get("robust_output", "off") == "on",
            noise_mode: get("noise_mode", "1")
                .parse()
                .unwrap_or(ki_voice::NOISE_RNNOISE),
            input_gain: get("input_gain", "1.0").parse().unwrap_or(1.0),
            output_gain: get("output_gain", "1.0").parse().unwrap_or(1.0),
            vad_threshold: get("vad_threshold", "0.02").parse().unwrap_or(0.02),
            vad_neural: get("vad_neural", "on") != "off",
            vad_sens: get("vad_sens", "0.5").parse().unwrap_or(0.5),
            vad_hangover_ms: get("vad_hangover_ms", "400").parse().unwrap_or(400),
            bitrate: get("bitrate", "0").parse().unwrap_or(0),
            dred_mode: get("dred_mode", "1").parse().unwrap_or(1),
            agc: get("agc", "on") != "off",
            aec_on: get("aec", "on") != "off",
            agc_target: get("agc_target", "0.30").parse().unwrap_or(0.30),
            gate_threshold: if porte_inerte { 0.0 } else { get("gate_threshold", "0").parse().unwrap_or(0.0) },
            compression: get("compression", "1")
                .parse()
                .unwrap_or(ki_voice::dynamique::COMPRESSION_DOUCE)
                .min(ki_voice::dynamique::COMPRESSION_PERSO),
            adoucir_cris: get("adoucir_cris", "on") != "off",
            casque_nom: get("casque_nom", ""),
            retour_voix: get("retour_voix", "off") == "on",
            retour_voix_volume: get("retour_voix_volume", "0.5").parse().unwrap_or(0.5),
            egaliseur: ki_voice::egaliseur::lire(&get("egaliseur", "")),
            egaliseur_micro: ki_voice::egaliseur::lire(&get("egaliseur_micro", "")),
            mode_studio: get("mode_studio", "off") == "on",
            explications: get("explications", "on") != "off",
            studio: studio_ui::lire_studio(&get("studio", "")),
            profils_voix: studio_ui::lire_profils(&get("profils_voix", "[]")),
            changeur_actif: get("changeur_actif", "off") == "on",
            changeur: changeur_ui::lire_changeur(&get("changeur", "")),
            changeur_vers: get("changeur_vers", "0").parse::<u8>().unwrap_or(0).min(ki_voice::changeur::VERS_KICHAT),
            hotkey_changeur: PttKey::from_id(&get("hotkey_changeur", "")),
            micro_jeux: get("micro_jeux", "off") == "on",
            micro_jeux_sortie: Some(get("micro_jeux_sortie", "")).filter(|s| !s.is_empty()),
            jitter_frames: get("jitter_frames", "0").parse().unwrap_or(0),
            ptt_release_ms: get("ptt_release_ms", "100").parse().unwrap_or(100),
            proximite_force: get("proximite_force", "0")
                .parse::<u8>()
                .unwrap_or(ki_voice::proximite::PROXIMITE_OFF)
                .min(ki_voice::proximite::PROXIMITE_FORTE),
            proximite_ancres: voisine_ui::lire_ancres(&get("proximite_ancres", "{}")),
        
        }
    }

    /// Le seuil de l'activation vocale tel que le moteur le reçoit : il ne
    /// joue qu'en mode activation vocale.
    pub(crate) fn vad_effectif(&self) -> f32 {
        if self.mode == MicMode::Vad {
            self.vad_threshold
        } else {
            0.0
        }
    }

    /// Écrit les réglages, chacun sous la clé d'où [`ReglagesAudio::load`]
    /// le relit.
    pub(crate) fn save(&self, storage: &mut dyn eframe::Storage) {
        if let Ok(json) = serde_json::to_string(&self.all_volumes) {
            storage.set_string("user_volumes", json);
        }
        let touche = |t: Option<PttKey>| t.map(|k| k.id()).unwrap_or("").to_string();
        storage.set_string("mic_mode", self.mode.id().into());
        storage.set_string("ptt_key", self.ptt_key.id().into());
        storage.set_string("hotkey_micro", touche(self.hotkey_micro));
        storage.set_string("hotkey_sourd", touche(self.hotkey_sourd));
        storage.set_string("input_device", self.pref_input.clone().unwrap_or_default());
        storage.set_string("output_device", self.pref_output.clone().unwrap_or_default());
        storage.set_string("native_audio", on_off(self.native_audio));
        storage.set_string("raw_mic", on_off(self.raw_mic));
        storage.set_string("comms_mic", on_off(self.comms_mic));
        storage.set_string("robust_output", on_off(self.robust_output));
        storage.set_string("noise_mode", self.noise_mode.to_string());
        storage.set_string("input_gain", self.input_gain.to_string());
        storage.set_string("output_gain", self.output_gain.to_string());
        storage.set_string("vad_threshold", self.vad_threshold.to_string());
        storage.set_string("vad_neural", on_off(self.vad_neural));
        storage.set_string("vad_sens", self.vad_sens.to_string());
        storage.set_string("vad_hangover_ms", self.vad_hangover_ms.to_string());
        storage.set_string("bitrate", self.bitrate.to_string());
        storage.set_string("dred_mode", self.dred_mode.to_string());
        storage.set_string("agc", on_off(self.agc));
        storage.set_string("aec", on_off(self.aec_on));
        storage.set_string("agc_target", self.agc_target.to_string());
        storage.set_string("gate_threshold", self.gate_threshold.to_string());
        storage.set_string("compression", self.compression.to_string());
        storage.set_string("adoucir_cris", on_off(self.adoucir_cris));
        storage.set_string("casque_nom", self.casque_nom.clone());
        storage.set_string("retour_voix", on_off(self.retour_voix));
        storage.set_string("retour_voix_volume", self.retour_voix_volume.to_string());
        storage.set_string("egaliseur", ki_voice::egaliseur::ecrire(&self.egaliseur));
        storage.set_string("egaliseur_micro", ki_voice::egaliseur::ecrire(&self.egaliseur_micro));
        storage.set_string("mode_studio", on_off(self.mode_studio));
        storage.set_string("explications", on_off(self.explications));
        storage.set_string("studio", studio_ui::ecrire_studio(&self.studio));
        storage.set_string("profils_voix", studio_ui::ecrire_profils(&self.profils_voix));
        storage.set_string("changeur_actif", on_off(self.changeur_actif));
        storage.set_string("changeur", changeur_ui::ecrire_changeur(&self.changeur));
        storage.set_string("changeur_vers", self.changeur_vers.to_string());
        storage.set_string("hotkey_changeur", touche(self.hotkey_changeur));
        storage.set_string("micro_jeux", on_off(self.micro_jeux));
        storage.set_string("micro_jeux_sortie", self.micro_jeux_sortie.clone().unwrap_or_default());
        storage.set_string("jitter_frames", self.jitter_frames.to_string());
        storage.set_string("ptt_release_ms", self.ptt_release_ms.to_string());
        storage.set_string("proximite_force", self.proximite_force.to_string());
        storage.set_string("proximite_ancres", voisine_ui::ecrire_ancres(&self.proximite_ancres));
    }
}

/// L'état du vocal, qui ne se sauve pas : micro coupé, émission en cours, débit
/// choisi par le réseau, mesures et essais en cours.
pub(crate) struct Vocal {
    pub(crate) muted: bool,
    /// Sourd : plus rien ne sort de la carte son — les voix, le son du jeu
    /// d'un stream, les notifications — et le micro est coupé avec. Le
    /// volume de sortie réglé reste en mémoire pour le retour.
    pub(crate) sourd: bool,
    /// Pressions déjà traitées de chaque raccourci.
    pub(crate) hotkey_vues: (u32, u32),
    /// Micro armé (mute/PTT/mode) — commande envoyée au moteur.
    pub(crate) armed: bool,
    /// Émission réellement en cours (lue depuis le moteur, après VAD).
    pub(crate) transmitting: bool,
    /// Débit courant en mode Auto.
    pub(crate) auto_bitrate: i32,
    /// Rapports "propres" consécutifs (pour remonter le débit).
    pub(crate) good_reports: u8,
    /// Dernières pertes montantes signalées par le serveur (%).
    pub(crate) upstream_loss: Option<f32>,
    /// DRED actuellement engagé (mode auto).
    pub(crate) dred_active: bool,
    /// Dernière saturation du micro vue par l'onglet Audio : l'alerte reste
    /// affichée un moment après, au lieu de clignoter entre deux phrases.
    pub(crate) alerte_saturation: Option<std::time::Instant>,
    /// Lequel des deux la page Casque montre (`reglages_casque::VUE_*`).
    pub(crate) egaliseur_vue: u8,
    /// L'éditeur d'égaliseur (bande choisie, comparaison, spectre).
    pub(crate) eq_editeur: egaliseur_ui::EditeurEq,
    /// Le spectre que la page veut voir cette image, et celui que le moteur
    /// dépose (`ki_voice::ANALYSE_*`).
    pub(crate) analyse_voulue: u8,
    pub(crate) analyse_actuelle: u8,
    pub(crate) profil_nom: String,
    pub(crate) hotkey_changeur_vues: u32,
    /// Le calibrage des gains du micro en cours (page Casque), et son
    /// dernier verdict, affiché un moment.
    pub(crate) calibrage_micro: Option<reglages_casque::Calibrage>,
    pub(crate) calibrage_verdict: Option<(std::time::Instant, String)>,
    /// Les périphériques que suit le fil du matériel (dernier ordre donné).
    pub(crate) materiel_suivi: Option<(Option<String>, Option<String>)>,
    pub(crate) loopback: bool,
    /// Le dernier essai « enregistrer et réécouter » gardé en WAV (son
    /// numéro), et le dossier où il l'a été.
    pub(crate) essai_garde: u64,
    pub(crate) essai_dossier: Option<std::path::PathBuf>,
    /// L'imitation assistée du changeur : la voix visée (le nom de
    /// l'extrait, son empreinte), son analyse en cours sur un fil, l'erreur à
    /// dire, et l'empreinte de sa propre voix (mesurée sur l'essai de ce
    /// numéro).
    pub(crate) imitation_cible: Option<(String, ki_voice::imitation::EmpreinteVoix)>,
    pub(crate) imitation_calcul: Option<std::sync::mpsc::Receiver<Result<(String, ki_voice::imitation::EmpreinteVoix), String>>>,
    pub(crate) imitation_erreur: Option<String>,
    pub(crate) imitation_moi: Option<(u64, Option<ki_voice::imitation::EmpreinteVoix>)>,
    /// Calibration des seuils en cours : (départ, crête ambiante mesurée).
    pub(crate) calibrating: Option<(std::time::Instant, f32)>,
    pub(crate) mesure_prox: Option<voisine_ui::MesureProx>,
    pub(crate) mesure_prox_verdict: Option<String>,
    /// Page Casque : la crête brute retenue 300 ms pour la jauge « Marge »
    /// (sinon un cri de 150 ms y passait une fois sur deux), et le dernier
    /// gain total de la carte vu pour ce micro — quand il bouge, l'ancre de
    /// la voix d'à côté bouge d'autant.
    pub(crate) marge_crete: Option<(f32, std::time::Instant)>,
    pub(crate) gain_carte_vu: Option<(String, f32)>,
}

impl Vocal {
    /// L'état au lancement : hors vocal, rien de coupé, rien en cours.
    pub(crate) fn new() -> Self {
        Self {
            muted: false,
            sourd: false,
            hotkey_vues: (0, 0),
            armed: false,
            transmitting: false,
            auto_bitrate: DEBIT_AUTO_MAX,
            good_reports: 0,
            upstream_loss: None,
            dred_active: false,
            alerte_saturation: None,
            egaliseur_vue: reglages_casque::VUE_TA_VOIX,
            eq_editeur: egaliseur_ui::EditeurEq::new(),
            analyse_voulue: ki_voice::ANALYSE_AUCUNE,
            analyse_actuelle: ki_voice::ANALYSE_AUCUNE,
            profil_nom: String::new(),
            hotkey_changeur_vues: 0,
            calibrage_micro: None,
            calibrage_verdict: None,
            materiel_suivi: None,
            loopback: false,
            essai_garde: 0,
            essai_dossier: None,
            imitation_cible: None,
            imitation_calcul: None,
            imitation_erreur: None,
            imitation_moi: None,
            calibrating: None,
            mesure_prox: None,
            mesure_prox_verdict: None,
            marge_crete: None,
            gain_carte_vu: None,
        
        }
    }

    /// Le débit envoyé au moteur : celui qu'on a choisi, ou celui que le
    /// réseau a fait choisir (réglage sur Auto, `bitrate == 0`).
    pub(crate) fn debit_effectif(&self, reglages: &ReglagesAudio) -> i32 {
        if reglages.bitrate == 0 {
            self.auto_bitrate
        } else {
            reglages.bitrate
        }
    }

    /// La protection contre les pertes (DRED) envoyée au moteur : coupée,
    /// toujours là, ou — en automatique — engagée tant que le réseau perd.
    pub(crate) fn dred_effectif(&self, reglages: &ReglagesAudio) -> i32 {
        match reglages.dred_mode {
            0 => 0,
            2 => ki_voice::DRED_DEFAULT,
            _ if self.dred_active => ki_voice::DRED_DEFAULT,
            _ => 0,
        }
    }

    /// Le serveur rapporte les pertes montantes (en %), toutes les cinq
    /// secondes. Rend vrai quand le débit ou la protection a changé : les
    /// réglages sont alors à repousser au moteur.
    ///
    /// Un rapport est propre sous 1 % de pertes ; tout autre remet le compte
    /// des rapports propres à zéro. La protection automatique s'engage dès
    /// 2 % de pertes et se relâche sous 0,5 % après deux rapports propres,
    /// que le débit soit automatique ou choisi à la main. Le débit
    /// automatique baisse d'un cran au-dessus de 5 % de pertes et remonte
    /// d'un cran après trois rapports propres de suite (15 s), sans dépasser
    /// 64 kbit/s. Un débit choisi à la main n'est jamais piloté.
    pub(crate) fn sur_rapport_reseau(&mut self, reglages: &ReglagesAudio, pertes: f32) -> bool {
        self.upstream_loss = Some(pertes);
        let mut change = false;
        if reglages.dred_mode == 1 {
            let avant = self.dred_active;
            if pertes > 2.0 {
                self.dred_active = true;
            } else if pertes < 0.5 && self.good_reports >= 2 {
                self.dred_active = false;
            }
            change |= avant != self.dred_active;
        }
        // Les rapports propres se comptent quel que soit le débit : la
        // protection en a besoin pour se relâcher. Ils ne l'étaient qu'en
        // débit automatique, et avec un débit choisi à la main elle restait
        // engagée jusqu'à la fin de la session.
        let propre = pertes < 1.0;
        self.good_reports = if propre { self.good_reports.saturating_add(1) } else { 0 };
        if reglages.bitrate != 0 {
            return change;
        }
        let cran = BITRATES.iter().position(|&b| b == self.auto_bitrate).unwrap_or(3);
        if pertes > 5.0 {
            if cran > 0 {
                self.auto_bitrate = BITRATES[cran - 1];
                change = true;
            }
        } else if propre && self.good_reports >= 3 && self.auto_bitrate < DEBIT_AUTO_MAX {
            self.auto_bitrate = BITRATES[cran + 1].min(DEBIT_AUTO_MAX);
            self.good_reports = 0;
            change = true;
        }
        change
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MicMode {
    Open,
    Ptt,
    /// Activation vocale : émet quand le niveau dépasse le seuil réglé.
    Vad,
}

impl MicMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            MicMode::Open => "Micro ouvert",
            MicMode::Ptt => "Push-to-talk",
            MicMode::Vad => "Activation vocale",
        }
    }

    pub(crate) fn id(self) -> &'static str {
        match self {
            MicMode::Open => "open",
            MicMode::Ptt => "ptt",
            MicMode::Vad => "vad",
        }
    }

    /// L'inverse de [`MicMode::id`] ; ce qui ne se lit pas vaut push-to-talk.
    pub(crate) fn depuis(id: &str) -> Self {
        match id {
            "open" => MicMode::Open,
            "vad" => MicMode::Vad,
            _ => MicMode::Ptt,
        }
    }
}

/// Débits Opus proposés (bits/s).
pub(crate) const BITRATES: [i32; 6] = [24_000, 32_000, 48_000, 64_000, 96_000, 128_000];

/// Le débit d'où part le réglage Auto, et qu'il ne dépasse pas : au-delà,
/// la voix n'y gagne rien d'audible.
const DEBIT_AUTO_MAX: i32 = 64_000;

impl KiApp {
    /// Le serveur rapporte les pertes montantes : l'état du vocal s'y adapte
    /// ([`Vocal::sur_rapport_reseau`]), et ce qui a changé repart au moteur.
    pub(crate) fn on_net_quality(&mut self, loss_pct: f32) {
        if self.vocal.sur_rapport_reseau(&self.audio, loss_pct) {
            self.apply_audio_settings();
        }
    }

    /// Pousse tous les réglages audio actuels vers le moteur, à chaud.
    pub(crate) fn apply_audio_settings(&self) {
        if let Some(engine) = self.link.engine.lock().unwrap().as_ref() {
            engine.set_noise_mode(self.audio.noise_mode);
            engine.set_input_gain(self.audio.input_gain);
            // Sourd : la sortie à zéro, le réglage de volume gardé pour le
            // retour.
            engine.set_output_gain(if self.vocal.sourd { 0.0 } else { self.audio.output_gain });
            engine.set_vad_threshold(self.audio.vad_effectif());
            engine.set_vad_hangover_ms(self.audio.vad_hangover_ms);
            engine.set_vad_neural(self.audio.vad_neural);
            engine.set_vad_sensitivity(self.audio.vad_sens);
            engine.set_bitrate(self.vocal.debit_effectif(&self.audio));
            // Pendant la calibration sur le bruit de la pièce, la porte et le
            // gain automatique restent suspendus, quel que soit le réglage
            // qui repasse par ici entre-temps (débit auto, DRED, un curseur).
            let calibration = self.vocal.calibrating.is_some();
            engine.set_agc(self.audio.agc && !calibration);
            engine.set_aec(self.audio.aec_on);
            engine.set_agc_target(self.audio.agc_target);
            engine.set_gate_threshold(if calibration { 0.0 } else { self.audio.gate_threshold });
            engine.set_proximite(self.proximite_reglages());
            engine.set_compression(self.audio.compression);
            engine.set_adoucir_cris(self.audio.adoucir_cris);
            engine.set_retour_voix(self.audio.retour_voix, self.audio.retour_voix_volume);
            let (ecoute, voix) = self.egaliseurs_effectifs();
            engine.set_egaliseur(&ecoute);
            engine.set_egaliseur_micro(&voix);
            engine.set_studio(self.audio.studio);
            engine.set_changeur(self.audio.changeur_actif.then_some(self.audio.changeur), self.audio.changeur_vers);
            engine.set_jitter_frames(self.audio.jitter_frames);
            engine.set_dred(self.vocal.dred_effectif(&self.audio));
            engine.set_loopback(self.vocal.loopback);
        }
    }

    pub(crate) fn voice_prefs(&self) -> net::VoicePrefs {
        net::VoicePrefs {
            input_device: self.audio.pref_input.clone(),
            output_device: self.audio.pref_output.clone(),
            native_audio: self.audio.native_audio,
            medias: self.visionneuse.file.clone(),
            raw_mic: self.audio.raw_mic,
            robust_output: self.audio.robust_output,
            comms_mic: self.audio.comms_mic,
            noise_mode: self.audio.noise_mode,
            volumes: self.audio
                .all_volumes
                .get(self.username.trim())
                .cloned()
                .unwrap_or_default(),
            input_gain: self.audio.input_gain,
            output_gain: self.audio.output_gain,
            vad_threshold: self.audio.vad_effectif(),
            vad_hangover_ms: self.audio.vad_hangover_ms,
            vad_neural: self.audio.vad_neural,
            vad_sensitivity: self.audio.vad_sens,
            // Le PC garde Silero seul juge, comme jusqu'ici.
            vad_garde: false,
            bitrate: self.vocal.debit_effectif(&self.audio),
            agc: self.audio.agc,
            aec: self.audio.aec_on,
            agc_target: self.audio.agc_target,
            gate_threshold: self.audio.gate_threshold,
            proximite: self.proximite_reglages(),
            compression: self.audio.compression,
            adoucir_cris: self.audio.adoucir_cris,
            retour_voix: self.audio.retour_voix,
            retour_voix_gain: self.audio.retour_voix_volume,
            egaliseur: self.audio.egaliseur.clone(),
            egaliseur_micro: self.audio.egaliseur_micro.clone(),
            studio: self.audio.studio,
            changeur: self.audio.changeur_actif.then_some(self.audio.changeur),
            changeur_vers: self.audio.changeur_vers,
            micro_jeux: self.micro_jeux_resolu(),
            jitter_frames: self.audio.jitter_frames,
            dred: self.vocal.dred_effectif(&self.audio),
            moteur_a_la_demande: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[derive(Default)]
    struct EnMemoire(BTreeMap<String, String>);

    impl eframe::Storage for EnMemoire {
        fn get_string(&self, cle: &str) -> Option<String> {
            self.0.get(cle).cloned()
        }
        fn set_string(&mut self, cle: &str, valeur: String) {
            self.0.insert(cle.to_string(), valeur);
        }
        fn remove_string(&mut self, cle: &str) {
            self.0.remove(cle);
        }
        fn flush(&mut self) {}
    }

    fn lire(rangement: &EnMemoire) -> ReglagesAudio {
        ReglagesAudio::load(|cle, defaut| rangement.0.get(cle).cloned().unwrap_or_else(|| defaut.to_string()))
    }

    fn sauver(reglages: &ReglagesAudio) -> EnMemoire {
        let mut rangement = EnMemoire::default();
        reglages.save(&mut rangement);
        rangement
    }

    /// Des réglages dont aucun n'est resté à sa valeur d'origine.
    fn tout_change() -> ReglagesAudio {
        let mut r = lire(&EnMemoire::default());
        r.all_volumes.insert("moi".into(), HashMap::from([(7, 0.5)]));
        r.mode = MicMode::Vad;
        r.ptt_key = PttKey::from_id("caps").unwrap();
        r.hotkey_micro = PttKey::from_id("f5");
        r.hotkey_sourd = PttKey::from_id("f6");
        r.pref_input = Some("Micro d'essai".into());
        r.pref_output = Some("Casque d'essai".into());
        r.native_audio = !r.native_audio;
        r.raw_mic = !r.raw_mic;
        r.comms_mic = !r.comms_mic;
        r.robust_output = !r.robust_output;
        r.noise_mode = 2;
        r.input_gain = 1.5;
        r.output_gain = 0.8;
        r.vad_threshold = 0.05;
        r.vad_neural = !r.vad_neural;
        r.vad_sens = 0.7;
        r.vad_hangover_ms = 250;
        r.bitrate = 48_000;
        r.dred_mode = 2;
        r.agc = !r.agc;
        r.aec_on = !r.aec_on;
        r.agc_target = 0.25;
        r.gate_threshold = 0.01;
        r.compression = ki_voice::dynamique::COMPRESSION_FORTE;
        r.adoucir_cris = !r.adoucir_cris;
        r.casque_nom = "Casque d'essai".into();
        r.retour_voix = !r.retour_voix;
        r.retour_voix_volume = 0.3;
        r.egaliseur = ki_voice::egaliseur::lire("0,3,0,0,0");
        r.egaliseur_micro = ki_voice::egaliseur::lire("0,0,-2,0,0");
        r.mode_studio = !r.mode_studio;
        r.explications = !r.explications;
        r.studio.chaleur = 0.5;
        r.profils_voix.push(studio_ui::ProfilVoix {
            nom: "essai".into(),
            egaliseur: String::new(),
            studio: String::new(),
            compression: 1,
            porte: 0.0,
            agc: true,
            agc_cible: 0.3,
        });
        r.changeur_actif = !r.changeur_actif;
        r.changeur.hauteur = 3.0;
        r.changeur_vers = ki_voice::changeur::VERS_KICHAT;
        r.hotkey_changeur = PttKey::from_id("f7");
        r.micro_jeux = !r.micro_jeux;
        r.micro_jeux_sortie = Some("Câble d'essai".into());
        r.jitter_frames = 4;
        r.ptt_release_ms = 200;
        r.proximite_force = ki_voice::proximite::PROXIMITE_FORTE;
        r.proximite_ancres.insert("Micro d'essai".into(), -21.5);
        r
    }

    /// Ce que ce module promet : une clé écrite est relue, et relue telle
    /// qu'elle a été écrite. Un réglage ajouté à `save` sans l'être à `load`
    /// (ou l'inverse) revient à sa valeur d'origine à chaque lancement, sans
    /// rien dire.
    #[test]
    fn ce_qui_se_sauve_se_relit_tel_quel() {
        let origine = lire(&EnMemoire::default());
        assert_eq!(lire(&sauver(&origine)), origine, "les valeurs d'origine ne survivent pas à un aller-retour");

        let change = tout_change();
        let range = sauver(&change);
        assert_eq!(lire(&range), change, "un réglage changé ne survit pas à un aller-retour");

        // Et le test lui-même couvre tout : chaque clé écrite porte ici une
        // valeur qui n'est pas celle d'origine. Une clé de plus dans `save`
        // doit être changée dans `tout_change`, sinon son aller-retour ne
        // prouve rien.
        let d_origine = sauver(&origine);
        for (cle, valeur) in &range.0 {
            assert_ne!(d_origine.0.get(cle), Some(valeur), "« {cle} » garde sa valeur d'origine dans ce test");
        }
        assert_eq!(range.0.len(), 45, "une clé de plus ou de moins : mettre ce test à jour avec elle");
    }

    #[test]
    fn une_porte_inerte_est_desactivee_a_la_lecture() {
        let mut rangement = EnMemoire::default();
        rangement.0.insert("gate_threshold".into(), "0.02".into());
        assert_eq!(lire(&rangement).gate_threshold, 0.02);
        // Une profondeur de 0 dB : la porte ne coupait rien, son seuil ne
        // veut rien dire.
        rangement.0.insert("studio".into(), "porte=0,5,100,200".into());
        assert_eq!(lire(&rangement).gate_threshold, 0.0);
    }

    #[test]
    fn le_debit_auto_baisse_sur_les_pertes_et_remonte_au_calme() {
        let mut reglages = lire(&EnMemoire::default());
        assert_eq!(reglages.bitrate, 0, "le débit est sur Auto d'origine");
        // Sans la protection automatique, qui a son propre test.
        reglages.dred_mode = 0;
        let mut vocal = Vocal::new();
        assert_eq!(vocal.debit_effectif(&reglages), 64_000);
        // 5 % de pertes ou moins : rien ne bouge.
        assert!(!vocal.sur_rapport_reseau(&reglages, 5.0));
        // Au-dessus, un cran par rapport, jusqu'au plancher.
        for attendu in [48_000, 32_000, 24_000] {
            assert!(vocal.sur_rapport_reseau(&reglages, 8.0));
            assert_eq!(vocal.debit_effectif(&reglages), attendu);
        }
        assert!(!vocal.sur_rapport_reseau(&reglages, 8.0), "au plancher, plus rien à baisser");
        assert_eq!(vocal.upstream_loss, Some(8.0));
        // Trois rapports propres de suite pour remonter d'un cran ; un
        // rapport moyen (entre 1 et 5 %) remet le compte à zéro.
        assert!(!vocal.sur_rapport_reseau(&reglages, 0.0));
        assert!(!vocal.sur_rapport_reseau(&reglages, 0.0));
        assert!(!vocal.sur_rapport_reseau(&reglages, 3.0));
        assert!(!vocal.sur_rapport_reseau(&reglages, 0.0));
        assert!(!vocal.sur_rapport_reseau(&reglages, 0.0));
        assert!(vocal.sur_rapport_reseau(&reglages, 0.0));
        assert_eq!(vocal.debit_effectif(&reglages), 32_000);
        // Et jamais au-dessus de 64 kbit/s.
        for _ in 0..30 {
            vocal.sur_rapport_reseau(&reglages, 0.0);
        }
        assert_eq!(vocal.debit_effectif(&reglages), 64_000);
    }

    #[test]
    fn un_debit_choisi_a_la_main_n_est_pas_pilote() {
        let mut reglages = lire(&EnMemoire::default());
        reglages.bitrate = 96_000;
        reglages.dred_mode = 0;
        let mut vocal = Vocal::new();
        for _ in 0..5 {
            assert!(!vocal.sur_rapport_reseau(&reglages, 20.0));
        }
        assert_eq!(vocal.debit_effectif(&reglages), 96_000);
        assert_eq!(vocal.auto_bitrate, 64_000);
        // Les pertes restent affichées.
        assert_eq!(vocal.upstream_loss, Some(20.0));
    }

    #[test]
    fn la_protection_auto_s_engage_sur_les_pertes_et_se_relache_au_calme() {
        let reglages = lire(&EnMemoire::default());
        assert_eq!(reglages.dred_mode, 1, "la protection est sur Auto d'origine");
        let mut vocal = Vocal::new();
        assert_eq!(vocal.dred_effectif(&reglages), 0);
        // Dès 2 % de pertes (le débit, lui, ne bouge pas avant 5 %).
        assert!(vocal.sur_rapport_reseau(&reglages, 3.0));
        assert_eq!(vocal.dred_effectif(&reglages), ki_voice::DRED_DEFAULT);
        assert_eq!(vocal.debit_effectif(&reglages), 64_000);
        // Elle tient pendant deux rapports propres, et se relâche au troisième.
        assert!(!vocal.sur_rapport_reseau(&reglages, 0.2));
        assert!(!vocal.sur_rapport_reseau(&reglages, 0.2));
        assert!(vocal.sur_rapport_reseau(&reglages, 0.2));
        assert_eq!(vocal.dred_effectif(&reglages), 0);

        // Coupée ou toujours là : les pertes n'y changent rien.
        let mut coupee = lire(&EnMemoire::default());
        coupee.dred_mode = 0;
        let mut toujours = lire(&EnMemoire::default());
        toujours.dred_mode = 2;
        let vocal = Vocal::new();
        assert_eq!(vocal.dred_effectif(&coupee), 0);
        assert_eq!(vocal.dred_effectif(&toujours), ki_voice::DRED_DEFAULT);
    }

    /// Le défaut corrigé : les rapports propres n'étaient comptés qu'en
    /// débit automatique, et la protection, une fois engagée avec un débit
    /// choisi à la main, ne se relâchait plus de la session.
    #[test]
    fn la_protection_se_relache_aussi_avec_un_debit_choisi_a_la_main() {
        let mut reglages = lire(&EnMemoire::default());
        reglages.bitrate = 96_000;
        assert_eq!(reglages.dred_mode, 1);
        let mut vocal = Vocal::new();
        assert!(vocal.sur_rapport_reseau(&reglages, 3.0));
        assert_eq!(vocal.dred_effectif(&reglages), ki_voice::DRED_DEFAULT);
        // Un rapport moyen entre deux propres : le compte repart de zéro.
        assert!(!vocal.sur_rapport_reseau(&reglages, 0.2));
        assert!(!vocal.sur_rapport_reseau(&reglages, 1.5));
        assert!(!vocal.sur_rapport_reseau(&reglages, 0.2));
        assert!(!vocal.sur_rapport_reseau(&reglages, 0.2));
        assert_eq!(vocal.dred_effectif(&reglages), ki_voice::DRED_DEFAULT);
        assert!(vocal.sur_rapport_reseau(&reglages, 0.2));
        assert_eq!(vocal.dred_effectif(&reglages), 0);
        // Le débit, lui, n'a pas bougé.
        assert_eq!(vocal.debit_effectif(&reglages), 96_000);
        assert_eq!(vocal.auto_bitrate, 64_000);
    }

    /// Au plancher du débit, un rapport de pertes n'est pas un rapport
    /// propre : il faut trois vrais rapports propres pour remonter.
    #[test]
    fn au_plancher_les_pertes_remettent_le_compte_a_zero() {
        let mut reglages = lire(&EnMemoire::default());
        reglages.dred_mode = 0;
        let mut vocal = Vocal::new();
        vocal.auto_bitrate = 24_000;
        assert!(!vocal.sur_rapport_reseau(&reglages, 0.0));
        assert!(!vocal.sur_rapport_reseau(&reglages, 0.0));
        assert!(!vocal.sur_rapport_reseau(&reglages, 9.0));
        assert!(!vocal.sur_rapport_reseau(&reglages, 0.0), "un seul rapport propre après des pertes ne suffit pas");
        assert_eq!(vocal.debit_effectif(&reglages), 24_000);
    }

    #[test]
    fn le_seuil_vocal_ne_joue_qu_en_activation_vocale() {
        let mut reglages = lire(&EnMemoire::default());
        reglages.vad_threshold = 0.04;
        reglages.mode = MicMode::Ptt;
        assert_eq!(reglages.vad_effectif(), 0.0);
        reglages.mode = MicMode::Open;
        assert_eq!(reglages.vad_effectif(), 0.0);
        reglages.mode = MicMode::Vad;
        assert_eq!(reglages.vad_effectif(), 0.04);
    }

    #[test]
    fn le_mode_du_micro_se_relit_par_son_identifiant() {
        for mode in [MicMode::Open, MicMode::Ptt, MicMode::Vad] {
            assert_eq!(MicMode::depuis(mode.id()), mode);
        }
        assert_eq!(MicMode::depuis("n'importe quoi"), MicMode::Ptt);
    }
}
