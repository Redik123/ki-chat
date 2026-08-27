//! Découpage d'un message en fragments à peindre : liens, mentions, gras,
//! italique, code.
//!
//! # Pourquoi un module à part
//!
//! C'est de la **logique pure** : une chaîne entre, une structure sort. Elle
//! se teste donc sans ouvrir de fenêtre, là où le rendu lui-même ne se juge
//! qu'à l'œil. Tout ce qui peut se tromper — une étoile isolée, un accent
//! coupé en deux, une mention qui déborde sur le mot suivant — est attrapé
//! ici plutôt qu'observé par trente personnes.
//!
//! # Ce qu'on ne fait pas
//!
//! Pas de Markdown complet. Ni titres, ni listes, ni tableaux, ni images : ce
//! sont des messages de chat, pas des documents. Quatre marques, celles qu'on
//! tape sans y penser parce qu'elles viennent de Discord — `**gras**`,
//! `*italique*`, `` `code` `` et les blocs triples — plus les liens et les
//! mentions.
//!
//! # Le texte reçu vient de quelqu'un d'autre
//!
//! Il a déjà traversé `safe_display` côté protocole (caractères de contrôle et
//! commandes bidirectionnelles retirés). Ce module n'ajoute pas de règle de
//! sécurité : il découpe, et ne rend jamais que des tranches du texte reçu —
//! jamais de chaîne construite, jamais de contenu réinterprété.

