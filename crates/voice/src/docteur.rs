//! Docteur audio : pourquoi le micro bugue au lancement d'un jeu.
//!
//! # Ce que ce module peut, et ce qu'il ne peut pas
//!
//! Windows n'offre **aucune API de « priorité micro »**. Quand un autre
//! logiciel prend la voie de capture — la voix intégrée d'un jeu, la chaîne
//! d'effets d'un casque, un pilote virtuel — on ne peut pas la lui reprendre.
//! Les deux premiers paliers du chantier audio ont donc visé la seule chose
//! atteignable : *récupérer vite et bien*, comme le fait Discord. Noms de
//! périphériques tolérants à la ré-énumération USB, réouverture sur les zéros
//! stricts, moteur WASAPI natif, escalade en catégorie communications.
//!
//! Il reste les cas où l'on ne récupère pas, parce que la cause est
//! **ailleurs que dans notre processus**. Ce module ne les corrige pas : il
//! les **nomme**. C'est la différence entre « ça bugue » et « Sonar
//! s'interpose, voici le réglage » — la première phrase engendre un message à
//! l'admin, la seconde une action.
//!
//! # La règle qui gouverne tout ce fichier
//!
//! **On conseille, on n'agit jamais.** Pas d'écriture dans le registre, pas de
//! modification de réglage système, pas d'arrêt de processus. Décocher « mode
//! exclusif » à la place de quelqu'un demanderait des droits d'administrateur,
//! toucherait à une configuration qui ne nous appartient pas, et casserait
//! silencieusement les logiciels qui en dépendent — une station audionumérique,
//! un pilote ASIO. Le diagnostic se lit, se copie, et c'est l'utilisateur qui
//! décide.

/// Un logiciel connu pour s'interposer sur le chemin audio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suite {
    /// Nom lisible, tel qu'on le dit à l'utilisateur.
    pub nom: &'static str,
    /// Le processus qui l'a trahi.
    pub processus: String,
    /// Ce qu'il faut en faire. Une phrase, actionnable.
    pub conseil: &'static str,
}

/// Ce que le docteur a trouvé.
#[derive(Clone, Debug, Default)]
pub struct Diagnostic {
    /// Suites détectées en cours d'exécution.
    pub suites: Vec<Suite>,
    /// Mode exclusif explicitement **refusé** sur le micro ?
    ///
    /// `Some(false)` = quelqu'un a décoché la case. `Some(true)` = elle est
    /// cochée explicitement. `None` = le réglage n'a jamais été touché, et
    /// Windows autorise alors le mode exclusif par défaut.
    ///
    /// À lire avec prudence : sur les machines observées, la valeur n'existe
    /// tout simplement pas tant que personne n'a ouvert le panneau — `None`
    /// est donc le cas courant, pas une anomalie. C'est pourquoi le conseil
    /// correspondant est déclenché par une **preuve mesurée** (la famine du
    /// micro) et non par ce drapeau : un réglage mal lu enverrait quelqu'un
    /// fouiller un panneau pour rien.
    pub exclusif_micro: Option<bool>,
    /// Idem pour la sortie.
    pub exclusif_sortie: Option<bool>,
    /// Périphériques réellement en service, s'ils sont connus. Sert à
    /// reconnaître un **pilote virtuel**, que l'énumération des processus ne
    /// peut pas voir.
    pub peripherique_micro: Option<String>,
    pub peripherique_sortie: Option<String>,
    /// Ouvertures du micro sans un seul bloc reçu, depuis le démarrage du
    /// moteur. C'est **la** signature du micro affamé : le flux s'ouvre sans
    /// erreur, mais un autre logiciel tient la voie et la nôtre n'est jamais
    /// servie.
    pub ouvertures_affamees: u32,
    /// Trames incomplètes parties vers la carte son (voir `VoiceStats`).
    pub trames_incompletes: u64,
    /// Le moteur natif est-il en service, ou est-on retombé sur cpal ?
    pub moteur_natif: bool,
    /// Le micro tourne en catégorie « communications » — la case « partager
    /// le micro avec la voix du jeu », ou l'escalade anti-famine. Aux yeux
    /// de Windows, c'est un appel permanent.
    pub micro_communications: bool,
    /// Le réglage Windows « activité de communication » : 0 = couper les
    /// autres sons, 1 = réduire de 80 %, 2 = réduire de 50 %, 3 = ne rien
    /// faire. `None` = jamais réglé, Windows applique alors 80 %.
    pub attenuation_windows: Option<u32>,
}

