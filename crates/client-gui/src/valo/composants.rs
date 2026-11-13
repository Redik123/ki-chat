//! Les morceaux d'interface que les onglets de la page se partagent : la
//! tuile d'un chiffre et sa grille, le texte d'une case de table, la
//! pastille d'une carte, le portrait d'un agent, les médailles d'un match.

use super::*;

/// Une tuile : un titre en petit, une valeur en grand à sa teinte, une
/// ligne du bas (qui, ou sur combien), une icône devant la valeur s'il y
/// en a une, et ce que dit le survol.
pub(super) struct Tuile<'a> {
    titre: &'a str,
    valeur: String,
    teinte: Color32,
    sous: String,
    icone: Option<&'a egui::TextureHandle>,
    info: String,
}

impl<'a> Tuile<'a> {
    pub(super) fn new(titre: &'a str, valeur: impl Into<String>, teinte: Color32) -> Self {
        Self { titre, valeur: valeur.into(), teinte, sous: String::new(), icone: None, info: String::new() }
    }

    /// Une tuile sans candidat : le tiret, et pourquoi.
    pub(super) fn vide(titre: &'a str, pourquoi: &str) -> Self {
        Self::new(titre, TIRET, TEXT_FAINT).sous(pourquoi)
    }

    pub(super) fn sous(mut self, sous: impl Into<String>) -> Self {
        self.sous = sous.into();
        self
    }

    pub(super) fn icone(mut self, icone: Option<&'a egui::TextureHandle>) -> Self {
        self.icone = icone;
        self
    }

    pub(super) fn info(mut self, info: impl Into<String>) -> Self {
        self.info = info.into();
        self
    }
}

/// Dessine une tuile de `largeur` (marges comprises) et d'au moins
/// `hauteur` ; `bas` ajoute ce qui va sous la ligne du bas — une jauge,
/// une barre. Rend la réponse du cadre.
pub(super) fn tuile(ui: &mut Ui, largeur: f32, hauteur: f32, t: Tuile, bas: impl FnOnce(&mut Ui)) -> Response {
    let interieur = largeur - 2.0 * MARGE_TUILE;
    let reponse = egui::Frame::new()
        .fill(theme::BG_RAISED)
        .corner_radius(egui::CornerRadius::same(rayon::XL))
        .inner_margin(egui::Margin::same(MARGE_TUILE as i8))
        .show(ui, |ui| {
            // Le cadre hérite de la disposition du parent — une ligne qui
            // replie, quand les tuiles sont côte à côte — et sans ceci le
            // titre, la valeur et la sous-ligne partiraient en escalier :
            // dedans, on empile toujours de haut en bas.
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_min_size(Vec2::new(interieur, hauteur - 2.0 * MARGE_TUILE));
                ui.set_max_width(interieur);
                ui.spacing_mut().item_spacing.y = espace::XXS;
                ui.add(egui::Label::new(Texte::Aide.riche(ui, t.titre)).truncate());
                ui.horizontal(|ui| {
                    if let Some(icone) = t.icone {
                        ui.add(egui::Image::new(icone).fit_to_exact_size(Vec2::splat(18.0)));
                    }
                    ui.add(egui::Label::new(Texte::TitreFenetre.riche(ui, &t.valeur).color(t.teinte)).truncate());
                });
                if !t.sous.is_empty() {
                    ui.add(egui::Label::new(Texte::Aide.riche_ton(ui, Ton::Discret, &t.sous)).truncate());
                }
                bas(ui);
            });
        })
        .response;
    if t.info.is_empty() {
        reponse
    } else {
        reponse.on_hover_text(t.info)
    }
}

/// Ce qui va sous la ligne du bas d'une tuile : une jauge, une barre, rien.
type Bas<'a> = Box<dyn FnOnce(&mut Ui) + 'a>;

/// Des tuiles de même taille rangées en lignes : autant par ligne que la
/// largeur disponible en contient, puis on passe à la suivante.
/// `horizontal_wrapped` ne convient pas ici : un cadre qui impose sa
/// largeur minimale ne replie pas, il déborde — et la fenêtre grandit pour
/// le contenir, jusqu'à sortir de l'écran sans pouvoir être réduite.
pub(super) struct Grille<'a> {
    largeur: f32,
    hauteur: f32,
    tuiles: Vec<(Tuile<'a>, Bas<'a>)>,
}