/// Un fragment d'une ligne, tel qu'il se peint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fragment<'a> {
    Texte(&'a str),
    Lien(&'a str),
    /// Une mention. `moi` distingue « on parle de moi » de « on parle de
    /// quelqu'un » : la première mérite d'attirer l'œil, la seconde non.
    Mention { pseudo: &'a str, moi: bool },
    Gras(&'a str),
    Italique(&'a str),
    Code(&'a str),
}

/// Un bloc de message. Les blocs de code sortent du fil de la phrase : ils
/// occupent leur propre espace, sans retour à la ligne automatique.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bloc<'a> {
    Ligne(Vec<Fragment<'a>>),
    Code(&'a str),
}

/// Découpe un message en blocs.
///
/// `membres` sert à reconnaître les mentions : `@` suivi d'un pseudo **connu**.
/// Sans cette liste, `@` n'importe quoi passerait pour une mention, et une
/// adresse électronique écrite dans un message deviendrait un surlignage.
///
/// `moi` est le pseudo de celui qui lit, pour distinguer sa propre mention.
pub fn decouper<'a>(texte: &'a str, membres: &[&str], moi: Option<&str>) -> Vec<Bloc<'a>> {
    let mut blocs = Vec::new();
    let mut reste = texte;

    while let Some(debut) = reste.find("```") {
        // Ce qui précède la clôture ouvrante est du texte ordinaire — moins le
        // saut de ligne qui la précède, lequel appartient à la clôture et non
        // au texte. Sans ce retrait, tout bloc de code s'ouvrait sur une ligne
        // vide et s'en refermait d'une autre.
        pousser_lignes(&mut blocs, sans_fin_de_ligne(&reste[..debut]), membres, moi);
        let apres = &reste[debut + 3..];
        match apres.find("```") {
            Some(fin) => {
                blocs.push(Bloc::Code(nettoyer_bloc(&apres[..fin])));
                reste = sans_debut_de_ligne(&apres[fin + 3..]);
            }
            None => {
                // Clôture jamais refermée : on prend tout ce qui suit. Le
                // contraire — traiter les trois accents graves comme du texte
                // — ferait apparaître un bloc de code puis disparaître au
                // caractère suivant, pendant qu'on tape.
                blocs.push(Bloc::Code(nettoyer_bloc(apres)));
                reste = "";
            }
        }
    }
    pousser_lignes(&mut blocs, reste, membres, moi);
    blocs
}

/// Retire le saut de ligne d'ouverture et l'espace de fermeture d'un bloc de
/// code, sans toucher à son indentation.
fn nettoyer_bloc(bloc: &str) -> &str {
    sans_debut_de_ligne(bloc).trim_end_matches(['\n', '\r'])
}

/// Retire **un** saut de ligne en tête. Un seul : une ligne vraiment vide,
/// voulue par l'auteur, reste une ligne vide.
fn sans_debut_de_ligne(texte: &str) -> &str {
    texte
        .strip_prefix("\r\n")
        .or_else(|| texte.strip_prefix('\n'))
        .unwrap_or(texte)
}

/// Retire **un** saut de ligne en fin, même raison.
fn sans_fin_de_ligne(texte: &str) -> &str {
    match texte.strip_suffix('\n') {
        Some(t) => t.strip_suffix('\r').unwrap_or(t),
        None => texte,
    }
}

fn pousser_lignes<'a>(
    blocs: &mut Vec<Bloc<'a>>,
    texte: &'a str,
    membres: &[&str],
    moi: Option<&str>,
) {
    if texte.is_empty() {
        return;
    }
    for ligne in texte.split('\n') {
        let ligne = ligne.strip_suffix('\r').unwrap_or(ligne);
        blocs.push(Bloc::Ligne(fragments(ligne, membres, moi)));
    }
}

/// Découpe une ligne en fragments.
///
/// L'ordre des reconnaissances n'est pas indifférent. Le code littéral passe
/// **en premier** : ce qu'il contient ne doit être réinterprété par personne,
/// et écrire `` `**` `` doit donner deux étoiles, pas du gras avorté. Les
/// liens ensuite, avant le gras : une adresse peut contenir des étoiles.
fn fragments<'a>(ligne: &'a str, membres: &[&str], moi: Option<&str>) -> Vec<Fragment<'a>> {
    let mut out = Vec::new();
    let octets = ligne.as_bytes();
    let mut debut = 0usize;
    let mut i = 0usize;

    while i < ligne.len() {
        // On ne coupe jamais au milieu d'un caractère : le texte reçu est de
        // l'UTF-8 quelconque, accents et émojis compris.
        if !ligne.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let reste = &ligne[i..];

        // 1. Code littéral.
        if octets[i] == b'`' {
            if let Some(fin) = reste[1..].find('`') {
                let contenu = &reste[1..1 + fin];
                if !contenu.is_empty() {
                    pousser_texte(&mut out, &ligne[debut..i]);
                    out.push(Fragment::Code(contenu));
                    i += 1 + fin + 1;
                    debut = i;
                    continue;
                }
            }
        }

        // 2. Liens.
        if reste.starts_with("http://") || reste.starts_with("https://") {
            let fin = reste.find(char::is_whitespace).map_or(ligne.len(), |o| i + o);
            let lien = rogner_ponctuation(&ligne[i..fin]);
            pousser_texte(&mut out, &ligne[debut..i]);
            out.push(Fragment::Lien(lien));
            i += lien.len();
            debut = i;
            continue;
        }

        // 3. Gras, puis italique — le double avant le simple, sans quoi
        //    `**gras**` se lirait comme un italique vide suivi de texte.
        //
        //    Un drapeau explicite, et non une comparaison de curseurs : la
        //    première version testait `debut == i`, ce qui est vrai au tout
        //    premier caractère comme après chaque balise — et bouclait donc
        //    sans jamais avancer. Les tests ont pendu au lieu d'échouer, ce
        //    qui est la façon la plus désagréable d'apprendre qu'on s'est
        //    trompé.
        let mut balise_trouvee = false;
        for (marque, fabrique) in [
            ("**", Fragment::Gras as fn(&'a str) -> Fragment<'a>),
            ("*", Fragment::Italique as fn(&'a str) -> Fragment<'a>),
        ] {
            if let Some(contenu) = entoure(reste, marque) {
                pousser_texte(&mut out, &ligne[debut..i]);
                out.push(fabrique(contenu));
                i += marque.len() * 2 + contenu.len();
                debut = i;
                balise_trouvee = true;
                break;
            }
        }
        if balise_trouvee {
            continue;
        }

        // 4. Mentions.
        if octets[i] == b'@' {
            if let Some(pseudo) = mention(&reste[1..], membres) {
                pousser_texte(&mut out, &ligne[debut..i]);
                let moi = moi.is_some_and(|m| m.eq_ignore_ascii_case(pseudo));
                out.push(Fragment::Mention { pseudo, moi });
                i += 1 + pseudo.len();
                debut = i;
                continue;
            }
        }

        i += 1;
    }
    pousser_texte(&mut out, &ligne[debut..]);
    if out.is_empty() {
        out.push(Fragment::Texte(""));
    }
    out
}

fn pousser_texte<'a>(out: &mut Vec<Fragment<'a>>, texte: &'a str) {
    if !texte.is_empty() {
        out.push(Fragment::Texte(texte));
    }
}

/// Le contenu entouré par `marque`, s'il y en a un et qu'il n'est pas vide.
///
/// Refuse le contenu vide et celui qui commence par une espace : `a * b * c`
/// est de l'arithmétique, pas de l'italique, et le traiter autrement
/// transformerait la moitié des messages en charabia penché.
fn entoure<'a>(reste: &'a str, marque: &str) -> Option<&'a str> {
    let apres = reste.strip_prefix(marque)?;
    if apres.starts_with(' ') || apres.starts_with(marque) {
        return None;
    }
    let fin = apres.find(marque)?;
    let contenu = &apres[..fin];
    if contenu.is_empty() || contenu.ends_with(' ') {
        return None;
    }
    Some(contenu)
}

