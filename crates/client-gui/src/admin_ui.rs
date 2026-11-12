//! La fenêtre d'administration : serveur, salons, rôles, membres, invitations,
//! fichiers, journal, tableau de bord et diagnostics.
//!
//! Ce qu'elle retient est dans [`Admin`] ; ce qu'elle demande part au serveur, qui
//! vérifie lui-même les permissions.

use ki_ui::typo::Texte;
use eframe::egui::{self, RichText, Sense, Vec2};
use ki_protocol::{
    AccountInfo, AuditRecord, ChannelId, ChannelKind, ClientMsg, IconChange, InviteInfo,
};
use ki_ui::jetons::{espace, marge, texte};

use crate::{admin_fichiers, graphes, safe_name, theme, ui, visionneuse, Apercu, KiApp};
use crate::formats::{day_label, debit_lisible, duree_lisible, format_time, megabytes};
use crate::icons::Icon;
use crate::saisie::menu_edition;
use crate::televersement::erreur_http;
use crate::theme::{ACCENT, DANGER, INFO, SPEAK, TEXT_DIM, TEXT_FAINT, WARN};
use crate::ui::Ton;

/// L'administration : la fenêtre, ses onglets, et ce qu'on y saisit.
pub(crate) struct Admin {
    pub(crate) ouverte: bool,
    /// Nom du serveur en cours d'édition dans le panneau admin.
    pub(crate) nom: String,
    /// Adresse web publique en cours d'édition (Admin → Serveur).
    pub(crate) adresse_web: String,
    /// Logo choisi dans le panneau admin, pas encore envoyé.
    pub(crate) icone: IconChange,
    pub(crate) comptes: Vec<AccountInfo>,
    pub(crate) invitations: Vec<InviteInfo>,
    pub(crate) derniere_invitation: Option<String>,
    pub(crate) reset_cible: Option<String>,
    pub(crate) reset_mot_de_passe: String,
    /// Journal d'audit, du plus récent au plus ancien.
    pub(crate) audit: Vec<AuditRecord>,
    /// Onglet ouvert dans la fenêtre d'administration.
    pub(crate) onglet: AdminTab,
    /// Admin → Fichiers : la liste des fichiers partagés, ses filtres, sa
    /// sélection.
    pub(crate) fichiers: admin_fichiers::Fichiers,
    /// Réglages du prochain code d'invitation à créer.
    pub(crate) invitation_usages: Option<u32>,
    pub(crate) invitation_libelle: String,
    /// Bannissement en cours de saisie : la cible, le motif, la durée.
    pub(crate) ban: Option<BanDraft>,
    /// Rôle en cours de création ou d'édition.
    pub(crate) role: Option<RoleDraft>,
    /// Salon en cours de création.
    pub(crate) salon: Option<ChannelDraft>,
    /// Salon en cours de renommage, et verrou vocal en cours de pose. Un
    /// seul de chaque à la fois : deux formulaires ouverts sur la même
    /// liste, on ne sait plus lequel on remplit.
    pub(crate) salon_edite: Option<ChannelEdit>,
    pub(crate) verrou: Option<VerrouDraft>,
    /// Compte dont on modifie les rôles, et la sélection en cours.
    pub(crate) roles_cible: Option<String>,
    pub(crate) roles_choisis: Vec<ki_protocol::RoleId>,
    /// Onglet admin « Diagnostics » : le dernier lot récupéré du serveur,
    /// rempli par un thread de récupération.
    pub(crate) diag: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// Les versions de ki-chat présentes dans les archives, pour proposer la
    /// purge d'un lot ancien.
    pub(crate) diag_versions: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    /// Onglet admin « Tableau de bord » : le dernier état reçu du serveur,
    /// rempli par un fil ; le moment de la demande ; une demande en vol.
    pub(crate) tableau: TableauRecu,
    pub(crate) tableau_demande: Option<std::time::Instant>,
    pub(crate) tableau_en_vol: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Le débit réseau du serveur, à part du tableau : relu toutes les cinq
    /// secondes tant que l'onglet est ouvert — la route ne fait que lire
    /// des relevés en mémoire.
    pub(crate) reseau: ReseauRecu,
    pub(crate) reseau_demande: Option<std::time::Instant>,
    pub(crate) reseau_en_vol: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Onglets de la fenêtre d'administration. Tout tenait auparavant dans une
/// seule colonne déroulante ; avec les bannissements et le journal, elle ne
/// se lisait plus.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdminTab {
    Tableau,
    Server,
    Channels,
    Roles,
    Members,
    Fichiers,
    Invites,
    Audit,
    Diagnostics,
}

impl AdminTab {
    const ALL: [AdminTab; 9] = [
        AdminTab::Tableau,
        AdminTab::Server,
        AdminTab::Channels,
        AdminTab::Roles,
        AdminTab::Members,
        AdminTab::Fichiers,
        AdminTab::Invites,
        AdminTab::Audit,
        AdminTab::Diagnostics,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            AdminTab::Tableau => "Tableau de bord",
            AdminTab::Server => "Serveur",
            AdminTab::Channels => "Salons",
            AdminTab::Roles => "Rôles",
            AdminTab::Members => "Membres",
            AdminTab::Fichiers => "Fichiers",
            AdminTab::Invites => "Invitations",
            AdminTab::Audit => "Journal",
            AdminTab::Diagnostics => "Diagnostics",
        }
    }

    /// Les permissions qui donnent accès à cet onglet — **une seule**
    /// suffit. Un onglet hors de portée est **caché**, pas grisé : un onglet
    /// vide n'apprend rien.
    pub(crate) fn needs(self) -> &'static [ki_protocol::Perms] {
        use ki_protocol::perm::*;
        match self {
            // L'état du serveur entier — stocks, diagnostics — : au
            // super-admin, comme les diagnostics.
            AdminTab::Tableau => &[ADMINISTRATOR],
            AdminTab::Server => &[MANAGE_SERVER],
            AdminTab::Channels => &[MANAGE_CHANNELS],
            AdminTab::Roles => &[MANAGE_ROLES],
            // Les comptes, à qui peut agir dessus : débannir et
            // réinitialiser un mot de passe se font d'ici, et l'onglet
            // n'ouvrait qu'avec « Expulser ».
            AdminTab::Members => &[KICK, BAN, RESET_PASSWORD],
            // Retirer ce que les autres ont envoyé : de la modération,
            // comme supprimer leurs messages.
            AdminTab::Fichiers => &[DELETE_MESSAGES],
            AdminTab::Invites => &[CREATE_INVITE, MANAGE_INVITES],
            AdminTab::Audit => &[VIEW_AUDIT_LOG],
            // Les journaux techniques des joueurs : réservé au super-admin,
            // pas à quiconque sait expulser quelqu'un.
            AdminTab::Diagnostics => &[ADMINISTRATOR],
        }
    }
}

/// Rôle en cours d'édition dans l'onglet Rôles.
pub(crate) struct RoleDraft {
    /// `None` = création.
    pub(crate) id: Option<ki_protocol::RoleId>,
    pub(crate) name: String,
    pub(crate) color: egui::Color32,
    pub(crate) colored: bool,
    pub(crate) rank: u16,
    /// Le rang enregistré du rôle modifié, `None` à la création. Le serveur
    /// n'exige un rang libre que de celui qui change : une égalité ancienne
    /// ne doit pas empêcher de renommer le rôle.
    pub(crate) rank_saved: Option<u16>,
    pub(crate) perms: ki_protocol::Perms,
}

/// Salon en cours de création dans l'onglet Salons.
pub(crate) struct ChannelDraft {
    pub(crate) name: String,
    pub(crate) kind: ChannelKind,
    /// Vide = visible par tout le monde.
    pub(crate) allowed_roles: Vec<ki_protocol::RoleId>,
    pub(crate) restricted: bool,
}

/// Salon en cours de modification, dans le panneau d'administration.
///
/// Séparé du brouillon de création : les deux formulaires peuvent être
/// ouverts en même temps, et confondre les deux ferait qu'ouvrir une
/// modification effacerait la création commencée.
pub(crate) struct ChannelEdit {
    pub(crate) id: ki_protocol::ChannelId,
    pub(crate) draft: ChannelDraft,
    /// Le type ne se change pas après coup — on ne convertit pas un salon
    /// textuel plein d'historique en salon vocal. Conservé pour le
    /// renvoyer tel quel au serveur.
    pub(crate) position: u32,
}

/// Verrou vocal en cours de pose.
pub(crate) struct VerrouDraft {
    pub(crate) channel: ki_protocol::ChannelId,
    pub(crate) mot_de_passe: String,
    pub(crate) ttl_secs: u32,
}

/// Durées proposées pour un verrou vocal. Le serveur borne entre une minute
/// et un jour ; on ne propose que ce qui a un sens : le temps d'une partie.
pub(crate) const VOICE_LOCK_DURATIONS: &[(&str, u32)] =
    &[("15 minutes", 900), ("1 heure", 3600), ("4 heures", 14_400), ("1 jour", 86_400)];

/// Durées proposées pour un bannissement. Un menu court vaut mieux qu'un
/// champ libre : personne ne bannit « 137 minutes ».
pub(crate) const BAN_DURATIONS: &[(&str, u64)] = &[
    ("1 heure", 3600),
    ("1 jour", 86_400),
    ("7 jours", 604_800),
    ("30 jours", 2_592_000),
    ("Définitif", 0),
];

/// Bannissement en cours de saisie.
pub(crate) struct BanDraft {
    pub(crate) username: String,
    pub(crate) reason: String,
    /// Durée en secondes ; 0 = définitif.
    pub(crate) duration_secs: u64,
}

/// Ce que le fil du tableau de bord rapporte : l'état du serveur, ou l'erreur.
pub(crate) type TableauRecu =
    std::sync::Arc<std::sync::Mutex<Option<Result<ki_protocol::TableauAdmin, String>>>>;

/// Ce que le fil du débit rapporte : les relevés (`None` s'il n'y en a pas
/// encore, ou hors Linux), ou l'erreur.
pub(crate) type ReseauRecu =
    std::sync::Arc<std::sync::Mutex<Option<Result<Option<ki_protocol::TableauReseau>, String>>>>;

