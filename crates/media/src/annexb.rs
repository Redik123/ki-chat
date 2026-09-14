//! Le H.264 « Annex B » : des unités NAL séparées par des codes de départ
//! (`00 00 01` ou `00 00 00 01`). C'est ce que NVENC et openh264 rendent,
//! et ce que l'enregistreur garde en mémoire. On n'en lit que l'enveloppe :
//! le type de chaque NAL, jamais son contenu.

/// Type de NAL : 5 = tranche IDR, 7 = SPS, 8 = PPS, 9 = délimiteur d'unité
/// d'accès.
pub const IDR: u8 = 5;
pub const SPS: u8 = 7;
pub const PPS: u8 = 8;
pub const AUD: u8 = 9;

/// Les NAL d'un flux : (type, début du code de départ, début du NAL, fin).
fn decouper(flux: &[u8]) -> Vec<(u8, usize, usize, usize)> {
    let mut nals = Vec::new();
    let mut i = 0usize;
    // Les codes de départ, dans l'ordre.
    let mut departs: Vec<(usize, usize)> = Vec::new();
    while i + 3 <= flux.len() {
        if flux[i] == 0 && flux[i + 1] == 0 {
            if flux[i + 2] == 1 {
                departs.push((i, i + 3));
                i += 3;
                continue;
            }
            if i + 4 <= flux.len() && flux[i + 2] == 0 && flux[i + 3] == 1 {
                departs.push((i, i + 4));
                i += 4;
                continue;
            }
        }
        i += 1;
    }
    for (k, &(code, debut)) in departs.iter().enumerate() {
        let fin = departs.get(k + 1).map(|d| d.0).unwrap_or(flux.len());
        if debut < fin {
            nals.push((flux[debut] & 0x1f, code, debut, fin));
        }
    }
    nals
}

/// Les NAL d'une unité d'accès : (type, octets du NAL sans son code de départ).
pub fn nals(unite: &[u8]) -> Vec<(u8, &[u8])> {
    decouper(unite).into_iter().map(|(t, _, d, f)| (t, &unite[d..f])).collect()
}

/// Vrai si l'unité contient une tranche IDR : une trame clé.
pub fn est_cle(unite: &[u8]) -> bool {
    nals(unite).iter().any(|(t, _)| *t == IDR)
}

/// SPS et PPS de l'unité, remis en Annex B (codes de départ de quatre
/// octets), ou `None` si elle n'en porte pas.
pub fn parametres(unite: &[u8]) -> Option<Vec<u8>> {
    let mut sortie = Vec::new();
    let (mut sps, mut pps) = (false, false);
    for (t, nal) in nals(unite) {
        if t == SPS || t == PPS {
            sortie.extend_from_slice(&[0, 0, 0, 1]);
            sortie.extend_from_slice(nal);
            sps |= t == SPS;
            pps |= t == PPS;
        }
    }
    (sps && pps).then_some(sortie)
}

/// Découpe un flux brut en unités d'accès, sur les délimiteurs (type 9) —
/// chaque unité les inclut. Sans délimiteur, tout le flux est une unité.
pub fn unites_d_acces(flux: &[u8]) -> Vec<&[u8]> {
    let nals = decouper(flux);
    let debuts: Vec<usize> = nals.iter().filter(|(t, ..)| *t == AUD).map(|(_, code, ..)| *code).collect();
    if debuts.is_empty() {
        return vec![flux];
    }
    let mut unites = Vec::with_capacity(debuts.len());
    for (k, &debut) in debuts.iter().enumerate() {
        let fin = debuts.get(k + 1).copied().unwrap_or(flux.len());
        unites.push(&flux[debut..fin]);
    }
    unites
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flux() -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(&[0, 0, 0, 1, 0x09, 0xf0]); // AUD
        f.extend_from_slice(&[0, 0, 0, 1, 0x67, 1, 2, 3]); // SPS
        f.extend_from_slice(&[0, 0, 1, 0x68, 4, 5]); // PPS (code court)
        f.extend_from_slice(&[0, 0, 0, 1, 0x65, 9, 9, 9]); // IDR
        f.extend_from_slice(&[0, 0, 0, 1, 0x09, 0x30]); // AUD
        f.extend_from_slice(&[0, 0, 0, 1, 0x41, 7, 7]); // tranche P
        f
    }

    #[test]
    fn les_nal_se_lisent_par_leur_type() {
        let f = flux();
        let types: Vec<u8> = nals(&f).into_iter().map(|(t, _)| t).collect();
        assert_eq!(types, vec![AUD, SPS, PPS, IDR, AUD, 1]);
        assert_eq!(nals(&f)[1].1, &[0x67, 1, 2, 3]);
    }

    #[test]
    fn les_unites_d_acces_se_coupent_aux_delimiteurs() {
        let f = flux();
        let u = unites_d_acces(&f);
        assert_eq!(u.len(), 2);
        assert!(est_cle(u[0]));
        assert!(!est_cle(u[1]));
        assert_eq!(parametres(u[0]), Some(vec![0, 0, 0, 1, 0x67, 1, 2, 3, 0, 0, 0, 1, 0x68, 4, 5]));
        assert_eq!(parametres(u[1]), None);
    }

    #[test]
    fn sans_delimiteur_tout_est_une_unite() {
        let f = vec![0, 0, 0, 1, 0x65, 1, 2];
        assert_eq!(unites_d_acces(&f).len(), 1);
        assert!(nals(&[1, 2, 3]).is_empty());
    }
}
