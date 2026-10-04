//! L'état du client tel que le serveur le décrit : salons, membres, fil du
//! salon ouvert, non-lus, vocal, permissions.
//!
//! Sans interface. [`Etat::appliquer`] tient l'état à jour d'après chaque
//! [`ServerMsg`] et rend les [`Effet`]s que l'interface doit produire (un son,
//! une bannière, une notification) : c'est elle qui sait comment les rendre.
//! Les gestes de l'utilisateur (ouvrir un salon, entrer en vocal) passent par
//! les méthodes de [`Etat`], qui rendent les messages à envoyer au serveur.
//!
//! Les règles reprennent celles du client PC (`handle_server_msg` de
//! client-gui), qui n'utilise pas encore ce module.

use std::collections::HashMap;

use ki_protocol::{
    ChannelId, ChannelInfo, ChannelKind, ChatRecord, ClientMsg, Member, Perms, Reaction,
    ReplyRef, RoleInfo, ServerInfo, ServerMsg, UserId,
};

/// Au-delà, les plus anciens messages du fil sont oubliés : on les retrouve
/// en remontant l'historique.
pub const MESSAGES_MAX: usize = 500;

/// Les messages demandés à l'ouverture d'un salon.
pub const HISTORIQUE_PREMIERE_PAGE: u32 = 100;

/// Ce qu'on n'a pas lu dans un salon : de quoi peindre sa pastille.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NonLu {
    /// Messages non lus. Le serveur compte jusqu'à mille au plus.
    pub nb: u32,
    /// L'un d'eux me nomme.
    pub mention: bool,
    /// Tout message d'horodatage **supérieur** est non lu : c'est là que se
    /// pose le séparateur « nouveaux messages » à l'entrée du salon.
    pub depuis: u64,
}

impl NonLu {
    /// Un message de plus. Le premier non-lu fixe l'endroit du séparateur.
    pub fn ajouter(&mut self, ts: u64, mention: bool) {
        if self.nb == 0 {
            self.depuis = ts.saturating_sub(1);
        }
        self.nb = self.nb.saturating_add(1);
        self.mention |= mention;
    }
}

/// Ce que l'interface doit faire après un message du serveur.
#[derive(Debug, Clone)]
pub enum Effet {
    /// Un message mérite qu'on prévienne (son, vibration, notification).
    /// `mention` : il me nomme, ce qui appelle une réponse.
    Prevenir { salon: ChannelId, mention: bool },
    /// À envoyer au serveur (entrée dans un salon, historique…).
    Envoyer(ClientMsg),
    /// Une information à montrer en passant.
    Info(String),
    /// Une erreur à montrer.
    Erreur(String),
    /// Quelqu'un me poke.
    Poke(String),
    /// Le salon vocal est verrouillé : demander son mot de passe.
    MotDePasseVocal { salon: ChannelId, faux: bool },
    /// La session est finie, et le serveur dit pourquoi (refus avant
    /// l'accueil, expulsion). Pas de reconnexion automatique.
    Fin(String),
}

/// L'état du client pour une connexion.
#[derive(Debug, Clone, Default)]
pub struct Etat {
    /// L'appareil de cette connexion : un compte peut être ouvert à la fois
    /// sur un PC et sur un téléphone, avec une seule voix.
    pub appareil: ki_protocol::Appareil,
    /// Accueilli par le serveur : avant, une erreur est un refus.
    pub accueilli: bool,
    pub moi: Option<UserId>,
    pub perms: Perms,
    pub rang: u16,
    pub roles: Vec<RoleInfo>,
    pub salons: Vec<ChannelInfo>,
    pub serveur: ServerInfo,
    /// Le jeton de session HTTP (partage de fichiers, images), en hexadécimal.
    pub jeton_http: String,
    /// Tous les comptes non bannis, triés par pseudo ; `online` dit qui est
    /// là.
    pub membres: Vec<Member>,
    /// Le salon textuel ouvert, et son fil.
    pub courant: Option<ChannelId>,
    pub messages: Vec<ChatRecord>,
    /// Reste-t-il du passé à remonter dans le salon ouvert ?
    pub historique_suite: bool,
    pub historique_en_cours: bool,
    /// Le repère « nouveaux messages » du salon ouvert.
    pub separateur: Option<u64>,
    pub non_lus: HashMap<ChannelId, NonLu>,
    /// Le serveur tient les lus : on lui dit ce qu'on lit.
    pub serveur_gere_lus: bool,
    /// Le salon vocal où le serveur nous liste, **depuis cet appareil**.
    pub vocal: Option<ChannelId>,
    /// L'interface montre le fil du salon ouvert, à jour : appli au premier
    /// plan, fil en bas. Un message qui y arrive est alors lu, pas non lu.
    /// Tenu par l'interface.
    pub regarde: bool,
    /// Le serveur parle un protocole plus récent que le nôtre.
    pub serveur_plus_recent: bool,
}

