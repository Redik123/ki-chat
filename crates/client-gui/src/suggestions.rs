//! Les suggestions de la zone de saisie : `@` pour les pseudos, `/` en début de
//! message pour les commandes, `:` pour les emoji.
//!
//! Le mot sous le curseur décide de ce qu'on propose ([`jeton_au_curseur`]), et
//! [`Suggestions`] tient le panneau : ↑ ↓ pour choisir, Tab ou Entrée pour
//! valider, Échap pour le refermer. Tout passe avant la zone de saisie, qui
//! prendrait sinon ces touches pour elle : Entrée enverrait le message, Tab
//! ferait partir le focus.
//!
//! Ni réseau ni état de ki-chat ici : l'appelant donne ses membres et ses
//! commandes ([`Sources`]), et décide quoi faire d'une commande validée
//! ([`lire_commande`]).

use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, EventFilter, FontId, Id, Key, Modifiers, Order, Rect, Sense, Stroke};
use ki_ui::jetons::{couleur as c, espace, marge, rayon, texte};
use ki_ui::selecteur_emoji;

/// Au-delà, on continue d'écrire pour affiner.
const MAX_SUGGESTIONS: usize = 8;
/// La hauteur d'une ligne du panneau, et celle de son titre.
const LIGNE: f32 = 30.0;
const TITRE: f32 = 22.0;
/// `:)` n'est pas une recherche d'emoji : il en faut au moins deux lettres.
const MIN_RECHERCHE_EMOJI: usize = 2;

/// Une commande de la zone de saisie, telle que le panneau la présente.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Commande {
    pub nom: &'static str,
    /// Ce qu'elle attend, pour mémoire.
    pub argument: &'static str,
    pub aide: &'static str,
    /// Ce que valider écrit. `/poke @` rouvre aussitôt la liste des pseudos.
    pub insertion: &'static str,
}

pub const POKE: Commande =
    Commande { nom: "poke", argument: "@pseudo", aide: "attirer son attention", insertion: "/poke @" };
pub const MUSIQUE: Commande =
    Commande { nom: "musique", argument: "lien ou recherche", aide: "ajouter un morceau", insertion: "/musique " };
pub const PAUSE: Commande = Commande { nom: "pause", argument: "", aide: "mettre la musique en pause", insertion: "/pause" };
pub const REPRENDRE: Commande =
    Commande { nom: "reprendre", argument: "", aide: "relancer la musique", insertion: "/reprendre" };
pub const SUIVANT: Commande = Commande { nom: "suivant", argument: "", aide: "passer au morceau suivant", insertion: "/suivant" };
pub const CARTES: Commande = Commande { nom: "cartes", argument: "", aide: "ouvrir la table de cartes", insertion: "/cartes" };
pub const CHERCHER: Commande =
    Commande { nom: "chercher", argument: "mots", aide: "chercher dans les messages", insertion: "/chercher " };
pub const MOI: Commande = Commande { nom: "moi", argument: "ce que tu fais", aide: "le dire en italique", insertion: "/moi " };
pub const MUET: Commande = Commande { nom: "muet", argument: "", aide: "couper ou rouvrir ton micro", insertion: "/muet" };
pub const SOURD: Commande = Commande { nom: "sourd", argument: "", aide: "ne plus entendre, ou entendre à nouveau", insertion: "/sourd" };
pub const TABLEAU: Commande =
    Commande { nom: "tableau", argument: "", aide: "ouvrir le tableau tactique", insertion: "/tableau" };
pub const RACCOURCIS: Commande =
    Commande { nom: "raccourcis", argument: "", aide: "voir et changer les raccourcis clavier", insertion: "/raccourcis" };

