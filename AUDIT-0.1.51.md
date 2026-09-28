# Audit ki-chat — version 0.1.51

**Date :** 28 septembre 2026 · **Commit audité :** `f726633` (« Version 0.1.51 »)
**Périmètre :** les 11 crates (≈ 94 000 lignes de Rust), la CI, le déploiement, la documentation.

**Méthode.** Le code a été relu par zones, en neuf relectures :
- serveur : plan de contrôle ; HTTP, fichiers et médias ; porte web ; musique, stream et VALORANT ;
- protocole et transport ;
- client : architecture ; modules sensibles ;
- moteur vocal ;
- vidéo et médias.

Chaque constat critique et la plupart des majeurs ont ensuite été vérifiés une seconde fois dans le code. Exécutions faites sous Linux :
- `cargo test` et `cargo clippy` du serveur et du protocole, comme le job CI Linux ;
- `cargo fmt --check` et `cargo deny check` ;
- reproduction isolée du plantage des mentions ;
- lecture de l'état de la CI GitHub ;
- lecture des sources officielles de SpeexDSP 1.2.1, pour un point précis.

**Limites.**
- Rien n'a tourné sous Windows ni sur carte graphique : WASAPI, NVENC et Media Foundation ont seulement été relus.
- Aucun test de charge.
- Les réglages du dépôt GitHub (protection des tags, environnements) ne sont pas visibles.
- L'historique git local est tronqué à 50 commits.
- Les numéros de ligne renvoient au commit audité.

Ce document fait suite à [`AUDIT.md`](AUDIT.md) (version 0.1.3), dont le suivi est repris au §7.

**Légende.** Chaque constat porte un identifiant de domaine (`SRV-3`, `CLI-1`…) et un niveau de confiance :
- **Reproduit** : exécuté ;
- **Vérifié** : lu dans le code, souvent par deux relecteurs ;
- **Probable** : mécanisme vérifié, mais l'impact dépend de l'environnement.

---

## État des corrections (branche `fix/audit-0.1.51`)

Relu et vérifié le 28 septembre 2026, puis corrigé lot par lot, chaque lot
dans son commit. ✅ corrigé ; ◐ corrigé en partie, le reste est une décision
à prendre (en fin de section). Les numéros de ligne cités plus bas renvoient
toujours au commit audité.

