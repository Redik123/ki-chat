//! Les portes web, côté client : la bannière « Kevin veut rejoindre par le
//! web », le panneau qui ouvre et ferme les portes, et le menu d'un invité.
//!
//! Une porte est un lien `https://<serveur>/s/<slug>` vers un salon textuel
//! temporaire. Quelqu'un y frappe depuis son navigateur en donnant un nom,
//! un membre l'accepte, et il écrit là sans compte (voir `ki_protocol`,
//! « Les portes web »). Ce module ne connaît pas l'application : il rend,
//! et rapporte des [`Action`]s que `main.rs` applique — envoyer un message,
//! copier un lien, ouvrir un salon. Même patron que le soundboard ou la
//! visionneuse : jamais de `&mut KiApp` ici.
//!
//! Tout ce qu'il sait vient du serveur : `PorteOuverte` (le lien, à l'hôte
//! seul), `PorteDemande` (quelqu'un frappe), `PorteEtat` (la liste des
//! invités et des demandes, à chaque changement) et `PorteFermee`. Le
//! serveur dit dès son `Welcome` s'il sert les portes (`portes: true`) ;
//! face à un serveur antérieur, le champ manque, `disponible` reste faux
//! et aucun de ces messages n'arrive : le panneau ne se montre pas, et
//! rien de nouveau ne part.

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, CornerRadius, Rect, RichText, Sense, Stroke, Vec2};
use ki_protocol::{ChannelId, ChannelInfo, ClientMsg, DemandeWeb, InviteWeb, UserId};

use crate::icons::{self, Icon};
use crate::theme::{self, ACCENT, TEXT, TEXT_DIM, TEXT_FAINT, WARN};
use crate::ui::{self, Tone};

/// Combien de temps une demande reste affichée sans nouvelles : le serveur
/// laisse cinq minutes à celui qui frappe, puis ferme sa page. Passé ce
/// délai, répondre n'aurait plus de destinataire — la bannière s'efface.
/// Un `PorteEtat` qui ne la liste plus (quelqu'un d'autre a répondu)
/// l'efface avant.
pub const DUREE_DEMANDE: Duration = Duration::from_secs(5 * 60);

/// Les durées proposées à l'ouverture, en minutes. La dernière est le
/// plafond du serveur ([`ki_protocol::PORTE_TTL_MAX_SECS`]) : une soirée
/// entière — une partie avec un invité a duré cinq heures et demie.
const DUREES: [(u64, &str); 4] = [(30, "30 min"), (60, "1 h"), (120, "2 h"), (360, "6 h")];

/// Le côté du QR code du lien, en points.
const COTE_QR: f32 = 160.0;

/// Quelqu'un qui frappe, tel que la bannière le montre.
pub struct Demande {
    pub slug: String,
    pub demande_id: u64,
    /// Déjà passé par `safe_display` : ce qui s'affiche, pas ce qui est
    /// arrivé.
    pub nom: String,
    pub ip_masquee: String,
    pub recue: Instant,
}

impl Demande {
    fn reste(&self) -> Duration {
        DUREE_DEMANDE.saturating_sub(self.recue.elapsed())
    }
}

/// Une porte que le serveur nous décrit : la sienne (on l'a ouverte), ou
/// celle d'un autre quand on détient « Expulser ».
pub struct Porte {
    pub slug: String,
    pub salon: ChannelId,
    pub invites: Vec<InviteWeb>,
    pub demandes: Vec<DemandeWeb>,
    /// Fermeture au plus tard (ms Unix).
    pub expire_le: u64,
    /// Le lien complet, tel qu'on le montre et qu'on le copie : donné à
    /// l'ouverture (`PorteOuverte`), puis dans chaque état (`PorteEtat`,
    /// depuis 0.1.45).
    pub url: Option<String>,
    /// Le QR code du lien, dessiné à la première ouverture du panneau.
    qr: Option<egui::TextureHandle>,
    /// Ouverte par moi : j'y ai tous les droits, même sans « Expulser ».
    pub mienne: bool,
}

/// Ce que le panneau, la bannière ou le menu demandent à l'application.
pub enum Action {
    Envoyer(ClientMsg),
    Copier(String),
    /// Lire ce salon — celui de la porte qu'on vient d'ouvrir, ou de la
    /// demande qu'on vient d'accepter.
    Lire(ChannelId),
    /// « Lui offrir ki-chat » : le serveur crée l'invitation, poste le lien
    /// dans le salon de la porte et pousse le code à la page de l'invité.
    Offrir { invite_id: UserId },
}

