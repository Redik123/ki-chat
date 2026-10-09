//! Les couleurs de ki-chat, sans egui : celle d'un pseudo, celle d'un rang
//! VALORANT. Les mêmes règles que le client PC (`theme::member_color`,
//! `graphes::couleur_de_rang`), pour que l'appli mobile montre les mêmes
//! couleurs aux mêmes personnes. Les couleurs sont en `0xRRGGBB`.

use ki_protocol::Member;

/// Les huit teintes des pseudos, stables par hachage du nom.
const PALETTE: [u32; 8] = [
    0x2dd48f, 0x62a8ff, 0xffa95c, 0xff8ac4, 0xba92ff, 0x2ad3dd, 0xffd863, 0xff7d7d,
];

/// La couleur des invités web (`theme::INVITE`).
pub const INVITE: u32 = 0xf0b86c;

/// Couleur attribuée à un pseudo — même pseudo, même couleur, partout.
pub fn couleur_pseudo(nom: &str) -> u32 {
    let h = nom.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    PALETTE[(h % PALETTE.len() as u32) as usize]
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

    /// Le hachage, tel que le PC le fait. La comparaison avec le PC —
    /// palette, ordre des teintes, rangs, invités — est dans ki-chat
    /// (`theme.rs`, `copies`) : ki-core ne voit pas ki-ui.
    #[test]
    fn meme_hachage_que_le_pc() {
        let h = "Redik_".bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
        assert_eq!(couleur_pseudo("Redik_"), PALETTE[(h % 8) as usize]);
        assert_eq!(hex(0x2dd48f), "#2dd48f");
    }
}
