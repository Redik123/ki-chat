//! Télécharge le tarball officiel de SpeexDSP, vérifie son empreinte
//! SHA-256, puis compile les quatre fichiers C de l'annulateur d'écho (MDF)
//! et de sa FFT — sans autotools : le seul header que le configure aurait
//! généré est fabriqué ici.
//!
//! Hors-ligne : poser `KI_SPEEXDSP_SRC` sur un dossier contenant les sources
//! déjà extraites court-circuite le téléchargement (même convention que
//! `KI_OPUS_SRC`).

use std::io::Read;
use std::path::{Path, PathBuf};

// speexdsp 1.2.1 : dernière stable. Empreinte calculée sur le tarball de
// downloads.xiph.org au moment de l'épinglage.
const URL: &str = "https://downloads.xiph.org/releases/speex/speexdsp-1.2.1.tar.gz";
const SHA256: &str = "8c777343e4a6399569c72abc38a95b24db56882c83dbdb6c6424a5f4aeb54d3d";
const DIR: &str = "speexdsp-1.2.1"; // nom du dossier dans le tarball
/// La même, telle que la déclare le `configure.ac` des sources.
const VERSION: &str = "1.2.1";
/// Posée une fois les sources extraites en entier (voir `extraire`).
const MARQUE: &str = ".ki-extraction-complete";

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));

    let src = match std::env::var("KI_SPEEXDSP_SRC") {
        Ok(dir) => {
            let dir = PathBuf::from(dir);
            verifier_version(&dir);
            dir
        }
        Err(_) => {
            let extracted = out.join(DIR);
            if !extracted.join(MARQUE).exists() {
                extraire(&out, &extracted);
            }
            extracted
        }
    };

    // Le header de types que le configure autotools aurait produit : les
    // sources incluent <speex/speexdsp_config_types.h>, qui n'existe que
    // sous forme de gabarit `.in`. stdint fait l'affaire partout.
    let gen = out.join("gen").join("speex");
    std::fs::create_dir_all(&gen).expect("dossier des headers générés");
    std::fs::write(
        gen.join("speexdsp_config_types.h"),
        "#ifndef __SPEEX_TYPES_H__\n\
         #define __SPEEX_TYPES_H__\n\
         #include <stdint.h>\n\
         typedef int16_t spx_int16_t;\n\
         typedef uint16_t spx_uint16_t;\n\
         typedef int32_t spx_int32_t;\n\
         typedef uint32_t spx_uint32_t;\n\
         #endif\n",
    )
    .expect("écriture du header de types");

    let dsp = src.join("libspeexdsp");
    cc::Build::new()
        .include(src.join("include"))
        .include(&dsp)
        .include(out.join("gen"))
        // Virgule flottante et FFT embarquée : la configuration portable de
        // référence, celle des paquets Linux.
        .define("FLOATING_POINT", None)
        .define("USE_KISS_FFT", None)
        .define("EXPORT", Some(""))
        // M_PI sous MSVC.
        .define("_USE_MATH_DEFINES", None)
        // Le filtre seul : preprocess.c et filterbank.c (la suppression de
        // résidu) ne sont plus appelés, voir src/lib.rs.
        .file(dsp.join("mdf.c"))
        .file(dsp.join("fftwrap.c"))
        .file(dsp.join("kiss_fft.c"))
        .file(dsp.join("kiss_fftr.c"))
        // Du DSP par trame de 20 ms : optimisé même quand nous compilons en
        // debug, comme le reste de la chaîne audio.
        .opt_level(2)
        .warnings(false)
        .compile("speexdsp_aec");

    println!("cargo:rerun-if-env-changed=KI_SPEEXDSP_SRC");
}

/// Des sources fournies à la main doivent être celles de la version épinglée,
/// celle que les tests de l'annulateur ont validée.
fn verifier_version(dir: &Path) {
    let attendu = format!("AC_INIT([speexdsp],[{VERSION}]");
    let lu = std::fs::read_to_string(dir.join("configure.ac")).unwrap_or_default();
    assert!(
        lu.contains(&attendu),
        "KI_SPEEXDSP_SRC ({}) ne contient pas les sources de speexdsp {VERSION} (configure.ac)",
        dir.display()
    );
}

/// Extrait le tarball dans un dossier à part, mis en place d'un seul
/// renommage puis marqué. Extraire en place laissait, après un build
/// interrompu, un arbre à moitié écrit que le test d'existence prenait pour
/// complet : chaque build suivant échouait alors, jusqu'au `cargo clean`.
fn extraire(out: &Path, extracted: &Path) {
    let bytes = download_verified();
    let partiel = out.join("extraction-partielle");
    let _ = std::fs::remove_dir_all(&partiel);
    let _ = std::fs::remove_dir_all(extracted);
    let gz = flate2::read::GzDecoder::new(&bytes[..]);
    tar::Archive::new(gz)
        .unpack(&partiel)
        .expect("extraction du tarball speexdsp");
    std::fs::rename(partiel.join(DIR), extracted).expect("mise en place des sources speexdsp");
    std::fs::write(extracted.join(MARQUE), b"").expect("marque d'extraction complète");
    let _ = std::fs::remove_dir_all(&partiel);
}

fn download_verified() -> Vec<u8> {
    let bytes = telecharger(URL);
    use sha2::Digest;
    let digest: String =
        sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        digest, SHA256,
        "empreinte SHA-256 du tarball speexdsp inattendue — téléchargement corrompu ou altéré"
    );
    bytes
}

/// Télécharge `url` en entier, en réessayant : un runner de CI perd parfois
/// sa connexion TLS au premier essai, et un seul échec réseau ne doit pas
/// coûter une release. Quatre essais, 3 s, 6 s puis 12 s d'attente.
fn telecharger(url: &str) -> Vec<u8> {
    let mut derniere = String::new();
    for essai in 0..4u32 {
        if essai > 0 {
            std::thread::sleep(std::time::Duration::from_secs(3 << (essai - 1)));
        }
        let tentative = ureq::get(url)
            .timeout(std::time::Duration::from_secs(180))
            .call()
            .map_err(|e| e.to_string())
            .and_then(|r| {
                let mut bytes = Vec::new();
                r.into_reader().read_to_end(&mut bytes).map_err(|e| e.to_string())?;
                Ok(bytes)
            });
        match tentative {
            Ok(bytes) => return bytes,
            Err(e) => {
                println!("cargo:warning=téléchargement de {url} raté (essai {}) : {e}", essai + 1);
                derniere = e;
            }
        }
    }
    panic!("téléchargement de {url} impossible après quatre essais (réseau requis au premier build) : {derniere}");
}