/// Ce que l'application sait et que le rendu doit connaître : mes
/// permissions, mon salon vocal, les salons pour nommer les vocaux.
pub struct Contexte<'a> {
    /// « Créer des invitations » : ouvrir une porte, offrir ki-chat.
    pub peut_ouvrir: bool,
    /// « Expulser » : répondre aux demandes, expulser, amener un invité
    /// dans son vocal ou l'en sortir, fermer les portes des autres — tout
    /// ce que le serveur réserve à l'hôte de la porte ou à qui peut
    /// expulser. « Déplacer en vocal » n'y suffit pas : le serveur le
    /// refuserait.
    pub peut_expulser: bool,
    pub mon_vocal: Option<ChannelId>,
    pub salons: &'a [ChannelInfo],
    /// La base des liens : l'adresse web réglée par un admin, sinon celle
    /// par laquelle on joint le serveur — pour montrer le lien avant même
    /// d'ouvrir la porte.
    pub base_web: String,
}

impl Contexte<'_> {
    fn nom_salon(&self, id: ChannelId) -> String {
        self.salons
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| format!("salon {id}"))
    }
}

pub struct Portes {
    /// La fenêtre « Portes web » est ouverte.
    pub ouvert: bool,
    /// Le serveur en face sait servir les portes : il l'a dit dans son
    /// `Welcome` (`portes: true`), ou n'importe quel message de porte est
    /// arrivé. Avant ça, rien de nouveau ne part — un serveur antérieur
    /// répondrait « message invalide » — et le panneau reste caché. Un
    /// `NonLus` reçu ne prouve rien : la 0.1.43 l'envoie déjà.
    pub disponible: bool,
    demandes: Vec<Demande>,
    portes: Vec<Porte>,
    /// Le formulaire d'ouverture.
    slug: String,
    nom_salon: String,
    duree_min: u64,
    /// Un mot pour le panneau : porte ouverte, porte fermée et pourquoi.
    message: Option<String>,
}

impl Default for Portes {
    fn default() -> Self {
        Self::new()
    }
}

impl Portes {
    pub fn new() -> Self {
        Self {
            ouvert: false,
            disponible: false,
            demandes: Vec::new(),
            portes: Vec::new(),
            slug: String::new(),
            nom_salon: String::new(),
            duree_min: 60,
            message: None,
        }
    }

    /// Les portes sont celles d'un serveur : à la déconnexion, tout tombe,
    /// et le suivant devra prouver à nouveau qu'il les sert.
    pub fn reinitialiser(&mut self) {
        *self = Self::new();
    }

    /// Rien à montrer : ni porte connue, ni demande en attente.
    pub fn est_vide(&self) -> bool {
        self.portes.is_empty() && self.demandes.is_empty()
    }

    fn porte_mut_ou_cree(&mut self, slug: &str, salon: ChannelId) -> &mut Porte {
        let i = match self.portes.iter().position(|p| p.slug == slug) {
            Some(i) => i,
            None => {
                self.portes.push(Porte {
                    slug: slug.to_string(),
                    salon,
                    invites: Vec::new(),
                    demandes: Vec::new(),
                    expire_le: 0,
                    url: None,
                    qr: None,
                    mienne: false,
                });
                self.portes.len() - 1
            }
        };
        &mut self.portes[i]
    }

    /// `PorteOuverte` : la porte que je viens d'ouvrir, avec son lien. Le
    /// panneau s'ouvre pour le montrer — c'est ce qu'on attendait.
    pub fn ouverte(&mut self, slug: &str, url: String, salon: ChannelId, expire_le: u64) {
        self.disponible = true;
        let porte = self.porte_mut_ou_cree(slug, salon);
        porte.salon = salon;
        porte.expire_le = expire_le;
        porte.url = Some(url);
        porte.qr = None;
        porte.mienne = true;
        self.slug.clear();
        self.nom_salon.clear();
        self.message = Some(format!("porte « {slug} » ouverte : le lien est prêt à partager"));
        self.ouvert = true;
    }

    /// `PorteDemande` : quelqu'un frappe. Vrai si c'est une nouvelle
    /// demande (un serveur qui la répète ne fait pas deux bannières).
    pub fn demande(&mut self, slug: &str, demande_id: u64, nom: &str, ip_masquee: &str) -> bool {
        self.disponible = true;
        if self.demandes.iter().any(|d| d.demande_id == demande_id) {
            return false;
        }
        self.demandes.push(Demande {
            slug: slug.to_string(),
            demande_id,
            nom: nom.to_string(),
            ip_masquee: ip_masquee.to_string(),
            recue: Instant::now(),
        });
        true
    }

