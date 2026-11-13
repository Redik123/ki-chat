//! Les médailles de l'acte, membre par membre — sous le classement, sur
//! l'accueil et dans son onglet. Lues chez HenrikDev par le serveur, ou par
//! le client de chacun.
//!
//! Un `ui::Tableau`, comme le classement : le joueur, le MVP et le total
//! restent quoi qu'il arrive ; les autres médailles se masquent par
//! priorité quand la place manque, les dernières de la liste d'abord.
//! Jamais de défilement de côté : ce qui est masqué se retrouve dans la vue
//! Médailles du profil, à un clic de la ligne. Les titres sont les noms
//! courts des pastilles de match ; le nom entier est au survol.

use ki_ui::jetons::police;
use ki_ui::tableau::{Colonne, Tableau, Tri};

use super::*;

/// Ce que montre une colonne.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    Joueur,
    Une(Medaille),
    Toutes,
    Lues,
}

/// Les mêmes que le classement, juste au-dessus : les avatars s'alignent.
const HAUTEUR_LIGNE: f32 = 34.0;
const AVATAR: f32 = 22.0;

/// La largeur d'une colonne de chiffres : son titre en capitales, et la
/// flèche du tri à côté.
fn largeur_du_titre(titre: &str) -> f32 {
    (titre.chars().count() as f32 * 6.5 + 12.0).max(36.0)
}

/// Les colonnes, dans l'ordre : le joueur, les douze médailles — le MVP
/// d'abord, qui ne se masque jamais —, le total, et quand elles ont été
/// lues, qui part la première : le survol du pseudo le dit aussi.
fn colonnes() -> Vec<(Col, Colonne<'static>)> {
    let mut colonnes = vec![(Col::Joueur, Colonne::new("Joueur", 110.0).grandit().triable())];
    for m in Medaille::TOUTES {
        let titre = nom_court(m);
        let colonne = Colonne::new(titre, largeur_du_titre(titre)).droite().triable().priorite(m.ordre() as u8).info(m.nom());
        colonnes.push((Col::Une(m), colonne));
    }
    let toutes = Colonne::new("Toutes", largeur_du_titre("Toutes")).droite().triable().info("toutes ses médailles de l'acte");
    colonnes.push((Col::Toutes, toutes));
    let lues = Colonne::new("Lues", 64.0).droite().priorite(Medaille::TOUTES.len() as u8).info("quand ses médailles ont été lues");
    colonnes.push((Col::Lues, lues));
    colonnes
}

/// Le tableau, trié par les MVP tant qu'on n'a cliqué aucun en-tête.
fn tableau() -> Tableau<'static, Col> {
    colonnes()
        .into_iter()
        .fold(Tableau::new("valo_medailles"), |t, (col, colonne)| t.colonne(col, colonne))
        .hauteur_ligne(HAUTEUR_LIGNE)
        .tri_par_defaut(Col::Une(Medaille::Mvp), false)
}

/// Un membre et ses médailles, comptées pour l'acte du groupe.
struct Compte<'a> {
    f: &'a FicheMembre,
    md: &'a Medailles,
    /// Lues pendant l'acte du groupe : une lecture d'un autre acte ne dit
    /// rien de celui-ci, et compte pour zéro.
    a_jour: bool,
}

impl Compte<'_> {
    fn fois(&self, medaille: Medaille) -> u32 {
        if self.a_jour {
            self.md.fois(medaille, false)
        } else {
            0
        }
    }

    fn total(&self) -> u32 {
        if self.a_jour {
            self.md.cet_acte.iter().map(|c| c.fois).sum()
        } else {
            0
        }
    }

    /// Ce que dit le survol du pseudo : de quand date la lecture, et si
    /// elle est d'un autre acte.
    fn lecture(&self) -> String {
        let quand = crate::formats::il_y_a(self.md.maj);
        if self.a_jour {
            format!("médailles lues {quand}")
        } else {
            format!("médailles lues {quand}, pendant « {} » : pas comptées dans cet acte", self.md.acte)
        }
    }
}

/// L'acte dont parlent la plupart des membres (« V26 · ACTE V ») : celui
/// d'une lecture d'avant un changement d'acte ne compte plus.
fn acte_du_groupe<'a>(medailles: impl Iterator<Item = &'a Medailles>) -> String {
    let mut comptes: BTreeMap<&str, usize> = BTreeMap::new();
    for m in medailles.filter(|m| !m.acte.is_empty()) {
        *comptes.entry(m.acte.as_str()).or_default() += 1;
    }
    comptes.into_iter().max_by_key(|(acte, n)| (*n, *acte)).map(|(acte, _)| acte.to_string()).unwrap_or_default()
}

/// L'acte du groupe, et ceux dont on connaît des médailles.
fn comptes<'a>(fiches: impl Iterator<Item = &'a FicheMembre>) -> (String, Vec<Compte<'a>>) {
    let avec: Vec<(&FicheMembre, &Medailles)> = fiches
        .filter_map(|f| f.fiche.medailles.as_ref().map(|m| (f, m)))
        .filter(|(_, m)| !m.cet_acte.is_empty() || !m.carriere.is_empty())
        .collect();
    let acte = acte_du_groupe(avec.iter().map(|(_, m)| *m));
    let comptes = avec
        .into_iter()
        .map(|(f, md)| Compte { f, md, a_jour: acte.is_empty() || md.acte.is_empty() || md.acte == acte })
        .collect();
    (acte, comptes)
}

