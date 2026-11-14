//! Ctrl+K : aller à un salon ou à un membre sans quitter le clavier.
//!
//! Une petite fenêtre, un champ, une liste : on tape quelques lettres, ↑ ↓
//! choisissent, Entrée y va. Un salon textuel s'ouvre, un salon vocal se
//! rejoint ; un membre, c'est sa fiche VALORANT s'il a lié son compte, et
//! sinon une mention posée dans la saisie. `#` ne garde que les salons, `@`
//! que les membres.
//!
//! `>` en tête, et c'est la palette : la liste est celle des commandes —
//! tout ce que `registre` connaît, avec la touche de chacune. Ctrl+Maj+P
//! l'ouvre directement là.
//!
//! [`proposer`] et [`proposer_commandes`] font le tri, sans fenêtre ni
//! réseau : ce sont eux qui se testent.

use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, FontId, Key, Modifiers, Rect, Sense};
use ki_protocol::{ChannelId, ChannelKind, UserId};
use ki_ui::jetons::{couleur as c, espace, rayon, texte};
use ki_ui::selecteur_emoji::plier;

use crate::icons::{self, Icon};
use crate::membres::{couleur_de_membre, est_bot};
use crate::registre::{Action, Groupe};
use crate::{ui, KiApp};

/// Les lignes montrées à la fois. Pour les salons et les membres, la liste
/// s'arrête là : on continue d'écrire pour affiner. Celle des commandes
/// défile.
const MAX_ENTREES: usize = 8;
const LIGNE: f32 = 32.0;
const LARGEUR: f32 = 460.0;

/// L'état de la fenêtre.
#[derive(Default)]
pub(crate) struct AllerA {
    pub(crate) ouvert: bool,
    requete: String,
    choix: usize,
    focus: bool,
    /// Les touches lues en tête d'image, avant que quoi que ce soit d'autre
    /// ne les prenne : de combien déplacer le choix, et « Entrée ».
    pas: i32,
    valider: bool,
}

impl AllerA {
    pub(crate) fn ouvrir(&mut self) {
        *self = Self { ouvert: true, focus: true, ..Self::default() };
    }

    /// Ouvre sur les commandes : « > » déjà tapé.
    pub(crate) fn ouvrir_commandes(&mut self) {
        *self = Self { ouvert: true, focus: true, requete: ">".into(), ..Self::default() };
    }

    pub(crate) fn fermer(&mut self) {
        *self = Self::default();
    }

    /// La liste montrée est celle des commandes.
    pub(crate) fn en_commandes(&self) -> bool {
        mots_de_commande(&self.requete).is_some()
    }

    /// ↑, ↓ et Entrée, à lire en tête d'image tant que la fenêtre est là.
    pub(crate) fn lire_les_touches(&mut self, ctx: &egui::Context) {
        let aucune = Modifiers::NONE;
        ctx.input_mut(|i| {
            self.pas += i.count_and_consume_key(aucune, Key::ArrowDown) as i32;
            self.pas -= i.count_and_consume_key(aucune, Key::ArrowUp) as i32;
            self.valider |= i.consume_key(aucune, Key::Enter);
        });
    }
}

/// Un salon, tel que la fenêtre le connaît.
#[derive(Clone, Debug)]
pub(crate) struct Salon {
    pub(crate) id: ChannelId,
    pub(crate) nom: String,
    pub(crate) vocal: bool,
    pub(crate) non_lus: u32,
    /// Celui qu'on lit (textuel), ou celui où l'on parle (vocal).
    pub(crate) courant: bool,
    /// Combien de membres dans un salon vocal.
    pub(crate) occupants: usize,
}

/// Un membre, tel que la fenêtre le connaît.
#[derive(Clone, Debug)]
pub(crate) struct Membre {
    pub(crate) id: UserId,
    pub(crate) pseudo: String,
    pub(crate) couleur: Color32,
    pub(crate) en_ligne: bool,
    /// Il a lié son compte Riot : il a une fiche à ouvrir.
    pub(crate) fiche: bool,
}

/// Où mène une ligne.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Cible {
    Salon(ChannelId),
    Vocal(ChannelId),
    Fiche(UserId, String),
    Mention(String),
    Commande(Action),
}