/// Le pseudo mentionné à cet endroit, s'il en est un connu.
///
/// Le **plus long** d'abord : sans ça, `@marie-claire` s'arrêterait à `@marie`
/// si les deux comptes existent, et surlignerait la mauvaise personne.
fn mention<'a>(apres_arobase: &'a str, membres: &[&str]) -> Option<&'a str> {
    let mut meilleur: Option<&'a str> = None;
    for membre in membres {
        if membre.is_empty() || apres_arobase.len() < membre.len() {
            continue;
        }
        let debut = &apres_arobase[..membre.len()];
        if !debut.eq_ignore_ascii_case(membre) {
            continue;
        }
        // La mention doit se terminer sur une frontière : `@marie` ne
        // s'accroche pas au milieu de `@mariette`.
        let suite = &apres_arobase[membre.len()..];
        let fin_nette = suite
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_' && c != '-');
        if !fin_nette {
            continue;
        }
        if meilleur.is_none_or(|m| m.len() < debut.len()) {
            meilleur = Some(debut);
        }
    }
    meilleur
}

/// Retire la ponctuation qu'une phrase colle à la fin d'une adresse.
///
/// « regarde https://exemple.fr/page. » : le point appartient à la phrase, pas
/// à l'adresse. Les parenthèses se comptent, en revanche — une adresse
/// Wikipédia en contient légitimement.
fn rogner_ponctuation(lien: &str) -> &str {
    let mut fin = lien.len();
    while fin > 0 {
        let dernier = lien[..fin].chars().next_back().unwrap();
        let a_rogner = matches!(dernier, '.' | ',' | ';' | ':' | '!' | '?' | '»' | '"' | '\'')
            || (dernier == ')' && lien[..fin].matches('(').count() < lien[..fin].matches(')').count());
        if !a_rogner {
            break;
        }
        fin -= dernier.len_utf8();
    }
    &lien[..fin]
}