/// Étiquette courte d'un bannissement : « banni » ou le temps restant.
pub(crate) fn ban_summary(account: &AccountInfo) -> String {
    let Some(until) = account.ban_until else { return "banni".into() };
    let now = chrono::Local::now().timestamp_millis().max(0) as u64;
    let minutes = until.saturating_sub(now) / 60_000;
    match minutes {
        0 => "banni — expire".into(),
        1..=90 => format!("banni — {minutes} min"),
        91..=2879 => format!("banni — {} h", minutes / 60),
        _ => format!("banni — {} j", minutes / 1440),
    }
}

/// Traduit un verbe du journal d'audit. Les verbes ne sont jamais traduits
/// côté serveur — le fichier doit rester lisible et « greppable » — donc la
/// traduction se fait ici. Un verbe inconnu (serveur plus récent que le
/// client) s'affiche tel quel plutôt que de disparaître.
pub(crate) fn audit_label(action: &str) -> String {
    match action {
        "invite.create" => "invitation créée".into(),
        "invite.use" => "compte créé".into(),
        "invite.revoke" => "invitation révoquée".into(),
        "member.kick" => "expulsion".into(),
        "member.ban" => "bannissement".into(),
        "member.unban" => "ban annulé".into(),
        "member.password_reset" => "mot de passe réinitialisé".into(),
        "server.info" => "identité du serveur".into(),
        "fichiers.delete" => "fichiers supprimés".into(),
        other => other.to_string(),
    }
}

/// Couleur d'un verbe : rouge pour ce qui sanctionne, accent pour ce qui
/// ouvre un accès. Le journal se parcourt à l'œil avant de se lire.
pub(crate) fn audit_tone(action: &str) -> egui::Color32 {
    match action {
        "member.kick" | "member.ban" | "fichiers.delete" => DANGER,
        "invite.create" | "invite.use" => ACCENT,
        _ => TEXT_DIM,
    }
}

/// Le sélecteur « réservé à certains rôles » d'un salon.
///
/// Partagé par la création et la modification : deux copies divergeraient, et
/// c'est exactement le genre d'écart qui laisse un salon visible de tous alors
/// que le formulaire affichait le contraire.
pub(crate) fn restriction_par_roles(ui: &mut egui::Ui, roles: &[ki_protocol::RoleInfo], draft: &mut ChannelDraft) {
    ui.checkbox(&mut draft.restricted, "Réservé à certains rôles");
    if !draft.restricted {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for role in roles {
            // `@everyone` n'a pas de sens dans une restriction : le cocher
            // reviendrait à ne rien restreindre du tout.
            if role.id == ki_protocol::ROLE_EVERYONE {
                continue;
            }
            let mut on = draft.allowed_roles.contains(&role.id);
            if ui.checkbox(&mut on, &role.name).changed() {
                if on {
                    draft.allowed_roles.push(role.id);
                } else {
                    draft.allowed_roles.retain(|r| *r != role.id);
                }
            }
        }
    });
    if draft.allowed_roles.is_empty() {
        ui::hint(ui, "aucun rôle coché : le salon resterait invisible pour tous");
    }
}

/// Octets en méga-octets, pour l'affichage.
/// Une ligne de stock : la barre face au plafond quand il y en a un.
pub(crate) fn stock_ligne(ui: &mut egui::Ui, nom: &str, s: &ki_protocol::TableauStock) {
    let age = if s.ttl_jours == 0 {
        "sans limite d'âge".to_string()
    } else {
        format!("{} jours", s.ttl_jours)
    };
    if s.plafond_octets > 0 {
        let part = (s.octets as f64 / s.plafond_octets as f64).clamp(0.0, 1.0) as f32;
        ui.add(egui::ProgressBar::new(part).text(format!(
            "{nom} : {} · {:.0} Mo / {:.0} Mo · {age}",
            s.nombre,
            megabytes(s.octets),
            megabytes(s.plafond_octets)
        )));
    } else {
        ui.label(
            Texte::Libelle.riche(ui, format!("{nom} : {} · {:.0} Mo · sans plafond · {age}", s.nombre, megabytes(s.octets))),
        );
    }
}

impl KiApp {
    /// Identité du serveur : nom et logo, que seuls les admins règlent.
    /// C'est ici — et nulle part côté client — que le logo se change.
    pub(crate) fn server_identity_section(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        to_send: &mut Vec<ClientMsg>,
    ) {
        // Vignette rapportée par le thread du sélecteur de fichier.
        if let Some(outcome) = self.picked_icon.lock().unwrap().take() {
            match outcome {
                Ok(data) => self.admin.icone = IconChange::Set { data },
                Err(e) => self.error = Some(format!("image illisible : {e}")),
            }
        }

        ui::group_title(ui, Icon::Server, "Identité du serveur");

        // Ce que verra tout le monde une fois enregistré.
        let pending = match &self.admin.icone {
            IconChange::Set { data } => Some(data.clone()),
            IconChange::Clear => None,
            IconChange::Keep => self.server_info.icon.clone(),
        };
        let preview = self.preview_texture(ctx, Apercu::LogoServeur, pending.as_deref());
        let has_icon = pending.is_some();

        ui.horizontal_top(|ui| {
            let (badge, _) = ui.allocate_exact_size(Vec2::splat(62.0), Sense::hover());
            ui::paint_server_badge(
                ui.painter(),
                badge,
                &self.admin.nom,
                self.url.trim(),
                preview.as_ref(),
            );
            ui.add_space(espace::XS);
            ui.vertical(|ui| {
                ui::field_label(ui, "Nom du serveur");
                ui.add(ui::champ(&mut self.admin.nom, "ex. Chez Kévin"));
                ui.add_space(espace::S);
                ui.horizontal(|ui| {
                    if ui::Bouton::new("Choisir un logo").icone(Icon::Pencil).show(ui).clicked() {
                        self.pick_image(ctx, "Logo du serveur", &self.picked_icon);
                    }
                    if has_icon && ui::Bouton::new("Retirer").icone(Icon::Trash).show(ui).clicked() {
                        self.admin.icone = IconChange::Clear;
                    }
                });
            });
        });
        ui.add_space(espace::S);
        ui::hint(
            ui,
            "PNG ou JPG, réduit en vignette 64×64. Nom et logo sont poussés à \
             tous les membres : eux ne peuvent pas les changer.",
        );

        let dirty = !matches!(self.admin.icone, IconChange::Keep)
            || self.admin.nom.trim() != self.server_info.name;
        let too_long = self.admin.nom.trim().chars().count() > ki_protocol::MAX_SERVER_NAME;
        if too_long {
            ui.add_space(espace::XS);
            ui::hint(
                ui,
                format!("{} caractères maximum", ki_protocol::MAX_SERVER_NAME),
            );
        }

        ui.add_space(espace::L);
        let clicked = ui
            .add_enabled_ui(dirty && !too_long, |ui| {
                ui::Bouton::new("Appliquer au serveur").icone(Icon::Check).principal().show(ui)
            })
            .inner
            .clicked();
        if clicked {
            to_send.push(ClientMsg::AdminSetServerInfo {
                name: Some(self.admin.nom.trim().to_string()),
                icon: std::mem::take(&mut self.admin.icone),
            });
        }

        // L'adresse web publique : la base des liens des portes et des QR
        // codes. Le serveur ne sait pas sous quel nom on le joint —
        // derrière Docker ou une box, rien ne le lui dit : un admin le lui
        // apprend. Un serveur sans portes n'en a pas l'usage.
        if self.portes.disponible {
            ui.add_space(espace::XL);
            ui::hairline(ui);
            ui.add_space(espace::M);
            ui::field_label(ui, "Adresse web publique — la base des liens des portes");
            let auto = self.http_base();
            ui.add(ui::champ(&mut self.admin.adresse_web, &format!("automatique : {auto}")));
            let normalisee = ki_protocol::normaliser_adresse_web(&self.admin.adresse_web);
            match &normalisee {
                Err(e) => {
                    ui.label(Texte::Libelle.riche_ton(ui, Ton::Danger, e.as_str()));
                }
                Ok(n) if n.is_empty() => {
                    ui::hint(ui, format!("vide : automatique, les liens partent de l'adresse de connexion — {auto}/salon1"));
                }
                Ok(n) => ui::hint(ui, format!("les liens des portes : {n}/salon1")),
            }
            ui::hint(
                ui,
                "comme dans la barre d'adresse du navigateur : ex. ts.baws.fun:8080. Le https:// est ajouté \
                 s'il manque — sans lui, le navigateur tenterait http:// et la page ne s'ouvrirait pas.",
            );
            ui.add_space(espace::S);
            let change = normalisee.as_ref().is_ok_and(|n| *n != self.server_info.adresse_web);
            let enregistrer = ui
                .add_enabled_ui(change, |ui| {
                    ui::Bouton::new("Enregistrer l'adresse").icone(Icon::Check).principal().show(ui)
                })
                .inner
                .clicked();
            if enregistrer {
                if let Ok(adresse) = normalisee {
                    // Le champ montre tout de suite la forme enregistrée.
                    self.admin.adresse_web = adresse.clone();
                    to_send.push(ClientMsg::AdminSetAdresseWeb { adresse });
                }
            }
        }

        // Le fil de jeu VALORANT : le salon où le serveur annonce les
        // parties finies des membres liés. Le choix part tout de suite, le
        // serveur le confirme en repoussant son identité à tout le monde.
        ui.add_space(espace::XL);
        ui::hairline(ui);
        ui.add_space(espace::M);
        ui::field_label(ui, "Fil de jeu VALORANT");
        let actuel = self.server_info.fil_valorant;
        let nom_du_salon = |id: ChannelId| -> String {
            self.channels
                .iter()
                .find(|c| c.id == id)
                .map(|c| format!("#{}", c.name))
                .unwrap_or_else(|| format!("salon {id}"))
        };
        let mut choix = actuel;
        egui::ComboBox::from_id_salt("fil_valorant")
            .width(240.0)
            .selected_text(match choix {
                None => "aucun — fil éteint".to_string(),
                Some(id) => nom_du_salon(id),
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut choix, None, "aucun — fil éteint");
                for c in self.channels.iter().filter(|c| c.kind == ki_protocol::ChannelKind::Text) {
                    ui.selectable_value(&mut choix, Some(c.id), format!("#{}", c.name));
                }
            });
        if choix != actuel {
            to_send.push(ClientMsg::AdminSetFilValorant { channel: choix });
        }
        ui::hint(
            ui,
            "à chaque partie finie d'un membre qui a lié son compte Riot, le serveur poste \
             le résultat, sa ligne et ses RR dans ce salon — les coéquipiers du groupe sur \
             un même message. Les modes d'arcade ne sont pas annoncés.",
        );