    /// `PorteEtat` : la liste fait foi. Une demande que l'état ne liste
    /// plus a eu sa réponse (d'un autre, ou expirée) : sa bannière tombe.
    pub fn etat(
        &mut self,
        slug: &str,
        salon: ChannelId,
        invites: Vec<InviteWeb>,
        demandes: Vec<DemandeWeb>,
        expire_le: u64,
        url: Option<String>,
    ) {
        self.disponible = true;
        self.demandes.retain(|d| {
            d.slug != slug || demandes.iter().any(|x| x.demande_id == d.demande_id)
        });
        let porte = self.porte_mut_ou_cree(slug, salon);
        porte.salon = salon;
        porte.invites = invites;
        porte.demandes = demandes;
        if expire_le != 0 {
            porte.expire_le = expire_le;
        }
        // Le lien voyage aussi dans l'état : qui gère la porte sans l'avoir
        // ouverte le voit, l'hôte le retrouve après une reconnexion, et il
        // suit l'adresse publique quand un admin la change — QR compris.
        if let Some(url) = url {
            if porte.url.as_deref() != Some(url.as_str()) {
                porte.url = Some(url);
                porte.qr = None;
            }
        }
    }

    /// `PorteFermee` : la porte disparaît, ses demandes avec.
    pub fn fermee(&mut self, slug: &str, motif: &str) {
        self.disponible = true;
        self.portes.retain(|p| p.slug != slug);
        self.demandes.retain(|d| d.slug != slug);
        self.message = Some(if motif.is_empty() {
            format!("porte « {slug} » fermée")
        } else {
            format!("porte « {slug} » fermée : {motif}")
        });
    }

    /// Les demandes trop vieilles pour qu'une réponse arrive encore.
    fn purger(&mut self) {
        self.demandes.retain(|d| d.recue.elapsed() < DUREE_DEMANDE);
    }

    /// Reste-t-il une bannière à faire vivre ? (Son compte à rebours
    /// demande une image par seconde.) Sans rien purger : le calcul du
    /// délai de la prochaine image n'a que `&self`.
    pub fn a_des_demandes(&self) -> bool {
        self.demandes.iter().any(|d| d.recue.elapsed() < DUREE_DEMANDE)
    }

    pub fn salon_de(&self, slug: &str) -> Option<ChannelId> {
        self.portes.iter().find(|p| p.slug == slug).map(|p| p.salon)
    }

    pub fn slug_du_salon(&self, salon: ChannelId) -> Option<&str> {
        self.portes.iter().find(|p| p.salon == salon).map(|p| p.slug.as_str())
    }

    pub fn porte_de_l_invite(&self, invite_id: UserId) -> Option<&Porte> {
        self.portes
            .iter()
            .find(|p| p.invites.iter().any(|i| i.invite_id == invite_id))
    }

    /// Puis-je fermer cette porte, expulser ses invités, répondre à ses
    /// demandes ? La mienne, toujours ; celle d'un autre avec « Expulser ».
    /// Le serveur tranche de toute façon : ici on ne fait que cacher ce
    /// qu'il refuserait.
    pub fn peut_fermer(&self, slug: &str, contexte: &Contexte) -> bool {
        contexte.peut_expulser || self.portes.iter().any(|p| p.slug == slug && p.mienne)
    }

    // -----------------------------------------------------------------
    // La bannière, en tête du salon
    // -----------------------------------------------------------------

    /// Une bannière par demande, empilées : « Kevin veut rejoindre par le
    /// web (porte salon1) — Accepter / Refuser », avec le temps qui reste.
    pub fn bannieres(&mut self, ui: &mut egui::Ui) -> Vec<Action> {
        self.purger();
        let mut actions = Vec::new();
        let mut reponse: Option<(usize, bool)> = None;
        for (i, d) in self.demandes.iter().enumerate() {
            ui.add_space(8.0);
            let texte = format!(
                "{} veut rejoindre par le web (porte {}) · {}",
                d.nom,
                d.slug,
                reste_court(d.reste()),
            );
            let survol = if d.ip_masquee.is_empty() {
                "quelqu'un frappe à une porte web : il n'a pas de compte, il ne verra que le salon temporaire".to_string()
            } else {
                format!("depuis l'adresse {} — il n'a pas de compte, il ne verra que le salon temporaire", d.ip_masquee)
            };
            match banniere_demande(ui, &texte, &survol) {
                Some(true) => reponse = Some((i, true)),
                Some(false) => reponse = Some((i, false)),
                None => {}
            }
        }
        if let Some((i, accepter)) = reponse {
            let d = self.demandes.remove(i);
            actions.push(Action::Envoyer(ClientMsg::PorteRepondre {
                demande_id: d.demande_id,
                accepter,
                motif: String::new(),
            }));
            // Accepter, c'est vouloir lui parler : on ouvre le salon de la
            // porte, s'il est connu.
            if accepter {
                if let Some(salon) = self.salon_de(&d.slug) {
                    actions.push(Action::Lire(salon));
                }
            }
        }
        actions
    }

