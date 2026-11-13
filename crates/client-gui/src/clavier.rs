//! Les raccourcis du chat : mettre en forme ce qu'on écrit, et passer d'un
//! salon à l'autre sans la souris.
//!
//! - Dans la saisie : Ctrl+B (gras), Ctrl+I (italique), Ctrl+E (code)
//!   emballent la sélection — ou posent la paire de marques, le curseur au
//!   milieu. Refaits, ils la déballent.
//! - Partout : Alt+↑ / Alt+↓ vont au salon textuel d'à côté, Alt+Maj+↑ / ↓
//!   au prochain qui a du non-lu ; Ctrl+F ouvre la recherche, Ctrl+K la
//!   fenêtre « aller à » (voir `aller_a`).
//! - Dans le fil : PageHaut / PageBas remontent ou redescendent d'un écran,
//!   Échap revient tout en bas — et marque lu, puisqu'on y est.
//!
//! Ce module tient le calcul, qui se teste sans fenêtre, et ce que chaque
//! action fait ; quelle touche la demande, c'est `registre` qui le sait —
//! les combinaisons ci-dessus sont celles d'origine, et se changent dans la
//! fenêtre « Raccourcis clavier ».

use eframe::egui;
use ki_protocol::{ChannelId, ChannelKind};

use crate::registre::Action;
use crate::saisie::id_saisie;
use crate::KiApp;

/// Ce qu'un raccourci de mise en forme pose autour de la sélection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Marque {
    Gras,
    Italique,
    Code,
}

impl Marque {
    fn signe(self) -> char {
        match self {
            Marque::Gras | Marque::Italique => '*',
            Marque::Code => '`',
        }
    }

    fn longueur(self) -> usize {
        match self {
            Marque::Gras => 2,
            Marque::Italique | Marque::Code => 1,
        }
    }

    /// `n` signes de suite, de chaque côté : la marque y est-elle ? Trois
    /// étoiles sont du gras **et** de l'italique ; deux, du gras seul.
    fn presente(self, n: usize) -> bool {
        match self {
            Marque::Gras => n >= 2,
            Marque::Italique => n % 2 == 1,
            Marque::Code => n >= 1,
        }
    }
}

/// Emballe les caractères `debut..fin` de `texte` dans la marque — ou les
/// déballe s'ils le sont déjà, que les signes soient juste autour de la
/// sélection ou pris dedans. Rend le texte et la sélection à y remettre :
/// la même, sans ses marques. Une sélection vide reçoit la paire, le curseur
/// au milieu.
pub(crate) fn emballer(texte: &str, debut: usize, fin: usize, marque: Marque) -> (String, usize, usize) {
    let mut c: Vec<char> = texte.chars().collect();
    let (debut, fin) = (debut.min(fin).min(c.len()), fin.max(debut).min(c.len()));
    let (signe, n) = (marque.signe(), marque.longueur());
    let suite = |it: &mut dyn Iterator<Item = &char>| it.take_while(|x| **x == signe).count();

    // Les signes juste autour de la sélection.
    let avant = suite(&mut c[..debut].iter().rev());
    let apres = suite(&mut c[fin..].iter());
    if marque.presente(avant.min(apres)) {
        c.drain(fin..fin + n);
        c.drain(debut - n..debut);
        return (c.into_iter().collect(), debut - n, fin - n);
    }
    // Les signes pris dans la sélection, à ses deux bouts.
    let dedans = &c[debut..fin];
    let (tete, queue) = (suite(&mut dedans.iter()), suite(&mut dedans.iter().rev()));
    if dedans.len() > 2 * n && tete + queue < dedans.len() && marque.presente(tete.min(queue)) {
        c.drain(fin - n..fin);
        c.drain(debut..debut + n);
        return (c.into_iter().collect(), debut, fin - 2 * n);
    }
    // Rien : on emballe.
    for position in [fin, debut] {
        for _ in 0..n {
            c.insert(position, signe);
        }
    }
    (c.into_iter().collect(), debut + n, fin + n)
}

/// Le salon où aller depuis `courant`, dans la liste `salons` (chacun avec
/// « il a du non-lu ») : le suivant ou le précédent, en bouclant ; avec
/// `non_lus`, le prochain qui a du non-lu. Rien s'il n'y a nulle part où
/// aller.
pub(crate) fn salon_voisin(salons: &[(ChannelId, bool)], courant: Option<ChannelId>, vers_le_bas: bool, non_lus: bool) -> Option<ChannelId> {
    let n = salons.len();
    if n == 0 {
        return None;
    }
    let ici = courant.and_then(|c| salons.iter().position(|(id, _)| *id == c));
    (1..=n)
        .map(|pas| match (ici, vers_le_bas) {
            (Some(i), true) => (i + pas) % n,
            (Some(i), false) => (i + n - pas % n) % n,
            (None, true) => pas - 1,
            (None, false) => n - pas,
        })
        .map(|i| salons[i])
        .find(|(id, a_du_non_lu)| Some(*id) != courant && (!non_lus || *a_du_non_lu))
        .map(|(id, _)| id)
}

