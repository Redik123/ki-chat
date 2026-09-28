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

/// Le moteur qui tient le micro, d'après sa dernière tentative d'ouverture.
///
/// Quatre états et non un booléen : « pas encore ouvert » se confondait avec
/// « moteur de secours ». Le rapport qui part au démarrage de la session
/// était calculé avant la première ouverture du micro, et la moitié des
/// rapports reçus annonçaient un repli sur cpal qui n'avait jamais eu lieu.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Moteur {
    /// Aucune tentative encore : le moteur vient de démarrer.
    #[default]
    PasEncore = 0,
    /// La dernière ouverture a échoué, moteur natif comme moteur de secours.
    Aucun = 1,
    /// Le moteur WASAPI natif.
    Natif = 2,
    /// Le moteur de secours (cpal).
    Secours = 3,
}

impl Moteur {
    /// L'état tel que le fil de capture le range dans un atomique.
    pub(crate) fn code(self) -> u8 {
        self as u8
    }

    pub(crate) fn depuis_code(code: u8) -> Self {
        match code {
            1 => Self::Aucun,
            2 => Self::Natif,
            3 => Self::Secours,
            _ => Self::PasEncore,
        }
    }
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
    /// Périphériques en service, s'ils sont connus : le micro tel que le
    /// moteur l'a ouvert, la sortie telle qu'elle est réglée. Sert à
    /// reconnaître un **pilote virtuel**, que l'énumération des processus ne
    /// peut pas voir, et à nommer le micro qui ne livre rien.
    pub peripherique_micro: Option<String>,
    pub peripherique_sortie: Option<String>,
    /// Le micro suit-il le défaut de Windows — « défaut système » dans les
    /// réglages, ou repli faute du micro choisi ? Les conseils sur les
    /// défauts de Windows ne valent que dans ce cas : un périphérique choisi
    /// les ignore.
    pub micro_suit_defaut: bool,
    /// Idem pour la sortie.
    pub sortie_suit_defaut: bool,
    /// Quand Windows a deux micros par défaut : (le périphérique par défaut,
    /// que suit ki-chat ; le périphérique de communication, que suit le
    /// « Défaut » de Discord). `None` quand c'est le même, le cas courant.
    pub micro_defauts: Option<(String, String)>,
    /// Idem pour la sortie.
    pub sortie_defauts: Option<(String, String)>,
    /// Ouvertures du micro sans un seul bloc reçu, depuis le démarrage du
    /// moteur. C'est **la** signature du micro affamé : le flux s'ouvre sans
    /// erreur, mais rien n'en sort — l'appareil est éteint derrière un
    /// récepteur resté branché, ou un autre logiciel tient la voie.
    pub ouvertures_affamees: u32,
    /// Trames incomplètes parties vers la carte son (voir `VoiceStats`) :
    /// la voix d'un locuteur arrivée trop tard, en pleine parole.
    pub trames_incompletes: u64,
    /// La carte son trouvée à court en pleine lecture (voir `VoiceStats`) :
    /// le fil de rendu réveillé trop tard.
    pub sortie_a_sec: u64,
    /// Le moteur qui tient le micro : natif, secours (cpal), aucun, ou pas
    /// encore essayé.
    pub moteur: Moteur,
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

