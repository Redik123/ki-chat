//! Les messages de contrôle, en JSON, une ligne par message. C'est la
//! première chose que le serveur lit d'un client, avant même de savoir qui
//! il est — et le client lit ceux du serveur avec la même confiance limitée.
//!
//! serde_json est robuste ; ce que l'on cherche, ce sont nos propres
//! `Deserialize` : un `#[serde(default)]` oublié, un champ qui refuse ce
//! qu'il écrit lui-même. D'où l'aller-retour : ce qui se lit se réécrit, et
//! se relit à l'identique.
#![no_main]

use libfuzzer_sys::fuzz_target;

fn aller_retour<T>(data: &[u8])
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    if data.len() > ki_protocol::MAX_LINE {
        return;
    }
    if let Ok(msg) = serde_json::from_slice::<T>(data) {
        let ecrit = serde_json::to_string(&msg).expect("un message lu s'écrit");
        let relu: T = serde_json::from_str(&ecrit).expect("un message écrit se relit");
        assert_eq!(serde_json::to_string(&relu).unwrap(), ecrit, "l'aller-retour n'est pas stable");
    }
}

fuzz_target!(|data: &[u8]| {
    aller_retour::<ki_protocol::ClientMsg>(data);
    aller_retour::<ki_protocol::ServerMsg>(data);
});