/// Une commande reconnue dans un message prêt à partir.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ordre<'a> {
    Poke(&'a str),
    Musique(&'a str),
    Pause,
    Reprendre,
    Suivant,
    Cartes,
    Chercher(&'a str),
    Moi(&'a str),
    Muet,
    Sourd,
    Tableau,
    Raccourcis,
}

/// La commande que ce message demande. `None` : c'est un message comme un
/// autre, qui part tel quel — `/s`, une adresse de fichier ou « /pause café ? »
/// ne disparaissent pas parce qu'ils ressemblent à une commande.
pub fn lire_commande(texte: &str) -> Option<Ordre<'_>> {
    let reste = texte.strip_prefix('/')?;
    let (nom, argument) = reste.split_once(char::is_whitespace).unwrap_or((reste, ""));
    let argument = argument.trim();
    let seule = |ordre| argument.is_empty().then_some(ordre);
    match nom.to_ascii_lowercase().as_str() {
        "poke" => Some(Ordre::Poke(argument.strip_prefix('@').unwrap_or(argument))),
        "musique" => Some(Ordre::Musique(argument)),
        "pause" => seule(Ordre::Pause),
        "reprendre" => seule(Ordre::Reprendre),
        "suivant" => seule(Ordre::Suivant),
        "cartes" => seule(Ordre::Cartes),
        "chercher" => Some(Ordre::Chercher(argument)),
        "moi" => Some(Ordre::Moi(argument)),
        "muet" => seule(Ordre::Muet),
        "sourd" => seule(Ordre::Sourd),
        "tableau" => seule(Ordre::Tableau),
        "raccourcis" => seule(Ordre::Raccourcis),
        _ => None,
    }
}

/// Ce que le mot en cours demande.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Genre {
    Membre,
    Commande,
    Emoji,
}

impl Genre {
    fn marque(self) -> char {
        match self {
            Genre::Membre => '@',
            Genre::Commande => '/',
            Genre::Emoji => ':',
        }
    }

    fn titre(self) -> &'static str {
        match self {
            Genre::Membre => "Membres",
            Genre::Commande => "Commandes",
            Genre::Emoji => "Emoji",
        }
    }
}

/// Le mot sous le curseur, quand il appelle des suggestions. `debut` et `fin`
/// comptent en caractères, comme le curseur d'egui.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Jeton {
    pub debut: usize,
    pub fin: usize,
    pub genre: Genre,
    pub requete: String,
}

/// Le mot que le curseur termine, s'il commence par une marque.
///
/// Un `@` au milieu d'un mot (une adresse e-mail) n'en est pas une, ni un `/`
/// ailleurs qu'en tête de message, ni ce qui est entre accents graves : le rendu
/// n'y reconnaît pas les mentions, et l'on y colle souvent du code.
pub fn jeton_au_curseur(texte: &str, curseur: usize) -> Option<Jeton> {
    let lettres: Vec<char> = texte.chars().collect();
    let curseur = curseur.min(lettres.len());
    if lettres.get(curseur).is_some_and(|&l| l.is_alphanumeric() || l == '_' || l == '-') {
        return None;
    }
    let debut = lettres[..curseur].iter().rposition(|l| l.is_whitespace()).map_or(0, |i| i + 1);
    let (&marque, suite) = lettres[debut..curseur].split_first()?;
    let requete: String = suite.iter().collect();
    let genre = match marque {
        '@' if !suite.contains(&'@') => Genre::Membre,
        // Seulement des lettres : `/home/moi` ou `/r/rust` ne sont pas des commandes.
        '/' if debut == 0 && suite.iter().all(|l| l.is_alphabetic()) => Genre::Commande,
        ':' if suite.len() >= MIN_RECHERCHE_EMOJI
            && suite.iter().all(|&l| l.is_alphanumeric() || matches!(l, '_' | '-' | '+')) =>
        {
            Genre::Emoji
        }
        _ => return None,
    };
    // Un nombre impair d'accents graves avant le mot : on est dans du code.
    if lettres[..debut].iter().filter(|&&l| l == '`').count() % 2 == 1 {
        return None;
    }
    Some(Jeton { debut, fin: curseur, genre, requete })
}

/// Un membre qu'on peut mentionner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Membre {
    pub pseudo: String,
    pub couleur: Color32,
    pub en_ligne: bool,
    /// « en vocal », « hors ligne »… À droite de la ligne.
    pub detail: &'static str,
}