/// Ce qui vient du serveur lui-même : le fil de jeu (identifiant 0) et le bot
/// musique. Jamais un compte.
pub fn est_bot(user_id: UserId) -> bool {
    user_id == 0 || user_id == ki_protocol::MUSIQUE_ID
}

/// Pseudo affichable : sans caractères dangereux et de longueur bornée.
pub fn nom_sur(username: &str) -> String {
    ki_protocol::safe_display(username, ki_protocol::MAX_USERNAME)
}

/// Un message reçu, nettoyé pour l'affichage.
pub fn message_sur(mut record: ChatRecord) -> ChatRecord {
    record.username = nom_sur(&record.username);
    record.text = ki_protocol::safe_display(&record.text, ki_protocol::MAX_CHAT_TEXT);
    record.reply_to = record.reply_to.map(reponse_sure);
    record.reactions.retain(|r| ki_protocol::clean_emoji(&r.emoji).is_some());
    record
}

/// Le rappel d'une réponse, composé par le serveur d'après autrui.
pub fn reponse_sure(mut r: ReplyRef) -> ReplyRef {
    r.username = nom_sur(&r.username);
    r.excerpt = ki_protocol::safe_display(&r.excerpt, ki_protocol::MAX_EXCERPT + 1);
    r
}

/// L'identité du serveur, affichable.
pub fn serveur_sur(mut server: ServerInfo) -> ServerInfo {
    server.name = ki_protocol::safe_display(&server.name, ki_protocol::MAX_SERVER_NAME);
    server.adresse_web =
        ki_protocol::normaliser_adresse_web(&server.adresse_web).unwrap_or_default();
    server
}

/// Applique une réaction posée ou retirée, avec la règle du serveur.
pub fn reaction_locale(reactions: &mut Vec<Reaction>, emoji: &str, by: UserId, on: bool) {
    match reactions.iter().position(|r| r.emoji == emoji) {
        Some(i) => {
            let r = &mut reactions[i];
            if on {
                if !r.users.contains(&by) {
                    r.users.push(by);
                }
            } else {
                r.users.retain(|u| *u != by);
                if r.users.is_empty() {
                    reactions.remove(i);
                }
            }
        }
        None if on => reactions.push(Reaction { emoji: emoji.to_string(), users: vec![by] }),
        None => {}
    }
}

impl Etat {
    /// Mon pseudo, d'après la liste des membres.
    pub fn mon_pseudo(&self) -> Option<&str> {
        let moi = self.moi?;
        self.membres.iter().find(|m| m.user_id == moi).map(|m| m.username.as_str())
    }

    /// Ce message me nomme-t-il ?
    pub fn me_nomme(&self, user_id: UserId, texte: &str) -> bool {
        Some(user_id) != self.moi
            && !est_bot(user_id)
            && self.mon_pseudo().is_some_and(|moi| {
                let membres: Vec<&str> = self.membres.iter().map(|m| m.username.as_str()).collect();
                crate::markup::me_mentionne(texte, &membres, moi)
            })
    }

    /// Ai-je cette permission ?
    pub fn peut(&self, besoin: Perms) -> bool {
        ki_protocol::perm::has(self.perms, besoin)
    }

    /// Le total des non-lus.
    pub fn total_non_lus(&self) -> u32 {
        self.non_lus.values().fold(0u32, |n, v| n.saturating_add(v.nb))
    }

    /// Le salon ouvert d'office : un salon textuel ordinaire d'abord, un
    /// temporaire sinon (il disparaîtra, et l'on retomberait dedans).
    pub fn premier_salon_texte(&self) -> Option<ChannelId> {
        self.salons
            .iter()
            .find(|c| c.kind == ChannelKind::Text && c.expire_le.is_none())
            .or_else(|| self.salons.iter().find(|c| c.kind == ChannelKind::Text))
            .map(|c| c.id)
    }