    // -----------------------------------------------------------------
    // Le menu d'un invité (liste des membres)
    // -----------------------------------------------------------------

    /// Les actions sur un invité, dans son menu contextuel : le vocal,
    /// l'expulsion, l'offre. Ce qu'on ne peut pas faire ne s'affiche pas.
    pub fn menu_invite(
        &self,
        ui: &mut egui::Ui,
        invite_id: UserId,
        vocal: Option<ChannelId>,
        contexte: &Contexte,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        let porte = self.porte_de_l_invite(invite_id);
        let mienne = porte.is_some_and(|p| p.mienne);
        let droits = Droits {
            agir: mienne || contexte.peut_expulser,
            deplacer: mienne || contexte.peut_expulser,
            offrir: contexte.peut_ouvrir,
        };
        boutons_invite(ui, invite_id, vocal, droits, contexte, &mut actions);
        actions
    }

    // -----------------------------------------------------------------
    // Le panneau « Portes web »
    // -----------------------------------------------------------------

    /// La fenêtre, si elle est ouverte : ouvrir une porte, le lien et son
    /// QR, les portes ouvertes avec leurs invités et leurs demandes.
    pub fn fenetre(&mut self, ctx: &egui::Context, contexte: &Contexte) -> Vec<Action> {
        if !self.ouvert {
            return Vec::new();
        }
        self.purger();
        let mut actions = Vec::new();
        let mut ouvert = true;
        let maintenant = maintenant_ms();
        egui::Window::new("Portes web")
            .open(&mut ouvert)
            .default_width(500.0)
            .default_height(460.0)
            .min_width(380.0)
            .resizable(true)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui::hint(
                        ui,
                        "une porte est un lien vers un salon temporaire : qui l'ouvre dans son navigateur \
                         donne un nom et frappe, un membre le fait entrer — sans compte, sans installation",
                    );
                    ui.add_space(10.0);
                    if contexte.peut_ouvrir {
                        self.formulaire(ui, contexte, &mut actions);
                        ui.add_space(12.0);
                        ui::hairline(ui);
                        ui.add_space(10.0);
                    }
                    if self.portes.is_empty() {
                        ui::group_title(ui, Icon::Key, "Portes ouvertes");
                        ui::hint(ui, "aucune pour l'instant");
                    } else {
                        for porte in &mut self.portes {
                            carte_porte(ui, ctx, porte, contexte, maintenant, &mut actions);
                            ui.add_space(10.0);
                        }
                    }
                    if let Some(message) = self.message.clone() {
                        ui.add_space(8.0);
                        if ui::banner(ui, Tone::Info, &message, true) {
                            self.message = None;
                        }
                    }
                });
            });
        // « expire dans » et « il y a » vivent à la seconde.
        ctx.request_repaint_after(Duration::from_secs(1));
        self.ouvert = ouvert;
        actions
    }

    fn formulaire(&mut self, ui: &mut egui::Ui, contexte: &Contexte, actions: &mut Vec<Action>) {
        ui::group_title(ui, Icon::Plus, "Ouvrir une porte");
        ui::field_label(ui, "Nom de la porte (dans le lien)");
        if ui.add(ui::text_field(&mut self.slug, "ex. salon1", false)).changed() {
            self.slug = slug_normalise(&self.slug);
        }
        let valide = ki_protocol::slug_valide(&self.slug);
        let deja = self.portes.iter().any(|p| p.slug == self.slug);
        let plafond = self.portes.len() >= ki_protocol::PORTES_MAX;
        if !self.slug.is_empty() && !valide {
            ui::hint(
                ui,
                &format!(
                    "de {} à {} caractères : minuscules, chiffres et tirets — un mot qu'on dicte en vocal",
                    ki_protocol::PORTE_SLUG_MIN,
                    ki_protocol::PORTE_SLUG_MAX
                ),
            );
        } else if deja {
            ui::hint(ui, "cette porte est déjà ouverte");
        } else {
            let exemple = if self.slug.is_empty() { "salon1" } else { self.slug.as_str() };
            ui::hint(ui, &format!("le lien : {}/{exemple} — un mot qu'on dicte en vocal", contexte.base_web));
        }
        ui.add_space(8.0);
        ui::field_label(ui, "Nom du salon temporaire (facultatif)");
        ui.add(ui::text_field(&mut self.nom_salon, "sinon, celui de la porte", false));
        ui.add_space(8.0);
        ui::field_label(ui, "Durée");
        ui.horizontal_wrapped(|ui| {
            for (minutes, label) in DUREES {
                if ui.selectable_label(self.duree_min == minutes, label).clicked() {
                    self.duree_min = minutes;
                }
            }
        });
        ui::hint(ui, "la porte ferme d'elle-même au bout de ce temps, ou dix minutes après le départ du dernier invité ; le salon est alors effacé");
        ui.add_space(8.0);
        if plafond {
            ui::hint(ui, &format!("{} portes ouvertes, c'est le maximum", ki_protocol::PORTES_MAX));
        }
        ui.add_enabled_ui(valide && !deja && !plafond, |ui| {
            if ui::primary_button(ui, Some(Icon::Key), "Ouvrir la porte", None).clicked() {
                actions.push(Action::Envoyer(ClientMsg::PorteOuvrir {
                    slug: self.slug.clone(),
                    nom_salon: self.nom_salon.trim().to_string(),
                    ttl_secs: (self.duree_min * 60).min(ki_protocol::PORTE_TTL_MAX_SECS),
                }));
            }
        });
    }
}

