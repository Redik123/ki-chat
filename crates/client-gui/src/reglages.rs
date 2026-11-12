//! Les réglages d'usage de ki-chat, hors audio : ce que l'on entend de
//! l'interface, de quoi elle prévient, ce qu'elle montre de soi aux autres.
//!
//! Lus et écrits au même endroit, comme les réglages audio (`audio.rs`) : une
//! clé relue qui ne serait pas sauvée se voit ici, côte à côte.

use std::collections::HashSet;

use crate::Notif;

fn on_off(vrai: bool) -> String {
    if vrai { "on" } else { "off" }.into()
}

/// Une liste rangée sous une seule clé, séparée par des virgules.
fn liste(valeur: &str) -> impl Iterator<Item = String> + '_ {
    valeur.split(',').filter(|e| !e.is_empty()).map(str::to_string)
}

/// Les tailles d'interface proposées, en pour cent : de quoi lire sur un
/// portable serré comme sur un grand écran regardé de loin.
pub(crate) const ECHELLES: [u32; 8] = [80, 90, 100, 110, 120, 130, 140, 150];

/// La fenêtre la plus petite où l'interface tient, en points d'interface.
pub(crate) const FENETRE_MIN: [f32; 2] = [900.0, 560.0];

/// L'échelle à appliquer : celle qu'on veut, ramenée dans les bornes, et pas
/// plus grande que ce que l'écran permet — agrandie, l'interface demande une
/// fenêtre plus grande, et la plus petite fenêtre doit encore tenir à
/// l'écran. `ecran` : sa taille en points d'interface à l'échelle `actuelle`.
pub(crate) fn echelle_bornee(voulue: u32, actuelle: f32, ecran: Option<[f32; 2]>) -> u32 {
    let (min, max) = (ECHELLES[0], ECHELLES[ECHELLES.len() - 1]);
    let voulue = voulue.clamp(min, max);
    let Some([largeur, hauteur]) = ecran.filter(|e| e[0] > 0.0 && e[1] > 0.0 && actuelle > 0.0) else { return voulue };
    // L'écran, en points d'interface à 100 % ; un peu de marge pour la barre
    // des tâches et les bords de la fenêtre.
    let tient = (largeur * actuelle / FENETRE_MIN[0]).min(hauteur * actuelle * 0.92 / FENETRE_MIN[1]);
    let plafond = ((tient * 10.0).floor() as u32 * 10).max(min);
    voulue.min(plafond)
}

/// L'échelle d'un cran plus grande ou plus petite, sans sortir des bornes.
pub(crate) fn echelle_voisine(actuelle: u32, plus: bool) -> u32 {
    let rang = ECHELLES.iter().position(|e| *e >= actuelle).unwrap_or(ECHELLES.len() - 1);
    let rang = if plus { (rang + 1).min(ECHELLES.len() - 1) } else { rang.saturating_sub(1) };
    ECHELLES[rang]
}

/// Les réglages d'usage, hors audio : sons de l'interface, notifications,
/// zone de notification, pokes, présence et VALORANT.
pub(crate) struct Reglages {
    /// La taille de l'interface, en pour cent (voir [`ECHELLES`]).
    pub(crate) echelle: u32,
    /// Événements coupés individuellement dans les réglages.
    pub(crate) sfx_muted: HashSet<String>,
    pub(crate) sfx_on: bool,
    pub(crate) sfx_volume: f32,
    /// De quoi prévenir : tout, les mentions, rien.
    pub(crate) notif: Notif,
    /// Est-ce que j'accepte les pokes ? Réglage ⚙, persisté.
    pub(crate) pokes_accepter: bool,
    /// Réglage ⚙ : la croix réduit dans la zone au lieu de quitter.
    pub(crate) reduire_zone: bool,
    /// L'avertissement « ki-chat continue à côté de l'horloge » a été lu
    /// une fois ; persisté.
    pub(crate) zone_avertie: bool,
    /// Dire aux membres à quoi je joue — le jeu reconnu à sa fenêtre,
    /// sans rien lire dedans. Coché de base : c'est un nom, pas une partie.
    pub(crate) presence_jeux: bool,
    /// Partager son activité VALORANT (lue dans son propre client Riot).
    /// Désactivé de base : personne ne partage sans l'avoir choisi.
    pub(crate) valorant_presence: bool,
    /// Ajouter ses médailles à sa fiche : le serveur les lit chez HenrikDev
    /// (0.1.63), et son client Riot les donne dès la fin de chaque partie —
    /// sous « Partager mon activité », qui dit quand une partie finit.
    /// Décoché, le serveur l'apprend et les efface. Coché de base : lier
    /// son compte, c'est déjà montrer ses statistiques au groupe.
    pub(crate) valorant_medailles: bool,
    /// Les comptes non liés dont on ne veut plus entendre parler : des
    /// empreintes de puuid (`membres::empreinte_compte`), gardées sur cette
    /// machine seulement.
    pub(crate) comptes_tus: Vec<String>,
    /// « Garder l'historique de mes parties » : décoché d'origine.
    pub(crate) journal_parties: bool,
    /// La bannière qui le propose a déjà eu sa réponse.
    pub(crate) journal_propose: bool,
}

