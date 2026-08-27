//! Rééchantillonneur cubique simple et sans allocation sur le chemin chaud.
//!
//! L'interpolation du premier ordre se comporte comme un filtre passe-bas
//! grossier : sur les conversions 44,1 ⇄ 48 kHz elle mange les aigus et
//! replie du spectre, ce qui s'entend sur un bon casque. L'interpolation
//! cubique de Hermite (Catmull-Rom, 4 points) fait tomber cette erreur de
//! deux ordres de grandeur pour quelques multiplications de plus, là où un
//! banc de filtres sinc coûterait bien trop cher dans un rappel temps réel.
//!
//! Ce que ça coûte : deux échantillons d'avance (~42 µs à 48 kHz) et un
//! échantillon de passé conservés dans le tampon entre deux appels.

use std::collections::VecDeque;

pub struct CubicResampler {
    /// Échantillons d'entrée consommés par échantillon de sortie,
    /// **après** pré-moyennage.
    ratio: f64,
    /// Pré-moyennage par blocs de `preavg` échantillons avant interpolation.
    /// 1 = aucun. Voir `new` pour le pourquoi.
    preavg: usize,
    /// Échantillons d'entrée en attente d'un bloc de moyennage complet.
    carry: Vec<f32>,
    buf: VecDeque<f32>,
    /// Position fractionnaire de lecture dans `buf`.
    pos: f64,
    /// Rapport exactement 1 et aucun pré-moyennage : la sortie est l'entrée.
    passthrough: bool,
}

impl CubicResampler {
    pub fn new(mut ratio: f64) -> Self {
        assert!(ratio > 0.0);
        // Décimation forte (micros en 96 ou 192 kHz) : l'interpolation
        // cubique seule ne filtre rien — elle lit quatre points autour d'une
        // position et saute par-dessus le reste, si bien que l'ultrason se
        // replie en pleine bande vocale. On moyenne d'abord par blocs de 2,
        // autant de fois qu'il faut pour ramener le rapport sous 1,5 : un
        // passe-bas grossier mais réel (zéro exact à la nouvelle fréquence de
        // Nyquist, −14 dB là où les repliements font le plus mal), et le
        // cubique travaille ensuite près du rapport 1, où il excelle.
        let mut preavg = 1usize;
        while ratio > 1.5 {
            preavg *= 2;
            ratio /= 2.0;
        }
        // Rapport exactement 1 : rien à convertir. C'est le cas le PLUS
        // courant — le moteur demande 48 kHz et la plupart des cartes le
        // donnent — et il coûtait pourtant une interpolation cubique
        // complète, soit autant qu'une vraie conversion (mesuré : 5,29 µs
        // par trame contre 5,68 pour un 44,1 → 48). On le reconnaît une fois,
        // à la construction, et `pull` se contente alors de recopier.
        let passthrough = preavg == 1 && ratio == 1.0;
        Self {
            ratio,
            preavg,
            passthrough,
            carry: Vec::new(),
            buf: VecDeque::new(),
            pos: 0.0,
        }
    }

    pub fn push(&mut self, samples: &[f32]) {
        if self.preavg == 1 {
            self.buf.extend(samples);
            return;
        }
        for &s in samples {
            self.carry.push(s);
            if self.carry.len() == self.preavg {
                let avg = self.carry.iter().sum::<f32>() / self.preavg as f32;
                self.buf.push_back(avg);
                self.carry.clear();
            }
        }
    }

    /// Vrai si `out_len` échantillons de sortie peuvent être produits.
    pub fn can_pull(&self, out_len: usize) -> bool {
        if out_len == 0 {
            return true;
        }
        if self.passthrough {
            // Aucune marge d'avance à réserver : on ne lit qu'un point.
            return self.buf.len() >= out_len;
        }
        // Le dernier échantillon produit lit jusqu'à `i + 2` ; sans cette
        // marge de deux, `pull` retomberait sur ses valeurs de repli en fin
        // de tampon et l'on entendrait la couture.
        let last = self.pos + self.ratio * (out_len - 1) as f64;
        (last as usize) + 3 <= self.buf.len()
    }