/// Ce que l'appelant a à proposer. Lu seulement quand le mot en cours change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sources {
    pub membres: Vec<Membre>,
    /// Les commandes **disponibles** : celles qu'on n'a pas le droit de lancer
    /// ne sont pas proposées.
    pub commandes: Vec<Commande>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Apparence {
    Membre { couleur: Color32, en_ligne: bool },
    Commande { argument: &'static str },
    Emoji(&'static str),
}

/// Une ligne du panneau.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    libelle: String,
    detail: String,
    insertion: String,
    apparence: Apparence,
}

/// Ce qu'il y a à proposer pour ce mot.
fn proposer(jeton: &Jeton, sources: &Sources) -> Vec<Suggestion> {
    let mut liste = match jeton.genre {
        Genre::Membre => proposer_membres(&jeton.requete, &sources.membres),
        Genre::Commande => proposer_commandes(&jeton.requete, &sources.commandes),
        Genre::Emoji => selecteur_emoji::rechercher(&jeton.requete, MAX_SUGGESTIONS)
            .into_iter()
            .map(|(emoji, nom)| Suggestion {
                libelle: nom,
                detail: String::new(),
                insertion: format!("{emoji} "),
                apparence: Apparence::Emoji(emoji),
            })
            .collect(),
    };
    // Le mot est déjà écrit en entier, et il n'y a rien d'autre à proposer : plus
    // de panneau, sinon Entrée le « validerait » sans jamais envoyer le message.
    let ecrit = format!("{}{}", jeton.genre.marque(), jeton.requete);
    if liste.len() == 1 && liste[0].insertion.trim_end().eq_ignore_ascii_case(&ecrit) {
        liste.clear();
    }
    liste
}

/// Ceux dont le pseudo commence par la recherche d'abord, puis ceux qui la
/// contiennent ; à l'intérieur, ceux qui sont là avant les absents.
fn proposer_membres(requete: &str, membres: &[Membre]) -> Vec<Suggestion> {
    let requete = selecteur_emoji::plier(requete);
    let mut trouves: Vec<(u8, bool, String, &Membre)> = membres
        .iter()
        .filter_map(|m| {
            let plie = selecteur_emoji::plier(&m.pseudo);
            let rang = if plie.starts_with(&requete) {
                0
            } else if plie.contains(&requete) {
                1
            } else {
                return None;
            };
            Some((rang, !m.en_ligne, plie, m))
        })
        .collect();
    trouves.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
    trouves
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|(_, _, _, m)| Suggestion {
            libelle: ki_protocol::safe_display(&m.pseudo, 32),
            detail: m.detail.to_string(),
            // Le vrai pseudo : c'est lui que le rendu reconnaît comme mention.
            insertion: format!("@{} ", m.pseudo),
            apparence: Apparence::Membre { couleur: m.couleur, en_ligne: m.en_ligne },
        })
        .collect()
}

fn proposer_commandes(requete: &str, commandes: &[Commande]) -> Vec<Suggestion> {
    let requete = requete.to_lowercase();
    commandes
        .iter()
        .filter(|c| c.nom.starts_with(&requete))
        .take(MAX_SUGGESTIONS)
        .map(|c| Suggestion {
            libelle: format!("/{}", c.nom),
            detail: c.aide.to_string(),
            insertion: c.insertion.to_string(),
            apparence: Apparence::Commande { argument: c.argument },
        })
        .collect()
}

/// Ce qu'une validation remplace dans la saisie : `debut..fin` en caractères.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choix {
    pub debut: usize,
    pub fin: usize,
    pub insertion: String,
}

/// L'état du panneau, d'une image à l'autre.
#[derive(Debug, Default)]
pub struct Suggestions {
    jeton: Option<Jeton>,
    liste: Vec<Suggestion>,
    selection: usize,
    /// Échap : on ne repropose rien tant qu'on reste sur le mot qui commence ici.
    refuse_a: Option<usize>,
    focus: bool,
    /// Le pointeur sur le panneau : un clic y retire le focus à la saisie, et le
    /// panneau doit tenir jusqu'à son relâchement, sans quoi le clic se perd.
    survol: bool,
    derniere_image: u64,
}

impl Suggestions {
    /// La zone de saisie a-t-elle été dessinée à l'image d'avant ? Sinon, on est
    /// sur une autre page, et ses touches ne sont pas pour nous.
    fn a_jour(&self, ctx: &egui::Context) -> bool {
        ctx.cumulative_frame_nr() <= self.derniere_image + 1
    }

    /// Le panneau écoute-t-il le clavier ?
    pub fn ouvert(&self, ctx: &egui::Context) -> bool {
        self.focus && !self.liste.is_empty() && self.a_jour(ctx)
    }

