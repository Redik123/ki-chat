//! Le banc des pages VALORANT, dans ki-chat même — en version de debug
//! seulement : `KI_BANC_VALO=dossier ki-chat` ouvre une fenêtre sur un
//! groupe inventé, photographie chaque scène (`valo-1-profil-riche.png`…)
//! dans `dossier`, puis se ferme. Il prend les vrais modules — la page, le
//! catalogue, les icônes de rang — et non des copies : ce qu'il montre est
//! ce que ki-chat dessine. Les images viennent du cache du catalogue sur
//! le disque, s'il y en a ; sinon le texte tient leur place, comme hors
//! ligne.
//!
//! Chaque scène pose la page dans une colonne de la largeur donnée, sur
//! une fenêtre assez haute pour la montrer entière.

use std::path::PathBuf;

use super::*;
use ki_protocol::{CompteMedaille, DetailManches, MedailleGagnee, MedaillesDuMatch, RangValorant, StatsSaison};

/// La fenêtre : assez large pour la plus large scène, assez haute pour
/// une page entière.
const FENETRE: [f32; 2] = [1500.0, 3300.0];

/// Le dossier des photos, si le banc est demandé.
pub(crate) fn demande() -> Option<PathBuf> {
    std::env::var_os("KI_BANC_VALO").map(PathBuf::from)
}

pub(crate) fn lancer(dossier: PathBuf) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size(FENETRE).with_title("Banc VALORANT"),
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native(
        "ki-chat-banc-valo",
        options,
        Box::new(|cc| {
            theme::install(&cc.egui_ctx);
            Ok(Box::new(Banc::new(dossier)))
        }),
    )
}

/// Une scène : son nom de fichier, la fiche ouverte, la largeur de la
/// colonne et la vue du profil.
struct Scene {
    nom: &'static str,
    fiche: usize,
    largeur: f32,
    vue: profil::VueProfil,
}

const SCENES: [Scene; 7] = [
    Scene { nom: "profil-riche", fiche: 0, largeur: 980.0, vue: profil::VueProfil::Ensemble },
    Scene { nom: "profil-etroit", fiche: 0, largeur: 720.0, vue: profil::VueProfil::Ensemble },
    Scene { nom: "profil-large", fiche: 0, largeur: 1440.0, vue: profil::VueProfil::Ensemble },
    Scene { nom: "profil-matchs", fiche: 0, largeur: 980.0, vue: profil::VueProfil::Matchs },
    Scene { nom: "profil-medailles", fiche: 0, largeur: 980.0, vue: profil::VueProfil::Medailles },
    Scene { nom: "profil-pauvre", fiche: 1, largeur: 980.0, vue: profil::VueProfil::Ensemble },
    Scene { nom: "profil-vide", fiche: 2, largeur: 980.0, vue: profil::VueProfil::Ensemble },
];

struct Banc {
    dossier: PathBuf,
    scene: usize,
    images: u32,
    demandee: bool,
    page: PageValo,
    stats: Vec<FicheMembre>,
    membres: Vec<Member>,
    activite: Vec<u16>,
    ouvertes: Vec<FicheOuverte>,
    rangs: rangs::Rangs,
    catalogue: Catalogue,
}

impl Banc {
    fn new(dossier: PathBuf) -> Self {
        let maintenant = maintenant_ms();
        let noms = ["drion", "Jerem", "Pandora", "Kevin", "Lina"];
        let fiches: Vec<FicheValorant> = [
            (1.08, (16, 64), 40, 0x5eed),
            (1.22, (19, 30), 36, 0xbeef),
            (0.95, (13, 72), 30, 0xcafe),
            (0.88, (11, 15), 24, 0xf00d),
            (1.0, (14, 48), 28, 0xabba),
        ]
        .iter()
        .zip(noms)
        .map(|(&(niveau, rang, n, graine), nom)| fiche_inventee(nom, niveau, rang, n, graine, maintenant))
        .collect();
        let stats: Vec<FicheMembre> = fiches
            .iter()
            .zip(noms)
            .enumerate()
            .map(|(i, (f, nom))| FicheMembre {
                user_id: i as UserId + 1,
                username: nom.to_string(),
                bilan: Some(bilan_local(f, maintenant)),
                fiche: f.resume(5, 10),
            })
            .collect();
        let membres = noms.iter().enumerate().map(|(i, nom)| membre(i as UserId + 1, nom)).collect();
        // Une soirée pauvre : trois classés ; puis rien du tout.
        let pauvre = FicheValorant { matchs: fiches[3].matchs[..3].to_vec(), historique_rr: fiches[3].historique_rr[..3].to_vec(), saisons: Vec::new(), medailles: None, ..fiches[3].clone() };
        let ouvertes = vec![
            FicheOuverte { user_id: 1, username: "drion".into(), recue: true, fiche: Some(fiches[0].clone()) },
            FicheOuverte { user_id: 4, username: "Kevin".into(), recue: true, fiche: Some(pauvre) },
            FicheOuverte { user_id: 9, username: "Nouveau".into(), recue: true, fiche: Some(FicheValorant { riot_id: "Nouveau#EUW".into(), ..Default::default() }) },
        ];
        let mut page = PageValo::load(|_, defaut| defaut.to_string());
        page.fiche_classe = true;
        page.fiche_periode = PeriodeFiche::TrenteJours;
        Self {
            dossier,
            scene: 0,
            images: 0,
            demandee: false,
            page,
            stats,
            membres,
            activite: activite_du_groupe(),
            ouvertes,
            rangs: rangs::Rangs::new(),
            catalogue: Catalogue::new(),
        }
    }
}