/// Ce qui se dessine à gauche d'une ligne.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Signe {
    Texte,
    Vocal,
    Membre { couleur: Color32, en_ligne: bool },
    Commande,
}

/// Une ligne de la liste.
#[derive(Clone, Debug)]
pub(crate) struct Entree {
    pub(crate) libelle: String,
    pub(crate) detail: String,
    pub(crate) signe: Signe,
    pub(crate) cible: Cible,
}

fn entree_de_salon(s: &Salon) -> Entree {
    let detail = match (s.vocal, s.courant) {
        (false, true) => "tu y es".to_string(),
        (false, false) if s.non_lus > 0 => format!("{} non lu{}", s.non_lus, if s.non_lus > 1 { "s" } else { "" }),
        (false, false) => String::new(),
        (true, true) => "vocal · tu y es".to_string(),
        (true, false) if s.occupants > 0 => format!("vocal · {} dedans · rejoindre", s.occupants),
        (true, false) => "vocal · rejoindre".to_string(),
    };
    Entree {
        libelle: ki_protocol::safe_display(&s.nom, 40),
        detail,
        signe: if s.vocal { Signe::Vocal } else { Signe::Texte },
        cible: if s.vocal { Cible::Vocal(s.id) } else { Cible::Salon(s.id) },
    }
}

fn entree_de_membre(m: &Membre) -> Entree {
    Entree {
        libelle: ki_protocol::safe_display(&m.pseudo, 32),
        detail: if m.fiche { "fiche VALORANT" } else { "mentionner" }.to_string(),
        signe: Signe::Membre { couleur: m.couleur, en_ligne: m.en_ligne },
        cible: if m.fiche { Cible::Fiche(m.id, m.pseudo.clone()) } else { Cible::Mention(m.pseudo.clone()) },
    }
}

/// La clé du tri : rang, famille, ordre dans la famille, place dans la
/// colonne, nom plié.
type Cle = (u8, u8, u8, usize, String);

/// Ce que la fenêtre propose pour `requete`, du plus probable au moins
/// probable, [`MAX_ENTREES`] au plus.
///
/// Sans rien de tapé : les salons où il y a du non-lu, puis les autres, dans
/// l'ordre de la colonne — pas celui qu'on lit, on y est. Avec des lettres :
/// ce qui commence par elles avant ce qui les contient ; à égalité, les
/// salons textuels, puis les vocaux, puis les membres, ceux qui sont là
/// avant les absents. Le salon où l'on est passe après les autres : Entrée
/// doit mener quelque part. `#` et `@` en tête ne gardent que les salons, ou
/// que les membres.
pub(crate) fn proposer(requete: &str, salons: &[Salon], membres: &[Membre]) -> Vec<Entree> {
    let requete = requete.trim();
    let (avec_salons, avec_membres, mots) = match requete.chars().next() {
        Some('#') => (true, false, &requete[1..]),
        Some('@') => (false, true, &requete[1..]),
        _ => (true, true, requete),
    };
    let mots = plier(mots.trim());
    let rang = |nom: &str| match (nom.starts_with(&mots), nom.contains(&mots)) {
        (true, _) => Some(0u8),
        (false, true) => Some(1),
        (false, false) => None,
    };
    let mut trouves: Vec<(Cle, Entree)> = Vec::new();
    if avec_salons {
        for (place, s) in salons.iter().enumerate() {
            // Rien de tapé : inutile de proposer d'aller là où l'on est.
            if mots.is_empty() && s.courant {
                continue;
            }
            let nom = plier(&s.nom);
            let Some(rang) = rang(&nom) else { continue };
            let dans_la_famille = match (s.courant, s.non_lus > 0) {
                (true, _) => 2,
                (false, true) => 0,
                (false, false) => 1,
            };
            trouves.push(((rang, u8::from(s.vocal), dans_la_famille, place, nom), entree_de_salon(s)));
        }
    }
    // Sans lettres, les membres ne viennent que si on les demande : la liste
    // est celle des salons.
    if avec_membres && (!mots.is_empty() || !avec_salons) {
        for m in membres {
            let nom = plier(&m.pseudo);
            let Some(rang) = rang(&nom) else { continue };
            trouves.push(((rang, 2, u8::from(!m.en_ligne), 0, nom), entree_de_membre(m)));
        }
    }
    trouves.sort_by(|a, b| a.0.cmp(&b.0));
    trouves.into_iter().take(MAX_ENTREES).map(|(_, e)| e).collect()
}