impl Diagnostic {
    /// Les conseils qui découlent de l'état constaté, suites comprises.
    ///
    /// Rendus dans l'ordre où ils valent la peine d'être essayés : ce qui
    /// s'interpose d'abord, les réglages système ensuite, les symptômes en
    /// dernier.
    pub fn conseils(&self) -> Vec<String> {
        let mut out: Vec<String> = self.suites
            .iter()
            .map(|s| format!("{} est en cours d'exécution. {}", s.nom, s.conseil))
            .collect();

        // Un périphérique virtuel en service : c'est la cause la plus simple
        // d'un « micro qui ne capte rien » ou d'un « je n'entends personne »,
        // et la plus facile à rater — on parle dans un câble qui ne va nulle
        // part, ou on écoute au bout d'un câble que rien n'alimente.
        //
        // Le sens compte : le même pilote virtuel ne se corrige pas de la même
        // façon selon qu'il est à l'entrée ou à la sortie, et un conseil qui
        // dit « choisis ton micro » à propos des écouteurs ne sert personne.
        for (entree, nom) in [
            (true, self.peripherique_micro.as_deref()),
            (false, self.peripherique_sortie.as_deref()),
        ] {
            let Some(nom) = nom else { continue };
            if let Some(conseil) = virtuel(nom, entree) {
                let quoi = if entree { "Le micro" } else { "La sortie" };
                out.push(format!("{quoi} en service est « {nom} ». {conseil}"));
            }
        }

        // Le micro en catégorie « communications » + le réglage d'atténuation
        // de Windows : c'est LA chaîne qui fait chuter le volume du jeu — vue
        // sur le terrain, typiquement chez qui a un micro séparé du casque
        // qui famine (pilote virtuel sans son application, périphérique
        // coincé) et déclenche l'escalade sans s'en douter. On nomme la
        // chaîne entière : la cause, l'effet, et le remède de chaque bout.
        if self.micro_communications {
            let effet = match self.attenuation_windows {
                Some(3) => None, // « Ne rien faire » : Windows n'y est pour rien.
                Some(0) => Some("couper tous les autres sons"),
                Some(2) => Some("réduire les autres sons de 50 %"),
                // 1 explicite, ou jamais réglé : le défaut de Windows.
                _ => Some("réduire les autres sons de 80 %"),
            };
            match effet {
                Some(effet) => out.push(format!(
                    "Le micro tourne en catégorie « communications » (voie partagée \
                     avec la voix du jeu), et Windows est réglé pour {effet} pendant \
                     une communication : c'est LUI qui baisse le volume du jeu tant \
                     que le vocal est ouvert. Remède immédiat : Panneau de \
                     configuration → Son → onglet Communications → « Ne rien \
                     faire ». Et si cette catégorie s'est enclenchée toute seule \
                     (micro affamé), le vrai correctif est de choisir dans ⚙ Audio \
                     le micro physique — pas un périphérique virtuel dont \
                     l'application ne tourne pas."
                )),
                None => out.push(
                    "Le micro tourne en catégorie « communications », mais Windows \
                     est déjà réglé sur « Ne rien faire » : si le volume du jeu \
                     baisse quand même, le coupable est le mixeur du casque \
                     (ChatMix de Sonar, Wave Link…), qui baisse le jeu dès qu'une \
                     session d'appel existe — voir les logiciels détectés."
                        .into(),
                ),
            }
        }

        // Le conseil sur le mode exclusif est déclenché par la **preuve**, pas
        // par le drapeau : le réglage n'existe dans le registre que si
        // quelqu'un l'a touché, si bien que son absence ne prouve rien. La
        // famine du micro, elle, est mesurée par le moteur lui-même.
        if self.ouvertures_affamees >= 3 && !self.micro_communications {
            out.push(format!(
                "Le micro s'est ouvert {} fois sans livrer un seul bloc. C'est la \
                 signature d'une voie de capture tenue par un autre logiciel — la voix \
                 intégrée d'un jeu, le plus souvent. Dans Valorant : Réglages → Audio → \
                 Chat vocal → couper le micro de la voix intégrée, que tu n'utilises pas \
                 puisque tu es ici.",
                self.ouvertures_affamees
            ));
            let etat = match self.exclusif_micro {
                Some(false) => " (le mode exclusif est déjà refusé sur ce micro : \
                                 cherche plutôt du côté des logiciels ci-dessus)",
                _ => " Si cela persiste : Panneau son Windows → Enregistrement → ton \
                      micro → Propriétés → Avancé → décocher « Autoriser les \
                      applications à prendre le contrôle exclusif ». À ne faire que si \
                      tu n'utilises ni station audionumérique ni pilote ASIO, qui en \
                      dépendent.",
            };
            out.push(etat.into());
        }
        if !self.moteur_natif {
            out.push(
                "Le moteur audio natif n'a pas pu s'ouvrir : on tourne sur le moteur de \
                 secours, qui ne demande à Windows ni le périphérique de communication, \
                 ni la conversion de format automatique, ni le mode brut. Le journal \
                 audio dit pourquoi."
                    .into(),
            );
        }
        if self.trames_incompletes > 0 {
            out.push(format!(
                "{} trames incomplètes sont parties vers la carte son : autant de \
                 craquements. Si cela arrive pendant une partie, c'est que la machine \
                 est saturée ou la carte son USB fragile — coche « Sortie audio \
                 robuste » dans ⚙ Audio → Sortie (plus de marge, un peu plus de \
                 latence), et vérifie que le jeu tourne en fenêtré sans bordure plutôt \
                 qu'en plein écran exclusif.",
                self.trames_incompletes
            ));
        }
        if out.is_empty() {
            out.push(
                "Rien à signaler : aucune suite connue en cours d'exécution, pas de \
                 famine du micro, pas de trame manquée."
                    .into(),
            );
        }
        out
    }

