//! Ce que coûte de diffuser un roster à trente personnes.
//!
//! L'audit affirme que `broadcast_all` paie N fois ce qu'il pourrait payer
//! une : un clone profond du `ServerMsg` par destinataire, puis une
//! sérialisation JSON par tâche d'écriture. P5.1 propose de sérialiser une
//! seule fois et de faire porter au canal des octets partagés.
//!
//! Ce banc mesure les deux, côte à côte. Si l'écart n'est pas là, P5.1 ne
//! vaut pas d'être écrit — et c'est une réponse aussi utile que l'inverse.
//!
//! `cargo bench -p ki-protocol`

use std::hint::black_box;
use std::sync::Arc;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use ki_protocol::{Member, ServerMsg};

/// Un roster plausible : des pseudos de longueur variable, des rôles, des
/// couleurs, et une empreinte d'avatar pour la plupart. C'est le contenu qui
/// fait le coût du clone — un `Member` vide ne prouverait rien.
fn roster(n: usize) -> Vec<Member> {
    (0..n)
        .map(|i| Member {
            user_id: i as u64 + 1,
            username: format!("joueur_{i:02}_avec_un_pseudo_moyen"),
            speaking: i % 7 == 0,
            muted: i % 11 == 0,
            admin: i == 0,
            // Une empreinte FNV-1a telle que la produit le serveur.
            avatar: (i % 4 != 0).then(|| format!("{:016x}", 0x811c_9dc5_u64 ^ i as u64)),
            voice: (i % 3 == 0).then_some(2),
            roles: if i % 5 == 0 { vec![1, 3] } else { vec![3] },
            online: i % 3 != 2,
            // 0xRRGGBB : le découpage naturel est par couleur, pas par
            // groupes de quatre chiffres — d'où l'absence de séparateur.
            color: Some(0x5865f2),
            rank: (i % 4) as u16 * 10,
        })
        .collect()
}

/// Aujourd'hui : un clone profond par destinataire, puis une sérialisation
/// par destinataire. C'est exactement ce que fait la paire
/// `broadcast_all` + tâche d'écriture.
fn par_destinataire(membres: &[Member], destinataires: usize) -> usize {
    let msg = ServerMsg::Members { members: membres.to_vec() };
    let mut octets = 0;
    for _ in 0..destinataires {
        let copie = clone_msg(&msg);
        let json = serde_json::to_string(&copie).expect("sérialisation");
        octets += json.len();
    }
    octets
}

/// P5.1 : une sérialisation, N partages de compteur de références.
fn une_seule_fois(membres: &[Member], destinataires: usize) -> usize {
    let msg = ServerMsg::Members { members: membres.to_vec() };
    let json = serde_json::to_string(&msg).expect("sérialisation");
    let partage: Arc<[u8]> = Arc::from(json.into_bytes().into_boxed_slice());
    let mut octets = 0;
    for _ in 0..destinataires {
        let pour_ce_client = Arc::clone(&partage);
        octets += pour_ce_client.len();
    }
    octets
}

/// `ServerMsg` ne dérive pas `Clone` — le serveur le clone en pratique par
/// `msg.clone()` sur la variante, ce qu'on reproduit ici pour la seule
/// variante qui nous intéresse. Recopier le `Vec<Member>` est bien le coût
/// réel : chaque `Member` porte deux `String` et un `Vec`.
fn clone_msg(msg: &ServerMsg) -> ServerMsg {
    match msg {
        ServerMsg::Members { members } => ServerMsg::Members { members: members.clone() },
        _ => unreachable!("ce banc ne mesure que le roster"),
    }
}

fn diffusion(c: &mut Criterion) {
    let mut groupe = c.benchmark_group("diffusion_roster");
    // Trente personnes, la taille visée par le projet. Cinq pour voir la
    // pente : le coût par destinataire doit être plat d'un côté et linéaire
    // de l'autre.
    for n in [5usize, 30] {
        let membres = roster(n);
        let taille = serde_json::to_string(&ServerMsg::Members { members: membres.clone() })
            .expect("sérialisation")
            .len();
        // Le débit compté en octets de JSON réellement produits : c'est la
        // grandeur qui distingue les deux approches.
        groupe.throughput(Throughput::Bytes((taille * n) as u64));

        groupe.bench_with_input(
            BenchmarkId::new("par_destinataire", n),
            &membres,
            |b, m| b.iter(|| black_box(par_destinataire(m, n))),
        );
        groupe.bench_with_input(
            BenchmarkId::new("une_seule_fois", n),
            &membres,
            |b, m| b.iter(|| black_box(une_seule_fois(m, n))),
        );
    }
    groupe.finish();
}

criterion_group!(bancs, diffusion);
criterion_main!(bancs);