/// Ce qui suit le « > » qui demande les commandes — ou rien, si la requête
/// n'en est pas une.
fn mots_de_commande(requete: &str) -> Option<&str> {
    requete.trim_start().strip_prefix('>')
}

/// Les commandes que `mots` retient, parmi `commandes` (chacune avec sa
/// touche, si elle en a une) : celles dont le nom commence par eux, puis
/// celles dont un mot commence par eux, puis celles qui les contiennent ;
/// à égalité, l'ordre du registre. Toutes : cette liste-là défile.
pub(crate) fn proposer_commandes(mots: &str, commandes: &[(Action, Option<String>)]) -> Vec<Entree> {
    let mots = plier(mots.trim());
    let mut trouvees: Vec<(u8, usize, Entree)> = commandes
        .iter()
        .enumerate()
        .filter_map(|(place, (action, touche))| {
            let nom = plier(action.libelle());
            let rang = if nom.starts_with(&mots) {
                0
            } else if nom.split(|c: char| !c.is_alphanumeric()).any(|mot| mot.starts_with(&mots)) {
                1
            } else if nom.contains(&mots) {
                2
            } else {
                return None;
            };
            let entree = Entree {
                libelle: action.libelle().to_string(),
                detail: touche.clone().unwrap_or_default(),
                signe: Signe::Commande,
                cible: Cible::Commande(*action),
            };
            Some((rang, place, entree))
        })
        .collect();
    trouvees.sort_by_key(|(rang, place, _)| (*rang, *place));
    trouvees.into_iter().map(|(_, _, e)| e).collect()
}

fn ligne(ui: &egui::Ui, rect: Rect, e: &Entree, choisie: bool) {
    let p = ui.painter();
    if choisie {
        p.rect_filled(rect, CornerRadius::same(rayon::M), c::BG_ACTIVE);
    }
    let centre = rect.center().y;
    let signe = Rect::from_center_size(pos2(rect.left() + espace::S + 9.0, centre), egui::Vec2::splat(16.0));
    let couleur = match e.signe {
        Signe::Texte => {
            icons::draw(p, signe, Icon::Hash, c::TEXT_FAINT);
            c::TEXT
        }
        Signe::Vocal => {
            icons::draw(p, signe, Icon::Volume, c::TEXT_FAINT);
            c::TEXT
        }
        Signe::Membre { couleur, en_ligne } => {
            p.circle_filled(signe.center(), 4.0, if en_ligne { c::ACCENT } else { c::TEXT_FAINT });
            couleur
        }
        Signe::Commande => {
            icons::draw(p, signe, Icon::ChevronRight, c::TEXT_FAINT);
            c::TEXT
        }
    };
    let gauche = signe.right() + espace::S;
    p.text(pos2(gauche, centre), Align2::LEFT_CENTER, &e.libelle, FontId::proportional(texte::COURANT), couleur);
    p.text(
        pos2(rect.right() - espace::S, centre),
        Align2::RIGHT_CENTER,
        &e.detail,
        FontId::proportional(texte::PETIT),
        c::TEXT_DIM,
    );
}

impl KiApp {
    fn sources_aller_a(&self) -> (Vec<Salon>, Vec<Membre>) {
        let s = &self.session;
        let salons = s
            .channels
            .iter()
            .filter(|ch| matches!(ch.kind, ChannelKind::Text | ChannelKind::Voice))
            .map(|ch| {
                let vocal = ch.kind == ChannelKind::Voice;
                Salon {
                    id: ch.id,
                    nom: ch.name.clone(),
                    vocal,
                    non_lus: s.non_lus.get(&ch.id).map_or(0, |n| n.nb),
                    // La page VALORANT ou Claude a pris la place du chat :
                    // on n'« est » alors dans aucun salon textuel.
                    courant: if vocal {
                        s.voice_channel == Some(ch.id)
                    } else {
                        s.current == Some(ch.id) && !self.valo_au_centre() && !self.claude.ouvert
                    },
                    occupants: if vocal { s.members.iter().filter(|m| m.voice == Some(ch.id)).count() } else { 0 },
                }
            })
            .collect();
        let membres = s
            .members
            .iter()
            .filter(|m| Some(m.user_id) != s.my_id && !est_bot(m.user_id))
            .map(|m| Membre {
                id: m.user_id,
                pseudo: m.username.clone(),
                couleur: couleur_de_membre(m),
                en_ligne: m.online,
                fiche: m.riot_id.is_some(),
            })
            .collect();
        (salons, membres)
    }