    /// Le rapport complet, tel qu'on se le fait copier-coller.
    pub fn rapport(&self) -> String {
        let mut out = String::from("--- docteur audio ---\n");
        out.push_str(&format!(
            "moteur : {}\n",
            if self.moteur_natif { "natif (WASAPI)" } else { "secours (cpal)" }
        ));
        out.push_str(&format!(
            "périphériques : micro {} · sortie {}\n",
            self.peripherique_micro.as_deref().unwrap_or("inconnu"),
            self.peripherique_sortie.as_deref().unwrap_or("inconnu")
        ));
        out.push_str(&format!(
            "mode exclusif : micro {} · sortie {}\n",
            etat(self.exclusif_micro),
            etat(self.exclusif_sortie)
        ));
        out.push_str(&format!(
            "ouvertures affamées : {} · trames incomplètes : {}\n",
            self.ouvertures_affamees, self.trames_incompletes
        ));
        out.push_str(&format!(
            "catégorie du micro : {} · atténuation Windows : {}\n",
            if self.micro_communications {
                "communications (réglage ou escalade)"
            } else {
                "standard"
            },
            attenuation(self.attenuation_windows)
        ));
        if self.suites.is_empty() {
            out.push_str("logiciels détectés : aucun\n");
        } else {
            out.push_str("logiciels détectés :\n");
            for s in &self.suites {
                out.push_str(&format!("  - {} ({})\n", s.nom, s.processus));
            }
        }
        out.push_str("\nconseils :\n");
        for (i, c) in self.conseils().iter().enumerate() {
            out.push_str(&format!("{}. {c}\n", i + 1));
        }
        out
    }
}