    /// Échap referme le panneau, et rien d'autre : à appeler en tête d'image,
    /// avant ce qu'Échap ferme par ailleurs (une réponse, une édition, une fenêtre).
    pub fn echap(&mut self, ctx: &egui::Context) {
        if self.ouvert(ctx) && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.refuse_a = self.jeton.as_ref().map(|j| j.debut);
            self.liste.clear();
        }
    }

    /// ↑ ↓ Tab Entrée, pris à la zone de saisie : à appeler avant elle.
    pub fn clavier(&mut self, ctx: &egui::Context) -> Option<Choix> {
        if !self.ouvert(ctx) {
            return None;
        }
        let (bas, haut, valider) = ctx.input_mut(|i| {
            (
                i.count_and_consume_key(Modifiers::NONE, Key::ArrowDown),
                i.count_and_consume_key(Modifiers::NONE, Key::ArrowUp),
                // `|` et non `||` : les deux touches sont consommées.
                i.consume_key(Modifiers::NONE, Key::Enter) | i.consume_key(Modifiers::NONE, Key::Tab),
            )
        });
        let n = self.liste.len();
        self.selection = (self.selection + bas + n - haut % n) % n;
        valider.then(|| self.choisir(self.selection)).flatten()
    }

    fn choisir(&mut self, index: usize) -> Option<Choix> {
        let jeton = self.jeton.as_ref()?;
        let suggestion = self.liste.get(index)?;
        let choix = Choix { debut: jeton.debut, fin: jeton.fin, insertion: suggestion.insertion.clone() };
        self.liste.clear();
        Some(choix)
    }

    /// Met le panneau à jour, une fois la zone de saisie dessinée. `curseur` :
    /// sa position, `None` s'il y a une sélection.
    pub fn actualiser(
        &mut self,
        ctx: &egui::Context,
        texte: &str,
        curseur: Option<usize>,
        focus: bool,
        sources: impl FnOnce() -> Sources,
    ) {
        self.focus = focus;
        self.derniere_image = ctx.cumulative_frame_nr();
        // Sans focus, la saisie ne dit plus où est son curseur. On garde le mot
        // connu : un clic sur le panneau en a besoin, jusqu'à son relâchement.
        if !focus {
            return;
        }
        let jeton = curseur.and_then(|c| jeton_au_curseur(texte, c));
        if jeton == self.jeton {
            return;
        }
        if jeton.as_ref().map(|j| j.debut) != self.refuse_a {
            self.refuse_a = None;
        }
        self.liste = match &jeton {
            Some(j) if self.refuse_a.is_none() => proposer(j, &sources()),
            _ => Vec::new(),
        };
        self.selection = 0;
        self.jeton = jeton;
    }

    /// Garde Tab et Échap à la zone de saisie tant que le panneau est ouvert :
    /// egui en fait partir le focus au début de l'image suivante, avant que
    /// [`Self::clavier`] puisse les consommer.
    pub fn retenir_le_focus(&self, ctx: &egui::Context, id: Id) {
        let ouvert = self.ouvert(ctx);
        ctx.memory_mut(|m| {
            m.set_focus_lock_filter(
                id,
                EventFilter {
                    tab: ouvert,
                    escape: ouvert,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                },
            );
        });
    }

    /// Dessine le panneau juste au-dessus de `ancre`. Rend ce qu'un clic a choisi.
    pub fn panneau(&mut self, ctx: &egui::Context, ancre: Rect) -> Option<Choix> {
        let genre = self.jeton.as_ref().map(|j| j.genre);
        let Some(genre) = genre.filter(|_| !self.liste.is_empty() && (self.focus || self.survol) && self.a_jour(ctx))
        else {
            self.survol = false;
            return None;
        };
        let largeur = ancre.width().clamp(260.0, 460.0);
        let cadre = marge::egale(espace::S);
        // La hauteur est connue d'avance : le panneau reste posé sur la saisie, sans
        // sauter à chaque lettre comme il le ferait en s'ancrant par le bas.
        // (Le cadre compte aussi son trait, d'un point de chaque côté.)
        let hauteur = TITRE + self.liste.len() as f32 * LIGNE + cadre.sum().y + 2.0;
        let mut clique = None;
        let zone = egui::Area::new(Id::unique("suggestions_saisie"))
            .order(Order::Foreground)
            // Tout de suite : on tape, et un fondu ferait attendre la liste.
            .fade_in(false)
            .fixed_pos(pos2(ancre.left(), ancre.top() - espace::XS - hauteur))
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(c::BG_RAISED)
                    .stroke(Stroke::new(1.0_f32, c::BORDER))
                    .corner_radius(CornerRadius::same(rayon::L))
                    .inner_margin(cadre)
                    .shadow(ui.visuals().popup_shadow)
                    .show(ui, |ui| {
                        ui.set_width(largeur);
                        ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                        titre(ui, genre, largeur);
                        for i in 0..self.liste.len() {
                            let (rect, reponse) = ui.allocate_exact_size(vec2(largeur, LIGNE), Sense::click());
                            if reponse.hovered() && ctx.input(|e| e.pointer.is_moving()) {
                                self.selection = i;
                            }
                            ligne(ui, rect, &self.liste[i], i == self.selection);
                            if reponse.clicked() {
                                clique = Some(i);
                            }
                        }
                    });
            });
        self.survol = ctx.pointer_hover_pos().is_some_and(|p| zone.response.rect.contains(p));
        clique.and_then(|i| self.choisir(i))
    }
}

fn titre(ui: &mut egui::Ui, genre: Genre, largeur: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(largeur, TITRE), Sense::hover());
    let police = FontId::proportional(texte::PETIT);
    let p = ui.painter();
    p.text(rect.left_center() + vec2(espace::XS, 0.0), Align2::LEFT_CENTER, genre.titre(), police.clone(), c::TEXT_DIM);
    p.text(
        rect.right_center() - vec2(espace::XS, 0.0),
        Align2::RIGHT_CENTER,
        "↑↓ choisir · Tab valider · Échap fermer",
        police,
        c::TEXT_FAINT,
    );
}