/// Ce que je peux faire sur un invité donné.
#[derive(Clone, Copy)]
struct Droits {
    /// Expulser, répondre, fermer.
    agir: bool,
    /// Le mettre en vocal, l'en sortir.
    deplacer: bool,
    /// Lui offrir ki-chat.
    offrir: bool,
}

/// La carte d'une porte : le lien et son QR (pour l'hôte), les invités,
/// les demandes, la fermeture.
fn carte_porte(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    porte: &mut Porte,
    contexte: &Contexte,
    maintenant: u64,
    actions: &mut Vec<Action>,
) {
    let droits = Droits {
        agir: porte.mienne || contexte.peut_expulser,
        deplacer: porte.mienne || contexte.peut_expulser,
        offrir: contexte.peut_ouvrir,
    };
    ui::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui::glyph(ui, Icon::Key, 16.0, WARN);
            ui.label(RichText::new(format!("porte « {} »", porte.slug)).color(TEXT).size(15.0).strong());
            ui.label(
                RichText::new(format!("· #{}", contexte.nom_salon(porte.salon)))
                    .color(TEXT_DIM)
                    .size(12.5),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(reste_texte(porte.expire_le, maintenant)).color(TEXT_FAINT).size(11.5),
                );
            });
        });

        // Le lien complet : à copier, à dicter, ou à scanner.
        if let Some(url) = porte.url.clone() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add(
                    egui::Label::new(RichText::new(&url).color(ACCENT).monospace().size(13.0))
                        .truncate(),
                );
                if ui::icon_button_ex(ui, Icon::Copy, 24.0, "Copier le lien", None).clicked() {
                    actions.push(Action::Copier(url.clone()));
                }
            });
            if porte.qr.is_none() {
                if let Some(image) = crate::atelier::qr_image(&url) {
                    porte.qr = Some(ctx.load_texture(
                        format!("porte-qr-{}", porte.slug),
                        image,
                        egui::TextureOptions::NEAREST,
                    ));
                }
            }
            if let Some(qr) = &porte.qr {
                ui.add_space(4.0);
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(COTE_QR), Sense::hover());
                ui.painter().rect_filled(rect, CornerRadius::same(6), Color32::WHITE);
                ui.painter().image(
                    qr.id(),
                    rect.shrink(6.0),
                    Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            ui::hint(ui, "à dire en vocal, à coller, ou à scanner avec le téléphone — le navigateur avertira une fois du certificat du serveur");
        }

        // Les invités présents.
        ui.add_space(8.0);
        ui.label(
            RichText::new(match porte.invites.len() {
                0 => "aucun invité pour l'instant".to_string(),
                1 => "1 invité".to_string(),
                n => format!("{n} invités"),
            })
            .color(TEXT_DIM)
            .size(12.0),
        );
        let invites = porte.invites.clone();
        for invite in &invites {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&invite.nom).color(theme::INVITE).strong());
                pastille_invite(ui);
                if invite.depuis != 0 {
                    ui.label(
                        RichText::new(format!("entré {}", crate::il_y_a(invite.depuis)))
                            .color(TEXT_FAINT)
                            .size(11.0),
                    );
                }
                if let Some(v) = invite.vocal {
                    ui.label(
                        RichText::new(format!("· en vocal dans {}", contexte.nom_salon(v)))
                            .color(ACCENT)
                            .size(11.0),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.menu_button("…", |ui| {
                        ui.set_width(228.0);
                        boutons_invite(ui, invite.invite_id, invite.vocal, droits, contexte, actions);
                    });
                });
            });
        }

        // Ceux qui attendent derrière.
        if !porte.demandes.is_empty() {
            ui.add_space(8.0);
            ui.label(
                RichText::new(match porte.demandes.len() {
                    1 => "1 demande en attente".to_string(),
                    n => format!("{n} demandes en attente"),
                })
                .color(WARN)
                .size(12.0),
            );
            for demande in &porte.demandes {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&demande.nom).color(TEXT).strong());
                    if demande.depuis != 0 {
                        ui.label(
                            RichText::new(format!("a frappé {}", crate::il_y_a(demande.depuis)))
                                .color(TEXT_FAINT)
                                .size(11.0),
                        );
                    }
                    if droits.agir {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui::tinted_button(ui, Some(Icon::Close), "Refuser", Tone::Danger).clicked() {
                                actions.push(Action::Envoyer(ClientMsg::PorteRepondre {
                                    demande_id: demande.demande_id,
                                    accepter: false,
                                    motif: String::new(),
                                }));
                            }
                            if ui::tinted_button(ui, Some(Icon::Check), "Accepter", Tone::Accent).clicked() {
                                actions.push(Action::Envoyer(ClientMsg::PorteRepondre {
                                    demande_id: demande.demande_id,
                                    accepter: true,
                                    motif: String::new(),
                                }));
                                actions.push(Action::Lire(porte.salon));
                            }
                        });
                    }
                });
            }
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui::button(ui, Icon::Chat, "Ouvrir le salon").clicked() {
                actions.push(Action::Lire(porte.salon));
            }
            if droits.agir
                && ui::tinted_button(ui, Some(Icon::Ban), "Fermer la porte", Tone::Danger)
                    .on_hover_text("les invités sont congédiés et le salon temporaire est effacé")
                    .clicked()
            {
                actions.push(Action::Envoyer(ClientMsg::PorteFermer { slug: porte.slug.clone() }));
            }
        });
    });
}

