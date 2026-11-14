//! Découpage d'un message en fragments à peindre : liens, mentions, gras,
//! italique, barré, souligné, spoiler, code — et, en tête de ligne, les
//! citations, les listes et les titres.
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
//! Pas de Markdown complet. Ni tableaux, ni images, ni listes imbriquées : ce
//! sont des messages de chat, pas des documents. Les marques sont celles
//! qu'on tape sans y penser parce qu'elles viennent de Discord — `**gras**`,
//! `*italique*`, `~~barré~~`, `__souligné__`, `||spoiler||`, `` `code` `` et
//! les blocs triples, `[texte](adresse)` — plus les liens et les mentions. En
//! tête de ligne : `> ` cite, `- ` et `1. ` font une liste, `# ` un titre.
//!
//! Une marque ne s'imbrique pas dans une autre : ce qu'elle entoure est rendu
//! tel quel. Une barre oblique inverse devant une marque la rend au texte
//! (`\*pas penché\*`) ; devant autre chose, elle reste une barre oblique
//! inverse — un chemin Windows et « ¯\_(ツ)_/¯ » passent entiers.
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
    Barre(&'a str),
    Souligne(&'a str),
    /// Caché tant que le lecteur n'a pas choisi de le voir.
    Spoiler(&'a str),
    Code(&'a str),
    /// `[texte](adresse)` : un lien montré sous un autre nom. Jamais sous
    /// celui d'une autre adresse — voir [`lien_nomme`].
    LienNomme { texte: &'a str, url: &'a str },
}

/// Un bloc de message. Les blocs de code sortent du fil de la phrase : ils
/// occupent leur propre espace, sans retour à la ligne automatique.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bloc<'a> {
    Ligne(Vec<Fragment<'a>>),
    Code(&'a str),
    /// `> ` en tête de ligne.
    Citation(Vec<Fragment<'a>>),
    /// `- ` ou `* ` en tête de ligne.
    Puce(Vec<Fragment<'a>>),
    /// `1. ` en tête de ligne. `numero` est ce que l'auteur a écrit, sans
    /// le point : on ne renumérote pas à sa place.
    Numero { numero: &'a str, ligne: Vec<Fragment<'a>> },
    /// `# `, `## ` ou `### ` en tête de ligne : `niveau` vaut 1, 2 ou 3.
    Titre { niveau: u8, ligne: Vec<Fragment<'a>> },
}

impl<'a> Bloc<'a> {
    /// Les fragments du bloc ; aucun pour un bloc de code, qui est du texte
    /// cité et rien d'autre.
    pub fn fragments(&self) -> &[Fragment<'a>] {
        match self {
            Bloc::Code(_) => &[],
            Bloc::Ligne(f) | Bloc::Citation(f) | Bloc::Puce(f) => f,
            Bloc::Numero { ligne, .. } | Bloc::Titre { ligne, .. } => ligne,
        }
    }
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
        blocs.push(bloc_de(ligne, membres, moi));
    }
}

/// Le bloc d'une ligne : ce que dit sa tête, sinon une ligne ordinaire.
///
/// La marque est suivie d'une espace, toujours : « #général », « >:( » et
/// « -5 » restent ce qu'ils sont. Et il faut quelque chose derrière — un
/// tiret seul n'est pas une liste.
fn bloc_de<'a>(ligne: &'a str, membres: &[&str], moi: Option<&str>) -> Bloc<'a> {
    let lire = |reste: &'a str| fragments(reste, membres, moi);
    if ligne == ">" {
        return Bloc::Citation(lire(""));
    }
    if let Some(reste) = ligne.strip_prefix("> ") {
        return Bloc::Citation(lire(reste));
    }
    let plein = |reste: &&str| !reste.trim().is_empty();
    if let Some(reste) = ligne.strip_prefix("- ").or_else(|| ligne.strip_prefix("* ")).filter(plein) {
        return Bloc::Puce(lire(reste));
    }
    let dieses = ligne.bytes().take_while(|o| *o == b'#').count();
    if (1..=3).contains(&dieses) {
        if let Some(reste) = ligne[dieses..].strip_prefix(' ').filter(plein) {
            return Bloc::Titre { niveau: dieses as u8, ligne: lire(reste) };
        }
    }
    let chiffres = ligne.bytes().take_while(u8::is_ascii_digit).count();
    if (1..=3).contains(&chiffres) {
        if let Some(reste) = ligne[chiffres..].strip_prefix(". ").filter(plein) {
            return Bloc::Numero { numero: &ligne[..chiffres], ligne: lire(reste) };
        }
    }
    Bloc::Ligne(lire(ligne))
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

        // 0. La barre oblique inverse rend une marque au texte. Seulement
        //    devant une marque : partout ailleurs elle reste elle-même, et
        //    « C:\Users », « \\serveur » ou « ¯\_(ツ)_/¯ » passent entiers.
        if octets[i] == b'\\' {
            if let Some(signes) = marque_echappee(&reste[1..], i == 0) {
                pousser_texte(&mut out, &ligne[debut..i]);
                debut = i + 1;
                i += 1 + signes;
                continue;
            }
        }

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
        if octets[i] == b'[' {
            if let Some((texte, url, longueur)) = lien_nomme(reste) {
                pousser_texte(&mut out, &ligne[debut..i]);
                out.push(Fragment::LienNomme { texte, url });
                i += longueur;
                debut = i;
                continue;
            }
        }

        // 3. Les marques qui entourent — le double avant le simple, sans quoi
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
            ("__", Fragment::Souligne as fn(&'a str) -> Fragment<'a>),
            ("~~", Fragment::Barre as fn(&'a str) -> Fragment<'a>),
            ("||", Fragment::Spoiler as fn(&'a str) -> Fragment<'a>),
            ("*", Fragment::Italique as fn(&'a str) -> Fragment<'a>),
        ] {
            if let Some(contenu) = entoure(reste, marque) {
                // Le souligné ne se prend pas au milieu d'un mot : les
                // noms à tirets bas (`ma__variable__x`) restent entiers.
                let fin = i + marque.len() * 2 + contenu.len();
                if marque == "__" && (colle_a_un_mot(ligne[..i].chars().next_back()) || colle_a_un_mot(ligne[fin..].chars().next())) {
                    continue;
                }
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

fn colle_a_un_mot(voisin: Option<char>) -> bool {
    voisin.is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// Ce qui suit une barre oblique inverse est-il une marque ? Rend alors le
/// nombre d'octets à laisser au texte : toute la suite du même signe, pour
/// que `\**` ne laisse pas une étoile ouvrir un italique.
///
/// `en_tete` : au début d'une ligne, les marques de bloc s'échappent aussi.
fn marque_echappee(suite: &str, en_tete: bool) -> Option<usize> {
    let de_bloc = en_tete && (suite.starts_with('>') || suite.starts_with('#') || suite.starts_with("- "));
    let en_ligne = ["*", "`", "[", "~~", "__", "||"].iter().any(|m| suite.starts_with(m));
    if !(de_bloc || en_ligne) {
        return None;
    }
    let signe = suite.as_bytes()[0];
    Some(suite.bytes().take_while(|o| *o == signe).count())
}

/// `[texte](adresse)` en tête de `reste` : le texte, l'adresse, et la
/// longueur du tout en octets.
///
/// Un lien nommé peut mentir sur sa destination. Deux garde-fous : l'adresse
/// commence par `http://` ou `https://`, et le texte ne ressemble pas
/// lui-même à une adresse — « [banque.fr](https://ailleurs.example) » reste
/// du texte, où l'adresse vraie se lit. Le client montre de toute façon
/// l'adresse au survol.
fn lien_nomme(reste: &str) -> Option<(&str, &str, usize)> {
    let apres = reste.strip_prefix('[')?;
    let fin_texte = apres.find("](")?;
    let texte = &apres[..fin_texte];
    if texte.trim().is_empty() || texte.contains('[') || ressemble_a_une_adresse(texte) {
        return None;
    }
    let suite = &apres[fin_texte + 2..];
    let schema = ["http://", "https://"].into_iter().find(|s| suite.starts_with(s))?;
    // L'adresse va jusqu'à la parenthèse qui ferme le lien ; celles qu'elle
    // ouvre elle-même se comptent — une adresse Wikipédia en contient.
    let mut ouvertes = 0usize;
    let mut fin = None;
    for (o, c) in suite.char_indices() {
        match c {
            c if c.is_whitespace() => return None,
            '(' => ouvertes += 1,
            ')' if ouvertes == 0 => {
                fin = Some(o);
                break;
            }
            ')' => ouvertes -= 1,
            _ => {}
        }
    }
    let fin = fin?;
    if fin <= schema.len() {
        return None;
    }
    Some((texte, &suite[..fin], 1 + fin_texte + 2 + fin + 1))
}

/// Le texte d'un lien nommé se donne-t-il pour une adresse ? Un schéma, un
/// « www. », ou un mot qui finit comme un nom de domaine (« banque.fr »).
fn ressemble_a_une_adresse(texte: &str) -> bool {
    let bas = texte.to_lowercase();
    if bas.contains("://") || bas.contains("www.") {
        return true;
    }
    bas.split_whitespace().any(|mot| {
        let mot = mot.trim_end_matches(|c: char| !c.is_alphanumeric());
        mot.rsplit_once('.').is_some_and(|(avant, extension)| {
            !avant.is_empty() && extension.len() >= 2 && extension.chars().all(|c| c.is_ascii_alphabetic())
        })
    })
}

/// Le pseudo mentionné à cet endroit, s'il en est un connu.
///
/// Le **plus long** d'abord : sans ça, `@marie-claire` s'arrêterait à `@marie`
/// si les deux comptes existent, et surlignerait la mauvaise personne.
fn mention<'a>(apres_arobase: &'a str, membres: &[&str]) -> Option<&'a str> {
    let mut meilleur: Option<&'a str> = None;
    for membre in membres {
        if membre.is_empty() {
            continue;
        }
        // `split_at_checked` et non `[..n]` : la longueur du pseudo, en
        // octets, peut tomber au milieu d'un caractère du message. Face à
        // « kevin » (5 octets), « @bob ça va » se coupait dans le « ç », et
        // la panique — `panic = "abort"` — fermait ki-chat chez tous ceux
        // qui recevaient le message. Trop court ou coupé : pas ce pseudo.
        let Some((debut, suite)) = apres_arobase.split_at_checked(membre.len()) else {
            continue;
        };
        if !debut.eq_ignore_ascii_case(membre) {
            continue;
        }
        // La mention doit se terminer sur une frontière : `@marie` ne
        // s'accroche pas au milieu de `@mariette`.
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
    decouper(texte, membres, Some(moi))
        .iter()
        .any(|bloc| bloc.fragments().iter().any(|f| matches!(f, Fragment::Mention { moi: true, .. })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lignes<'a>(texte: &'a str, membres: &[&str]) -> Vec<Vec<Fragment<'a>>> {
        decouper(texte, membres, Some("moi"))
            .into_iter()
            .filter_map(|b| match b {
                Bloc::Ligne(f) => Some(f),
                _ => None,
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

    /// Remet bout à bout ce que `decouper` a découpé, marques comprises.
    fn recomposer(blocs: &[Bloc<'_>]) -> String {
        let tete = |b: &Bloc<'_>| match b {
            Bloc::Ligne(_) | Bloc::Code(_) => String::new(),
            Bloc::Citation(_) => "> ".into(),
            Bloc::Puce(_) => "- ".into(),
            Bloc::Numero { numero, .. } => format!("{numero}. "),
            Bloc::Titre { niveau, .. } => format!("{} ", "#".repeat(*niveau as usize)),
        };
        blocs
            .iter()
            .map(|b| match b {
                Bloc::Code(c) => (*c).to_string(),
                autre => {
                    let corps: String = autre
                        .fragments()
                        .iter()
                        .map(|f| match f {
                            Fragment::Texte(t) | Fragment::Lien(t) => (*t).to_string(),
                            Fragment::Code(t) => format!("`{t}`"),
                            Fragment::Gras(t) => format!("**{t}**"),
                            Fragment::Italique(t) => format!("*{t}*"),
                            Fragment::Barre(t) => format!("~~{t}~~"),
                            Fragment::Souligne(t) => format!("__{t}__"),
                            Fragment::Spoiler(t) => format!("||{t}||"),
                            Fragment::LienNomme { texte, url } => format!("[{texte}]({url})"),
                            Fragment::Mention { pseudo, .. } => format!("@{pseudo}"),
                        })
                        .collect();
                    tete(autre) + &corps
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn le_barre_le_souligne_et_le_spoiler() {
        let l = lignes("~~faux~~, __important__ et ||la fin||", &[]);
        assert_eq!(
            l[0],
            vec![
                Fragment::Barre("faux"),
                Fragment::Texte(", "),
                Fragment::Souligne("important"),
                Fragment::Texte(" et "),
                Fragment::Spoiler("la fin"),
            ]
        );
        // Une marque ne s'imbrique pas : ce qu'elle entoure est rendu tel quel.
        assert_eq!(lignes("||**gras** caché||", &[])[0], vec![Fragment::Spoiler("**gras** caché")]);
    }

    /// Du code, des comparaisons, des noms à tirets bas : rien de tout cela
    /// n'est de la mise en forme.
    #[test]
    fn ce_qui_ressemble_a_une_marque_sans_en_etre_une() {
        for brut in [
            "a || b || c",
            "if (x || y) { z }",
            "ma__variable__x et x__y__",
            "~ environ ~~ 3",
            "2 ~~ 3 ~~ 4",
            "un _seul_ tiret bas",
        ] {
            assert_eq!(lignes(brut, &[])[0], vec![Fragment::Texte(brut)], "« {brut} »");
        }
        // Le mot entier entre doubles tirets bas, lui, est souligné — comme
        // sur Discord, `__init__` compris.
        assert_eq!(lignes("__init__", &[])[0], vec![Fragment::Souligne("init")]);
    }

    fn textes(texte: &str) -> String {
        lignes(texte, &[])[0]
            .iter()
            .map(|f| match f {
                Fragment::Texte(t) => *t,
                autre => panic!("{autre:?} n'est pas du texte"),
            })
            .collect()
    }

    #[test]
    fn la_barre_oblique_inverse_rend_une_marque_au_texte() {
        assert_eq!(textes("\\*pas penché\\*"), "*pas penché*");
        assert_eq!(textes("\\**pas gras**"), "**pas gras**");
        assert_eq!(textes("\\~~pas barré~~ \\||visible|| \\__plat__"), "~~pas barré~~ ||visible|| __plat__");
        assert_eq!(textes("\\`pas du code`"), "`pas du code`");
        let l = lignes("\\[texte](https://exemple.fr)", &[]);
        assert_eq!(l[0][0], Fragment::Texte("[texte]("));
        assert_eq!(l[0][1], Fragment::Lien("https://exemple.fr"));
        // Une seule des deux échappée : l'autre n'a plus rien à fermer.
        assert_eq!(textes("\\*presque*"), "*presque*");
    }

    /// Devant autre chose qu'une marque, la barre oblique inverse est une
    /// barre oblique inverse.
    #[test]
    fn la_barre_oblique_inverse_reste_ailleurs() {
        for brut in ["C:\\Users\\drion\\_cache", "\\\\serveur\\partage", "¯\\_(ツ)_/¯", "une \\ seule", "fin \\"] {
            assert_eq!(lignes(brut, &[])[0], vec![Fragment::Texte(brut)], "« {brut} »");
        }
    }

    #[test]
    fn un_lien_nomme() {
        let l = lignes("lis [la doc](https://exemple.fr/doc) avant", &[]);
        assert_eq!(l[0][1], Fragment::LienNomme { texte: "la doc", url: "https://exemple.fr/doc" });
        assert_eq!(l[0][2], Fragment::Texte(" avant"));
        // Les parenthèses de l'adresse se comptent.
        let l = lignes("[Rust](https://fr.wikipedia.org/wiki/Rust_(langage)).", &[]);
        assert_eq!(
            l[0],
            vec![
                Fragment::LienNomme { texte: "Rust", url: "https://fr.wikipedia.org/wiki/Rust_(langage)" },
                Fragment::Texte("."),
            ]
        );
        // Un numéro de version n'est pas une adresse.
        assert!(matches!(lignes("[v0.2.0](https://exemple.fr/v)", &[])[0][0], Fragment::LienNomme { .. }));
    }

    /// Un lien nommé ne se fait pas passer pour une autre adresse, et ne
    /// mène que sur le web.
    #[test]
    fn un_lien_nomme_ne_ment_pas() {
        for brut in [
            "[banque.fr](https://ailleurs.example)",
            "[va sur www.banque.fr](https://ailleurs.example)",
            "[clique sur Banque.FR !](https://ailleurs.example)",
        ] {
            let l = lignes(brut, &[]);
            assert!(!l[0].iter().any(|f| matches!(f, Fragment::LienNomme { .. })), "« {brut} »");
            // L'adresse vraie reste un lien, à lire telle quelle.
            assert!(l[0].contains(&Fragment::Lien("https://ailleurs.example")), "« {brut} »");
        }
        for brut in ["[https://banque.fr](https://ailleurs.example)", "[ici](javascript:alert(1))", "[ici](file:///C:/Windows)", "[ici](https://)", "[](https://exemple.fr)", "[ici](https://exemple.fr sans fin"] {
            let l = lignes(brut, &[]);
            assert!(!l[0].iter().any(|f| matches!(f, Fragment::LienNomme { .. })), "« {brut} »");
        }
    }

    #[test]
    fn les_tetes_de_ligne() {
        let blocs = decouper("# Titre\n### Petit **titre**\n> cité\n>\n- un\n* deux\n1. premier\n12. douzième\nfin", &[], None);
        assert_eq!(
            blocs,
            vec![
                Bloc::Titre { niveau: 1, ligne: vec![Fragment::Texte("Titre")] },
                Bloc::Titre { niveau: 3, ligne: vec![Fragment::Texte("Petit "), Fragment::Gras("titre")] },
                Bloc::Citation(vec![Fragment::Texte("cité")]),
                Bloc::Citation(vec![Fragment::Texte("")]),
                Bloc::Puce(vec![Fragment::Texte("un")]),
                Bloc::Puce(vec![Fragment::Texte("deux")]),
                Bloc::Numero { numero: "1", ligne: vec![Fragment::Texte("premier")] },
                Bloc::Numero { numero: "12", ligne: vec![Fragment::Texte("douzième")] },
                Bloc::Ligne(vec![Fragment::Texte("fin")]),
            ]
        );
    }

    /// Sans espace derrière la marque, ou sans rien derrière, ce n'est pas
    /// une tête de ligne ; ni au milieu d'une ligne, ni échappée.
    #[test]
    fn ce_qui_ressemble_a_une_tete_de_ligne_sans_en_etre_une() {
        for brut in [
            "#général", ">:(", "-5 degrés", "- ", "#", "# ", "#### trop", "1.", "1.5 point", "2026. Une année", "au milieu > ici",
            "au milieu - ici", " - décalé", "*penché* en tête",
        ] {
            let blocs = decouper(brut, &[], None);
            assert!(matches!(blocs.as_slice(), [Bloc::Ligne(_)]), "« {brut} » : {blocs:?}");
        }
        for (brut, attendu) in [("\\# pas un titre", "# pas un titre"), ("\\> pas cité", "> pas cité"), ("\\- pas une puce", "- pas une puce")] {
            assert!(matches!(decouper(brut, &[], None).as_slice(), [Bloc::Ligne(_)]), "« {brut} »");
            assert_eq!(textes(brut), attendu);
        }
    }

    #[test]
    fn une_mention_reveille_ou_qu_elle_soit() {
        let membres = ["moi"];
        for brut in ["> @moi tu avais dit ça", "- @moi", "# @moi", "3. @moi", "~~rien~~ @moi"] {
            assert!(me_mentionne(brut, &membres, "moi"), "« {brut} »");
        }
        // Dans une marque, c'est du texte rendu tel quel.
        assert!(!me_mentionne("||@moi||", &membres, "moi"));
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
            "~~café~~ ||🚀|| __é__ [été](https://exemple.fr/é)",
            "> élève\n- 🎧 casque\n# Été\n1. ça",
            "\\é \\🎧 ¯\\_(ツ)_/¯",
            "[é](é) [🎧 ~~ || __",
        ] {
            let blocs = decouper(brut, &["élodie"], None);
            // La reconstruction rend l'original : rien n'est perdu ni
            // dupliqué, donc aucune frontière n'a été franchie de travers.
            assert_eq!(recomposer(&blocs), brut, "« {brut} » n'est pas rendu à l'identique");
        }
    }

    /// La panique de la 0.1.51 : chaque pseudo du salon était essayé en
    /// coupant le texte à sa longueur en octets, et « kevin » coupait
    /// « @bob ça va » dans le « ç ». ki-chat se fermait chez tous ceux qui
    /// recevaient le message — `me_mentionne` tourne dès la réception.
    #[test]
    fn un_pseudo_ne_coupe_jamais_le_message_dans_un_caractere() {
        let membres = ["bob", "kevin", "Zoé"];
        assert_eq!(
            lignes("@bob ça va", &membres)[0],
            vec![Fragment::Mention { pseudo: "bob", moi: false }, Fragment::Texte(" ça va")]
        );
        assert_eq!(
            lignes("salut @Zoé", &membres)[0][1],
            Fragment::Mention { pseudo: "Zoé", moi: false }
        );
        assert!(me_mentionne("@bob ça va", &membres, "bob"));
        assert!(!me_mentionne("@bob ça va", &membres, "kevin"));
        assert!(me_mentionne("salut @Zoé", &membres, "Zoé"));
    }

    /// Des pseudos de toutes les longueurs, de 1 à 12 octets : derrière chaque
    /// arobase, l'une d'elles tombe forcément au milieu d'un caractère.
    #[test]
    fn aucune_longueur_de_pseudo_ne_coupe_un_caractere() {
        let pseudos: Vec<String> = (1..=12).map(|n| "k".repeat(n)).collect();
        let mut membres: Vec<&str> = pseudos.iter().map(String::as_str).collect();
        membres.extend(["élodie", "Zoé"]);
        for brut in [
            "@élodie, ça va ?",
            "salut @Zoé",
            "jean@société.fr",
            "@🎧🎤 micro coupé",
            "@ é @é @",
        ] {
            let blocs = decouper(brut, &membres, Some("moi"));
            assert_eq!(recomposer(&blocs), brut, "« {brut} » n'est pas rendu à l'identique");
            assert!(!me_mentionne(brut, &membres, "moi"));
        }
    }
}