fn etat(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "autorisé (explicitement)",
        Some(false) => "refusé",
        // Windows autorise par défaut, et n'écrit la valeur que si on la
        // change : « jamais réglé » est donc le cas courant, et il veut dire
        // « autorisé ».
        None => "jamais réglé (donc autorisé)",
    }
}

/// Le réglage « activité de communication », en clair. Même logique que le
/// mode exclusif : la valeur n'existe que si quelqu'un a touché le panneau,
/// et son absence signifie le défaut de Windows — réduire de 80 %.
fn attenuation(v: Option<u32>) -> &'static str {
    match v {
        Some(3) => "ne rien faire",
        Some(2) => "réduire de 50 %",
        Some(0) => "couper les autres sons",
        Some(_) => "réduire de 80 %",
        None => "jamais réglée (donc réduire de 80 %)",
    }
}

/// Un périphérique virtuel connu, et ce qu'il faut en penser.
///
/// Ceux-là sont des **pilotes**, pas des processus : l'énumération de la table
/// des processus ne peut pas les voir, alors qu'ils sont en service sous nos
/// yeux. On les reconnaît donc à leur nom.
///
/// `entree` distingue le micro de la sortie : le même pilote ne se corrige pas
/// de la même façon des deux côtés, et un conseil qui parle du micro à propos
/// des écouteurs ne sert personne.
fn virtuel(nom: &str, entree: bool) -> Option<&'static str> {
    let n = nom.to_ascii_lowercase();
    if n.contains("vb-audio") || n.contains("cable output") || n.contains("cable input") {
        return Some(if entree {
            "C'est un câble audio virtuel, pas un micro : si tu ne l'as pas \
             installé exprès, tu parles dans le vide. Choisis ton micro physique \
             dans ⚙ Audio."
        } else {
            "C'est un câble audio virtuel, pas un casque : si tu ne l'as pas \
             installé exprès, tu n'entendras personne. Choisis ta sortie physique \
             dans ⚙ Audio."
        });
    }
    if n.contains("voicemeeter") {
        return Some(if entree {
            "C'est une entrée de table de mixage virtuelle. Voulu si tu as \
             installé Voicemeeter exprès ; sinon, vise ton micro physique dans \
             ⚙ Audio."
        } else {
            "C'est une sortie de table de mixage virtuelle. Voulu si tu as \
             installé Voicemeeter exprès ; sinon, vise ton casque dans ⚙ Audio."
        });
    }
    if n.contains("nvidia broadcast") {
        return Some(if entree {
            "C'est le micro virtuel de NVIDIA Broadcast, qui applique son propre \
             débruitage. Deux débruiteurs en série se battent : mets notre \
             suppression de bruit sur « désactivée », ou choisis le micro physique \
             et laisse DeepFilterNet faire."
        } else {
            "C'est la sortie virtuelle de NVIDIA Broadcast. Elle ajoute sa propre \
             latence au chemin d'écoute : pour du vocal, la sortie physique vaut \
             mieux."
        });
    }
    if n.contains("sonar") || n.contains("steelseries") {
        return Some(if entree {
            "C'est un micro virtuel de SteelSeries Sonar, la cause la plus \
             fréquente de micro muet après le lancement d'un jeu. Essaie ton micro \
             physique dans ⚙ Audio, ou coche « micro brut »."
        } else {
            "C'est une sortie virtuelle de SteelSeries Sonar. Voulu si tu t'en \
             sers pour mixer le jeu et le vocal ; sinon, vise ton casque dans \
             ⚙ Audio."
        });
    }
    None
}