    /// Produit `out.len()` échantillons par interpolation cubique de Hermite.
    /// Appeler `can_pull` d'abord ; sinon la dernière valeur connue est
    /// maintenue (un zéro, lui, ferait un clic).
    pub fn pull(&mut self, out: &mut [f32]) {
        if self.passthrough {
            // Recopie directe. `as_slices` donne les deux moitiés contiguës
            // du tampon circulaire : de quoi laisser le compilateur vectoriser
            // ce que `pop_front()` échantillon par échantillon lui interdit.
            let n = out.len().min(self.buf.len());
            let (a, b) = self.buf.as_slices();
            let pris_a = a.len().min(n);
            out[..pris_a].copy_from_slice(&a[..pris_a]);
            out[pris_a..n].copy_from_slice(&b[..n - pris_a]);
            // À sec : on tient la dernière valeur plutôt que de plonger à
            // zéro, exactement comme le chemin cubique.
            if n < out.len() {
                let tenue = out[..n].last().copied().unwrap_or(0.0);
                out[n..].fill(tenue);
            }
            self.buf.drain(..n);
            return;
        }
        for o in out.iter_mut() {
            let i = self.pos as usize;
            let t = (self.pos - i as f64) as f32;

            // Quatre points autour de la position de lecture : `p1` et `p2`
            // l'encadrent, `p0` et `p3` donnent les pentes aux extrémités.
            let p1 = match self.buf.get(i) {
                Some(&s) => s,
                // Tampon à sec : on tient la dernière valeur au lieu de
                // plonger à zéro.
                None => self.buf.back().copied().unwrap_or(0.0),
            };
            // Au tout premier échantillon du flux il n'y a pas de passé : on
            // duplique `p1`, ce qui démarre sur une pente nulle plutôt que
            // sur un saut inventé.
            let p0 = if i > 0 { self.buf.get(i - 1).copied().unwrap_or(p1) } else { p1 };
            let p2 = self.buf.get(i + 1).copied().unwrap_or(p1);
            let p3 = self.buf.get(i + 2).copied().unwrap_or(p2);

            // Forme de Horner : quatre multiplications, aucune division.
            let c0 = p1;
            let c1 = 0.5 * (p2 - p0);
            let c2 = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
            let c3 = 0.5 * (p3 - p0) + 1.5 * (p1 - p2);
            *o = ((c3 * t + c2) * t + c1) * t + c0;

            self.pos += self.ratio;
        }
        // On garde un échantillon derrière la position de lecture : c'est le
        // `p0` du premier échantillon du prochain bloc. Tout drainer
        // repartirait d'une pente nulle à chaque appel, d'où un craquement
        // périodique cadencé par le rappel audio.
        let consumed = (self.pos as usize).saturating_sub(1).min(self.buf.len());
        self.buf.drain(..consumed);
        self.pos -= consumed as f64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    /// Au rapport 1, la sortie doit être l'entrée **au bit près** — pas
    /// « à peu près ». C'est tout l'intérêt du chemin rapide : le cubique,
    /// lui, recalculait une valeur interpolée qui ne tombait pas exactement
    /// sur l'échantillon d'origine.
    #[test]
    fn a_l_identite_la_sortie_est_l_entree() {
        let entree: Vec<f32> = (0..960).map(|i| (i as f32 * 0.017).sin()).collect();
        let mut r = CubicResampler::new(1.0);
        assert!(r.passthrough, "48 -> 48 doit prendre le chemin rapide");

        r.push(&entree);
        assert!(r.can_pull(entree.len()));
        let mut sortie = vec![0f32; entree.len()];
        r.pull(&mut sortie);
        assert_eq!(sortie, entree);
    }

    /// Le chemin rapide doit survivre au tampon circulaire enroulé : c'est
    /// justement le cas que `as_slices` rend en deux morceaux, et le seul où
    /// une recopie naïve se tromperait.
    #[test]
    fn le_chemin_rapide_survit_a_l_enroulement() {
        let mut r = CubicResampler::new(1.0);
        // Assez grand pour le plus gros bloc produit plus bas.
        let mut sortie = vec![0f32; 200];
        // Des blocs de tailles inégales, poussés et tirés en alternance : le
        // tampon circulaire finit forcément par s'enrouler, et c'est le seul
        // cas où une recopie naïve d'`as_slices` se tromperait.
        for tour in 0..40u32 {
            let n = 60 + (tour as usize % 7) * 20;
            let bloc: Vec<f32> = (0..n).map(|i| (tour * 1000 + i as u32) as f32).collect();
            r.push(&bloc);
            assert!(r.can_pull(n));
            let sortie = &mut sortie[..n];
            r.pull(sortie);
            assert_eq!(sortie, &bloc[..], "tour {tour}");
        }
    }

    /// À sec, on tient la dernière valeur au lieu de plonger à zéro — un zéro
    /// franc s'entendrait comme un clic. Même contrat que le chemin cubique.
    #[test]
    fn a_sec_le_chemin_rapide_tient_la_derniere_valeur() {
        let mut r = CubicResampler::new(1.0);
        r.push(&[0.5, 0.5, 0.5]);
        let mut sortie = vec![0f32; 6];
        r.pull(&mut sortie);
        assert_eq!(sortie, vec![0.5; 6], "la valeur tenue, pas du silence");
    }

    /// L'ancienne interpolation du premier ordre, gardée comme étalon : elle
    /// sert à prouver que le cubique apporte vraiment quelque chose.
    fn linear_reference(input: &[f32], ratio: f64, out_len: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(out_len);
        let mut pos = 0f64;
        for _ in 0..out_len {
            let i = pos as usize;
            let frac = (pos - i as f64) as f32;
            let a = input.get(i).copied().unwrap_or(0.0);
            let b = input.get(i + 1).copied().unwrap_or(a);
            out.push(a + (b - a) * frac);
            pos += ratio;
        }
        out
    }

    fn rms_error(got: &[f32], want: &[f32]) -> f64 {
        let sum: f64 = got
            .iter()
            .zip(want)
            .map(|(&g, &w)| {
                let d = (g - w) as f64;
                d * d
            })
            .sum();
        (sum / got.len() as f64).sqrt()
    }

    fn sine(freq: f64, rate: f64, n: usize) -> Vec<f32> {
        (0..n).map(|k| (2.0 * PI * freq * k as f64 / rate).sin() as f32).collect()
    }

    #[test]
    fn identity_ratio_passes_through() {
        let mut r = CubicResampler::new(1.0);
        let input: Vec<f32> = (0..100).map(|i| i as f32).collect();
        r.push(&input);
        assert!(r.can_pull(64));
        let mut out = [0f32; 64];
        r.pull(&mut out);
        for (i, &s) in out.iter().enumerate() {
            assert!((s - i as f32).abs() < 1e-6);
        }
    }

    #[test]
    fn downsample_consumes_more_input() {
        // 44,1 kHz -> 48 kHz inversé : ratio > 1 consomme plus qu'il ne produit.
        let mut r = CubicResampler::new(48_000.0 / 44_100.0);
        r.push(&vec![0.5f32; 2000]);
        assert!(r.can_pull(960));
        let mut out = [0f32; 960];
        r.pull(&mut out);
        assert!(out.iter().all(|s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn cannot_pull_without_enough_input() {
        // Un rapport non unitaire : c'est le chemin cubique, celui qui lit
        // deux points au-delà de sa position et réclame donc de l'avance.
        // Au rapport 1 exact, le chemin rapide ne lit qu'un point et n'a rien
        // à réserver — vérifié juste en dessous.
        let mut r = CubicResampler::new(44_100.0 / 48_000.0);
        assert!(!r.passthrough);
        r.push(&[0.0; 100]);
        assert!(!r.can_pull(110));
        assert!(r.can_pull(98));
    }

    #[test]
    fn le_chemin_rapide_ne_reserve_aucune_avance() {
        let mut r = CubicResampler::new(1.0);
        r.push(&[0.0; 100]);
        // Tout ce qui est entré peut sortir, jusqu'au dernier échantillon.
        assert!(r.can_pull(100));
        assert!(!r.can_pull(101));
    }

    #[test]
    fn strong_decimation_attenuates_ultrasound() {
        // Un micro 96 kHz qui capte de l'ultrason (42 kHz) : sans pré-filtre,
        // la décimation le repliait à 6 kHz, en pleine bande vocale, à
        // pleine puissance (RMS ~0,7). Le pré-moyennage doit l'écraser.
        let input = sine(42_000.0, 96_000.0, 9600);
        let mut r = CubicResampler::new(96_000.0 / 48_000.0);
        r.push(&input);
        assert!(r.can_pull(2000));
        let mut out = vec![0f32; 2000];
        r.pull(&mut out);
        let rms = (out.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>()
            / out.len() as f64)
            .sqrt();
        assert!(rms < 0.25, "repliement insuffisamment atténué : RMS {rms:.3}");
    }

    #[test]
    fn cubic_beats_linear_on_a_sine() {
        // Le cas qui fait mal en production : 44,1 kHz -> 48 kHz.
        const IN_RATE: f64 = 44_100.0;
        const OUT_RATE: f64 = 48_000.0;
        const FREQ: f64 = 1_000.0;
        const OUT_LEN: usize = 4000;
        let ratio = IN_RATE / OUT_RATE;

        let input = sine(FREQ, IN_RATE, 4410);
        let mut r = CubicResampler::new(ratio);
        r.push(&input);
        assert!(r.can_pull(OUT_LEN));
        let mut cubic = vec![0f32; OUT_LEN];
        r.pull(&mut cubic);

        let linear = linear_reference(&input, ratio, OUT_LEN);
        // Vérité terrain : la même sinusoïde échantillonnée directement à la
        // fréquence de sortie.
        let ideal = sine(FREQ, OUT_RATE, OUT_LEN);

        let e_cubic = rms_error(&cubic, &ideal);
        let e_linear = rms_error(&linear, &ideal);
        // Mesuré : 2,5e-5 en cubique contre 1,3e-3 en linéaire, soit ~53 fois
        // moins d'erreur. Le seuil est volontairement lâche (facteur 10).
        assert!(
            e_cubic * 10.0 < e_linear,
            "erreur RMS cubique {e_cubic:.3e}, linéaire {e_linear:.3e} : pas le gain attendu"
        );
    }

    #[test]
    fn block_boundaries_are_seamless() {
        // Découper l'entrée et la sortie ne doit rien changer : c'est ce que
        // garantit l'échantillon de passé conservé d'un `pull` à l'autre.
        const OUT_LEN: usize = 1024;
        const BLOCK: usize = 64;
        let ratio = 44_100.0 / 48_000.0;
        let input = sine(1_000.0, 44_100.0, 2000);

        let mut chunked = CubicResampler::new(ratio);
        let mut produced: Vec<f32> = Vec::with_capacity(OUT_LEN);
        let mut fed = 0usize;
        while produced.len() < OUT_LEN {
            if !chunked.can_pull(BLOCK) {
                let end = (fed + 137).min(input.len());
                assert!(end > fed, "entrée épuisée avant la fin du test");
                chunked.push(&input[fed..end]);
                fed = end;
                continue;
            }
            let mut block = [0f32; BLOCK];
            chunked.pull(&mut block);
            produced.extend_from_slice(&block);
        }

        let mut whole = CubicResampler::new(ratio);
        whole.push(&input);
        let mut reference = vec![0f32; OUT_LEN];
        assert!(whole.can_pull(OUT_LEN));
        whole.pull(&mut reference);

        let worst = produced
            .iter()
            .zip(&reference)
            .map(|(&a, &b)| (a - b).abs())
            .fold(0f32, f32::max);
        assert!(worst < 1e-5, "les coutures de blocs dévient de {worst:.3e}");

        // Aucun saut anormal non plus : à 1 kHz sur 48 kHz, le pas maximal
        // entre deux échantillons vaut 2·pi·1000/48000 ≈ 0,131.
        let max_step = produced.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
        assert!(max_step < 0.2, "discontinuité : pas de {max_step:.3} entre deux échantillons");
    }
}
