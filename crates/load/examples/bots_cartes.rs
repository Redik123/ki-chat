//! Des bots à la table de cartes : de quoi jouer seul contre la machine, et
//! éprouver le serveur de bout en bout — vraie connexion QUIC, vrais
//! messages, vrai arbitrage.
//!
//! ```text
//! cargo run -p ki-load --example bots_cartes -- <serveur> --invite CODE
//!       [--bots 3] [--salon 101] [--ouvrir] [--joueurs N] [--manches 1]
//!       [--depart] [--renfort]
//! ```
//!
//! Les bots entrent dans le salon vocal et s'assoient à la table qu'on y a
//! ouverte : ouvre-la depuis ki-chat, attends qu'ils s'assoient, distribue.
//! Avec `--ouvrir`, le premier bot l'ouvre lui-même et distribue dès que
//! `--joueurs` places sont prises (tous les bots, par défaut) : c'est
//! l'essai sans personne. Ils s'en vont après `--manches` manches gagnées.
//!
//! Pour éprouver les places tenues par un bot : avec `--depart`, le dernier
//! bot quitte le salon vocal vingt secondes après la première donne — le
//! serveur met un bot à sa place ; avec `--renfort`, un bot de plus arrive
//! quinze secondes après ce départ (une minute après le lancement, sans
//! `--depart`), et prend la place que tient ce bot, cartes comprises.
//!
//! Ils jouent sans finesse — la première carte qui va, la couleur qu'ils ont
//! le plus — prennent leur temps comme des humains, contrent qui oublie
//! d'annoncer, et oublient eux-mêmes une fois sur quatre.

use std::time::Duration;

use ki_client_quic::QuicClient;
use ki_protocol::{
    ActionCartes, Carte, ClientMsg, CouleurCarte, EvenementCartes, PhaseCartes, ServerMsg, TableCartes, UserId,
};
use tokio::time::Instant;

struct Options {
    serveur: String,
    invite: String,
    bots: usize,
    salon: u32,
    ouvrir: bool,
    joueurs: usize,
    manches: u32,
    depart: bool,
    renfort: bool,
}