/// Les logiciels qu'on sait reconnaître, et ce qu'il faut en faire.
///
/// Le nom de processus est comparé **sans casse et sans l'extension**, parce
/// que Windows n'est pas regardant et que les versions renomment.
///
/// Cette liste est volontairement courte : elle ne contient que ce qui est
/// remonté du terrain ou documenté comme s'interposant sur le chemin audio.
/// Nommer un logiciel innocent ferait perdre du temps à quelqu'un, ce qui est
/// exactement le contraire du but.
const CONNUS: &[(&str, &str, &str)] = &[
    (
        "sonar",
        "SteelSeries Sonar",
        "Il crée des périphériques virtuels et redirige tout le son. C'est la \
         cause la plus fréquente de micro muet après le lancement d'un jeu. \
         Essaie de choisir le micro **physique** dans ⚙ Audio plutôt qu'un \
         périphérique « Sonar », ou coche « micro brut » pour court-circuiter \
         sa chaîne d'effets.",
    ),
    (
        "steelseriesgg",
        "SteelSeries GG",
        "C'est lui qui héberge Sonar. Même remède : viser le micro physique, \
         ou activer « micro brut ».",
    ),
    (
        "nahimic",
        "Nahimic",
        "Chaîne d'effets audio livrée avec beaucoup de cartes mères et de \
         portables MSI. Elle s'insère avant nous et produit des micros \
         zombies. Coche « micro brut » dans ⚙ Audio, ou désactive son service \
         audio.",
    ),
    (
        "nahimicsvc32",
        "Nahimic (service)",
        "Le service de la chaîne d'effets Nahimic. Voir Nahimic.",
    ),
    (
        "nahimicsvc64",
        "Nahimic (service)",
        "Le service de la chaîne d'effets Nahimic. Voir Nahimic.",
    ),
    (
        "razer synapse",
        "Razer Synapse",
        "Ses effets micro (THX Spatial, réduction de bruit) s'ajoutent aux \
         nôtres et se disputent la voie de capture. Coche « micro brut », ou \
         désactive ses effets audio dans Synapse.",
    ),
    (
        "razersynapse",
        "Razer Synapse",
        "Ses effets micro (THX Spatial, réduction de bruit) s'ajoutent aux \
         nôtres et se disputent la voie de capture. Coche « micro brut », ou \
         désactive ses effets audio dans Synapse.",
    ),
    // `lghub_updater` est exclu plus bas : accuser un programme de mise à jour
    // de toucher au micro ferait chercher au mauvais endroit.
    (
        "lghub",
        "Logitech G HUB",
        "Ses traitements micro (Blue VO!CE) s'insèrent avant nous. Coche \
         « micro brut », ou désactive Blue VO!CE dans G HUB.",
    ),
    (
        "nvidia broadcast",
        "NVIDIA Broadcast",
        "Il fabrique un micro virtuel et applique son propre débruitage. \
         Deux débruiteurs en série se battent : choisis l'un ou l'autre — \
         soit son micro virtuel avec notre suppression de bruit sur \
         « désactivée », soit le micro physique avec DeepFilterNet.",
    ),
    (
        "voicemeeter",
        "Voicemeeter",
        "Table de mixage virtuelle : le périphérique que tu choisis ici n'est \
         pas le matériel. C'est voulu si tu l'as installé exprès ; sinon, vise \
         le micro physique dans ⚙ Audio.",
    ),
    (
        "vbaudio_cable",
        "VB-Audio Virtual Cable",
        "Câble audio virtuel. S'il est ton périphérique par défaut, ki-chat \
         capte du silence : choisis le micro physique dans ⚙ Audio.",
    ),
    (
        "valorant",
        "Valorant",
        "Sa voix intégrée (Vivox) tient la voie de capture même quand tu ne \
         t'en sers pas. Réglages → Audio → Chat vocal → couper le micro de la \
         voix intégrée, et mettre « Atténuation VoIP » à 0 % : c'est ce curseur \
         qui baisse le son du jeu par à-coups dès que sa détection vocale croit \
         entendre quelqu'un — un micro de bureau qui capte le casque suffit à \
         la déclencher. Et préfère le jeu en fenêtré sans bordure : le plein \
         écran exclusif aggrave tout ce qui touche au son.",
    ),
];