/// Les lignes dans l'ordre du tri : par la colonne choisie ; à égalité — et
/// tant qu'on n'a rien choisi — les MVP, puis le total, puis le pseudo.
fn ordonner<'c, 'a>(comptes: &'c [Compte<'a>], tri: Option<Tri<Col>>) -> Vec<&'c Compte<'a>> {
    let mut ordre: Vec<&Compte> = comptes.iter().collect();
    ordre.sort_by_key(|c| (std::cmp::Reverse(c.fois(Medaille::Mvp)), std::cmp::Reverse(c.total()), c.f.username.to_lowercase()));
    let Some(tri) = tri else { return ordre };
    // Des tris stables : l'ordre ci-dessus départage.
    let mut par = |cle: &dyn Fn(&Compte) -> u32| {
        ordre.sort_by(|a, b| if tri.croissant { cle(a).cmp(&cle(b)) } else { cle(b).cmp(&cle(a)) });
    };
    match tri.colonne {
        Col::Une(m) => par(&|c| c.fois(m)),
        Col::Toutes => par(&|c| c.total()),
        Col::Joueur => {
            ordre.sort_by_key(|c| c.f.username.to_lowercase());
            if !tri.croissant {
                ordre.reverse();
            }
        }
        Col::Lues => {}
    }
    ordre
}

