//! Les couleurs de ki-chat, sans egui : celle d'un pseudo, celle d'un rang
//! VALORANT. Les pseudos et les invités viennent de ki-ui-base, comme sur
//! le PC ; les rangs suivent `graphes::couleur_de_rang` du client PC. Pour
//! que l'appli mobile montre les mêmes couleurs aux mêmes personnes. Les
//! couleurs sont en `0xRRGGBB`.

use ki_protocol::Member;
use ki_ui_base::couleur as base;

/// La couleur des invités web.
pub const INVITE: u32 = base::INVITE.en_u32();

/// Couleur attribuée à un pseudo — même pseudo, même couleur, partout.
pub fn couleur_pseudo(nom: &str) -> u32 {
    base::pour_pseudo(nom).en_u32()
}

/// Couleur d'un membre : celle de son rôle, sinon celle de son pseudo ; les
/// invités web ont la leur.
pub fn couleur_membre(m: &Member) -> u32 {
    if m.invite || ki_protocol::est_invite(m.user_id) {
        INVITE
    } else {
        m.color.unwrap_or_else(|| couleur_pseudo(&m.username))
    }
}

/// Couleur d'un palier de rang VALORANT.
pub fn couleur_rang(palier: u8) -> u32 {
    match palier {
        3..=5 => 0x8f8f8f,
        6..=8 => 0xb57f4a,
        9..=11 => 0xc8d0d8,
        12..=14 => 0xe8c040,
        15..=17 => 0x3fb8c8,
        18..=20 => 0xb07cf0,
        21..=23 => 0x4fc86a,
        24..=26 => 0xe04a5a,
        27.. => 0xfff29a,
        _ => 0x63707f,
    }
}

/// Le palier affiché à côté d'un membre : celui de sa fiche, ou de sa
/// présence en jeu ; rien pour un non classé.
pub fn palier(m: &Member) -> Option<u8> {
    m.rang_valorant
        .or_else(|| m.jeu.as_ref().filter(|_| m.online).map(|j| j.rang))
        .filter(|t| *t >= 3)
}

/// `0xRRGGBB` en « #rrggbb ».
pub fn hex(c: u32) -> String {
    format!("#{:06x}", c & 0xff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Les teintes de ki-ui-base, en `0xRRGGBB`. La comparaison avec le PC
    /// — rangs compris — est dans ki-chat (`theme.rs`, `copies`).
    #[test]
    fn meme_couleurs_que_le_pc() {
        assert_eq!(couleur_pseudo("Redik_"), 0xba92ff);
        assert_eq!(couleur_pseudo("drion"), 0xffa95c);
        assert_eq!(INVITE, 0xf0b86c);
        assert_eq!(hex(0x2dd48f), "#2dd48f");
    }
}