/// Reconnaît un processus dans la liste des suites connues.
///
/// Isolé et testable exprès : la détection est la partie du docteur qui peut
/// se tromper, et se tromper ici fait perdre du temps à quelqu'un.
fn reconnaitre(processus: &str) -> Option<&'static (&'static str, &'static str, &'static str)> {
    let nom = processus
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(processus)
        .trim_end_matches(".exe")
        .trim_end_matches(".EXE")
        .to_ascii_lowercase();
    // Les programmes de mise à jour portent le nom de leur suite sans en
    // partager le comportement : `lghub_updater` ne touche pas au micro, et
    // l'accuser ferait chercher au mauvais endroit.
    if nom.ends_with("_updater") || nom.ends_with("updater") || nom.ends_with("_update") {
        return None;
    }
    CONNUS.iter().find(|(motif, _, _)| nom.contains(motif))
}

#[cfg(windows)]
mod plateforme {
    use super::{reconnaitre, Suite};
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    /// Les suites connues actuellement en cours d'exécution.
    ///
    /// Un instantané de la table des processus, et rien de plus : on ne lit
    /// aucune mémoire, on n'ouvre aucun processus. Un antivirus n'y verra
    /// qu'une énumération, ce que fait le gestionnaire des tâches.
    pub fn suites_en_cours() -> Vec<Suite> {
        let mut out: Vec<Suite> = Vec::new();
        unsafe {
            let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
                return out;
            };
            let mut entree = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            if Process32FirstW(snapshot, &mut entree).is_ok() {
                loop {
                    let nom = String::from_utf16_lossy(
                        &entree.szExeFile[..entree
                            .szExeFile
                            .iter()
                            .position(|c| *c == 0)
                            .unwrap_or(entree.szExeFile.len())],
                    );
                    if let Some((_, affiche, conseil)) = reconnaitre(&nom) {
                        // Une suite peut avoir plusieurs processus (Nahimic en
                        // a trois) : on ne la nomme qu'une fois.
                        if !out.iter().any(|s| s.nom == *affiche) {
                            out.push(Suite {
                                nom: affiche,
                                processus: nom,
                                conseil,
                            });
                        }
                    }
                    if Process32NextW(snapshot, &mut entree).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snapshot);
        }
        out
    }
}

#[cfg(not(windows))]
mod plateforme {
    use super::Suite;

    /// Hors Windows, il n'y a rien de tout cela : ni Sonar, ni Nahimic, ni
    /// mode exclusif. Le docteur ne dit donc rien plutôt que d'inventer.
    pub fn suites_en_cours() -> Vec<Suite> {
        Vec::new()
    }
}