/// Ce qu'une page du clavier fait défiler, en hauteurs de vue : un peu moins
/// qu'un écran, pour garder sous les yeux la ligne où l'on en était.
const PAGE: f32 = 0.85;

impl KiApp {
    /// Les touches du fil, à lire **après** la saisie — Échap y renonce
    /// d'abord à une réponse ou à une modification — et avant de dessiner
    /// le fil. Rend de combien de pages défiler ; Échap, lui, demande le
    /// retour en bas. Rien quand quelque chose recouvre le fil.
    pub(crate) fn fil_au_clavier(&mut self, ctx: &egui::Context) -> f32 {
        let recouvert = ki_ui::fenetre::une_est_ouverte(ctx)
            || egui::Popup::is_any_open(ctx)
            || self.visionneuse.est_ouverte()
            || self.atelier.est_ouvert();
        if recouvert {
            return 0.0;
        }
        if !self.fil_en_bas && crate::prendre_echap(ctx) {
            self.fil_au_bas = true;
            return 0.0;
        }
        let aucune = egui::Modifiers::NONE;
        ctx.input_mut(|i| {
            if i.consume_key(aucune, egui::Key::PageUp) {
                -PAGE
            } else if i.consume_key(aucune, egui::Key::PageDown) {
                PAGE
            } else {
                0.0
            }
        })
    }

    /// Les raccourcis de mise en forme, à lire **avant** de dessiner la
    /// saisie : elle prendrait ces touches pour elle. Seulement quand c'est
    /// elle qui a le clavier.
    pub(crate) fn mise_en_forme_au_clavier(&mut self, ctx: &egui::Context) {
        if !ctx.memory(|m| m.has_focus(id_saisie())) {
            return;
        }
        let marque = ctx.input_mut(|i| {
            [(Action::Gras, Marque::Gras), (Action::Italique, Marque::Italique), (Action::Code, Marque::Code)]
                .into_iter()
                .find(|(action, _)| self.registre.faite(i, *action))
                .map(|(_, marque)| marque)
        });
        let Some(marque) = marque else { return };
        let mut etat = egui::TextEdit::load_state(ctx, id_saisie()).unwrap_or_default();
        let total = self.input.chars().count();
        let (debut, fin) = match etat.cursor.char_range() {
            Some(r) => (r.primary.index.0.min(r.secondary.index.0), r.primary.index.0.max(r.secondary.index.0)),
            None => (total, total),
        };
        let (texte, debut, fin) = emballer(&self.input, debut, fin, marque);
        if texte.chars().count() > ki_protocol::MAX_CHAT_TEXT {
            return;
        }
        self.input = texte;
        let selection = egui::text::CCursorRange::two(egui::text::CCursor::new(debut), egui::text::CCursor::new(fin));
        etat.cursor.set_char_range(Some(selection));
        etat.store(ctx, id_saisie());
    }

