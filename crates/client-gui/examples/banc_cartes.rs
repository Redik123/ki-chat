//! Le banc de la table de cartes : la fenêtre « Dernière carte » sur des
//! tables inventées, sans serveur ni ki-chat autour.
//!
//! `cargo run -p ki-client-gui --example banc_cartes`
//!
//! `BANC_CAPTURE=dossier` : le banc photographie chaque scène
//! (`cartes-1.png`, `cartes-2.png`…) puis se ferme — de quoi voir la table
//! sans la lancer soi-même. Sans capture, Espace passe à la scène suivante.
//! `BANC_PETIT=1` : un écran de 700×600, où la fenêtre tombe à sa taille
//! minimale (`petit-cartes-1.png`…).
//!
//! Les scènes d'animation reçoivent un état, puis un second quelques images
//! plus tard (`suite`), et se photographient en plein effet.

#[allow(dead_code)]
#[path = "../src/cartes.rs"]
mod cartes;

use eframe::egui;
use ki_protocol::{
    Carte, CouleurCarte, EvenementCartes, JoueurCartes, PhaseCartes, ReglesCartes, TableCartes, UserId,
};

const MOI: UserId = 1;
const NOMS: [&str; 6] = ["", "drion", "Kevin", "Lina", "Tom", "Sacha"];

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(if petit() { [700.0, 600.0] } else { [1000.0, 720.0] })
            .with_title("Banc des cartes"),
        // Chaque lancement à sa taille : rien de retenu du précédent.
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native(
        "banc-cartes",
        options,
        Box::new(|cc| {
            ki_ui::style::installer(&cc.egui_ctx);
            let capture = std::env::var_os("BANC_CAPTURE").map(std::path::PathBuf::from);
            let mut banc = Banc {
                scene: 0,
                images: 0,
                demandee: false,
                capture,
                cartes: cartes::Cartes::default(),
                suite_le: None,
            };
            banc.poser();
            Ok(Box::new(banc))
        }),
    )
}

fn petit() -> bool {
    std::env::var_os("BANC_PETIT").is_some()
}

struct Banc {
    scene: usize,
    images: u32,
    demandee: bool,
    capture: Option<std::path::PathBuf>,
    cartes: cartes::Cartes,
    /// Quand la suite de la scène a été reçue.
    suite_le: Option<std::time::Instant>,
}

/// Le nombre de scènes.
const SCENES: usize = 21;

/// Les deux règles en plus.
const SEPT_ET_ZERO: ReglesCartes = ReglesCartes { sept: true, zero: true };

/// Une table en pleine manche, à mon tour.
fn en_cours() -> TableCartes {
    TableCartes {
        salon: 3,
        hote: MOI,
        phase: PhaseCartes::EnCours,
        joueurs: vec![joueur(1, 8), joueur(2, 5), JoueurCartes { annonce: true, ..joueur(3, 2) }, JoueurCartes { absent: true, ..joueur(4, 9) }, joueur(5, 4)],
        // 7 rouge et 3 rouge, 7 bleu, +2 vert, passe jaune, joker, +4, 9 vert.
        main: [13u8, 5, 88, 73, 44, 100, 104, 67].into_iter().map(Carte).collect(),
        dessus: Some(Carte(13)),
        couleur: Some(CouleurCarte::Rouge),
        tour: Some(MOI),
        horaire: true,
        pioche: 61,
        reste_ms: 21_000,
        manche: 2,
        journal: vec![
            EvenementCartes::Manche,
            EvenementCartes::Joue { par: 2, carte: Carte(13), couleur: None },
            EvenementCartes::Pioche { par: 3, n: 1, manque: 0 },
            EvenementCartes::Annonce { par: 4 },
            EvenementCartes::Joue { par: 4, carte: Carte(100), couleur: Some(CouleurCarte::Rouge) },
            EvenementCartes::Joue { par: 5, carte: Carte(13), couleur: None },
        ],
        numero: 12,
        ..Default::default()
    }
}

/// La table après un événement de plus.
fn apres(mut t: TableCartes, e: EvenementCartes) -> TableCartes {
    t.journal.push(e);
    t.numero += 1;
    t
}

