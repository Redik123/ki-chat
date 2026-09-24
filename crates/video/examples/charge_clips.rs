//! Banc de charge de l'enregistreur de clips : ce que la capture et
//! l'encodage coûtent à la machine, mesuré par Windows lui-même — le
//! processeur (utilisateur et noyau), les défauts de page, la mémoire
//! privée et vidéo, et les moteurs de la carte que le processus occupe
//! (3D, copie, encodeur, CUDA). De quoi comparer deux chemins sur la même
//! machine, devant le même écran.
//!
//!     cargo run -p ki-video --example charge_clips --release -- ancien 60
//!     cargo run -p ki-video --example charge_clips --release -- gpu 60 [pid de dwm]
//!
//! « ancien » : la boucle du partage d'écran réglée en clip (capture lue
//! par le processeur, conversion, NVENC) ; « gpu » : la chaîne des clips
//! qui ne quitte pas la carte. Le pid de dwm.exe, s'il est donné, ajoute
//! ce que le compositeur dépense pour nous fournir les images.

#[cfg(not(windows))]
fn main() {
    eprintln!("banc de charge : Windows seulement");
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    banc::lancer()
}

#[cfg(windows)]
mod banc {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use windows::core::{Interface, HSTRING};
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIAdapter3, IDXGIFactory1, DXGI_MEMORY_SEGMENT_GROUP_LOCAL,
        DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO,
    };
    use windows::Win32::System::Performance::*;
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    use ki_video::{CaptureSource, EncodedFrame, EncoderChoice, Profil, StageStats, StreamConfig, StreamerLoop};

    pub fn lancer() -> anyhow::Result<()> {
        let args: Vec<String> = std::env::args().collect();
        let mode = args.get(1).cloned().unwrap_or_else(|| "ancien".into());
        let secondes: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(60);
        let dwm: Option<u32> = args.get(3).and_then(|s| s.parse().ok());

        let stats = Arc::new(StageStats::default());
        let octets = Arc::new(AtomicU64::new(0));
        let images = Arc::new(AtomicU64::new(0));
        // KI_BANC_H264=fichier.h264 : le flux encodé, tel quel, pour
        // l'inspecter (ffprobe, ffmpeg) — couleurs déclarées, images.
        let vidage = std::env::var_os("KI_BANC_H264")
            .and_then(|p| std::fs::File::create(p).ok())
            .map(|f| Arc::new(std::sync::Mutex::new(f)));
        let emit: ki_video::FrameEmit = {
            let (octets, images) = (octets.clone(), images.clone());
            Arc::new(move |f: EncodedFrame| {
                octets.fetch_add(f.data.len() as u64, Ordering::Relaxed);
                images.fetch_add(1, Ordering::Relaxed);
                if let Some(v) = &vidage {
                    use std::io::Write;
                    let _ = v.lock().unwrap().write_all(&f.data);
                }
            })
        };
        let origine = Instant::now();
        let arreter: Box<dyn FnOnce()> = match mode.as_str() {
            "ancien" => {
                let config = StreamConfig {
                    source: CaptureSource::Monitor(0),
                    max_height: 1080,
                    fps: 60,
                    bitrate_bps: 12_000_000,
                    cursor: true,
                    preview: false,
                    encoder: EncoderChoice::Auto,
                    gop_s: 1,
                    profil: Profil::Clip,
                };
                let boucle = StreamerLoop::start(
                    stats.clone(),
                    Arc::new(|_| {}),
                    emit,
                    config,
                    Arc::new(AtomicBool::new(false)),
                    origine,
                    None,
                )?;
                Box::new(move || boucle.stop())
            }
            "gpu" => {
                let config = ki_video::ConfigClip {
                    source: CaptureSource::Monitor(0),
                    hauteur_max: 1080,
                    fps: 60,
                    debit_bps: 12_000_000,
                    curseur: true,
                    gop_s: 1,
                };
                let chaine = ki_video::ClipGpu::demarrer(config, stats.clone(), emit, origine)?;
                Box::new(move || chaine.arreter())
            }
            // Rien : ce que la machine fait sans nous, pour comparer.
            "rien" => Box::new(|| ()),
            autre => anyhow::bail!("mode inconnu : {autre} (ancien | gpu | rien)"),
        };

        let mut mesure = Mesure::new(std::process::id(), dwm);
        println!("banc « {mode} » : {secondes} s, écran principal, 1080p60, 12 Mbit/s");
        let mut t = 0;
        let mut images_avant = 0;
        while t < secondes {
            std::thread::sleep(Duration::from_secs(5));
            t += 5;
            let n = images.load(Ordering::Relaxed);
            println!(
                "t+{t:>3}s  {:>4.1} i/s  enc {:>4.1} ms  conv {:>4.1} ms  | {}",
                (n - images_avant) as f32 / 5.0,
                stats.encode_ms.get(),
                stats.convert_ms.get(),
                mesure.ligne()
            );
            images_avant = n;
        }
        arreter();
        println!(
            "fin : {} images, {:.1} Mo, {:.0} kbit/s",
            images.load(Ordering::Relaxed),
            octets.load(Ordering::Relaxed) as f64 / 1e6,
            octets.load(Ordering::Relaxed) as f64 * 8.0 / 1000.0 / origine.elapsed().as_secs_f64()
        );
        Ok(())
    }

    fn cent_ns(f: FILETIME) -> u64 {
        (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime)
    }

    /// Les compteurs du processus, de la carte et du système, relus à
    /// chaque ligne ; les écarts font les débits.
    struct Mesure {
        avant: Instant,
        cpu: (u64, u64),
        defauts: u32,
        requete: PDH_HQUERY,
        moteurs: PDH_HCOUNTER,
        moteurs_dwm: Option<PDH_HCOUNTER>,
        zeros: PDH_HCOUNTER,
        carte: Option<IDXGIAdapter3>,
    }

    impl Mesure {
        fn new(pid: u32, dwm: Option<u32>) -> Self {
            let mut requete = PDH_HQUERY::default();
            let mut moteurs = PDH_HCOUNTER::default();
            let mut moteurs_dwm = PDH_HCOUNTER::default();
            let mut zeros = PDH_HCOUNTER::default();
            unsafe {
                PdhOpenQueryW(None, 0, &mut requete);
                let chemin = HSTRING::from(format!("\\GPU Engine(pid_{pid}_*)\\Utilization Percentage"));
                PdhAddEnglishCounterW(requete, &chemin, 0, &mut moteurs);
                if let Some(d) = dwm {
                    let chemin = HSTRING::from(format!("\\GPU Engine(pid_{d}_*)\\Utilization Percentage"));
                    PdhAddEnglishCounterW(requete, &chemin, 0, &mut moteurs_dwm);
                }
                PdhAddEnglishCounterW(requete, &HSTRING::from("\\Memory\\Demand Zero Faults/sec"), 0, &mut zeros);
                PdhCollectQueryData(requete);
            }
            let carte = unsafe {
                CreateDXGIFactory1::<IDXGIFactory1>().ok().and_then(|f| {
                    let mut i = 0;
                    while let Ok(a) = f.EnumAdapters1(i) {
                        i += 1;
                        if a.GetDesc1().is_ok_and(|d| d.VendorId == 0x10DE) {
                            return a.cast::<IDXGIAdapter3>().ok();
                        }
                    }
                    None
                })
            };
            let mut m = Self {
                avant: Instant::now(),
                cpu: (0, 0),
                defauts: 0,
                requete,
                moteurs,
                moteurs_dwm: dwm.map(|_| moteurs_dwm),
                zeros,
                carte,
            };
            let (cpu, defauts, _, _) = m.processus();
            m.cpu = cpu;
            m.defauts = defauts;
            m
        }

        /// (noyau, utilisateur) en centaines de ns, défauts de page,
        /// mémoire privée, ensemble de travail.
        fn processus(&self) -> ((u64, u64), u32, usize, usize) {
            unsafe {
                let p = GetCurrentProcess();
                let (mut c, mut e, mut k, mut u) = Default::default();
                let _ = GetProcessTimes(p, &mut c, &mut e, &mut k, &mut u);
                let mut mem = PROCESS_MEMORY_COUNTERS_EX {
                    cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
                    ..Default::default()
                };
                let _ = GetProcessMemoryInfo(
                    p,
                    &mut mem as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
                    mem.cb,
                );
                ((cent_ns(k), cent_ns(u)), mem.PageFaultCount, mem.PrivateUsage, mem.WorkingSetSize)
            }
        }

        /// Les moteurs de la carte, par type (3D, Copy, VideoEncode, Cuda…),
        /// en pour cent, sommés sur les instances.
        fn moteurs(&self, compteur: PDH_HCOUNTER) -> BTreeMap<String, f64> {
            let mut sortie = BTreeMap::new();
            unsafe {
                let (mut taille, mut n) = (0u32, 0u32);
                let st = PdhGetFormattedCounterArrayW(compteur, PDH_FMT_DOUBLE, &mut taille, &mut n, None);
                if st != PDH_MORE_DATA || taille == 0 {
                    return sortie;
                }
                let mut tampon = vec![0u8; taille as usize];
                let items = tampon.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
                if PdhGetFormattedCounterArrayW(compteur, PDH_FMT_DOUBLE, &mut taille, &mut n, Some(items)) != 0 {
                    return sortie;
                }
                for i in 0..n as usize {
                    let item = &*items.add(i);
                    let nom = item.szName.to_string().unwrap_or_default();
                    let genre = nom.rsplit("engtype_").next().unwrap_or("?").to_string();
                    *sortie.entry(genre).or_insert(0.0) += item.FmtValue.Anonymous.doubleValue;
                }
            }
            sortie
        }

        fn ligne(&mut self) -> String {
            unsafe { PdhCollectQueryData(self.requete) };
            let dt = self.avant.elapsed().as_secs_f64().max(0.001);
            self.avant = Instant::now();
            let (cpu, defauts, prive, travail) = self.processus();
            let noyau = (cpu.0 - self.cpu.0) as f64 / 1e7 / dt * 100.0;
            let util = (cpu.1 - self.cpu.1) as f64 / 1e7 / dt * 100.0;
            let fautes = f64::from(defauts.wrapping_sub(self.defauts)) / dt;
            self.cpu = cpu;
            self.defauts = defauts;
            let zeros = unsafe {
                let mut v = PDH_FMT_COUNTERVALUE::default();
                if PdhGetFormattedCounterValue(self.zeros, PDH_FMT_DOUBLE, None, &mut v) == 0 {
                    v.Anonymous.doubleValue
                } else {
                    0.0
                }
            };
            let vram = self.carte.as_ref().map(|a| unsafe {
                let mut local = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
                let mut partage = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
                let _ = a.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut local);
                let _ = a.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL, &mut partage);
                (local.CurrentUsage, partage.CurrentUsage)
            });
            let format_moteurs = |m: BTreeMap<String, f64>| {
                m.into_iter()
                    .filter(|(_, v)| *v >= 0.05)
                    .map(|(k, v)| format!("{k} {v:.1}%"))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            let mut s = format!(
                "cpu {util:>4.1}% + noyau {noyau:>4.1}% (d'un cœur) | défauts {fautes:>6.0}/s (système : zéros {zeros:>6.0}/s) | privée {} Mo, travail {} Mo",
                prive / 1_000_000,
                travail / 1_000_000
            );
            if let Some((l, p)) = vram {
                s += &format!(" | vram {} Mo, partagée {} Mo", l / 1_000_000, p / 1_000_000);
            }
            s += &format!(" | carte : {}", format_moteurs(self.moteurs(self.moteurs)));
            if let Some(d) = self.moteurs_dwm {
                s += &format!(" | dwm : {}", format_moteurs(self.moteurs(d)));
            }
            s
        }
    }
}