    /// Ce que la palette propose : tout ce qui a un sens à l'instant, sauf
    /// les deux raccourcis de cette fenêtre même, et la mise en forme — la
    /// saisie perd sa sélection dès qu'on la quitte pour venir ici.
    fn commandes_de_la_palette(&self) -> Vec<(Action, Option<String>)> {
        Action::TOUTES
            .into_iter()
            .filter(|a| !matches!(a, Action::AllerA | Action::Palette) && a.groupe() != Groupe::Ecrire && self.possible(*a))
            .map(|a| (a, self.registre.libelle(a)))
            .collect()
    }

    /// La fenêtre de Ctrl+K, tant qu'elle est ouverte.
    pub(crate) fn aller_a_window(&mut self, ctx: &egui::Context) {
        if !self.aller_a.ouvert {
            return;
        }
        let en_commandes = self.aller_a.en_commandes();
        let entrees = match mots_de_commande(&self.aller_a.requete) {
            Some(mots) => proposer_commandes(mots, &self.commandes_de_la_palette()),
            None => {
                let (salons, membres) = self.sources_aller_a();
                proposer(&self.aller_a.requete, &salons, &membres)
            }
        };
        let etat = &mut self.aller_a;
        // Le choix fait le tour de la liste, comme dans les suggestions.
        let n = entrees.len() as i32;
        let pas = std::mem::take(&mut etat.pas);
        etat.choix = if n == 0 { 0 } else { (etat.choix.min(n as usize - 1) as i32 + pas).rem_euclid(n) as usize };
        let mut choisie = std::mem::take(&mut etat.valider).then_some(etat.choix);

        let sortie = ki_ui::fenetre::Modale::new("aller-a", "").largeur(LARGEUR).show(ctx, |ui| {
            let champ = ui.add(ui::champ(&mut etat.requete, "aller à un salon ou à un membre…"));
            if std::mem::take(&mut etat.focus) {
                champ.request_focus();
                // Ouverte sur « > » : la suite s'écrit après, pas avant.
                let mut edition = egui::TextEdit::load_state(ui.ctx(), champ.id).unwrap_or_default();
                let fin = egui::text::CCursor::new(etat.requete.chars().count());
                edition.cursor.set_char_range(Some(egui::text::CCursorRange::one(fin)));
                edition.store(ui.ctx(), champ.id);
            }
            if champ.changed() {
                etat.choix = 0;
            }
            ui.add_space(espace::S);
            // La place de la liste est prise d'avance, pleine : la fenêtre
            // ne change pas de taille — ni le champ de place — à chaque
            // lettre.
            let (zone, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), MAX_ENTREES as f32 * LIGNE), Sense::hover());
            // Une liste plus longue que la fenêtre défile avec le choix.
            let premiere = (etat.choix + 1).saturating_sub(MAX_ENTREES);
            for (i, e) in entrees.iter().enumerate().skip(premiere).take(MAX_ENTREES) {
                let rect = Rect::from_min_size(zone.min + vec2(0.0, (i - premiere) as f32 * LIGNE), vec2(zone.width(), LIGNE));
                let reponse = ui.interact(rect, ui.scope_id().with(("aller-a", i)), Sense::click());
                if reponse.hovered() && ui.input(|e| e.pointer.is_moving()) {
                    etat.choix = i;
                }
                ligne(ui, rect, e, i == etat.choix);
                if reponse.clicked() {
                    choisie = Some(i);
                }
            }
            if entrees.is_empty() {
                ui.painter().text(
                    zone.min + vec2(espace::S, LIGNE / 2.0),
                    Align2::LEFT_CENTER,
                    if en_commandes { "aucune commande de ce nom" } else { "rien de ce nom" },
                    FontId::proportional(texte::COURANT),
                    c::TEXT_DIM,
                );
            }
            ui.add_space(espace::S);
            ui::hint(
                ui,
                if en_commandes {
                    "↑↓ choisir · Entrée lancer · Échap fermer"
                } else {
                    "↑↓ choisir · Entrée y aller · Échap fermer · # salons, @ membres, > commandes"
                },
            );
        });

        if let Some(e) = choisie.and_then(|i| entrees.into_iter().nth(i)) {
            self.aller_a.fermer();
            self.aller(ctx, e.cible);
        } else if sortie.fermee {
            self.aller_a.fermer();
            self.focus_input = true;
        }
    }

    fn aller(&mut self, ctx: &egui::Context, cible: Cible) {
        match cible {
            Cible::Salon(id) => {
                self.retour_au_chat();
                self.join(id);
                self.focus_input = true;
            }
            Cible::Vocal(id) => self.join_voice(id),
            Cible::Fiche(id, pseudo) => self.ouvrir_fiche(id, pseudo),
            Cible::Commande(action) => {
                // Le micro coupé, on se remet à écrire.
                self.focus_input = action.groupe() == Groupe::Voix;
                self.faire(ctx, action);
            }
            Cible::Mention(pseudo) => {
                self.retour_au_chat();
                self.inserer_dans_la_saisie(ctx, &format!("@{pseudo} "));
                self.focus_input = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn salon(id: ChannelId, nom: &str) -> Salon {
        Salon { id, nom: nom.into(), vocal: false, non_lus: 0, courant: false, occupants: 0 }
    }

    fn vocal(id: ChannelId, nom: &str, occupants: usize) -> Salon {
        Salon { vocal: true, occupants, ..salon(id, nom) }
    }

    fn membre(id: UserId, pseudo: &str, en_ligne: bool, fiche: bool) -> Membre {
        Membre { id, pseudo: pseudo.into(), couleur: Color32::WHITE, en_ligne, fiche }
    }

    fn monde() -> (Vec<Salon>, Vec<Membre>) {
        let salons = vec![
            Salon { courant: true, ..salon(1, "général") },
            salon(2, "gaming"),
            salon(4, "blabla"),
            Salon { non_lus: 3, ..salon(3, "clips") },
            vocal(10, "Général", 0),
            vocal(11, "Valo", 2),
        ];
        let membres = vec![
            membre(21, "Gaëlle", false, false),
            membre(22, "mg4230", true, true),
            membre(23, "Valentin", true, false),
            membre(24, "clipman", false, true),
        ];
        (salons, membres)
    }

    fn noms(requete: &str) -> Vec<String> {
        let (salons, membres) = monde();
        proposer(requete, &salons, &membres).into_iter().map(|e| e.libelle).collect()
    }

    #[test]
    fn sans_rien_taper_les_salons_le_non_lu_d_abord() {
        // Pas « général » : on y est. Pas de membres : on ne les a pas
        // demandés. Après le non-lu, l'ordre de la colonne, pas l'alphabet.
        assert_eq!(noms(""), ["clips", "gaming", "blabla", "Général", "Valo"]);
        assert_eq!(noms("   "), ["clips", "gaming", "blabla", "Général", "Valo"]);
        let (salons, membres) = monde();
        let details: Vec<String> = proposer("", &salons, &membres).into_iter().map(|e| e.detail).collect();
        assert_eq!(details, ["3 non lus", "", "", "vocal · rejoindre", "vocal · 2 dedans · rejoindre"]);
    }

    #[test]
    fn les_lettres_trient_du_plus_probable_au_moins_probable() {
        // Ce qui commence par « g » : les salons textuels — celui où l'on
        // est en dernier —, le vocal, le membre ; puis ce qui le contient
        // seulement.
        assert_eq!(noms("g"), ["gaming", "général", "Général", "Gaëlle", "mg4230"]);
        // Les accents et la casse ne comptent pas.
        assert_eq!(noms("GENE"), ["général", "Général"]);
        assert_eq!(noms("gae"), ["Gaëlle"]);
        // À rang égal, celui qui est là passe avant l'absent.
        assert_eq!(noms("val"), ["Valo", "Valentin"]);
        assert_eq!(noms("cli"), ["clips", "clipman"]);
        assert_eq!(noms("zzz"), Vec::<String>::new());
    }

    #[test]
    fn le_diese_et_l_arobase_filtrent() {
        assert_eq!(noms("#g"), ["gaming", "général", "Général"]);
        assert_eq!(noms("@g"), ["Gaëlle", "mg4230"]);
        // « @ » seul : tous les membres, ceux qui sont là d'abord.
        assert_eq!(noms("@"), ["mg4230", "Valentin", "clipman", "Gaëlle"]);
        assert_eq!(noms("# cli"), ["clips"]);
    }

    #[test]
    fn chaque_ligne_mene_au_bon_endroit() {
        let (salons, membres) = monde();
        let cible = |requete: &str| proposer(requete, &salons, &membres).remove(0).cible;
        assert_eq!(cible("gam"), Cible::Salon(2));
        assert_eq!(cible("valo"), Cible::Vocal(11));
        // Un compte Riot lié : sa fiche. Sinon, une mention.
        assert_eq!(cible("@mg"), Cible::Fiche(22, "mg4230".into()));
        assert_eq!(cible("@valen"), Cible::Mention("Valentin".into()));
        // Le salon où l'on est se trouve quand on le nomme, et se dit.
        let ici = proposer("géné", &salons, &membres).remove(0);
        assert_eq!((ici.cible, ici.detail.as_str()), (Cible::Salon(1), "tu y es"));
    }

    #[test]
    fn la_palette_trie_les_commandes() {
        let commandes: Vec<(Action, Option<String>)> = [
            (Action::Chercher, Some("Ctrl+F")),
            (Action::Reglages, Some("Ctrl+,")),
            (Action::Raccourcis, None),
            (Action::Compte, None),
            (Action::Claude, None),
            (Action::Micro, None),
            (Action::QuitterVocal, None),
        ]
        .into_iter()
        .map(|(a, t)| (a, t.map(String::from)))
        .collect();
        let noms = |mots: &str| -> Vec<String> { proposer_commandes(mots, &commandes).into_iter().map(|e| e.libelle).collect() };
        // Rien de tapé : toutes, dans l'ordre du registre — et plus de huit
        // ne seraient pas coupées.
        assert_eq!(noms("").len(), commandes.len());
        assert_eq!(noms(" ")[0], "Chercher dans les messages");
        // Le début du nom, puis le début d'un mot, puis n'importe où ; les
        // accents et la casse ne comptent pas.
        assert_eq!(
            noms("c"),
            [
                "Chercher dans les messages",
                "Couper ou rouvrir le micro",
                "Raccourcis clavier",
                "Mon compte",
                "Salon de Claude",
                "Quitter le salon vocal",
            ]
        );
        assert_eq!(noms("REGL"), ["Réglages"]);
        assert_eq!(noms("micro"), ["Couper ou rouvrir le micro"]);
        assert_eq!(noms("voc"), ["Quitter le salon vocal"]);
        assert_eq!(noms("our"), ["Raccourcis clavier"]);
        assert_eq!(noms("zzz"), Vec::<String>::new());
        // La touche se lit à droite, et la ligne mène à l'action.
        let chercher = proposer_commandes("cher", &commandes).remove(0);
        assert_eq!((chercher.detail.as_str(), chercher.cible), ("Ctrl+F", Cible::Commande(Action::Chercher)));
        // « > » en tête demande les commandes, et pas autrement.
        assert_eq!(mots_de_commande(" >regl"), Some("regl"));
        assert_eq!(mots_de_commande("regl>"), None);
        assert_eq!(mots_de_commande("#general"), None);
    }

    #[test]
    fn la_liste_s_arrete_a_huit() {
        let salons: Vec<Salon> = (0..30).map(|i| salon(i, &format!("salon-{i:02}"))).collect();
        let liste = proposer("salon", &salons, &[]);
        assert_eq!(liste.len(), MAX_ENTREES);
        assert_eq!(liste[0].libelle, "salon-00");
    }
}