impl Reglages {
    /// Relit les réglages. `get(clé, défaut)` rend ce qui est rangé sous la
    /// clé, ou le défaut : ce qui manque ou ne se lit pas garde sa valeur
    /// d'origine.
    pub(crate) fn load(get: impl Fn(&str, &str) -> String) -> Self {
        Self {
            echelle: get("echelle", "100").parse().unwrap_or(100).clamp(ECHELLES[0], ECHELLES[ECHELLES.len() - 1]),
            sfx_muted: liste(&get("sfx_muted", "")).collect(),
            sfx_on: get("sfx_on", "on") == "on",
            sfx_volume: get("sfx_volume", "0.6").parse().unwrap_or(0.6),
            notif: Notif::depuis(&get("notif", "tout")),
            pokes_accepter: get("pokes", "on") == "on",
            reduire_zone: get("reduire_zone", "on") == "on",
            zone_avertie: get("zone_avertie", "off") == "on",
            presence_jeux: get("presence_jeux", "on") == "on",
            valorant_presence: get("valorant_presence", "off") == "on",
            valorant_medailles: get("valorant_medailles", "on") == "on",
            comptes_tus: liste(&get("comptes_tus", "")).collect(),
            journal_parties: get("journal_parties", "off") == "on",
            // « oui » / « non », et non « on » / « off » comme les autres :
            // c'est ce qui est rangé chez les joueurs depuis la 0.1.60.
            journal_propose: get("journal_propose", "non") == "oui",
        }
    }