/// Les médailles de l'acte, membre par membre, le MVP d'abord. Un clic sur
/// un en-tête trie, sur une ligne ouvre la fiche. Une ligne d'aide tant
/// qu'on n'en connaît aucune.
pub(super) fn du_groupe(ui: &mut Ui, lignes: &[Ligne], noms: &Annuaire, cx: &Contexte<'_>, demandes: &mut Vec<Demande>) {
    let (acte, comptes) = comptes(lignes.iter().map(|l| l.f));
    ui.add_space(espace::XL);
    let titre = if acte.is_empty() { "Médailles de l'acte".to_string() } else { format!("Médailles — {acte}") };
    ui.label(Texte::Corps.gras(ui, titre));
    if comptes.is_empty() {
        ui::hint(
            ui,
            "aucune médaille connue pour l'instant : le serveur les lit chez HenrikDev à chaque \
             rafraîchissement des fiches, pour qui garde « Ajouter mes médailles à ma fiche » (⚙ → Jeu)",
        );
        return;
    }
    ui.add_space(espace::XS);
    let tableau = tableau();
    let ordre = ordonner(&comptes, tableau.tri(ui));
    let clic = tableau.show(ui, ordre.len(), |ui, i, colonne| {
        let c = ordre[i];
        let f = c.f;
        match colonne {
            Col::Joueur => {
                ui.spacing_mut().item_spacing.x = espace::S;
                classement::avatar(ui, &f.username, cx.photos.get(&f.user_id).map(|(_, t)| t), AVATAR);
                // Sa propre ligne se repère : son pseudo pèse.
                let police = if cx.moi == Some(f.user_id) {
                    police::moyenne(ui.ctx(), texte::COURANT)
                } else {
                    FontId::proportional(texte::COURANT)
                };
                ui.label(RichText::new(&f.username).font(police).color(noms.couleur(f.user_id))).on_hover_text(c.lecture());
            }
            Col::Une(m) => {
                let n = c.fois(m);
                if n == 0 {
                    cellule(ui, TIRET, TEXT_FAINT);
                    return;
                }
                let couleur = if matches!(m, Medaille::Mvp | Medaille::Distinction) { theme::WARN } else { TEXT };
                let reponse = ui.label(Texte::Aide.gras(ui, n.to_string()).color(couleur));
                if let Some(compte) = c.md.cet_acte.iter().find(|x| x.medaille == m) {
                    reponse.on_hover_text(format!("{} ×{n} · record de l'acte : {}", m.nom(), m.valeur(compte.meilleur)));
                }
            }
            Col::Toutes => {
                cellule(ui, c.total().to_string(), TEXT_DIM);
            }
            Col::Lues => {
                cellule(ui, crate::formats::il_y_a(c.md.maj), TEXT_FAINT);
            }
        }
    });
    if let Some(i) = clic {
        let f = ordre[i].f;
        demandes.push(Demande::OuvrirFiche(f.user_id, f.username.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ki_protocol::CompteMedaille;

    fn montrees(largeur: f32) -> Vec<Col> {
        let (cols, colonnes): (Vec<Col>, Vec<Colonne>) = colonnes().into_iter().unzip();
        ki_ui::tableau::disposer(&colonnes, largeur, espace::M).iter().map(|c| cols[c.0]).collect()
    }

    /// À 980 px de page (884 px de tableau, comme le classement), tout
    /// tient ; plus étroit, la date de lecture part la première, puis les
    /// médailles par la fin de la liste ; le joueur, le MVP et le total
    /// restent quoi qu'il arrive.
    #[test]
    fn les_medailles_gardent_l_essentiel_a_chaque_largeur() {
        assert_eq!(montrees(884.0).len(), 15, "tout tient à 980 px");
        let etroit = montrees(624.0);
        assert!(!etroit.contains(&Col::Lues), "{etroit:?}");
        let medailles: Vec<Medaille> = etroit.iter().filter_map(|c| if let Col::Une(m) = c { Some(*m) } else { None }).collect();
        assert_eq!(medailles, Medaille::TOUTES[..medailles.len()], "les premières de la liste restent, dans l'ordre");
        assert!(medailles.len() >= 6, "à 720 px : {medailles:?}");
        assert_eq!(montrees(150.0), [Col::Joueur, Col::Une(Medaille::Mvp), Col::Toutes], "ce qui ne part jamais");
    }

    fn membre(user_id: UserId, nom: &str, acte: &str, cet_acte: &[(Medaille, u32)]) -> FicheMembre {
        let medailles = Medailles {
            acte: acte.into(),
            cet_acte: cet_acte.iter().map(|&(medaille, fois)| CompteMedaille { medaille, fois, meilleur: 1.0 }).collect(),
            ..Default::default()
        };
        FicheMembre {
            user_id,
            username: nom.into(),
            fiche: FicheValorant { medailles: Some(medailles), ..Default::default() },
            bilan: None,
        }
    }

    fn groupe() -> Vec<FicheMembre> {
        vec![
            membre(1, "zed", "V26 · ACTE V", &[(Medaille::Mvp, 2), (Medaille::Aces, 1)]),
            membre(2, "Amnesyak", "V26 · ACTE V", &[(Medaille::Mvp, 2), (Medaille::Kills, 6)]),
            membre(3, "mg4230", "V26 · ACTE V", &[(Medaille::Kills, 9)]),
            // Lu pendant l'acte d'avant : ses neuf MVP ne comptent pas ici.
            membre(4, "BS", "V26 · ACTE IV", &[(Medaille::Mvp, 9)]),
            // Sans médailles du tout : pas de ligne.
            FicheMembre { user_id: 5, username: "Nouveau".into(), ..Default::default() },
        ]
    }

    /// L'acte est celui de la plupart ; une lecture d'un autre acte garde
    /// sa ligne, mais compte pour zéro, et le survol dit pourquoi.
    #[test]
    fn une_lecture_d_un_autre_acte_ne_compte_pas() {
        let stats = groupe();
        let (acte, comptes) = comptes(stats.iter());
        assert_eq!(acte, "V26 · ACTE V");
        assert_eq!(comptes.len(), 4, "qui n'a aucune médaille n'a pas de ligne");
        let bs = comptes.iter().find(|c| c.f.username == "BS").unwrap();
        assert_eq!((bs.fois(Medaille::Mvp), bs.total()), (0, 0));
        assert!(bs.lecture().contains("ACTE IV"), "{}", bs.lecture());
        let zed = comptes.iter().find(|c| c.f.username == "zed").unwrap();
        assert_eq!((zed.fois(Medaille::Mvp), zed.total()), (2, 3));
        // Personne n'a nommé l'acte : tout le monde compte.
        let sans_nom = [membre(1, "a", "", &[(Medaille::Mvp, 1)])];
        let (acte, comptes) = super::comptes(sans_nom.iter());
        assert!(acte.is_empty() && comptes[0].a_jour);
    }

    /// D'origine : les MVP, puis le total, puis le pseudo. Un en-tête
    /// cliqué trie par sa colonne, dans le sens demandé, et l'ordre
    /// d'origine départage.
    #[test]
    fn l_ordre_suit_la_colonne_choisie() {
        let stats = groupe();
        let (_, comptes) = comptes(stats.iter());
        let noms = |tri| ordonner(&comptes, tri).iter().map(|c| c.f.username.as_str()).collect::<Vec<_>>();
        // Amnesyak et zed ont deux MVP ; Amnesyak a huit médailles, zed trois.
        assert_eq!(noms(None), ["Amnesyak", "zed", "mg4230", "BS"]);
        assert_eq!(noms(tableau().tri_d_origine()), noms(None), "le tri d'origine est celui des MVP");
        let tri = |colonne, croissant| Some(Tri { colonne, croissant });
        assert_eq!(noms(tri(Col::Une(Medaille::Kills), false)), ["mg4230", "Amnesyak", "zed", "BS"]);
        assert_eq!(noms(tri(Col::Une(Medaille::Kills), true)), ["zed", "BS", "Amnesyak", "mg4230"]);
        assert_eq!(noms(tri(Col::Toutes, false)), ["mg4230", "Amnesyak", "zed", "BS"]);
        assert_eq!(noms(tri(Col::Joueur, true)), ["Amnesyak", "BS", "mg4230", "zed"]);
        assert_eq!(noms(tri(Col::Joueur, false)), ["zed", "mg4230", "BS", "Amnesyak"]);
        assert_eq!(noms(tri(Col::Lues, false)), noms(None), "une colonne qui ne trie pas laisse l'ordre d'origine");
    }
}
