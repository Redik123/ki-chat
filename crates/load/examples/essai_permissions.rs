//! Les permissions de bout en bout : de vrais comptes, de vraies connexions
//! QUIC, contre un serveur **neuf** — le premier compte créé y devient
//! propriétaire.
//!
//! ```text
//! cargo run -p ki-load --example essai_permissions -- <serveur> --invite CODE --donnees <KI_DATA_DIR>
//! ```
//!
//! `--donnees` : le dossier de données du serveur, pour y déposer un fichier
//! partagé (l'onglet Fichiers lit le disque).
//!
//! Ce qu'il vérifie :
//! - un salon vocal fermé par un mot de passe : un administrateur n'y entre
//!   pas sans, il y entre avec ; le propriétaire y entre sans ;
//! - personne d'autre que le propriétaire n'y déplace quelqu'un, et l'on ne
//!   déplace pas quelqu'un qui n'est pas en vocal ;
//! - l'onglet Fichiers d'un modérateur ne liste pas ce qui n'a été posté
//!   que dans un salon privé, et son journal n'en dit pas le nom ;
//! - qui ne fait que créer des invitations ne reçoit ni les comptes ni les
//!   codes des autres, et révoque les siens ;
//! - le propriétaire règle la couleur de son rôle.
//!
//! Il dit ce qui passe et ce qui casse, et sort en erreur si quelque chose
//! casse.

use std::time::Duration;

use ki_client_quic::QuicClient;
use ki_protocol::{perm, ChannelId, ChannelKind, ClientMsg, Member, RoleId, ServerMsg, UserId};

struct Compte {
    id: UserId,
    c: QuicClient,
}

impl Compte {
    async fn ouvrir(serveur: &str, invite: &str, nom: &str) -> anyhow::Result<Self> {
        let mut c = QuicClient::connect(serveur, None).await?;
        c.send_msg(&ClientMsg::Auth {
            username: nom.into(),
            password: format!("{nom}-mot-de-passe"),
            invite: Some(invite.into()),
            protocole: ki_protocol::PROTOCOLE,
            appareil: Default::default(),
        })
        .await?;
        let id = attendre(&mut c, |m| match m {
            ServerMsg::Welcome { user_id, .. } => Some(*user_id),
            _ => None,
        })
        .await
        .ok_or_else(|| anyhow::anyhow!("{nom} : pas de Welcome"))?;
        Ok(Compte { id, c })
    }

    async fn dire(&mut self, msg: ClientMsg) {
        self.c.send_msg(&msg).await.expect("envoi");
    }

    /// Le premier message qui répond, à cinq secondes au plus.
    async fn attendre<T>(&mut self, f: impl FnMut(&ServerMsg) -> Option<T>) -> Option<T> {
        attendre(&mut self.c, f).await
    }

    /// La réponse à une commande : son erreur, ou `Ok` si `ok` reconnaît ce
    /// qui arrive en cas de succès.
    async fn resultat(&mut self, mut ok: impl FnMut(&ServerMsg) -> bool) -> Result<(), String> {
        self.attendre(|m| match m {
            ServerMsg::Error { message } => Some(Err(message.clone())),
            m if ok(m) => Some(Ok(())),
            _ => None,
        })
        .await
        .unwrap_or_else(|| Err("pas de réponse".into()))
    }

    /// Ce qui arrive encore dans la demi-seconde : jeté.
    async fn vider(&mut self) {
        while tokio::time::timeout(Duration::from_millis(500), self.c.next_msg()).await.is_ok() {}
    }
}

async fn attendre<T>(c: &mut QuicClient, mut f: impl FnMut(&ServerMsg) -> Option<T>) -> Option<T> {
    let fin = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let reste = fin.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(reste, c.next_msg()).await {
            Ok(Some(m)) => {
                if let Some(t) = f(&m) {
                    return Some(t);
                }
            }
            _ => return None,
        }
    }
}

fn voix_de(m: &ServerMsg, qui: UserId) -> Option<Option<ChannelId>> {
    let cherche = |l: &[Member]| l.iter().find(|x| x.user_id == qui).map(|x| x.voice);
    match m {
        ServerMsg::Members { members } => cherche(members),
        ServerMsg::MemberUpdate { member } if member.user_id == qui => Some(member.voice),
        _ => None,
    }
}

struct Bilan {
    ok: usize,
    ko: Vec<String>,
}