        // Le bot musique : qui peut y ajouter des morceaux.
        ui.add_space(espace::XL);
        ui::hairline(ui);
        ui.add_space(espace::M);
        ui::field_label(ui, "Bot musique");
        let mut ajoutent = self.server_info.musique_membres_ajoutent;
        if ui.checkbox(&mut ajoutent, "Les membres peuvent chercher et ajouter des morceaux").changed() {
            to_send.push(ClientMsg::AdminSetMusique { membres_ajoutent: ajoutent });
        }
        ui::hint(
            ui,
            "en fin de file seulement : lecture, pause, ordre, volume et playlists restent aux \
             rôles qui ont « Contrôler la musique ».",
        );
    }

    /// Onglet Diagnostics : les journaux techniques que les joueurs
    /// volontaires partagent (⚙ Audio → « Partager mes diagnostics »). Tout
    /// se récupère en un clic, s'affiche, et se copie d'un bloc — pour être
    /// collé à qui débogue.
    pub(crate) fn admin_diag_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // Marqueurs du résumé dans le slot d'affichage : ils permettent d'en
        // remplacer un périmé sans toucher au détail, et de garder le résumé
        // entier à l'écran quand le détail, lui, est coupé.
        const ENTETE_RESUME: &str = "===== résumé par version =====\n";
        const FIN_RESUME: &str = "\n===== détail des archives =====\n";
        ui::group_title(ui, Icon::Info, "Diagnostics des joueurs");
        ui::hint(
            ui,
            "les journaux n'existent que pour ceux qui ont coché « Partager mes \
             diagnostics » dans leurs réglages (Aide & diagnostics) ; les rapports de plantage, eux, \
             arrivent de tout le monde — technique seulement, jamais les messages ni la voix",
        );
        ui.add_space(espace::S);
        if ui::Bouton::new("Récupérer tous les diagnostics").icone(Icon::Refresh).show(ui).clicked() {
            let base = self.http_base();
            let agent = self.http_agent();
            let token_hex = format!("{:x}", self.voice_token);
            let slot = self.admin.diag.clone();
            let versions_slot = self.admin.diag_versions.clone();
            *slot.lock().unwrap() = Some("récupération en cours…".into());
            std::thread::spawn(move || {
                let mut versions: Vec<String> = Vec::new();
                let resultat = (|| -> Result<String, String> {
                    let liste = agent
                        .get(&format!("{base}/diag"))
                        .set("x-ki-token", &token_hex)
                        .call()
                        .map_err(|e| e.to_string())?
                        .into_string()
                        .map_err(|e| e.to_string())?;
                    if liste.trim().is_empty() {
                        return Ok("aucun diagnostic reçu pour l'instant — personne n'a \
                                   coché l'option, ou le serveur vient d'être redéployé"
                            .into());
                    }
                    // La fin de chaque archive suffit : c'est le passé récent
                    // qu'on débogue, et l'affichage comme le presse-papiers
                    // n'ont pas à charrier des mégaoctets d'historique.
                    // Les archives arrivent triées par version (le serveur
                    // les classe ainsi) : l'historique des bugs se lit par
                    // livraison.
                    let mut tout = String::new();
                    for ligne in liste.lines() {
                        let Some(fichier) = ligne.split('\t').next() else { continue };
                        if let Some(version) = fichier.split('/').next() {
                            if versions.last().map(String::as_str) != Some(version) {
                                versions.push(version.to_string());
                            }
                        }
                        match agent
                            .get(&format!("{base}/diag/{fichier}?tail=65536"))
                            .set("x-ki-token", &token_hex)
                            .call()
                        {
                            Ok(r) => {
                                let corps = r.into_string().unwrap_or_default();
                                // Un plantage se voit dès l'en-tête : c'est
                                // ce qu'on cherche en premier dans ce mur.
                                let plantages = corps.matches("\"type\":\"crash\"").count();
                                let marque = match plantages {
                                    0 => String::new(),
                                    1 => " ⚠ 1 PLANTAGE".to_string(),
                                    n => format!(" ⚠ {n} PLANTAGES"),
                                };
                                tout.push_str(&format!("\n===== {ligne}{marque} =====\n"));
                                tout.push_str(&corps);
                            }
                            Err(e) => {
                                tout.push_str(&format!("\n===== {ligne} =====\n(illisible : {e})\n"))
                            }
                        }
                    }
                    Ok(tout)
                })();
                *versions_slot.lock().unwrap() = versions;
                *slot.lock().unwrap() = Some(match resultat {
                    Ok(t) => t,
                    Err(e) => format!("échec de la récupération : {e}"),
                });
            });
        }
        // L'état des lieux avant le détail : le serveur compte, par version,
        // les joueurs et les marqueurs qui fâchent (sessions, réouvertures,
        // famines, erreurs, crashs) — un écran au lieu d'un défilement.
        ui.add_space(espace::XS);
        if ui::Bouton::new("Résumé par version").icone(Icon::Info).show(ui).clicked() {
            let base = self.http_base();
            let agent = self.http_agent();
            let token_hex = format!("{:x}", self.voice_token);
            let slot = self.admin.diag.clone();
            std::thread::spawn(move || {
                let resume = match agent
                    .get(&format!("{base}/diag-resume"))
                    .set("x-ki-token", &token_hex)
                    .call()
                    .map_err(|e| e.to_string())
                    .and_then(|r| r.into_string().map_err(|e| e.to_string()))
                {
                    Ok(t) => t,
                    Err(e) => format!("résumé indisponible : {e}"),
                };
                // Le résumé se pose AU-DESSUS du détail déjà récupéré, dans
                // le même slot : même affichage monospace, et la copie
                // emporte les deux d'un bloc.
                let mut slot = slot.lock().unwrap();
                let ancien = slot.take().unwrap_or_default();
                let detail = match ancien.split_once(FIN_RESUME) {
                    Some((_, d)) => d.to_string(),
                    None if ancien.starts_with(ENTETE_RESUME) => String::new(),
                    None => ancien,
                };
                *slot = Some(if detail.is_empty() {
                    format!("{ENTETE_RESUME}{resume}")
                } else {
                    format!("{ENTETE_RESUME}{resume}{FIN_RESUME}{detail}")
                });
            });
        }
        // La purge par version : l'historique des bugs d'une livraison
        // dépassée n'apprend plus rien, autant rendre la place.
        let versions = self.admin.diag_versions.lock().unwrap().clone();
        if !versions.is_empty() {
            ui.add_space(espace::S);
            ui::field_label(ui, "purger les archives d'une version");
            ui.horizontal_wrapped(|ui| {
                for version in &versions {
                    if ui::Bouton::new(version).icone(Icon::Trash).show(ui).clicked() {
                        let base = self.http_base();
                        let agent = self.http_agent();
                        let token_hex = format!("{:x}", self.voice_token);
                        let slot = self.admin.diag.clone();
                        let versions_slot = self.admin.diag_versions.clone();
                        let version = version.clone();
                        std::thread::spawn(move || {
                            let r = agent
                                .delete(&format!("{base}/diag/{version}"))
                                .set("x-ki-token", &token_hex)
                                .call();
                            let message = match r {
                                Ok(_) => format!(
                                    "archives de la version {version} supprimées — \
                                     re-récupère la liste"
                                ),
                                Err(e) => format!("suppression impossible : {e}"),
                            };
                            versions_slot.lock().unwrap().retain(|v| v != &version);
                            *slot.lock().unwrap() = Some(message);
                        });
                    }
                }
            });
        }
        let contenu = self.admin.diag.lock().unwrap().clone();
        if let Some(texte) = contenu {
            ui.add_space(espace::S);
            if ui::Bouton::new("Tout copier").icone(Icon::Copy).show(ui).clicked() {
                ctx.copy_text(texte.clone());
                self.info = Some("diagnostics copiés — prêts à coller".into());
            }
            ui.add_space(espace::XS);
            // L'affichage est borné aux derniers caractères : des mégaoctets
            // de journal mettraient l'interface à genoux. La copie, elle,
            // emporte tout ce qui a été récupéré. Seule exception : le résumé
            // en tête reste entier — c'est un écran, et c'est lui qu'on est
            // venu lire ; la coupe ne s'applique qu'au détail qui le suit.
            let (tete, reste) = match texte.find(FIN_RESUME) {
                Some(i) => texte.split_at(i + FIN_RESUME.len()),
                None => ("", texte.as_str()),
            };
            let debut = reste
                .char_indices()
                .rev()
                .nth(20_000)
                .map(|(i, _)| i)
                .unwrap_or(0);
            egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                if !tete.is_empty() {
                    ui.label(
                        Texte::Aide.riche_ton(ui, Ton::Neutre, tete).monospace(),
                    );
                }
                ui.label(
                    Texte::Aide.riche_ton(ui, Ton::Neutre, &reste[debut..]).monospace(),
                );
            });
        }
    }

    /// Demande le tableau de bord au serveur, sur un fil ; l'ancien reste
    /// affiché le temps que le nouveau arrive.
    pub(crate) fn demander_tableau(&mut self) {
        if self.admin.tableau_en_vol.swap(true, std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        self.admin.tableau_demande = Some(std::time::Instant::now());
        let base = self.http_base();
        let agent = self.http_agent();
        let token_hex = format!("{:x}", self.voice_token);
        let slot = self.admin.tableau.clone();
        let en_vol = self.admin.tableau_en_vol.clone();
        let ctx = self.app_ctx.clone();
        std::thread::spawn(move || {
            let resultat = agent
                .get(&format!("{base}/admin/tableau"))
                .set("x-ki-token", &token_hex)
                .timeout(std::time::Duration::from_secs(20))
                .call()
                .map_err(erreur_http)
                .and_then(|r| {
                    r.into_json::<ki_protocol::TableauAdmin>().map_err(|e| e.to_string())
                });
            *slot.lock().unwrap() = Some(resultat);
            en_vol.store(false, std::sync::atomic::Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    /// Demande le débit réseau au serveur, sur un fil ; la courbe d'avant
    /// reste affichée le temps que la nouvelle arrive.
    pub(crate) fn demander_reseau(&mut self) {
        if self.admin.reseau_en_vol.swap(true, std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        self.admin.reseau_demande = Some(std::time::Instant::now());
        let base = self.http_base();
        let agent = self.http_agent();
        let token_hex = format!("{:x}", self.voice_token);
        let slot = self.admin.reseau.clone();
        let en_vol = self.admin.reseau_en_vol.clone();
        let ctx = self.app_ctx.clone();
        std::thread::spawn(move || {
            let resultat = agent
                .get(&format!("{base}/admin/reseau"))
                .set("x-ki-token", &token_hex)
                .timeout(std::time::Duration::from_secs(10))
                .call()
                .map_err(|e| match e {
                    ureq::Error::Status(404, _) => {
                        "le serveur ne le relève pas encore (version d'avant 0.1.50)".to_string()
                    }
                    e => erreur_http(e),
                })
                .and_then(|r| {
                    r.into_json::<Option<ki_protocol::TableauReseau>>().map_err(|e| e.to_string())
                });
            *slot.lock().unwrap() = Some(resultat);
            en_vol.store(false, std::sync::atomic::Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    /// Onglet « Tableau de bord » : l'état du serveur en un écran, demandé
    /// à l'ouverture puis toutes les trente secondes tant qu'il est ouvert ;
    /// le débit réseau, lui, toutes les cinq.
    pub(crate) fn admin_tableau_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let perime = self.admin
            .tableau_demande
            .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(30));
        if perime {
            self.demander_tableau();
        }
        // Quatre secondes et non cinq : le réveil ci-dessous tombe à cinq,
        // un seuil égal raterait un tour sur deux.
        if self.admin.reseau_demande.is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(4)) {
            self.demander_reseau();
        }
        // Pour l'« il y a … » et le rafraîchissement, même sans souris.
        ctx.request_repaint_after(std::time::Duration::from_secs(5));
        let mut rafraichir = false;
        ui.horizontal(|ui| {
            ui::group_title(ui, Icon::Server, "Tableau de bord");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui::Bouton::new("Rafraîchir").icone(Icon::Refresh).show(ui).clicked() {
                    rafraichir = true;
                }
                if let Some(t) = self.admin.tableau_demande {
                    ui.label(
                        Texte::Aide.riche(ui, format!("il y a {} s", t.elapsed().as_secs())),
                    );
                }
            });
        });
        if rafraichir {
            self.admin.tableau_demande = None;
            self.admin.reseau_demande = None;
        }
        ui.add_space(espace::XS);
        let tableau = self.admin.tableau.lock().unwrap().clone();
        match tableau {
            None => ui::hint(ui, "récupération de l'état du serveur…"),
            Some(Err(e)) => {
                ui::bandeau(ui, Ton::Alerte, &format!("tableau indisponible : {e}"));
            }
            Some(Ok(t)) => self.peindre_tableau(ui, &t),
        }
    }

    /// La carte « Réseau » : le débit du serveur maintenant, sa courbe sur
    /// dix minutes, la pointe et le total depuis le démarrage.
    pub(crate) fn peindre_reseau(&self, ui: &mut egui::Ui) {
        let recu = self.admin.reseau.lock().unwrap().clone();
        ui::card(ui, |ui| {
            ui::section_label(ui, "Réseau");
            let r = match recu {
                None => return ui::hint(ui, "relevé du débit…"),
                Some(Err(e)) => return ui::hint(ui, format!("débit indisponible : {e}")),
                Some(Ok(None)) => {
                    return ui::hint(
                        ui,
                        "pas encore de relevé : le serveur vient de démarrer, ou ne tourne pas sous Linux",
                    );
                }
                Some(Ok(Some(r))) => r,
            };
            // Les deux mots en couleur servent de légende à la courbe.
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = espace::XS;
                ui.label(Texte::Libelle.riche_ton(ui, Ton::Info, "entrant"));
                ui.label(Texte::Libelle.gras(ui, debit_lisible(r.entrant_kbps)));
                ui.label(Texte::Libelle.riche_ton(ui, Ton::Pale, "·"));
                ui.label(Texte::Libelle.riche_ton(ui, Ton::Accent, "sortant"));
                ui.label(Texte::Libelle.gras(ui, debit_lisible(r.sortant_kbps)));
                ui::hint(ui, format!("sur les {} dernières secondes", r.periode_s));
            });
            ui.add_space(espace::XS);
            graphes::debits(ui, &r.historique, r.periode_s, 76.0, (INFO, ACCENT), debit_lisible);
            ui.add_space(espace::XS);
            let fenetre = if r.fenetre_s < 60 {
                format!("{} s", r.fenetre_s)
            } else {
                duree_lisible(u64::from(r.fenetre_s))
            };
            ui.label(
                Texte::Libelle.riche(ui, format!(
                    "pointe sur {fenetre} : entrant {} · sortant {} · depuis le démarrage : reçu {}, envoyé {}",
                    debit_lisible(r.pointe_entrant_kbps),
                    debit_lisible(r.pointe_sortant_kbps),
                    admin_fichiers::taille_lisible(r.total_entrant_octets),
                    admin_fichiers::taille_lisible(r.total_sortant_octets),
                )),
            );
            ui::hint(ui, "le sortant porte les streams : chaque spectateur reçoit sa propre copie");
        });
    }

    /// Le tableau lui-même, carte par carte.
    pub(crate) fn peindre_tableau(&self, ui: &mut egui::Ui, t: &ki_protocol::TableauAdmin) {
        let nom_salon = |id: ChannelId| {
            self.channels
                .iter()
                .find(|c| c.id == id)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| format!("salon {id}"))
        };
        let dim = |ui: &egui::Ui, texte: String| Texte::Libelle.riche(ui, texte);

        ui::card(ui, |ui| {
            ui::section_label(ui, "Serveur");
            let mut ligne = format!(
                "version {} · en ligne depuis {} · {} comptes, {} connecté(s) · {} salons \
                 textuels, {} vocaux",
                t.version,
                duree_lisible(t.depuis_s),
                t.comptes,
                t.en_ligne.len(),
                t.salons_texte,
                t.vocal.len()
            );
            if let Some(m) = t.memoire_octets {
                ligne.push_str(&format!(" · mémoire {:.0} Mo", megabytes(m)));
            }
            if let Some(d) = t.disque_libre_octets {
                ligne.push_str(&format!(" · disque libre {:.1} Go", d as f64 / 1e9));
            }
            ui.label(Texte::Libelle.riche_ton(ui, Ton::Neutre, ligne));
            ui.add_space(espace::XS);
            ui::section_label(ui, "Connectés");
            if t.en_ligne.is_empty() {
                ui::hint(ui, "personne");
            }
            for m in &t.en_ligne {
                let mut mots = vec![safe_name(&m.pseudo)];
                if let Some(v) = &m.vocal {
                    mots.push(format!("vocal : {}", ki_protocol::safe_display(v, 40)));
                }
                if m.diffuse {
                    mots.push("diffuse son écran".into());
                }
                if let Some(j) = &m.jeu {
                    mots.push(ki_protocol::safe_display(j, 80));
                }
                ui.label(dim(ui, mots.join(" · ")));
            }
        });
        ui.add_space(espace::M);

        self.peindre_reseau(ui);
        ui.add_space(espace::M);

        ui::card(ui, |ui| {
            ui::section_label(ui, "Vocal");
            if t.vocal.is_empty() {
                ui::hint(ui, "aucun salon vocal");
            }
            for s in &t.vocal {
                let occupants = if s.occupants.is_empty() {
                    "personne".to_string()
                } else {
                    s.occupants.iter().map(|o| safe_name(o)).collect::<Vec<_>>().join(", ")
                };
                let verrou = if s.verrouille { " · verrouillé" } else { "" };
                ui.label(dim(ui, format!("{} : {occupants}{verrou}", ki_protocol::safe_display(&s.nom, 40))));
            }
            ui.add_space(espace::XS);
            ui::section_label(ui, "Diffusions");
            if t.diffusions.is_empty() {
                ui::hint(ui, "aucune diffusion en cours");
            }
            for d in &t.diffusions {
                let palier = d
                    .palier
                    .map(|p| format!(" · bridé à {p} kbit/s pour un spectateur"))
                    .unwrap_or_default();
                ui.label(dim(ui, format!(
                    "{} · {} spectateur(s) · {}x{} à {} i/s · {} kbit/s{palier}",
                    safe_name(&d.streamer),
                    d.spectateurs,
                    d.largeur,
                    d.hauteur,
                    d.fps,
                    d.kbps
                )));
            }
        });
        ui.add_space(espace::M);

        ui::card(ui, |ui| {
            ui::section_label(ui, "Stockage");
            stock_ligne(ui, "fichiers partagés", &t.fichiers);
            stock_ligne(ui, "clips", &t.clips);
            ui::hint(ui, "les plafonds et les âges viennent des variables KI_FILES_* et KI_CLIPS_* du serveur");
            // La fabrique des vidéos : un ffmpeg à la fois, et ce qui
            // attend derrière — c'est là qu'on voit un export qui traîne.
            let f = &t.fabrique;
            let quoi = match &f.en_cours {
                Some(tache) => format!(
                    "{} depuis {}",
                    ki_protocol::safe_display(tache, 80),
                    if f.depuis_s < 60 { format!("{} s", f.depuis_s) } else { duree_lisible(f.depuis_s) }
                ),
                None => "rien en cours".into(),
            };
            ui.label(dim(ui, format!("fabrique : {} en file · {quoi}", f.en_file)));
        });
        ui.add_space(espace::M);

        ui::card(ui, |ui| {
            ui::section_label(ui, "Musique");
            let m = &t.musique;
            if !m.disponible {
                ui::hint(ui, "bot indisponible : yt-dlp ou ffmpeg manquent sur le serveur");
            } else {
                let ou = m.salon.map(nom_salon).unwrap_or_else(|| "nulle part".into());
                let quoi = match &m.en_cours {
                    Some(titre) => format!(
                        "{} : {}",
                        if m.lecture { "joue" } else { "en pause sur" },
                        ki_protocol::safe_display(titre, 80)
                    ),
                    None => "rien en cours".into(),
                };
                ui.label(dim(ui, format!("dans {ou} · {quoi} · {} en file", m.file)));
                let outil = if m.yt_dlp.is_empty() { "absent".to_string() } else { m.yt_dlp.clone() };
                ui.label(dim(ui, format!(
                    "{} piste(s) jouée(s) · {} échec(s) · premier son en {} ms en moyenne · yt-dlp : {outil}",
                    m.pistes_jouees, m.echecs, m.premier_son_ms
                )));
            }
            ui.add_space(espace::XS);
            ui::section_label(ui, "VALORANT");
            ui.label(dim(ui, ki_protocol::safe_display(&t.valorant, 400)));
        });
        ui.add_space(espace::M);

        ui::card(ui, |ui| {
            ui::section_label(ui, "Diagnostics des joueurs");
            if t.diagnostics.is_empty() {
                ui::hint(
                    ui,
                    "aucune archive — personne n'a coché l'option, ou le serveur vient d'être redéployé",
                );
                return;
            }
            egui::Grid::new("tableau-diag").striped(true).spacing([14.0, 3.0]).show(ui, |ui| {
                for entete in ["version", "joueurs", "sessions", "réouvertures", "famines", "erreurs", "crashs", "taille"] {
                    ui::hint(ui, entete);
                }
                ui.end_row();
                for d in &t.diagnostics {
                    ui.label(Texte::Libelle.riche_ton(ui, Ton::Neutre, ki_protocol::safe_display(&d.version, 20)));
                    for v in [d.joueurs, d.sessions, d.reouvertures, d.famines, d.erreurs] {
                        ui.label(dim(ui, v.to_string()));
                    }
                    let teinte = if d.crashs > 0 { DANGER } else { TEXT_DIM };
                    ui.label(RichText::new(d.crashs.to_string()).color(teinte).size(texte::COURANT));
                    ui.label(dim(ui, format!("{} Ko", d.taille_ko)));
                    ui.end_row();
                }
            });
            ui::hint(ui, "le détail des journaux est dans l'onglet Diagnostics");
        });
    }

    pub(crate) fn admin_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let mut to_send: Vec<ClientMsg> = Vec::new();
        let roomy = (ctx.content_rect().height() - 120.0).clamp(320.0, 900.0);
        egui::Window::new("Administration")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(460.0)
            .default_height(roomy)
            .min_width(360.0)
            .min_height(260.0)
            .show(ctx, |ui| {
                // Onglets : tout tenait auparavant dans une seule colonne
                // déroulante, qui ne se lisait plus une fois les
                // bannissements et le journal ajoutés.
                ui.horizontal_wrapped(|ui| {
                    let tabs: Vec<AdminTab> =
                        AdminTab::ALL.into_iter().filter(|t| self.onglet_ouvert(*t)).collect();
                    for tab in tabs {
                        // Un clic, même sur l'onglet ouvert, recharge ce
                        // qu'il montre.
                        if ui::onglet(ui, self.admin.onglet == tab, tab.label()).clicked() {
                            self.admin.onglet = tab;
                            // Le journal ne se charge qu'à l'ouverture de
                            // son onglet : inutile de le pousser à chaque
                            // ouverture du panneau.
                            if tab == AdminTab::Audit {
                                to_send.push(ClientMsg::AdminAuditLog { limit: 200 });
                            }
                            // La liste des fichiers aussi : relire le stock
                            // coûte au serveur, on ne le fait pas pour rien.
                            if tab == AdminTab::Fichiers {
                                to_send.push(self.admin.fichiers.demander());
                            }
                        }
                    }
                });
                ui.add_space(espace::M);
                ui::hairline(ui);
                ui.add_space(espace::L);

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // L'onglet ouvert peut être devenu hors de portée si
                        // les rôles viennent de changer sous nos pieds.
                        if !self.onglet_ouvert(self.admin.onglet) {
                            self.admin.onglet = AdminTab::ALL
                                .into_iter()
                                .find(|t| self.onglet_ouvert(*t))
                                .unwrap_or(AdminTab::Server);
                        }
                        match self.admin.onglet {
                            AdminTab::Tableau => self.admin_tableau_tab(ui, ctx),
                            AdminTab::Server => self.server_identity_section(ui, ctx, &mut to_send),
                            AdminTab::Channels => self.admin_channels_tab(ui, &mut to_send),
                            AdminTab::Roles => self.admin_roles_tab(ui, &mut to_send),
                            AdminTab::Members => self.admin_members_tab(ui, &mut to_send),
                            AdminTab::Fichiers => self.admin_fichiers_tab(ui, &mut to_send),
                            AdminTab::Invites => self.admin_invites_tab(ui, ctx, &mut to_send),
                            AdminTab::Audit => self.admin_audit_tab(ui),
                            AdminTab::Diagnostics => self.admin_diag_tab(ui, ctx),
                        }
                        if let Some(info) = self.info.clone() {
                            ui.add_space(espace::L);
                            if ui::bandeau_fermable(ui, Ton::Accent, &info) {
                                self.info = None;
                            }
                        }
                    });
            });
        for msg in to_send {
            self.send(msg);
        }
        if !open {
            self.close_admin();
        }
    }

    /// Onglet « Salons » : créer, renommer, restreindre, supprimer.
    pub(crate) fn admin_channels_tab(&mut self, ui: &mut egui::Ui, to_send: &mut Vec<ClientMsg>) {
        ui::group_title(ui, Icon::Plus, "Nouveau salon");
        let draft = self.admin.salon.get_or_insert_with(|| ChannelDraft {
            name: String::new(),
            kind: ChannelKind::Text,
            allowed_roles: Vec::new(),
            restricted: false,
        });
        ui.add(ui::champ(&mut draft.name, "nom du salon"));
        ui.horizontal(|ui| {
            ui.selectable_value(&mut draft.kind, ChannelKind::Text, "Textuel");
            ui.selectable_value(&mut draft.kind, ChannelKind::Voice, "Vocal");
        });
        let roles = self.roles.clone();
        restriction_par_roles(ui, &roles, draft);
        let ready = !draft.name.trim().is_empty()
            && (!draft.restricted || !draft.allowed_roles.is_empty());
        let create = ui
            .add_enabled_ui(ready, |ui| ui::Bouton::new("Créer le salon").icone(Icon::Plus).show(ui))
            .inner
            .clicked();
        if create {
            to_send.push(ClientMsg::AdminCreateChannel {
                name: draft.name.trim().to_string(),
                kind: draft.kind,
                allowed_roles: draft.restricted.then(|| draft.allowed_roles.clone()),
            });
            self.admin.salon = None;
        }

        ui.add_space(espace::L);
        ui::hairline(ui);
        ui.add_space(espace::L);
        ui::group_title(ui, Icon::Hash, "Salons existants");
        let channels = self.channels.clone();
        // L'ordre affiché **est** l'ordre du serveur : la liste arrive déjà
        // triée par position, et qui gère les salons les voit tous. C'est ce
        // qui permet d'envoyer une permutation exacte — le serveur refuse
        // toute liste incomplète, et une vue filtrée en serait une.
        let ids: Vec<ki_protocol::ChannelId> = channels.iter().map(|c| c.id).collect();
        for (i, ch) in channels.iter().enumerate() {
            ui.horizontal(|ui| {
                ui::glyph(
                    ui,
                    if ch.kind == ChannelKind::Voice { Icon::Volume } else { Icon::Hash },
                    13.0,
                    TEXT_DIM,
                );
                ui.label(Texte::Corps.riche(ui, &ch.name));
                if ch.allowed_roles.is_some() {
                    ui.label(Texte::Aide.riche_ton(ui, Ton::Accent, "privé"));
                }
                if ch.locked {
                    ui.label(Texte::Aide.riche_ton(ui, Ton::Alerte, "verrouillé"));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui::Bouton::icone_seule(Icon::Trash).cote(24.0).bulle("Supprimer").show(ui).clicked() {
                        to_send.push(ClientMsg::AdminDeleteChannel { channel: ch.id });
                    }
                    if ui::Bouton::icone_seule(Icon::Pencil).cote(24.0).bulle("Renommer").show(ui).clicked() {
                        // Un formulaire déjà ouvert sur ce salon se referme :
                        // le crayon fait aller-retour, sans bouton « annuler »
                        // à chercher.
                        self.admin.salon_edite = match self.admin.salon_edite.take() {
                            Some(e) if e.id == ch.id => None,
                            _ => Some(ChannelEdit {
                                id: ch.id,
                                position: ch.position,
                                draft: ChannelDraft {
                                    name: ch.name.clone(),
                                    kind: ch.kind,
                                    restricted: ch.allowed_roles.is_some(),
                                    allowed_roles: ch.allowed_roles.clone().unwrap_or_default(),
                                },
                            }),
                        };
                    }
                    // Le verrou n'a de sens que sur un salon vocal : c'est le
                    // seul endroit où l'on entre, et donc le seul où l'on
                    // puisse être arrêté à la porte.
                    if ch.kind == ChannelKind::Voice {
                        let quoi = if ch.locked { "Changer le verrou" } else { "Verrouiller" };
                        let teinte = ch.locked.then_some(WARN);
                        if ui::Bouton::icone_seule(Icon::Key).cote(24.0).couleur_si(teinte).bulle(quoi).show(ui).clicked() {
                            self.admin.verrou = match self.admin.verrou.take() {
                                Some(v) if v.channel == ch.id => None,
                                _ => Some(VerrouDraft {
                                    channel: ch.id,
                                    mot_de_passe: String::new(),
                                    ttl_secs: VOICE_LOCK_DURATIONS[1].1,
                                }),
                            };
                        }
                    }
                    // Monter et descendre plutôt qu'un glisser-déposer : sur
                    // une dizaine de salons c'est aussi rapide, et ça ne
                    // dépend pas de la précision de la souris.
                    let bouger = |haut: bool| -> ClientMsg {
                        let mut order = ids.clone();
                        order.swap(i, if haut { i - 1 } else { i + 1 });
                        ClientMsg::AdminReorderChannels { order }
                    };
                    if ui
                        .add_enabled_ui(i + 1 < ids.len(), |ui| {
                            ui::Bouton::icone_seule(Icon::ArrowDown).cote(24.0).bulle("Descendre").show(ui)
                        })
                        .inner
                        .clicked()
                    {
                        to_send.push(bouger(false));
                    }
                    if ui
                        .add_enabled_ui(i > 0, |ui| {
                            ui::Bouton::icone_seule(Icon::ArrowUp).cote(24.0).bulle("Monter").show(ui)
                        })
                        .inner
                        .clicked()
                    {
                        to_send.push(bouger(true));
                    }
                });
            });

            // Les deux formulaires s'ouvrent sous leur salon, et pas dans une
            // fenêtre à part : on voit ce qu'on modifie.
            if self.admin.salon_edite.as_ref().is_some_and(|e| e.id == ch.id) {
                let mut valider = false;
                let mut annuler = false;
                ui.indent(("edition", ch.id), |ui| {
                    let edit = self.admin.salon_edite.as_mut().expect("vérifié à l'instant");
                    ui.add(ui::champ(&mut edit.draft.name, "nom du salon"));
                    restriction_par_roles(ui, &roles, &mut edit.draft);
                    let ok = !edit.draft.name.trim().is_empty()
                        && (!edit.draft.restricted || !edit.draft.allowed_roles.is_empty());
                    ui.horizontal(|ui| {
                        valider = ui
                            .add_enabled_ui(ok, |ui| ui::Bouton::new("Enregistrer").icone(Icon::Check).show(ui))
                            .inner
                            .clicked();
                        annuler = ui::Bouton::new("Annuler").icone(Icon::Close).show(ui).clicked();
                    });
                });
                if valider {
                    let edit = self.admin.salon_edite.take().expect("vérifié à l'instant");
                    to_send.push(ClientMsg::AdminEditChannel {
                        channel: ki_protocol::ChannelInfo {
                            id: edit.id,
                            name: edit.draft.name.trim().to_string(),
                            // La nature ne se change pas — le serveur le
                            // refuse — mais le message remplace tout le
                            // salon : il faut la lui redire à l'identique.
                            kind: edit.draft.kind,
                            // Idem pour la position : la taire reviendrait à
                            // demander le rang 0, et renommer un salon le
                            // ferait remonter en tête de liste.
                            position: edit.position,
                            locked: false,
                            allowed_roles: edit
                                .draft
                                .restricted
                                .then(|| edit.draft.allowed_roles.clone()),
                            // Un salon temporaire ne s'édite pas d'ici ;
                            // le serveur garde sa propre date.
                            expire_le: None,
                        },
                    });
                } else if annuler {
                    self.admin.salon_edite = None;
                }
            }

            if self.admin.verrou.as_ref().is_some_and(|v| v.channel == ch.id) {
                let mut poser = false;
                let mut retirer = false;
                ui.indent(("verrou", ch.id), |ui| {
                    let v = self.admin.verrou.as_mut().expect("vérifié à l'instant");
                    ui.add(ui::champ_secret(&mut v.mot_de_passe, "mot de passe"));
                    ui.horizontal_wrapped(|ui| {
                        for (label, secs) in VOICE_LOCK_DURATIONS {
                            ui.selectable_value(&mut v.ttl_secs, *secs, *label);
                        }
                    });
                    let ok = !v.mot_de_passe.is_empty();
                    ui.horizontal(|ui| {
                        poser = ui
                            .add_enabled_ui(ok, |ui| ui::Bouton::new("Poser le verrou").icone(Icon::Key).show(ui))
                            .inner
                            .clicked();
                        // Retirer n'est proposé que s'il y a quelque chose à
                        // retirer, sinon le bouton ne fait rien de visible.
                        retirer = ch.locked
                            && ui::Bouton::new("Retirer le verrou").icone(Icon::Close).show(ui).clicked();
                    });
                    ui::hint(
                        ui,
                        "seuls ceux qui ont le mot de passe entrent, admins compris ; \
                         le propriétaire seul passe sans, et lui seul y déplace du monde. \
                         Ton ki-chat retient celui-ci pour toi.",
                    );
                });
                if poser || retirer {
                    let v = self.admin.verrou.take().expect("vérifié à l'instant");
                    if poser {
                        self.mots_de_passe_vocaux.insert(v.channel, v.mot_de_passe.clone());
                    }
                    to_send.push(ClientMsg::AdminSetVoicePassword {
                        channel: v.channel,
                        password: poser.then_some(v.mot_de_passe),
                        ttl_secs: v.ttl_secs,
                    });
                }
            }
        }
        ui::hint(
            ui,
            "supprimer un salon n'efface pas son historique : le fichier est archivé",
        );
    }

    /// Onglet « Rôles » : couleur, rang, permissions.
    pub(crate) fn admin_roles_tab(&mut self, ui: &mut egui::Ui, to_send: &mut Vec<ClientMsg>) {
        // Du plus haut au plus bas : l'ordre est ce qui décide qui agit sur
        // qui, autant qu'il se lise.
        let mut roles = self.roles.clone();
        roles.sort_by_key(|r| std::cmp::Reverse(r.rank));
        ui::group_title(ui, Icon::Crown, "Rôles");
        ui::hint(ui, "du plus haut au plus bas : on n'agit que sur qui est plus bas que soi");
        for role in &roles {
            ui.horizontal(|ui| {
                let color = theme::member_color(role.color, &role.name);
                ui::status_dot(ui, color, "", 10.0);
                ui.label(Texte::Corps.gras(ui, &role.name).color(color));
                ui::hint(ui, format!("rang {}", role.rank));
                if role.system {
                    ui::hint(ui, "système");
                }
                // Qui le porte : @everyone, tout le monde ; les autres, la
                // liste au survol.
                if role.id != ki_protocol::ROLE_EVERYONE {
                    let porteurs: Vec<&str> = self
                        .members
                        .iter()
                        .filter(|m| !m.invite && m.roles.contains(&role.id))
                        .map(|m| m.username.as_str())
                        .collect();
                    let n = porteurs.len();
                    let compte = match n {
                        0 => "personne".to_string(),
                        1 => "1 membre".to_string(),
                        n => format!("{n} membres"),
                    };
                    let reponse = ui.label(Texte::Aide.riche(ui, compte));
                    if n > 0 {
                        reponse.on_hover_text(porteurs.join(", "));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // On ne touche qu'à ce qui est strictement sous son
                    // propre rang : sinon on se donnerait des pouvoirs.
                    if self.outranks(role.rank)
                        && !role.system
                        && ui::Bouton::icone_seule(Icon::Trash).cote(24.0).bulle("Supprimer").show(ui).clicked()
                    {
                        to_send.push(ClientMsg::AdminDeleteRole { id: role.id });
                    }
                    // `@everyone` se modifie aussi, et c'est indispensable :
                    // les permissions étant une union, c'est le seul endroit
                    // d'où l'on peut retirer un droit à tout le monde. Son nom
                    // et son rang restent figés, le serveur les refuse — seul
                    // l'administrateur y touche, pour la même raison.
                    let everyone = role.id == ki_protocol::ROLE_EVERYONE
                        && self.can(ki_protocol::perm::ADMINISTRATOR);
                    if ((self.outranks(role.rank) && !role.system) || everyone)
                        && ui::Bouton::icone_seule(Icon::Pencil).cote(24.0).bulle("Modifier").show(ui).clicked()
                    {
                        self.admin.role = Some(RoleDraft {
                            id: Some(role.id),
                            name: role.name.clone(),
                            color: theme::member_color(role.color, &role.name),
                            colored: role.color.is_some(),
                            rank: role.rank,
                            rank_saved: Some(role.rank),
                            perms: role.perms,
                        });
                    }
                });
            });
        }

        ui.add_space(espace::L);
        if self.admin.role.is_none() && ui::Bouton::new("Nouveau rôle").icone(Icon::Plus).show(ui).clicked() {
            self.admin.role = Some(RoleDraft {
                id: None,
                name: String::new(),
                color: ACCENT,
                colored: true,
                // Par défaut tout en bas : le premier rang libre au-dessus
                // d'@everyone, comme dans Discord. Il naissait juste sous son
                // auteur — au-dessus des modérateurs quand le propriétaire le
                // créait : un rôle de couleur suffisait à mettre ceux qui le
                // portent hors de portée de la modération, sans que rien ne
                // le dise. Libre, parce que deux rôles ne partagent pas un
                // rang : à égalité, personne n'agit sur l'autre.
                rank: ki_protocol::lowest_free_rank(&self.roles, self.my_rank.saturating_sub(1)).unwrap_or(0),
                rank_saved: None,
                perms: ki_protocol::perm::DEFAULT,
            });
        }

        let Some(mut draft) = self.admin.role.take() else { return };
        let mut keep = true;
        ui.add_space(espace::M);
        ui::hairline(ui);
        ui.add_space(espace::M);
        // `@everyone` : ni nom ni rang ne se changent, le serveur les refuse.
        // On règle uniquement ce que reçoit tout le monde.
        let everyone = draft.id == Some(ki_protocol::ROLE_EVERYONE);
        ui::field_label(ui, "Nom");
        ui.add_enabled_ui(!everyone, |ui| {
            ui.add(ui::champ(&mut draft.name, "ex. Modérateur"));
        });
        ui.add_space(espace::S);
        ui.horizontal(|ui| {
            ui.checkbox(&mut draft.colored, "Couleur de pseudo");
            if draft.colored {
                ui.color_edit_button_srgba(&mut draft.color);
            }
        });
        ui.add_space(espace::S);
        // Le rôle qui occupe déjà le rang choisi : le serveur refuse que deux
        // rôles le partagent, autant le dire avant d'enregistrer.
        let mut taken = None;
        if everyone {
            ui::hint(ui, "ce que reçoit tout le monde, y compris les comptes sans rôle");
        } else {
            let ceiling = self.my_rank.saturating_sub(1);
            ui.add(egui::Slider::new(&mut draft.rank, 0..=ceiling.max(1)).text("rang"));
            ui::hint(ui, "un rang supérieur l'emporte : on n'agit que sur plus bas que soi");
            // Où il tombe parmi les autres : un nombre seul ne dit pas s'il
            // passe au-dessus des modérateurs.
            let autres: Vec<&ki_protocol::RoleInfo> =
                self.roles.iter().filter(|r| Some(r.id) != draft.id).collect();
            let dessous = autres.iter().filter(|r| r.rank < draft.rank).max_by_key(|r| r.rank);
            let dessus = autres.iter().filter(|r| r.rank > draft.rank).min_by_key(|r| r.rank);
            let place = match (dessous, dessus) {
                (Some(b), Some(h)) => format!("entre « {} » et « {} »", safe_name(&b.name), safe_name(&h.name)),
                (Some(b), None) => format!("au-dessus de « {} »", safe_name(&b.name)),
                (None, Some(h)) => format!("en dessous de « {} »", safe_name(&h.name)),
                (None, None) => String::new(),
            };
            if !place.is_empty() {
                ui::hint(ui, place);
            }
            if Some(draft.rank) != draft.rank_saved {
                taken = ki_protocol::rank_holder(&self.roles, draft.rank, draft.id)
                    .map(|r| r.name.clone());
            }
            if let Some(other) = &taken {
                ui.label(
                    Texte::Aide.riche_ton(ui, Ton::Danger, format!(
                        "rang déjà pris par « {} » : à rang égal, aucun des deux ne pourrait agir sur l'autre",
                        safe_name(other)
                    )),
                );
            }
        }
        ui.add_space(espace::S);
        ui::field_label(ui, "Permissions");
        for (bit, label, why) in ki_protocol::perm::ALL {
            // On n'accorde pas ce qu'on n'a pas soi-même : sans cette borne,
            // gérer les rôles suffirait à devenir administrateur.
            if !self.can(*bit) {
                continue;
            }
            // Et certaines n'ont pas de sens accordées à tout le monde : les
            // poser sur `@everyone` mettrait le serveur à plat, sans retour
            // possible. Le serveur les refuse, autant ne pas les proposer.
            if everyone && ki_protocol::perm::NOT_FOR_EVERYONE & bit != 0 {
                continue;
            }
            let mut on = draft.perms & bit != 0;
            let response = ui.checkbox(&mut on, *label);
            if !why.is_empty() {
                response.on_hover_text(*why);
            }
            if on {
                draft.perms |= bit;
            } else {
                draft.perms &= !bit;
            }
        }
        ui.add_space(espace::M);
        ui.horizontal(|ui| {
            let ready = !draft.name.trim().is_empty() && taken.is_none();
            let save = ui
                .add_enabled_ui(ready, |ui| {
                    ui::Bouton::new("Enregistrer").icone(Icon::Check).principal().show(ui)
                })
                .inner
                .clicked();
            if save {
                let color = draft.colored.then(|| {
                    let c = draft.color;
                    ((c.r() as u32) << 16) | ((c.g() as u32) << 8) | c.b() as u32
                });
                to_send.push(match draft.id {
                    Some(id) => ClientMsg::AdminEditRole {
                        role: ki_protocol::RoleInfo {
                            id,
                            name: draft.name.trim().to_string(),
                            color,
                            rank: draft.rank,
                            perms: draft.perms,
                            system: false,
                        },
                    },
                    None => ClientMsg::AdminCreateRole {
                        name: draft.name.trim().to_string(),
                        color,
                        rank: draft.rank,
                        perms: draft.perms,
                    },
                });
                keep = false;
            }
            if ui::Bouton::new("Annuler").icone(Icon::Close).show(ui).clicked() {
                keep = false;
            }
        });
        // Remis en place seulement si l'édition continue : enregistrer ou
        // annuler referme le formulaire.
        if keep {
            self.admin.role = Some(draft);
        }
    }

    /// Onglet « Invitations » : créer, copier, révoquer.
    pub(crate) fn admin_invites_tab(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        to_send: &mut Vec<ClientMsg>,
    ) {
        ui::group_title(ui, Icon::Plus, "Nouveau code");
        ui::field_label(ui, "Étiquette (pour s'y retrouver plus tard)");
        ui.add(ui::champ(&mut self.admin.invitation_libelle, "ex. tournoi du samedi"));
        ui.add_space(espace::M);

        ui::field_label(ui, "Nombre d'utilisations");
        ui::segmente(
            ui,
            &mut self.admin.invitation_usages,
            &[(Some(1u32), "1"), (Some(5), "5"), (Some(25), "25"), (None, "Illimité")],
        );
        if self.admin.invitation_usages.is_none() {
            ui::hint(ui, "lien permanent — chaque compte créé sera consigné au journal");
        }
        ui.add_space(espace::M);
        // Gérer les invitations sans pouvoir en créer : on révoque, on ne
        // génère pas.
        let peut_creer = self.can(ki_protocol::perm::CREATE_INVITE);
        let generer = ui
            .add_enabled_ui(peut_creer, |ui| ui::Bouton::new("Générer le code").icone(Icon::Plus).show(ui))
            .inner
            .on_disabled_hover_text("il faut la permission « Créer des invitations »");
        if generer.clicked() {
            to_send.push(ClientMsg::AdminCreateInvite {
                uses: self.admin.invitation_usages,
                label: self.admin.invitation_libelle.trim().to_string(),
                ttl_secs: 0,
            });
            self.admin.invitation_libelle.clear();
        }

        if let Some(code) = self.admin.derniere_invitation.clone() {
            ui.add_space(espace::M);
            ui::encart(ui, ACCENT, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(&code).color(ACCENT).strong().monospace().size(texte::TITRE),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui::Bouton::icone_seule(Icon::Copy).bulle("Copier « serveur + code »").show(ui)
                                .clicked()
                            {
                                ctx.copy_text(format!(
                                    "serveur : {}  |  code d'invitation : {code}",
                                    self.url
                                ));
                            }
                            if ui::Bouton::icone_seule(Icon::Key).bulle("Copier le code").show(ui).clicked() {
                                ctx.copy_text(code.clone());
                            }
                        });
                    });
                });
        }

        let invites = self.admin.invitations.clone();
        if !invites.is_empty() {
            ui.add_space(espace::L);
            ui::hairline(ui);
            ui.add_space(espace::L);
            ui::group_title(ui, Icon::Key, "Codes émis");
            for invite in &invites {
                ui.horizontal(|ui| {
                    let color = if invite.revoked { TEXT_FAINT } else { TEXT_DIM };
                    let mut code = RichText::new(&invite.code).color(color).monospace().size(texte::CORPS);
                    if invite.revoked {
                        code = code.strikethrough();
                    }
                    ui.label(code);
                    // Un lien permanent se signale : c'est l'information
                    // qui décide s'il faut le révoquer.
                    match invite.uses_left {
                        None if !invite.revoked => {
                            ui.label(Texte::Aide.riche_ton(ui, Ton::Accent, "permanent"));
                        }
                        Some(left) if !invite.revoked => {
                            ui.label(
                                Texte::Aide.riche(ui, format!("{left} restant(s)")),
                            );
                        }
                        _ => {
                            ui.label(Texte::Aide.riche_ton(ui, Ton::Danger, "révoqué"));
                        }
                    }
                    if invite.uses > 0 {
                        ui.label(
                            Texte::Aide.riche(ui, format!("· {} compte(s) créé(s)", invite.uses)),
                        );
                    }
                    if !invite.label.is_empty() {
                        ui.label(
                            Texte::Aide.riche(ui, format!("· {}", invite.label)),
                        );
                    }
                    // Ses propres codes, qui sait en créer les révoque ;
                    // ceux des autres, il faut gérer les invitations.
                    let revocable = self.can(ki_protocol::perm::MANAGE_INVITES)
                        || invite.created_by == self.username.trim();
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !invite.revoked
                            && revocable
                            && ui::Bouton::icone_seule(Icon::Ban).cote(24.0).bulle("Révoquer").show(ui).clicked()
                        {
                            to_send
                                .push(ClientMsg::AdminRevokeInvite { code: invite.code.clone() });
                        }
                        if ui::Bouton::icone_seule(Icon::Copy).cote(24.0).bulle("Copier").show(ui).clicked() {
                            ctx.copy_text(invite.code.clone());
                        }
                    });
                });
            }
        }
    }

    /// Onglet « Membres » : bannir, débannir, réinitialiser un mot de passe.
    pub(crate) fn admin_members_tab(&mut self, ui: &mut egui::Ui, to_send: &mut Vec<ClientMsg>) {
        ui::group_title(ui, Icon::User, "Comptes");

        let users = self.admin.comptes.clone();
        let my_name = users
            .iter()
            .find(|u| Some(u.user_id) == self.my_id)
            .map(|u| u.username.clone());
        for account in &users {
            ui.horizontal(|ui| {
                let photo = self.avatars.get(&account.user_id);
                ui::Avatar::new(&account.username, 26.0).photo(photo.map(|(_, t)| t)).show(ui);
                let dot = if account.online { SPEAK } else { theme::BORDER };
                ui::status_dot(ui, dot, "", 8.0);

                // La couleur du compte vient de son rôle le mieux classé,
                // comme partout ailleurs dans l'application.
                let account_color = theme::member_color(
                    self.roles
                        .iter()
                        .filter(|r| account.roles.contains(&r.id))
                        .max_by_key(|r| r.rank)
                        .and_then(|r| r.color),
                    &account.username,
                );
                let mut name =
                    RichText::new(&account.username).color(account_color).size(texte::CORPS);
                if account.banned {
                    name = name.strikethrough();
                }
                ui.label(name);
                if account.admin {
                    ui::glyph(ui, Icon::Crown, 13.0, ACCENT);
                }
                for role in self.roles.iter().filter(|r| account.roles.contains(&r.id)) {
                    ui.label(
                        RichText::new(&role.name)
                            .color(theme::member_color(role.color, &role.name))
                            .size(texte::MINUSCULE),
                    );
                }
                if account.banned {
                    ui.label(Texte::Aide.riche_ton(ui, Ton::Danger, ban_summary(account)));
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Mêmes bornes que le serveur : la permission, et le
                    // rang strictement supérieur. Le bouton n'apparaît que
                    // s'il aboutirait.
                    if self.can(ki_protocol::perm::MANAGE_ROLES)
                        && self.outranks(account.rank)
                        && ui::Bouton::icone_seule(Icon::Crown).cote(26.0).bulle("Rôles…").show(ui).clicked()
                    {
                        self.admin.roles_cible = Some(account.username.clone());
                        self.admin.roles_choisis = account.roles.clone();
                    }
                    // Mêmes bornes que le serveur, ici aussi : la permission
                    // **et** le rang, et jamais sur soi-même. Sans elles, ces
                    // boutons s'affichaient à qui n'avait que « Expulser »,
                    // ouvraient une fenêtre, et le clic final se faisait
                    // refuser — ou, pour le bannissement de soi, aboutissait.
                    let myself = my_name.as_ref() == Some(&account.username);
                    let can_ban = self.can(ki_protocol::perm::BAN)
                        && self.outranks(account.rank)
                        && !myself;
                    if can_ban {
                        if account.banned {
                            if ui::Bouton::icone_seule(Icon::Check).cote(26.0).bulle("Annuler le ban").show(ui)
                                .clicked()
                            {
                                to_send.push(ClientMsg::AdminUnban {
                                    username: account.username.clone(),
                                });
                            }
                        } else if ui::Bouton::icone_seule(Icon::Ban).cote(26.0).bulle("Bannir…").show(ui)
                            .clicked()
                        {
                            self.admin.ban = Some(BanDraft {
                                username: account.username.clone(),
                                reason: String::new(),
                                duration_secs: 86_400,
                            });
                        }
                    }
                    // On règle toujours son propre mot de passe ; celui d'un
                    // autre demande la permission et le rang.
                    let can_reset = self.can(ki_protocol::perm::RESET_PASSWORD)
                        && (myself || self.outranks(account.rank));
                    if can_reset
                        && ui::Bouton::icone_seule(Icon::Key).cote(26.0).bulle("Réinitialiser le mot de passe").show(ui)
                        .clicked()
                    {
                        self.admin.reset_cible = Some(account.username.clone());
                        self.admin.reset_mot_de_passe.clear();
                    }
                });
            });
            // Motif et auteur sous la ligne : c'est ce qu'on cherche quand
            // on revient sur un bannissement des semaines plus tard.
            if account.banned && !(account.ban_reason.is_empty() && account.ban_by.is_empty()) {
                ui.horizontal(|ui| {
                    ui.add_space(34.0);
                    let mut detail = String::new();
                    if !account.ban_reason.is_empty() {
                        detail.push_str(&safe_name(&account.ban_reason));
                    }
                    if !account.ban_by.is_empty() {
                        if !detail.is_empty() {
                            detail.push_str(" — ");
                        }
                        detail.push_str(&format!("par {}", safe_name(&account.ban_by)));
                    }
                    ui::hint(ui, detail);
                });
            }
        }

        // --- Attribution des rôles ---
        if let Some(target) = self.admin.roles_cible.clone() {
            ui.add_space(espace::L);
            ui::hairline(ui);
            ui.add_space(espace::M);
            ui::field_label(ui, &format!("Rôles de {target}"));
            let roles = self.roles.clone();
            let mut assignable = 0;
            for role in &roles {
                // `@everyone` est implicite : il n'est jamais stocké sur un
                // compte, et le proposer laisserait croire qu'on peut le
                // retirer à quelqu'un.
                if role.id == ki_protocol::ROLE_EVERYONE {
                    continue;
                }
                // On n'attribue qu'un rôle strictement sous son propre rang :
                // sinon on se donnerait, par personne interposée, un pouvoir
                // qu'on n'a pas.
                if !self.outranks(role.rank) {
                    continue;
                }
                assignable += 1;
                let mut on = self.admin.roles_choisis.contains(&role.id);
                let label = RichText::new(&role.name)
                    .color(theme::member_color(role.color, &role.name));
                if ui.checkbox(&mut on, label).changed() {
                    if on {
                        self.admin.roles_choisis.push(role.id);
                    } else {
                        self.admin.roles_choisis.retain(|r| *r != role.id);
                    }
                }
            }
            if assignable == 0 {
                ui::hint(ui, "aucun rôle sous ton rang n'est attribuable");
            }
            ui.add_space(espace::S);
            ui.horizontal(|ui| {
                if ui::Bouton::new("Appliquer").icone(Icon::Check).principal().show(ui).clicked() {
                    to_send.push(ClientMsg::AdminSetUserRoles {
                        username: target.clone(),
                        roles: self.admin.roles_choisis.clone(),
                    });
                    self.admin.roles_cible = None;
                    self.admin.roles_choisis.clear();
                }
                if ui::Bouton::new("Annuler").icone(Icon::Close).show(ui).clicked() {
                    self.admin.roles_cible = None;
                    self.admin.roles_choisis.clear();
                }
            });
        }

        // --- Formulaire de réinitialisation ---
        if let Some(target) = self.admin.reset_cible.clone() {
            ui.add_space(espace::L);
            ui::hairline(ui);
            ui.add_space(espace::M);
            ui::field_label(ui, &format!("Nouveau mot de passe pour {target}"));
            ui.horizontal(|ui| {
                let champ = ui.add(
                    egui::TextEdit::singleline(&mut self.admin.reset_mot_de_passe)
                        .password(true)
                        .margin(marge::symetrique(espace::L, espace::M))
                        .background_color(theme::BG_DEEP)
                        .desired_width(170.0),
                );
                menu_edition(&champ, &mut self.admin.reset_mot_de_passe, true);
                let ok = self.admin.reset_mot_de_passe.len() >= 6;
                let clicked = ui
                    .add_enabled_ui(ok, |ui| {
                        ui::Bouton::new("Appliquer").icone(Icon::Check).principal().show(ui)
                    })
                    .inner
                    .clicked();
                if clicked {
                    to_send.push(ClientMsg::AdminResetPassword {
                        username: target.clone(),
                        new_password: self.admin.reset_mot_de_passe.clone(),
                    });
                    self.admin.reset_cible = None;
                    self.admin.reset_mot_de_passe.clear();
                }
                if ui::Bouton::new("Annuler").icone(Icon::Close).show(ui).clicked() {
                    self.admin.reset_cible = None;
                    self.admin.reset_mot_de_passe.clear();
                }
            });
            if self.admin.reset_mot_de_passe.len() < 6 && !self.admin.reset_mot_de_passe.is_empty() {
                ui::hint(ui, "6 caractères minimum");
            }
        }
    }

    /// Onglet « Journal » : les actions d'administration, du plus récent au
    /// plus ancien. C'est ce qui rend un lien d'invitation permanent
    /// acceptable — on sait toujours qui est entré par où.
    /// Admin → Fichiers : la liste et ses actions ; ouvrir un fichier passe
    /// par la visionneuse, sur l'adresse HTTPS du serveur.
    pub(crate) fn admin_fichiers_tab(&mut self, ui: &mut egui::Ui, to_send: &mut Vec<ClientMsg>) {
        for action in self.admin.fichiers.onglet(ui) {
            match action {
                admin_fichiers::Action::Envoyer(msg) => to_send.push(msg),
                admin_fichiers::Action::Voir { chemin, video } => {
                    let url = format!("{}{chemin}", self.http_base());
                    let cible = if video {
                        visionneuse::Cible::Video(url)
                    } else {
                        visionneuse::Cible::Image(url)
                    };
                    self.visionneuse.ouvrir(cible.clone(), vec![cible]);
                }
                admin_fichiers::Action::Copier(chemin) => {
                    ui.ctx().copy_text(format!("{}{chemin}", self.http_base()));
                    self.info = Some("lien copié".into());
                }
            }
        }
    }

    pub(crate) fn admin_audit_tab(&mut self, ui: &mut egui::Ui) {
        if self.admin.audit.is_empty() {
            ui::hint(ui, "aucune action consignée pour l'instant");
            return;
        }
        let records = self.admin.audit.clone();
        for record in &records {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    Texte::Aide.riche(ui, format!("{} {}", day_label(record.ts), format_time(record.ts))).monospace(),
                );
                ui.label(
                    Texte::Libelle.gras(ui, audit_label(&record.action)).color(audit_tone(&record.action)),
                );
                let actor = if record.actor.is_empty() {
                    "le serveur".to_string()
                } else {
                    safe_name(&record.actor)
                };
                ui.label(Texte::Libelle.riche(ui, format!("par {actor}")));
                if !record.target.is_empty() {
                    ui.label(
                        Texte::Libelle.riche(ui, format!("→ {}", safe_name(&record.target))),
                    );
                }
                if !record.detail.is_empty() {
                    ui.label(
                        Texte::Aide.riche(ui, ki_protocol::safe_display(&record.detail, 200)),
                    );
                }
            });
            ui.add_space(espace::XXS);
        }
    }

    pub(crate) fn close_admin(&mut self) {
        // Rouvrir redemande le tableau : l'état d'un serveur bouge.
        self.admin.tableau_demande = None;
        self.admin.reseau_demande = None;
        self.admin.ouverte = false;
        self.info = None;
        self.admin.derniere_invitation = None;
        self.admin.icone = IconChange::Keep;
        self.preview_icons.remove(&Apercu::LogoServeur);
    }

    /// Saisie d'un bannissement : motif et durée.
    ///
    /// Une fenêtre plutôt qu'un menu : bannir se justifie, et le motif est
    /// relu par la personne bannie comme par les autres admins dans le
    /// journal. Lui demander de le taper à la volée dans un menu contextuel
    /// donnerait des motifs vides.
    pub(crate) fn ban_window(&mut self, ctx: &egui::Context) {
        let Some(draft) = self.admin.ban.as_mut() else { return };
        let username = draft.username.clone();
        let mut open = true;
        let mut confirm = false;
        let mut cancel = false;

        egui::Window::new(format!("Bannir {username}"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(320.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui::field_label(ui, "Motif (visible par la personne bannie)");
                ui.add(ui::champ(&mut draft.reason, "ex. spam répété"));
                ui.add_space(espace::M);

                ui::field_label(ui, "Durée");
                let durees: Vec<_> = BAN_DURATIONS.iter().map(|(label, secs)| (*secs, *label)).collect();
                ui::pastilles(ui, &mut draft.duration_secs, &durees);
                ui.add_space(espace::L);
                ui::hairline(ui);
                ui.add_space(espace::M);

                ui.horizontal(|ui| {
                    if ui::Bouton::new("Bannir").icone(Icon::Ban).teinte(Ton::Danger).show(ui).clicked() {
                        confirm = true;
                    }
                    if ui::Bouton::new("Annuler").icone(Icon::Close).show(ui).clicked() {
                        cancel = true;
                    }
                });
            });

        if confirm {
            if let Some(draft) = self.admin.ban.take() {
                self.send(ClientMsg::AdminBan {
                    username: draft.username,
                    reason: draft.reason,
                    duration_secs: draft.duration_secs,
                });
            }
        } else if cancel || !open {
            self.admin.ban = None;
        }
    }
}