/// Les boutons d'un invité : vocal, expulsion, offre. Partagés entre la
/// carte de la porte et le menu de la liste des membres.
#[allow(clippy::too_many_arguments)]
fn boutons_invite(
    ui: &mut egui::Ui,
    invite_id: UserId,
    vocal: Option<ChannelId>,
    droits: Droits,
    contexte: &Contexte,
    actions: &mut Vec<Action>,
) {
    if droits.deplacer {
        match contexte.mon_vocal {
            Some(mien) if vocal != Some(mien) => {
                if ui::button(ui, Icon::Volume, "L'inviter dans mon vocal")
                    .on_hover_text("il n'a pas de compte, donc pas de bouton : c'est toi qui l'y mets")
                    .clicked()
                {
                    actions.push(Action::Envoyer(ClientMsg::PorteVocal { invite_id, channel: Some(mien) }));
                    ui.close();
                }
            }
            Some(_) => {}
            None if vocal.is_none() => {
                ui.add_enabled_ui(false, |ui| {
                    let _ = ui::button(ui, Icon::Volume, "L'inviter dans mon vocal")
                        .on_disabled_hover_text("entre d'abord dans un salon vocal");
                });
            }
            None => {}
        }
        if vocal.is_some()
            && ui::tinted_button(ui, Some(Icon::Logout), "Le sortir du vocal", Tone::Warn).clicked()
        {
            actions.push(Action::Envoyer(ClientMsg::PorteVocal { invite_id, channel: None }));
            ui.close();
        }
    }
    if droits.offrir
        && ui::button(ui, Icon::Download, "Lui offrir ki-chat")
            .on_hover_text("une invitation à usage unique, valable sept jours : le lien de téléchargement dans le salon, le code sur sa page")
            .clicked()
    {
        actions.push(Action::Offrir { invite_id });
        ui.close();
    }
    if droits.agir
        && ui::tinted_button(ui, Some(Icon::Ban), "Expulser", Tone::Danger)
            .on_hover_text("sa page se ferme ; il peut frapper à nouveau")
            .clicked()
    {
        actions.push(Action::Envoyer(ClientMsg::PorteExpulser { invite_id }));
        ui.close();
    }
}

/// La bannière d'une demande : texte, et deux boutons. `Some(true)` =
/// accepté, `Some(false)` = refusé, `None` = rien cette image.
fn banniere_demande(ui: &mut egui::Ui, texte: &str, survol: &str) -> Option<bool> {
    let color = ACCENT;
    let mut reponse = None;
    egui::Frame::NONE
        .fill(theme::alpha(color, 26))
        .stroke(Stroke::new(1.0_f32, theme::alpha(color, 70)))
        .corner_radius(CornerRadius::same(9))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                icons::draw(ui.painter(), rect, Icon::User, color);
                ui.add_space(2.0);
                // Les boutons d'abord, à droite ; le texte prend le reste
                // et se tronque plutôt que de les pousser hors du cadre.
                let boutons = 190.0;
                let largeur = (ui.available_width() - boutons).max(60.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(largeur, 0.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        ui.add(egui::Label::new(RichText::new(texte).color(color).size(13.0)).truncate())
                            .on_hover_text(survol);
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui::tinted_button(ui, Some(Icon::Close), "Refuser", Tone::Danger).clicked() {
                        reponse = Some(false);
                    }
                    if ui::tinted_button(ui, Some(Icon::Check), "Accepter", Tone::Accent).clicked() {
                        reponse = Some(true);
                    }
                });
            });
        });
    reponse
}