| Constat | Commit | Ce qui a été fait |
|---|---|---|
| ✅ CLI-1 | `8d6e022` | Correctif publié avant l'audit (0.1.51). |
| ✅ SRV-1 à SRV-10 | `2950dda` | Budgets par classe de message et par membre, sémaphore Argon2, essais réservés avant hachage, audit borné et après validation, fin des diffusions à tout changement de salon, plafonds de rôles, salons et invitations, `EditMessage` soumis à `SEND_MESSAGE`, code maître secret (tiré au hasard sans `KI_TOKEN`, valeurs d'exemple refusées). |
| ✅ WEB-1 à WEB-6 | `4a2434c` | Authentification avant tout corps, plafonds de connexions et délai d'en-têtes, quotas d'envoi, lectures bornées, ffmpeg en liste blanche de protocoles et environnement vidé, compteurs d'invités décalés. |
| ✅ MUS-1 à MUS-3 | `4a2434c`, `3cf57d7` | Groupes de processus arrêtés TERM puis KILL, dossier temporaire purgé, yt-dlp limité à trois, file bornée partout, état plafonné, fins de morceau détectées. |
| ✅ CRY-1 | `050509b` | Le README ne promet plus de bout en bout ; le code est inchangé (le serveur détient les clés). |
| ✅ CRY-2 | `2785a4d` | Compteur du son du jeu porté par le live. |
| ✅ PRO-1 | `2785a4d` | Variantes inconnues tolérées, version de protocole annoncée, lignes illisibles journalisées. |
| ✅ CLI-2 à CLI-7 | `5e82386` | Cible figée, connexion annulable, diffusion et visionnage arrêtés à toute sortie, lien d'envoi dans le salon d'origine, message non perdu, session vidée en entier ; l'expulsion n'est plus prise pour une coupure (M20). |
| ✅ SEC-1 | `5e82386` | Manifeste signé (plateforme, version, SHA-256) exigé par le client ; release taguée sans clé = échec. |
| ✅ SEC-2 | `050509b` | Signature dans un travail isolé (crate `crates/signer`, sans cache, environnement `release`), clé vérifiée contre celle du client, actions épinglées par empreinte. |
| ✅ SEC-3 | `5e82386` | Budget global de pixels, canevas contrôlé avant décodage, deux décodages à la fois. |
| ✅ AUD-1 | `35b49d2` | Étage `speex_preprocess` retiré plutôt que rallumé : il ne supprimait rien et retardait la voix de 20 ms ; le rallumer gardait ce retard et mangeait la voix en double parole. |
| ✅ AUD-2 | `35b49d2` | Famine comptée seulement si la voix coulait encore ; carte son à court mesurée à part ; le docteur ne conseille la sortie robuste que sur elle. |
| ✅ VID-1, VID-2 | `4fc89e5` | Rééchantillonneur borné (test de propriétés) ; capture morte détectée, diffusion conclue avec sa raison. |
| ◐ OPS-1 | `050509b` | Essai du serveur sous dix clients virtuels avant l'image, chaîne Rust épinglée en CI. **Reste** : `latest` suit toujours `main` (voir décisions). |
| ✅ OPS-2 | `050509b` | Compose sur le fork maintenu `nickfedor/watchtower`. |
| ✅ OPS-3 | `050509b` | Permissions en lecture seule, écriture réservée à la publication, `persist-credentials: false`. |
| ✅ OPS-4 | `050509b` | tini en PID 1, compose sans capacités ni gain de privilèges, mémoire, processus et journal bornés, unité systemd confinée. `read_only` non posé (yt-dlp et deno écrivent hors du volume). |
| ◐ DOC-1 | `050509b` | Section licences du README exacte. **Reste** : le fichier `LICENSE` et les notices tierces (voir décisions). |

**Mineurs.** Corrigés dans les mêmes lots, et dans `1d7cca3` pour le client :
nom de fichier des clips, rapport de plantage limité au serveur du plantage
et citations masquées, diagnostics sans titre de fenêtre ni PUUID, empreinte
« même machine » non traçable, agents Riot sans redirection, mises à jour en
HTTPS seul, client HTTP réutilisé, NVENC chargé depuis System32 ; côté audio,
fin de phrase jouée, DeepFilterNet borné et réinitialisé sur NaN, DRED
stéréo, trames tenues sous le MTU initial ; côté vidéo, trame clé servie sur
image immobile, horodatage à la capture, FFI vérifiées, liste blanche de la
visionneuse, clip inachevé effacé. Restent notamment : pré-roll de la
détection vocale, verrous et allocations du rappel de sortie, démarrage des
chaînes GPU et autres attentes sur le fil de l'interface, repeint en vocal,
Échap et Entrée, suppressions sans confirmation, contrastes, IPv6.

**Décisions à prendre.**
- `KI_VERSION=0.1` dans la stack de production, pour ne déployer que les
  versions publiées (OPS-1).
- Le fichier `LICENSE` (MIT, déjà déclaré) et les notices tierces des
  binaires et de l'image (DOC-1).
- L'environnement `release` du dépôt : approbateurs, restriction aux tags
  `v*`, secret déplacé (voir `deploy/SIGNATURE.md`).
- Le rapport de plantage part sans l'option de partage : c'est un choix, que
  le code dit désormais.
- Riot : prévenir les joueurs que les endpoints ne sont pas publics, et
  rendre les médailles facultatives.
- Clips : la voix des copains gardée par défaut, sans avertir le salon.
- Formatage (`cargo fmt`), découpage de `main.rs`, montée d'egui et d'ureq.

---

## Synthèse

ki-chat est un projet ambitieux. Pour un développeur seul, il est remarquablement soigné dans l'ensemble :
- des bornes explicites presque partout ;
- un épinglage TLS dont la signature est réellement vérifiée ;
- des écritures atomiques et durables ;
- une FFI NVENC exacte au champ près ;
- une page de porte web sous CSP stricte ;
- du fuzzing et `cargo deny` en CI.

Depuis l'audit de la 0.1.3, les six critiques et deux tiers des majeurs ont été corrigés.

Trois problèmes dominent pourtant.

1. **Un message banal fait planter le client.** « @bob ça va » ferme ki-chat chez tous ceux qui reçoivent le message (reproduit). Avec le profil `panic = "abort"`, le moindre découpage de chaîne fautif arrête l'application entière. Il en va de même pour le serveur.
2. **Un seul membre, parfois même un inconnu, peut encore faire tomber le serveur ou le figer, avec des messages ordinaires.** Quatre vecteurs : une photo de profil rediffusée à l'infini, des Argon2 sans limite de concurrence, des yt-dlp sans limite, et des corps HTTP lus avant l'authentification (ce dernier sans compte). En cause : le budget global de 100 messages/s traite un `Ping` et un Argon2 de la même façon.
3. **Le README promet un chiffrement de bout en bout qui n'existe pas.** Le serveur tire la clé de la voix, reçoit celle de chaque stream, et s'en sert pour le bot musique et les invités web.

En toile de fond, une dette de structure explique la plupart des défauts nouveaux :
- côté client, `KiApp` compte 264 champs et un seul `impl` d'environ 11 500 lignes ;
- côté serveur, `handle_msg` est un `match` d'environ 2 100 lignes, sans aucun test.

Permission, rang, budget et remise à zéro s'y répètent à la main, et chaque oubli devient un bug.

Enfin, dans le déploiement documenté, chaque push sur `main` part en production en cinq minutes, par un Watchtower archivé depuis décembre 2025.

### Chiffres clés

| | |
|---|---|
| Code | 94 227 lignes de Rust, 11 crates, 108 fichiers ; `client-gui/src/main.rs` seul : 15 094 lignes |
| Tests | 521 au total ; serveur et protocole : 246 au vert sous Linux (2 ignorés, réseau réel) ; **aucun** dans `quic.rs` (3 029 lignes) ; aucun test d'intégration QUIC |
| CI | verte sur `main`, avec clippy en `-D warnings` sur une chaîne non épinglée (Rust 1.94 échoue déjà) |
| Formatage | 93 fichiers sur 108 hors `rustfmt` (2 039 écarts), sans contrôle en CI |
| `unsafe` | environ 280 occurrences (vidéo, média, voix, client), pour une vingtaine de commentaires `SAFETY` au plus |
| Dépendances | `cargo deny` : aucun avis RustSec actif (1 ignoré, justifié) ; plusieurs versions majeures de retard (annexe B) |
| Constats | 3 critiques, 38 majeurs, une centaine de mineurs (dont une trentaine hérités de la 0.1.3) |

### Les dix priorités

1. **CLI-1.** Corriger le découpage des mentions (une ligne), publier aussitôt, puis activer `clippy::string_slice` pour attraper toute la classe de défaut.
2. **SRV-1, SRV-2.** Supprimer la diffusion de `SetAvatar{Keep}`. Poser un sémaphore Argon2 global et un budget par compte.
3. **SRV-3.** Des classes de budget par type de message : léger, diffusion, coûteux.
4. **WEB-1, WEB-2.** Authentifier avant de lire le corps des requêtes. Délai de lecture des en-têtes et plafond de connexions sur les écoutes HTTPS.
5. **MUS-1 à MUS-3.** yt-dlp tué avec son groupe de processus, `init: true`, sémaphore, file de lecture bornée.
6. **CRY-1, CRY-2.** Réécrire la section chiffrement du README. Rattacher le compteur du son du jeu au stream.
7. **PRO-1.** `#[serde(other)]` sur tous les enums échangés et une version de protocole, indispensables tant que le serveur se déploie avant les clients.
8. **OPS-1 à OPS-3, SEC-2.** Ne mettre en production que des tags, avec un test de fumée de l'image par `ki-load`. Remplacer Watchtower. Réduire les permissions de la CI et isoler la signature.
9. **SEC-1.** Un manifeste de mise à jour signé (plateforme et version) avec protection contre le retour arrière.
10. **Structure.** Un objet `Session` côté client, une politique déclarative par message côté serveur, et les premiers tests d'intégration QUIC.

---

## 1. Le projet en bref

Un serveur de chat privé façon Discord pour une trentaine de joueurs, avec chat, vocal, partage d'écran, clips, bot musique, porte web pour les invités et intégration VALORANT.

**Transport.** Une connexion QUIC par client, sur `9987/udp` :
- lignes JSON pour le contrôle ;
- datagrammes XChaCha20-Poly1305 pour la voix ;
- un flux unidirectionnel par trame vidéo.

À côté, une écoute HTTPS (`8080`) sert les fichiers, les portes web et le diagnostic.

**Déploiement.**
- **Serveur :** image Docker multi-architecture, publiée à chaque push sur `main` et tirée par Watchtower.
- **Client Windows :** installeur Inno Setup et mise à jour automatique signée en Ed25519.
- **Client macOS :** exécutable universel.

| Crate | Lignes (tests compris) | Rôle |
|---|---:|---|
| `client-gui` | 42 343 | application egui : chat, vocal, streams, clips, visionneuse, VALORANT, admin |
| `server` | 27 890 | QUIC, HTTPS, relais voix et vidéo, fichiers, ffmpeg, bot musique, VALORANT, porte web |
| `voice` | 8 136 | moteur audio : WASAPI/cpal, Opus, gigue, AEC, DeepFilterNet, Silero, son du jeu |
| `video` | 6 077 | capture WGC, NVENC maison, openh264, chaînes tout-GPU |
| `protocol` | 5 444 | messages, en-têtes binaires, validations, calculs VALORANT |
| `media` | 1 779 | Media Foundation : lecture et écriture MP4 |
| `client-quic`, `client-cli`, `load` | 1 595 | transport client, CLI, charge |
| `ki-opus`, `ki-aec` | 807 | libopus 1.6.1 et SpeexDSP compilés depuis les sources |

---

## 2. Constats critiques

### ✅ CLI-1 — Une mention suivie d'un caractère accentué fait planter tous les destinataires · *Reproduit*

> **Corrigé sur la branche `claude/ki-chat-audit-qqe0vv`** (`split_at_checked` dans `mention`, deux tests de non-régression). Il reste à le publier dans une release pour que les joueurs en profitent.

- **Où :** `crates/client-gui/src/markup.rs:251`, dans `&apres_arobase[..membre.len()]`.
  - `decouper` y passe tout le reste de la ligne (`markup.rs:198`).
  - Le code est appelé par `me_nomme` (`main.rs:3789`) à la réception de `ServerMsg::Chat` (`main.rs:4835`) et de `ServerMsg::Nouveau` (`main.rs:4902`), puis à l'affichage (`main.rs:14086`).
- **Mécanisme :** pour chaque pseudo du roster, le texte qui suit l'`@` est coupé à la longueur **en octets** du pseudo, sans vérifier que la coupure tombe entre deux caractères. Si elle tombe au milieu d'un caractère multi-octets, `str` panique, et `panic = "abort"` arrête l'application.
- **Reproduction :**
  - fonction extraite telle quelle, roster `["bob", "kevin"]`, texte `bob ça va` ;
  - résultat : `byte index 5 is not a char boundary; it is inside 'ç' (bytes 4..6)`, puis arrêt (code 134) ;
  - même chose avec « salut @Zoé » dès qu'un pseudo de 3 octets existe.
- **Conséquence :**
  - N'importe quel membre, ou un invité web accepté, fait tomber sans le vouloir tous les clients qui voient le salon.
  - L'auteur tombe à l'affichage.
  - Rouvrir le salon refait tomber le client tant que le message existe.
  - Le texte du message part ensuite dans le rapport de plantage (voir SEC, mineurs).
- **Correctif :**
  - `let Some(debut) = apres_arobase.get(..membre.len()) else { continue };`
  - un test avec des pseudos de longueurs variées et du texte accentué ;
  - `#![warn(clippy::string_slice)]` dans `client-gui`, `protocol` et `server`.

### ✅ SRV-1 — `SetAvatar` sans photo : 23 octets reçus, jusqu'à 96 Kio renvoyés à chaque connecté · *Vérifié*

- **Où :** `crates/server/src/quic.rs:2477-2515`. Le champ `avatar` vaut `IconChange::Keep` par défaut (`protocol/src/lib.rs:450-453`).
- **Mécanisme :** `Keep` rend `Ok(())` sans rien écrire, puis déclenche `broadcast_all(Avatar{data})` avec la photo actuelle. La seule borne est le budget global (100 messages/s, rafale 200, `quic.rs:435`).
- **Conséquence :** un membre qui a une photo envoie `{"type":"set_avatar"}` en boucle.
  - Avec 30 connectés, cela fait environ 290 Mo/s sortants : le lien sature, voix comprise.
  - Les files d'envoi de 512 lignes se remplissent en quelques secondes et toutes les connexions sont fermées (`state.rs:112-123`).
  - Le phénomène recommence à chaque reconnexion.
- **Correctif :** faire de `Keep` un no-op ; budget dédié (un changement toutes les 10 s) ; ne diffuser que l'empreinte (les clients savent déjà redemander une photo).

### ✅ SRV-2 — Argon2 sans limite de concurrence : `ChangePassword` épuise la mémoire · *Vérifié (OOM probable selon la RAM)*

- **Où :** `quic.rs:2535-2554` puis `accounts.rs:584-592`. `AdminResetPassword` a le même défaut (`quic.rs:2306-2339`). Aucun `Semaphore` dans `crates/server`.
- **Mécanisme :**
  - Chaque message lance un `spawn_blocking` et une vérification Argon2id de 19 Mio, même quand l'ancien mot de passe est faux.
  - Le pool bloquant de tokio monte jusqu'à 512 fils.
  - Le budget global laisse passer 200 messages d'un coup.
- **Conséquence :**
  - Une rafale coûte environ 3,8 Gio, un rythme soutenu jusqu'à 9,7 Gio.
  - Le noyau tue le serveur, Docker le relance, et n'importe quel membre recommence.
  - L'authentification a le même profil (SRV-7).
- **Correctif :** un sémaphore Argon2 global de 2 à 4 jetons, partagé par l'authentification, le changement et la réinitialisation ; un budget par compte (un essai toutes les 5 s).

---

## 3. Constats majeurs

### 3.1 Serveur — plan de contrôle

**✅ SRV-3 — Un seul budget pour tout : les messages coûteux passent au même tarif qu'un `Ping`.** *Vérifié.*
- **`Search`** (`quic.rs:1981-2025`, puis `history.rs:814-981`) relit tous les journaux visibles et clone `etats`. Cent requêtes par seconde occupent des centaines de fils bloquants et affament les ouvriers tokio : la voix se hache pour tout le monde.
- **`Musique::Ajouter`** lance un yt-dlp par message (MUS-2).
- **Autres :** `StatsValorant` ; `History{1000}` ; `GameStatus`, `VoiceState` et `StreamStart`, rediffusés à tous jusqu'à 100 fois par seconde.
- **Correctif :** trois classes de budget (léger, diffusion à environ 5/s, coûteux à un toutes les quelques secondes), une seule recherche en cours par membre, et un sémaphore global pour Argon2, yt-dlp et la recherche.

**✅ SRV-4 — Des commandes musique suffisent à vider le journal d'audit.** *Vérifié.*
- **Mécanisme :**
  - `detail`, c'est-à-dire l'URL ou le nom de playlist, jusqu'à 160 Kio, est consigné **avant** sa validation (`quic.rs:1629-1642`, contre `1678`, `1703` et `1748`) ;
  - la rotation ne garde que 6 fichiers de 8 Mio (`audit.rs:27`, `34`).
- **Conséquence :**
  - environ 300 messages effacent toute la piste d'audit, par exemple juste après un bannissement ;
  - une seule entrée plus longue que `MAX_LINE` vide le panneau `AuditLog`.
- **Qui peut le faire :** les porteurs de « Contrôler la musique », ou tout membre si l'option « les membres ajoutent » est active.
- **Correctif :** auditer après validation, tronquer `detail` à environ 200 caractères dans `Audit::record`, ne pas auditer Position ni Volume.

**✅ SRV-5 — Changer de salon vocal ou perdre l'accès n'arrête pas les streams.** *Vérifié par deux relecteurs.*
- **Mécanisme :**
  - `fin_de_streams` n'est appelée que par `LeaveVoice`, `StreamStop` et la déconnexion ;
  - trois autres chemins changent de salon vocal sans elle : un `JoinVoice` direct (`quic.rs:985`), `AdminVoiceMove` (`quic.rs:1959`) et `reconcile_memberships` (`state.rs:756-773`) ;
  - le relais (`stream.rs:954`, `990`, `1008`) ne revérifie jamais le salon du spectateur ;
  - le client n'envoie pas `Unwatch` quand on change de salon (`main.rs:3941-3964`).
- **Conséquences :**
  - un membre retiré d'un salon privé garde la vidéo et le son du jeu ;
  - un streamer déplacé diffuse toujours vers son ancien salon.
- **Correctif :** une seule fonction « changer de salon vocal » qui recalcule les routes, les streams et les spectateurs, plus un test d'intégration.

**✅ SRV-6 — Des messages d'une seule ligne, sans plafond sur le nombre d'éléments, disparaissent en silence.** *Vérifié ; seuils estimés.*
- **Messages concernés :**
  - `Welcome` : le logo, tous les rôles, tous les salons ;
  - `Members` : tous les comptes ;
  - `AdminInfo` : tous les comptes, et toutes les invitations, jamais purgées ;
  - `MusiqueEtat` (MUS-3) et `AuditLog`.
- **Mécanisme :** au-delà de `MAX_LINE` (160 Kio), `state::encode` les jette sans rien dire (`state.rs:54-59`).
- **Aggravants :**
  - aucune commande de suppression de compte ;
  - `KI_TOKEN` crée des comptes sans limite, et chaque création réussie remet à zéro le compteur de l'IP (`throttle.rs:90-94`).
- **Conséquences :**
  - quelques centaines de comptes créés avec le code maître, et la liste des membres comme le panneau d'administration ne sont plus jamais reçus ;
  - quelques centaines de rôles ou de salons, et plus personne ne reçoit `Welcome`.
- **Correctif :**
  - plafonds, pagination ou fragmentation, suppression de compte ;
  - un test « pire cas » pour chaque message à liste. Le modèle existe déjà : `fit_within` pour l'historique.

**✅ SRV-7 — Le limiteur d'authentification ne voit pas les essais simultanés.** *Vérifié ; hérité de la 0.1.3.*
- **Mécanisme :** le contrôle se fait en lecture seule (`throttle.rs:96-110`), et l'échec n'est compté qu'après l'Argon2 (`quic.rs:259-268`).
- **Conséquence :** avec un sas de 32 connexions par IP, le débit de devinette est multiplié par 32, tout comme le nombre d'Argon2 lancés par IP.
- **Correctif :** réserver l'essai avant le hachage, et le sémaphore de SRV-2.

**✅ SRV-8 — Le droit d'écrire se contourne.** *Vérifié.*
- `EditMessage` (`quic.rs:1276-1327`) et `React` (`1193-1237`) n'exigent pas `SEND_MESSAGE`.
- Un membre privé d'écriture peut donc réécrire ses anciens messages, jusqu'à 4 000 caractères, qui sont rediffusés au salon.

**✅ SRV-9 — `KI_TOKEN` a des valeurs par défaut dangereuses.** *Vérifié.*
- **Défauts :**
  - variable absente : « changeme », avec un simple avertissement (`main.rs:82-87`) ;
  - variable vide : un code d'invitation vide est accepté (`accounts.rs:341`, `362`) ;
  - l'unité systemd livrée pose `CHANGE_MOI` ;
  - le premier compte créé devient Propriétaire (`accounts.rs:388`).
- **Exposition du code :**
  - le code maître est recopié en clair dans l'entrée d'audit `invite.use` (`quic.rs:274-281`), lisible par les modérateurs ;
  - il est comparé par `!=`, qui n'est pas à temps constant.
- **Correctif :** refuser de démarrer si la valeur est absente, vide ou connue ; masquer le code dans l'audit ; utiliser `secret_eq`.

**✅ SRV-10 — Des E/S bloquantes restent sur la boucle asynchrone (reste de M28).** *Vérifié.*
- **Où :**
  - gestion des rôles et des salons : `quic.rs:2583` à `2844`, dont `set_roles` et `remove_role`, qui réécrivent `users.json` ;
  - `DelierRiot` (`quic.rs:1373`, qui réécrit `fiches.json`, environ 1 Mo) ;
  - `porte::ouvrir` (`quic.rs:2924`), qui peut attendre jusqu'à 2 s.
- **Aggravant :** `Accounts::save` sérialise `users.json`, photos en base64 comprises, puis fait le fsync sous le verrou des comptes (`accounts.rs:791-801`). Or ce verrou est pris par `roster()` et `member_of` depuis les ouvriers tokio.
- **Correctif :** `spawn_blocking` ; sortir les photos de `users.json` ; sérialiser un instantané hors du verrou.

Deux majeurs de la 0.1.3 restent ouverts dans cette zone, **M20** (le motif d'expulsion n'arrive pas) et **M25** (les codes d'invitation sont visibles de qui n'a que KICK) : voir le §7.

### 3.2 Serveur — HTTP, fichiers, médias, porte web

**✅ WEB-1 — Le corps de la requête est lu avant l'authentification.** *Vérifié.*
- **Mécanisme :**
  - `files::upload` (`files.rs:117-130`) reçoit `body: Bytes` : axum lit jusqu'à 25 Mo avant que le handler ne regarde `x-ki-token` ;
  - même chose pour `/upload/partiel` (8 Mo, `medias.rs:413-422`) et `/diag` (256 Ko, `diag.rs:102-113`) ;
  - le routeur n'a ni plafond de connexions ni délai de lecture.
- **Conséquence :** sans compte, des envois concurrents font monter la mémoire du serveur jusqu'à ce que le système le tue.
- **Correctif :** authentifier sur les en-têtes, avec un middleware `from_fn` ou en extrayant `Request` puis `to_bytes` après le contrôle ; écrire les envois sur disque au fil de l'eau ; limiter la concurrence.

**✅ WEB-2 — Les écoutes HTTPS n'ont ni délai de lecture des en-têtes ni plafond de connexions.** *Probable : lu dans axum-server 0.8 et hyper 1.11, non reproduit.*
- **Mécanisme :**
  - le code est en `main.rs:346-348` et `435-437` ;
  - axum-server construit hyper sans horloge, ce qui désactive le délai de lecture des en-têtes ;
  - seule la poignée de main TLS est bornée (10 s) ;
  - chaque connexion peut réserver environ 400 Kio de tampon.
- **Conséquence :**
  - des connexions qui envoient leurs en-têtes au compte-gouttes (slowloris), sans compte, suffisent à épuiser la mémoire ;
  - la publication des liens de porte et l'écoute 443 aggravent l'exposition.
- **Correctif :** `http_builder().http1().timer(TokioTimer::new()).header_read_timeout(10 s)` ; `max_buf_size` ; sémaphore de connexions global et par IP ; `TimeoutLayer`.

**✅ WEB-3 — Les quotas se contournent.** *Vérifié.*
- **`upload-partiel/`** (`medias.rs:429-443`) :
  - les identifiants d'envoi sont choisis par le client, sans limite de nombre ;
  - le dossier n'entre dans aucun quota ;
  - il n'est purgé qu'après une heure d'inactivité.
- **`/diag`** (`diag.rs:123-155`) : la rotation à 5 Mo vaut par couple (membre, version), et la version est libre (en-tête `x-ki-version`).
- **Conséquence :** un membre remplit le disque ; or un disque plein arrête l'écriture de l'historique et des comptes.
- **Correctif :** quotas global et par membre ; version validée ; plafond global de `diag/` et purge par âge.

**✅ WEB-4 — Les sorties de ffmpeg et ffprobe sont lues sans limite.** *Vérifié ; ampleur probable.*
- **Où :** `executer_borne` (`musique.rs:476-489`), `export.rs:737-741` et `medias.rs:824-842`.
- **Mécanisme :** `trames_cles` demande à ffprobe **tous** les paquets vidéo du fichier en JSON, puis charge le tout dans un `serde_json::Value`.
- **Conséquence :** la taille de cette sortie dépend du fichier fourni par le membre ; un fichier inhabituel fait donc gonfler la mémoire de ki-server lui-même.
- **Correctif :** lecture bornée, `-read_intervals` limité à la fenêtre utile, sortie CSV lue ligne à ligne.

**✅ WEB-5 — ffmpeg tourne avec tous les droits du serveur.** *Vérifié.*
- **Accès :**
  - même utilisateur que le serveur ;
  - lecture et écriture de tout `/data` : comptes, clé TLS, `diag.token` ;
  - réseau ouvert.
- **Configuration :**
  - environnement hérité, `KI_TOKEN` et `KI_HENRIK_KEY` compris : aucun `env_clear` dans tout le serveur ;
  - ni `-protocol_whitelist` ni format d'entrée forcé ;
  - aucune limite de mémoire ni de taille de sortie ; ffprobe tourne sans `nice` ;
  - build « master-latest » non épinglé.
- **Conséquence :** une faille dans un démultiplexeur, sur un fichier envoyé par un membre, donne accès à tout le serveur.
- **Correctif, par étapes :**
  1. `env_clear()` ;
  2. `-protocol_whitelist file` et `-f` ou `-format_whitelist` ;
  3. limites `rlimit` posées dans `pre_exec` ;
  4. confinement du système de fichiers (Landlock) ou conteneur de transcodage séparé, sans les secrets ;
  5. ffmpeg stable et épinglé.

**✅ WEB-6 — Les invités de la porte web n'entendent ni le bot ni les autres invités.** *Probable : lecture croisée du serveur et du JS.*
- **Mécanisme :**
  - les compteurs 64 bits du bot et des invités partent entre 0 et 2⁶³ (`porte.rs:487`, `musique.rs:883`) ;
  - la page les lit comme des nombres JavaScript, précis jusqu'à 2⁵³ seulement, et jette toute trame dont le numéro égale le précédent (`porte.js:1305-1315`, `1331`) ;
  - au-delà de 2⁵³, deux numéros consécutifs se confondent, et la plupart des trames sont jetées ;
  - les membres, dont le compteur part sous 2⁴⁸, ne sont pas touchés.
- **Correctif :** tirer ces compteurs comme le client (`>> 16`), ou numéroter côté page sur 32 bits en arithmétique modulaire ; ajouter un test.

### 3.3 Musique et yt-dlp

**✅ MUS-1 — Un yt-dlp tué laisse fuir `/tmp` et des zombies, et ses délais ne bornent rien.** *Mécanisme vérifié.*
- **Contexte :**
  - l'image embarque `yt-dlp_linux`, un exécutable PyInstaller « onefile » ;
  - son chargeur extrait l'application dans `/tmp/_MEI…`, puis lance l'interpréteur en processus fils.
- **Mécanisme :**
  - `Child::kill()` (`musique.rs:495`) ne tue que le chargeur : le serveur ne crée aucun groupe de processus ;
  - l'orphelin garde les tubes ouverts, donc `lecteur.join()` (`musique.rs:503-504`) attend sa fin, et les délais de 40 et 60 s ne bornent rien ;
  - ki-server est le PID 1, sans init (`ENTRYPOINT`, pas d'`init: true`) : il ne récolte pas les orphelins, qui restent en zombies ;
  - le dossier extrait n'est jamais supprimé.
- **Déclencheurs, tous ordinaires :** Suivant, Position, Arrêter, départ du bot.
- **Conséquence :** une soirée de morceaux sautés remplit la couche du conteneur, jusqu'au disque plein.
- **Correctif :** `process_group(0)`, puis SIGTERM au groupe et SIGKILL différé (ou l'archive `yt-dlp_linux.zip`, qui n'extrait rien) ; `TMPDIR` dédié et purgé au démarrage ; `init: true`.

**✅ MUS-2 — Rien ne limite le nombre de yt-dlp simultanés, et `std::thread::spawn` peut abattre le serveur.** *Vérifié.*
- **Mécanisme :**
  - chaque `Ajouter` lance un yt-dlp (environ 100 Mo), sans sémaphore ni budget par membre (`quic.rs:1774-1805`) ;
  - chaque Position ou Suivant démarre une nouvelle chaîne de lecture ;
  - les fils de lecture passent par `std::thread::spawn` (`musique.rs:480`, `485`), qui panique si le système refuse un fil. Avec `panic = "abort"`, tout le serveur tombe.
- **Correctif :** sémaphore global (2 ou 3), budget par membre, `thread::Builder::spawn`, et les `rlimit` prévues par `PLAN-MUSIQUE.md:145`.

**✅ MUS-3 — La file de lecture n'a pas de limite, et l'état du bot peut se figer pour tout le monde.** *Vérifié.*
- **Mécanisme :**
  - `MAX_FILE_MUSIQUE` n'est appliqué que dans `AjouterPlusieurs` (`musique.rs:1010`), pas dans `Ajouter`, `AjouterPiste` ni `PlaylistCharger` ;
  - `PlaylistAjouterPiste` ne nettoie pas `source` (`quic.rs:1725-1734`) ;
  - `MusiqueEtat` transporte toute la file toutes les 5 s, et disparaît au-delà de `MAX_LINE`.
- **Conséquence :**
  - la bannière se fige, et les nouveaux venus ne reçoivent plus aucun état ;
  - cela persiste après un redémarrage, car `file.json` est relu sans limite.
- **Correctif :** borne dans `appliquer`, une seule fonction `Piste::nettoyer`, un message léger dédié à la position de lecture.

### 3.4 Chiffrement : ce que promet le README, ce que fait le code

**✅ CRY-1 — Le serveur détient les clés de la voix et des streams, et s'en sert.** *Vérifié par trois relecteurs.*
- **Voix :**
  - la clé est tirée par le serveur (`state.rs:495`) et envoyée en hexadécimal dans `Welcome` (`quic.rs:352`) ;
  - c'est **la même clé pour tout le serveur et tous les salons**, privés compris, et elle n'est jamais renouvelée ;
  - le serveur déchiffre la voix des salons où écoute un invité web (`quic.rs:620-622`, `porte.rs:826-827`) ;
  - il chiffre lui-même la voix des invités et du bot (`porte.rs:1950`, `musique.rs:1184`).
- **Streams :**
  - la clé est tirée par le client, mais envoyée en clair dans le JSON de `StreamStart` (`main.rs:10281-10296`) ;
  - le serveur la conserve (`quic.rs:1038-1044`, `stream.rs:185`, `778`) ;
  - il la remet à chaque spectateur dans `WatchAccepted` (`stream.rs:939`).
- **Conséquence :**
  - « Chiffrement intégral de bout en bout » (`README.md:50`) et « Le serveur SFU relaie les trames sans avoir la capacité de les déchiffrer » (`README.md:192`) sont faux, tout comme le commentaire `state.rs:418-420` ;
  - la section « Porte » du README dit d'ailleurs le contraire ;
  - la confidentialité d'un salon privé repose sur le routage, pas sur la cryptographie.
- **Ce n'est pas un défaut** pour un serveur entre amis. C'est une promesse fausse.
- **Correctif :**
  - écrire « chiffré en transit (TLS 1.3) ; le serveur détient les clés » ;
  - si l'on veut un vrai chiffrement de bout en bout pour les streams : des enveloppes X25519 par spectateur, déjà prévues dans `PLAN-STREAM.md` ;
  - pour la voix, c'est incompatible avec le bot et les invités web tels qu'ils sont conçus : c'est une décision à prendre.

**✅ CRY-2 — Son du jeu : le nonce est réutilisé et les spectateurs perdent le son.** *Vérifié par quatre relecteurs.*
- **Mécanisme :**
  - `game_audio_emit` repart de `AtomicU64::new(0)` à chaque appel (`client-gui/src/net.rs:311`) ;
  - décocher puis recocher « son du jeu » pendant un live recrée l'émetteur avec la même clé et le même `stream_id` (`main.rs:10484-10488`) : les nonces XChaCha20 se répètent, le même défaut que M15 ;
  - le lecteur jette tout `seq` inférieur ou égal au dernier reçu (`voice/src/jeu.rs:212-215`).
- **Conséquence :** les spectateurs n'entendent plus le jeu pendant une durée égale à celle déjà diffusée. La vidéo, elle, garde son émetteur (`partage.rs:456`).
- **Correctif :** porter le compteur dans l'état du live, avec un `Arc<AtomicU64>` créé en même temps que la clé ; tester la bascule.

### 3.5 Protocole et compatibilité

**✅ PRO-1 — Une variante ajoutée à un enum imbriqué rend le message illisible, et il est jeté sans trace.** *Vérifié ; latent.*
- **Mécanisme :**
  - seul `Medaille` porte `#[serde(other)]` (`protocol/src/lib.rs:2253`) ;
  - il manque notamment à `ChannelKind` (`lib.rs:1744`, présent dans `Welcome` et `ChannelsUpdated`) et à `JeuEtat` (`lib.rs:3037`, présent dans `Members`) ;
  - le client ignore sans journal toute ligne qu'il ne sait pas lire (`client-quic/src/lib.rs:198`, `303`) ;
  - aucune version de protocole n'est échangée.
- **Conséquences :**
  - le jour où le serveur ajoute un type de salon, tous les clients pas encore à jour jettent `Welcome` et affichent « le serveur n'a pas répondu » ;
  - la CLI, elle, attend indéfiniment.
- **Pourquoi c'est structurel ici :** le serveur se déploie tout seul à chaque push, les clients seulement à chaque release.
- **Correctif :** `#[serde(other)] Inconnu` sur tous les enums échangés ; un champ `protocole` et des capacités dans `Auth` et `Welcome` ; un journal limité des types ignorés ; des fichiers JSON de référence en test.

### 3.6 Client — architecture et comportement

**✅ CLI-2 — La reprise automatique peut se reconnecter au mauvais serveur.** *Vérifié.*
- **Mécanisme :**
  - `Reprise` (`main.rs:440-450`) ne retient ni le serveur ni l'adresse ;
  - `connect()` lit le formulaire du lanceur au moment de l'appel ;
  - changer de serveur n'annule pas la reprise.
- **Scénario :** en vocal sur A, A tombe. On se connecte à B pendant le décompte. Au `Welcome` de B, ki-chat rejoint le salon vocal **de B** qui porte le même numéro, micro armé.
- **Variante :** en cliquant B pendant une tentative vers A, le nom et le logo de A s'écrivent dans la fiche de B, et l'empreinte TLS de A peut y être épinglée.
- **Correctif :** capturer une `ServeurCible { id, adresse, pseudo }` au `connect`, et la faire porter par la reprise.

**✅ CLI-3 — « Annuler » pendant « Connexion… » fige l'interface jusqu'à 15 s.** *Vérifié.*
- **Mécanisme :** `NetHandle::quit()` (`net.rs:252`) fait un `join()` sans limite de temps, alors que le fil réseau ne lit ses ordres qu'après `QuicClient::connect` (`net.rs:565-583`).
- **Même famille :** toute déconnexion volontaire bloque aussi l'interface jusqu'à 600 ms, plus l'arrêt du moteur audio.
- **Correctif :** `select!` entre la connexion et les ordres ; `quit()` non bloquant ; arrêt du moteur audio sur un fil séparé.

**✅ CLI-4 — Se déconnecter ou être expulsé n'arrête ni la diffusion ni le visionnage.** *Vérifié.*
- **Mécanisme :** l'arrêt n'existe que dans `connexion_perdue` (`main.rs:4021-4026`) ; `fermer_session` (`4094-4182`) n'y touche pas.
- **Conséquences :**
  - la capture et NVENC tournent pour rien ;
  - la fenêtre de visionnage reste figée ;
  - `demarrer_diffusion` refuse ensuite de redémarrer (`10277`).
- **Correctif :** déplacer ces arrêts dans `fermer_session`.

**✅ CLI-5 — Le lien d'un fichier envoyé part dans le salon ouvert à la fin de l'envoi.** *Vérifié.*
- **Mécanisme :**
  - `ClientMsg::Chat` ne porte pas de salon (`protocol/src/lib.rs:151`) ;
  - le serveur publie donc dans `current_channel` (`quic.rs:1115`) ;
  - `televerser` (`main.rs:5616`) publie le lien à la fin de l'envoi.
- **Conséquences :**
  - changer de salon pendant un long envoi publie un fichier d'un salon privé dans `#général` ;
  - après une reprise, le message part dans une connexion morte, et le bandeau s'efface comme si tout allait bien.
- **Correctif :** finaliser côté serveur avec le salon d'origine (comme `/clips/fin`), ou ajouter le salon au message `Chat`.

**✅ CLI-6 — Un message tapé pendant une coupure est perdu (M9, toujours présent).**
- `send` puis `input.clear()` (`main.rs:7812-7816`) ; `NetHandle::send` ignore l'échec (`net.rs:236`).
- **Correctif :** ne vider le champ qu'après succès, ou remettre le texte en cas d'échec.

**✅ CLI-7 — L'état de session n'existe pas en tant qu'objet.**
- **Mécanisme :** `fermer_session` remet à zéro une soixantaine de champs à la main. Elle en oublie une vingtaine ajoutés depuis la 0.1.3 :
  - `reponse_a` ;
  - l'état musique ;
  - les fiches VALORANT ;
  - les tableaux d'administration ;
  - les brouillons de rôle et de salon ;
  - `clips_partage`, qui garde un numéro de salon de A et le présente sur B.
- **Conséquence :** le problème M21 revient avec chaque nouvelle fonctionnalité.
- **Correctif :** voir le §6.

### 3.7 Client — sécurité, mise à jour, vie privée

**✅ SEC-1 — La signature de mise à jour ne lie ni la version ni la plateforme.** *Vérifié.*
- **Mécanisme :**
  - seuls les octets de l'actif sont signés (`update.rs:269`, `366-397`, `464-479`) ;
  - la version vient de l'étiquette de release, qui n'est pas signée ;
  - la même clé signe `ki-chat.exe` et `ki-chat-macos.tar.gz`.
- **Conséquence :** celui qui peut publier une release — exactement la menace que vise `deploy/SIGNATURE.md` — peut :
  - resservir une ancienne version signée et vulnérable ;
  - ou renommer l'archive macOS signée en `ki-chat.exe` : chaque client Windows la vérifie avec succès, puis remplace son exécutable par un gzip.
- **Correctif :**
  - signer un manifeste `{produit, plateforme, version, sha256}` ;
  - n'accepter que sa propre plateforme et une version supérieure à la courante, en mémorisant la plus haute version vue ;
  - contrôler l'en-tête PE/Mach-O ;
  - activer les releases immuables.

**✅ SEC-2 — La CI signe tout ce qu'on lui présente.** *Probable : les réglages du dépôt ne sont pas visibles.*
- **Mécanisme :**
  - la signature a lieu pour chaque tag `v*` et chaque `workflow_dispatch`, sur n'importe quelle branche (`release.yml:17-19`) ;
  - le secret est un secret de dépôt, sans environnement protégé ;
  - des actions tierces, épinglées par étiquette mobile, tournent dans le même job que la signature. Ce job compile environ mille crates, dont les scripts de build, et restaure un cache ;
  - une seule clé publique est gravée : aucune rotation n'est possible.
- **Correctif :**
  - un environnement « release » avec approbation obligatoire ;
  - une signature dans un job séparé qui ne compile rien ;
  - pas de cache pour les builds de release ;
  - des actions épinglées par SHA ;
  - deux clés publiques gravées, pour pouvoir faire tourner la clé.

**✅ SEC-3 — Des aperçus animés peuvent réclamer plusieurs gigaoctets.** *Probable : les bornes sont vérifiées, l'impact dépend de la machine.*
- **Mécanisme :**
  - une animation peut garder 48 Mpx, soit environ 192 Mo en RGBA, puis autant en mémoire vidéo ;
  - jusqu'à 40 aperçus sont en cache, décodés en parallèle ;
  - les limites d'image-rs s'appliquent image par image, pas au cumul (`images.rs:33`, `39-40`, `449-472`).
- **Conséquence :** 40 GIF de quelques Ko représentent environ 7,7 Go chez chaque client qui ouvre le salon.
- **Correctif :** budget global de pixels (environ 256 Mo), réduction à la taille d'aperçu, plafond d'environ 8 Mpx, 2 à 4 décodages simultanés.

### 3.8 Audio

**✅ AUD-1 — La suppression d'écho résiduel ne s'exécute jamais.** *Vérifié dans les sources de SpeexDSP 1.2.1.*
- **Mécanisme :**
  - `ki-aec` coupe le débruitage Speex (`SET_DENOISE = 0`, `ki-aec/src/lib.rs:98-113`) pour « ne garder que la suppression de résidu » ;
  - or `speex_preprocess_run` force alors un gain de 1 sur toutes les bandes (`preprocess.c:932-937` : « If noise suppression is off, don't apply the gain »).
- **Conséquences :**
  - seul le filtre linéaire MDF agit : l'écho non linéaire, par exemple de haut-parleurs saturés, repart chez les autres ;
  - deux FFT par trame sont calculées pour rien ;
  - le test `un_echo_pur_est_annule`, qui utilise un écho linéaire, ne peut pas le voir.
- **Correctif :** `SET_DENOISE = 1` avec `SET_NOISE_SUPPRESS = 0` (plancher à 0 dB, donc pas de débruitage), en gardant `ECHO_SUPPRESS` et `ECHO_SUPPRESS_ACTIVE` ; un test avec un écho écrêté.

**✅ AUD-2 — Chaque fin de phrase compte comme une « trame incomplète » : le Docteur audio accuse la machine à tort.** *Vérifié.*
- **Mécanisme :**
  - `mix_into` compte une famine dès qu'un tampon amorcé est vide (`jitter.rs:142-149`) ;
  - en push-to-talk ou en détection vocale, chaque fin de prise de parole vide le tampon normalement ;
  - à l'inverse, les vrais sous-remplissages de la carte son ne sont mesurés nulle part.
- **Conséquence :**
  - une soirée produit des centaines de faux craquements ;
  - le docteur conseille alors « Sortie audio robuste », qui ajoute 70 ms de latence pour rien (`docteur.rs:289-298`, affiché en rouge en `main.rs:8933-8936`).
- **Correctif :** ne compter une famine que si un paquet du même locuteur arrive peu après ; mesurer `GetCurrentPadding() == 0` au réveil du fil de rendu.

### 3.9 Vidéo et médias

**✅ VID-1 — Le rééchantillonneur audio panique au-delà de 192 kHz.** *Vérifié.*
- **Où :** `crates/media/src/son.rs:58-80`.
- **Mécanisme :**
  - après la boucle, `consomme = floor(position)` peut dépasser la longueur du tampon quand le pas (cadence ÷ 48 000) dépasse environ 4, et `drain(..consomme)` panique ;
  - entre 144 et 192 kHz, le tampon peut se vider entièrement ; un paquet vide qui suit fait alors déborder `len() - 1` ;
  - la cadence n'est contrôlée que pour être non nulle (`mf.rs:352-358`).
- **Conséquence :** l'application s'arrête, vocal compris, dans deux cas :
  - un FLAC ou ALAC à 352,8 ou 384 kHz dans le dossier du soundboard ;
  - une vidéo que le serveur n'a pas normalisée.
- **Correctif :** `consomme.min(len - 3)` avec une garde si `len < 4` ; cadence bornée entre 8 et 384 kHz ; tests de propriétés.

**✅ VID-2 — Sur le chemin processeur, un étage meurt et la diffusion reste « en cours ».** *Mécanisme vérifié jusque dans windows-capture 2.0.1.*
- **Mécanisme :**
  - une erreur dans `on_frame_arrived` arrête la capture sans appeler `on_closed` : le pipeline attend indéfiniment ;
  - à l'inverse, un échec définitif de l'encodeur fait sortir le pipeline pendant que la capture continue (`capture/wgc.rs:191-193`, `lib.rs:813-823`, `893-895`, `915-918`) ;
  - `source_closed()` ne détecte aucun des deux cas.
- **Conséquence :** image figée chez tous les spectateurs, « 0 i/s » chez le streamer, sans aucun message. C'est le seul chemin des cartes AMD et Intel.
- **Correctif :** un état de santé commun à toutes les boucles (Marche, SourceFermée, EnPanne) ; côté interface, une relance puis `StreamStop`.

### 3.10 CI, déploiement, chaîne d'approvisionnement

**◐ OPS-1 — Chaque push sur `main` part en production en cinq minutes, sans valider l'image.**
- **Chaîne :** `docker.yml` publie `latest` après clippy et les tests du serveur, puis Watchtower la tire toutes les 5 minutes.
- **Ce qui manque :**
  - aucun démarrage de l'image en CI ;
  - aucun test de bout en bout, alors que `ki-load` existe et tourne sous Linux.
- **Rythme observé :** de 0.1.43 à 0.1.51 en deux semaines.
- **Correctif :**
  - `latest` égal au dernier tag, ou un canal « bêta » séparé, et `KI_VERSION` épinglée en production ;
  - un job de fumée : `docker run`, healthcheck, 30 s de `ki-load`, puis vérifier que le journal ne contient aucune panique.

**✅ OPS-2 — Watchtower (`containrrr/watchtower`) est archivé depuis le 17 décembre 2025, et il tient la socket Docker.**
- **Risque :** la socket Docker équivaut à un accès root sur l'hôte, confié à un projet qui ne reçoit plus de correctifs.
- **Compatibilité :** rien ne garantit qu'il suive les Docker Engine récents ; les mises à jour peuvent s'arrêter sans bruit.
- **Correctif :** Portainer GitOps avec webhook (route 2, déjà documentée), ou un timer systemd qui lance `docker compose pull && up -d` sur un tag épinglé.

**✅ OPS-3 — La CI accorde des permissions trop larges.**
- **Mécanisme :**
  - `release.yml` donne `contents: write` à tous ses jobs, y compris le workflow `ci.yml` qu'il appelle ;
  - ce workflow compile plus de mille crates, installe `cargo-fuzz` et tourne en nightly ;
  - `ci.yml` ne déclare lui-même aucun bloc `permissions` ;
  - `actions/checkout` laisse le jeton dans `.git/config`.
- **Conséquence :** n'importe quel script de build d'une dépendance peut pousser dans le dépôt.
- **Correctif :** `permissions: contents: read` par défaut ; `contents: write` réservé au job `publish` ; `persist-credentials: false`.

**✅ OPS-4 — Le conteneur n'a ni init ni durcissement.**
- **Ce qui manque au compose :**
  - `init: true`, ce qui cause les zombies de MUS-1 ;
  - `logging` avec `max-size`, alors que le journal peut être inondé sans authentification (porte web) ;
  - `read_only`, `cap_drop: [ALL]` et `no-new-privileges` ;
  - des limites de mémoire, de processus et de CPU.
- **L'unité systemd :** elle garde `KI_TOKEN` en clair et n'active aucune option de confinement (`NoNewPrivileges`, `ProtectSystem=strict`…).

### 3.11 Licences et documentation

**◐ DOC-1 — Le dépôt n'a aucun fichier de licence, et aucune notice tierce n'accompagne les binaires.**
- **Licence du projet :**
  - le workspace se déclare MIT et le dépôt est public, mais le texte de la licence n'existe nulle part ;
  - la licence MIT exige que sa notice accompagne les copies : sans elle, les droits des tiers restent flous.
- **Licences des dépendances :**
  - les binaires (installeur Inno, paquet macOS) n'embarquent pas les notices qu'exigent les licences MIT, BSD et Apache des quelque mille dépendances, ni celles de libopus, SpeexDSP et openh264 ;
  - l'image Docker publiée sur GHCR redistribue un ffmpeg sous GPL (build « gpl » de BtbN), sans proposer ses sources ;
  - openh264 est compilé depuis ses sources : la licence de brevets H.264 de Cisco ne couvre que les binaires distribués par Cisco.
- **README :** il affirme « MIT / Apache-2.0 / BSD » pour l'ensemble.
- **Correctif :** un fichier `LICENSE` ; des `THIRD-PARTY-NOTICES` générées (par exemple avec `cargo about`) et embarquées dans l'installeur et l'image ; une section licences exacte dans le README.

---

## 4. Constats mineurs

Regroupés par domaine. « (0.1.3) » signale un point hérité de l'audit précédent.

### Serveur — plan de contrôle
- **Double connexion :** la session écrasée n'est pas fermée et continue d'agir au nom du compte (`quic.rs:290-294` contre `313-341`). (0.1.3)
- **Durées non saturées :** `now + …` (`accounts.rs:501`, `630`) ; un ban ou une invitation démesurés expirent aussitôt en release et paniquent en debug. (0.1.3)
- **Mot de passe de salon vocal :** il est vérifié avant le budget vocal, ce qui permet 100 essais par seconde (`quic.rs:956` contre `974`).
- **Pseudos :** seul `is_control` est filtré (`quic.rs:192`) ; bidi, largeur nulle et homoglyphes passent. (0.1.3)
- **`next_id` des comptes et des rôles :** il n'est pas reconsolidé au démarrage. Un identifiant recyclé peut modifier ou supprimer les messages de l'ancien titulaire. (0.1.3, aggravé)
- **File d'envoi :** elle est bornée en lignes, pas en octets, soit jusqu'à environ 80 Mio par connexion.
- **Fuites d'information :**
  - l'activité des salons vocaux privés est visible de tous (`Member.voice`, `StreamStarted`, `VoiceState`) (0.1.3) ;
  - le motif d'un ban et l'existence d'un compte sont révélés avant le mot de passe (`accounts.rs:262-274`, `332-334`).
- **Mémoire et disque :** `etats` garde en mémoire le texte de toutes les éditions, et les `.jsonl` grossissent sans limite.
- **`AdminVoiceMove` :** ne vérifie pas que l'acteur voit le salon cible (`quic.rs:1928-1950`).
- **Arrêt et certificat :** `quitter()` ne vide pas la file d'audit ; le certificat TLS est écrit sans atomicité, sans fsync et avec les permissions par défaut ; `send_direct` n'a pas de délai.
- **Restes de M27 et M30 :**
  - l'état en mémoire est modifié avant la sauvegarde et jamais annulé (`revoke_invite`, `accounts.rs:517-518`) ;
  - `unique_ts` est séparé d'`append` ;
  - il n'existe toujours pas d'identifiant de message.
- **Toujours présents (0.1.3) :**
  - `AdminResetPassword` ne coupe pas la session de la cible ;
  - `AdminEditChannel` remplace tout le salon ;
  - deux serveurs peuvent partager le même `data/` ;
  - noms de fichiers réservés sous Windows ;
  - quota vérifié puis écrit sans verrou.

### Serveur — HTTP, fichiers, médias, porte web
- **En-têtes de `/files` et `/tel` :** ni `nosniff` ni CSP (`files.rs:298-306`), alors que la même origine sert la porte en `script-src 'self'`. HTML et SVG partent bien en pièce jointe.
- **`/clips/{id}/partager` :** contourne l'anti-spam du chat ; `meta.messages` grossit et se perd quand deux partages se croisent.
- **Téléchargements :**
  - ils ne vérifient pas le salon : l'URL, avec 64 bits aléatoires, fait office de clé ;
  - le `meta.json` public expose l'auteur, le salon et les messages ;
  - la liste d'administration donne à DELETE_MESSAGES les liens de salons qu'il ne voit pas.
- **Fabrique :** file sans limite par membre ; des demandes simultanées sur un même clip passent toutes.
- **`valider` :** additions non vérifiées (`export.rs:278-314`).
- **Jeton de diagnostic :**
  - comparé avec `==` ;
  - fichier en 0644, relu sur disque à chaque requête (`diag.rs:208-214`) ;
  - en-tête `/diag` construit en JSON à la main ;
  - séquences ANSI non filtrées.
- **Clip nommé « source.mp4 » :** la branche d'erreur supprime la source (`clips.rs:134-142`).
- **Routeur :** les deux écoutes servent le même, donc l'administration et les envois sont aussi exposés sur l'écoute publique.
- **`/musique/vignette/{id}` :** ni authentification ni limite de débit (`main.rs:323`).
- **Porte web :**
  - demandes :
    - un refus n'écarte que 60 s ;
    - cinq adresses suffisent à occuper toutes les demandes ;
    - les slugs sont devinables ;
  - pseudos des membres énumérables avant le limiteur (`porte.rs:1370-1381`) ;
  - noms invisibles et homoglyphes acceptés : « Redik veut rejoindre » ;
  - en-tête `Host` repris sans validation dans un message signé « Porte » ;
  - journal inondable sans authentification ;
  - un invité peut faire rediffuser la liste des membres deux fois par seconde ;
  - autorisation vocale jamais réévaluée : un invité peut rester 6 h dans le salon ;
  - slugs capturés par les routes statiques (`upload`, `diag`, `tel`, `clips`) ;
  - le nonce des invités est calculé en `wrapping_add` sur un compteur fourni par la page.

### Musique, stream, VALORANT
- **Mise à jour de yt-dlp :**
  - son empreinte vient de la même source que le binaire, et `SHA2-256SUMS.sig` (GPG) n'est jamais vérifié, pas même au build (`ytdlp.rs:86-105`) ;
  - un binaire défaillant reste en service (`ytdlp.rs:113-121`) ;
  - elle bloque un ouvrier tokio jusqu'à 20 s.
- **`cookies.txt` :** réécrit de façon non atomique par chaque yt-dlp concurrent.
- **Lecture :** fin de morceau hachée (`try_recv` consomme un bloc sur deux, `musique.rs:1297`) ; fsync dans la boucle du bot.
- **Relais de stream :**
  - `MEM_MAX` de 32 Mio partagé par toutes les diffusions ;
  - `open_uni` sans délai ;
  - `kbps` non borné côté serveur ;
  - flux lus un par un, ce qui recrée le blocage de tête de ligne.
- **Sanctions vocales :** elles ne s'appliquent pas au stream. Un membre au micro coupé diffuse quand même le son du jeu, et un membre rendu sourd l'entend.
- **VALORANT :**
  - aucune preuve que le compte Riot appartient au membre (`valorant.rs:602-624`) ;
  - médailles déclaratives, que reprend le fil de jeu ;
  - un compte délié pendant une relecture revient ;
  - réponses HenrikDev lues sans limite de taille ;
  - noms d'agents et de cartes non nettoyés dans les annonces ;
  - budget de 20 appels par minute accaparable par un seul membre.
- **Fichiers illisibles :** `playlists.json`, `file.json` et les fiches VALORANT sont remplacés par du vide, sans copie.
- **Morceaux :** aucune limite de durée ni de taille ; les directs sont acceptés.

### Protocole, transport, CLI, outil de charge
- **TOFU :**
  - l'empreinte présentée n'est jamais affichée ;
  - « Accepter » efface l'ancienne et épingle à l'aveugle ;
  - l'épinglage se fait par adresse exacte, si bien que `hôte` et `hôte:9987` sont deux serveurs ;
  - la CLI et `ki-load` n'épinglent rien (`pinned_tls_config(None)` accepte tout).
- **Client IPv4 seulement :** socket liée à `0.0.0.0`, première adresse résolue seulement ; `to_socket_addrs` est un appel bloquant dans une fonction async.
- **`check_png` :** plus permissif que sa documentation (IEND non vide, second IHDR, IDAT libre) ; l'invariant de la cible de fuzzing `image` est donc faux.
- **Cache des photos :** `avatar_hash` (FNV-1a 64 bits) sert d'adresse au cache disque, et le client ne recalcule pas l'empreinte. Une collision permet d'écraser la photo d'un autre chez tous les membres.
- **Nettoyage du texte contournable :**
  - `collapse_blank_lines` laisse passer `"\n \n"` répété ;
  - `clean_emoji` accepte U+200B et U+202E ;
  - `is_dangerous` oublie U+061C.
- **Fuzzing :**
  - faux positif sur un `f32` infini ;
  - graine `react.json` invalide ;
  - aucune graine `ServerMsg`, `image` ni `datagrammes` ;
  - corpus non conservé ;
  - cibles manquantes : `url_musique_valide`, `normaliser_adresse_web`.
- **`ki-load` :**
  - le RTT n'est jamais mesuré ;
  - les déconnexions sont invisibles ;
  - la cadence est biaisée ;
  - la voix seule est testée ;
  - les comptes `charge000…`, au mot de passe écrit dans le source, restent sur le serveur.
- **CLI :**
  - mot de passe passé en argument ;
  - aucun délai sur `Welcome` ;
  - code de sortie 0 en cas de refus ;
  - `/lock 5` **déverrouille** le salon ;
  - les trois points hérités de la 0.1.3 : blocage sans `Welcome`, pas de fermeture propre, faux succès de `/mic on`.
- **Congestion :** BBR, expérimental dans quinn, est utilisé des deux côtés.

### Client — interface et comportement
- **Jeton HTTP :** celui de B peut partir vers A pendant un long envoi, car il est relu à chaque morceau.
- **Échap :** agit deux fois (fermer les réglages vide aussi la saisie) et ferme les fenêtres dans un ordre fixe. (0.1.3, aggravé)
- **Entrée au milieu du texte :** coupe le message en deux lignes, ou écrase la sélection.
- **Docteur audio :** tourne sur le fil de l'interface, sous le verrou du moteur.
- **Repeint :**
  - 20 i/s dès qu'on est en vocal, même fenêtre réduite ou cachée par le jeu ;
  - s'y ajoutent 4 i/s quand la fenêtre est réduite et 2 i/s quand l'enregistreur de clips tourne.
- **Réveils manquants :** certains fils de fond ne réveillent pas l'interface, et « récupération en cours… » reste affiché.
- **Agent HTTP :** `http_agent()` est reconstruit à chaque image, donc une poignée de main TLS par image.
- **Vocal :** l'échéance de l'intention vocale n'est évaluée qu'à l'arrivée de `Members`.
- **Reprise :** « Arrêter d'essayer » n'arrête pas la tentative en cours.
- **Suppressions sans confirmation :** un salon ou un rôle se supprime en un clic.
- **Attentes et E/S sur le fil de l'interface :** `Regard::arreter`, `clips::lister`, `photos::load` et `photos::store`, `list_devices`, et le démarrage des chaînes GPU (jusqu'à 10 s).
- **Toujours présents (0.1.3) :**
  - « Salon verrouillé » vole le focus ;
  - le chat en direct s'affiche sans `safe_display` ;
  - les noms du panneau d'administration s'affichent bruts ;
  - l'opacité du sélecteur de couleur assombrit la couleur ;
  - « connecté » a un contraste d'environ 1,4:1 ;
  - `messages.remove(0)` au-delà de 500 messages ;
  - une erreur périmée est affichée comme motif ;
  - l'IPv6 casse `http_base`.
- **Accessibilité :**
  - `TEXT_FAINT` a un contraste de 3,2 à 3,6:1, pour des textes de 10,5 à 11,5 px ;
  - `BORDER_STRONG`, utilisé comme couleur de texte, n'offre que 2,3:1 ;
  - aucune échelle d'interface n'est mémorisée ;
  - les widgets dessinés à la main ne s'annoncent pas aux lecteurs d'écran.

### Client — sécurité et vie privée
- **Rapport de plantage :**
  - il part **sans consentement**, contrairement à ce que disent `secours.rs:26-28` et `main.rs:190` ;
  - il contient le message de la panique, donc le texte du message en cause dans CLI-1 ;
  - il part vers le premier serveur rejoint ensuite, même si le plantage a eu lieu ailleurs.
- **Diagnostics partagés :** ils contiennent le titre de la fenêtre au premier plan (`overlay.rs:313-320`) et le PUUID Riot, présent dans les URL des erreurs réseau.
- **Empreinte machine :** un hachage du nom de la machine et du compte Windows est diffusé aux spectateurs d'un stream (`partage.rs:157-170`). C'est un identifiant stable, réversible par dictionnaire.
- **`install.sh` (macOS) :** ne vérifie ni empreinte ni signature, et retire l'attribut de quarantaine.
- **Riot :**
  - les endpoints utilisés ne sont pas publics ;
  - l'agent HTTP se fait passer pour le client du jeu afin de passer Cloudflare ;
  - les médailles sont lues automatiquement après chaque partie ;
  - le jeton `X-Riot-Entitlements-JWT` est conservé en cas de redirection.
  - Le risque de sanction est faible, mais c'est chaque joueur qui le porte. Il faut prévenir les joueurs, rendre les médailles facultatives et désactiver les redirections sur cet agent.
  - En revanche, **les jetons Riot ne partent que vers Riot**, jamais vers le serveur ki-chat.
- **Clips :** la voix des copains est gardée par défaut, sans que le salon en soit averti ; le nom du jeu est utilisé brut dans le nom de fichier (« Hunt: Showdown »).
- **Mise à jour :**
  - installer depuis le tampon vérifié plutôt que relire le fichier ;
  - `https_only` sur les agents HTTP ;
  - `verifieur.rs` utilise `verify` alors que le client utilise `verify_strict`.
- **NVENC :** `nvEncodeAPI64.dll` est chargée par son seul nom, donc selon l'ordre de recherche des DLL. Passer par `LoadLibraryExW` avec `LOAD_LIBRARY_SEARCH_SYSTEM32`.
- **Seuls `http(s)` sont cliquables :** le texte affiché est l'URL elle-même, sans `file://` ni chemin UNC. C'est sain, mais le lien s'ouvre sans confirmation.

### Audio
- **Tampon de gigue :**
  - la fin d'une phrase peut être perdue, ou rejouée en tête de la suivante (`jitter.rs:285-299`) ;
  - micro ouvert, la latence ne redescend pas vers sa cible (M17 partiel).
- **Rappel de sortie :**
  - il prend jusqu'à 8+N `std::Mutex` par tranche de 20 ms ;
  - le soundboard est mixé sous verrou (jusqu'à 5,8 Mo) ;
  - le robinet des copains alloue sous verrou ;
  - `ChunkTx` libère et réalloue sur le fil temps réel.
- **Modèles de débruitage :** DeepFilterNet et Silero sont chargés sur le fil de capture, ce qui jette 160 à 500 ms de voix ; la sortie de DeepFilterNet n'est pas bornée (valeurs NaN). (0.1.3)
- **Mutex empoisonné :** il fait tomber l'interface (58 `lock().unwrap()`). (0.1.3)
- **`ki-opus` :**
  - `Dred::decode_into` passe `pcm.len()` comme taille de trame, ce qui est un comportement indéfini en stéréo (latent) ;
  - le crate n'a aucun test.
- **Arrêt du moteur :** jusqu'à 500 ms, et `Drop` n'attend pas les fils.
- **Scripts de build :**
  - l'extraction n'est pas atomique ;
  - `KI_OPUS_SRC` et `KI_SPEEXDSP_SRC` ne sont pas vérifiés, alors que DRED dépend de la version ;
  - le téléchargement échoue derrière un proxy TLS (constaté ici).
- **Reste de M18 :** une trame de 1 365 octets dépasse la taille d'un datagramme au MTU initial de quinn. L'envoi échoue sans bruit, mais le paquet est compté comme envoyé.
- **Priorités et mesures :**
  - MMCSS n'est posé que sur les fils d'entrée et de sortie ;
  - les fils ne sont pas nommés ;
  - la détection vocale n'a pas de pré-roll, donc le premier mot est coupé ;
  - les bancs ne mesurent pas les vrais coûts CPU.

### Vidéo et médias
- **Encodage :** les échecs de la basse qualité ne sont pas plafonnés (NVENC peut être rouvert jusqu'à 30 fois par seconde).
- **Trame clé :** une trame clé demandée n'est pas servie sur une image immobile, donc un nouveau spectateur voit un écran noir.
- **Capture processeur :** elle encode l'image la plus ancienne et la date tard, d'où une image de latence en plus et un décalage entre image et son.
- **Démarrage des chaînes GPU :** il se fait sur le fil de l'interface (jusqu'à 10 s), y compris la bascule automatique de source des clips.
- **Hypothèses FFI non vérifiées :**
  - `copy_nonoverlapping` utilise des strides reçus par une API publique sûre (`nvenc.rs:707-718`) : c'est un trou de soundness ;
  - la longueur d'une tranche MF est comptée depuis le début du tampon ;
  - un pas impair provoque une panique.
- **Media Foundation :** aucune liste blanche (tout conteneur, jusqu'à 8192×8192) alors que le serveur peut servir un fichier non normalisé.
- **Pannes et fichiers :** un device perdu n'est pas détecté quand la capture se tait ; un clip partiel reste sur le disque après un échec d'écriture.
- **Mémoire et arrêt :** le RGBA est réalloué à chaque image (0.1.3) ; `StreamerLoop` et `LocalLoop` n'ont pas de `Drop`.

### CI, outillage, dépendances
- **Toolchain non épinglée alors que clippy tourne en `-D warnings` :**
  - avec Rust 1.94, clippy échoue déjà sur `quic.rs:1593` (`nonminimal_bool`), alors que la CI est verte ;
  - une nouvelle version de Rust peut donc bloquer d'un coup les releases et le déploiement du serveur ;
  - correctif : un `rust-toolchain.toml` et un `rust-version`.
- **Formatage :** aucun (93 fichiers sur 108). Faire un commit de formatage unique, ajouter `.git-blame-ignore-revs` et un contrôle `cargo fmt --check`.
- **`.cargo/config.toml` :** il grave les chemins d'une machine précise (`C:\Users\drion\…`, le cmake de VS 2026) pour toutes les cibles.
- **Signature facultative :** si le secret manque, la release sort non signée (`release.yml:551-559`), alors que tout client depuis la 0.1.12 la refusera. Il faut faire échouer la release.
- **Image non reproductible :**
  - yt-dlp et deno sont pris en « latest », ffmpeg en « master-latest » ;
  - les images de base ne sont pas épinglées par digest ;
  - pas de SBOM, de provenance ni de signature de l'image.
- **`deny.toml` :** `MPL-2.0` et `OpenSSL` sont autorisés sans être utilisés ; la justification de RUSTSEC-2026-0217 oublie que Silero passe aussi par tract.
- **Dependabot :** un seul groupe pour tous les crates, donc une seule incompatibilité bloque toutes les mises à jour.
- **Tests sans matériel :**
  - les tests NVENC et Media Foundation réussissent en silence sans carte ni ffmpeg ;
  - `ki-video` et `ki-media` ne sont testés nulle part sous Linux.

---

## 5. Ce qui est solide

À garder tel quel, et à prendre comme modèle.

- **Serveur :**
  - file d'envoi bornée, fermée en cas de saturation ; budget global ; sas avant authentification (10 s, 32 connexions par IP) ;
  - Argon2 hors verrou, ban relu après le hachage, invitation revérifiée au moment de la consommer ;
  - `write_atomic` avec fsync du fichier et du dossier ; fils d'écriture dédiés pour l'audit et l'historique ; pagination indexée et `fit_within` ;
  - `grantable` sur les permissions ajoutées, rang jamais contourné, salon caché indiscernable d'un salon inexistant, `broadcast` qui revérifie `can_view` ;
  - aucun verrou tenu à travers un `.await`, aucune inversion d'ordre des verrous.
- **HTTP et médias :**
  - identifiants tirés par un générateur cryptographique, chemins assainis ;
  - liste blanche de ce qu'un clip peut servir, et c'est testé ;
  - `Range` sans panique possible ;
  - recette d'export validée deux fois, titre passé par fichier avec `expansion=none` ;
  - aucun shell, délais sur les processus, `nice 19`, une tâche à la fois.
- **Porte web :**
  - un seul chemin crée un invité ; il reste hors de `users` et ne reçoit que la diffusion de son salon (testé) ;
  - CSP sans `unsafe-inline`, avec empreintes ; `frame-ancestors 'none'`, `nosniff`, COOP ;
  - `textContent` partout, en-tête `Origin` vérifié et testé.
- **yt-dlp, stream, VALORANT :**
  - séparateur `--` partout, liste blanche de 8 hôtes en https ;
  - relais de stream borné par spectateur, avec une mémoire comptée et testée ;
  - clé HenrikDev jamais exposée, limiteur de débit, stockage borné, récap hebdomadaire idempotent.
- **Protocole et transport :**
  - TLS 1.3 seul avec ALPN, et la signature de l'épinglage réellement vérifiée (M23) ;
  - lecture de lignes bornée des deux côtés ;
  - en-têtes binaires lus sans panique ;
  - en-têtes KF/KA passés en AAD, domaines de nonce séparés ;
  - `#[serde(default)]` testé dans les deux sens.
- **Client :**
  - jetons Riot jamais envoyés au serveur ;
  - seuls les liens `http(s)` sont cliquables ;
  - DPAPI et Trousseau sans repli en clair ;
  - mise à jour vérifiée avant tout remplacement, avec retour arrière en cas d'échec ;
  - verrou d'instance sûr, `RegisterHotKey` sans crochet ni injection ;
  - images décodées hors du fil de l'interface ;
  - reprise exponentielle avec dispersion, fil de discussion virtualisé.
- **Audio :**
  - sources C vérifiées par SHA-256, conformes aux sommes officielles ;
  - FFI Opus et Speex conforme aux en-têtes ;
  - COM WASAPI confiné à son fil ;
  - gestion des périphériques (repli signalé, retour détecté) très soignée ;
  - décodage sorti du chemin temps réel ;
  - 53 tests, dont de vrais tests de régression.
- **Vidéo :**
  - FFI NVENC vérifiée champ par champ contre `nvEncodeAPI.h` 12.0 (86 décalages, 16 tailles), sans CUDA ;
  - ordre de destruction D3D/COM documenté et juste ;
  - reconstruction après panne et repli sur le processeur sans couper le stream ;
  - côté spectateur, les dimensions d'en-tête ne servent jamais à allouer ; openh264 2.6.0 inclut le correctif de CVE-2025-27091.
- **Outillage :**
  - `cargo deny` (avis, licences, sources), CodeQL, fuzzing et Dependabot ;
  - `--locked` partout ;
  - binaire Windows autonome vérifié avec `dumpbin` ; cohérence entre tag et version vérifiée ;
  - image multi-architecture en compilation croisée ; conteneur non-root avec healthcheck ;
  - des commentaires qui expliquent le pourquoi.

---

## 6. Qualité du code et structure

### Où se trouve la dette

| Élément | Taille | Symptôme |
|---|---|---|
| `client-gui/src/main.rs` | 15 094 lignes (37 % du crate) | un seul `impl KiApp` d'environ 11 500 lignes et 180 méthodes |
| `struct KiApp` | 264 champs | une centaine de session, une soixantaine de préférences, une quarantaine de sous-systèmes ; 14 `Arc<Mutex>` boîtes aux lettres |
| `settings_window` / `handle_server_msg` | 1 260 / 699 lignes | 51 variantes traitées en ligne ; 64 envois de `ClientMsg` au milieu du rendu |
| `server/src/quic.rs` · `handle_msg` | un `match` synchrone d'environ 2 100 lignes (890-2992) | permission, rang, budget et `spawn_blocking` choisis à la main dans chaque branche ; **aucun test** |
| `protocol/src/lib.rs` | 5 327 lignes | dont environ 1 800 de tests et 1 170 de calculs VALORANT |
| `server/src/valorant.rs` / `porte.rs` | 4 359 / 3 392 lignes | client HTTP, parsing, fil de jeu, récap et stockage mêlés ; table, cycle, voix, HTTP et CSP mêlés |
| `video` : `clip_gpu.rs` / `diffusion_gpu.rs` | 1 156 / 892 lignes | environ 80 % de code copié entre les deux |
| `voice/src/lib.rs` | 3 218 lignes | capture, lecture, réseau, périphériques et journal au même endroit |

Les défauts SRV-1, SRV-2, SRV-3, SRV-8 et MUS-2 sont exactement des oublis du code répété dans `handle_msg`. CLI-4 et CLI-7 viennent, eux, de la remise à zéro manuelle de `KiApp`. La structure est donc la cause première, et pas seulement un problème de confort.

### Recommandations

1. **Serveur : une politique déclarative par message.**
   - Chaque variante déclare sa permission, sa règle de rang, sa classe de budget et son mode d'exécution (boucle ou bloquant).
   - Un seul répartiteur applique cette politique.
   - `quic.rs` est découpé en modules : modération, rôles, salons, musique, vocal, profil.
   - `quic::run` rend son adresse d'écoute (port 0), pour écrire des tests d'intégration avec `ki_client_quic` (comme `ki-load`). Premiers tests :
     - la matrice permission × rang ;
     - `SetAvatar{Keep}` ;
     - la concurrence Argon2 ;
     - l'arrêt des streams après un retrait d'accès ;
     - le pire cas face à `MAX_LINE` ;
     - la réception du motif d'expulsion ;
     - la double connexion.
2. **Client : un découpage incrémental, sans changer la logique au début.**
   1. **Déplacer.** Plusieurs `impl KiApp` peuvent vivre dans des sous-modules (`ecrans/…`, `session/reseau.rs`), qui voient les champs privés. `main.rs` redescend sous 1 500 lignes.
   2. **`Prefs`.** Une structure avec `#[serde(default)]` remplace la soixantaine de réglages lus et écrits un à un.
   3. **`Session`.** Un `session: Option<Session>` créé au `connect` avec la `ServeurCible`. `fermer_session` devient « arrêter les effets de bord, puis `self.session = None` », et M21 disparaît par construction.
   4. **Réducteur.** `appliquer(&mut Session, ServerMsg, Instant) -> Vec<Effet>` rend le traitement des messages testable sans fenêtre.
   5. **Généraliser `Vec<Action>`.** Le modèle déjà utilisé par `porte_ui`, le soundboard et la visionneuse s'étend aux autres fenêtres, testées avec `egui::Context::run`.
3. **Protocole :**
   - découper `lib.rs` (messages, validation, binaire, VALORANT) ;
   - un type « scelleur » (clé, domaine, compteur), qui ne peut pas repartir de zéro, partagé par la voix, KF, KA, le serveur et `ki-load`. Cela règle CRY-2 et le nonce des invités web.
4. **Voix et vidéo :**
   - découper les `lib.rs` ;
   - une discipline temps réel testée : anneaux SPSC et `assert_no_alloc` sur le rappel de sortie ;
   - un état de santé commun aux boucles vidéo ;
   - un moteur GPU commun aux clips et à la diffusion.
5. **Persistance (moyen terme).** SQLite (rusqlite, compilé avec la bibliothèque) supprimerait plusieurs familles de défauts d'un coup :
   - identifiants de messages et pagination (M30) ;
   - réécritures complètes de `users.json` avec les photos (SRV-10) ;
   - état modifié avant la sauvegarde (M27).
6. **`panic = "abort"` : limiter les dégâts d'une panique.**
   - Un lint ciblé attrape les causes : `clippy::string_slice`, qui aurait trouvé CLI-1.
   - Autres lints : `clippy::undocumented_unsafe_blocks` ; `unwrap_used` et `expect_used` hors tests dans le serveur.
   - Pour le serveur, un profil `release-serveur` (`inherits = "release"`, `panic = "unwind"`) limiterait une panique à la tâche fautive, à condition de traiter l'empoisonnement des verrous (`parking_lot`, ou une aide tolérante comme `verrou()` dans `clips.rs`).
7. **Sans matériel :**
   - des tests de propriétés pour le rééchantillonneur, les bornes NV12 et le réducteur d'image ;
   - des tests NVENC et Media Foundation marqués « sautés » plutôt que verts ;
   - une liste de contrôle manuelle avant chaque release : TDR forcé, écran débranché, fenêtre de jeu fermée.

---

## 7. Suivi de l'audit 0.1.3

**Critiques : les six sont corrigés.**

| Point | Statut | Remarque |
|---|---|---|
| C1 ligne abîmée → pas de redémarrage | corrigé | lecture tolérante |
| C2 `write_atomic` sans fsync | corrigé | fsync du fichier et du dossier, fichier temporaire unique |
| C3 réponse `History` trop grosse | corrigé | `fit_within` et garde d'écriture ; reste SRV-6 |
| C4 verrou des comptes pendant Argon2 | corrigé | reste SRV-2 (concurrence) |
| C5 file non bornée | corrigé | file bornée et budget global ; reste SRV-1 et SRV-3 |
| C6 décodage sous le mutex du rappel | corrigé | |

**Majeurs : 23 corrigés, 7 partiels, 3 toujours présents.**

| Point | Statut | Remarque |
|---|---|---|
| M1 permissions « membre » | corrigé, avec une brèche | `EditMessage` et `React` sans `SEND_MESSAGE` (SRV-8) |
| M2, M3, M4, M6 | corrigés | `ServerMsg::Perms` ; `grantable` sur les bits ajoutés ; rang au débannissement |
| M5 panneau admin sans garde | corrigé | d'après `AUDIT.md`, non revérifié |
| M7 pagination en boucle | corrigé | |
| M8 page d'historique dans le mauvais salon | corrigé pour l'historique | `Chat` toujours sans salon (CLI-5) |
| **M9 message perdu pendant une coupure** | **toujours présent** | CLI-6 |
| M11, M12, M13, M14, M16 | corrigés | M12 : `Drop` sans `join` |
| M15 nonce réutilisé | corrigé pour la voix | **réintroduit pour le son du jeu** (CRY-2) |
| M17 latence de gigue | partiel | micro ouvert |
| M18 trames > 1 365 octets | corrigé | reste le MTU initial |
| M19 « Connexion… » bloqué | corrigé | délai de 20 s ; reste CLI-3 |
| **M20 motif de kick jamais reçu** | **toujours présent** | la connexion est fermée juste après l'envoi (`state.rs:1216`) |
| M21 état non réinitialisé | partiel | CLI-4, CLI-7 |
| M22 HTTP en clair, M23 certificat non vérifié | corrigés | M23 : signature vérifiée ; reste l'ergonomie du TOFU |
| M24 `accept_bi` sans délai | corrigé | pas de plafond global |
| **M25 codes d'invitation visibles avec KICK** | **toujours présent** | `send_admin_info` (`quic.rs:826-838`), atteint par six permissions |
| M26 `ChangePassword` | partiel | ordre corrigé, mais aucune limite (SRV-2) |
| M27 erreurs disque avalées | partiel | l'erreur remonte, mais l'état en mémoire n'est pas annulé |
| M28 E/S bloquantes | partiel | l'audit ne bloque plus ; reste SRV-10 |
| M29 `ServerMeta` | corrigé | |
| M30 messages de la même milliseconde | partiel | `unique_ts`, mais toujours pas d'identifiant de message |
| M31 rotation de l'audit | corrigé | mais voir SRV-4 |
| M32, M33, M34 aperçus d'images | corrigés | reste une limite en octets (SEC-3) |

**Mineurs.**
- **Corrigés :**
  - compteur de pertes voix ;
  - `before()` ;
  - horloge qui recule (historique) ;
  - renommer ou réordonner un salon, verrou vocal ;
  - saisie multiligne ;
  - deux instances ;
  - mise à jour signée ;
  - recyclage des tampons vidéo ;
  - décodeur recréé sous le mutex ;
  - mort silencieuse du fil de capture.
- **Partiels :**
  - noms longs ;
  - `safe_display` dans l'administration ;
  - repeint et push-to-talk ;
  - `disconnect` bloquant ;
  - `http_base` ;
  - vignette indécodable ;
  - trames coincées dans `pending` ;
  - verrous du rappel de sortie ;
  - stéréo dans ki-opus ;
  - arrêt du pipeline vidéo ;
  - verrou vocal expiré.
- **Toujours présents :** tous les autres (liste au §4, marqués « (0.1.3) »), ainsi que les trois points de la CLI.

`AUDIT.md` peut désormais servir d'archive. Le suivi gagnerait à passer dans des issues GitHub : une par identifiant, avec une étiquette de gravité.

---

## 8. Feuille de route proposée

### Tout de suite : correctifs ciblés, à faible risque

Chacun tient en quelques lignes à quelques dizaines de lignes.

1. **CLI-1** : un test, puis une release immédiate.
2. **SRV-1** : `Keep` sans diffusion, et un budget.
3. **SRV-2** : sémaphore Argon2 et budget par compte.
4. **SRV-8** : `SEND_MESSAGE` exigé sur `EditMessage` et `React`.
5. **WEB-1** : authentifier avant de lire le corps.
6. **WEB-2** : horloge hyper, délai de lecture des en-têtes, plafond de connexions.
7. **MUS-1** : groupe de processus, et `init: true` (qui règle aussi une partie d'OPS-4).
8. **MUS-2** : sémaphore et `Builder::spawn`.
9. **MUS-3** : limite de la file.
10. **CRY-2, VID-1, AUD-1** : chacun tient en une ligne ou presque.
11. **SRV-5 et CLI-4** : arrêter streams et visionnages à chaque changement de salon et à la déconnexion.
12. **PRO-1** : `#[serde(other)]` et version de protocole, à poser avant la prochaine évolution du protocole.
13. **SRV-9** : `KI_TOKEN` obligatoire.
14. **DOC-1 et CRY-1** : fichier `LICENSE`, README corrigé.
15. **OPS-4** : `logging.max-size` et des limites de ressources dans le compose.

### Dans le mois : durcissement

- **Serveur :**
  - classes de budget (SRV-3) ;
  - audit après validation (SRV-4) ;
  - plafonds, pagination et suppression de compte (SRV-6) ;
  - M20 et M25.
- **Fichiers et médias :**
  - quotas (WEB-3) ;
  - lectures bornées (WEB-4) ;
  - ffmpeg confiné (WEB-5) : `env_clear`, listes blanches, `rlimit`.
- **Chaîne de livraison :**
  - manifeste de mise à jour et protection contre le retour arrière (SEC-1) ;
  - CI : environnement protégé, signature isolée, actions épinglées par SHA, permissions minimales (SEC-2, OPS-3) ;
  - déploiement sur tags avec test de fumée (OPS-1), Watchtower remplacé (OPS-2) ;
  - toolchain épinglée, formatage.
- **Client :**
  - CLI-2, CLI-3, CLI-5, CLI-6 ;
  - budget de pixels (SEC-3) ;
  - métrique de famine corrigée (AUD-2) ;
  - état de santé vidéo (VID-2) ;
  - rapport de plantage soumis à consentement ;
  - diagnostics sans titres de fenêtres.
- **Tests :** les premiers tests d'intégration QUIC.

### Dans le trimestre : structure

- **Client :** `Session`, réducteur, découpage de `main.rs`.
- **Serveur :** répartiteur déclaratif ; découpage de `quic.rs`, `valorant.rs` et `porte.rs` ; profil `panic = "unwind"` à étudier.
- **Persistance :** SQLite, et les photos sorties de `users.json`.
- **Dépendances :** montées de version (annexe B), avec un plan de sortie pour DeepFilterNet et tract 0.19.
- **Décisions :**
  - chiffrement de bout en bout des streams : enveloppes par spectateur, ou renoncement assumé ;
  - IPv6.
- **Accessibilité :** contrastes, échelle d'interface, noms accessibles.
- **Documentation :**
  - un dossier `docs/` pour les `PLAN-*.md` (165 Ko à la racine) ;
  - `CHANGELOG.md`, sorti du README de 43 Ko ;
  - `SECURITY.md` ;
  - notices tierces.

---

## Annexe A — Vérifications exécutées

| Commande | Résultat |
|---|---|
| `cargo test -p ki-server -p ki-protocol --locked` (Linux, Rust 1.94.1) | 63 et 183 tests au vert, 2 ignorés (réseau réel) |
| `cargo clippy -p ki-server -p ki-protocol --all-targets --locked -- -D warnings` | 1 erreur `clippy::nonminimal_bool` en `quic.rs:1593` ; la CI (stable courante) est verte sur `f726633` |
| `cargo fmt --all --check` | 2 039 écarts dans 93 fichiers |
| `cargo deny check` | avis, licences et sources au vert ; `MPL-2.0` et `OpenSSL` jamais rencontrés |
| CLI-1 : `mention()` extraite telle quelle, compilée avec `-C panic=abort` | `byte index 5 is not a char boundary; it is inside 'ç' (bytes 4..6) of 'bob ça va'`, arrêt (134) |
| AUD-1 : `speexdsp-1.2.1/libspeexdsp/preprocess.c` (SHA-256 conforme à `ki-aec/build.rs`) | gain forcé à 1 quand le débruitage est coupé (l. 932-937) |
| État de la CI (API GitHub) | `ci`, `docker`, `release` et `codeql` au vert pour la 0.1.51 |

À noter : derrière un proxy TLS, les `build.rs` de `ki-opus` et `ki-aec` échouent, car ureq utilise ses propres racines de certificats. Il faut alors télécharger l'archive soi-même, vérifier son empreinte et la passer par `KI_OPUS_SRC`.

## Annexe B — Dépendances en retard (28 septembre 2026)

| Crate | Verrouillé | Dernière | Remarque |
|---|---|---|---|
| eframe / egui | 0.32.3 | 0.36.2 | le plus gros chantier (API cassée à chaque version) |
| ureq | 2.12.1 | 3.4.2 | client, serveur, `build.rs` |
| tract-onnx | 0.19.16 | 0.23.8 | figé par `deep_filter` (tag git v0.5.6) ; avis RUSTSEC-2026-0217 ignoré |
| cpal | 0.16.0 | 0.18.2 | |
| argon2 | 0.5.3 | 0.6.0 | |
| chacha20poly1305 | 0.10.1 | 0.11.0 | |
| ed25519-dalek | 2.2.0 | 3.0.0 | à monter avec le signeur, en gardant la compatibilité des signatures |
| rfd | 0.15.4 | 0.17.2 | |
| ndarray | 0.15.6 | 0.17.2 | |
| criterion | 0.5.1 | 0.8.2 | bancs seulement |
| prost | 0.11.9 | 0.14.4 | développement seulement |
| windows | 0.48, 0.54, 0.58, 0.61, 0.62 | 0.62.2 | aligner nos crates (0.61 et 0.62) sur une seule version |
| rcgen / socket2 | 0.13 + 0.14 / 0.5 + 0.6 | 0.14.10 / 0.6.5 | doublons |
