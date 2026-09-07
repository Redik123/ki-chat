//! Les fonctions qui lisent du texte venu d'en face : nettoyage d'un
//! message, affichage borné, extrait, emoji de réaction, hexadécimal.
//!
//! On ne cherche pas seulement le plantage : chaque fonction promet
//! quelque chose (une longueur bornée, un aller-retour exact), et la
//! promesse est vérifiée sur chaque entrée.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Le premier octet règle la borne d'affichage ; le reste est le texte,
    // rendu valide UTF-8 comme le ferait un client qui reçoit des octets.
    let Some((&borne, reste)) = data.split_first() else { return };
    let texte = String::from_utf8_lossy(reste);
    let borne = usize::from(borne);

    if let Ok(propre) = ki_protocol::clean_chat(&texte) {
        assert!(!propre.trim().is_empty(), "clean_chat a accepté du vide");
        assert!(propre.chars().count() <= ki_protocol::MAX_CHAT_TEXT);
        // Ce qui est propre le reste : nettoyer deux fois ne change rien.
        assert_eq!(ki_protocol::clean_chat(&propre).as_deref(), Ok(propre.as_str()));
    }

    let affiche = ki_protocol::safe_display(&texte, borne);
    // Au plus `borne` caractères, plus l'ellipse.
    assert!(affiche.chars().count() <= borne + 1);

    let extrait = ki_protocol::excerpt_of(&texte);
    assert!(extrait.chars().count() <= ki_protocol::MAX_EXCERPT + 1);
    assert!(!extrait.contains('\n'), "un extrait tient sur une ligne");

    if let Some(emoji) = ki_protocol::clean_emoji(&texte) {
        assert!(emoji.len() <= 16 && emoji.chars().count() <= 4);
    }

    // hex : ce qui se décode se réencode à l'identique, et réciproquement.
    if let Some(octets) = ki_protocol::hex_decode(&texte) {
        assert_eq!(
            ki_protocol::hex_encode(&octets),
            texte.to_ascii_lowercase(),
            "hex_decode a accepté ce que hex_encode n'écrirait pas"
        );
    }
    let encode = ki_protocol::hex_encode(reste);
    assert_eq!(ki_protocol::hex_decode(&encode).as_deref(), Some(reste));
});