impl eframe::App for Banc {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.catalogue.preparer(&ctx);
        self.catalogue.demarrer();
        for tier in 3..=27 {
            self.rangs.preparer(&ctx, tier);
        }
        let Some(scene) = SCENES.get(self.scene) else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        };
        self.page.vue_profil = scene.vue;
        let cx = Contexte {
            stats: &self.stats,
            recu: true,
            esports: &[],
            activite: &self.activite,
            moi: Some(1),
            membres: &self.membres,
            rangs: &self.rangs,
            catalogue: &self.catalogue,
        };
        let ouverte = &self.ouvertes[scene.fiche];
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(theme::BG_BASE)).show(ui, |ui| {
            let colonne = Rect::from_min_size(ui.max_rect().min, Vec2::new(scene.largeur, ui.max_rect().height()));
            ui.painter().rect_filled(colonne, 0.0, theme::BG_BASE);
            let mut dedans = ui.new_child(egui::UiBuilder::new().max_rect(colonne.shrink(espace::XL)));
            self.page.profil(&mut dedans, ouverte, &cx);
        });
        // La scène suivante change de vue : celle-ci ne doit pas la garder.
        self.page.vue_profil = scene.vue;

        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let largeur = (scene.largeur as usize + 2).min(image.width());
            let chemin = self.dossier.join(format!("valo-{}-{}.png", self.scene + 1, scene.nom));
            if let Err(e) = ecrire_png(&chemin, &image, largeur) {
                eprintln!("{} : {e}", chemin.display());
            } else {
                println!("{}", chemin.display());
            }
            self.scene += 1;
            self.images = 0;
            self.demandee = false;
            ctx.request_repaint();
            return;
        }
        self.images += 1;
        // Le temps que les images du disque montent et que l'anneau glisse.
        if !self.demandee && self.images > 30 {
            self.demandee = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        ctx.request_repaint();
    }
}

/// La photo, rognée à la largeur de la colonne et à la hauteur de ce qui
/// est dessiné (le bas vide de la fenêtre en moins).
fn ecrire_png(chemin: &std::path::Path, image: &egui::ColorImage, largeur: usize) -> anyhow::Result<()> {
    let [w, h] = image.size;
    let fond = theme::BG_BASE;
    let vide = |y: usize| (0..largeur).all(|x| image.pixels[y * w + x] == fond);
    let hauteur = (0..h).rev().find(|&y| !vide(y)).map_or(h, |y| (y + 24).min(h));
    let mut rgba = Vec::with_capacity(largeur * hauteur * 4);
    for y in 0..hauteur {
        for x in 0..largeur {
            rgba.extend_from_slice(&image.pixels[y * w + x].to_array());
        }
    }
    let rogne = image::RgbaImage::from_raw(largeur as u32, hauteur as u32, rgba).ok_or_else(|| anyhow::anyhow!("image"))?;
    rogne.save(chemin)?;
    Ok(())
}

fn membre(user_id: UserId, nom: &str) -> Member {
    Member {
        user_id,
        username: nom.into(),
        speaking: false,
        muted: false,
        streaming: None,
        force_muted: false,
        force_deafened: false,
        admin: false,
        avatar: None,
        voice: None,
        jeu: None,
        riot_id: Some(format!("{nom}#EUW")),
        rang_valorant: None,
        compte_joue: None,
        roles: Vec::new(),
        online: true,
        color: None,
        rank: 0,
        invite: false,
        mobile: false,
    }
}

/// Un tirage déterministe : la même graine, la même soirée.
struct Des(u64);

impl Des {
    fn suivant(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    /// Entre 0 et 1.
    fn part(&mut self) -> f32 {
        (self.suivant() % 10_000) as f32 / 10_000.0
    }

    fn entre(&mut self, a: u32, b: u32) -> u32 {
        a + (self.suivant() % u64::from(b - a + 1)) as u32
    }

    fn parmi<'a>(&mut self, choix: &[&'a str]) -> &'a str {
        choix[(self.suivant() % choix.len() as u64) as usize]
    }
}

