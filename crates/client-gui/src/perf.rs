//! Mesure du coût de l'interface, en direct et depuis l'application elle-même.
//!
//! # Pourquoi ça vit ici et pas dans un profileur
//!
//! Le client tourne à côté d'un jeu, sur trente machines qu'on ne verra
//! jamais. Un profileur répond « sur MA machine, au repos » — ce qui est
//! justement la situation qui ne pose aucun problème. Ce module répond
//! « chez toi, pendant ta partie », et se copie-colle comme le journal audio.
//!
//! # Ce qu'on mesure, et pourquoi ces trois-là
//!
//! - **le temps d'une image** : ce que `update()` coûte de bout en bout ;
//! - **le temps du fil de discussion** : la part qui croît avec le nombre de
//!   messages chargés, et que P3.3 doit rendre constante ;
//! - **les images par seconde réellement peintes** : aujourd'hui vingt, quoi
//!   qu'il arrive et même fenêtre réduite. C'est le chiffre que P3.2 doit
//!   faire tomber au repos — et que le partage d'écran devra faire monter.
//!
//! On garde des **quantiles**, pas des moyennes : une moyenne de 3 ms cache
//! une image sur vingt à 40 ms, et c'est exactement celle-là qui se voit.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Fenêtre d'observation : 600 images, soit trente secondes au rythme
/// inconditionnel actuel de 20 images par seconde. Assez pour qu'un à-coup
/// reste visible le temps d'ouvrir les réglages, assez court pour qu'un
/// correctif se constate tout de suite.
const FENETRE: usize = 600;

/// Série de durées bornée, dont on tire des quantiles.
#[derive(Default)]
struct Serie {
    ms: VecDeque<f32>,
}

impl Serie {
    fn push(&mut self, ms: f32) {
        if self.ms.len() == FENETRE {
            self.ms.pop_front();
        }
        self.ms.push_back(ms);
    }

    /// p50, p95 et maximum, en millisecondes. `None` tant qu'on n'a rien vu.
    ///
    /// Le tri se fait sur une copie, à la demande : cette fonction n'est
    /// appelée que quand le panneau est ouvert, jamais sur le chemin d'une
    /// image ordinaire.
    fn quantiles(&self) -> Option<(f32, f32, f32)> {
        if self.ms.is_empty() {
            return None;
        }
        let mut tri: Vec<f32> = self.ms.iter().copied().collect();
        tri.sort_by(|a, b| a.total_cmp(b));
        let at = |q: f32| tri[((tri.len() - 1) as f32 * q).round() as usize];
        Some((at(0.50), at(0.95), tri[tri.len() - 1]))
    }
}

/// Compteurs d'allocations, quand la mesure est demandée à la compilation.
///
/// Une incrémentation atomique par allocation, ce n'est pas gratuit : sur
/// une ligne de cache que se disputent le fil de l'interface, celui du
/// réseau et **celui de l'audio**, c'est même le genre de partage qui coûte
/// précisément là où il ne faut pas. D'où le drapeau : le binaire livré ne
/// porte pas ce compteur, et `cargo run -p ki-client-gui --features mesures`
/// le rend quand on en a besoin.
#[cfg(feature = "mesures")]
pub mod alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicU64, Ordering};

    pub static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

    pub struct Compteur;

    // SAFETY: on délègue tout à `System`, sans toucher aux pointeurs ni aux
    // dispositions. Le compteur n'est qu'un effet de bord.
    unsafe impl GlobalAlloc for Compteur {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            unsafe { System.realloc(ptr, layout, new_size) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            unsafe { System.alloc_zeroed(layout) }
        }
    }

    pub fn total() -> u64 {
        ALLOCATIONS.load(Ordering::Relaxed)
    }
}

#[derive(Default)]
pub struct Perf {
    image: Serie,
    fil: Serie,
    /// Début de l'image en cours.
    debut_image: Option<Instant>,
    /// Début de la mise en page du fil, dans l'image en cours.
    debut_fil: Option<Instant>,
    /// Messages effectivement parcourus par le fil à la dernière image. Sous
    /// P3.3 (virtualisation), ce nombre doit cesser de suivre le total.
    messages_rendus: usize,
    /// Messages en mémoire à la dernière image, pour la comparaison.
    messages_charges: usize,
    /// Comptage des images pour la cadence réelle.
    images: u32,
    depuis: Option<Instant>,
    cadence: f32,
    /// Allocations relevées au début de l'image en cours.
    #[cfg(feature = "mesures")]
    alloc_debut: u64,
    #[cfg(feature = "mesures")]
    alloc_par_image: u64,
}