    /// Ouvre un salon textuel : on change ce qu'on lit, rien d'autre. Rend
    /// les messages à envoyer.
    pub fn ouvrir_salon(&mut self, salon: ChannelId) -> Vec<ClientMsg> {
        let meme = self.courant == Some(salon);
        self.courant = Some(salon);
        self.messages.clear();
        // Il y a un passé à remonter jusqu'à preuve du contraire : la
        // première page le dira.
        self.historique_suite = true;
        self.historique_en_cours = false;
        let repere = self.non_lus.remove(&salon).filter(|n| n.nb > 0).map(|n| n.depuis);
        self.separateur = if meme { repere.or(self.separateur) } else { repere };
        vec![ClientMsg::Join { channel: salon }, ClientMsg::History { limit: HISTORIQUE_PREMIERE_PAGE }]
    }

    /// Demande la page d'historique précédente du salon ouvert.
    pub fn remonter(&mut self) -> Option<ClientMsg> {
        if !self.historique_suite || self.historique_en_cours {
            return None;
        }
        let avant = self.messages.first()?.ts;
        self.historique_en_cours = true;
        Some(ClientMsg::HistoryBefore { before_ts: avant, limit: 50, channel: self.courant.unwrap_or(0) })
    }

    /// Tient le salon pour lu : la pastille tombe, et le serveur l'apprend
    /// s'il tient les lus (un serveur antérieur répondrait « message
    /// invalide »).
    pub fn marquer_lu(&mut self, salon: ChannelId) -> Option<ClientMsg> {
        self.non_lus.remove(&salon);
        if !self.serveur_gere_lus {
            return None;
        }
        let ts = if self.courant == Some(salon) {
            self.messages.iter().map(|m| m.ts).max().unwrap_or(u64::MAX)
        } else {
            u64::MAX
        };
        Some(ClientMsg::Lu { channel: salon, ts })
    }

    /// Entre dans un salon vocal. Le serveur refuserait sans la permission :
    /// on le dit tout de suite. Le salon ne compte qu'une fois le serveur
    /// nous y liste ([`Etat::vocal`]).
    pub fn rejoindre_vocal(
        &self,
        salon: ChannelId,
        mot_de_passe: Option<String>,
    ) -> Result<ClientMsg, String> {
        if !self.peut(ki_protocol::perm::CONNECT_VOICE) {
            return Err("tu n'as pas le droit de rejoindre le vocal".into());
        }
        Ok(ClientMsg::JoinVoice { channel: salon, password: mot_de_passe })
    }

    fn compter_non_lu(&mut self, salon: ChannelId, ts: u64, mention: bool) {
        self.non_lus.entry(salon).or_default().ajouter(ts, mention);
    }

    /// Le vocal suit la liste des membres : c'est elle qui fait foi. Ma fiche
    /// dit `mobile` quand ma voix passe par le téléphone : elle n'est à cet
    /// appareil que s'il en est un.
    fn suivre_vocal(&mut self) {
        let sur_mobile = self.appareil == ki_protocol::Appareil::Mobile;
        if let Some(moi) = self.moi {
            if let Some(m) = self.membres.iter().find(|m| m.user_id == moi) {
                self.vocal = m.voice.filter(|_| m.mobile == sur_mobile);
            }
        }
    }

