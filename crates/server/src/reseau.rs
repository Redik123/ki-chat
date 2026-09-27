//! Le débit réseau du serveur, pour le tableau de bord : les compteurs que
//! le noyau Linux tient déjà pour chaque interface (`/proc/net/dev`),
//! relus toutes les cinq secondes. `/proc` est en mémoire : une lecture de
//! quelques centaines d'octets, sans disque, quelques microsecondes — rien
//! sur le chemin de la voix ni des streams, rien qui compte des paquets.
//!
//! Ce qui se lit : le débit entrant et sortant des cinq dernières secondes,
//! la courbe et la pointe sur dix minutes, le total depuis le démarrage. Le
//! sortant est ce qui porte les streams — chaque spectateur reçoit sa
//! copie : quand il plafonne alors qu'on diffuse, c'est la liaison du
//! serveur qui limite.
//!
//! Seule compte l'interface de la route par défaut, celle qui mène à
//! Internet : dans le conteneur, c'est `eth0` ; sur une machine qui
//! héberge aussi Docker, les ponts et les paires virtuelles recompteraient
//! le même trafic. Faute de route par défaut lisible, toutes comptent sauf
//! la boucle locale.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ki_protocol::TableauReseau;

/// Un relevé toutes les cinq secondes.
pub const PERIODE: Duration = Duration::from_secs(5);
/// La courbe et les pointes : dix minutes de relevés.
const FENETRE: usize = 120;

#[derive(Default)]
struct Etat {
    /// Le relevé précédent : instant, octets reçus, octets émis.
    dernier: Option<(Instant, u64, u64)>,
    /// Les compteurs au premier relevé : l'origine des totaux.
    origine: Option<(u64, u64)>,
    /// Les débits des derniers relevés (entrant, sortant), en kbit/s.
    debits: VecDeque<(u32, u32)>,
    totaux: (u64, u64),
}

/// Le débit du serveur, relevé par [`boucle`].
#[derive(Default)]
pub struct Reseau {
    etat: Mutex<Etat>,
}

impl Reseau {
    pub fn new() -> Self {
        Self::default()
    }

    /// Un relevé : les compteurs (octets reçus, émis) à cet instant.
    pub fn noter(&self, maintenant: Instant, recus: u64, emis: u64) {
        let mut e = self.etat.lock().unwrap();
        let (r0, e0) = *e.origine.get_or_insert((recus, emis));
        e.totaux = (recus.saturating_sub(r0), emis.saturating_sub(e0));
        if let Some((avant, r, s)) = e.dernier {
            let secondes = maintenant.duration_since(avant).as_secs_f64();
            if secondes > 0.0 {
                // Un compteur qui recule (interface recréée) vaut zéro, pas
                // un débit absurde.
                let kbps = |delta: u64| (delta as f64 * 8.0 / 1000.0 / secondes).min(f64::from(u32::MAX)) as u32;
                e.debits.push_back((kbps(recus.saturating_sub(r)), kbps(emis.saturating_sub(s))));
                while e.debits.len() > FENETRE {
                    e.debits.pop_front();
                }
            }
        }
        e.dernier = Some((maintenant, recus, emis));
    }

    /// Ce que le tableau de bord montre ; `None` tant qu'il n'y a pas deux
    /// relevés (ou hors Linux).
    pub fn tableau(&self) -> Option<TableauReseau> {
        let e = self.etat.lock().unwrap();
        let &(entrant, sortant) = e.debits.back()?;
        Some(TableauReseau {
            entrant_kbps: entrant,
            sortant_kbps: sortant,
            pointe_entrant_kbps: e.debits.iter().map(|d| d.0).max().unwrap_or(0),
            pointe_sortant_kbps: e.debits.iter().map(|d| d.1).max().unwrap_or(0),
            fenetre_s: (e.debits.len() as u64 * PERIODE.as_secs()) as u32,
            historique: e.debits.iter().copied().collect(),
            periode_s: PERIODE.as_secs() as u32,
            total_entrant_octets: e.totaux.0,
            total_sortant_octets: e.totaux.1,
        })
    }
}

/// Les interfaces qui portent la route par défaut (IPv4), d'après
/// `/proc/net/route` : une ligne d'en-tête, puis `Iface Destination Gateway
/// Flags RefCnt Use Metric Mask …`, adresses en hexadécimal. La route par
/// défaut va partout : destination et masque nuls.
fn interfaces_par_defaut(routes: &str) -> Vec<&str> {
    routes
        .lines()
        .skip(1)
        .filter_map(|ligne| match ligne.split_whitespace().collect::<Vec<_>>().as_slice() {
            [nom, "00000000", _, _, _, _, _, "00000000", ..] => Some(*nom),
            _ => None,
        })
        .collect()
}

