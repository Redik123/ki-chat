//! Les diagnostics partagés (opt-in) : le journal technique, le rapport du docteur
//! audio et les rapports de plantage envoyés au serveur du groupe.

use crate::{secours, KiApp};

/// Les diagnostics partagés : ce qui est déjà parti vers le serveur, et quand.
pub(crate) struct Diag {
    /// Diagnostic partagé : le journal technique part vers le serveur
    /// (opt-in). Voir `flush_diag` pour ce qui transite — et ne transite pas.
    pub(crate) partage: bool,
    /// Horodatage (epoch ms) de la dernière ligne de journal déjà envoyée.
    pub(crate) dernier_ts: u64,
    /// Dernier envoi périodique, pour la cadence de dix minutes.
    pub(crate) dernier_envoi: Option<std::time::Instant>,
    /// Le rapport du docteur de la session est parti. Il attend la première
    /// ouverture du micro : calculé avant, il décrivait un micro que personne
    /// n'avait encore ouvert.
    pub(crate) docteur_envoye: bool,
    /// Horodatage de modification du rapport de plantage déjà transmis
    /// (préférence persistée) : un même crash ne repart pas à chaque session.
    pub(crate) crash_envoye: String,
    /// Le rapport de plantage de la session précédente a été vérifié (et
    /// envoyé s'il y en avait un) pour cette session — une fois suffit.
    pub(crate) crash_verifie: bool,
}

impl KiApp {
    /// Envoie au serveur les diagnostics accumulés depuis le dernier envoi :
    /// méta (version, système), nouvelles lignes du journal audio, et — une
    /// fois par session, dès la première ouverture du micro, ou sur demande —
    /// le rapport du docteur.
    ///
    /// C'est TOUT ce qui transite, et rien d'autre : pas un message, pas une
    /// trame audio. Le lot part en JSONL sur le canal HTTPS épinglé du
    /// serveur (même authentification que le partage de fichiers), dans un
    /// thread — l'interface ne l'attend jamais.
    pub(crate) fn flush_diag(&mut self, manual: bool) {
        if self.conn.is_none() {
            return;
        }
        let journal = ki_voice::journal_snapshot();
        let fresh: Vec<&(u64, String)> =
            journal.iter().filter(|(t, _)| *t > self.diag.dernier_ts).collect();
        // Le docteur énumère les processus : pas de quoi le faire chaque
        // minute. Une fois par session, et pas avant la première ouverture du
        // micro — calculé au démarrage du moteur, il annonçait un « moteur de
        // secours » et zéro famine dans la moitié des rapports reçus. À la
        // demande, il part tel quel : le rapport dit lui-même s'il est tôt.
        let docteur = if manual || !self.diag.docteur_envoye {
            self.link
                .engine
                .lock()
                .unwrap()
                .as_ref()
                .filter(|e| manual || e.docteur_pret())
                .map(|e| e.docteur().rapport())
        } else {
            None
        };
        if fresh.is_empty() && docteur.is_none() && !manual {
            return;
        }
        let now_ms = chrono::Local::now().timestamp_millis().max(0) as u64;
        let mut lot = String::new();
        lot.push_str(&format!(
            "{{\"type\":\"meta\",\"t\":{now_ms},\"version\":\"{}\",\"os\":\"{} {}\",\"gpu\":{}}}\n",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            serde_json::Value::String(ki_video::inventaire_pret().unwrap_or("").to_string()),
        ));
        for (t, msg) in &fresh {
            lot.push_str(&format!(
                "{{\"type\":\"journal\",\"t\":{t},\"msg\":{}}}\n",
                serde_json::Value::String((*msg).clone())
            ));
        }
        if let Some(rapport) = docteur {
            lot.push_str(&format!(
                "{{\"type\":\"docteur\",\"t\":{now_ms},\"rapport\":{}}}\n",
                serde_json::Value::String(rapport)
            ));
            self.diag.docteur_envoye = true;
        }
        // Le plantage de la session précédente, si le dispositif de secours
        // en a consigné un : il part avec le premier lot de la session, et
        // une seule fois — l'horodatage du fichier déjà envoyé reste dans
        // les préférences. Comme tout le reste du lot, c'est du technique
        // (panic, pile, erreur de rendu) : jamais un message, jamais de
        // l'audio.
        if self.diag.dernier_ts == 0 {
            if let Some((tampon, rapport)) =
                secours::rapport_a_envoyer(&self.diag.crash_envoye, &self.url)
            {
                lot.push_str(&format!(
                    "{{\"type\":\"crash\",\"t\":{now_ms},\"rapport\":{}}}\n",
                    serde_json::Value::String(rapport)
                ));
                self.diag.crash_envoye = tampon;
            }
        }
        if let Some((t, _)) = journal.last() {
            self.diag.dernier_ts = *t;
        }
        let base = self.http_base();
        let agent = self.http_agent();
        let token_hex = format!("{:x}", self.voice_token);
        std::thread::spawn(move || {
            if let Err(e) = agent
                .post(&format!("{base}/diag"))
                .set("x-ki-token", &token_hex)
                .set("x-ki-version", env!("CARGO_PKG_VERSION"))
                .send_string(&lot)
            {
                // Raté silencieux : le diagnostic est un luxe, pas une
                // fonction — la prochaine minute réessaiera avec le cumul.
                tracing::debug!("envoi du diagnostic raté : {e}");
            }
        });
    }

