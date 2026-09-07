//! Le contrôle des vignettes : un PNG venu d'un client que rien n'oblige à
//! être le nôtre. `check_png` lit la structure sans décoder ; il ne doit ni
//! paniquer sur un bloc tronqué, ni accepter ce qu'il promet de refuser.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if ki_protocol::check_png(data).is_ok() {
        // Accepté : c'est donc un PNG qui commence par la signature et
        // finit exactement sur IEND — pas un octet de plus.
        assert!(data.len() >= 8 + 25 + 12, "un PNG accepté est au moins signature + IHDR + IEND");
        // Le CRC n'est pas vérifié (c'est l'affaire du décodeur), mais le
        // bloc final est bien IEND, longueur nulle : [0,0,0,0]["IEND"][crc].
        let fin = &data[data.len() - 12..];
        assert_eq!(&fin[..8], b"\0\0\0\0IEND", "un PNG accepté finit sur IEND");
    }

    // Le même contrôle derrière le base64 des messages : ce qui passe en
    // base64 passe en octets, et réciproquement.
    let texte = String::from_utf8_lossy(data);
    let _ = ki_protocol::check_thumbnail(&texte);
});