    /// Tient l'état à jour d'après un message du serveur.
    pub fn appliquer(&mut self, msg: ServerMsg) -> Vec<Effet> {
        let mut effets = Vec::new();
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
                protocole,
                ..
            } => {
                self.accueilli = true;
                self.moi = Some(user_id);
                self.serveur_plus_recent = protocole > ki_protocol::PROTOCOLE;
                if self.serveur_plus_recent {
                    effets.push(Effet::Info(
                        "ce serveur est plus récent que ton ki-chat — installe la dernière version"
                            .into(),
                    ));
                }
                // Un serveur antérieur aux rôles ne dit que `is_admin` : on
                // lui accorde tout plutôt que rien à son admin.
                self.perms = if perms == 0 && is_admin {
                    ki_protocol::perm::ADMINISTRATOR
                } else if perms == 0 {
                    ki_protocol::perm::DEFAULT
                } else {
                    perms
                };
                self.rang = rank;
                self.roles = roles;
                self.jeton_http = format!("{voice_token:x}");
                self.salons = channels;
                self.serveur = serveur_sur(server);
                // On ouvre un salon pour avoir de quoi lire (celui d'avant
                // une reconnexion s'il existe encore), mais aucun vocal :
                // ça se décide.
                let salon = self
                    .courant
                    .filter(|c| self.salons.iter().any(|k| k.id == *c))
                    .or_else(|| self.premier_salon_texte());
                match salon {
                    Some(c) => effets.extend(self.ouvrir_salon(c).into_iter().map(Effet::Envoyer)),
                    None => {
                        self.courant = None;
                        self.messages.clear();
                    }
                }
            }
            ServerMsg::Chat { user_id, username, text, ts, reply_to, channel } => {
                let de_moi = Some(user_id) == self.moi;
                // Écrit dans le salon qu'on vient de quitter, croisé avec
                // notre `Join` : il compte comme un `Nouveau`. `0` = serveur
                // antérieur, qui ne le dit pas.
                if channel != 0 && self.courant != Some(channel) {
                    if !de_moi {
                        let mention = self.me_nomme(user_id, &text);
                        self.compter_non_lu(channel, ts, mention);
                        if !est_bot(user_id) {
                            effets.push(Effet::Prevenir { salon: channel, mention });
                        }
                    }
                    return effets;
                }
                let mention = self.me_nomme(user_id, &text);
                if let Some(c) = self.courant {
                    if !de_moi && !self.regarde {
                        self.compter_non_lu(c, ts, mention);
                    }
                    // Pas de son pour soi ni pour le serveur ; pour les
                    // autres, quand on ne regarde pas, ou quand on est nommé.
                    if !de_moi && !est_bot(user_id) && (mention || !self.regarde) {
                        effets.push(Effet::Prevenir { salon: c, mention });
                    }
                }
                self.messages.push(message_sur(ChatRecord {
                    user_id,
                    username,
                    text,
                    ts,
                    reply_to,
                    reactions: Vec::new(),
                    edited: false,
                }));
                if self.messages.len() > MESSAGES_MAX {
                    self.messages.remove(0);
                }
            }
            ServerMsg::Nouveau { channel, user_id, text, ts, .. } => {
                if Some(user_id) == self.moi || self.courant == Some(channel) {
                    return effets;
                }
                let mention = self.me_nomme(user_id, &text);
                self.compter_non_lu(channel, ts, mention);
                if !est_bot(user_id) {
                    effets.push(Effet::Prevenir { salon: channel, mention });
                }
            }
            ServerMsg::NonLus { salons } => {
                // Ce que le serveur compte remplace ce qu'on croyait.
                self.serveur_gere_lus = true;
                for s in salons {
                    if s.non_lus == 0 {
                        self.non_lus.remove(&s.channel);
                    } else {
                        self.non_lus.insert(
                            s.channel,
                            NonLu { nb: s.non_lus, mention: s.mention, depuis: s.dernier_ts },
                        );
                    }
                }
                if let Some(c) = self.courant {
                    if let Some(n) = self.non_lus.get(&c) {
                        self.separateur = Some(n.depuis);
                    }
                }
            }
            ServerMsg::History { messages } => {
                // Une première page moins pleine que demandé : il n'y a rien
                // avant.
                if messages.len() < HISTORIQUE_PREMIERE_PAGE as usize {
                    self.historique_suite = false;
                }
                self.messages = messages.into_iter().map(message_sur).collect();
            }
            ServerMsg::HistoryPage { messages, more, channel } => {
                // Une page d'un autre salon a croisé un changement de salon.
                if channel != 0 && self.courant != Some(channel) {
                    return effets;
                }
                self.historique_en_cours = false;
                self.historique_suite = more;
                // En tête, sans doublon : des messages ont pu arriver entre la
                // demande et la réponse.
                let connus: std::collections::HashSet<(UserId, u64)> =
                    self.messages.iter().map(|m| (m.user_id, m.ts)).collect();
                let mut anciens: Vec<ChatRecord> = messages
                    .into_iter()
                    .map(message_sur)
                    .filter(|m| !connus.contains(&(m.user_id, m.ts)))
                    .collect();
                anciens.append(&mut self.messages);
                self.messages = anciens;
            }
            ServerMsg::Reaction { channel, message, emoji, by, on } => {
                if self.courant == Some(channel) {
                    if let Some(m) = self
                        .messages
                        .iter_mut()
                        .find(|m| m.user_id == message.user_id && m.ts == message.ts)
                    {
                        reaction_locale(&mut m.reactions, &emoji, by, on);
                    }
                }
            }
            ServerMsg::MessageDeleted { channel, message } => {
                if self.courant == Some(channel) {
                    self.messages.retain(|m| !(m.user_id == message.user_id && m.ts == message.ts));
                }
            }
            ServerMsg::MessageEdited { channel, message, text } => {
                if self.courant == Some(channel) {
                    let propre = ki_protocol::safe_display(&text, ki_protocol::MAX_CHAT_TEXT);
                    if let Some(m) = self
                        .messages
                        .iter_mut()
                        .find(|m| m.user_id == message.user_id && m.ts == message.ts)
                    {
                        m.text = propre;
                        m.edited = true;
                    }
                }
            }
            ServerMsg::Members { members } => {
                self.membres = members
                    .into_iter()
                    .map(|mut m| {
                        m.username = nom_sur(&m.username);
                        m
                    })
                    .collect();
                self.suivre_vocal();
            }
            ServerMsg::MemberUpdate { mut member } => {
                member.username = nom_sur(&member.username);
                match self.membres.iter_mut().find(|m| m.user_id == member.user_id) {
                    Some(place) => *place = member,
                    None => {
                        // À sa place : la liste est triée par pseudo, comme le
                        // serveur la produit.
                        let cle = member.username.to_lowercase();
                        let pos = self.membres.partition_point(|m| m.username.to_lowercase() < cle);
                        self.membres.insert(pos, member);
                    }
                }
                self.suivre_vocal();
            }
            ServerMsg::UserLeft { user_id } => {
                self.membres.retain(|m| m.user_id != user_id);
            }
            ServerMsg::VoiceState { user_id, speaking, muted } => {
                if let Some(m) = self.membres.iter_mut().find(|m| m.user_id == user_id) {
                    m.speaking = speaking;
                    m.muted = muted;
                }
            }
            ServerMsg::Perms { perms, rank, .. } => {
                // Pas de repli ici : `perms == 0` est l'état légitime de qui
                // vient de tout se faire retirer.
                self.perms = perms;
                self.rang = rank;
            }
            ServerMsg::Roles { roles } => self.roles = roles,
            ServerMsg::ChannelsUpdated { channels } => {
                self.salons = channels;
                // Le salon lu a pu disparaître ou m'être retiré.
                let encore = self.courant.is_some_and(|c| self.salons.iter().any(|k| k.id == c));
                if !encore {
                    match self.premier_salon_texte() {
                        Some(c) => {
                            effets.extend(self.ouvrir_salon(c).into_iter().map(Effet::Envoyer))
                        }
                        None => {
                            self.courant = None;
                            self.messages.clear();
                        }
                    }
                }
                if self.vocal.is_some_and(|c| !self.salons.iter().any(|k| k.id == c)) {
                    self.vocal = None;
                    effets.push(Effet::Info("le salon vocal a été fermé".into()));
                }
            }
            ServerMsg::ServerInfo { server } => self.serveur = serveur_sur(server),
            ServerMsg::VoiceLocked { channel, wrong } => {
                effets.push(Effet::MotDePasseVocal { salon: channel, faux: wrong });
            }
            ServerMsg::Poke { username, .. } => {
                effets.push(Effet::Poke(ki_protocol::safe_display(&username, 64)));
            }
            ServerMsg::PokeRefuse { message, .. } => {
                effets.push(Effet::Erreur(ki_protocol::safe_display(&message, 300)));
            }
            ServerMsg::Error { message } => {
                let message = ki_protocol::safe_display(&message, 300);
                // Avant l'accueil, une erreur est un refus de connexion.
                effets.push(if self.accueilli { Effet::Erreur(message) } else { Effet::Fin(message) });
            }
            ServerMsg::Info { message } => {
                effets.push(Effet::Info(ki_protocol::safe_display(&message, 300)));
            }
            ServerMsg::Kicked { reason } => {
                let reason = ki_protocol::safe_display(&reason, 300);
                effets.push(Effet::Fin(if reason.is_empty() {
                    "tu as été expulsé par un admin".into()
                } else {
                    format!("expulsé par un admin : {reason}")
                }));
            }
            // Le reste (streams, administration, VALORANT, musique, portes)
            // n'a pas encore sa place ici : chaque interface le traite.
            _ => {}
        }
        effets
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn membre(user_id: UserId, nom: &str, voice: Option<ChannelId>) -> Member {
        serde_json::from_value(serde_json::json!({
            "user_id": user_id, "username": nom, "speaking": false, "voice": voice,
        }))
        .unwrap()
    }

    fn salon(id: ChannelId, nom: &str, vocal: bool) -> ChannelInfo {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": nom, "kind": if vocal { "voice" } else { "text" },
        }))
        .unwrap()
    }

    fn accueilli() -> Etat {
        let mut e = Etat::default();
        let welcome: ServerMsg = serde_json::from_value(serde_json::json!({
            "type": "welcome", "user_id": 1, "voice_token": 255, "udp_port": 0,
            "voice_key": "00", "channels": [], "server": {},
        }))
        .unwrap_or_else(|err| panic!("Welcome de test : {err}"));
        e.appliquer(welcome);
        e.salons = vec![salon(10, "général", false), salon(11, "autre", false), salon(20, "vocal", true)];
        e.membres = vec![membre(2, "bob", None), membre(1, "moi", None)];
        e.ouvrir_salon(10);
        e
    }

    /// `ClientMsg` ne se compare pas : on compare ce qu'ils affichent.
    fn egal(a: &[Effet], b: &[Effet]) -> bool {
        format!("{a:?}") == format!("{b:?}")
    }

    fn contient(a: &[Effet], e: Effet) -> bool {
        let cherche = format!("{e:?}");
        a.iter().any(|x| format!("{x:?}") == cherche)
    }

    fn chat(user_id: UserId, texte: &str, ts: u64, channel: ChannelId) -> ServerMsg {
        ServerMsg::Chat {
            user_id,
            username: "bob".into(),
            text: texte.into(),
            ts,
            reply_to: None,
            channel,
        }
    }

    #[test]
    fn l_accueil_ouvre_le_premier_salon_texte() {
        let mut e = Etat::default();
        let welcome: ServerMsg = serde_json::from_value(serde_json::json!({
            "type": "welcome", "user_id": 7, "voice_token": 255, "udp_port": 0,
            "voice_key": "00", "server": {},
            "channels": [
                { "id": 3, "name": "vocal", "kind": "voice" },
                { "id": 4, "name": "général", "kind": "text" },
            ],
        }))
        .unwrap();
        let effets = e.appliquer(welcome);
        assert_eq!(e.moi, Some(7));
        assert_eq!(e.courant, Some(4));
        assert_eq!(e.jeton_http, "ff");
        assert!(contient(&effets, Effet::Envoyer(ClientMsg::Join { channel: 4 })));
    }

    #[test]
    fn un_message_regarde_ne_compte_pas_comme_non_lu() {
        let mut e = accueilli();
        e.regarde = true;
        let effets = e.appliquer(chat(2, "salut", 100, 10));
        assert_eq!(e.messages.len(), 1);
        assert!(e.non_lus.is_empty());
        assert!(effets.is_empty());
    }

    #[test]
    fn un_message_pas_regarde_compte_et_previent() {
        let mut e = accueilli();
        let effets = e.appliquer(chat(2, "salut", 100, 10));
        assert_eq!(e.non_lus[&10].nb, 1);
        assert_eq!(e.non_lus[&10].depuis, 99);
        assert!(egal(&effets, &[Effet::Prevenir { salon: 10, mention: false }]));
    }

    #[test]
    fn une_mention_previent_meme_en_regardant() {
        let mut e = accueilli();
        e.regarde = true;
        let effets = e.appliquer(chat(2, "@moi tu viens ?", 100, 10));
        assert!(egal(&effets, &[Effet::Prevenir { salon: 10, mention: true }]));
    }

    #[test]
    fn un_chat_croise_avec_un_changement_de_salon_devient_non_lu() {
        let mut e = accueilli();
        let effets = e.appliquer(chat(2, "salut", 100, 11));
        assert!(e.messages.is_empty());
        assert_eq!(e.non_lus[&11].nb, 1);
        assert_eq!(effets.len(), 1);
    }

    #[test]
    fn ses_propres_messages_ne_sont_jamais_non_lus() {
        let mut e = accueilli();
        let effets = e.appliquer(chat(1, "moi", 100, 10));
        assert!(e.non_lus.is_empty());
        assert!(effets.is_empty());
    }

    #[test]
    fn ouvrir_un_salon_fait_tomber_sa_pastille_et_pose_le_separateur() {
        let mut e = accueilli();
        e.appliquer(ServerMsg::Nouveau {
            channel: 11,
            user_id: 2,
            username: "bob".into(),
            text: "hé".into(),
            ts: 50,
        });
        assert_eq!(e.total_non_lus(), 1);
        e.ouvrir_salon(11);
        assert_eq!(e.total_non_lus(), 0);
        assert_eq!(e.separateur, Some(49));
    }

    #[test]
    fn le_vocal_suit_la_liste_des_membres() {
        let mut e = accueilli();
        e.appliquer(ServerMsg::MemberUpdate { member: membre(1, "moi", Some(20)) });
        assert_eq!(e.vocal, Some(20));
        e.appliquer(ServerMsg::MemberUpdate { member: membre(1, "moi", None) });
        assert_eq!(e.vocal, None);
    }

    #[test]
    fn la_voix_passee_sur_l_autre_appareil_n_est_plus_la_mienne() {
        let mut e = accueilli();
        let mut m = membre(1, "moi", Some(20));
        m.mobile = true;
        e.appliquer(ServerMsg::MemberUpdate { member: m.clone() });
        assert_eq!(e.vocal, None, "PC : la voix est sur le téléphone");
        e.appareil = ki_protocol::Appareil::Mobile;
        e.appliquer(ServerMsg::MemberUpdate { member: m });
        assert_eq!(e.vocal, Some(20), "téléphone : la voix est ici");
    }

    #[test]
    fn un_nouveau_membre_s_insere_a_sa_place() {
        let mut e = accueilli();
        e.appliquer(ServerMsg::MemberUpdate { member: membre(3, "alice", None) });
        let noms: Vec<&str> = e.membres.iter().map(|m| m.username.as_str()).collect();
        assert_eq!(noms, vec!["alice", "bob", "moi"]);
    }

    #[test]
    fn une_page_d_historique_se_place_en_tete_sans_doublon() {
        let mut e = accueilli();
        e.appliquer(chat(2, "récent", 200, 10));
        let ancien = |ts, t: &str| ChatRecord {
            user_id: 2,
            username: "bob".into(),
            text: t.into(),
            ts,
            reply_to: None,
            reactions: vec![],
            edited: false,
        };
        e.appliquer(ServerMsg::HistoryPage {
            messages: vec![ancien(100, "vieux"), ancien(200, "récent")],
            more: false,
            channel: 10,
        });
        let ts: Vec<u64> = e.messages.iter().map(|m| m.ts).collect();
        assert_eq!(ts, vec![100, 200]);
        assert!(!e.historique_suite);
    }

    #[test]
    fn une_erreur_avant_l_accueil_met_fin_a_la_session() {
        let mut e = Etat::default();
        let effets = e.appliquer(ServerMsg::Error { message: "mot de passe incorrect".into() });
        assert!(egal(&effets, &[Effet::Fin("mot de passe incorrect".into())]));
    }

    #[test]
    fn le_salon_ouvert_qui_disparait_cede_la_place() {
        let mut e = accueilli();
        e.vocal = Some(20);
        let effets =
            e.appliquer(ServerMsg::ChannelsUpdated { channels: vec![salon(11, "autre", false)] });
        assert_eq!(e.courant, Some(11));
        assert_eq!(e.vocal, None);
        assert!(contient(&effets, Effet::Info("le salon vocal a été fermé".into())));
    }

    #[test]
    fn marquer_lu_ne_parle_qu_a_un_serveur_qui_tient_les_lus() {
        let mut e = accueilli();
        assert!(e.marquer_lu(10).is_none());
        e.appliquer(ServerMsg::NonLus { salons: vec![] });
        e.appliquer(chat(2, "salut", 100, 10));
        assert!(matches!(e.marquer_lu(10), Some(ClientMsg::Lu { channel: 10, ts: 100 })));
        assert!(e.non_lus.is_empty());
    }
}