/// Les scènes animées : le second état, et quand le photographier (en
/// secondes après l'avoir reçu).
fn suite(n: usize) -> Option<(TableCartes, f32)> {
    let base = en_cours();
    Some(match n {
        // Lina dit « Dernière carte ! ».
        10 => (apres(base, EvenementCartes::Annonce { par: 3 }), 0.4),
        // Kevin contre Lina, qui pioche deux cartes.
        11 => {
            let mut t = apres(base, EvenementCartes::Contre { par: 2, cible: 3, n: 2 });
            t.joueurs[2].cartes = 4;
            t.joueurs[2].annonce = false;
            (t, 0.35)
        }
        // Je pose mon 3 rouge : il vole vers la défausse.
        12 => {
            let mut t = apres(base, EvenementCartes::Joue { par: MOI, carte: Carte(5), couleur: None });
            t.main.retain(|c| *c != Carte(5));
            t.dessus = Some(Carte(5));
            t.tour = Some(2);
            (t, 0.16)
        }
        // Je pioche deux cartes : elles volent vers ma main.
        13 => {
            let mut t = apres(base, EvenementCartes::Pioche { par: MOI, n: 2, manque: 0 });
            t.main.extend([Carte(11), Carte(77)]);
            t.main.sort_by_key(|c| c.rang());
            t.pioche = 59;
            (t, 0.2)
        }
        // Lina gagne : confettis.
        14 => {
            let mut t = apres(base, EvenementCartes::Gagne { par: 3, bot: false });
            t.phase = PhaseCartes::Finie;
            t.tour = None;
            t.gagnant = Some(3);
            t.joueurs[2].cartes = 0;
            t.joueurs[2].victoires = 1;
            (t, 0.9)
        }
        // Mon 7 rouge, et j'échange ma main avec celle de Kevin.
        19 => {
            let mut t = apres(regle(base), EvenementCartes::Joue { par: MOI, carte: Carte(13), couleur: None });
            t = apres(t, EvenementCartes::Echange { par: MOI, avec: 2 });
            t.main = [30u8, 31, 55, 80, 101].into_iter().map(Carte).collect();
            t.joueurs[0].cartes = 5;
            t.joueurs[1].cartes = 7;
            t.tour = Some(2);
            (t, 0.45)
        }
        // Le 0 de Kevin : chaque main passe au voisin.
        20 => {
            let mut t = apres(regle(base), EvenementCartes::Joue { par: 2, carte: Carte(25), couleur: None });
            t = apres(t, EvenementCartes::Tourne { par: 2 });
            t.dessus = Some(Carte(25));
            t.main = [26u8, 27, 51, 76].into_iter().map(Carte).collect();
            let comptes = [4, 8, 4, 2, 9];
            t.joueurs.iter_mut().zip(comptes).for_each(|(j, n)| j.cartes = n);
            t.tour = Some(3);
            (t, 0.45)
        }
        // La donne d'une nouvelle manche, carte par carte.
        15 => {
            let mut t = en_cours();
            t.journal = vec![EvenementCartes::Manche];
            t.numero = 5;
            t.joueurs.iter_mut().for_each(|j| {
                j.cartes = 7;
                j.annonce = false;
            });
            t.main.truncate(7);
            (t, 0.5)
        }
        _ => return None,
    })
}

/// La même table, avec les deux règles en plus.
fn regle(t: TableCartes) -> TableCartes {
    TableCartes { regles: SEPT_ET_ZERO, ..t }
}

fn joueur(user_id: UserId, cartes: u8) -> JoueurCartes {
    JoueurCartes { user_id, cartes, ..Default::default() }
}

/// Les scènes : sans table, l'attente, mon tour, un cumul et un contre, la
/// fin de manche, la table vue par un spectateur, deux grosses mains, et
/// les places tenues par un bot.
fn scene(n: usize) -> Option<Option<TableCartes>> {
    let en_cours = en_cours();
    Some(match n {
        0 => None,
        1 | 15 => Some(TableCartes {
            salon: 3,
            hote: MOI,
            phase: PhaseCartes::Attente,
            joueurs: vec![joueur(1, 0), joueur(2, 0), joueur(3, 0)],
            horaire: true,
            journal: vec![EvenementCartes::Assis { par: 1 }, EvenementCartes::Assis { par: 2 }, EvenementCartes::Assis { par: 3 }],
            numero: 3,
            ..Default::default()
        }),
        2 => Some(en_cours),
        3 => Some(TableCartes {
            dessus: Some(Carte(73)),
            couleur: Some(CouleurCarte::Vert),
            cumul: 4,
            horaire: false,
            expose: Some(2),
            joueurs: vec![joueur(1, 8), joueur(2, 1), joueur(3, 6), joueur(4, 9), joueur(5, 4)],
            reste_ms: 6_000,
            ..en_cours.clone()
        }),
        4 => Some(TableCartes {
            phase: PhaseCartes::Finie,
            tour: None,
            gagnant: Some(3),
            joueurs: vec![
                JoueurCartes { victoires: 1, ..joueur(1, 3) },
                joueur(2, 5),
                JoueurCartes { victoires: 2, ..joueur(3, 0) },
                joueur(4, 9),
                joueur(5, 1),
            ],
            journal: vec![EvenementCartes::Gagne { par: 3, bot: false }],
            ..en_cours.clone()
        }),
        5 => Some(TableCartes { main: Vec::new(), tour: Some(2), joueurs: en_cours.joueurs[1..].to_vec(), ..en_cours }),
        // Une grosse main : 26 cartes, puis 60, après une guerre de +4.
        6 | 7 => {
            let n = if n == 6 { 26 } else { 60 };
            let main: Vec<Carte> = (0..ki_protocol::CARTES_DU_JEU).step_by(108 / n + 1).take(n).map(Carte).collect();
            let mut main = main;
            let mut k = 1u8;
            while main.len() < n {
                if !main.contains(&Carte(k)) {
                    main.push(Carte(k));
                }
                k += 1;
            }
            main.sort_by_key(|c| c.rang());
            let mut joueurs = en_cours.joueurs.clone();
            joueurs[0].cartes = n as u8;
            Some(TableCartes { main, joueurs, ..en_cours })
        }
        // Tom est parti : un bot tient sa place. Vue d'un pote qui arrive
        // (8), puis de ma place, qu'un bot tient pendant que je suis ailleurs (9).
        8 | 9 => {
            let mut joueurs = en_cours.joueurs.clone();
            joueurs[3].bot = true;
            let mut journal = en_cours.journal.clone();
            journal.push(EvenementCartes::Remplace { par: 4 });
            if n == 9 {
                joueurs[0].bot = true;
                journal.push(EvenementCartes::Remplace { par: 1 });
            }
            let tour = if n == 8 { Some(4) } else { Some(MOI) };
            let main = if n == 8 { Vec::new() } else { en_cours.main.clone() };
            Some(TableCartes { joueurs, journal, tour, main, numero: 14, ..en_cours })
        }
        10..=14 => Some(en_cours),
        // Les règles en plus, avant de distribuer : vues de l'hôte (16),
        // puis d'un joueur assis (17).
        16 | 17 => Some(TableCartes {
            salon: 3,
            hote: if n == 16 { MOI } else { 2 },
            phase: PhaseCartes::Attente,
            joueurs: vec![joueur(1, 0), joueur(2, 0), joueur(3, 0), joueur(4, 0)],
            horaire: true,
            regles: ReglesCartes { sept: true, zero: false },
            journal: vec![EvenementCartes::Assis { par: 4 }],
            numero: 5,
            ..Default::default()
        }),
        // Je pose mon 7 rouge : avec qui échanger ? (18) ; puis l'échange
        // (19) et le 0 qui fait tourner (20), animés.
        18..=20 => Some(regle(en_cours)),
        _ => return None,
    })
}

