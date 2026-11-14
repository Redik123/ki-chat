//! Ce qui arrive du serveur : chaque message, et ce qu'il change dans l'appli.

use ki_protocol::{ChannelId, ChannelKind, ChatRecord, ClientMsg, ServerMsg, UserId};

use crate::{
    cartes, net, partage, photos, porte_ui, safe_name, safe_server_info, secours, servers, sfx,
    KiApp, NonLu, HISTORIQUE_PREMIERE_PAGE, MES_COMPTES_MAX,
};
use crate::fil::{clean_record, clean_reply, reaction_locale};
use crate::membres::{couleur_de_membre, est_bot};
use crate::salons::VoicePrompt;
use crate::theme::TEXT_DIM;

impl KiApp {
    pub(crate) fn poll_events(&mut self) {
        let Some(conn) = &self.session.conn else { return };
        let events: Vec<net::Event> = conn.events.try_iter().collect();
        for event in events {
            match event {
                net::Event::ConnectFailed(e) => {
                    // Identité changée : c'est le seul échec où l'utilisateur
                    // a une décision à prendre, et il lui faut de quoi la
                    // prendre. On le dit en clair et l'on propose la sortie,
                    // au lieu de laisser une erreur de bibliothèque et un
                    // carnet qu'aucun écran ne permet de corriger.
                    let changed = net::identite_changee(&e);
                    if changed {
                        self.identity_alarm = Some(self.url.trim().to_string());
                        self.disconnect(Some(
                            "l'identité du serveur a changé depuis la dernière connexion"
                                .into(),
                        ));
                    } else {
                        self.connexion_perdue(Some(e));
                    }
                }
                net::Event::Disconnected => {
                    let had_error = self.error.take();
                    self.connexion_perdue(
                        had_error.or_else(|| Some("déconnecté du serveur".into())),
                    );
                }
                // Expulsé, banni, mot de passe réinitialisé : une sortie
                // voulue par le serveur, pas une coupure. La prendre pour une
                // coupure, c'était se reconnecter dans la seconde et
                // retourner dans son salon vocal — une expulsion ne durait
                // qu'une seconde.
                net::Event::Congedie(message) => {
                    ki_voice::journal(format!("session close par le serveur : {message}"));
                    self.disconnect(Some(message));
                    // La reprise au lancement suivant non plus.
                    self.lanceur.session_auto = None;
                }
                net::Event::Msg(msg) => self.handle_server_msg(msg),
                net::Event::Fingerprint(fp) => {
                    // Première connexion à ce serveur : on retient son
                    // identité. Les suivantes la compareront, et refuseront
                    // quiconque se présenterait à sa place. Une empreinte
                    // déjà connue ne change pas ici — la connexion aurait
                    // échoué avant si elle avait différé.
                    let address = self.url.trim().to_string();
                    self.server_fingerprint = fp.clone();
                    if let Some(s) = self.lanceur.book.iter_mut().find(|s| s.address == address) {
                        if s.cert_fingerprint.is_empty() {
                            tracing::info!("empreinte de {address} retenue : {fp}");
                            s.cert_fingerprint = fp;
                        }
                    }
                }
            }
        }
    }

