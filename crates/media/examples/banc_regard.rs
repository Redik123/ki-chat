//! Le banc du décodeur des spectateurs : le même flux H.264 décodé par
//! openh264 (l'ancien, un cœur), puis par le décodeur de Windows sur le
//! processeur et sur la carte graphique — en millisecondes par image,
//! conversion en RGBA comprise, et l'écart de leurs pixels avec openh264.
//!
//! Le flux : du H.264 brut (Annex B) avec des AUD, comme ffmpeg l'écrit avec
//! `-x264-params aud=1:repeat-headers=1 -bf 0 -f h264`.
//!
//! `cargo run --release -p ki-media --example banc_regard -- flux.h264`

use std::time::Instant;

use ki_media::h264::DecodeurH264;

fn main() -> anyhow::Result<()> {
    let chemin = std::env::args().nth(1).ok_or_else(|| anyhow::anyhow!("le chemin d'un flux .h264"))?;
    let octets = std::fs::read(&chemin)?;
    let trames = ki_media::annexb::unites_d_acces(&octets);
    println!("{} trames", trames.len());

    // openh264 : comme `ViewerDecoder`, décodage puis RGBA sur un cœur.
    let mut ancien = openh264::decoder::Decoder::new()?;
    let mut durees = Vec::new();
    let mut reference = Vec::new();
    for t in &trames {
        let debut = Instant::now();
        if let Some(image) = ancien.decode(t)? {
            use openh264::formats::YUVSource;
            let (l, h) = image.dimensions();
            let mut rgba = vec![0u8; l * h * 4];
            image.write_rgba8(&mut rgba);
            durees.push(debut.elapsed().as_secs_f64() * 1000.0);
            reference.push(rgba);
        }
    }
    resume("openh264                ", &durees);

    for (nom, decodeur) in [
        ("Windows, processeur      ", DecodeurH264::logiciel()),
        ("Windows, carte graphique ", DecodeurH264::new()),
    ] {
        let mut decodeur = decodeur?;
        let mut durees = Vec::new();
        let (mut somme, mut compte, mut pire) = (0u64, 0u64, 0u8);
        for t in &trames {
            let debut = Instant::now();
            if let Some(image) = decodeur.decoder(t)? {
                durees.push(debut.elapsed().as_secs_f64() * 1000.0);
                if let Some(attendu) = reference.get(durees.len() - 1) {
                    for (a, b) in image.rgba.iter().zip(attendu) {
                        let d = a.abs_diff(*b);
                        somme += u64::from(d);
                        pire = pire.max(d);
                    }
                    compte += image.rgba.len() as u64;
                }
            }
        }
        resume(nom, &durees);
        println!(
            "    sur la carte : {:?} ; écart des pixels avec openh264 : {:.3} en moyenne (sur 255), {pire} au pire",
            decodeur.sur_la_carte(),
            somme as f64 / compte.max(1) as f64,
        );
    }
    Ok(())
}

fn resume(nom: &str, durees: &[f64]) {
    let mut triees = durees.to_vec();
    triees.sort_by(f64::total_cmp);
    let rang = |q: f64| triees.get(((triees.len() as f64 - 1.0) * q).round() as usize).copied().unwrap_or(0.0);
    let moyenne = durees.iter().sum::<f64>() / durees.len().max(1) as f64;
    let en_retard = durees.iter().filter(|d| **d > 33.3).count();
    println!(
        "{nom}: {} images, {moyenne:.1} ms en moyenne, médiane {:.1}, 95 % sous {:.1}, pire {:.1} ; {en_retard} au-delà de 33 ms",
        durees.len(),
        rang(0.5),
        rang(0.95),
        rang(1.0),
    );
}
