//! Le banc de la voix d'à côté : rejoue un enregistrement de `sonde-voisine`
//! (ou n'importe quel WAV 48 kHz mono) dans la chaîne de décision telle
//! qu'elle tourne dans ki-chat — proximité, gain d'entrée, expanseur, gain
//! automatique, Silero, hystérésis, maintien — et dit, phase par phase,
//! quelle part des trames serait partie, et à quel niveau.
//!
//! ```text
//! cargo run --release -p ki-voice --example banc-voisine -- voisine-brut.wav [voisine-phases.txt] [force] [sens] [gain] [agc]
//! ```
//!
//! `force` : 0 coupée, 1 douce, 2 normale (défaut), 3 forte ; `sens` : le
//! seuil de parole Silero (défaut 0,5) ; `gain` : le pré-ampli (défaut 1,0) ;
//! `agc` : `ancien` (s'adapte sur tout, comme avant la 0.1.57) ou `nouveau`
//! (défaut : ne s'adapte que sur une trame proche). Débruitage, égaliseur et
//! compresseur ne sont pas simulés. Sans fichier de phases, tout
//! l'enregistrement est une seule phase. Le but : « moi » proche de 100 %,
//! « elle » proche de 0 %.

use ki_voice::agc::{Adaptation, Agc};
use ki_voice::effects::load_wav_file;
use ki_voice::parole::{maintien_trames, Decision, Parole};
use ki_voice::proximite::{Proximite, ReglagesProximite};
use ki_voice::silero::Silero;
use ki_voice::FRAME_SAMPLES;

struct Phase {
    nom: String,
    debut: usize,
    fin: usize,
}

fn lire_phases(chemin: Option<&String>, total: usize) -> Vec<Phase> {
    let Some(chemin) = chemin.filter(|c| !c.is_empty()) else {
        return vec![Phase { nom: "tout".into(), debut: 0, fin: total }];
    };
    let texte = std::fs::read_to_string(chemin).unwrap_or_default();
    let mut phases: Vec<Phase> = texte
        .lines()
        .filter_map(|l| {
            let mut m = l.split_whitespace();
            let nom = m.next()?.to_owned();
            let debut: usize = m.next()?.parse().ok()?;
            let fin: usize = m.next()?.parse().ok()?;
            Some(Phase { nom, debut, fin: fin.min(total) })
        })
        .collect();
    if phases.is_empty() {
        phases.push(Phase { nom: "tout".into(), debut: 0, fin: total });
    }
    phases
}

fn en_db(x: f32) -> f32 {
    20.0 * x.max(1e-9).log10()
}

fn efficace(t: &[f32]) -> f32 {
    (t.iter().map(|s| s * s).sum::<f32>() / t.len().max(1) as f32).sqrt()
}

fn centile(v: &mut [f32], c: f32) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    Some(v[((v.len() - 1) as f32 * c) as usize])
}

