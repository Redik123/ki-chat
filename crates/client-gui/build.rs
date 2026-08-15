//! Grave dans l'exécutable ce qui fait une application Windows présentable :
//! son icône (Explorateur, barre des tâches, raccourcis), le bloc de version
//! lu par les propriétés du fichier, et un manifeste.
//!
//! L'icône n'est pas un fichier du dépôt : elle est rendue ici, aux sept
//! tailles que réclame le shell, par le même code que l'icône de fenêtre
//! (`src/appicon.rs`). Un seul dessin, aucune image à régénérer à la main
//! quand la marque bouge.

// Inclus hors du crate : ce module ne dépend de rien, précisément pour ça.
// Il expose aussi ses couleurs, dont le script n'a pas l'usage.
#[allow(dead_code)]
#[path = "src/appicon.rs"]
mod appicon;

use std::path::PathBuf;

/// Tailles gravées dans le `.ico`. Windows pioche la plus proche de ce qu'il
/// affiche ; sans le 256, l'affichage « grandes icônes » interpole une
/// vignette floue.
const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/appicon.rs");

    // Les ressources Win32 n'ont de sens que pour une cible Windows.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let icon = out.join("ki-chat.ico");
    std::fs::write(&icon, ico(&SIZES)).expect("écriture de l'icône");

    let manifest = out.join("ki-chat.manifest");
    std::fs::write(&manifest, MANIFEST).expect("écriture du manifeste");

    let mut res = winresource::WindowsResource::new();
    res.set_icon(icon.to_str().expect("chemin d'icône"));
    res.set_manifest_file(manifest.to_str().expect("chemin de manifeste"));
    res.set("ProductName", "ki-chat");
    res.set("FileDescription", "ki-chat — chat et vocal privés");
    res.set("CompanyName", "ki-chat");
    res.set("LegalCopyright", "MIT");
    res.set("OriginalFilename", "ki-chat.exe");
    // Un échec ici passerait inaperçu : on livrerait un exécutable sans
    // icône ni manifeste, ce qui ne se voit qu'une fois chez l'utilisateur.
    res.compile()
        .expect("compilation des ressources Windows (rc.exe du SDK introuvable ?)");
}

/// Manifeste applicatif.
///
/// Trois déclarations qui changent quelque chose de visible :
/// `asInvoker` (pas d'élévation — l'application s'installe et se met à jour
/// dans le profil utilisateur, jamais dans `Program Files`), la conscience du
/// DPI par moniteur (sans quoi Windows étire la fenêtre sur un écran 4K, et
/// tout devient flou), et UTF-8 comme page de code du processus, pour que les
/// chemins accentués passent les API ANSI intactes.
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <activeCodePage xmlns="http://schemas.microsoft.com/SMI/2019/WindowsSettings">UTF-8</activeCodePage>
    </windowsSettings>
  </application>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}" />
    </application>
  </compatibility>
</assembly>
"#;

/// Assemble un `.ico` : un en-tête, un descripteur par taille, puis les
/// images elles-mêmes.
fn ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&s| dib(s)).collect();

    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // réservé
    out.extend_from_slice(&1u16.to_le_bytes()); // type : icône
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());

    // Les images suivent la table des descripteurs, 16 octets chacun.
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&size, image) in sizes.iter().zip(&images) {
        // Le champ ne fait qu'un octet : 256 s'y code par 0.
        let dim = if size >= 256 { 0 } else { size as u8 };
        out.push(dim);
        out.push(dim);
        out.push(0); // couleurs de palette : aucune, on est en truecolor
        out.push(0); // réservé
        out.extend_from_slice(&1u16.to_le_bytes()); // plans
        out.extend_from_slice(&32u16.to_le_bytes()); // bits par pixel
        out.extend_from_slice(&(image.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += image.len() as u32;
    }

    for image in images {
        out.extend_from_slice(&image);
    }
    out
}

/// Une image d'icône, au format DIB 32 bits attendu dans un `.ico`.
fn dib(size: u32) -> Vec<u8> {
    let rgba = appicon::render(size);
    let side = size as usize;
    let xor = side * side * 4;
    // Masque monochrome : lignes alignées sur 4 octets.
    let mask_row = size.div_ceil(32) as usize * 4;
    let and = mask_row * side;

    let mut out = Vec::with_capacity(40 + xor + and);
    out.extend_from_slice(&40u32.to_le_bytes()); // taille de l'en-tête
    out.extend_from_slice(&(size as i32).to_le_bytes()); // largeur
    // Hauteur doublée : la convention du format, qui compte l'image ET le
    // masque empilés dans le même bitmap.
    out.extend_from_slice(&((size * 2) as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // plans
    out.extend_from_slice(&32u16.to_le_bytes()); // bits par pixel
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB, non compressé
    out.extend_from_slice(&((xor + and) as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 16]); // résolution et palette : sans objet

    // Les pixels, en BGRA et de bas en haut.
    for y in (0..side).rev() {
        for x in 0..side {
            let i = (y * side + x) * 4;
            out.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    // Masque AND laissé à zéro : la transparence vient du canal alpha, que
    // tout Windows encore supporté sait lire.
    out.resize(out.len() + and, 0);
    out
}
