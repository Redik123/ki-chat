//! Les en-têtes binaires des datagrammes : voix, trames vidéo, son du jeu.
//! Le serveur les lit sur tout ce qu'un client connecté lui envoie.
//!
//! Deux promesses par format : lire n'importe quoi ne panique jamais, et
//! écrire puis relire rend exactement ce qu'on a écrit.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Lecture de n'importe quoi.
    if let Some(p) = ki_protocol::parse_voice_packet(data) {
        assert_eq!(p.payload.len(), data.len() - ki_protocol::VOICE_HEADER_LEN);
    }
    let _ = ki_protocol::parse_media_header(data);
    let _ = ki_protocol::parse_audio_header(data);
    let _ = ki_protocol::is_audio_datagram(data);

    // Aller-retour : les champs sont tirés des octets d'entrée.
    let mut champs = [0u8; 32];
    for (i, c) in champs.iter_mut().enumerate() {
        *c = *data.get(i).unwrap_or(&0);
    }
    let u64_a = |at: usize| u64::from_le_bytes(champs[at..at + 8].try_into().unwrap());
    let u32_a = |at: usize| u32::from_le_bytes(champs[at..at + 4].try_into().unwrap());
    let u16_a = |at: usize| u16::from_le_bytes(champs[at..at + 2].try_into().unwrap());

    let mut buf = [0u8; 40];
    ki_protocol::write_voice_header(&mut buf, u64_a(0), u64_a(8));
    let p = ki_protocol::parse_voice_packet(&buf).expect("un en-tête voix écrit se relit");
    assert_eq!((p.id, p.counter), (u64_a(0), u64_a(8)));

    let media = ki_protocol::MediaHeader {
        idr: champs[16] & 1 != 0,
        stream_id: u32_a(0),
        seq: u64_a(8),
        pts_us: u64_a(16),
        group_id: u32_a(24),
        width: u16_a(28),
        height: u16_a(30),
    };
    ki_protocol::write_media_header(&mut buf, &media);
    assert_eq!(ki_protocol::parse_media_header(&buf), Some(media));

    let audio = ki_protocol::AudioHeader { stream_id: u32_a(0), seq: u64_a(8), pts_us: u64_a(16) };
    ki_protocol::write_audio_header(&mut buf, &audio);
    assert_eq!(ki_protocol::parse_audio_header(&buf), Some(audio));
    assert!(ki_protocol::is_audio_datagram(&buf));

    // Les nonces de deux paquets distincts ne se confondent pas : c'est ce
    // qui tient le chiffrement debout.
    let n1 = ki_protocol::nonce_for_media(champs[0], u32_a(4), u64_a(8));
    let n2 = ki_protocol::nonce_for_media(champs[0], u32_a(4), u64_a(8).wrapping_add(1));
    assert_ne!(n1, n2);
});