pub use plateforme::suites_en_cours;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_reconnaissance_ignore_casse_chemin_et_extension() {
        for candidat in [
            "SteelSeriesSonar.exe",
            r"C:\Program Files\SteelSeries\SteelSeriesSonar.EXE",
            "steelseriessonar",
        ] {
            let trouve = reconnaitre(candidat).expect(candidat);
            assert_eq!(trouve.1, "SteelSeries Sonar");
        }
    }

    /// Nommer un logiciel innocent fait perdre du temps à quelqu'un : c'est
    /// exactement le contraire du but.
    #[test]
    fn un_processus_ordinaire_n_est_pas_accuse() {
        for innocent in ["explorer.exe", "chrome.exe", "ki-chat.exe", "cargo.exe", ""] {
            assert!(reconnaitre(innocent).is_none(), "{innocent} accusé à tort");
        }
    }

    /// Un pilote virtuel n'est pas un processus : l'énumération de la table
    /// des processus ne peut pas le voir, alors qu'il est en service sous nos
    /// yeux. On le reconnaît donc à son nom — c'est la cause la plus simple
    /// d'un « micro qui ne capte rien », et la plus facile à rater.
    #[test]
    fn un_peripherique_virtuel_se_reconnait_a_son_nom() {
        assert!(virtuel("CABLE Output (VB-Audio Virtual Cable)", true).is_some());
        assert!(virtuel("Voicemeeter Out B1", false).is_some());
        assert!(virtuel("Microphone (NVIDIA Broadcast)", true).is_some());
        // Du vrai matériel n'est pas accusé.
        assert!(virtuel("Microphone sur casque (ROG Strix HS)", true).is_none());
        assert!(virtuel("Realtek High Definition Audio", false).is_none());

        // Le conseil dépend du sens : dire « choisis ton micro » à propos des
        // écouteurs ne sert personne.
        let micro = virtuel("CABLE Output (VB-Audio Virtual Cable)", true).unwrap();
        let sortie = virtuel("CABLE Input (VB-Audio Virtual Cable)", false).unwrap();
        assert!(micro.contains("tu parles dans le vide"));
        assert!(sortie.contains("tu n'entendras personne"));
        assert_ne!(micro, sortie);
    }

    /// Un diagnostic vierge ne doit pas rester muet : « rien à signaler » est
    /// une réponse, « aucun conseil » n'en est pas une.
    #[test]
    fn un_diagnostic_vierge_dit_quand_meme_quelque_chose() {
        let d = Diagnostic { moteur_natif: true, ..Default::default() };
        let conseils = d.conseils();
        assert_eq!(conseils.len(), 1);
        assert!(conseils[0].contains("Rien à signaler"));
    }

    /// Et un diagnostic chargé nomme chaque cause, dans l'ordre où l'on veut
    /// les essayer : ce qui s'interpose d'abord.
    #[test]
    fn les_conseils_viennent_dans_l_ordre_utile() {
        let d = Diagnostic {
            suites: vec![Suite {
                nom: "SteelSeries Sonar",
                processus: "SteelSeriesSonar.exe".into(),
                conseil: "…",
            }],
            exclusif_micro: Some(true),
            exclusif_sortie: None,
            peripherique_micro: Some("CABLE Output (VB-Audio Virtual Cable)".into()),
            peripherique_sortie: None,
            ouvertures_affamees: 4,
            trames_incompletes: 12,
            moteur_natif: false,
            micro_communications: false,
            attenuation_windows: None,
        };
        let conseils = d.conseils();
        // Ce qui s'interpose d'abord, les symptômes ensuite.
        assert!(conseils[0].starts_with("SteelSeries Sonar est en cours"));
        assert!(conseils[1].starts_with("Le micro en service"));
        assert!(conseils[2].contains("Valorant"));
        assert!(conseils[3].contains("contrôle exclusif"));
        assert!(conseils[4].contains("secours"));
        assert!(conseils[5].contains("craquements"));

        // Le rapport se copie : il doit porter l'essentiel sans l'interface.
        let rapport = d.rapport();
        assert!(rapport.contains("moteur : secours"));
        assert!(rapport.contains("VB-Audio"));
        assert!(rapport.contains("jamais réglé"));
        assert!(rapport.contains("SteelSeries Sonar"));
    }

    /// La chaîne complète du « volume du jeu qui baisse » : micro passé en
    /// catégorie communications + atténuation Windows. Le docteur la nomme
    /// d'un bout à l'autre — et change de coupable quand Windows est déjà
    /// réglé sur « Ne rien faire » (c'est alors le mixeur du casque).
    #[test]
    fn le_micro_en_communications_nomme_l_attenuation() {
        let d = Diagnostic {
            micro_communications: true,
            moteur_natif: true,
            ..Default::default()
        };
        let conseils = d.conseils();
        assert!(conseils[0].contains("réduire les autres sons de 80 %"));
        assert!(conseils[0].contains("Ne rien faire"));

        let d = Diagnostic {
            micro_communications: true,
            attenuation_windows: Some(3),
            moteur_natif: true,
            ..Default::default()
        };
        let conseils = d.conseils();
        assert!(conseils[0].contains("déjà réglé"));
        assert!(conseils[0].contains("ChatMix"));

        // Et le rapport porte l'état, pour le copier-coller.
        assert!(d.rapport().contains("communications (réglage ou escalade)"));
        assert!(d.rapport().contains("ne rien faire"));
    }
}