impl Perf {
    pub fn debut_image(&mut self) {
        let maintenant = Instant::now();
        self.debut_image = Some(maintenant);
        #[cfg(feature = "mesures")]
        {
            self.alloc_debut = alloc::total();
        }

        // Cadence réelle : on ne divise pas par le délai demandé, on compte
        // ce qui est arrivé. Les deux diffèrent dès que la machine peine —
        // et c'est cet écart qui intéresse.
        let depuis = *self.depuis.get_or_insert(maintenant);
        self.images += 1;
        let ecoule = maintenant.duration_since(depuis);
        if ecoule >= Duration::from_secs(1) {
            self.cadence = self.images as f32 / ecoule.as_secs_f32();
            self.images = 0;
            self.depuis = Some(maintenant);
        }
    }

    pub fn fin_image(&mut self) {
        if let Some(debut) = self.debut_image.take() {
            self.image.push(debut.elapsed().as_secs_f32() * 1000.0);
        }
        #[cfg(feature = "mesures")]
        {
            self.alloc_par_image = alloc::total().saturating_sub(self.alloc_debut);
        }
    }

    pub fn debut_fil(&mut self) {
        self.debut_fil = Some(Instant::now());
    }

    /// Fin de la mise en page du fil. `rendus` est le nombre de messages
    /// réellement parcourus, `charges` le nombre en mémoire : tant que les
    /// deux sont égaux, le fil n'est pas virtualisé.
    pub fn fin_fil(&mut self, rendus: usize, charges: usize) {
        if let Some(debut) = self.debut_fil.take() {
            self.fil.push(debut.elapsed().as_secs_f32() * 1000.0);
        }
        self.messages_rendus = rendus;
        self.messages_charges = charges;
    }

    /// Le relevé, tel qu'il s'affiche et se copie. Une ligne par grandeur,
    /// pour que ça reste lisible collé dans un message.
    pub fn lignes(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let fmt = |q: Option<(f32, f32, f32)>| match q {
            Some((p50, p95, max)) => {
                format!("p50 {p50:.2} ms · p95 {p95:.2} ms · max {max:.2} ms")
            }
            None => "—".into(),
        };
        out.push(("Image complète".into(), fmt(self.image.quantiles())));
        out.push(("Fil de discussion".into(), fmt(self.fil.quantiles())));
        out.push((
            "Messages parcourus".into(),
            format!("{} sur {} chargés", self.messages_rendus, self.messages_charges),
        ));
        out.push((
            "Images par seconde".into(),
            if self.cadence > 0.0 {
                format!("{:.1} réellement peintes", self.cadence)
            } else {
                "—".into()
            },
        ));
        #[cfg(feature = "mesures")]
        out.push((
            "Allocations par image".into(),
            format!("{}", self.alloc_par_image),
        ));
        #[cfg(not(feature = "mesures"))]
        out.push((
            "Allocations par image".into(),
            "non compilé (--features mesures)".into(),
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Les quantiles doivent voir l'à-coup qu'une moyenne noierait : c'est
    /// toute la raison de ne pas garder une moyenne.
    #[test]
    fn le_pic_survit_aux_quantiles() {
        let mut s = Serie::default();
        for _ in 0..99 {
            s.push(3.0);
        }
        s.push(40.0);
        let (p50, _, max) = s.quantiles().expect("série non vide");
        assert_eq!(p50, 3.0, "le régime ordinaire reste le régime ordinaire");
        assert_eq!(max, 40.0, "l'image lente est celle qu'on vient chercher");
    }

    /// La fenêtre est bornée : une session de plusieurs heures ne doit pas
    /// faire grandir la mesure elle-même.
    #[test]
    fn la_fenetre_ne_grandit_pas() {
        let mut s = Serie::default();
        for i in 0..FENETRE * 3 {
            s.push(i as f32);
        }
        assert_eq!(s.ms.len(), FENETRE);
        // Et ce sont bien les dernières qui restent.
        assert_eq!(*s.ms.back().unwrap(), (FENETRE * 3 - 1) as f32);
    }

    /// Sans une seule image mesurée, on ne prétend rien.
    #[test]
    fn rien_a_dire_avant_la_premiere_image() {
        assert!(Serie::default().quantiles().is_none());
        let lignes = Perf::default().lignes();
        assert!(lignes.iter().any(|(k, v)| k == "Image complète" && v == "—"));
    }
}