    /// Un message du serveur, aiguillé vers son domaine. Pas de bras « tout le
    /// reste » ici : un message ajouté au protocole ne compile pas tant qu'il
    /// n'a pas sa place.
    pub(crate) fn handle_server_msg(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::Welcome { .. } => self.recu_accueil(msg),
            ServerMsg::Chat { .. }
            | ServerMsg::Nouveau { .. }
            | ServerMsg::NonLus { .. }
            | ServerMsg::History { .. }
            | ServerMsg::Reaction { .. }
            | ServerMsg::MessageDeleted { .. }
            | ServerMsg::MessageEdited { .. }
            | ServerMsg::SearchResults { .. }
            | ServerMsg::HistoryPage { .. } => self.recu_fil(msg),
            ServerMsg::Members { .. }
            | ServerMsg::MemberUpdate { .. }
            | ServerMsg::UserJoined { .. }
            | ServerMsg::UserLeft { .. }
            | ServerMsg::VoiceState { .. } => self.recu_membres(msg),
            ServerMsg::StreamGranted { .. }
            | ServerMsg::StreamStarted { .. }
            | ServerMsg::StreamStopped { .. }
            | ServerMsg::WatchDenied { .. }
            | ServerMsg::WatchAccepted { .. }
            | ServerMsg::KeyframeNeeded { .. }
            | ServerMsg::StreamMetaChanged { .. }
            | ServerMsg::StreamViewers { .. }
            | ServerMsg::StreamBudget { .. } => self.recu_streams(msg),
            ServerMsg::LiaisonRiot { .. }
            | ServerMsg::MesComptes { .. }
            | ServerMsg::FicheValorant { .. }
            | ServerMsg::MusiqueEtat { .. }
            | ServerMsg::MusiqueResultats { .. }
            | ServerMsg::Cartes { .. }
            | ServerMsg::CartesRefus { .. }
            | ServerMsg::TactiqueEtat { .. }
            | ServerMsg::Tactique { .. }
            | ServerMsg::TactiqueRefus { .. }
            | ServerMsg::MonJournal { .. }
            | ServerMsg::Soiree { .. }
            | ServerMsg::MatchsDuGroupe { .. }
            | ServerMsg::ValorantChange
            | ServerMsg::StatsValorant { .. } => self.recu_jeux(msg),
            ServerMsg::Poke { .. }
            | ServerMsg::PokeRefuse { .. }
            | ServerMsg::Error { .. }
            | ServerMsg::Kicked { .. }
            | ServerMsg::AdminInfo { .. }
            | ServerMsg::AuditLog { .. }
            | ServerMsg::Perms { .. }
            | ServerMsg::Roles { .. }
            | ServerMsg::ChannelsUpdated { .. }
            | ServerMsg::VoiceLocked { .. }
            | ServerMsg::InviteCreated { .. }
            | ServerMsg::AdminFichiers { .. }
            | ServerMsg::Info { .. }
            | ServerMsg::ServerInfo { .. }
            | ServerMsg::Avatar { .. }
            | ServerMsg::NetQuality { .. }
            | ServerMsg::Pong => self.recu_serveur(msg),
            ServerMsg::ClaudeConversation { .. }
            | ServerMsg::ClaudeMessage { .. }
            | ServerMsg::ClaudeEtape { .. }
            | ServerMsg::ClaudeErreur { .. }
            | ServerMsg::ClaudeMembres { .. } => self.recu_claude(msg),
            ServerMsg::PorteOuverte { .. }
            | ServerMsg::PorteDemande { .. }
            | ServerMsg::PorteEtat { .. }
            | ServerMsg::PorteFermee { .. }
            | ServerMsg::PorteInvitation { .. } => self.recu_portes(msg),
        }
    }

    /// L'accueil du serveur : qui l'on est, ce qu'il sait faire, où l'on arrive.
    fn recu_accueil(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::Welcome {
                user_id,
                voice_token,
                is_admin,
                perms,
                rank,
                roles,
                channels,
                server,
                portes,
                medailles,
                choix_medailles,
                cartes,
                comptes_joues,
                journal_parties,
                tactique,
                soiree,
                journal_lecture,
                suivi_valorant,
                matchs_groupe,
                claude,
                protocole,
                ..
            } => {
                self.session.welcomed = true;
                // Un plantage d'ici se racontera à ce serveur, et à lui seul.
                secours::serveur_courant(Some(&self.url));
                // Les brouillons rangés sont ceux de ce serveur : une reprise
                // les garde, un autre serveur les fait oublier.
                self.brouillons.serveur(&self.url);
                // Un serveur qui parle un protocole plus récent que nous :
                // ce qu'il envoie de neuf ne se lira pas ici. Le dire, plutôt
                // que de laisser des morceaux manquer sans explication.
                if protocole > ki_protocol::PROTOCOLE {
                    ki_voice::journal(format!(
                        "serveur au protocole {protocole}, ki-chat au protocole {} : mise à jour conseillée",
                        ki_protocol::PROTOCOLE
                    ));
                    self.info = Some(
                        "ce serveur est plus récent que ton ki-chat — installe la dernière version".into(),
                    );
                }
                // Le serveur dit lui-même s'il sert les portes web : un
                // serveur antérieur ne pose pas le champ, et le panneau
                // reste caché — rien de nouveau ne lui part.
                self.portes.disponible = portes;
                // Même preuve pour les médailles VALORANT.
                self.donnees_valo.serveur_medailles = medailles;
                // Et pour le choix de les montrer, que le serveur apprend
                // une fois par connexion, puis à chaque changement.
                self.donnees_valo.serveur_choix_medailles = choix_medailles;
                self.donnees_valo.choix_medailles_dit = None;
                // Et pour les tables de cartes — et leurs règles en plus,
                // qu'un serveur au protocole 1 prendrait pour un geste
                // invalide.
                self.serveur_cartes = cartes;
                self.cartes.lock().unwrap().regles_disponibles = protocole >= 2;
                // Et pour la carte tactique du salon.
                self.serveur_tactique = tactique;
                // Et pour le fil de la soirée.
                self.donnees_valo.serveur_soiree = soiree;
                // Et pour l'avis « des fiches ont changé » : sans lui, rien
                // ne lui part, et la page s'actualise à la main.
                self.donnees_valo.suivi_a_la_connexion(suivi_valorant);
                // Et pour les parties du groupe, page par page : ce qu'on
                // en gardait est à relire sur cette connexion.
                self.donnees_valo.serveur_matchs = matchs_groupe;
                self.donnees_valo.matchs.reconnexion();
                // Et pour la relecture de mon journal des parties.
                self.donnees_valo.serveur_journal_lecture = journal_lecture;
                // Et pour Claude : sans clé d'API chez lui, pas de salon.
                // Resté ouvert d'avant une coupure, il se recharge.
                if self.claude.sur_bienvenue(claude) {
                    self.send(ClientMsg::ClaudeHistorique { de: None });
                }
                // Et pour nos comptes Riot, qu'il envoie juste après.
                self.donnees_valo.serveur_comptes_joues = comptes_joues;
                self.donnees_valo.mes_comptes.clear();
                // Et pour le journal de nos parties, dont il apprendra notre
                // choix au prochain tour.
                self.donnees_valo.serveur_journal = journal_parties;
                self.donnees_valo.choix_journal_dit = None;
                self.session.connecting = false;
                self.session.connect_started = None;
                self.error = None;
                self.remember_connection();
                // Connecté : on y reviendra tout seul au prochain lancement,
                // si le mot de passe est mémorisé — sans lui, impossible.
                self.lanceur.session_auto = self.lanceur.selected.filter(|_| self.lanceur.remember_password);
                // Une nouvelle connexion ne connaît pas notre statut de jeu,
                // ni si l'on accepte les pokes : ils repartiront au prochain
                // tour.
                self.jeu_envoye = None;
                self.pokes_envoye = None;
                self.session.my_id = Some(user_id);
                // `is_admin` reste la réponse d'un serveur antérieur aux
                // rôles : sans permissions annoncées, on lui accorde tout
                // plutôt que rien, sans quoi son propre admin perdrait son
                // panneau en mettant le client à jour.
                self.session.my_perms = if perms == 0 && is_admin {
                    ki_protocol::perm::ADMINISTRATOR
                } else if perms == 0 {
                    ki_protocol::perm::DEFAULT
                } else {
                    perms
                };
                self.session.my_rank = rank;
                self.session.roles = roles;
                self.session.voice_token = voice_token;
                *self.jeton_http.lock().unwrap() =
                    net::JetonHttp { base: self.http_base(), jeton: format!("{voice_token:x}") };
                self.session.channels = channels;
                // L'identité du serveur arrive **dès** le Welcome. L'ignorer
                // ici laissait nom et logo invisibles jusqu'à ce qu'un admin
                // les ré-enregistre, ce qui n'arrivait jamais.
                self.server_info = safe_server_info(server);
                self.cache_server_identity();
                if !self.admin.ouverte {
                    self.admin.nom = self.server_info.name.clone();
                    self.admin.adresse_web = self.server_info.adresse_web.clone();
                }
                // Reprise réussie : on rend la place qu'on occupait, salon lu
                // **et** salon vocal. Se reconnecter tout seul pour se
                // réveiller ailleurs, muet, en pleine partie, ne vaudrait
                // guère mieux que de ne pas se reconnecter.
                //
                // Le vocal est la seule exception au « on n'entre dans aucun
                // vocal, ça se décide » d'en dessous : ici, c'était décidé —
                // c'est le réseau qui en a décidé autrement.
                if let Some(r) = self.reprise.take() {
                    let salon = r.salon.filter(|c| self.session.channels.iter().any(|k| k.id == *c));
                    let vocal = r.vocal.filter(|c| self.session.channels.iter().any(|k| k.id == *c));
                    if let Some(c) = salon {
                        self.join(c);
                    }
                    if let Some(c) = vocal {
                        // Un salon devenu verrouillé pendant la coupure
                        // refusera : le client demande alors le mot de passe,
                        // comme pour une entrée ordinaire.
                        self.join_voice(c);
                    }
                    self.info = Some("connexion rétablie".into());
                    if salon.is_some() {
                        return;
                    }
                }
                // Relancé après un plantage : même raisonnement, c'était
                // décidé — c'est ki-chat qui en a décidé autrement. Une
                // fois, et seulement chez le serveur d'avant.
                if let Some(r) = self.retour.take().filter(|r| self.lanceur.selected == Some(r.serveur)) {
                    let salon = r.salon.filter(|c| self.session.channels.iter().any(|k| k.id == *c));
                    let vocal = r.vocal.filter(|c| self.session.channels.iter().any(|k| k.id == *c));
                    if let Some(c) = salon {
                        self.join(c);
                    }
                    if let Some(c) = vocal {
                        self.join_voice(c);
                    }
                    tracing::info!("retour après plantage : salon {salon:?}, vocal {vocal:?}");
                    // Le bandeau du démarrage s'efface à la connexion : sans
                    // ce mot, la fenêtre qui clignote passe pour un caprice.
                    self.info = Some(
                        if vocal.is_some() {
                            "ki-chat a planté et s'est relancé : te revoilà dans ton salon vocal"
                        } else {
                            "ki-chat a planté et s'est relancé : te revoilà où tu étais"
                        }
                        .into(),
                    );
                    if salon.is_some() {
                        return;
                    }
                }
                // On ouvre le premier salon textuel pour avoir de quoi lire,
                // mais on n'entre dans aucun vocal : ça se décide.
                let first_text = self.session
                    .channels
                    .iter()
                    .find(|c| c.kind == ChannelKind::Text)
                    .map(|c| c.id);
                if let Some(first) = first_text {
                    self.join(first);
                }
            }
            // L'aiguillage de `handle_server_msg` n'envoie ici que ce qui précède.
            autre => debug_assert!(false, "message mal aiguillé : {autre:?}"),
        }
    }

    /// Le fil des salons : messages, pastilles, historique, réactions, recherche.
    fn recu_fil(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::Chat {
                user_id,
                username,
                text,
                ts,
                reply_to,
                channel,
            } => {
                // Notre message est revenu : il est bien parti.
                if Some(user_id) == self.session.my_id
                    && self.envoi_en_attente.as_ref().is_some_and(|e| e.est_l_echo_du_message(&text))
                {
                    self.envoi_confirme();
                }
                // Le serveur ne l'envoie qu'aux lecteurs du salon, mais un
                // changement de salon peut le croiser en route : écrit dans
                // A juste avant que le serveur traite notre `Join` vers B, il
                // arrive en `Chat` et non en `Nouveau`. Il n'entre pas dans
                // ce fil, mais il compte comme un `Nouveau` — le jeter, c'était
                // perdre sa pastille et son son, et le serveur nous tient
                // pour lecteur de A jusqu'avant lui. `0` = serveur antérieur,
                // qui ne le dit pas — on lui fait confiance.
                if channel != 0 && self.session.current != Some(channel) {
                    if Some(user_id) != self.session.my_id {
                        let pour_moi = self.me_nomme(user_id, &text);
                        self.compter_non_lu(channel, ts, pour_moi);
                        if !est_bot(user_id) {
                            self.notifier(pour_moi);
                        }
                    }
                    return;
                }
                // Être **nommé** n'est pas un message de plus : ça appelle une
                // réponse. On prévient donc même fenêtre au premier plan, là où
                // un message ordinaire ne le fait pas.
                let pour_moi = self.me_nomme(user_id, &text);
                let de_moi = Some(user_id) == self.session.my_id;
                // Vu, ou pas : fenêtre au premier plan et fil collé en bas.
                // Sinon le salon qu'on « lit » sans le regarder prend sa
                // pastille lui aussi, jusqu'au retour du focus.
                let vu = self.window_focused && self.fil_en_bas;
                if !de_moi && !vu {
                    if let Some(c) = self.session.current {
                        self.compter_non_lu(c, ts, pour_moi);
                    }
                }
                // Jamais de son pour ses propres messages, ni pour ceux du
                // serveur (fil de jeu, bot) ; et pour ceux des autres,
                // seulement quand la fenêtre n'a pas le focus — en pleine
                // conversation, un bip par message serait insupportable.
                // La barre des tâches clignote en prime : le « quoi de neuf »
                // se voit même en jeu.
                if !de_moi && !est_bot(user_id) && (pour_moi || !self.window_focused) {
                    self.notifier(pour_moi);
                }
                if vu {
                    if let Some(c) = self.session.current {
                        self.programmer_lu(c);
                    }
                }
                self.session.messages.push(ChatRecord {
                    user_id,
                    username,
                    text,
                    ts,
                    reply_to: reply_to.map(clean_reply),
                    reactions: Vec::new(),
                    edited: false,
                });
                if self.session.messages.len() > 500 {
                    self.session.messages.remove(0);
                }
            }
            ServerMsg::Nouveau { channel, user_id, username: _, text, ts } => {
                // Un message dans un salon qu'on ne lit pas. Le sien n'est
                // pas du nouveau ; et s'il concerne le salon qu'on vient
                // d'ouvrir, c'est qu'il a croisé notre `Join` : l'historique
                // demandé dans la foulée le contiendra.
                if Some(user_id) == self.session.my_id || self.session.current == Some(channel) {
                    return;
                }
                let pour_moi = self.me_nomme(user_id, &text);
                self.compter_non_lu(channel, ts, pour_moi);
                // Le serveur (fil de jeu, bot) pose des pastilles, jamais
                // de son : il poste souvent, et personne ne lui répond.
                if !est_bot(user_id) {
                    self.notifier(pour_moi);
                }
            }
            ServerMsg::NonLus { salons } => {
                // Le serveur tient les lus : à partir d'ici, on lui dit ce
                // qu'on lit. Ce qu'il compte remplace ce qu'on croyait — il
                // sait, lui, ce qui s'est écrit pendant qu'on n'était pas là.
                self.session.serveur_gere_lus = true;
                for salon in salons {
                    if salon.non_lus == 0 {
                        self.session.non_lus.remove(&salon.channel);
                        continue;
                    }
                    self.session.non_lus.insert(
                        salon.channel,
                        NonLu { nb: salon.non_lus, mention: salon.mention, depuis: salon.dernier_ts },
                    );
                }
                // Le salon déjà ouvert (celui de la connexion, ou de la
                // reprise) : « nouveaux messages » se pose, et il se lira
                // dès qu'on le regarde — pas avant.
                if let Some(c) = self.session.current {
                    if let Some(n) = self.session.non_lus.get(&c) {
                        self.separateur_nouveaux = Some(n.depuis);
                    }
                    self.programmer_lu(c);
                }
            }
            ServerMsg::History { messages } => {
                let courte = messages.len() < HISTORIQUE_PREMIERE_PAGE as usize;
                self.session.messages = messages.into_iter().map(clean_record).collect();
                // Une première page moins pleine que demandé : le plus
                // souvent, il n'y a rien avant — et « Charger les messages
                // plus anciens » restait affiché dans un salon de trois
                // messages. Mais le serveur raccourcit aussi une page de
                // messages très longs : on ne conclut donc pas, on demande la
                // page d'avant. Sa réponse dit s'il en reste (`more`), et le
                // bouton s'en va de lui-même quand il n'y en a pas.
                if courte {
                    if self.session.messages.is_empty() {
                        self.session.history_more = false;
                    } else {
                        self.load_older_history();
                    }
                }
            }
            ServerMsg::Reaction { channel, message, emoji, by, on } => {
                if self.session.current != Some(channel) {
                    return;
                }
                if let Some(m) = self.session
                    .messages
                    .iter_mut()
                    .find(|m| m.user_id == message.user_id && m.ts == message.ts)
                {
                    reaction_locale(&mut m.reactions, &emoji, by, on);
                }
            }
            ServerMsg::MessageDeleted { channel, message } => {
                if self.session.current != Some(channel) {
                    return;
                }
                self.session.messages.retain(|m| !(m.user_id == message.user_id && m.ts == message.ts));
                if self.reponse_a.as_ref().is_some_and(|r| r.user_id == message.user_id && r.ts == message.ts) {
                    self.reponse_a = None;
                }
                if self.menu_message.as_ref().is_some_and(|m| m.message == message) {
                    self.menu_message = None;
                }
                if self.edition == Some(message) {
                    self.annuler_edition();
                }
            }
            ServerMsg::MessageEdited { channel, message, text } => {
                // Notre modification est revenue : elle est bien passée.
                if self.envoi_en_attente.as_ref().is_some_and(|e| e.est_l_echo_de_la_modification(&message)) {
                    self.envoi_confirme();
                }
                if self.session.current != Some(channel) {
                    return;
                }
                // Le texte vient d'autrui : les mêmes règles d'affichage
                // que le reste.
                let propre = ki_protocol::safe_display(&text, ki_protocol::MAX_CHAT_TEXT);
                if let Some(m) = self.session
                    .messages
                    .iter_mut()
                    .find(|m| m.user_id == message.user_id && m.ts == message.ts)
                {
                    m.text = propre.clone();
                    m.edited = true;
                }
                // La citation sous une réponse en cours suit le texte.
                if let Some(r) = self
                    .reponse_a
                    .as_mut()
                    .filter(|r| r.user_id == message.user_id && r.ts == message.ts)
                {
                    r.excerpt = ki_protocol::excerpt_of(&propre);
                }
            }
            ServerMsg::SearchResults { query, hits, more } => {
                // Une réponse à une requête qu'on ne pose plus est jetée : le
                // serveur relit des fichiers, et la réponse à « val » peut
                // très bien arriver après celle à « valorant ».
                if self.recherche.envoyee.as_deref() != Some(query.as_str()) {
                    return;
                }
                self.recherche.envoyee = None;
                self.recherche.resultats = hits;
                self.recherche.encore = more;
            }
            ServerMsg::HistoryPage { messages, more, channel } => {
                // Une page d'un autre salon est jetée : le serveur relit le
                // fichier hors de l'ordre du flux, si bien qu'elle peut
                // arriver après un changement de salon. L'appliquer collerait
                // une conversation en tête d'une autre, et écraserait au
                // passage le « reste-t-il du passé » du salon courant.
                // `0` = serveur antérieur, qui ne renseigne pas ce champ.
                if channel != 0 && self.session.current != Some(channel) {
                    return;
                }
                self.session.history_pending = false;
                self.session.history_more = more;
                if messages.is_empty() {
                    return;
                }
                // En tête, et surtout sans doublon : une page peut recouvrir
                // ce qui est déjà affiché si des messages sont arrivés entre
                // la demande et la réponse.
                let known: std::collections::HashSet<(UserId, u64)> =
                    self.session.messages.iter().map(|m| (m.user_id, m.ts)).collect();
                let mut older: Vec<ChatRecord> = messages
                    .into_iter()
                    .map(clean_record)
                    .filter(|m| !known.contains(&(m.user_id, m.ts)))
                    .collect();
                if older.is_empty() {
                    return;
                }
                // Rien à recaler : la liste du fil garde à sa place la ligne
                // qu'on lit quand le contenu grandit au-dessus d'elle.
                older.append(&mut self.session.messages);
                self.session.messages = older;
            }
            // L'aiguillage de `handle_server_msg` n'envoie ici que ce qui précède.
            autre => debug_assert!(false, "message mal aiguillé : {autre:?}"),
        }
    }

    /// Les membres : qui est là, qui arrive, qui part, qui parle.
    fn recu_membres(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::Members { members } => {
                self.fetch_missing_avatars(&members);
                self.session.members = members
                    .into_iter()
                    .map(|mut member| {
                        member.username = safe_name(&member.username);
                        member
                    })
                    .collect();
                // La couleur de chaque auteur est résolue **ici**, une fois
                // par roster, et plus dans le fil à chaque message et à
                // chaque image. Changer un rôle recolore donc toujours les
                // anciens messages : le serveur rediffuse la liste.
                self.author_colors = self.session
                    .members
                    .iter()
                    .map(|m| (m.user_id, couleur_de_membre(m)))
                    .collect();
                self.after_roster_change();
            }
            ServerMsg::MemberUpdate { member } => {
                // Une seule fiche a changé : on la fusionne au lieu de
                // remplacer la liste. Le serveur envoyait le roster entier —
                // tous les comptes non bannis, pas seulement les connectés —
                // à chaque entrée et sortie de vocal.
                let mut member = member;
                member.username = safe_name(&member.username);
                self.fetch_missing_avatars(std::slice::from_ref(&member));
                self.author_colors.insert(member.user_id, couleur_de_membre(&member));
                match self.session.members.iter_mut().find(|m| m.user_id == member.user_id) {
                    Some(place) => *place = member,
                    None => {
                        // Nouveau venu : inséré à sa place, la liste étant
                        // triée par pseudo comme le serveur la produit. La
                        // retrier entièrement ferait le travail qu'on vient
                        // d'éviter.
                        let cle = member.username.to_lowercase();
                        let pos = self.session
                            .members
                            .partition_point(|m| m.username.to_lowercase() < cle);
                        self.session.members.insert(pos, member);
                    }
                }
                self.after_roster_change();
            }
            ServerMsg::UserJoined { user_id, .. } => {
                // La liste complète suit immédiatement dans un `Members` :
                // ici on ne fait que réclamer la photo du nouveau venu.
                if !self.avatars.contains_key(&user_id) {
                    self.send(ClientMsg::RequestAvatars { user_ids: vec![user_id] });
                }
            }
            ServerMsg::UserLeft { user_id } => {
                self.session.members.retain(|m| m.user_id != user_id);
            }
            ServerMsg::VoiceState { user_id, speaking, muted } => {
                if let Some(m) = self.session.members.iter_mut().find(|m| m.user_id == user_id) {
                    m.speaking = speaking;
                    m.muted = muted;
                }
            }
            // L'aiguillage de `handle_server_msg` n'envoie ici que ce qui précède.
            autre => debug_assert!(false, "message mal aiguillé : {autre:?}"),
        }
    }

    /// Les partages d'écran : le sien, ceux qu'on regarde.
    fn recu_streams(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::StreamGranted { stream_id } => {
                self.diffusion.palier = None;
                self.diffusion.montant = false;
                self.diffusion.basse = None;
                self.diffusion_accordee(stream_id);
            }
            ServerMsg::StreamStarted { stream_id, user_id, .. } => {
                let mut nom = String::new();
                let mut son_vocal = None;
                if let Some(m) = self.session.members.iter_mut().find(|m| m.user_id == user_id) {
                    m.streaming = Some(stream_id);
                    nom = m.username.clone();
                    son_vocal = m.voice;
                }
                if Some(user_id) != self.session.my_id && !nom.is_empty() {
                    self.info = Some(format!(
                        "{nom} diffuse son écran — clique sur l'icône à côté de son nom pour regarder"
                    ));
                    // Le bandeau ne se voit que fenêtre ouverte : quand le
                    // stream part dans MON salon vocal — le seul dont on
                    // peut regarder les streams —, un son, et l'overlay le
                    // dit par-dessus le jeu. Le serveur n'annonce qu'au
                    // départ : un changement de réglage du streamer passe
                    // par `StreamMetaChanged`, sans refaire sonner.
                    if son_vocal.is_some() && son_vocal == self.session.voice_channel {
                        self.play_sfx(sfx::DIFFUSION);
                        self.overlay.annoncer(format!("{nom} diffuse son écran"));
                    }
                }
            }
            ServerMsg::StreamStopped { stream_id } => {
                self.diffusion.spectateurs.remove(&stream_id);
                for m in self.session.members.iter_mut() {
                    if m.streaming == Some(stream_id) {
                        m.streaming = None;
                    }
                }
                // Si c'est ce qu'on regardait : rideau, sans Unwatch — le
                // stream n'existe plus, il n'y a rien à quitter.
                if self.regard.flux.as_ref().map(|r| r.stream_id) == Some(stream_id) {
                    self.fermer_regard(false);
                }
                // Et si c'est le nôtre (arrêté par le serveur : sortie du
                // vocal, par exemple), la capture locale s'arrête aussi.
                if self.diffusion.live.as_ref().map(|g| g.stream_id) == Some(stream_id) {
                    ki_voice::journal(format!("diffusion arrêtée par le serveur (stream {stream_id})"));
                    if let Some(g) = self.diffusion.live.take() {
                        g.arreter();
                    }
                    self.diffusion.texture = None;
                    // Un arrêt en bon ordre : le marqueur de plantage tombe,
                    // sans quoi le lancement suivant croyait à une fin
                    // brutale et passait l'encodeur en logiciel.
                    secours::lever_diffusion();
                }
            }
            // Le serveur nous retire d'un public (changement de salon,
            // déplacement, accès perdu) : la fenêtre se ferme au lieu de
            // rester figée sur la dernière image.
            ServerMsg::WatchDenied { stream_id, reason }
                if self.regard.flux.as_ref().map(|r| r.stream_id) == Some(stream_id) =>
            {
                ki_voice::journal(format!("visionnage interrompu (stream {stream_id}) : {reason}"));
                self.fermer_regard(false);
                self.info = Some(format!("visionnage interrompu : {reason}"));
            }
            ServerMsg::WatchAccepted { stream_id, stream_key, meta } => {
                ki_voice::journal(format!("visionnage accepté (stream {stream_id})"));
                let cle = ki_protocol::hex_decode(&stream_key).unwrap_or_default();
                let meme_machine =
                    meta.machine != 0 && meta.machine == partage::empreinte_machine(&cle);
                self.regard_accepte(stream_id, &stream_key, meme_machine);
            }
            ServerMsg::WatchDenied { reason, .. } => {
                ki_voice::journal(format!("visionnage refusé : {reason}"));
                self.info = Some(format!("impossible de regarder : {reason}"));
            }
            ServerMsg::KeyframeNeeded { stream_id, basse } => {
                if let Some(g) = &self.diffusion.live {
                    if g.stream_id == stream_id && basse {
                        g.qualites.force_keyframe_basse();
                    } else if g.stream_id == stream_id {
                        g.boucle.force_keyframe();
                    }
                }
            }
            ServerMsg::StreamMetaChanged { .. } => {}
            ServerMsg::StreamViewers { stream_id, viewers } => {
                self.diffusion.spectateurs.insert(stream_id, viewers);
            }
            ServerMsg::StreamBudget { stream_id, kbps, basse, montant } => {
                if self.diffusion.live.as_ref().is_some_and(|g| g.stream_id == stream_id) {
                    self.budget_de_diffusion(kbps, basse, montant);
                }
            }
            // L'aiguillage de `handle_server_msg` n'envoie ici que ce qui précède.
            autre => debug_assert!(false, "message mal aiguillé : {autre:?}"),
        }
    }

    /// VALORANT, la musique, les cartes, la carte tactique.
    fn recu_jeux(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::LiaisonRiot { ok, message, .. } => {
                self.donnees_valo.riot_message = Some((ok, message));
            }
            ServerMsg::MesComptes { mut comptes } => {
                comptes.truncate(MES_COMPTES_MAX);
                self.donnees_valo.mes_comptes = comptes;
            }
            ServerMsg::FicheValorant { user_id, fiche } => {
                // Une fiche, lue de plusieurs endroits et d'une fenêtre à
                // part : partagée, pas copiée.
                let fiche = fiche.map(std::sync::Arc::new);
                if Some(user_id) == self.session.my_id {
                    self.donnees_valo.ma_fiche = fiche.clone();
                }
                if let Some(c) = self.donnees_valo.comparee.as_mut().filter(|c| c.user_id == user_id) {
                    c.recue = true;
                    c.fiche = fiche.clone();
                }
                if let Some(f) = &mut self.donnees_valo.fiche {
                    if f.user_id == user_id {
                        f.recue = true;
                        f.fiche = fiche;
                    }
                }
            }
            ServerMsg::MusiqueEtat { etat } => {
                self.musique.etat = etat;
                self.musique.recu = std::time::Instant::now();
            }
            ServerMsg::MusiqueResultats { texte, pistes } => {
                self.musique.resultats = pistes;
                self.musique.resultats_pour = texte;
            }
            ServerMsg::Cartes { table } => {
                let sons = self.cartes.lock().unwrap().recevoir(table, self.session.my_id);
                for son in sons {
                    self.play_sfx(match son {
                        cartes::Son::MonTour => sfx::CARTES_TOUR,
                        cartes::Son::FinDeManche => sfx::CARTES_FIN,
                        cartes::Son::Annonce => sfx::CARTES_DERNIERE,
                        cartes::Son::Contre => sfx::CARTES_CONTRE,
                        cartes::Son::Echange => sfx::CARTES_ECHANGE,
                    });
                }
            }
            ServerMsg::CartesRefus { message } => {
                self.cartes.lock().unwrap().refuser(ki_protocol::safe_display(&message, 200));
            }
            ServerMsg::TactiqueEtat { carte } => self.tactique.lock().unwrap().recevoir_etat(carte),
            ServerMsg::Tactique { evenement } => {
                let membres = &self.session.members;
                let couleur = |id: UserId| membres.iter().find(|m| m.user_id == id).map_or(TEXT_DIM, couleur_de_membre);
                self.tactique.lock().unwrap().recevoir(evenement, self.session.my_id.unwrap_or(0), &couleur);
            }
            ServerMsg::TactiqueRefus { message } => {
                self.tactique.lock().unwrap().refuser(ki_protocol::safe_display(&message, 200));
            }
            ServerMsg::MonJournal { jours } => {
                self.donnees_valo.mon_journal = Some(std::sync::Arc::new(jours));
            }
            ServerMsg::Soiree { depuis, parties } => {
                // Une réponse d'avant 6 h du matin ne remplace pas la soirée.
                if depuis == self.donnees_valo.soiree_depuis {
                    self.donnees_valo.soiree = std::sync::Arc::new(parties);
                }
            }
            ServerMsg::MatchsDuGroupe { depuis, apres, parties, suite } => {
                // Une page : la suivante se demande, jusqu'à la dernière.
                let suivante = self.donnees_valo.matchs.recevoir(depuis, apres, parties, suite, std::time::Instant::now());
                if let Some((depuis, apres)) = suivante {
                    self.send(ClientMsg::MatchsValorant { depuis, apres });
                }
            }
            ServerMsg::ValorantChange => self.fiches_valorant_changees(),
            ServerMsg::StatsValorant { fiches, esports, activite } => {
                self.donnees_valo.stats = std::sync::Arc::new(fiches);
                self.donnees_valo.esports = std::sync::Arc::new(esports);
                self.donnees_valo.activite = std::sync::Arc::new(activite);
                self.donnees_valo.stats_recu = true;
                self.donnees_valo.stats_demandees = None;
            }
            // L'aiguillage de `handle_server_msg` n'envoie ici que ce qui précède.
            autre => debug_assert!(false, "message mal aiguillé : {autre:?}"),
        }
    }

    /// Ce que le serveur dit de lui-même et de nos droits : pokes, erreurs, expulsion,
    /// administration, rôles, salons, avatars, qualité du lien.
    fn recu_serveur(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::Poke { user_id: _, username } => {
                // Quelqu'un me veut : son, clignotement insistant (jusqu'au
                // retour du focus), bandeau dans le salon, et une ligne
                // par-dessus le jeu pour qui est alt-tabbé. Ça ne dépend
                // pas du réglage « Notifications » : un poke est un appel
                // personnel, et c'est « Accepter les pokes » qui le coupe.
                let username = ki_protocol::safe_display(&username, 64);
                self.play_sfx(sfx::POKE);
                self.wants_attention = true;
                self.attention_critique = true;
                self.overlay.annoncer(format!("{username} te poke"));
                if self.zone.reduite() {
                    self.zone_poke = Some(username.clone());
                }
                self.poke_recu = Some((username, std::time::Instant::now()));
            }
            ServerMsg::PokeRefuse { user_id, message } => {
                // Le motif vient du serveur, en toutes lettres ; la cible
                // reste grisée un moment, le temps de le lire.
                self.error = Some(ki_protocol::safe_display(&message, 300));
                self.pokes_refuses.insert(user_id, std::time::Instant::now());
            }
            ServerMsg::Error { message } => {
                let message = ki_protocol::safe_display(&message, 300);
                // Avant le Welcome, une erreur = échec de connexion (jeton...).
                if !self.session.welcomed {
                    self.disconnect(Some(message));
                } else {
                    // Rien n'est défait ici. Une erreur n'est pas rattachable
                    // à la demande qui l'a provoquée — « tu écris trop vite »
                    // arrive dans la même fenêtre qu'une entrée en vocal — et
                    // s'en servir pour sortir du salon éjectait sur une erreur
                    // sans rapport. Si le serveur a refusé l'entrée, il ne
                    // nous listera pas dedans : la fenêtre d'intention expire,
                    // et sa liste des membres nous remet d'aplomb.
                    //
                    // Une exception, qui ne défait rien : ce qu'on vient
                    // d'envoyer et qui n'est pas revenu retourne dans le
                    // champ. Si l'erreur parlait d'autre chose, l'écho
                    // arrivera et le champ se revidera (`envoi_confirme`).
                    self.rendre_l_envoi_refuse();
                    self.error = Some(message);
                }
            }
            ServerMsg::Kicked { reason } => {
                // Le motif vient d'un autre membre : il passe par le même
                // nettoyage que n'importe quel texte reçu avant affichage.
                let reason = ki_protocol::safe_display(&reason, 300);
                self.disconnect(Some(if reason.is_empty() {
                    "tu as été expulsé par un admin".into()
                } else {
                    format!("expulsé par un admin : {reason}")
                }));
            }
            ServerMsg::AdminInfo { users, invites } => {
                self.admin.comptes = users;
                self.admin.invitations = invites;
            }
            ServerMsg::AuditLog { records } => {
                self.admin.audit = records;
            }
            ServerMsg::Perms { perms, rank, .. } => {
                // Pas de repli sur DEFAULT ici, contrairement au Welcome : ce
                // message n'existe que sur un serveur qui connaît les rôles,
                // et `perms == 0` y est un état légitime — celui de qui vient
                // de tout se faire retirer. S'accorder DEFAULT reviendrait à
                // afficher des actions que le serveur refuse.
                self.session.my_perms = perms;
                self.session.my_rank = rank;
                // Un panneau ouvert sur des actions qu'on vient de perdre
                // n'aboutirait plus : on le referme plutôt que de le laisser
                // proposer des boutons qui échouent.
                if !self.any_admin_power() {
                    self.close_admin();
                }
            }
            ServerMsg::Roles { roles } => {
                self.session.roles = roles;
            }
            ServerMsg::ChannelsUpdated { channels } => {
                self.session.channels = channels;
                // Un salon supprimé emporte ce qu'on y écrivait.
                let salons: Vec<ChannelId> = self.session.channels.iter().map(|c| c.id).collect();
                self.brouillons.garder_seulement(|salon| salons.contains(&salon));
                // Le salon lu a pu disparaître, ou m'être retiré : on se
                // rabat sur le premier salon textuel encore visible plutôt
                // que de laisser une vue morte.
                let still_there =
                    self.session.current.is_some_and(|c| self.session.channels.iter().any(|ch| ch.id == c));
                if !still_there {
                    // Un salon ordinaire d'abord : un salon temporaire
                    // disparaîtra à son tour, et l'on retomberait dedans.
                    let first = self.session
                        .channels
                        .iter()
                        .find(|c| c.kind == ChannelKind::Text && c.expire_le.is_none())
                        .or_else(|| self.session.channels.iter().find(|c| c.kind == ChannelKind::Text))
                        .map(|c| c.id);
                    match first {
                        Some(id) => self.join(id),
                        None => {
                            self.session.current = None;
                            self.session.messages.clear();
                        }
                    }
                }
                // Idem pour le vocal : on ne reste pas dans un salon qu'on
                // ne voit plus, sinon on parle à des gens qu'on ne voit pas.
                if self.session.voice_channel.is_some_and(|c| !self.session.channels.iter().any(|ch| ch.id == c))
                {
                    self.session.voice_channel = None;
                    self.vocal.armed = false;
                    self.vocal.transmitting = false;
                    self.info = Some("le salon vocal a été fermé".into());
                }
            }
            ServerMsg::VoiceLocked { channel, wrong } => {
                self.session.voice_channel = None;
                self.session.voice_intent = None;
                self.mots_de_passe_vocaux.remove(&channel);
                self.voice_prompt = Some(VoicePrompt {
                    channel,
                    password: String::new(),
                    wrong,
                });
            }
            ServerMsg::InviteCreated { code } => {
                self.admin.derniere_invitation = Some(code);
            }
            ServerMsg::AdminFichiers { fichiers, total_octets, plafond_octets, tronque } => {
                self.admin.fichiers.recevoir(fichiers, total_octets, plafond_octets, tronque);
            }
            ServerMsg::Info { message } => {
                self.info = Some(ki_protocol::safe_display(&message, 300));
            }
            ServerMsg::ServerInfo { server } => {
                // Un admin vient de changer le nom ou le logo : tout le
                // monde le reçoit et le carnet se met à jour.
                self.server_info = safe_server_info(server);
                self.cache_server_identity();
                if !self.admin.ouverte {
                    self.admin.nom = self.server_info.name.clone();
                    self.admin.adresse_web = self.server_info.adresse_web.clone();
                }
            }
            ServerMsg::Avatar { user_id, hash, data } => match data {
                // Le montage en texture demande le contexte egui : la
                // vignette attend ici, `mount_avatars` la reprendra au
                // prochain rendu. Elle part aussi au cache disque, pour ne
                // plus jamais la retélécharger tant qu'elle ne change pas.
                Some(data) => {
                    // On ne fait pas confiance au serveur non plus : la
                    // vignette est contrôlée avant d'atterrir sur le disque.
                    match servers::decode_base64(&data)
                        .filter(|png| ki_protocol::check_png(png).is_ok())
                    {
                        Some(png) => {
                            photos::store(&hash, &png);
                            self.incoming_avatars.insert(user_id, (hash, png));
                        }
                        None => tracing::warn!("photo de {user_id} refusée : vignette invalide"),
                    }
                }
                None => {
                    self.avatars.remove(&user_id);
                    self.incoming_avatars.remove(&user_id);
                }
            },
            ServerMsg::NetQuality { loss_pct } => {
                self.on_net_quality(loss_pct);
            }
            ServerMsg::Pong => {}
            // L'aiguillage de `handle_server_msg` n'envoie ici que ce qui précède.
            autre => debug_assert!(false, "message mal aiguillé : {autre:?}"),
        }
    }

    /// Claude : sa conversation, ses étapes, ses chiffres.
    fn recu_claude(&mut self, msg: ServerMsg) {
        match msg {
            // --- Claude ---
            ServerMsg::ClaudeConversation { de, messages, etat } => {
                self.claude.sur_conversation(self.session.my_id, de, messages, etat);
            }
            ServerMsg::ClaudeMessage { message, etat } => self.claude.sur_message(message, etat),
            ServerMsg::ClaudeEtape { texte } => self.claude.sur_etape(texte),
            ServerMsg::ClaudeErreur { message, etat } => self.claude.sur_erreur(message, etat),
            ServerMsg::ClaudeMembres { membres, modele, par_jour, modeles } => {
                self.claude.sur_membres(membres, modele, par_jour, modeles);
            }
            // L'aiguillage de `handle_server_msg` n'envoie ici que ce qui précède.
            autre => debug_assert!(false, "message mal aiguillé : {autre:?}"),
        }
    }

    /// Les portes web : ouvertures, demandes d'entrée, fermetures.
    fn recu_portes(&mut self, msg: ServerMsg) {
        match msg {
            // --- Les portes web ---
            ServerMsg::PorteOuverte { slug, url, salon, expire_le } => {
                // Ma porte : le lien et son QR dans le panneau, et le
                // salon temporaire sous les yeux — il arrive par
                // `ChannelsUpdated`, avant ou après, peu importe : lire
                // un salon ne demande que son identifiant.
                // Complet, `https://` compris : un chemin seul (le serveur ne
                // connaît pas son adresse publique) se complète ici.
                let url = porte_ui::lien_complet(&self.base_web(), &ki_protocol::safe_display(&url, 300));
                self.portes.ouverte(&slug, url, salon, expire_le);
                self.join(salon);
            }
            ServerMsg::PorteDemande { slug, demande_id, nom, ip_masquee } => {
                // Quelqu'un frappe : bannière en tête du salon, son à
                // part, clignotement insistant, une ligne par-dessus le
                // jeu. Comme un poke : c'est un appel, pas un message.
                let nom = ki_protocol::safe_display(&nom, 64);
                let slug = ki_protocol::safe_display(&slug, ki_protocol::PORTE_SLUG_MAX);
                if self.portes.demande(&slug, demande_id, &nom, &ip_masquee) {
                    self.play_sfx(sfx::PORTE);
                    self.wants_attention = true;
                    self.attention_critique = true;
                    self.overlay.annoncer(format!("{nom} frappe à la porte {slug}"));
                }
            }
            ServerMsg::PorteEtat { slug, salon, invites, demandes, expire_le, url } => {
                let invites = invites
                    .into_iter()
                    .map(|mut i| {
                        i.nom = ki_protocol::safe_display(&i.nom, 64);
                        i
                    })
                    .collect();
                let demandes = demandes
                    .into_iter()
                    .map(|mut d| {
                        d.nom = ki_protocol::safe_display(&d.nom, 64);
                        d
                    })
                    .collect();
                // Vide chez un serveur d'avant 0.1.45 : le lien reçu à
                // l'ouverture reste alors celui qu'on montre.
                let url = (!url.is_empty())
                    .then(|| porte_ui::lien_complet(&self.base_web(), &ki_protocol::safe_display(&url, 300)));
                self.portes.etat(&slug, salon, invites, demandes, expire_le, url);
            }
            ServerMsg::PorteFermee { slug, motif } => {
                let motif = ki_protocol::safe_display(&motif, ki_protocol::MAX_PORTE_MOTIF);
                self.portes.fermee(&slug, &motif);
                // Le salon disparaît par `ChannelsUpdated` ; ici on dit
                // seulement pourquoi.
                self.info = Some(if motif.is_empty() {
                    format!("porte « {slug} » fermée")
                } else {
                    format!("porte « {slug} » fermée : {motif}")
                });
            }
            // À l'invité web seulement, par sa porte : un client ki-chat
            // ne le reçoit jamais.
            ServerMsg::PorteInvitation { .. } => {}
            // L'aiguillage de `handle_server_msg` n'envoie ici que ce qui précède.
            autre => debug_assert!(false, "message mal aiguillé : {autre:?}"),
        }
    }
}