/// Ce que le banc retient d'une trame.
struct Trame {
    proche: bool,
    parole: bool,
    part: bool,
    /// Niveau efficace de ce qui partirait (après gain, expanseur, AGC).
    sortie_db: f32,
    /// Niveau efficace brut, avant tout.
    brut_db: f32,
    p: f32,
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(!args.is_empty(), "usage : banc-voisine <brut.wav> [phases.txt] [force] [sens] [gain] [ancien|nouveau]");
    let pcm = load_wav_file(&args[0])?;
    let phases = lire_phases(args.get(1), pcm.len());
    let force: u8 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(2);
    let sens: f32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.5);
    let gain: f32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let agc_ancien = args.get(5).is_some_and(|s| s == "ancien");
    // Des réglages imposés, pour chercher les bons : `marge=14`, `hyst=6`,
    // `maintien=8`, `prof=-18`.
    let marge: Option<f32> = args.iter().find_map(|a| a.strip_prefix("marge=")?.parse().ok());
    let hyst: Option<f32> = args.iter().find_map(|a| a.strip_prefix("hyst=")?.parse().ok());
    let maintien_prox: Option<u32> = args.iter().find_map(|a| a.strip_prefix("maintien=")?.parse().ok());
    let prof: Option<f32> = args.iter().find_map(|a| a.strip_prefix("prof=")?.parse().ok());

    // La chaîne, sans ancre : elle apprend en route, comme au premier
    // lancement.
    let mut vad = Silero::new()?;
    let mut reglages = ReglagesProximite::de_force(force, None);
    if let Some(m) = marge {
        reglages.marge_db = m;
    }
    if let Some(h) = hyst {
        reglages.hysterese_db = h;
    }
    if let Some(m) = maintien_prox {
        reglages.maintien_trames = m;
    }
    if let Some(p) = prof {
        reglages.profondeur_db = p;
    }
    let mut prox = Proximite::new(reglages);
    let mut agc = Agc::new();
    let mut decision = Decision::new();
    let maintien = maintien_trames(150);
    let mut p_prec: Option<f32> = None;

    let trames: Vec<Trame> = pcm
        .as_chunks::<FRAME_SAMPLES>()
        .0
        .iter()
        .map(|t| {
            let brut_db = en_db(efficace(t));
            let verdict = prox.mesurer(t, p_prec);
            let mut f = *t;
            if (gain - 1.0).abs() > 0.001 {
                for s in f.iter_mut() {
                    *s *= gain;
                }
            }
            prox.attenuer(&mut f);
            let adaptation = if agc_ancien || verdict.sur {
                Adaptation::Sure
            } else if verdict.proche {
                Adaptation::Proche
            } else {
                Adaptation::Non
            };
            agc.process(&mut f, 0.30, adaptation);
            vad.traiter(&f);
            let p = vad.derniere();
            p_prec = Some(p);
            let part = decision.trame(Parole::Neuronale { p, sens }, verdict.proche, maintien);
            Trame { proche: verdict.proche, parole: p >= sens, part, sortie_db: en_db(efficace(&f)), brut_db, p }
        })
        .collect();

    let r = prox.reglages();
    println!(
        "force {force} (marge {} dB, profondeur {} dB, hystérésis {} dB, maintien {} trames), seuil de parole {sens:.2}, pré-ampli {gain:.2}, AGC {} — référence apprise : {}, seuil : {}",
        r.marge_db,
        r.profondeur_db,
        r.hysterese_db,
        r.maintien_trames,
        if agc_ancien { "ancien (s'adapte sur tout)" } else { "nouveau (sur la voix proche)" },
        prox.dernier().reference_db.map(|r| format!("{r:.1} dBFS")).unwrap_or_else(|| "aucune".into()),
        prox.seuil_db().map(|r| format!("{r:.1} dBFS")).unwrap_or_else(|| "aucun".into()),
    );
    println!("phase      | trames | proche | parole | PART  | envoyé (eff. 90e) | brut (eff. 90e) | brut des trames sûres 50/70/90");
    for ph in &phases {
        let de = ph.debut / FRAME_SAMPLES;
        let a = (ph.fin / FRAME_SAMPLES).min(trames.len());
        if a <= de {
            continue;
        }
        let bloc = &trames[de..a];
        let n = bloc.len() as f32;
        let pour_cent = |f: &dyn Fn(&Trame) -> bool| bloc.iter().filter(|t| f(t)).count() as f32 / n * 100.0;
        let mut envoye: Vec<f32> = bloc.iter().filter(|t| t.part).map(|t| t.sortie_db).collect();
        let mut brut: Vec<f32> = bloc.iter().map(|t| t.brut_db).collect();
        let mut sures: Vec<f32> = bloc.iter().filter(|t| t.p >= 0.85).map(|t| t.brut_db).collect();
        let fmt = |v: Option<f32>| v.map(|x| format!("{x:+6.1}")).unwrap_or_else(|| "     —".into());
        let s50 = centile(&mut sures, 0.5);
        let s70 = centile(&mut sures, 0.7);
        let s90 = centile(&mut sures, 0.9);
        println!(
            "{:<10} | {:>6} | {:>4.0} % | {:>4.0} % | {:>3.0} % | {:>17} | {:>15} | {} / {} / {}",
            ph.nom,
            bloc.len(),
            pour_cent(&|t| t.proche),
            pour_cent(&|t| t.parole),
            pour_cent(&|t| t.part),
            fmt(centile(&mut envoye, 0.9)),
            fmt(centile(&mut brut, 0.9)),
            fmt(s50),
            fmt(s70),
            fmt(s90),
        );
    }
    Ok(())
}