    /// Écrit les réglages, chacun sous la clé d'où [`Reglages::load`] le
    /// relit.
    pub(crate) fn save(&self, storage: &mut dyn eframe::Storage) {
        // Triés : l'ordre d'un ensemble change d'un lancement à l'autre, et
        // le fichier des réglages changerait sans que rien n'ait bougé.
        let mut coupes: Vec<&str> = self.sfx_muted.iter().map(String::as_str).collect();
        coupes.sort_unstable();
        storage.set_string("sfx_muted", coupes.join(","));
        storage.set_string("echelle", self.echelle.to_string());
        storage.set_string("sfx_on", on_off(self.sfx_on));
        storage.set_string("sfx_volume", format!("{}", self.sfx_volume));
        storage.set_string("notif", self.notif.cle().into());
        storage.set_string("pokes", on_off(self.pokes_accepter));
        storage.set_string("reduire_zone", on_off(self.reduire_zone));
        storage.set_string("zone_avertie", on_off(self.zone_avertie));
        storage.set_string("presence_jeux", on_off(self.presence_jeux));
        storage.set_string("valorant_presence", on_off(self.valorant_presence));
        storage.set_string("valorant_medailles", on_off(self.valorant_medailles));
        storage.set_string("comptes_tus", self.comptes_tus.join(","));
        storage.set_string("journal_parties", on_off(self.journal_parties));
        storage.set_string("journal_propose", if self.journal_propose { "oui" } else { "non" }.into());
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

    fn lire(rangement: &EnMemoire) -> Reglages {
        Reglages::load(|cle, defaut| rangement.0.get(cle).cloned().unwrap_or_else(|| defaut.to_string()))
    }

    fn sauver(reglages: &Reglages) -> EnMemoire {
        let mut rangement = EnMemoire::default();
        reglages.save(&mut rangement);
        rangement
    }

    /// Les valeurs d'origine, telles que les joueurs les ont : une
    /// installation neuve fait du bruit, prévient de tout, réduit à côté de
    /// l'horloge, dit à quoi l'on joue — et ne partage rien de VALORANT
    /// qu'on n'ait choisi.
    #[test]
    fn une_installation_neuve_a_ses_valeurs_d_origine() {
        let r = lire(&EnMemoire::default());
        assert!(r.sfx_on && r.sfx_muted.is_empty());
        assert_eq!(r.echelle, 100);
        assert_eq!(r.sfx_volume, 0.6);
        assert_eq!(r.notif, Notif::Tout);
        assert!(r.pokes_accepter && r.reduire_zone && !r.zone_avertie);
        assert!(r.presence_jeux && !r.valorant_presence && r.valorant_medailles);
        assert!(r.comptes_tus.is_empty() && !r.journal_parties && !r.journal_propose);
    }

    /// Ce qui se sauve se relit tel quel : quatorze clés, et aucune qui reste
    /// à sa valeur d'origine sans que le test le voie.
    #[test]
    fn ce_qui_se_sauve_se_relit_tel_quel() {
        let origine = sauver(&lire(&EnMemoire::default()));
        assert_eq!(origine.0.len(), 14, "une clé de plus ou de moins : mettre ce test à jour");

        let mut r = lire(&EnMemoire::default());
        r.sfx_muted = HashSet::from(["poke".to_string(), "join".to_string()]);
        r.sfx_on = false;
        r.echelle = 130;
        r.sfx_volume = 0.25;
        r.notif = Notif::Mentions;
        r.pokes_accepter = false;
        r.reduire_zone = false;
        r.zone_avertie = true;
        r.presence_jeux = false;
        r.valorant_presence = true;
        r.valorant_medailles = false;
        r.comptes_tus = vec!["a1b2".into(), "c3d4".into()];
        r.journal_parties = true;
        r.journal_propose = true;
        let range = sauver(&r);
        for (cle, valeur) in &range.0 {
            assert_ne!(Some(valeur), origine.0.get(cle), "« {cle} » n'a pas été changée par ce test");
        }
        assert_eq!(range.0["sfx_muted"], "join,poke", "trié : le fichier ne bouge pas sans raison");
        assert_eq!(range.0["journal_propose"], "oui");

        // L'aller-retour : relu puis resauvé, rien n'a bougé.
        let relu = lire(&range);
        assert_eq!(sauver(&relu).0, range.0);
        assert_eq!(relu.sfx_muted, r.sfx_muted);
        assert_eq!(relu.comptes_tus, r.comptes_tus);
        assert_eq!((relu.notif, relu.sfx_volume), (Notif::Mentions, 0.25));
    }

    /// L'échelle ne dépasse pas ce que l'écran permet : agrandie, la plus
    /// petite fenêtre doit encore y tenir.
    #[test]
    fn l_echelle_ne_depasse_pas_l_ecran() {
        // Sans écran connu : seulement les bornes.
        assert_eq!(echelle_bornee(150, 1.0, None), 150);
        assert_eq!(echelle_bornee(40, 1.0, None), 80);
        // Un grand écran (1920×1080 points à 100 %) : tout passe.
        assert_eq!(echelle_bornee(150, 1.0, Some([1920.0, 1080.0])), 150);
        // Un portable de 1366×768 points : 900×560 agrandi de moitié n'y
        // tient pas en hauteur (768 × 0,92 / 560 = 1,26) — 120 % au plus.
        assert_eq!(echelle_bornee(150, 1.0, Some([1366.0, 768.0])), 120);
        assert_eq!(echelle_bornee(110, 1.0, Some([1366.0, 768.0])), 110);
        // Le même écran vu depuis une interface déjà à 120 % : il paraît
        // plus petit en points, le plafond reste le même.
        assert_eq!(echelle_bornee(150, 1.2, Some([1366.0 / 1.2, 768.0 / 1.2])), 120);
        // Un écran minuscule ne fait pas descendre sous la plus petite.
        assert_eq!(echelle_bornee(100, 1.0, Some([640.0, 480.0])), 80);
    }

    #[test]
    fn l_echelle_avance_d_un_cran() {
        assert_eq!(echelle_voisine(100, true), 110);
        assert_eq!(echelle_voisine(100, false), 90);
        assert_eq!(echelle_voisine(150, true), 150);
        assert_eq!(echelle_voisine(80, false), 80);
        // Une valeur entre deux crans (réglages retouchés à la main).
        assert_eq!(echelle_voisine(105, true), 120);
        assert_eq!(echelle_voisine(105, false), 100);
    }

    /// Une valeur abîmée ne casse rien : elle retombe sur l'origine.
    #[test]
    fn une_valeur_abimee_retombe_sur_l_origine() {
        let mut rangement = EnMemoire::default();
        rangement.0.insert("sfx_volume".into(), "fort".into());
        rangement.0.insert("notif".into(), "parfois".into());
        rangement.0.insert("sfx_muted".into(), ",,poke,".into());
        rangement.0.insert("echelle".into(), "900".into());
        let r = lire(&rangement);
        assert_eq!(r.echelle, 150, "hors bornes : ramenée à la plus grande");
        assert_eq!(r.sfx_volume, 0.6);
        assert_eq!(r.notif, Notif::Tout);
        assert_eq!(r.sfx_muted, HashSet::from(["poke".to_string()]));
    }
}