impl<'a> Grille<'a> {
    pub(super) fn new(largeur: f32, hauteur: f32) -> Self {
        Self { largeur, hauteur, tuiles: Vec::new() }
    }

    /// Une tuile de plus, avec ce qui va sous sa ligne du bas.
    pub(super) fn tuile(&mut self, t: Tuile<'a>, bas: impl FnOnce(&mut Ui) + 'a) {
        self.tuiles.push((t, Box::new(bas)));
    }

    pub(super) fn montrer(self, ui: &mut Ui) {
        let ecart = ui.spacing().item_spacing.x;
        let au_plus = (((ui.available_width() + ecart) / (self.largeur + ecart)).floor() as usize).max(1);
        // Des lignes équilibrées : six tuiles dans une fenêtre qui en
        // contient cinq font deux lignes de trois, pas cinq et une.
        let n = self.tuiles.len().max(1);
        let lignes = n.div_ceil(au_plus);
        let par_ligne = n.div_ceil(lignes);
        let mut reste = self.tuiles.into_iter().peekable();
        while reste.peek().is_some() {
            ui.horizontal(|ui| {
                for (t, bas) in reste.by_ref().take(par_ligne) {
                    tuile(ui, self.largeur, self.hauteur, t, bas);
                }
            });
        }
    }
}

/// Un texte de cellule, en 11,5 — la taille des tables de la page.
pub(super) fn cellule(ui: &mut Ui, texte: impl Into<String>, couleur: Color32) -> Response {
    ui.label(RichText::new(texte.into()).color(couleur).size(texte::PETIT))
}

/// Ce que dit le survol d'un point de RR : la carte, le delta, le rang,
/// quand, et ce qui l'explique (bonus, placement, descente protégée).
pub(super) fn info_de_point(x: &PointRR) -> String {
    let mut texte = format!(
        "{} · {} · {} · {} RR · {}",
        if x.carte.is_empty() { "?" } else { x.carte.as_str() },
        signe(x.delta).0,
        nom_de_rang(x.tier),
        x.rr,
        crate::formats::il_y_a(x.date)
    );
    if x.bonus != 0 {
        texte.push_str(&format!(" · dont {} de bonus de performance", signe(x.bonus).0));
    }
    if x.placement {
        texte.push_str(" · placement");
    }
    if x.protege {
        texte.push_str(" · descente protégée");
    }
    texte
}

/// L'agent d'une ligne de table : son portrait (valorant-api.com) puis son
/// nom — le nom seul tant que l'image n'est pas là.
pub(super) fn agent_cellule(ui: &mut Ui, agent: &str, catalogue: &Catalogue) -> Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = espace::XS;
        if let Some(portrait) = catalogue.agent(agent).filter(|_| !agent.is_empty()) {
            ui.add(egui::Image::new(portrait).fit_to_exact_size(Vec2::splat(PORTRAIT)));
        }
        cellule(ui, agent, TEXT_DIM);
    })
    .response
}

/// La carte d'une ligne de table : son nom sur son bandeau, assombri pour
/// que le texte se lise — le nom seul tant que l'image n'est pas là.
pub(super) fn carte_cellule(ui: &mut Ui, carte: &str, catalogue: &Catalogue) -> Response {
    let Some(bandeau) = catalogue.bandeau(carte).filter(|_| !carte.is_empty()) else {
        return cellule(ui, carte, TEXT);
    };
    pastille_carte(ui, carte, bandeau, HAUTEUR_PASTILLE, 11.5)
}

/// Un nom sur le bandeau de sa carte : l'image rognée au centre pour
/// remplir la pastille, un voile sombre, le texte en blanc.
pub(super) fn pastille_carte(ui: &mut Ui, texte: &str, bandeau: &egui::TextureHandle, hauteur: f32, taille: f32) -> Response {
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(texte.to_string(), FontId::proportional(taille), Color32::WHITE));
    let dimensions = Vec2::new((galley.size().x + 16.0).max(LARGEUR_PASTILLE_MIN), hauteur);
    let (rect, reponse) = ui.allocate_exact_size(dimensions, Sense::hover());
    if ui.is_rect_visible(rect) {
        egui::Image::new(bandeau)
            .uv(graphes::uv_couvrant(bandeau.size_vec2(), rect.size()))
            .corner_radius(4)
            .paint_at(ui, rect);
        ui.painter().rect_filled(rect, egui::CornerRadius::same(rayon::S), Color32::from_black_alpha(120));
        ui.painter().galley(Pos2::new(rect.left() + 8.0, rect.center().y - galley.size().y / 2.0), galley, Color32::WHITE);
    }
    reponse
}

