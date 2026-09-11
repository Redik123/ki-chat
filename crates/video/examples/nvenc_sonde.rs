//! Sonde NVENC : les deux chemins d'entrée (texture Direct3D, tampon
//! historique) sur la carte de cette machine, dix images chacun.
//!
//!     cargo run -p ki-video --example nvenc_sonde --release
fn main() {
    println!("{}", ki_video::sonde());
}