fn ligne(ui: &egui::Ui, rect: Rect, s: &Suggestion, choisie: bool) {
    let p = ui.painter();
    if choisie {
        p.rect_filled(rect, CornerRadius::same(rayon::M), c::BG_ACTIVE);
    }
    let gauche = rect.left() + espace::S;
    let centre = rect.center().y;
    let debut_texte = gauche + 24.0;
    let (couleur_libelle, suite) = match &s.apparence {
        Apparence::Membre { couleur, en_ligne } => {
            let point = if *en_ligne { c::ACCENT } else { c::TEXT_FAINT };
            p.circle_filled(pos2(gauche + 8.0, centre), 4.0, point);
            (*couleur, "")
        }
        Apparence::Commande { argument } => (c::TEXT, *argument),
        Apparence::Emoji(emoji) => {
            let vignette = Rect::from_center_size(pos2(gauche + 8.0, centre), egui::Vec2::splat(20.0));
            if !ki_ui::emoji::peindre_vignette(p, vignette, emoji) {
                p.text(vignette.center(), Align2::CENTER_CENTER, *emoji, FontId::proportional(texte::COURANT), c::TEXT);
            }
            (c::TEXT, "")
        }
    };
    let courant = FontId::proportional(texte::COURANT);
    let petit = FontId::proportional(texte::PETIT);
    // Le détail à droite d'abord : le libellé prend ce qui reste, et se coupe.
    let droite = rect.right() - espace::S;
    let detail = p.layout_no_wrap(s.detail.clone(), petit.clone(), c::TEXT_FAINT);
    p.galley(pos2(droite - detail.size().x, centre - detail.size().y / 2.0), detail.clone(), c::TEXT_FAINT);
    let mut job = egui::text::LayoutJob::default();
    job.append(&s.libelle, 0.0, egui::TextFormat::simple(courant, couleur_libelle));
    if !suite.is_empty() {
        job.append(suite, espace::S, egui::TextFormat::simple(petit, c::TEXT_DIM));
    }
    job.wrap.max_width = (droite - detail.size().x - espace::M - debut_texte).max(40.0);
    job.wrap.max_rows = 1;
    let galley = p.layout_job(job);
    p.galley(pos2(debut_texte, centre - galley.size().y / 2.0), galley, couleur_libelle);
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Event, RawInput, TextEdit};

    fn membre(pseudo: &str, en_ligne: bool) -> Membre {
        Membre { pseudo: pseudo.into(), couleur: Color32::WHITE, en_ligne, detail: "" }
    }

    fn sources() -> Sources {
        Sources {
            membres: vec![
                membre("Marie", true),
                membre("Marc", false),
                membre("Anne-Marie", true),
                membre("Élodie", true),
                membre("Bob", true),
            ],
            commandes: vec![POKE, MUSIQUE, PAUSE, CARTES],
        }
    }

    fn jeton(texte: &str) -> Option<Jeton> {
        jeton_au_curseur(texte, texte.chars().count())
    }

    fn libelles(jeton: &Jeton) -> Vec<String> {
        proposer(jeton, &sources()).into_iter().map(|s| s.libelle).collect()
    }

    #[test]
    fn le_mot_sous_le_curseur_decide() {
        let j = jeton("salut @ma").unwrap();
        assert_eq!((j.debut, j.fin, j.genre, j.requete.as_str()), (6, 9, Genre::Membre, "ma"));
        assert_eq!(jeton("@").unwrap().genre, Genre::Membre);
        assert_eq!(jeton("/").unwrap().genre, Genre::Commande);
        assert_eq!(jeton("/po").unwrap().requete, "po");
        assert_eq!(jeton(":feu").unwrap().genre, Genre::Emoji);
        assert_eq!(jeton("ligne\n@bo").unwrap().debut, 6);
        // Un pseudo accentué : les positions comptent en caractères.
        assert_eq!(jeton("é @é").unwrap().debut, 2);
    }

    #[test]
    fn ce_qui_ressemble_a_une_marque_n_en_est_pas_une() {
        for texte in [
            "bob",
            "moi@exemple.fr",
            "@bob@exemple",
            "un /chemin",
            "/home/moi",
            "/r/rust",
            "https://exemple.fr",
            "12:30",
            ":)",
            ":d",
            "`@bo",
            "```\n@bo",
            "bonjour ",
            "@bob ",
        ] {
            assert_eq!(jeton(texte), None, "« {texte} »");
        }
        // Le code refermé : on peut de nouveau mentionner.
        assert!(jeton("`code` @bo").is_some());
    }

    #[test]
    fn le_curseur_au_milieu_d_un_mot_ne_propose_rien() {
        assert_eq!(jeton_au_curseur("@bob", 2), None);
        let j = jeton_au_curseur("@bo et toi", 3).unwrap();
        assert_eq!((j.debut, j.fin), (0, 3));
        // Un curseur en retard sur le texte ne fait pas sortir du tableau.
        assert_eq!(jeton_au_curseur("@", 99).unwrap().fin, 1);
    }

    #[test]
    fn les_pseudos_se_trient_du_plus_probable_au_moins_probable() {
        let j = jeton("@").unwrap();
        // Tous, ceux qui sont là d'abord, le hors ligne en dernier.
        assert_eq!(libelles(&j), ["Anne-Marie", "Bob", "Élodie", "Marie", "Marc"]);
        // Le début du pseudo passe avant le milieu ; en ligne avant hors ligne.
        assert_eq!(libelles(&jeton("@mar").unwrap()), ["Marie", "Marc", "Anne-Marie"]);
        // Ni la casse ni les accents.
        assert_eq!(libelles(&jeton("@ELO").unwrap()), ["Élodie"]);
        assert!(libelles(&jeton("@zzz").unwrap()).is_empty());
    }

    #[test]
    fn la_mention_inseree_est_reconnue_par_le_rendu() {
        let l = proposer(&jeton("@anne").unwrap(), &sources());
        assert_eq!(l[0].insertion, "@Anne-Marie ");
        let membres = ["Anne-Marie"];
        assert!(ki_core::markup::me_mentionne(&l[0].insertion, &membres, "Anne-Marie"));
    }

    #[test]
    fn on_ne_propose_que_les_commandes_disponibles() {
        assert_eq!(libelles(&jeton("/").unwrap()), ["/poke", "/musique", "/pause", "/cartes"]);
        assert_eq!(libelles(&jeton("/P").unwrap()), ["/poke", "/pause"]);
        let sans_droit = Sources { commandes: vec![POKE], ..sources() };
        assert_eq!(proposer(&jeton("/pa").unwrap(), &sans_droit), Vec::new());
    }

    #[test]
    fn un_mot_deja_complet_ne_retient_plus_entree() {
        assert!(proposer(&jeton("/pause").unwrap(), &sources()).is_empty());
        assert!(proposer(&jeton("@bob").unwrap(), &sources()).is_empty());
        // `/poke` reste proposée : elle ajoute le @ qui rouvre les pseudos.
        assert_eq!(proposer(&jeton("/poke").unwrap(), &sources())[0].insertion, "/poke @");
    }

    #[test]
    fn les_emoji_se_cherchent_en_francais() {
        let l = proposer(&jeton(":feu").unwrap(), &sources());
        assert!(l.iter().any(|s| s.insertion == "🔥 "), "{l:?}");
        assert!(l.len() <= MAX_SUGGESTIONS);
    }

    #[test]
    fn une_commande_se_lit_et_un_message_reste_un_message() {
        assert_eq!(lire_commande("/poke @Bob"), Some(Ordre::Poke("Bob")));
        assert_eq!(lire_commande("/poke Jean Dupont"), Some(Ordre::Poke("Jean Dupont")));
        assert_eq!(lire_commande("/POKE"), Some(Ordre::Poke("")));
        assert_eq!(lire_commande("/musique daft punk"), Some(Ordre::Musique("daft punk")));
        assert_eq!(lire_commande("/pause"), Some(Ordre::Pause));
        assert_eq!(lire_commande("/reprendre"), Some(Ordre::Reprendre));
        assert_eq!(lire_commande("/suivant"), Some(Ordre::Suivant));
        assert_eq!(lire_commande("/cartes"), Some(Ordre::Cartes));
        assert_eq!(lire_commande("/chercher  rendez-vous 21h "), Some(Ordre::Chercher("rendez-vous 21h")));
        assert_eq!(lire_commande("/chercher"), Some(Ordre::Chercher("")));
        assert_eq!(lire_commande("/moi part manger"), Some(Ordre::Moi("part manger")));
        assert_eq!(lire_commande("/muet"), Some(Ordre::Muet));
        assert_eq!(lire_commande("/Sourd"), Some(Ordre::Sourd));
        assert_eq!(lire_commande("/tableau"), Some(Ordre::Tableau));
        assert_eq!(lire_commande("/raccourcis"), Some(Ordre::Raccourcis));
        for message in ["bonjour", "/s", "/inconnue test", "/pause café ?", "/home/moi", " /pause", "pause", "/muet stp", "/tableau des scores"] {
            assert_eq!(lire_commande(message), None, "« {message} »");
        }
    }

    // ---------------------------------------------------------------------
    // Le panneau et une vraie zone de saisie, touche par touche, dans l'ordre
    // de `chat_input`.
    // ---------------------------------------------------------------------

    struct Banc {
        ctx: egui::Context,
        texte: String,
        suggestions: Suggestions,
        envoyes: Vec<String>,
        echap_ailleurs: u32,
        redonner_focus: bool,
    }

    fn touche(key: Key, modifiers: Modifiers) -> Event {
        Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    impl Banc {
        fn new() -> Self {
            let mut banc = Self {
                ctx: egui::Context::default(),
                texte: String::new(),
                suggestions: Suggestions::default(),
                envoyes: Vec::new(),
                echap_ailleurs: 0,
                redonner_focus: true,
            };
            banc.image(Vec::new());
            banc.image(Vec::new());
            banc
        }

        fn id() -> Id {
            Id::unique("saisie_de_banc")
        }

        /// Comme `remplacer_dans_la_saisie` : le mot remplacé, le curseur après,
        /// et le focus rendu à la saisie.
        fn appliquer(&mut self, ctx: &egui::Context, choix: &Choix) {
            let octet = |n: usize| self.texte.char_indices().nth(n).map_or(self.texte.len(), |(o, _)| o);
            let (a, b) = (octet(choix.debut), octet(choix.fin));
            self.texte.replace_range(a..b, &choix.insertion);
            let mut etat = TextEdit::load_state(ctx, Self::id()).unwrap_or_default();
            let c = egui::text::CCursor::new(choix.debut + choix.insertion.chars().count());
            etat.cursor.set_char_range(Some(egui::text::CCursorRange::one(c)));
            etat.store(ctx, Self::id());
            self.redonner_focus = true;
        }

        fn image(&mut self, events: Vec<Event>) {
            let ctx = self.ctx.clone();
            let entree = RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(800.0, 600.0))),
                events,
                ..Default::default()
            };
            let sortie = ctx.run_ui(entree, |ui| {
                self.suggestions.echap(ui.ctx());
                if ui.input(|i| i.key_pressed(Key::Escape)) {
                    self.echap_ailleurs += 1;
                }
                if let Some(choix) = self.suggestions.clavier(ui.ctx()) {
                    self.appliquer(ui.ctx(), &choix);
                }
                let edit = TextEdit::multiline(&mut self.texte).id(Self::id()).show(ui);
                let reponse = edit.response.response;
                if std::mem::take(&mut self.redonner_focus) {
                    reponse.request_focus();
                }
                if reponse.has_focus() && ui.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.shift) {
                    if self.texte.ends_with('\n') {
                        self.texte.pop();
                    }
                    self.envoyes.push(std::mem::take(&mut self.texte));
                }
                let curseur = edit
                    .cursor_range
                    .filter(|r| r.primary == r.secondary)
                    .map(|r| r.primary.index.0);
                self.suggestions.actualiser(ui.ctx(), &self.texte, curseur, reponse.has_focus(), sources);
                self.suggestions.retenir_le_focus(ui.ctx(), Self::id());
                let ancre = Rect::from_min_size(pos2(10.0, 500.0), vec2(500.0, 40.0));
                if let Some(choix) = self.suggestions.panneau(ui.ctx(), ancre) {
                    self.appliquer(ui.ctx(), &choix);
                }
            });
            sortie.drop_without_applying_deltas();
        }

        fn taper(&mut self, texte: &str) {
            for lettre in texte.chars() {
                self.image(vec![Event::Text(lettre.to_string())]);
            }
        }

        fn appuyer(&mut self, key: Key) {
            self.image(vec![touche(key, Modifiers::NONE)]);
            self.image(Vec::new());
        }

        fn ouvert(&self) -> bool {
            self.suggestions.ouvert(&self.ctx)
        }

        fn a_le_focus(&self) -> bool {
            self.ctx.memory(|m| m.has_focus(Self::id()))
        }
    }

    #[test]
    fn taper_un_arobase_ouvre_le_panneau_et_tab_complete() {
        let mut banc = Banc::new();
        banc.taper("salut @ma");
        assert!(banc.ouvert());
        banc.appuyer(Key::Tab);
        assert_eq!(banc.texte, "salut @Marie ");
        assert!(banc.a_le_focus(), "Tab a fait partir le focus");
        assert!(!banc.ouvert());
        // Et l'on continue d'écrire à la suite.
        banc.taper("ça va");
        assert_eq!(banc.texte, "salut @Marie ça va");
    }

    #[test]
    fn entree_valide_sans_envoyer_puis_envoie() {
        let mut banc = Banc::new();
        banc.taper("@bo");
        banc.appuyer(Key::Enter);
        assert_eq!(banc.texte, "@Bob ");
        assert!(banc.envoyes.is_empty(), "le message est parti : {:?}", banc.envoyes);
        banc.appuyer(Key::Enter);
        assert_eq!(banc.envoyes, ["@Bob "]);
    }

    #[test]
    fn les_fleches_choisissent_et_font_le_tour() {
        let mut banc = Banc::new();
        banc.taper("@mar");
        banc.appuyer(Key::ArrowDown);
        banc.appuyer(Key::Tab);
        assert_eq!(banc.texte, "@Marc ");

        let mut banc = Banc::new();
        banc.taper("@mar");
        // Vers le haut depuis le premier : le dernier.
        banc.appuyer(Key::ArrowUp);
        banc.appuyer(Key::Tab);
        assert_eq!(banc.texte, "@Anne-Marie ");
    }

    #[test]
    fn echap_referme_sans_toucher_au_reste_et_le_mot_suivant_rouvre() {
        let mut banc = Banc::new();
        banc.taper("@ma");
        banc.appuyer(Key::Escape);
        assert!(!banc.ouvert());
        assert_eq!(banc.echap_ailleurs, 0, "Échap a aussi fermé autre chose");
        assert!(banc.a_le_focus(), "Échap a fait partir le focus");
        assert_eq!(banc.texte, "@ma");
        // Toujours fermé en continuant ce mot, Entrée envoie donc le message.
        banc.taper("r");
        assert!(!banc.ouvert());
        banc.taper(" @b");
        assert!(banc.ouvert());
        // Panneau fermé, Échap repart à ses autres usages.
        banc.appuyer(Key::Escape);
        banc.appuyer(Key::Escape);
        assert_eq!(banc.echap_ailleurs, 1);
    }

    #[test]
    fn une_commande_rouvre_les_pseudos_et_une_commande_complete_part_a_la_premiere_entree() {
        let mut banc = Banc::new();
        banc.taper("/po");
        banc.appuyer(Key::Tab);
        assert_eq!(banc.texte, "/poke @");
        assert!(banc.ouvert(), "les pseudos auraient dû s'ouvrir");
        banc.taper("bo");
        banc.appuyer(Key::Enter);
        assert_eq!(banc.texte, "/poke @Bob ");

        let mut banc = Banc::new();
        banc.taper("/pause");
        assert!(!banc.ouvert());
        banc.appuyer(Key::Enter);
        assert_eq!(banc.envoyes, ["/pause"]);
    }

    #[test]
    fn sans_panneau_tab_et_les_fleches_gardent_leur_role() {
        let mut banc = Banc::new();
        banc.taper("bonjour");
        banc.appuyer(Key::Enter);
        assert_eq!(banc.envoyes, ["bonjour"]);
        // Maj+Entrée va à la ligne, panneau ouvert ou non.
        banc.taper("@ma");
        banc.image(vec![touche(Key::Enter, Modifiers::SHIFT)]);
        assert!(banc.envoyes.len() == 1);
    }

    /// Un clic retire le focus à la saisie au relâchement : le panneau doit tenir
    /// jusque-là, et rendre le focus ensuite.
    #[test]
    fn un_clic_sur_une_ligne_la_choisit_et_rend_le_focus() {
        let mut banc = Banc::new();
        banc.taper("@mar");
        assert!(banc.ouvert());
        // Trois lignes au-dessus de l'ancre : la première, sous le cadre et le titre.
        let hauteur = TITRE + 3.0 * LIGNE + espace::S * 2.0 + 2.0;
        let pos = pos2(100.0, 500.0 - espace::XS - hauteur + 1.0 + espace::S + TITRE + LIGNE / 2.0);
        let appui = |pressed| Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        banc.image(vec![Event::PointerMoved(pos)]);
        banc.image(vec![appui(true)]);
        banc.image(vec![appui(false)]);
        banc.image(Vec::new());
        assert_eq!(banc.texte, "@Marie ");
        assert!(banc.a_le_focus());
    }

    /// Un clic ailleurs : le panneau disparaît, et le texte ne bouge pas.
    #[test]
    fn un_clic_ailleurs_referme_le_panneau() {
        let mut banc = Banc::new();
        banc.taper("@mar");
        let pos = pos2(700.0, 580.0);
        let appui = |pressed| Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        banc.image(vec![Event::PointerMoved(pos)]);
        banc.image(vec![appui(true)]);
        banc.image(vec![appui(false)]);
        banc.image(Vec::new());
        assert!(!banc.ouvert());
        assert_eq!(banc.texte, "@mar");
    }
}