impl Banc {
    fn poser(&mut self) {
        self.cartes = cartes::Cartes::default();
        if let Some(table) = scene(self.scene) {
            // Le spectateur (5) et le pote qui arrive (8) ne sont pas assis.
            let moi = if matches!(self.scene, 5 | 8) { 9 } else { MOI };
            self.cartes.recevoir(table, Some(moi));
            self.cartes.ouvert = true;
            self.cartes.regles_disponibles = true;
            if self.scene == 18 {
                let t = self.cartes.table().cloned().expect("une table");
                self.cartes.jouer_carte(&t, MOI, Carte(13), &mut Vec::new());
            }
        }
        self.images = 0;
        self.demandee = false;
        self.suite_le = None;
    }
}

impl eframe::App for Banc {
    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ui.painter().rect_filled(ctx.content_rect(), 0.0, ki_ui::jetons::couleur::BG_BASE);
        let qui = |id: UserId| cartes::Identite {
            nom: NOMS.get(id as usize).copied().unwrap_or("quelqu'un").to_string(),
            couleur: ki_ui::jetons::couleur::pour_pseudo(NOMS.get(id as usize).copied().unwrap_or("?")),
            photo: None,
        };
        let moi = if matches!(self.scene, 5 | 8) { 9 } else { MOI };
        let _ = self.cartes.fenetre(&ctx, Some(moi), true, &qui);

        let Some(dossier) = self.capture.clone() else {
            if ctx.input(|i| i.key_pressed(egui::Key::Space)) {
                self.scene = (self.scene + 1) % SCENES;
                self.poser();
            }
            return;
        };
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let prefixe = if petit() { "petit-" } else { "" };
            let chemin = dossier.join(format!("{prefixe}cartes-{}.png", self.scene + 1));
            let fichier = std::fs::File::create(&chemin).expect("dossier de capture");
            let mut png = png::Encoder::new(std::io::BufWriter::new(fichier), image.width() as u32, image.height() as u32);
            png.set_color(png::ColorType::Rgba);
            png.set_depth(png::BitDepth::Eight);
            let octets: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            png.write_header().and_then(|mut w| w.write_image_data(&octets)).expect("écriture du PNG");
            println!("{}", chemin.display());
            self.scene += 1;
            if scene(self.scene).is_none() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            self.poser();
        }
        self.images += 1;
        // La suite d'une scène animée, quelques images après la première
        // vue ; la photo, en plein effet.
        let pret = match suite(self.scene) {
            Some((table, delai)) => {
                if self.suite_le.is_none() && self.images >= 4 {
                    self.cartes.recevoir(Some(table), Some(moi));
                    self.suite_le = Some(std::time::Instant::now());
                }
                self.suite_le.is_some_and(|q| q.elapsed().as_secs_f32() >= delai)
            }
            None => self.images > 8,
        };
        if !self.demandee && pret {
            self.demandee = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        ctx.request_repaint();
    }
}
