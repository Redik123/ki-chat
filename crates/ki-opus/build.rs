//! Télécharge le tarball officiel de libopus, vérifie son empreinte SHA-256,
//! puis le compile en bibliothèque statique avec les fonctions neuronales
//! (DRED, Deep PLC, OSCE) activées.
//!
//! Hors-ligne : poser la variable d'environnement `KI_OPUS_SRC` sur un
//! dossier contenant les sources déjà extraites court-circuite le
//! téléchargement.

use std::io::Read;
use std::path::{Path, PathBuf};

// libopus 1.6.1 (14 janv. 2026) : dernière stable, poids neuronaux inclus
// dans le tarball (rien d'autre à télécharger). Empreinte vérifiée contre le
// SHA256SUMS.txt officiel de downloads.xiph.org.
// ATTENTION : le format DRED est expérimental et verrouillé par version
// (v12 en 1.6.x) — tous les clients doivent embarquer le même libopus.
const OPUS_URL: &str = "https://downloads.xiph.org/releases/opus/opus-1.6.1.tar.gz";
const OPUS_SHA256: &str = "6ffcb593207be92584df15b32466ed64bbec99109f007c82205f0194572411a1";
const OPUS_DIR: &str = "opus-1.6.1"; // nom du dossier dans le tarball
/// La même, telle que l'écrit le fichier `package_version` des sources.
const OPUS_VERSION: &str = "1.6.1";
/// Posée une fois les sources extraites en entier (voir `extraire`).
const MARQUE: &str = ".ki-extraction-complete";

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));

    let src_dir = match std::env::var("KI_OPUS_SRC") {
        Ok(dir) => {
            let dir = PathBuf::from(dir);
            verifier_version(&dir);
            dir
        }
        Err(_) => {
            let extracted = out.join(OPUS_DIR);
            if !extracted.join(MARQUE).exists() {
                extraire(&out, &extracted);
            }
            extracted
        }
    };

    let mut config = cmake::Config::new(&src_dir);
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        android(&mut config);
    }
    let dst = config
        // Les chemins DSP/ML doivent être optimisés même en build debug.
        .profile("Release")
        .define("OPUS_BUILD_SHARED_LIBRARY", "OFF")
        .define("OPUS_BUILD_TESTING", "OFF")
        .define("OPUS_BUILD_PROGRAMS", "OFF")
        // OPUS_DRED implique le Deep PLC (OPUS_DNN) ; OSCE ajoute LACE/NoLACE
        // (amélioration neuronale de la parole à bas débit, complexité >= 6).
        .define("OPUS_DRED", "ON")
        .define("OPUS_OSCE", "ON")
        // MSVC : CRT statique (/MT), aligné sur notre rustflag crt-static.
        .define("OPUS_STATIC_RUNTIME", "ON")
        .build();

    println!("cargo:rustc-link-search=native={}", dst.join("lib").display());
    println!("cargo:rustc-link-lib=static=opus");
    println!("cargo:rerun-if-env-changed=KI_OPUS_SRC");
}

/// Android (cargo ndk) : sous Windows, cmake choisirait Visual Studio. On lui
/// impose Ninja et la toolchain du NDK ; `ANDROID_ABI` fait reconnaître la
/// compilation croisée au crate cmake. Ninja se prend dans le PATH ou, à
/// défaut, dans le cmake du SDK Android.
fn android(config: &mut cmake::Config) {
    let ndk = ["ANDROID_NDK_HOME", "ANDROID_NDK_ROOT", "ANDROID_NDK", "NDK_HOME"]
        .iter()
        .find_map(std::env::var_os)
        .map(PathBuf::from)
        .expect("ANDROID_NDK_HOME (ou NDK_HOME, posé par Tauri) : dossier du NDK requis pour compiler opus pour Android");
    let abi = match std::env::var("CARGO_CFG_TARGET_ARCH").expect("CARGO_CFG_TARGET_ARCH").as_str() {
        "aarch64" => "arm64-v8a",
        "arm" => "armeabi-v7a",
        "x86_64" => "x86_64",
        "x86" => "x86",
        autre => panic!("architecture Android non prise en charge : {autre}"),
    };
    // cargo ndk passe le niveau d'API dans la cible de l'éditeur de liens
    // (« aarch64-linux-android26 ») ; 26 sinon.
    let api: String = std::env::var("_CARGO_NDK_LINK_TARGET")
        .map(|t| t.chars().rev().take_while(char::is_ascii_digit).collect::<String>().chars().rev().collect())
        .ok()
        .filter(|a: &String| !a.is_empty())
        .unwrap_or_else(|| "26".into());
    config
        .generator("Ninja")
        .define("CMAKE_TOOLCHAIN_FILE", ndk.join("build/cmake/android.toolchain.cmake"))
        .define("ANDROID_ABI", abi)
        .define("ANDROID_PLATFORM", format!("android-{api}"));
    if let Some(ninja) = ninja_du_sdk() {
        config.define("CMAKE_MAKE_PROGRAM", ninja);
    }
    println!("cargo:rerun-if-env-changed=ANDROID_NDK_HOME");
    println!("cargo:rerun-if-env-changed=NDK_HOME");
}

/// Le ninja livré avec le cmake du SDK Android, si aucun n'est dans le PATH.
fn ninja_du_sdk() -> Option<PathBuf> {
    let dans_path = std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("ninja.exe").exists() || d.join("ninja").exists()));
    if dans_path {
        return None;
    }
    let sdk = std::env::var_os("ANDROID_HOME")
        .or_else(|| std::env::var_os("ANDROID_SDK_ROOT"))
        .map(PathBuf::from)?;
    let mut versions: Vec<PathBuf> = std::fs::read_dir(sdk.join("cmake")).ok()?.flatten().map(|e| e.path()).collect();
    versions.sort();
    versions
        .into_iter()
        .rev()
        .map(|v| v.join("bin").join(if cfg!(windows) { "ninja.exe" } else { "ninja" }))
        .find(|n| n.exists())
}

/// Des sources fournies à la main doivent être celles de la version épinglée :
/// le format DRED change d'une version à l'autre, et un client compilé sur
/// d'autres sources ne se comprendrait plus avec les autres, sans rien dire.
fn verifier_version(dir: &Path) {
    let attendu = format!("PACKAGE_VERSION=\"{OPUS_VERSION}\"");
    let lu = std::fs::read_to_string(dir.join("package_version")).unwrap_or_default();
    assert!(
        lu.contains(&attendu),
        "KI_OPUS_SRC ({}) ne contient pas les sources de libopus {OPUS_VERSION} \
         (fichier package_version) : le format DRED en dépend",
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
        .expect("extraction du tarball opus");
    std::fs::rename(partiel.join(OPUS_DIR), extracted).expect("mise en place des sources opus");
    std::fs::write(extracted.join(MARQUE), b"").expect("marque d'extraction complète");
    let _ = std::fs::remove_dir_all(&partiel);
}

fn download_verified() -> Vec<u8> {
    let bytes = telecharger(OPUS_URL);
    use sha2::Digest;
    let digest = hex(&sha2::Sha256::digest(&bytes));
    assert_eq!(
        digest, OPUS_SHA256,
        "empreinte SHA-256 du tarball opus inattendue — téléchargement corrompu ou altéré"
    );
    bytes
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