    /// Les raccourcis de navigation, à lire en tête d'image : d'un salon
    /// textuel à l'autre, vers le prochain non-lu, « aller à », la
    /// recherche, les réglages, la liste des raccourcis.
    pub(crate) fn navigation_au_clavier(&mut self, ctx: &egui::Context) {
        // On choisit une combinaison : le clavier est à la fenêtre des
        // raccourcis, en entier.
        if self.raccourcis.lire_la_capture(ctx, &mut self.registre) {
            return;
        }
        let faite = |app: &Self, action| ctx.input_mut(|i| app.registre.faite(i, action));
        if faite(self, Action::Raccourcis) {
            self.raccourcis.basculer();
        }
        if faite(self, Action::Reglages) {
            self.basculer_les_reglages();
        }
        // « Aller à » s'ouvre, et se referme. Ouverte, elle lit ses touches
        // ici, avant tout le monde, et le reste attend.
        if faite(self, Action::AllerA) {
            if self.aller_a.ouvert {
                self.aller_a.fermer();
            } else {
                self.aller_a.ouvrir();
            }
        }
        if self.aller_a.ouvert {
            self.aller_a.lire_les_touches(ctx);
            return;
        }
        let registre = &self.registre;
        let geste = ctx.input_mut(|i| {
            [
                (Action::SalonSuivant, (true, false)),
                (Action::SalonPrecedent, (false, false)),
                (Action::NonLuSuivant, (true, true)),
                (Action::NonLuPrecedent, (false, true)),
            ]
            .into_iter()
            .find(|(action, _)| registre.faite(i, *action))
            .map(|(_, geste)| geste)
        });
        if let Some((vers_le_bas, non_lus)) = geste {
            // Dans l'ordre de la colonne : les salons textuels, puis ceux
            // des portes web.
            let textuels = self.session.channels.iter().filter(|c| c.kind == ChannelKind::Text);
            let salons: Vec<(ChannelId, bool)> = textuels
                .clone()
                .filter(|c| c.expire_le.is_none())
                .chain(textuels.filter(|c| c.expire_le.is_some()))
                .map(|c| (c.id, self.session.non_lus.get(&c.id).is_some_and(|n| n.nb > 0)))
                .collect();
            match salon_voisin(&salons, self.session.current, vers_le_bas, non_lus) {
                Some(id) => {
                    self.retour_au_chat();
                    self.join(id);
                    self.focus_input = true;
                }
                None if non_lus => self.info = Some("rien de non lu ailleurs".into()),
                None => {}
            }
        }
        if faite(self, Action::Chercher) {
            self.retour_au_chat();
            self.recherche.ouverte = true;
            self.recherche.focus = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn une_selection_s_emballe_et_se_deballe() {
        // « bonjour à tous », « à » sélectionné (caractères 8..9).
        let (gras, d, f) = emballer("bonjour à tous", 8, 9, Marque::Gras);
        assert_eq!((gras.as_str(), d, f), ("bonjour **à** tous", 10, 11));
        // Refait sur la même sélection : déballé, sélection gardée.
        assert_eq!(emballer(&gras, d, f, Marque::Gras), ("bonjour à tous".to_string(), 8, 9));
        // Les marques prises dans la sélection : déballé aussi.
        assert_eq!(emballer(&gras, 8, 13, Marque::Gras), ("bonjour à tous".to_string(), 8, 9));
        // Italique et code.
        assert_eq!(emballer("un mot", 3, 6, Marque::Italique), ("un *mot*".to_string(), 4, 7));
        assert_eq!(emballer("un *mot*", 4, 7, Marque::Italique), ("un mot".to_string(), 3, 6));
        assert_eq!(emballer("cargo run", 0, 9, Marque::Code), ("`cargo run`".to_string(), 1, 10));
        assert_eq!(emballer("`cargo run`", 1, 10, Marque::Code), ("cargo run".to_string(), 0, 9));
        // La sélection à l'envers (du dernier caractère vers le premier).
        assert_eq!(emballer("un mot", 6, 3, Marque::Gras), ("un **mot**".to_string(), 5, 8));
    }

    /// Le gras et l'italique partagent l'étoile : l'un ne défait pas
    /// l'autre.
    #[test]
    fn le_gras_et_l_italique_ne_se_marchent_pas_dessus() {
        // Italique sur du gras : trois étoiles, pas « déballer le gras ».
        let (les_deux, d, f) = emballer("**fort**", 2, 6, Marque::Italique);
        assert_eq!((les_deux.as_str(), d, f), ("***fort***", 3, 7));
        // Puis retirer le gras laisse l'italique, et inversement.
        assert_eq!(emballer(&les_deux, 3, 7, Marque::Gras), ("*fort*".to_string(), 1, 5));
        assert_eq!(emballer(&les_deux, 3, 7, Marque::Italique), ("**fort**".to_string(), 2, 6));
        // Gras sur de l'italique.
        assert_eq!(emballer("*fort*", 1, 5, Marque::Gras), ("***fort***".to_string(), 3, 7));
    }

    #[test]
    fn sans_selection_la_paire_se_pose_curseur_au_milieu() {
        assert_eq!(emballer("dis ", 4, 4, Marque::Gras), ("dis ****".to_string(), 6, 6));
        // Refait aussitôt : la paire vide s'en va.
        assert_eq!(emballer("dis ****", 6, 6, Marque::Gras), ("dis ".to_string(), 4, 4));
        assert_eq!(emballer("", 0, 0, Marque::Code), ("``".to_string(), 1, 1));
        // Des bornes hors du texte ne font pas paniquer.
        assert_eq!(emballer("ab", 9, 12, Marque::Italique), ("ab**".to_string(), 3, 3));
        // Un texte fait seulement d'étoiles ne se « déballe » pas en rien.
        assert_eq!(emballer("****", 0, 4, Marque::Gras), ("********".to_string(), 2, 6));
    }

    #[test]
    fn on_passe_d_un_salon_a_l_autre_en_bouclant() {
        let salons = [(1, false), (2, false), (5, true), (9, false)];
        assert_eq!(salon_voisin(&salons, Some(1), true, false), Some(2));
        assert_eq!(salon_voisin(&salons, Some(9), true, false), Some(1), "du dernier au premier");
        assert_eq!(salon_voisin(&salons, Some(1), false, false), Some(9), "du premier au dernier");
        assert_eq!(salon_voisin(&salons, Some(5), false, false), Some(2));
        // Vers le prochain non-lu, dans les deux sens.
        assert_eq!(salon_voisin(&salons, Some(1), true, true), Some(5));
        assert_eq!(salon_voisin(&salons, Some(9), false, true), Some(5));
        assert_eq!(salon_voisin(&salons, Some(5), true, true), None, "le seul non-lu est celui qu'on lit");
        // Nulle part encore (page VALORANT, salon disparu) : le premier, ou le dernier.
        assert_eq!(salon_voisin(&salons, None, true, false), Some(1));
        assert_eq!(salon_voisin(&salons, Some(77), false, false), Some(9));
        // Rien où aller.
        assert_eq!(salon_voisin(&[], Some(1), true, false), None);
        assert_eq!(salon_voisin(&[(1, false)], Some(1), true, false), None);
    }
}