impl Bilan {
    fn verifier(&mut self, quoi: &str, vrai: bool, detail: impl std::fmt::Debug) {
        if vrai {
            self.ok += 1;
            println!("  ok    {quoi}");
        } else {
            println!("  CASSÉ {quoi} — {detail:?}");
            self.ko.push(quoi.to_string());
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let valeur = |nom: &str| args.iter().position(|a| a == nom).and_then(|i| args.get(i + 1)).cloned();
    let serveur = args.first().cloned().ok_or_else(|| anyhow::anyhow!("usage : essai_permissions <serveur> --invite CODE --donnees DOSSIER"))?;
    let invite = valeur("--invite").ok_or_else(|| anyhow::anyhow!("--invite CODE"))?;
    let donnees = valeur("--donnees").ok_or_else(|| anyhow::anyhow!("--donnees DOSSIER"))?;
    let mut b = Bilan { ok: 0, ko: Vec::new() };

    // Le premier compte d'un serveur neuf est le propriétaire.
    let mut proprio = Compte::ouvrir(&serveur, &invite, "proprio").await?;
    let mut admin = Compte::ouvrir(&serveur, &invite, "admin").await?;
    let mut modo = Compte::ouvrir(&serveur, &invite, "modo").await?;
    let mut membre = Compte::ouvrir(&serveur, &invite, "membre").await?;
    proprio.vider().await;

    // Les rôles : un administrateur, le modérateur d'origine (rôle 3), et un
    // rôle qui ne fait que créer des invitations.
    let role = |nom: &str, rank: u16, perms: u64| ClientMsg::AdminCreateRole {
        name: nom.into(),
        color: None,
        rank,
        perms,
    };
    proprio.dire(role("Admin", 500, perm::ADMINISTRATOR)).await;
    proprio.dire(role("Inviteur", 5, perm::CREATE_INVITE)).await;
    let roles = proprio
        .attendre(|m| match m {
            ServerMsg::Roles { roles } if roles.iter().any(|r| r.name == "Inviteur") => Some(roles.clone()),
            _ => None,
        })
        .await
        .expect("les rôles créés");
    let id_role = |nom: &str| -> RoleId { roles.iter().find(|r| r.name == nom).map(|r| r.id).expect(nom) };
    let (r_admin, r_inviteur) = (id_role("Admin"), id_role("Inviteur"));
    proprio.dire(ClientMsg::AdminSetUserRoles { username: "admin".into(), roles: vec![r_admin] }).await;
    proprio.dire(ClientMsg::AdminSetUserRoles { username: "modo".into(), roles: vec![3] }).await;
    proprio.dire(ClientMsg::AdminSetUserRoles { username: "membre".into(), roles: vec![r_inviteur] }).await;

    // Les salons : un salon texte réservé aux admins, un vocal ouvert, un
    // vocal qu'on fermera par un mot de passe.
    for (nom, kind, allowed) in [
        ("staff", ChannelKind::Text, Some(vec![r_admin])),
        ("Ouvert", ChannelKind::Voice, None),
        ("Secret", ChannelKind::Voice, None),
    ] {
        proprio.dire(ClientMsg::AdminCreateChannel { name: nom.into(), kind, allowed_roles: allowed }).await;
    }
    let salons = proprio
        .attendre(|m| match m {
            ServerMsg::ChannelsUpdated { channels } if channels.iter().any(|c| c.name == "Secret") => {
                Some(channels.clone())
            }
            _ => None,
        })
        .await
        .expect("les salons créés");
    let id_salon = |nom: &str| -> ChannelId { salons.iter().find(|c| c.name == nom).map(|c| c.id).expect(nom) };
    let (staff, ouvert, secret) = (id_salon("staff"), id_salon("Ouvert"), id_salon("Secret"));
    for c in [&mut proprio, &mut admin, &mut modo, &mut membre] {
        c.vider().await;
    }

    println!("Salon vocal fermé par un mot de passe");
    proprio
        .dire(ClientMsg::AdminSetVoicePassword { channel: secret, password: Some("sesame".into()), ttl_secs: 3600 })
        .await;
    let r = proprio.resultat(|m| matches!(m, ServerMsg::Info { message } if message.contains("verrouillé"))).await;
    b.verifier("le propriétaire pose le verrou", r.is_ok(), r);
    let id_admin = admin.id;
    admin.dire(ClientMsg::JoinVoice { channel: secret, password: None }).await;
    let refus = admin
        .attendre(|m| match m {
            ServerMsg::VoiceLocked { channel, .. } => Some(*channel == secret),
            m => voix_de(m, id_admin).filter(|v| *v == Some(secret)).map(|_| false),
        })
        .await;
    b.verifier("un administrateur n'entre pas sans le mot de passe", refus == Some(true), refus);
    admin.dire(ClientMsg::JoinVoice { channel: secret, password: Some("faux".into()) }).await;
    let refus = admin
        .attendre(|m| match m {
            ServerMsg::VoiceLocked { wrong, .. } => Some(*wrong),
            _ => None,
        })
        .await;
    b.verifier("ni avec un mot de passe faux", refus == Some(true), refus);
    admin.dire(ClientMsg::JoinVoice { channel: secret, password: Some("sesame".into()) }).await;
    let entre = admin.attendre(|m| voix_de(m, id_admin).filter(|v| *v == Some(secret))).await;
    b.verifier("il entre avec le bon", entre.is_some(), entre);
    let id_proprio = proprio.id;
    proprio.dire(ClientMsg::JoinVoice { channel: secret, password: None }).await;
    let entre = proprio
        .attendre(|m| match m {
            ServerMsg::VoiceLocked { .. } => Some(false),
            m => voix_de(m, id_proprio).filter(|v| *v == Some(secret)).map(|_| true),
        })
        .await;
    b.verifier("le propriétaire entre sans", entre == Some(true), entre);

    println!("Déplacer en vocal");
    admin.dire(ClientMsg::AdminVoiceMove { username: "membre".into(), channel: Some(ouvert) }).await;
    let r = admin.resultat(|_| false).await;
    b.verifier(
        "on ne déplace pas quelqu'un qui n'est pas en vocal",
        r.as_ref().is_err_and(|e| e.contains("pas en vocal")),
        r,
    );
    membre.dire(ClientMsg::JoinVoice { channel: ouvert, password: None }).await;
    let id_membre = membre.id;
    let entre = membre.attendre(|m| voix_de(m, id_membre).filter(|v| *v == Some(ouvert))).await;
    b.verifier("le membre entre dans le vocal ouvert", entre.is_some(), entre);
    admin.vider().await;
    admin.dire(ClientMsg::AdminVoiceMove { username: "membre".into(), channel: Some(secret) }).await;
    let r = admin.resultat(|_| false).await;
    b.verifier(
        "un administrateur n'amène personne dans le salon fermé",
        r.as_ref().is_err_and(|e| e.contains("mot de passe")),
        r,
    );
    proprio.vider().await;
    proprio.dire(ClientMsg::AdminVoiceMove { username: "membre".into(), channel: Some(secret) }).await;
    let bouge = proprio.attendre(|m| voix_de(m, id_membre).filter(|v| *v == Some(secret))).await;
    b.verifier("le propriétaire, si", bouge.is_some(), bouge);

    println!("Fichiers et journal d'un salon privé");
    let id_fichier = "00c0ffee00c0ffee";
    let dossier = std::path::Path::new(&donnees).join("files").join(id_fichier);
    std::fs::create_dir_all(&dossier)?;
    std::fs::write(dossier.join("capture.png"), b"pas vraiment une image")?;
    proprio.dire(ClientMsg::Join { channel: staff }).await;
    proprio.vider().await;
    proprio
        .dire(ClientMsg::Chat { text: format!("/files/{id_fichier}/capture.png"), reply_to: None, salon: Some(staff) })
        .await;
    proprio.vider().await;
    modo.vider().await;
    modo.dire(ClientMsg::AdminListFichiers).await;
    let liste = modo
        .attendre(|m| match m {
            ServerMsg::AdminFichiers { fichiers, .. } => Some(fichiers.iter().map(|f| f.id.clone()).collect::<Vec<_>>()),
            ServerMsg::Error { message } => Some(vec![format!("erreur : {message}")]),
            _ => None,
        })
        .await;
    b.verifier(
        "le modérateur ne voit pas le fichier du salon privé",
        liste.as_ref().is_some_and(|l| !l.iter().any(|i| i == id_fichier) && !l.iter().any(|i| i.starts_with("erreur"))),
        &liste,
    );
    proprio.dire(ClientMsg::AdminListFichiers).await;
    let liste = proprio
        .attendre(|m| match m {
            ServerMsg::AdminFichiers { fichiers, .. } => Some(fichiers.iter().map(|f| f.id.clone()).collect::<Vec<_>>()),
            _ => None,
        })
        .await;
    b.verifier("le propriétaire, si", liste.as_ref().is_some_and(|l| l.iter().any(|i| i == id_fichier)), &liste);
    modo.dire(ClientMsg::AdminAuditLog { limit: 100 }).await;
    let journal = modo
        .attendre(|m| match m {
            ServerMsg::AuditLog { records } => Some(records.clone()),
            _ => None,
        })
        .await
        .unwrap_or_default();
    let creation = journal.iter().find(|r| r.action == "channel.create" && r.target != "Ouvert" && r.target != "Secret");
    b.verifier(
        "le journal du modérateur ne nomme pas le salon privé",
        creation.is_some_and(|r| r.target == "un salon privé") && !journal.iter().any(|r| r.target == "staff"),
        creation,
    );

    println!("Invitations");
    membre.vider().await;
    membre.dire(ClientMsg::AdminCreateInvite { uses: Some(1), label: "à moi".into(), ttl_secs: 0 }).await;
    let code = membre
        .attendre(|m| match m {
            ServerMsg::InviteCreated { code } => Some(code.clone()),
            _ => None,
        })
        .await;
    b.verifier("qui a « Créer des invitations » en crée", code.is_some(), &code);
    proprio.dire(ClientMsg::AdminCreateInvite { uses: Some(1), label: "au proprio".into(), ttl_secs: 0 }).await;
    let code_proprio = proprio
        .attendre(|m| match m {
            ServerMsg::InviteCreated { code } => Some(code.clone()),
            _ => None,
        })
        .await
        .unwrap_or_default();
    membre.vider().await;
    membre.dire(ClientMsg::AdminListUsers).await;
    let info = membre
        .attendre(|m| match m {
            ServerMsg::AdminInfo { users, invites } => Some((users.len(), invites.clone())),
            ServerMsg::Error { .. } => Some((usize::MAX, Vec::new())),
            _ => None,
        })
        .await;
    b.verifier(
        "il ne reçoit ni les comptes ni les codes des autres",
        info.as_ref().is_some_and(|(n, inv)| *n == 0 && inv.iter().all(|i| i.created_by == "membre") && !inv.is_empty()),
        info.as_ref().map(|(n, inv)| (n, inv.iter().map(|i| (&i.code, &i.created_by)).collect::<Vec<_>>())),
    );
    membre.dire(ClientMsg::AdminRevokeInvite { code: code_proprio.clone() }).await;
    let r = membre.resultat(|m| matches!(m, ServerMsg::Info { message } if message.contains("révoquée"))).await;
    b.verifier("il ne révoque pas le code d'un autre", r.is_err(), r);
    if let Some(code) = code {
        membre.dire(ClientMsg::AdminRevokeInvite { code }).await;
        let r = membre.resultat(|m| matches!(m, ServerMsg::Info { message } if message.contains("révoquée"))).await;
        b.verifier("il révoque le sien", r.is_ok(), r);
    }
    modo.vider().await;
    modo.dire(ClientMsg::AdminListUsers).await;
    let info = modo
        .attendre(|m| match m {
            ServerMsg::AdminInfo { users, invites } => Some((users.len(), invites.len())),
            _ => None,
        })
        .await;
    b.verifier(
        "le modérateur (gérer les invitations, expulser) a les comptes et tous les codes",
        info.is_some_and(|(n, i)| n >= 4 && i >= 2),
        info,
    );

    println!("Le rôle du propriétaire");
    let mien = roles.iter().find(|r| r.id == ki_protocol::ROLE_OWNER).cloned().expect("rôle propriétaire");
    proprio.vider().await;
    proprio.dire(ClientMsg::AdminEditRole { role: ki_protocol::RoleInfo { color: Some(0xffaa00), ..mien } }).await;
    let r = proprio
        .resultat(|m| matches!(m, ServerMsg::Roles { roles } if roles.iter().any(|r| r.id == ki_protocol::ROLE_OWNER && r.color == Some(0xffaa00))))
        .await;
    b.verifier("le propriétaire change la couleur de son rôle", r.is_ok(), r);

    println!();
    println!("{} vérification(s) passée(s), {} cassée(s)", b.ok, b.ko.len());
    for k in &b.ko {
        println!("  - {k}");
    }
    if !b.ko.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}