    /// Le rapport de plantage de la session précédente part au serveur
    /// **quoi qu'il en soit** — option de partage cochée ou non. Décision de
    /// l'admin du groupe : un plantage se corrige avec sa pile d'appels, pas
    /// avec un « ça a planté » de mémoire ; et le rapport ne contient que du
    /// technique (panic, pile, erreur de rendu), jamais un message, jamais
    /// de l'audio. Une fois par session, et un même plantage ne voyage
    /// qu'une fois (horodatage mémorisé).
    pub(crate) fn envoyer_crash(&mut self) {
        let Some((tampon, rapport)) =
            secours::rapport_a_envoyer(&self.diag.crash_envoye, &self.url)
        else {
            return;
        };
        self.diag.crash_envoye = tampon;
        let now_ms = chrono::Local::now().timestamp_millis().max(0) as u64;
        let lot = format!(
            "{{\"type\":\"meta\",\"t\":{now_ms},\"version\":\"{}\",\"os\":\"{} {}\",\"gpu\":{}}}\n\
             {{\"type\":\"crash\",\"t\":{now_ms},\"rapport\":{}}}\n",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            serde_json::Value::String(ki_video::inventaire_pret().unwrap_or("").to_string()),
            serde_json::Value::String(rapport),
        );
        ki_voice::journal("rapport de plantage de la session précédente envoyé au serveur".into());
        let base = self.http_base();
        let agent = self.http_agent();
        let token_hex = format!("{:x}", self.voice_token);
        std::thread::spawn(move || {
            if let Err(e) = agent
                .post(&format!("{base}/diag"))
                .set("x-ki-token", &token_hex)
                .set("x-ki-version", env!("CARGO_PKG_VERSION"))
                .send_string(&lot)
            {
                tracing::warn!("envoi du rapport de plantage raté : {e}");
            }
        });
    }

    /// La cadence du diagnostic partagé : dix minutes, et seulement s'il y a
    /// du neuf. Appelée à chaque image ; le garde-fou coûte deux lectures.
    pub(crate) fn maybe_flush_diag(&mut self) {
        if self.conn.is_none() || !self.welcomed {
            return;
        }
        if !self.diag.crash_verifie {
            self.diag.crash_verifie = true;
            self.envoyer_crash();
        }
        if !self.diag.partage {
            return;
        }
        let due = self.diag
            .dernier_envoi
            .map(|t| t.elapsed() > std::time::Duration::from_secs(600))
            .unwrap_or(true);
        // Le rapport du docteur part dès que le micro s'est ouvert, sans
        // attendre la cadence : c'est le premier état fiable de la session.
        let docteur_du = !self.diag.docteur_envoye
            && self
                .link
                .engine
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|e| e.docteur_pret());
        if !due && !docteur_du {
            return;
        }
        if due {
            self.diag.dernier_envoi = Some(std::time::Instant::now());
        }
        self.flush_diag(false);
    }
}
