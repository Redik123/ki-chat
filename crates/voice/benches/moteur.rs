//! Les deux chemins du moteur audio où il y a réellement du calcul par
//! échantillon, mesurés avant qu'on y touche.
//!
//! Ce ne sont pas des bancs décoratifs : ce sont les repères contre lesquels
//! P4 se jugera. `mix_into` fait aujourd'hui `pop_front()` échantillon par
//! échantillon sur un `VecDeque`, ce que le compilateur ne peut pas
//! vectoriser ; le tampon circulaire à tranches contiguës prévu par le plan
//! doit se voir ici, ou il ne sert à rien.
//!
//! `cargo bench -p ki-voice`

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use ki_voice::jitter::{Playout, Receiver};
use ki_voice::resample::CubicResampler;
use ki_voice::{FRAME_SAMPLES, SAMPLE_RATE};

/// Un locuteur dont on remplit le tampon de lecture directement.
///
/// Deux pièges se sont refermés ici avant que ces chiffres tiennent debout.
///
/// Le premier : criterion appelle la routine mesurée des **millions** de
/// fois, et un tampon rempli une bonne fois se vide en quelques centaines
/// d'itérations — après quoi `mix_into` sort immédiatement sans rien mixer.
/// Le banc annonçait 45 ns pour 960 échantillons, ce qu'aucun processeur ne
/// fait. Le remplissage doit donc revenir, et hors du temps mesuré.
///
/// Le second : remplir en passant par `Receiver::push` fait intervenir la
/// logique adaptative du tampon de gigue, qui escamote délibérément des
/// trames pour rattraper la dérive. Précieux en production, ruineux ici : le
/// tampon retombait à sec et l'on chronométrait encore la sortie anticipée.
/// D'où `fill_for_bench`, qui dépose du PCM déjà prêt.
struct Locuteur {
    playout: std::sync::Arc<std::sync::Mutex<Playout>>,
    /// Tenu vivant : c'est lui qui possède le tampon partagé.
    _rx: Receiver,
    phase: f32,
}

impl Locuteur {
    fn nouveau() -> Self {
        let rx = Receiver::new();
        let playout = rx.playout();
        Self { playout, _rx: rx, phase: 0.0 }
    }

    /// Dépose `frames` trames de PCM. Du signal, pas des zéros : un tampon de
    /// silence se comprimerait tout aussi bien en mémoire, mais la boucle de
    /// mixage calcule une crête — autant lui donner de quoi calculer.
    fn remplir(&mut self, frames: usize) {
        let mut bloc = vec![0f32; FRAME_SAMPLES * frames];
        for s in bloc.iter_mut() {
            *s = 0.35 * self.phase.sin();
            self.phase += 2.0 * std::f32::consts::PI * 180.0 / SAMPLE_RATE as f32;
        }
        self.playout.lock().unwrap().fill_for_bench(&bloc);
    }
}

/// Trames déposées entre deux salves mesurées. Assez pour que le coût du
/// chronomètre s'amortisse largement, assez peu pour que le tampon ne pèse
/// pas un mégaoctet.
const SALVE: usize = 256;

/// Le cœur du rappel de sortie : additionner N locuteurs dans une trame.
///
/// Un salon vocal à dix personnes, c'est dix `mix_into` par trame de 20 ms,
/// cinquante fois par seconde. C'est le seul endroit du client où le coût
/// croît avec le nombre de gens dans la pièce.
fn mixage(c: &mut Criterion) {
    let mut groupe = c.benchmark_group("mixage");
    for locuteurs in [1usize, 4, 10] {
        groupe.throughput(Throughput::Elements(FRAME_SAMPLES as u64));
        groupe.bench_function(
            BenchmarkId::from_parameter(format!("{locuteurs} locuteurs")),
            |b| {
                // `iter_custom` plutôt que `iter` : c'est le seul moyen de
                // remplir les tampons SANS que le remplissage — un décodage
                // Opus complet, cent fois le coût du mixage — n'entre dans la
                // mesure.
                b.iter_custom(|iterations| {
                    let mut gens: Vec<Locuteur> =
                        (0..locuteurs).map(|_| Locuteur::nouveau()).collect();
                    let mut sortie = [0f32; FRAME_SAMPLES];
                    let mut total = std::time::Duration::ZERO;
                    let mut faites = 0u64;

                    while faites < iterations {
                        // Hors chronomètre.
                        for g in gens.iter_mut() {
                            g.remplir(SALVE);
                        }
                        let salve = (SALVE as u64).min(iterations - faites);

                        let debut = std::time::Instant::now();
                        for _ in 0..salve {
                            sortie.fill(0.0);
                            for g in &gens {
                                black_box(g.playout.lock().unwrap().mix_into(&mut sortie, 1.0));
                            }
                            black_box(&sortie);
                        }
                        total += debut.elapsed();
                        faites += salve;
                    }
                    total
                });
            },
        );
    }
    groupe.finish();
}

/// Le rééchantillonnage, des deux côtés du moteur.
///
/// 44,1 kHz est la fréquence par défaut d'une quantité de cartes son : la
/// conversion tourne alors en permanence, sur le fil de capture ET dans le
/// rappel de sortie. À l'identité (48 → 48) elle ne devrait presque rien
/// coûter — c'est ce que ce banc vérifie aussi.
fn reechantillonnage(c: &mut Criterion) {
    let mut groupe = c.benchmark_group("reechantillonnage");
    let entree: Vec<f32> = (0..FRAME_SAMPLES)
        .map(|i| (i as f32 * 0.013).sin() * 0.4)
        .collect();

    for (nom, ratio) in [
        ("44k1_vers_48k", 44_100.0 / 48_000.0),
        ("48k_vers_44k1", 48_000.0 / 44_100.0),
        ("identite_48k", 1.0),
    ] {
        groupe.throughput(Throughput::Elements(FRAME_SAMPLES as u64));
        groupe.bench_function(nom, |b| {
            let mut r = CubicResampler::new(ratio);
            let mut sortie = [0f32; FRAME_SAMPLES];
            b.iter(|| {
                r.push(black_box(&entree));
                while r.can_pull(FRAME_SAMPLES) {
                    r.pull(&mut sortie);
                    black_box(&sortie);
                }
            });
        });
    }
    groupe.finish();
}

criterion_group!(bancs, mixage, reechantillonnage);
criterion_main!(bancs);