/// L'or pour le MVP et la distinction, le texte pour les autres.
pub(super) fn teinte_de_medaille(m: Medaille) -> Color32 {
    match m {
        Medaille::Mvp | Medaille::Distinction => theme::WARN,
        _ => TEXT_DIM,
    }
}

/// Le nom court d'une médaille dans une case de table : les mots des
/// joueurs. Le nom entier est au survol.
pub(super) fn nom_court(m: Medaille) -> &'static str {
    match m {
        Medaille::Mvp => "MVP",
        Medaille::Distinction => "DIST",
        Medaille::TopFrag => "TOP",
        Medaille::Degats => "ADR",
        Medaille::Tetes => "HS",
        Medaille::Kills => "KILLS",
        Medaille::Assists => "ASSISTS",
        Medaille::Poses => "POSES",
        Medaille::PremiersSangs => "FB",
        Medaille::Aces => "ACE",
        Medaille::Clutchs => "CLUTCH",
        Medaille::Echanges => "TRADES",
        Medaille::Inconnue => "?",
    }
}

/// Les médailles d'un match à montrer, dans l'ordre : sans les inconnues,
/// et sans la distinction quand il y a le MVP — c'est la même note.
pub(super) fn medailles_a_montrer(md: &MedaillesDuMatch) -> Vec<&MedailleGagnee> {
    medailles_triees(&md.medailles)
}

/// [`medailles_a_montrer`], pour une liste de médailles.
pub(super) fn medailles_triees(medailles: &[MedailleGagnee]) -> Vec<&MedailleGagnee> {
    let mut liste: Vec<&MedailleGagnee> = medailles.iter().filter(|g| g.medaille != Medaille::Inconnue).collect();
    liste.sort_by_key(|g| g.medaille.ordre());
    let mvp = liste.iter().any(|g| g.medaille == Medaille::Mvp);
    liste.retain(|g| !(mvp && g.medaille == Medaille::Distinction));
    liste
}

/// Les médailles d'un match dans une case : trois pastilles au plus (★ :
/// record de l'acte), puis « +N » ; le détail au survol de chacune. Une
/// case vide sans médaille.
pub(super) fn medailles_cellule(ui: &mut Ui, md: Option<&MedaillesDuMatch>) -> Response {
    let liste = md.map(medailles_a_montrer).unwrap_or_default();
    if liste.is_empty() {
        return ui.label("");
    }
    let detail = liste
        .iter()
        .map(|g| {
            let record = if g.record { " · record de l'acte" } else { "" };
            format!("{} — {}{record}", g.medaille.nom(), g.medaille.valeur(g.valeur))
        })
        .collect::<Vec<_>>()
        .join("\n");
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = espace::XS;
        for g in liste.iter().take(3) {
            let texte = if g.record { format!("{}★", nom_court(g.medaille)) } else { nom_court(g.medaille).to_string() };
            badge(ui, &texte, teinte_de_medaille(g.medaille)).on_hover_text(&detail);
        }
        if liste.len() > 3 {
            ui.label(Texte::Minuscule.riche(ui, format!("+{}", liste.len() - 3))).on_hover_text(&detail);
        }
    })
    .response
}

/// Une étiquette en capitales sur un fond de sa couleur : « EN PARTIE ».
pub(super) fn badge(ui: &mut Ui, texte: &str, couleur: Color32) -> Response {
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(texte.to_string(), FontId::proportional(texte::MINUSCULE), couleur));
    let (rect, reponse) = ui.allocate_exact_size(Vec2::new(galley.size().x + 10.0, 16.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, egui::CornerRadius::same(rayon::S), theme::alpha(couleur, 36));
        ui.painter().galley(Pos2::new(rect.left() + 5.0, rect.center().y - galley.size().y / 2.0), galley, couleur);
    }
    reponse
}