const CARTES: [&str; 7] = ["Ascent", "Lotus", "Haven", "Sunset", "Bind", "Split", "Icebox"];

/// Une fiche de `n` classés sur trente jours, jouée en soirée, au
/// `niveau` donné (1 : la moyenne), et finie au `rang` (palier, RR).
fn fiche_inventee(nom: &str, niveau: f32, rang: (u8, u16), n: usize, graine: u64, maintenant: u64) -> FicheValorant {
    let mut des = Des(graine);
    let agents: [&str; 6] = match nom {
        "drion" => ["Jett", "Jett", "Jett", "Omen", "Sova", "Gekko"],
        "Jerem" => ["Raze", "Raze", "Jett", "Reyna", "Omen", "Raze"],
        "Pandora" => ["Sage", "Killjoy", "Sage", "Cypher", "Viper", "Sage"],
        "Kevin" => ["Sova", "Fade", "Skye", "Sova", "Breach", "KAY/O"],
        _ => ["Omen", "Brimstone", "Viper", "Astra", "Omen", "Clove"],
    };
    let mut matchs = Vec::new();
    let mut points = Vec::new();
    // Minuit, ce soir, à l'heure locale.
    let minuit = chrono::Local::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|t| t.and_local_timezone(chrono::Local).single())
        .map_or(maintenant, |t| t.timestamp_millis() as u64);
    for i in 0..n {
        // Trois soirs sur quatre, une à trois parties de 20 h à 1 h.
        let jour = (i as u64 * 3) / 4 + 1;
        let heure = 20 + u64::from(des.entre(0, 5));
        let date = (minuit + heure * 3_600_000 + u64::from(des.entre(0, 50)) * 60_000).saturating_sub(jour * JOUR_MS).min(maintenant);
        let gagne = des.part() < 0.42 + 0.14 * niveau;
        let perdu = des.entre(3, 11) as u8;
        let manches = if gagne { (13, perdu) } else { (perdu, 13) };
        let total = u16::from(manches.0 + manches.1);
        let kills = ((f32::from(total) * 0.75 * niveau) * (0.7 + 0.6 * des.part())).round() as u16;
        let deaths = ((f32::from(total) * 0.72 / niveau) * (0.8 + 0.4 * des.part())).round().max(4.0) as u16;
        let assists = des.entre(2, 9) as u16;
        let deroule: String = {
            let mut v: Vec<char> = std::iter::repeat_n('V', manches.0 as usize).chain(std::iter::repeat_n('D', manches.1 as usize)).collect();
            for k in (1..v.len()).rev() {
                v.swap(k, des.entre(0, k as u32) as usize);
            }
            // La dernière manche revient au vainqueur.
            if let Some(p) = v.iter().rposition(|&c| c == if gagne { 'V' } else { 'D' }) {
                let fin = v.len() - 1;
                v.swap(p, fin);
            }
            v.into_iter().collect()
        };
        let id = format!("{nom}-{i}");
        let perf = (240.0 + 160.0 * niveau * (0.6 + 0.5 * des.part())).min(480.0) as u16;
        let mvp = if perf > 420 && gagne { ki_protocol::MVP_DU_MATCH } else if perf > 380 { ki_protocol::MVP_D_EQUIPE } else { 0 };
        let saison = if jour < 12 { "e10a3" } else { "e10a2" };
        matchs.push(MatchResume {
            id: id.clone(),
            date,
            carte: des.parmi(&CARTES).into(),
            mode: MODE_CLASSE.into(),
            agent: des.parmi(&agents).into(),
            kills,
            deaths,
            assists,
            score: u32::from(kills) * 210 + u32::from(assists) * 60 + des.entre(400, 1_800),
            tete_pct: 0,
            manches,
            gagne: Some(gagne),
            tier: rang.0,
            duree_s: u32::from(total) * 100,
            saison: saison.into(),
            degats: u32::from(total) * (110 + (40.0 * niveau) as u32 + des.entre(0, 30)),
            degats_recus: u32::from(total) * 130,
            tetes: des.entre(12, 30) as u16,
            tirs: des.entre(80, 120) as u16,
            party: des.entre(2, 5) as u8,
            avec: Vec::new(),
            contre: Vec::new(),
            manches_detail: Some(DetailManches {
                manches: total as u8,
                kast: (f32::from(total) * (0.62 + 0.1 * niveau)).round() as u8,
                premiers_sangs: des.entre(0, 5) as u8,
                premieres_morts: des.entre(0, 4) as u8,
                triples: des.entre(0, 2) as u8,
                quadruples: u8::from(des.part() < 0.2),
                aces: u8::from(des.part() < 0.04),
                clutchs_tentes: des.entre(0, 3) as u8,
                clutchs: des.entre(0, 1) as u8,
                meilleur_clutch: des.entre(0, 3) as u8,
                poses: des.entre(0, 4) as u8,
                desamorcages: des.entre(0, 2) as u8,
                deroule,
            }),
            perf,
            mvp,
        });
        let delta = if gagne { des.entre(15, 24) as i32 } else { -(des.entre(12, 19) as i32) };
        points.push(PointRR { match_id: id, date, tier: rang.0, rr: 0, delta, saison: saison.into(), carte: String::new(), ..Default::default() });
    }
    // Les RR remontés depuis le rang de fin, du plus récent au plus ancien.
    let mut valeur = i32::from(rang.0) * 100 + i32::from(rang.1);
    for (p, m) in points.iter_mut().zip(&matchs) {
        p.tier = (valeur / 100).clamp(3, 27) as u8;
        p.rr = valeur.rem_euclid(100) as u16;
        p.carte = m.carte.clone();
        valeur -= p.delta;
    }
    for m in matchs.iter_mut() {
        m.tier = points.iter().find(|p| p.match_id == m.id).map_or(rang.0, |p| p.tier);
    }
    let medailles = (nom == "drion").then(|| medailles_inventees(&matchs, maintenant));
    FicheValorant {
        riot_id: format!("{nom}#EUW"),
        region: "eu".into(),
        plateforme: "pc".into(),
        niveau: 140 + (graine % 120) as u32,
        rang: RangValorant { tier: rang.0, rr: rang.1, delta: points.first().map_or(0, |p| p.delta), boucliers: 1, ..Default::default() },
        pic: Some(RangValorant { tier: rang.0, rr: (rang.1 + 7).min(99), saison: "e10a3".into(), ..Default::default() }),
        historique_rr: points,
        matchs,
        maj: maintenant - 4 * 60_000,
        saisons: [(9, 1), (10, 2), (12, 1), (13, 3), (14, 2), (rang.0, 3)]
            .iter()
            .enumerate()
            .map(|(k, &(tier, acte))| StatsSaison {
                saison: format!("e{}a{acte}", 8 + k / 3),
                victoires: 18 + k as u16 * 3,
                parties: 34 + k as u16 * 2,
                tier_fin: tier.min(rang.0),
                rr_fin: 40 + k as u16 * 9,
            })
            .collect(),
        carte_joueur: String::new(),
        titre_joueur: String::new(),
        medailles,
    }
}