fn options() -> anyhow::Result<Options> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let valeur = |nom: &str| args.iter().position(|a| a == nom).and_then(|i| args.get(i + 1)).cloned();
    let nombre = |nom: &str, defaut: u64| -> anyhow::Result<u64> {
        valeur(nom).map_or(Ok(defaut), |v| v.parse().map_err(|_| anyhow::anyhow!("{nom} attend un nombre")))
    };
    let serveur = args
        .iter()
        .enumerate()
        .find(|(i, a)| {
            !a.starts_with("--")
                && (*i == 0 || !args[i - 1].starts_with("--") || ["--ouvrir", "--depart", "--renfort"].contains(&args[i - 1].as_str()))
        })
        .map(|(_, a)| a.clone())
        .ok_or_else(|| anyhow::anyhow!("usage : bots_cartes <serveur> --invite CODE [--bots 3] [--salon 101]"))?;
    let bots = nombre("--bots", 3)?.clamp(1, 9) as usize;
    Ok(Options {
        serveur,
        invite: valeur("--invite")
            .or_else(|| std::env::var("KI_TOKEN").ok().filter(|t| !t.is_empty()))
            .ok_or_else(|| anyhow::anyhow!("--invite CODE (ou KI_TOKEN) : le code d'invitation du serveur"))?,
        bots,
        salon: nombre("--salon", 101)? as u32,
        ouvrir: args.iter().any(|a| a == "--ouvrir"),
        joueurs: nombre("--joueurs", bots as u64)?.max(2) as usize,
        manches: nombre("--manches", 1)?.max(1) as u32,
        depart: args.iter().any(|a| a == "--depart"),
        renfort: args.iter().any(|a| a == "--renfort"),
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let opts = std::sync::Arc::new(options()?);
    // Le départ prévient le renfort : il arrive quand il y a une place à
    // prendre, si long qu'ait été la première donne.
    let parti = std::sync::Arc::new(tokio::sync::Notify::new());
    let mut taches = Vec::new();
    for n in 0..opts.bots {
        let opts = opts.clone();
        let parti = parti.clone();
        taches.push(tokio::spawn(async move {
            let nom = format!("bot-{}", n + 1);
            let part = opts.depart && n + 1 == opts.bots;
            let graine = 0x9e37_79b9 ^ ((n as u64 + 1) * 0x2545_f491);
            match un_bot(&opts, &nom, n == 0 && opts.ouvrir, part.then_some(&*parti), graine).await {
                Ok(()) => println!("{nom} : fini"),
                Err(e) => println!("{nom} : {e}"),
            }
        }));
        // Pas tous à la même milliseconde : le premier ouvre, les autres
        // trouvent la table.
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    if opts.renfort {
        let opts = opts.clone();
        let parti = parti.clone();
        taches.push(tokio::spawn(async move {
            if opts.depart {
                parti.notified().await;
                tokio::time::sleep(Duration::from_secs(15)).await;
            } else {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
            match un_bot(&opts, "bot-renfort", false, None, 0x51ed_270b).await {
                Ok(()) => println!("bot-renfort : fini"),
                Err(e) => println!("bot-renfort : {e}"),
            }
        }));
    }
    for t in taches {
        let _ = t.await;
    }
    Ok(())
}

/// Un hasard sans dépendance : xorshift, assez pour hésiter comme un humain.
struct De(u64);

impl De {
    fn tirer(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn entre(&mut self, de: u64, a: u64) -> u64 {
        de + self.tirer() % (a - de + 1)
    }
    fn une_fois_sur(&mut self, n: u64) -> bool {
        self.tirer().is_multiple_of(n)
    }
}

/// `part` : ce bot quittera le salon vingt secondes après la première
/// donne, et le dira au renfort.
async fn un_bot(
    opts: &Options,
    nom: &str,
    hote: bool,
    part: Option<&tokio::sync::Notify>,
    graine: u64,
) -> anyhow::Result<()> {
    let mut client = QuicClient::connect(&opts.serveur, None).await?;
    client
        .send_msg(&ClientMsg::Auth {
            username: nom.to_string(),
            password: "bot-de-cartes".into(),
            invite: Some(opts.invite.clone()),
            protocole: ki_protocol::PROTOCOLE,
            appareil: ki_protocol::Appareil::Pc,
        })
        .await?;
    let (mut writer, mut reader) = client.split();
    let mut de = De(graine | 1);
    let mut moi: Option<UserId> = None;
    let mut prevu: Option<(Instant, ActionCartes)> = None;
    let mut gagnees_vues = 0u32;
    let mut dernier_numero = 0u32;
    let fin = Instant::now() + Duration::from_secs(30 * 60);
    // `--depart` : l'heure où ce bot quittera le salon, fixée à la première
    // donne.
    let mut depart: Option<Instant> = None;

    loop {
        let reveil = prevu.as_ref().map(|(quand, _)| *quand).unwrap_or(fin).min(depart.unwrap_or(fin));
        tokio::select! {
            msg = reader.next_msg() => {
                let Some(msg) = msg else { anyhow::bail!("connexion fermée") };
                match msg {
                    ServerMsg::Welcome { user_id, cartes, .. } => {
                        anyhow::ensure!(cartes, "ce serveur ne connaît pas les tables de cartes");
                        moi = Some(user_id);
                        writer.send_msg(&ClientMsg::JoinVoice { channel: opts.salon, password: None }).await?;
                        if hote {
                            tokio::time::sleep(Duration::from_millis(300)).await;
                            writer.send_msg(&ClientMsg::Cartes { action: ActionCartes::Ouvrir }).await?;
                        }
                    }
                    ServerMsg::Cartes { table } => {
                        if let Some(t) = &table {
                            // Les manches gagnées depuis notre arrivée.
                            let neufs = (t.numero.saturating_sub(dernier_numero) as usize).min(t.journal.len());
                            for e in &t.journal[t.journal.len() - neufs..] {
                                match e {
                                    EvenementCartes::Gagne { par, bot } => {
                                        gagnees_vues += 1;
                                        let qui = if *bot { "le bot de " } else { "" };
                                        println!("{nom} : manche gagnée par {qui}{par} ({gagnees_vues} en tout)");
                                    }
                                    // Une fois, par le premier bot : qui est remplacé, qui reprend.
                                    EvenementCartes::Remplace { par } if hote => println!("{nom} : un bot prend la place de {par}"),
                                    EvenementCartes::Reprend { par, place } if hote => println!("{nom} : {par} prend la place de {place}"),
                                    _ => {}
                                }
                            }
                            dernier_numero = t.numero;
                            if part.is_some() && depart.is_none() && t.phase == PhaseCartes::EnCours {
                                depart = Some(Instant::now() + Duration::from_secs(20));
                            }
                        }
                        if gagnees_vues >= opts.manches {
                            writer.send_msg(&ClientMsg::Cartes { action: ActionCartes::Quitter }).await?;
                            writer.close_gracefully().await;
                            return Ok(());
                        }
                        if let (Some(moi), Some(t)) = (moi, &table) {
                            let decision = decider(t, moi, opts, &mut de);
                            // Un même geste déjà prévu garde son heure : on
                            // n'hésite pas de nouveau à chaque message.
                            prevu = match (prevu.take(), decision) {
                                (Some((quand, ancien)), Some((neuf, _))) if std::mem::discriminant(&ancien) == std::mem::discriminant(&neuf) => {
                                    Some((quand, neuf))
                                }
                                (_, Some((neuf, delai))) => Some((Instant::now() + delai, neuf)),
                                (_, None) => None,
                            };
                        }
                    }
                    ServerMsg::CartesRefus { message } => println!("{nom} : refusé — {message}"),
                    ServerMsg::Error { message } => println!("{nom} : erreur — {message}"),
                    _ => {}
                }
            }
            _ = tokio::time::sleep_until(reveil) => {
                if Instant::now() >= fin {
                    anyhow::bail!("trente minutes sans finir");
                }
                if depart.is_some_and(|d| Instant::now() >= d) {
                    println!("{nom} : quitte le salon vocal, sa main reste à un bot du serveur");
                    writer.send_msg(&ClientMsg::LeaveVoice).await?;
                    if let Some(parti) = part {
                        parti.notify_one();
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    writer.close_gracefully().await;
                    return Ok(());
                }
                if let Some((_, action)) = prevu.take() {
                    writer.send_msg(&ClientMsg::Cartes { action }).await?;
                }
            }
        }
    }
}

/// Ce que fait le bot devant cette table, et après combien de temps.
fn decider(t: &TableCartes, moi: UserId, opts: &Options, de: &mut De) -> Option<(ActionCartes, Duration)> {
    let ms = |de: &mut De, a: u64, b: u64| Duration::from_millis(de.entre(a, b));
    let assis = t.joueur(moi);
    match t.phase {
        PhaseCartes::Attente | PhaseCartes::Finie => {
            if assis.is_none() && t.joueurs.len() < ki_protocol::MAX_JOUEURS_CARTES {
                return Some((ActionCartes::Rejoindre, ms(de, 200, 800)));
            }
            let pret = t.joueurs.len() >= opts.joueurs;
            if t.hote == moi && pret {
                let attente = if t.phase == PhaseCartes::Finie { (3000, 5000) } else { (800, 1500) };
                return Some((ActionCartes::Lancer, ms(de, attente.0, attente.1)));
            }
            None
        }
        PhaseCartes::EnCours => {
            let Some(moi_j) = assis else {
                // Pas de place : celle que tient un bot, s'il y en a une,
                // avec ses cartes.
                let bot = t.joueurs.iter().find(|j| j.bot)?;
                return Some((ActionCartes::Reprendre { place: bot.user_id }, ms(de, 600, 1500)));
            };
            if moi_j.bot {
                return Some((ActionCartes::Reprendre { place: moi }, ms(de, 600, 1500)));
            }
            if t.expose.is_some_and(|e| e != moi) {
                return Some((ActionCartes::Contrer, ms(de, 500, 1800)));
            }
            let a_moi = t.tour == Some(moi);
            if t.main.len() <= 2 && !moi_j.annonce && (a_moi || t.expose == Some(moi)) && !de.une_fois_sur(4) {
                return Some((ActionCartes::Annoncer, ms(de, 200, 700)));
            }
            if !a_moi {
                return None;
            }
            let delai = ms(de, 800, 2000);
            if let Some(p) = t.piochee {
                return Some(if t.jouable(moi, p) {
                    (ActionCartes::Jouer { carte: p, couleur: p.sauvage().then(|| couleur_preferee(&t.main)) }, delai)
                } else {
                    (ActionCartes::Passer, delai)
                });
            }
            // Les cartes de couleur d'abord, les jokers en dernier recours.
            let mut jouables: Vec<Carte> = t.main.iter().copied().filter(|c| t.jouable(moi, *c)).collect();
            jouables.sort_by_key(|c| c.sauvage());
            Some(match jouables.first() {
                Some(&carte) => {
                    (ActionCartes::Jouer { carte, couleur: carte.sauvage().then(|| couleur_preferee(&t.main)) }, delai)
                }
                None => (ActionCartes::Piocher, delai),
            })
        }
    }
}

/// La couleur dont on a le plus.
fn couleur_preferee(main: &[Carte]) -> CouleurCarte {
    CouleurCarte::TOUTES
        .into_iter()
        .max_by_key(|c| main.iter().filter(|x| x.couleur() == Some(*c)).count())
        .unwrap_or(CouleurCarte::Rouge)
}
