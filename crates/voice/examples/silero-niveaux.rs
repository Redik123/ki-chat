//! Silero face au niveau : la même voix, rejouée de plus en plus bas, garde-
//! t-elle sa probabilité de parole ? C'est ce qui décide si une voix
//! lointaine — donc faible — ouvre le micro comme une voix proche.
//!
//! ```text
//! cargo run --release -p ki-voice --example silero-niveaux -- voix.wav [autre.wav…]
//! ```

use ki_voice::effects::load_wav_file;
use ki_voice::silero::Silero;
use ki_voice::FRAME_SAMPLES;

fn main() -> anyhow::Result<()> {
    for chemin in std::env::args().skip(1) {
        let pcm = load_wav_file(&chemin)?;
        let crete = pcm.iter().fold(0f32, |m, s| m.max(s.abs()));
        println!("{chemin} — crête {:+.1} dBFS", 20.0 * crete.max(1e-9).log10());
        println!("  atténuation | crête dBFS | p ≥ 0,5 | p ≥ 0,86 | médiane | 90e centile");
        for att in [0, -6, -12, -18, -24, -30, -36, -42, -48, -60] {
            let g = 10f32.powf(att as f32 / 20.0);
            let mut vad = Silero::new()?;
            let mut ps = Vec::new();
            for t in pcm.as_chunks::<FRAME_SAMPLES>().0 {
                let trame: Vec<f32> = t.iter().map(|s| s * g).collect();
                if let Some(p) = vad.traiter(&trame) {
                    ps.push(p);
                }
            }
            let n = ps.len().max(1) as f32;
            let h5 = ps.iter().filter(|&&p| p >= 0.5).count() as f32 / n;
            let h86 = ps.iter().filter(|&&p| p >= 0.86).count() as f32 / n;
            ps.sort_by(f32::total_cmp);
            println!(
                "  {att:>8} dB | {:>+8.1}   | {:>5.0} % | {:>6.0} % | {:.2}    | {:.2}",
                20.0 * (crete * g).max(1e-9).log10(),
                h5 * 100.0,
                h86 * 100.0,
                ps[ps.len() / 2],
                ps[ps.len() * 9 / 10],
            );
        }
    }
    Ok(())
}
