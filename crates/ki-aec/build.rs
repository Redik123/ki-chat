//! Télécharge le tarball officiel de SpeexDSP, vérifie son empreinte
//! SHA-256, puis compile les six fichiers C de l'annulateur d'écho (MDF) et
//! de la suppression de résidu — sans autotools : le seul header que le
//! configure aurait généré est fabriqué ici.
//!
//! Hors-ligne : poser `KI_SPEEXDSP_SRC` sur un dossier contenant les sources
//! déjà extraites court-circuite le téléchargement (même convention que
//! `KI_OPUS_SRC`).

use std::io::Read;
use std::path::PathBuf;

// speexdsp 1.2.1 : dernière stable. Empreinte calculée sur le tarball de
// downloads.xiph.org au moment de l'épinglage.
const URL: &str = "https://downloads.xiph.org/releases/speex/speexdsp-1.2.1.tar.gz";
const SHA256: &str = "8c777343e4a6399569c72abc38a95b24db56882c83dbdb6c6424a5f4aeb54d3d";
const DIR: &str = "speexdsp-1.2.1"; // nom du dossier dans le tarball

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));

    let src = match std::env::var("KI_SPEEXDSP_SRC") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => {
            let extracted = out.join(DIR);
            if !extracted.join("libspeexdsp").join("mdf.c").exists() {
                let bytes = download_verified();
                let gz = flate2::read::GzDecoder::new(&bytes[..]);
                tar::Archive::new(gz)
                    .unpack(&out)
                    .expect("extraction du tarball speexdsp");
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
        .file(dsp.join("mdf.c"))
        .file(dsp.join("preprocess.c"))
        .file(dsp.join("filterbank.c"))
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

fn download_verified() -> Vec<u8> {
    let mut bytes = Vec::new();
    ureq::get(URL)
        .call()
        .expect("téléchargement du tarball speexdsp (réseau requis au premier build)")
        .into_reader()
        .read_to_end(&mut bytes)
        .expect("lecture du tarball");
    use sha2::Digest;
    let digest: String =
        sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        digest, SHA256,
        "empreinte SHA-256 du tarball speexdsp inattendue — téléchargement corrompu ou altéré"
    );
    bytes
}