/// Les octets reçus et émis, dans le format de `/proc/net/dev` : deux
/// lignes d'en-tête, puis `nom: reçus paquets … émis …` — les octets émis
/// sont le neuvième champ. Seulement les interfaces `retenues` quand il y
/// en a, sinon toutes sauf la boucle locale.
fn analyser(texte: &str, retenues: &[&str]) -> Option<(u64, u64)> {
    let mut total: Option<(u64, u64)> = None;
    for ligne in texte.lines().skip(2) {
        let Some((nom, champs)) = ligne.split_once(':') else { continue };
        let nom = nom.trim();
        let compte = if retenues.is_empty() { nom != "lo" } else { retenues.contains(&nom) };
        if !compte {
            continue;
        }
        let champs: Vec<&str> = champs.split_whitespace().collect();
        let (Some(r), Some(e)) = (champs.first(), champs.get(8)) else { continue };
        let (Ok(r), Ok(e)) = (r.parse::<u64>(), e.parse::<u64>()) else { continue };
        let t = total.get_or_insert((0, 0));
        t.0 = t.0.saturating_add(r);
        t.1 = t.1.saturating_add(e);
    }
    total
}

/// Les compteurs de la machine, là où le noyau Linux les donne. Ailleurs,
/// `/proc` n'existe pas : `None`, sans condition de compilation — le même
/// code se compile et se vérifie partout.
fn lire_compteurs() -> Option<(u64, u64)> {
    let routes = std::fs::read_to_string("/proc/net/route").unwrap_or_default();
    let dev = std::fs::read_to_string("/proc/net/dev").ok()?;
    let retenues = interfaces_par_defaut(&routes);
    // Une route par défaut vers une interface que `dev` ne montre pas
    // (tunnel, espace de noms) : on retombe sur toutes.
    analyser(&dev, &retenues).or_else(|| analyser(&dev, &[]))
}

/// Le relevé, toutes les cinq secondes tant que le serveur tourne. Hors
/// Linux, il n'y a rien à lire : la tâche s'arrête d'elle-même.
pub async fn boucle(state: Arc<crate::state::AppState>) {
    if lire_compteurs().is_none() {
        return;
    }
    let mut tic = tokio::time::interval(PERIODE);
    tic.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tic.tick().await;
        if let Some((recus, emis)) = lire_compteurs() {
            state.reseau.noter(Instant::now(), recus, emis);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEV: &str = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 1000000     100    0    0    0     0          0         0  1000000     100    0    0    0     0       0          0
  eth0: 2500000    2000    0    0    0     0          0         0 90000000   60000    0    0    0     0       0          0
docker0:  500000     400    0    0    0     0          0         0 10000000    7000    0    0    0     0       0          0
";

    const ROUTES: &str = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
eth0\t00000000\t010011AC\t0003\t0\t0\t0\t00000000\t0\t0\t0
eth0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0
docker0\t000012AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0
";

    #[test]
    fn seule_compte_l_interface_de_la_route_par_defaut() {
        let retenues = interfaces_par_defaut(ROUTES);
        assert_eq!(retenues, ["eth0"]);
        assert_eq!(analyser(DEV, &retenues), Some((2_500_000, 90_000_000)), "le pont docker0 ne recompte pas");
        // Sans route lisible : tout sauf la boucle locale.
        assert_eq!(analyser(DEV, &[]), Some((3_000_000, 100_000_000)));
        assert!(interfaces_par_defaut("").is_empty());
        // Une route vers une interface absente : rien, et l'appelant retombe
        // sur toutes.
        assert_eq!(analyser(DEV, &["tun0"]), None);
        assert_eq!(analyser("Inter-|\n face |\n", &[]), None, "aucune interface");
        assert_eq!(analyser("", &[]), None);
    }

    #[test]
    fn le_debit_se_calcule_entre_deux_releves_et_la_pointe_se_garde() {
        let r = Reseau::new();
        let t0 = Instant::now();
        r.noter(t0, 1_000_000, 5_000_000);
        assert!(r.tableau().is_none(), "un seul relevé : pas encore de débit");
        // Cinq secondes plus tard : 625 000 octets reçus (1 Mbit/s),
        // 25 000 000 émis (40 Mbit/s).
        r.noter(t0 + PERIODE, 1_625_000, 30_000_000);
        let t = r.tableau().unwrap();
        assert_eq!((t.entrant_kbps, t.sortant_kbps), (1000, 40_000));
        assert_eq!((t.total_entrant_octets, t.total_sortant_octets), (625_000, 25_000_000));
        // Le débit retombe : la pointe reste, la courbe garde les deux.
        r.noter(t0 + PERIODE * 2, 1_687_500, 30_625_000);
        let t = r.tableau().unwrap();
        assert_eq!((t.entrant_kbps, t.sortant_kbps), (100, 1000));
        assert_eq!((t.pointe_entrant_kbps, t.pointe_sortant_kbps), (1000, 40_000));
        assert_eq!(t.historique, [(1000, 40_000), (100, 1000)]);
        assert_eq!((t.fenetre_s, t.periode_s), (10, 5));
        // Un compteur qui recule (interface recréée) : zéro, pas l'infini.
        r.noter(t0 + PERIODE * 3, 10, 10);
        assert_eq!(r.tableau().unwrap().sortant_kbps, 0);
    }

    #[test]
    fn la_courbe_ne_garde_que_dix_minutes() {
        let r = Reseau::new();
        let t0 = Instant::now();
        for i in 0..=(FENETRE as u64 + 30) {
            r.noter(t0 + PERIODE * i as u32, i * 1000, i * 1000);
        }
        let t = r.tableau().unwrap();
        assert_eq!(t.historique.len(), FENETRE);
        assert_eq!(t.fenetre_s, 600);
    }
}
