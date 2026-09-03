//! Détection de parole neuronale : Silero VAD, exécuté par tract.
//!
//! Le seuil d'amplitude prend un clavier mécanique ou une respiration pour
//! de la voix ; ce modèle (Silero VAD v5, MIT, entraîné sur des milliers
//! d'heures de parole) rend une probabilité de parole par bloc de 32 ms, et
//! ne se laisse pas avoir par un bruit qui n'a pas la forme d'une voix. Il
//! tourne en 0,1 ms par bloc sur le même moteur d'inférence que
//! DeepFilterNet, sans rien d'autre à installer.
//!
//! Le modèle embarqué est l'export officiel 16 kHz **mis à plat** par
//! `examples/silero_sonde.rs` : l'export TorchScript est truffé de « If »
//! qui ne dépendent que des formes, que tract ne sait pas traduire ; avec
//! nos formes fixes, chaque condition a une réponse connue et le « If » est
//! remplacé par sa branche. L'atelier regénère le fichier si le modèle
//! d'origine change.
//!
//! Le modèle attend du 16 kHz par blocs de 512 échantillons **précédés des
//! 64 derniers du bloc d'avant** (le contexte de l'enveloppe Python de
//! Silero — sans lui, rien ne dépasse 0,2), et garde un état récurrent
//! [2, 1, 128] d'un bloc à l'autre.

use tract_onnx::prelude::*;

const MODELE: &[u8] = include_bytes!("../models/silero/silero_vad_16k_tract.onnx");

/// Échantillons d'un bloc, et de contexte, à 16 kHz.
const BLOC: usize = 512;
const CONTEXTE: usize = 64;
const RATE: i64 = 16_000;

pub struct Silero {
    modele: TypedRunnableModel<TypedModel>,
    /// Le modèle garde-t-il son entrée « fréquence » après optimisation ?
    trois_entrees: bool,
    etat: Tensor,
    contexte: [f32; CONTEXTE],
    /// Le 16 kHz en attente d'un bloc complet.
    tampon: Vec<f32>,
    entree: Vec<f32>,
    /// Dernière probabilité rendue.
    derniere: f32,
}

impl Silero {
    pub fn new() -> anyhow::Result<Self> {
        let mut lecteur = std::io::Cursor::new(MODELE);
        // Les formes de sortie déclarées portent des dimensions symboliques
        // que l'analyse ne sait pas ramener à 1 : on les ignore, elles se
        // déduisent des entrées. La fréquence est une constante.
        let modele = tract_onnx::onnx()
            .with_ignore_output_shapes(true)
            .model_for_read(&mut lecteur)?
            .with_input_fact(0, f32::fact([1, CONTEXTE + BLOC]).into())?
            .with_input_fact(1, f32::fact([2, 1, 128]).into())?
            .with_input_fact(2, InferenceFact::from(tensor0(RATE)))?
            .into_optimized()?
            .into_runnable()?;
        let trois_entrees = modele.model().inputs.len() == 3;
        Ok(Self {
            modele,
            trois_entrees,
            etat: Tensor::zero::<f32>(&[2, 1, 128])?,
            contexte: [0.0; CONTEXTE],
            tampon: Vec::with_capacity(BLOC * 2),
            entree: vec![0.0; CONTEXTE + BLOC],
            derniere: 0.0,
        })
    }

    /// Une trame 48 kHz mono de plus. Rend la probabilité de parole quand un
    /// bloc de 32 ms vient de se compléter, sinon rien — et l'appelant garde
    /// la dernière.
    pub fn traiter(&mut self, trame48: &[f32]) -> Option<f32> {
        // Décimation par trois : la moyenne de trois échantillons est un
        // passe-bas grossier, largement assez pour reconnaître une voix.
        let (triplets, _) = trame48.as_chunks::<3>();
        self.tampon.extend(triplets.iter().map(|c| (c[0] + c[1] + c[2]) / 3.0));
        let mut resultat = None;
        while self.tampon.len() >= BLOC {
            let bloc: Vec<f32> = self.tampon.drain(..BLOC).collect();
            self.entree[..CONTEXTE].copy_from_slice(&self.contexte);
            self.entree[CONTEXTE..].copy_from_slice(&bloc);
            self.contexte.copy_from_slice(&bloc[BLOC - CONTEXTE..]);
            match self.inferer() {
                Ok(p) => {
                    self.derniere = p;
                    resultat = Some(p);
                }
                Err(e) => {
                    tracing::warn!("Silero : {e}");
                    // L'état a pu se corrompre : on repart de zéro.
                    self.etat = Tensor::zero::<f32>(&[2, 1, 128]).unwrap_or_else(|_| self.etat.clone());
                }
            }
        }
        resultat
    }

    fn inferer(&mut self) -> TractResult<f32> {
        let entree = Tensor::from_shape(&[1, CONTEXTE + BLOC], &self.entree)?;
        let mut entrees: TVec<TValue> = tvec!(entree.into(), self.etat.clone().into());
        if self.trois_entrees {
            entrees.push(tensor0(RATE).into());
        }
        let sorties = self.modele.run(entrees)?;
        let p = sorties[0].to_array_view::<f32>()?[[0, 0]];
        self.etat = sorties[1].clone().into_tensor();
        Ok(p)
    }

    /// La dernière probabilité rendue.
    pub fn derniere(&self) -> f32 {
        self.derniere
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Le modèle embarqué se charge, tourne vite, et ne prend ni le silence
    /// ni un bruit blanc pour de la parole.
    #[test]
    fn le_modele_se_charge_et_ignore_le_bruit() {
        let mut vad = Silero::new().expect("modèle Silero");
        let silence = vec![0f32; 960];
        let mut graine: u32 = 7;
        let bruit: Vec<f32> = (0..960)
            .map(|_| {
                graine = graine.wrapping_mul(1664525).wrapping_add(1013904223);
                ((graine >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.2
            })
            .collect();
        let debut = std::time::Instant::now();
        let mut blocs = 0;
        for _ in 0..50 {
            if vad.traiter(&silence).is_some() {
                blocs += 1;
            }
        }
        assert!(blocs >= 30, "{blocs} blocs pour 50 trames de 20 ms");
        assert!(vad.derniere() < 0.1, "silence : {}", vad.derniere());
        for _ in 0..50 {
            vad.traiter(&bruit);
        }
        assert!(vad.derniere() < 0.3, "bruit blanc : {}", vad.derniere());
        let par_trame = debut.elapsed().as_secs_f64() * 1000.0 / 100.0;
        assert!(par_trame < 5.0, "{par_trame:.2} ms par trame de 20 ms");
    }

    /// Validation sur de la vraie parole, à la main : `KI_PAROLE_WAV=chemin
    /// cargo test -p ki-voice silero -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn la_parole_est_reconnue() {
        let chemin = std::env::var("KI_PAROLE_WAV").expect("KI_PAROLE_WAV");
        let pcm = crate::effects::load_wav_file(&chemin).expect("WAV");
        let mut vad = Silero::new().unwrap();
        let (mut hauts, mut blocs) = (0, 0);
        for trame in pcm.as_chunks::<960>().0 {
            if let Some(p) = vad.traiter(trame) {
                blocs += 1;
                hauts += (p >= 0.5) as usize;
            }
        }
        eprintln!("{hauts} blocs de parole sur {blocs}");
        assert!(hauts * 2 > blocs, "moins de la moitié des blocs reconnus");
    }
}