fn medailles_inventees(matchs: &[MatchResume], maintenant: u64) -> ki_protocol::Medailles {
    let compte = |medaille, fois, meilleur| CompteMedaille { medaille, fois, meilleur };
    ki_protocol::Medailles {
        maj: maintenant - 3_600_000,
        acte: "V26 · ACTE V".into(),
        cet_acte: vec![
            compte(Medaille::Mvp, 4, 468.0),
            compte(Medaille::Distinction, 5, 452.0),
            compte(Medaille::TopFrag, 9, 27.0),
            compte(Medaille::PremiersSangs, 11, 6.0),
            compte(Medaille::Degats, 7, 196.0),
            compte(Medaille::Tetes, 3, 41.0),
            compte(Medaille::Kills, 6, 27.0),
            compte(Medaille::Aces, 1, 1.0),
        ],
        carriere: vec![compte(Medaille::Mvp, 31, 482.0), compte(Medaille::PremiersSangs, 118, 7.0), compte(Medaille::Aces, 4, 1.0)],
        matchs: matchs
            .iter()
            .take(6)
            .enumerate()
            .filter(|(k, _)| k % 2 == 0)
            .map(|(k, m)| MedaillesDuMatch {
                id: m.id.clone(),
                debut: m.date,
                medailles: vec![MedailleGagnee { medaille: if k == 0 { Medaille::Mvp } else { Medaille::PremiersSangs }, valeur: 4.0, record: k == 0 }],
            })
            .collect(),
    }
}

/// Les parties du groupe par heure de la semaine, en UTC : le soir, plus
/// le week-end — la forme de la vraie carte de chaleur.
fn activite_du_groupe() -> Vec<u16> {
    (0..168)
        .map(|i| {
            let (jour, heure) = (i / 24, i % 24);
            let soir = matches!(heure, 18..=23 | 0..=1);
            let poids = if soir { 6 } else if heure >= 12 { 1 } else { 0 };
            let week_end = if jour >= 5 { 2 } else { 1 };
            (poids * week_end + (i * 7 % 3)) as u16 * u16::from(soir || heure >= 12)
        })
        .collect()
}