/// Ce message me mentionne-t-il ?
///
/// Sert à décider d'un son et d'un clignotement dans la barre des tâches : on
/// ne prévient que si l'on est nommé, jamais sur une mention adressée à
/// quelqu'un d'autre.
pub fn me_mentionne(texte: &str, membres: &[&str], moi: &str) -> bool {
    decouper(texte, membres, Some(moi)).iter().any(|bloc| match bloc {
        Bloc::Code(_) => false,
        Bloc::Ligne(frags) => frags
            .iter()
            .any(|f| matches!(f, Fragment::Mention { moi: true, .. })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lignes<'a>(texte: &'a str, membres: &[&str]) -> Vec<Vec<Fragment<'a>>> {
        decouper(texte, membres, Some("moi"))
            .into_iter()
            .filter_map(|b| match b {
                Bloc::Ligne(f) => Some(f),
                Bloc::Code(_) => None,
            })
            .collect()
    }

    #[test]
    fn le_texte_ordinaire_reste_entier() {
        let l = lignes("bonjour tout le monde", &[]);
        assert_eq!(l, vec![vec![Fragment::Texte("bonjour tout le monde")]]);
    }

    #[test]
    fn gras_italique_et_code() {
        let l = lignes("un **gras** et *penché* et `du code`", &[]);
        assert_eq!(
            l[0],
            vec![
                Fragment::Texte("un "),
                Fragment::Gras("gras"),
                Fragment::Texte(" et "),
                Fragment::Italique("penché"),
                Fragment::Texte(" et "),
                Fragment::Code("du code"),
            ]
        );
    }

    /// Le piège qui transformerait la moitié des messages en charabia penché.
    #[test]
    fn une_etoile_isolee_reste_une_etoile() {
        for brut in ["3 * 4 = 12", "un * seul", "**", "* ", "a ** b"] {
            let l = lignes(brut, &[]);
            let plat: String = l[0]
                .iter()
                .map(|f| match f {
                    Fragment::Texte(t) => *t,
                    _ => "<BALISE>",
                })
                .collect();
            assert_eq!(plat, brut, "« {brut} » ne doit pas être interprété");
        }
    }

    /// Ce qui est entre accents graves n'est réinterprété par personne :
    /// c'est tout l'intérêt d'écrire du code.
    #[test]
    fn le_code_litteral_protege_son_contenu() {
        let l = lignes("écris `**ceci**` pour du gras", &[]);
        assert_eq!(l[0][1], Fragment::Code("**ceci**"));
    }

    #[test]
    fn les_liens_sortent_de_la_ponctuation() {
        let l = lignes("va sur https://exemple.fr/page. Merci", &[]);
        assert_eq!(l[0][1], Fragment::Lien("https://exemple.fr/page"));
        assert_eq!(l[0][2], Fragment::Texte(". Merci"));
    }

    #[test]
    fn une_mention_ne_vaut_que_pour_un_pseudo_connu() {
        let membres = ["marie", "moi"];
        let l = lignes("salut @marie et @inconnu", &membres);
        assert_eq!(l[0][1], Fragment::Mention { pseudo: "marie", moi: false });
        assert_eq!(l[0][2], Fragment::Texte(" et @inconnu"));
    }

    /// Sans cette règle, `@marie` surlignerait la mauvaise personne dès qu'un
    /// compte `marie-claire` existe.
    #[test]
    fn la_mention_la_plus_longue_gagne() {
        let membres = ["marie", "marie-claire"];
        let l = lignes("coucou @marie-claire", &membres);
        assert_eq!(l[0][1], Fragment::Mention { pseudo: "marie-claire", moi: false });
    }

    /// Et elle ne s'accroche pas au milieu d'un mot.
    #[test]
    fn une_mention_ne_deborde_pas_sur_le_mot_suivant() {
        let l = lignes("bonjour @mariette", &["marie"]);
        assert_eq!(l[0], vec![Fragment::Texte("bonjour @mariette")]);
    }

    #[test]
    fn ma_propre_mention_se_distingue() {
        let membres = ["marie", "moi"];
        assert!(me_mentionne("hé @moi regarde", &membres, "moi"));
        assert!(me_mentionne("hé @MOI regarde", &membres, "moi"));
        assert!(!me_mentionne("hé @marie regarde", &membres, "moi"));
        // Une mention dans un bloc de code ne réveille personne : c'est du
        // texte cité, pas une interpellation.
        assert!(!me_mentionne("```\n@moi\n```", &membres, "moi"));
    }

    #[test]
    fn les_blocs_de_code_gardent_leurs_lignes() {
        let blocs = decouper("avant\n```\nligne 1\nligne 2\n```\naprès", &[], None);
        assert_eq!(blocs.len(), 3);
        assert_eq!(blocs[0], Bloc::Ligne(vec![Fragment::Texte("avant")]));
        assert_eq!(blocs[1], Bloc::Code("ligne 1\nligne 2"));
        assert_eq!(blocs[2], Bloc::Ligne(vec![Fragment::Texte("après")]));
    }

    /// Pendant qu'on tape, la clôture n'est pas encore refermée : le bloc doit
    /// exister quand même, sinon il apparaît et disparaît à chaque caractère.
    #[test]
    fn un_bloc_jamais_referme_prend_ce_qui_suit() {
        let blocs = decouper("regarde ```du code", &[], None);
        assert_eq!(blocs.last(), Some(&Bloc::Code("du code")));
    }

    #[test]
    fn le_multiligne_donne_une_ligne_par_ligne() {
        let blocs = decouper("un\ndeux\r\ntrois", &[], None);
        assert_eq!(blocs.len(), 3);
        assert_eq!(blocs[1], Bloc::Ligne(vec![Fragment::Texte("deux")]));
    }

    /// Le texte vient de quelqu'un d'autre : accents, émojis, et tout ce qui
    /// fait plus d'un octet. On ne doit jamais couper au milieu.
    #[test]
    fn les_caracteres_multioctets_ne_sont_jamais_coupes() {
        for brut in [
            "élève à l'école",
            "🎧 casque 🎤 micro",
            "**émoji 🚀 gras**",
            "`café` et `thé`",
            "@élodie salut",
        ] {
            let blocs = decouper(brut, &["élodie"], None);
            // La reconstruction rend l'original : rien n'est perdu ni
            // dupliqué, donc aucune frontière n'a été franchie de travers.
            let plat: String = blocs
                .iter()
                .map(|b| match b {
                    Bloc::Code(c) => (*c).to_string(),
                    Bloc::Ligne(f) => f
                        .iter()
                        .map(|f| match f {
                            Fragment::Texte(t) | Fragment::Lien(t) => (*t).to_string(),
                            Fragment::Code(t) => format!("`{t}`"),
                            Fragment::Gras(t) => format!("**{t}**"),
                            Fragment::Italique(t) => format!("*{t}*"),
                            Fragment::Mention { pseudo, .. } => format!("@{pseudo}"),
                        })
                        .collect(),
                })
                .collect();
            assert_eq!(plat, brut, "« {brut} » n'est pas rendu à l'identique");
        }
    }
}