        // Deux défauts Windows différents : en « défaut système », ki-chat
        // suit le périphérique par défaut, et le « Défaut » de Discord le
        // périphérique de communication. C'est le « ça marche sur Discord, pas
        // sur ki-chat » : un casque réglé comme périphérique de communication
        // seulement, ou un autre micro (webcam, manette) qui a pris le rôle de
        // défaut. Sans objet quand le périphérique est choisi dans les
        // réglages : ki-chat ignore alors les défauts.
        if self.micro_suit_defaut {
            if let Some((defaut, communication)) = &self.micro_defauts {
                out.push(format!(
                    "Windows a deux micros par défaut : « {defaut} » (le périphérique \
                     par défaut, que ki-chat suit en « défaut système ») et \
                     « {communication} » (le périphérique de communication, que suit \
                     Discord). Si ton micro est « {communication} », choisis-le dans \
                     ⚙ Audio → Micro, ou règle-le aussi comme périphérique par défaut \
                     dans le panneau son de Windows."
                ));
            }
        }
        if self.sortie_suit_defaut {
            if let Some((defaut, communication)) = &self.sortie_defauts {
                out.push(format!(
                    "Windows a deux sorties par défaut : « {defaut} » (le périphérique \
                     par défaut, que ki-chat suit en « défaut système ») et \
                     « {communication} » (le périphérique de communication, que suit \
                     Discord). Si tu entends Discord dans « {communication} » mais \
                     ki-chat ailleurs, choisis « {communication} » dans ⚙ Audio → \
                     Sortie."
                ));
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
                     faire ». Et si tu as accepté la bascule parce que le micro ne \
                     livrait rien, le vrai correctif est ailleurs : rallumer \
                     l'appareil, ou choisir dans ⚙ Audio le micro physique — pas un \
                     périphérique virtuel dont l'application ne tourne pas."
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
            // L'appareil éteint d'abord : c'est ce que le terrain a montré à
            // chaque fois — un casque abîmé, une manette endormie dont le
            // récepteur restait branché. La voix d'un jeu qui tiendrait la
            // voie n'a jamais été prouvée ; elle vient donc en second.
            let micro = match &self.peripherique_micro {
                Some(nom) => format!("Le micro « {nom} »"),
                None => "Le micro".into(),
            };
            let pas_le_tien = if self.micro_suit_defaut {
                " Et si ce n'est pas ton micro — c'est celui que Windows désigne par \
                 défaut —, choisis le bon dans ⚙ Audio."
            } else {
                ""
            };
            out.push(format!(
                "{micro} s'est ouvert {} fois sans livrer un seul bloc. Le plus \
                 souvent, l'appareil est éteint ou en veille alors que son récepteur \
                 reste branché — casque sans fil, manette : rallume-le.{pas_le_tien} \
                 Sinon, un autre logiciel tient la voie de capture — la voix intégrée \
                 d'un jeu : dans Valorant, Réglages → Audio → Chat vocal → couper le \
                 micro de la voix intégrée, que tu n'utilises pas puisque tu es ici.",
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
        match self.moteur {
            Moteur::Secours => out.push(
                "Le moteur audio natif n'a pas pu s'ouvrir : on tourne sur le moteur de \
                 secours, qui ne demande à Windows ni la conversion de format \
                 automatique, ni le mode brut. Le journal audio dit pourquoi."
                    .into(),
            ),
            Moteur::Aucun => out.push(
                "Le micro refuse de s'ouvrir, moteur natif comme moteur de secours : il \
                 a disparu, ou un autre logiciel le tient en mode exclusif. Le journal \
                 audio donne l'erreur de Windows."
                    .into(),
            ),
            // Pas encore essayé : rien à conseiller, le rapport le dit.
            Moteur::PasEncore | Moteur::Natif => {}
        }
        // Deux symptômes qui s'entendent pareil — un craquement, une
        // micro-coupure — mais n'ont ni la même cause ni le même remède. La
        // sortie robuste ajoute 70 ms de latence : elle ne se conseille que
        // sur la preuve que la carte son a manqué de données.
        if self.sortie_a_sec > 0 {
            out.push(format!(
                "La carte son s'est trouvée {} fois à court en pleine lecture : autant \
                 de craquements. Si cela arrive pendant une partie, c'est que la machine \
                 est saturée ou la carte son USB fragile — coche « Sortie audio \
                 robuste » dans ⚙ Audio → Sortie (plus de marge, un peu plus de \
                 latence), et vérifie que le jeu tourne en fenêtré sans bordure plutôt \
                 qu'en plein écran exclusif.",
                self.sortie_a_sec
            ));
        }
        if self.trames_incompletes > 0 {
            out.push(format!(
                "{} trames incomplètes sont parties vers la carte son : la voix d'un \
                 copain est arrivée trop tard, en pleine phrase — autant de \
                 micro-coupures. C'est le réseau (le sien ou le tien, souvent le \
                 Wi-Fi) ou une machine trop chargée pour décoder à temps. Si ça hache \
                 souvent, fixe le tampon de gigue plus haut dans ⚙ Réseau & qualité \
                 (60 ou 80 ms) : un peu plus de latence, plus de marge.",
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
            match self.moteur {
                Moteur::Natif => "natif (WASAPI)",
                Moteur::Secours => "secours (cpal)",
                Moteur::Aucun => "aucun — le micro refuse de s'ouvrir",
                Moteur::PasEncore => "micro pas encore ouvert",
            }
        ));
        out.push_str(&format!(
            "périphériques : micro {} ({}) · sortie {} ({})\n",
            self.peripherique_micro.as_deref().unwrap_or("inconnu"),
            origine(self.micro_suit_defaut),
            self.peripherique_sortie.as_deref().unwrap_or("inconnu"),
            origine(self.sortie_suit_defaut),
        ));
        // Consigné même quand un périphérique choisi le rend sans effet :
        // de loin, c'est ce qui départage « ki-chat prend le mauvais micro »
        // et « le micro choisi ne marche pas ».
        for (quoi, defauts) in [("micro", &self.micro_defauts), ("sortie", &self.sortie_defauts)] {
            if let Some((defaut, communication)) = defauts {
                out.push_str(&format!(
                    "défauts Windows différents ({quoi}) : « {defaut} » par défaut, \
                     « {communication} » en communication\n"
                ));
            }
        }
        out.push_str(&format!(
            "mode exclusif : micro {} · sortie {}\n",
            etat(self.exclusif_micro),
            etat(self.exclusif_sortie)
        ));
        out.push_str(&format!(
            "ouvertures affamées : {} · trames incomplètes : {} · carte son à sec : {}\n",
            self.ouvertures_affamees, self.trames_incompletes, self.sortie_a_sec
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

/// D'où vient le périphérique en service, en clair.
fn origine(suit_defaut: bool) -> &'static str {
    if suit_defaut {
        "défaut Windows"
    } else {
        "choisi"
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
// Hors Windows, seuls les tests la lisent : les suites qu'elle nomme n'y
// existent pas.
#[cfg_attr(not(windows), allow(dead_code))]
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
#[cfg_attr(not(windows), allow(dead_code))]
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
        let d = Diagnostic { moteur: Moteur::Natif, ..Default::default() };
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
            micro_suit_defaut: true,
            sortie_suit_defaut: false,
            micro_defauts: Some((
                "CABLE Output (VB-Audio Virtual Cable)".into(),
                "Microphone (PRO X 2 LIGHTSPEED)".into(),
            )),
            sortie_defauts: None,
            ouvertures_affamees: 4,
            trames_incompletes: 12,
            sortie_a_sec: 3,
            moteur: Moteur::Secours,
            micro_communications: false,
            attenuation_windows: None,
        };
        let conseils = d.conseils();
        // Ce qui s'interpose d'abord, le périphérique en service ensuite, les
        // symptômes en dernier.
        assert!(conseils[0].starts_with("SteelSeries Sonar est en cours"));
        assert!(conseils[1].starts_with("Le micro en service"));
        assert!(conseils[2].starts_with("Windows a deux micros par défaut"));
        assert!(conseils[3].contains("Valorant"));
        assert!(conseils[4].contains("contrôle exclusif"));
        assert!(conseils[5].contains("secours"));
        assert!(conseils[6].contains("craquements") && conseils[6].contains("Sortie audio robuste"));
        assert!(conseils[7].contains("tampon de gigue"));

        // Le rapport se copie : il doit porter l'essentiel sans l'interface.
        let rapport = d.rapport();
        assert!(rapport.contains("carte son à sec : 3"));
        assert!(rapport.contains("moteur : secours"));
        assert!(rapport.contains("VB-Audio"));
        assert!(rapport.contains("(défaut Windows)"));
        assert!(rapport.contains("défauts Windows différents (micro)"));
        assert!(rapport.contains("jamais réglé"));
        assert!(rapport.contains("SteelSeries Sonar"));
    }

    /// Des trous dans la voix reçue ne disent rien de la carte son : la
    /// sortie robuste (+70 ms) ne se conseille pas sur eux. Elle l'était, sur
    /// un compteur que chaque fin de phrase faisait monter.
    #[test]
    fn des_trous_dans_la_voix_ne_conseillent_pas_la_sortie_robuste() {
        let d = Diagnostic {
            moteur: Moteur::Natif,
            trames_incompletes: 40,
            ..Default::default()
        };
        let conseils = d.conseils();
        assert_eq!(conseils.len(), 1);
        assert!(conseils[0].contains("tampon de gigue"));
        assert!(!conseils[0].contains("Sortie audio robuste"));
    }

    /// Deux défauts Windows différents : le conseil ne vaut qu'en « défaut
    /// système » — un micro choisi ignore les défauts. Le rapport, lui, le
    /// consigne toujours : de loin, c'est ce qui départage les deux pannes.
    #[test]
    fn les_defauts_divergents_ne_comptent_qu_en_defaut_systeme() {
        let defauts = Some((
            "Microphone (HD Webcam C270)".to_string(),
            "Microphone (PRO X 2 LIGHTSPEED)".to_string(),
        ));
        let d = Diagnostic {
            moteur: Moteur::Natif,
            micro_suit_defaut: true,
            micro_defauts: defauts.clone(),
            ..Default::default()
        };
        let conseils = d.conseils();
        assert_eq!(conseils.len(), 1);
        assert!(conseils[0].contains("« Microphone (HD Webcam C270) »"));
        assert!(conseils[0].contains("« Microphone (PRO X 2 LIGHTSPEED) »"));
        assert!(conseils[0].contains("Discord"));

        let d = Diagnostic {
            moteur: Moteur::Natif,
            micro_suit_defaut: false,
            micro_defauts: defauts,
            ..Default::default()
        };
        assert!(d.conseils()[0].contains("Rien à signaler"));
        assert!(d.rapport().contains("défauts Windows différents (micro)"));
        assert!(d.rapport().contains("(choisi)"));

        // La sortie a son propre conseil, qui parle de la sortie.
        let d = Diagnostic {
            moteur: Moteur::Natif,
            sortie_suit_defaut: true,
            sortie_defauts: Some((
                "Haut-parleurs (Realtek(R) Audio)".into(),
                "Haut-parleurs (PRO X 2 LIGHTSPEED)".into(),
            )),
            ..Default::default()
        };
        let conseils = d.conseils();
        assert_eq!(conseils.len(), 1);
        assert!(conseils[0].contains("deux sorties"));
        assert!(conseils[0].contains("⚙ Audio → Sortie"));
    }

    /// Un rapport pris avant la première ouverture du micro ne crie pas au
    /// moteur de secours : il dit qu'il est trop tôt. C'est ce qui faussait
    /// la moitié des rapports partagés.
    #[test]
    fn un_rapport_trop_tot_dit_qu_il_est_trop_tot() {
        let d = Diagnostic::default();
        assert_eq!(d.moteur, Moteur::PasEncore);
        assert!(d.rapport().contains("moteur : micro pas encore ouvert"));
        assert!(d.conseils().iter().all(|c| !c.contains("secours")));

        // Un micro qui refuse de s'ouvrir n'est pas un repli sur cpal non plus.
        let d = Diagnostic { moteur: Moteur::Aucun, ..Default::default() };
        assert!(d.rapport().contains("refuse de s'ouvrir"));
        assert!(d.conseils()[0].contains("refuse de s'ouvrir"));

        // Et l'état fait l'aller-retour par l'atomique du fil de capture.
        for m in [Moteur::PasEncore, Moteur::Aucun, Moteur::Natif, Moteur::Secours] {
            assert_eq!(Moteur::depuis_code(m.code()), m);
        }
    }

    /// Le micro qui ne livre rien est nommé, et l'appareil éteint passe avant
    /// le logiciel qui tiendrait la voie : sur le terrain, c'était à chaque
    /// fois l'appareil — une manette endormie dont le récepteur restait
    /// branché, un casque abîmé.
    #[test]
    fn le_micro_affame_est_nomme_et_la_veille_vient_d_abord() {
        let d = Diagnostic {
            moteur: Moteur::Natif,
            peripherique_micro: Some("Microphone sur casque (2- Wireless Controller)".into()),
            micro_suit_defaut: true,
            ouvertures_affamees: 12,
            exclusif_micro: Some(false),
            ..Default::default()
        };
        let conseils = d.conseils();
        let c = &conseils[0];
        assert!(c.contains("« Microphone sur casque (2- Wireless Controller) »"));
        let veille = c.find("éteint ou en veille").expect("la veille est citée");
        let logiciel = c.find("autre logiciel").expect("le logiciel aussi");
        assert!(veille < logiciel);
        // Pris comme défaut Windows : on dit de choisir le bon micro.
        assert!(c.contains("choisis le bon"));

        // Choisi dans les réglages : pas de renvoi vers le défaut Windows.
        let d = Diagnostic { micro_suit_defaut: false, ..d };
        assert!(!d.conseils()[0].contains("choisis le bon"));
    }

    /// La chaîne complète du « volume du jeu qui baisse » : micro passé en
    /// catégorie communications + atténuation Windows. Le docteur la nomme
    /// d'un bout à l'autre — et change de coupable quand Windows est déjà
    /// réglé sur « Ne rien faire » (c'est alors le mixeur du casque).
    #[test]
    fn le_micro_en_communications_nomme_l_attenuation() {
        let d = Diagnostic {
            micro_communications: true,
            moteur: Moteur::Natif,
            ..Default::default()
        };
        let conseils = d.conseils();
        assert!(conseils[0].contains("réduire les autres sons de 80 %"));
        assert!(conseils[0].contains("Ne rien faire"));

        let d = Diagnostic {
            micro_communications: true,
            attenuation_windows: Some(3),
            moteur: Moteur::Natif,
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
