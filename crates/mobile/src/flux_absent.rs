//! Les partages d'écran n'existent que sur Android pour l'instant (voir `flux.rs`) :
//! ailleurs (iPhone, essai de l'interface sur PC), la page ne propose rien, et ces
//! fonctions ne font rien.

use ki_core::net::{NetHandle, VoiceLink};
use ki_protocol::StreamMeta;

use crate::Partage;

pub struct Regard {
    stream_id: u32,
}

impl Regard {
    pub fn demarrer(_stream_id: u32, _cle: [u8; 32], _meta: &StreamMeta, _net: &NetHandle, _lien: &VoiceLink) -> Option<Self> {
        None
    }

    pub fn stream_id(&self) -> u32 {
        self.stream_id
    }

    pub fn arreter(self, _net: Option<&NetHandle>) {}
}

pub fn source_changee(_stream_id: u32, _meta: &StreamMeta, _regard: Option<u32>) {}

pub fn installer(_partage: Partage) {}

pub fn diffusion() -> Option<u32> {
    None
}

pub fn diffusion_demandee() -> bool {
    false
}

pub fn accordee(_stream_id: u32, _net: &NetHandle) {}

pub fn trame_cle_demandee(_stream_id: u32) {}

pub fn budget(_stream_id: u32, _kbps: u32) {}

pub fn terminee(_stream_id: u32) -> bool {
    false
}

pub fn coupure() {}