/// La pastille « INVITÉ », à la couleur des invités, à côté d'un nom.
pub fn pastille_invite(ui: &mut egui::Ui) -> egui::Response {
    egui::Frame::new()
        .fill(theme::INVITE)
        .corner_radius(CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(4, 1))
        .show(ui, |ui| {
            ui.label(RichText::new("INVITÉ").size(9.5).strong().color(theme::BG_DEEP));
        })
        .response
        .on_hover_text("vient du web par une porte, sans compte — il ne voit que ce salon")
}

/// Un slug tel qu'on le tape : minuscules, sans espaces autour, jamais
/// plus long que la borne. La validation elle-même est celle du protocole.
pub fn slug_normalise(brut: &str) -> String {
    brut.trim().to_lowercase().chars().take(ki_protocol::PORTE_SLUG_MAX).collect()
}

/// « expire dans 12 min », « expire dans 1 h 05 », « expire dans 40 s »,
/// « expirée » — depuis une date (ms Unix) et l'heure qu'il est.
pub fn reste_texte(expire_le_ms: u64, maintenant_ms: u64) -> String {
    let s = expire_le_ms.saturating_sub(maintenant_ms) / 1000;
    match s {
        0 => "expirée".to_string(),
        1..=59 => format!("expire dans {s} s"),
        60..=3599 => format!("expire dans {} min", s / 60),
        _ => format!("expire dans {} h {:02}", s / 3600, (s % 3600) / 60),
    }
}

/// Le compte à rebours d'une demande : « 4 min », « 50 s ».
fn reste_court(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 {
        format!("{} min", s / 60)
    } else {
        format!("{s} s")
    }
}

/// L'heure qu'il est, en ms Unix — l'unité des dates du serveur.
/// Le lien d'une porte tel qu'on le montre et qu'on le copie : complet,
/// `https://` compris — sans lui, le navigateur tenterait `http://` et la
/// page, servie en TLS, ne s'ouvrirait pas. Le serveur le donne entier
/// quand il connaît son adresse publique ; sinon le chemin seul
/// (`/salon1`), qu'on complète avec `base` : l'adresse réglée par un
/// admin, ou celle par laquelle on joint le serveur.
pub fn lien_complet(base: &str, url: &str) -> String {
    let url = url.trim();
    if url.starts_with("https://") || url.starts_with("http://") {
        return url.to_string();
    }
    format!("{}/{}", base.trim_end_matches('/'), url.trim_start_matches('/'))
}

pub fn maintenant_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Le temps qui reste se lit en unités humaines, et une date passée
    /// dit « expirée » sans déborder.
    #[test]
    fn le_temps_qui_reste_se_dit_en_unites_humaines() {
        let t = 1_000_000_000;
        assert_eq!(reste_texte(t, t + 5_000), "expirée");
        assert_eq!(reste_texte(t + 40_000, t), "expire dans 40 s");
        assert_eq!(reste_texte(t + 12 * 60_000, t), "expire dans 12 min");
        assert_eq!(reste_texte(t + 65 * 60_000, t), "expire dans 1 h 05");
        assert_eq!(reste_court(Duration::from_secs(250)), "4 min");
        assert_eq!(reste_court(Duration::from_secs(50)), "50 s");
    }

    /// Ce qu'on tape devient un slug : minuscules, rogné, borné — et c'est
    /// le protocole qui dit ensuite s'il est valide.
    #[test]
    fn un_slug_se_normalise_sans_se_valider() {
        assert_eq!(slug_normalise("  Salon1 "), "salon1");
        assert_eq!(slug_normalise("a".repeat(40).as_str()).len(), ki_protocol::PORTE_SLUG_MAX);
        assert!(ki_protocol::slug_valide(&slug_normalise("Soiree-Du-Samedi")));
        assert!(!ki_protocol::slug_valide(&slug_normalise("salon 1")));
    }

    /// Une demande ne fait qu'une bannière, même répétée ; l'état qui ne
    /// la liste plus la retire ; la fermeture emporte tout.
    #[test]
    fn les_demandes_suivent_l_etat_de_la_porte() {
        let mut portes = Portes::new();
        assert!(!portes.disponible);
        assert!(portes.demande("salon1", 7, "Kevin", "82.65.x.x"));
        assert!(!portes.demande("salon1", 7, "Kevin", "82.65.x.x"), "répétée : pas deux bannières");
        assert!(portes.demande("salon1", 8, "Léa", ""));
        assert!(portes.disponible, "un message de porte prouve que le serveur les sert");
        assert!(portes.a_des_demandes());

        // L'état ne liste plus la 7 : quelqu'un d'autre a répondu.
        portes.etat(
            "salon1",
            9,
            vec![InviteWeb { invite_id: ki_protocol::INVITE_ID_BASE + 1, nom: "Kevin (web)".into(), depuis: 0, vocal: None }],
            vec![DemandeWeb { demande_id: 8, nom: "Léa".into(), depuis: 0 }],
            123,
            None,
        );
        assert_eq!(portes.demandes.len(), 1);
        assert_eq!(portes.demandes[0].demande_id, 8);
        assert_eq!(portes.salon_de("salon1"), Some(9));
        assert_eq!(portes.slug_du_salon(9), Some("salon1"));
        assert!(portes.porte_de_l_invite(ki_protocol::INVITE_ID_BASE + 1).is_some());
        assert!(portes.porte_de_l_invite(ki_protocol::INVITE_ID_BASE + 2).is_none());

        // Une autre porte n'y touche pas.
        portes.etat("autre", 10, Vec::new(), Vec::new(), 0, None);
        assert_eq!(portes.demandes.len(), 1);

        portes.fermee("salon1", "expirée");
        assert!(portes.demandes.is_empty());
        assert_eq!(portes.salon_de("salon1"), None);
        assert_eq!(portes.salon_de("autre"), Some(10));
        assert!(portes.message.as_deref().unwrap().contains("expirée"));
    }

    /// Une demande trop vieille s'efface d'elle-même : personne n'attend
    /// plus derrière.
    #[test]
    fn une_vieille_demande_s_efface() {
        let mut portes = Portes::new();
        portes.demande("salon1", 1, "Kevin", "");
        portes.demandes[0].recue = Instant::now()
            .checked_sub(DUREE_DEMANDE + Duration::from_secs(1))
            .expect("la machine tourne depuis plus de cinq minutes");
        assert!(!portes.a_des_demandes());
        portes.purger();
        assert!(portes.est_vide());
    }

    /// La porte que j'ouvre est mienne, garde son lien, et l'état qui suit
    /// ne le lui retire pas. La fermeture et la déconnexion font place nette.
    /// Un lien relatif se complète sur la base ; un lien complet reste tel
    /// quel. La dernière durée proposée est le plafond du serveur.
    #[test]
    fn un_lien_relatif_se_complete() {
        assert_eq!(lien_complet("https://ts.baws.fun:8080", "/valo"), "https://ts.baws.fun:8080/valo");
        assert_eq!(lien_complet("https://ts.baws.fun:8080/", "valo"), "https://ts.baws.fun:8080/valo");
        assert_eq!(lien_complet("https://autre:8080", "https://ts.baws.fun/valo"), "https://ts.baws.fun/valo");
        assert_eq!(DUREES[DUREES.len() - 1].0 * 60, ki_protocol::PORTE_TTL_MAX_SECS);
    }

    #[test]
    fn ma_porte_garde_son_lien() {
        let mut portes = Portes::new();
        portes.ouverte("salon1", "https://ts.baws.fun/s/salon1".into(), 9, 5_000);
        assert!(portes.ouvert, "le panneau s'ouvre pour montrer le lien");
        portes.etat("salon1", 9, Vec::new(), Vec::new(), 6_000, None);
        let p = &portes.portes[0];
        assert!(p.mienne);
        assert_eq!(p.url.as_deref(), Some("https://ts.baws.fun/s/salon1"));
        assert_eq!(p.expire_le, 6_000);
        // Un état qui porte un lien neuf (l'adresse publique a changé) le
        // remplace.
        portes.etat("salon1", 9, Vec::new(), Vec::new(), 6_000, Some("https://ts.baws.fun:8080/salon1".into()));
        assert_eq!(portes.portes[0].url.as_deref(), Some("https://ts.baws.fun:8080/salon1"));
        let salons: Vec<ChannelInfo> = Vec::new();
        let sans_droits = Contexte {
            peut_ouvrir: false,
            peut_expulser: false,
            mon_vocal: None,
            salons: &salons,
            base_web: "https://ts.baws.fun:8080".into(),
        };
        assert!(portes.peut_fermer("salon1", &sans_droits), "la mienne, sans permission");
        assert!(!portes.peut_fermer("autre", &sans_droits));
        portes.reinitialiser();
        assert!(portes.est_vide());
        assert!(!portes.disponible);
    }
}
